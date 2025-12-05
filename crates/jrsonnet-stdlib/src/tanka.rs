// Tanka-compatible native functions
// These are wrappers around the existing stdlib functions to provide
// Tanka-compatible API accessible via std.native()

use jrsonnet_evaluator::IStr;
use jrsonnet_evaluator::{
	error::{ErrorKind::*, Result},
	ObjValue, Val,
};
use jrsonnet_macros::builtin;
use serde::Deserialize;
use serde_json;
use serde_yaml_with_quirks as serde_yaml;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::RwLock;
use std::thread;

// Global Helm template cache - caches raw YAML output from helm to avoid
// redundant helm invocations (same optimization as Go Tanka)
// We cache the raw YAML string rather than Val because Val doesn't implement Sync
static HELM_TEMPLATE_CACHE: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

/// Get or create the Helm template cache
fn get_helm_cache() -> &'static RwLock<Option<HashMap<String, String>>> {
	// Initialize the cache if needed
	{
		let read = HELM_TEMPLATE_CACHE.read().unwrap();
		if read.is_some() {
			return &HELM_TEMPLATE_CACHE;
		}
	}
	{
		let mut write = HELM_TEMPLATE_CACHE.write().unwrap();
		if write.is_none() {
			*write = Some(HashMap::new());
		}
	}
	&HELM_TEMPLATE_CACHE
}

/// Generate a key for a manifest using the nameFormat template
/// This is a simplified implementation that handles the common case where nameFormat
/// includes namespace in the key format
fn generate_manifest_key_from_val(val: &Val, name_format: Option<&str>) -> Result<String> {
	// Check if we should use nameFormat or default format
	let use_namespace_in_key = name_format
		.map(|fmt| fmt.contains("metadata.namespace") || fmt.contains(".or .metadata.namespace"))
		.unwrap_or(false);

	if let Val::Obj(ref obj) = val {
		let kind = obj
			.get("kind".into())
			.ok()
			.flatten()
			.and_then(|v| match v {
				Val::Str(s) => Some(to_snake_case(&s.to_string())),
				_ => None,
			})
			.unwrap_or_else(|| "unknown".to_string());

		let metadata = obj.get("metadata".into()).ok().flatten();

		if let Some(Val::Obj(meta)) = metadata {
			let name = meta
				.get("name".into())
				.ok()
				.flatten()
				.and_then(|v| match v {
					Val::Str(s) => Some(to_snake_case(&s.to_string())),
					_ => None,
				})
				.unwrap_or_else(|| "unknown".to_string());

			// If nameFormat suggests using namespace, include it in the key
			if use_namespace_in_key {
				let namespace = meta
					.get("namespace".into())
					.ok()
					.flatten()
					.and_then(|v| match v {
						Val::Str(s) => Some(to_snake_case(&s.to_string())),
						_ => None,
					})
					.unwrap_or_else(|| "cluster".to_string());

				return Ok(format!("{}_{}_{}", namespace, kind, name));
			} else {
				return Ok(format!("{}_{}", kind, name));
			}
		}
	}

	Ok("unknown".to_string())
}

/// Parse YAML output from helm into a Val object
fn parse_helm_yaml_output(yaml_content: &str, name_format: Option<&str>) -> Result<Val> {
	use jrsonnet_evaluator::ObjValueBuilder;
	let mut builder = ObjValueBuilder::new();
	let deserializer = serde_yaml::Deserializer::from_str(yaml_content);
	let mut seen_keys = HashMap::new();

	for document in deserializer {
		let val: Val = Val::deserialize(document)
			.map_err(|e| RuntimeError(format!("failed to parse helm output: {e}").into()))?;
		// Skip null documents
		if matches!(val, Val::Null) {
			continue;
		}

		// Generate a key for this manifest: <snake_case_kind>_<snake_case_name>
		// Skip resources that don't have proper structure (like Lists)
		if let Val::Obj(ref obj) = val {
			// Check if this is a List (has "items" field) - skip Lists as they're just containers
			if let Ok(Some(Val::Arr(_))) = obj.get("items".into()) {
				continue;
			}
		} else {
			continue;
		}

		// Use the nameFormat-aware key generation
		let key = generate_manifest_key_from_val(&val, name_format)?;

		// Check for duplicate keys and add counter if needed
		let mut final_key = key.clone();
		let mut counter = 2;
		while seen_keys.contains_key(&final_key) {
			final_key = format!("{}_{}", key, counter);
			counter += 1;
		}
		seen_keys.insert(final_key.clone(), ());

		builder.field(&final_key).try_value(val)?;
	}

	Ok(Val::Obj(builder.build()))
}

/// Generate a cache key for Helm template
fn helm_cache_key(
	name: &str,
	chart_path: &str,
	namespace: Option<&str>,
	values_json: Option<&str>,
	include_crds: bool,
) -> String {
	let mut hasher = Sha256::new();
	hasher.update(name.as_bytes());
	hasher.update(b"|");
	hasher.update(chart_path.as_bytes());
	hasher.update(b"|");
	if let Some(ns) = namespace {
		hasher.update(ns.as_bytes());
	}
	hasher.update(b"|");
	if let Some(v) = values_json {
		hasher.update(v.as_bytes());
	}
	hasher.update(b"|");
	hasher.update(if include_crds { b"1" } else { b"0" });
	format!("{:x}", hasher.finalize())
}

/// Convert a string to snake_case (lowercase with underscores)
fn to_snake_case(s: &str) -> String {
	let mut result = String::new();
	let mut chars = s.chars().peekable();

	while let Some(ch) = chars.next() {
		if ch.is_uppercase() {
			// Add underscore before uppercase letters (except at start)
			if !result.is_empty() {
				result.push('_');
			}
			result.push(ch.to_lowercase().next().unwrap());
		} else if ch == '-' {
			// Replace hyphens with underscores
			result.push('_');
		} else {
			result.push(ch);
		}
	}

	result
}

use crate::regex::RegexCacheInner;
use std::rc::Rc;

/// Tanka-compatible parseJson
/// Parses a JSON string into a value
#[builtin]
pub fn builtin_tanka_parse_json(json: String) -> Result<Val> {
	serde_json::from_str(&json)
		.map_err(|e| RuntimeError(format!("failed to parse json: {e}").into()).into())
}

/// Tanka-compatible parseYaml
/// Parses a YAML string (potentially multiple documents) into an array of values
#[builtin]
pub fn builtin_tanka_parse_yaml(yaml: String) -> Result<Val> {
	let mut ret = Vec::new();
	let deserializer = serde_yaml::Deserializer::from_str(&yaml);

	for document in deserializer {
		let val: Val = Val::deserialize(document)
			.map_err(|e| RuntimeError(format!("failed to parse yaml: {e}").into()))?;
		ret.push(val);
	}

	Ok(Val::Arr(ret.into()))
}

/// Tanka-compatible manifestJsonFromJson
/// Reserializes JSON with custom indentation
#[builtin]
pub fn builtin_tanka_manifest_json_from_json(json: String, indent: usize) -> Result<String> {
	let parsed: serde_json::Value = serde_json::from_str(&json)
		.map_err(|e| RuntimeError(format!("failed to parse json: {e}").into()))?;

	let indentation = " ".repeat(indent);
	let formatter = serde_json::ser::PrettyFormatter::with_indent(indentation.as_bytes());
	let mut buf = Vec::new();
	let mut serializer = serde_json::Serializer::with_formatter(&mut buf, formatter);

	serde::Serialize::serialize(&parsed, &mut serializer)
		.map_err(|e| RuntimeError(format!("failed to serialize json: {e}").into()))?;

	buf.push(b'\n');
	String::from_utf8(buf)
		.map_err(|e| RuntimeError(format!("failed to convert to utf8: {e}").into()).into())
}

/// Tanka-compatible manifestYamlFromJson
/// Converts JSON string to YAML
#[builtin]
pub fn builtin_tanka_manifest_yaml_from_json(json: String) -> Result<String> {
	let parsed: Val = serde_json::from_str(&json)
		.map_err(|e| RuntimeError(format!("failed to parse json: {e}").into()))?;

	// Use jrsonnet's custom YAML formatter with Go-compatible settings:
	// - 4 space indentation (matching Go's default)
	// - No quotes on keys when possible
	use crate::manifest::YamlFormat;
	use jrsonnet_evaluator::manifest::ManifestFormat;

	let formatter = YamlFormat::cli(
		4, // 4-space indentation like Go
		#[cfg(feature = "exp-preserve-order")]
		false,
	);

	let mut output = String::new();
	formatter.manifest_buf(parsed, &mut output)?;

	Ok(output + "\n")
}

/// Tanka-compatible sha256
/// Computes SHA256 hash of a string
#[builtin]
pub fn builtin_tanka_sha256(str: String) -> String {
	let mut hasher = Sha256::new();
	hasher.update(str.as_bytes());
	format!("{:x}", hasher.finalize())
}

/// Tanka-compatible escapeStringRegex
/// Escapes regex special characters
#[builtin]
pub fn builtin_escape_string_regex(pattern: String) -> String {
	regex::escape(&pattern)
}

/// Tanka-compatible regexMatch
/// Returns true if the string matches the regex pattern
#[builtin(fields(
    cache: Rc<RegexCacheInner>,
))]
pub fn builtin_tanka_regex_match(
	this: &builtin_tanka_regex_match,
	regex: IStr,
	string: String,
) -> Result<bool> {
	let regex = this.cache.parse(regex)?;
	Ok(regex.is_match(&string))
}

/// Tanka-compatible regexSubst
/// Replaces all matches of regex with replacement string
#[builtin(fields(
    cache: Rc<RegexCacheInner>,
))]
pub fn builtin_tanka_regex_subst(
	this: &builtin_tanka_regex_subst,
	regex: IStr,
	src: String,
	repl: String,
) -> Result<String> {
	let regex = this.cache.parse(regex)?;
	let replaced = regex.replace_all(&src, repl.as_str());
	Ok(replaced.to_string())
}

/// Tanka-compatible helmTemplate
/// Executes `helm template` and returns the rendered manifests as an object
/// Each manifest is keyed by "<snake_case_kind>_<snake_case_name>"
#[builtin]
pub fn builtin_tanka_helm_template(name: String, chart: String, opts: ObjValue) -> Result<Val> {
	// calledFrom is required for proper path resolution

	let called_from = opts.get("calledFrom".into())?.ok_or_else(|| {
		RuntimeError("helmTemplate requires calledFrom field (usually std.thisFile)".into())
	})?;

	// Resolve chart path relative to calledFrom
	let chart_path = if let Val::Str(s) = called_from {
		let called_from_str = s.to_string();

		// Check that calledFrom is not empty
		if called_from_str.is_empty() {
			return Err(RuntimeError("calledFrom cannot be an empty string".into()).into());
		}

		let called_from_path = std::path::Path::new(&called_from_str);
		// Get the directory containing the calling file
		if let Some(dir) = called_from_path.parent() {
			// Check if directory exists
			if !dir.exists() {
				return Err(RuntimeError(
					format!("calledFrom directory does not exist: {}", dir.display()).into(),
				)
				.into());
			}
			// Prevent absolute paths by prefixing with '.' if chart starts with '/'
			let chart_relative = if chart.starts_with('/') {
				format!(".{}", chart)
			} else {
				chart
			};
			// Join the chart path with the directory
			let chart_full = dir.join(&chart_relative);

			// Check if the chart path exists
			if !chart_full.exists() {
				return Err(RuntimeError(
					format!("chart path does not exist: {}", chart_full.display()).into(),
				)
				.into());
			}

			chart_full
				.to_str()
				.ok_or_else(|| RuntimeError("invalid chart path".into()))?
				.to_string()
		} else {
			return Err(RuntimeError(
				format!("calledFrom has no parent directory: {}", called_from_str).into(),
			)
			.into());
		}
	} else {
		return Err(RuntimeError("calledFrom must be a string".into()).into());
	};

	// Extract namespace for cache key
	let namespace = if let Some(ns) = opts.get("namespace".into())? {
		if let Val::Str(s) = ns {
			Some(s.to_string())
		} else {
			None
		}
	} else {
		None
	};

	// Extract values and serialize to JSON for cache key
	let values_json =
		if let Some(values) = opts.get("values".into())? {
			Some(serde_json::to_string(&values).map_err(|e| {
				RuntimeError(format!("failed to serialize values to json: {e}").into())
			})?)
		} else {
			None
		};

	// Extract nameFormat if present
	let name_format = if let Some(nf) = opts.get("nameFormat".into())? {
		if let Val::Str(s) = nf {
			Some(s.to_string())
		} else {
			None
		}
	} else {
		None
	};

	// Extract includeCrds if present (defaults to false)
	let include_crds = if let Some(ic) = opts.get("includeCrds".into())? {
		matches!(ic, Val::Bool(true))
	} else {
		false
	};

	// Check cache first
	let cache_key = helm_cache_key(
		&name,
		&chart_path,
		namespace.as_deref(),
		values_json.as_deref(),
		include_crds,
	);
	{
		let cache = get_helm_cache();
		let read = cache.read().unwrap();
		if let Some(ref map) = *read {
			if let Some(cached_yaml) = map.get(&cache_key) {
				// Cache hit - parse the cached YAML
				return parse_helm_yaml_output(cached_yaml, name_format.as_deref());
			}
		}
	}

	let mut cmd = Command::new("helm");
	cmd.arg("template");
	cmd.arg(&name);
	cmd.arg(&chart_path);

	// Add namespace if present
	if let Some(ref ns) = namespace {
		cmd.arg("--namespace");
		cmd.arg(ns);
	}

	// Add --include-crds if requested
	if include_crds {
		cmd.arg("--include-crds");
	}

	// If we have values, configure stdin and add --values=-
	if values_json.is_some() {
		cmd.arg("--values=-");
		cmd.stdin(Stdio::piped());
	}
	cmd.stdout(Stdio::piped());
	cmd.stderr(Stdio::piped());

	let mut child = cmd
		.spawn()
		.map_err(|e| RuntimeError(format!("failed to execute helm: {e}").into()))?;

	// Write values to stdin if present, then close it
	if let Some(ref json) = values_json {
		if let Some(mut stdin) = child.stdin.take() {
			stdin.write_all(json.as_bytes()).map_err(|e| {
				RuntimeError(format!("failed to write values to helm stdin: {e}").into())
			})?;
			// Close stdin explicitly
			drop(stdin);
		}
	}

	// Take stdout and stderr handles
	let stdout = child
		.stdout
		.take()
		.ok_or_else(|| RuntimeError("failed to capture helm stdout".into()))?;
	let stderr = child
		.stderr
		.take()
		.ok_or_else(|| RuntimeError("failed to capture helm stderr".into()))?;

	// Spawn threads to collect stdout and stderr in parallel
	let stdout_handle = thread::spawn(move || {
		let mut stdout_buf = Vec::new();
		let mut stdout_reader = BufReader::new(stdout);
		stdout_reader.read_to_end(&mut stdout_buf).ok();
		stdout_buf
	});

	let stderr_handle = thread::spawn(move || {
		let mut stderr_buf = Vec::new();
		let mut stderr_reader = BufReader::new(stderr);
		stderr_reader.read_to_end(&mut stderr_buf).ok();
		stderr_buf
	});

	// Wait for the process to complete
	let status = child
		.wait()
		.map_err(|e| RuntimeError(format!("failed to wait for helm: {e}").into()))?;

	// Get stdout from the thread
	let stdout_buf = stdout_handle
		.join()
		.map_err(|_| RuntimeError("failed to join stdout thread".into()))?;

	// Get stderr from the thread
	let stderr_buf = stderr_handle
		.join()
		.map_err(|_| RuntimeError("failed to join stderr thread".into()))?;

	// Check if helm command succeeded
	if !status.success() {
		let stderr = String::from_utf8_lossy(&stderr_buf);
		return Err(RuntimeError(format!("helm template failed: {stderr}").into()).into());
	}

	// Convert stdout to string (YAML content)
	let yaml_content = String::from_utf8(stdout_buf)
		.map_err(|e| RuntimeError(format!("invalid UTF-8 in helm output: {e}").into()))?;

	// Store raw YAML in cache before parsing
	{
		let cache = get_helm_cache();
		let mut write = cache.write().unwrap();
		if let Some(ref mut map) = *write {
			map.insert(cache_key, yaml_content.clone());
		}
	}

	// Parse and return the YAML output
	parse_helm_yaml_output(&yaml_content, name_format.as_deref())
}

/// Tanka-compatible kustomizeBuild
/// Executes `kustomize build` and returns the rendered manifests as an object
/// Each manifest is keyed by "<snake_case_kind>_<snake_case_name>"
#[builtin]
pub fn builtin_tanka_kustomize_build(path: String, opts: ObjValue) -> Result<Val> {
	// calledFrom is required for proper path resolution
	let called_from = opts.get("calledFrom".into())?.ok_or_else(|| {
		RuntimeError("kustomizeBuild requires calledFrom field (usually std.thisFile)".into())
	})?;

	// Resolve kustomize path relative to calledFrom
	let kustomize_path = if let Val::Str(s) = called_from {
		let called_from_str = s.to_string();

		// Check that calledFrom is not empty
		if called_from_str.is_empty() {
			return Err(RuntimeError("calledFrom cannot be an empty string".into()).into());
		}

		let called_from_path = std::path::Path::new(&called_from_str);
		// Get the directory containing the calling file
		if let Some(dir) = called_from_path.parent() {
			// Check if directory exists
			if !dir.exists() {
				return Err(RuntimeError(
					format!("calledFrom directory does not exist: {}", dir.display()).into(),
				)
				.into());
			}
			// Prevent absolute paths by prefixing with '.' if path starts with '/'
			let path_relative = if path.starts_with('/') {
				format!(".{}", path)
			} else {
				path
			};
			// Join the kustomize path with the directory
			let kustomize_full = dir.join(&path_relative);

			// Check if the kustomize path exists
			if !kustomize_full.exists() {
				return Err(RuntimeError(
					format!(
						"kustomize path does not exist: {}",
						kustomize_full.display()
					)
					.into(),
				)
				.into());
			}

			kustomize_full
				.to_str()
				.ok_or_else(|| RuntimeError("invalid kustomize path".into()))?
				.to_string()
		} else {
			return Err(RuntimeError(
				format!("calledFrom has no parent directory: {}", called_from_str).into(),
			)
			.into());
		}
	} else {
		return Err(RuntimeError("calledFrom must be a string".into()).into());
	};

	let mut cmd = Command::new("kustomize");
	cmd.arg("build");
	cmd.arg(&kustomize_path);
	cmd.stdout(Stdio::piped());
	cmd.stderr(Stdio::piped());

	let mut child = cmd
		.spawn()
		.map_err(|e| RuntimeError(format!("failed to execute kustomize: {e}").into()))?;

	// Take stdout and stderr handles
	let stdout = child
		.stdout
		.take()
		.ok_or_else(|| RuntimeError("failed to capture kustomize stdout".into()))?;
	let stderr = child
		.stderr
		.take()
		.ok_or_else(|| RuntimeError("failed to capture kustomize stderr".into()))?;

	// Spawn a thread to collect stderr
	let stderr_handle = thread::spawn(move || {
		let mut stderr_buf = Vec::new();
		let mut stderr_reader = BufReader::new(stderr);
		stderr_reader.read_to_end(&mut stderr_buf).ok();
		stderr_buf
	});

	// Parse YAML output while streaming from stdout
	use jrsonnet_evaluator::ObjValueBuilder;
	let mut builder = ObjValueBuilder::new();
	let stdout_reader = BufReader::new(stdout);
	let deserializer = serde_yaml::Deserializer::from_reader(stdout_reader);
	let mut seen_keys = HashMap::new();

	for document in deserializer {
		let val: Val = Val::deserialize(document)
			.map_err(|e| RuntimeError(format!("failed to parse kustomize output: {e}").into()))?;
		// Skip null documents
		if matches!(val, Val::Null) {
			continue;
		}

		// Generate a key for this manifest: <snake_case_kind>_<snake_case_namespace>_<snake_case_name>
		// or <snake_case_kind>_<snake_case_name> if no namespace
		let key = if let Val::Obj(ref obj) = val {
			let kind = obj
				.get("kind".into())?
				.and_then(|v| match v {
					Val::Str(s) => Some(to_snake_case(&s.to_string())),
					_ => None,
				})
				.unwrap_or_else(|| "unknown".to_string());

			let metadata = obj.get("metadata".into())?;
			let (name, namespace) = if let Some(Val::Obj(meta)) = metadata {
				let name = meta
					.get("name".into())?
					.and_then(|v| match v {
						Val::Str(s) => Some(to_snake_case(&s.to_string())),
						_ => None,
					})
					.unwrap_or_else(|| "unknown".to_string());

				let namespace = meta.get("namespace".into())?.and_then(|v| match v {
					Val::Str(s) => Some(to_snake_case(&s.to_string())),
					_ => None,
				});

				(name, namespace)
			} else {
				("unknown".to_string(), None)
			};

			// Include namespace in key if present, otherwise just kind_name
			if let Some(ns) = namespace {
				format!("{}_{}_{}", kind, ns, name)
			} else {
				format!("{}_{}", kind, name)
			}
		} else {
			"unknown".to_string()
		};

		// Check for duplicate keys and add counter if needed
		let mut final_key = key.clone();
		let mut counter = 2;
		while seen_keys.contains_key(&final_key) {
			final_key = format!("{}_{}", key, counter);
			counter += 1;
		}
		seen_keys.insert(final_key.clone(), ());

		builder.field(&final_key).try_value(val)?;
	}

	// Wait for the process to complete
	let status = child
		.wait()
		.map_err(|e| RuntimeError(format!("failed to wait for kustomize: {e}").into()))?;

	// Get stderr from the thread
	let stderr_buf = stderr_handle
		.join()
		.map_err(|_| RuntimeError("failed to join stderr thread".into()))?;

	// Check if kustomize command succeeded
	if !status.success() {
		let stderr = String::from_utf8_lossy(&stderr_buf);
		return Err(RuntimeError(format!("kustomize build failed: {stderr}").into()).into());
	}

	Ok(Val::Obj(builder.build()))
}
