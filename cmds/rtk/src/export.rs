//! export - Export Tanka environments to files
//!
//! This module handles exporting multiple Tanka environments to files in parallel.
//! It evaluates environments and writes the resulting Kubernetes manifests to disk.

use anyhow::{bail, Context, Result};
use gtmpl::Value;
use rayon::prelude::*;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::discover::{find_environments, DiscoveredEnv};
use crate::eval::{eval, EvalOpts};

/// When exporting manifests to files, it becomes increasingly hard to map manifests back to its environment.
/// This file can be used to map the files back to their environment.
/// This is aimed to be used by CI/CD but can also be used for debugging purposes.
const MANIFEST_FILE: &str = "manifest.json";

/// Options for the export command
#[derive(Debug, Clone)]
pub struct ExportOpts {
	/// Output directory
	pub output_dir: PathBuf,
	/// File extension (yaml or json)
	pub extension: String,
	/// Filename format template (Go text/template syntax)
	pub format: String,
	/// Number of parallel workers
	pub parallelism: usize,
	/// Eval options to pass through
	pub eval_opts: EvalOpts,
	/// Environment name filter (for multi-env directories)
	pub name: Option<String>,
	/// Recursive mode - process all environments found
	pub recursive: bool,
	/// Skip generating manifest.json file that tracks exported files
	pub skip_manifest: bool,
}

impl Default for ExportOpts {
	fn default() -> Self {
		Self {
			output_dir: PathBuf::from("."),
			extension: "yaml".to_string(),
			format: "{{.apiVersion}}.{{.kind}}-{{or .metadata.name .metadata.generateName}}"
				.to_string(),
			parallelism: 8,
			eval_opts: EvalOpts::default(),
			name: None,
			recursive: false,
			skip_manifest: false,
		}
	}
}

/// Result of exporting a single environment
#[derive(Debug)]
pub struct ExportEnvResult {
	/// Path to the environment
	pub env_path: PathBuf,
	/// Files that were written (relative to output_dir)
	#[allow(dead_code)]
	pub files_written: Vec<PathBuf>,
	/// Environment namespace (for manifest.json tracking)
	pub env_namespace: Option<String>,
	/// Any error that occurred
	pub error: Option<String>,
}

/// Result of the export operation
#[derive(Debug)]
pub struct ExportResult {
	/// Total environments processed
	#[allow(dead_code)]
	pub total_envs: usize,
	/// Successfully exported environments
	#[allow(dead_code)]
	pub successful: usize,
	/// Failed environments
	pub failed: usize,
	/// Results for each environment
	pub results: Vec<ExportEnvResult>,
}

/// Errors that can occur during export
#[derive(Debug)]
enum ExportError {
	/// Fatal error - stop all processing
	Fatal(String),
	/// Per-environment error - log and continue
	#[allow(dead_code)]
	EnvError(PathBuf, String),
}

/// Export environments from given paths to the output directory
pub fn export(paths: &[String], opts: ExportOpts) -> Result<ExportResult> {
	// PHASE 1: Validate template format FIRST (fail fast - Issue #2)
	validate_filename_template(&opts.format)
		.context("Invalid filename format template - check Go text/template syntax")?;

	// PHASE 2: Discover environments
	let envs = find_environments(paths)?;

	if envs.is_empty() {
		return Ok(ExportResult {
			total_envs: 0,
			successful: 0,
			failed: 0,
			results: vec![],
		});
	}

	// PHASE 3: Check for ambiguous multi-environment case (Issue #4)
	if envs.len() > 1 && opts.name.is_none() && !opts.recursive {
		let env_names: Vec<_> = envs.iter().map(|e| e.path.display().to_string()).collect();
		bail!(
			"Found {} environments. Use --name to select one or --recursive to export all:\n{}",
			envs.len(),
			env_names
				.iter()
				.take(10)
				.map(|n| format!("  - {}", n))
				.collect::<Vec<_>>()
				.join("\n")
		);
	}

	// Filter by name if specified
	let envs: Vec<_> = if let Some(ref name) = opts.name {
		envs.into_iter()
			.filter(|e| {
				e.path
					.file_name()
					.and_then(|n| n.to_str())
					.map(|n| n.contains(name))
					.unwrap_or(false)
			})
			.collect()
	} else {
		envs
	};

	if envs.is_empty() {
		bail!(
			"No environments found matching name filter: {:?}",
			opts.name
		);
	}

	// Create output directory
	fs::create_dir_all(&opts.output_dir)
		.context(format!("creating output directory {:?}", opts.output_dir))?;

	// Set up rayon thread pool
	let pool = rayon::ThreadPoolBuilder::new()
		.num_threads(opts.parallelism)
		.build()
		.context("building thread pool")?;

	// Abort flag for early termination (Issue #3 & #5)
	let abort_flag = Arc::new(AtomicBool::new(false));

	// Process environments in parallel with early abort support
	let results: Vec<ExportEnvResult> = pool.install(|| {
		envs.par_iter()
			.map(|env| {
				// Check abort flag before expensive work (Issue #5)
				if abort_flag.load(Ordering::Relaxed) {
					return ExportEnvResult {
						env_path: env.path.clone(),
						files_written: vec![],
						env_namespace: None,
						error: Some("Skipped due to earlier fatal error".to_string()),
					};
				}

				match export_single_env(env, &opts) {
					Ok((files, namespace)) => ExportEnvResult {
						env_path: env.path.clone(),
						files_written: files,
						env_namespace: Some(namespace),
						error: None,
					},
					Err(ExportError::Fatal(msg)) => {
						// Set abort flag for fatal errors (Issue #3)
						abort_flag.store(true, Ordering::Relaxed);
						ExportEnvResult {
							env_path: env.path.clone(),
							files_written: vec![],
							env_namespace: None,
							error: Some(format!("FATAL: {}", msg)),
						}
					}
					Err(ExportError::EnvError(_, msg)) => ExportEnvResult {
						env_path: env.path.clone(),
						files_written: vec![],
						env_namespace: None,
						error: Some(msg),
					},
				}
			})
			.collect()
	});

	// Summarize results
	let successful = results.iter().filter(|r| r.error.is_none()).count();
	let failed = results.iter().filter(|r| r.error.is_some()).count();

	// Generate manifest.json file if not skipped
	if !opts.skip_manifest {
		export_manifest_file(&opts.output_dir, &results)?;
	}

	Ok(ExportResult {
		total_envs: envs.len(),
		successful,
		failed,
		results,
	})
}

/// Validate that the filename template is valid Go text/template syntax (Issue #2)
fn validate_filename_template(format: &str) -> Result<()> {
	use crate::spec::{Environment, Metadata, Spec};
	use std::collections::BTreeMap;

	// Create a test manifest with all expected fields
	let test_manifest = serde_json::json!({
		"apiVersion": "v1",
		"kind": "ConfigMap",
		"metadata": {
			"name": "test",
			"generateName": "test-",
			"namespace": "default",
			"labels": {
				"app": "test"
			}
		}
	});

	// Create a test environment with typical fields including labels
	let mut labels = BTreeMap::new();
	labels.insert("cluster_name".to_string(), "test-cluster".to_string());
	labels.insert("team".to_string(), "test-team".to_string());
	labels.insert("fluxExport".to_string(), "true".to_string());
	labels.insert("fluxExportDir".to_string(), "test-dir".to_string());

	let test_env = Some(Environment {
		api_version: "tanka.dev/v1alpha1".to_string(),
		kind: "Environment".to_string(),
		metadata: Metadata {
			name: Some("test-env".to_string()),
			namespace: Some("default".to_string()),
			labels: Some(labels),
		},
		spec: Spec {
			api_server: Some("https://kubernetes.default.svc".to_string()),
			context_names: None,
			namespace: "default".to_string(),
			diff_strategy: None,
			apply_strategy: None,
			inject_labels: None,
			resource_defaults: None,
			expect_versions: None,
			export_jsonnet_implementation: None,
		},
		data: None,
	});

	// Try to render with the template
	format_filename_gtmpl(&test_manifest, &test_env, format)
		.context("Template validation failed")?;

	Ok(())
}

/// Count Environment objects in a JSON value (for multi-env detection)
fn count_environment_objects(value: &JsonValue) -> usize {
	let mut count = 0;

	match value {
		JsonValue::Object(obj) => {
			// Check if this is an Environment object
			if obj.get("kind").and_then(|v| v.as_str()) == Some("Environment")
				&& obj.contains_key("apiVersion")
			{
				count += 1;
			}
			// Recurse into object values
			for v in obj.values() {
				count += count_environment_objects(v);
			}
		}
		JsonValue::Array(arr) => {
			for v in arr {
				count += count_environment_objects(v);
			}
		}
		_ => {}
	}

	count
}

/// Export a single environment
/// Returns (files_written, environment_namespace)
fn export_single_env(
	env: &DiscoveredEnv,
	opts: &ExportOpts,
) -> Result<(Vec<PathBuf>, String), ExportError> {
	// Evaluate the environment
	let result = eval(env.path.to_string_lossy().as_ref(), opts.eval_opts.clone())
		.map_err(|e| ExportError::EnvError(env.path.clone(), e.to_string()))?;

	// Check for multiple Environment objects (Issue C - match tk behavior)
	let env_count = count_environment_objects(&result.value);
	if env_count > 1 && opts.name.is_none() {
		return Err(ExportError::EnvError(
			env.path.clone(),
			format!(
				"found {} Environments. Use --name to select a single one",
				env_count
			),
		));
	}

	// Extract environment namespace (for manifest.json tracking)
	// Use metadata.namespace if available, otherwise fall back to spec.namespace
	let env_namespace = if let Some(ref env_spec) = result.spec {
		env_spec
			.metadata
			.namespace
			.clone()
			.or_else(|| Some(env_spec.spec.namespace.clone()))
			.unwrap_or_else(|| env.path.to_string_lossy().to_string())
	} else {
		// Fallback to environment path if no spec
		env.path.to_string_lossy().to_string()
	};

	// Extract Kubernetes manifests from the result
	let manifests = extract_manifests(&result.value)
		.map_err(|e| ExportError::EnvError(env.path.clone(), e.to_string()))?;

	if manifests.is_empty() {
		return Ok((vec![], env_namespace));
	}

	// Use output directory directly (matching tk behavior)
	// Note: tk writes directly to output_dir without creating env subdirectories
	fs::create_dir_all(&opts.output_dir)
		.map_err(|e| ExportError::EnvError(env.path.clone(), e.to_string()))?;

	let mut files_written = Vec::new();

	// Write each manifest to a file
	for mut manifest in manifests {
		// Inject namespace if needed (matching Tanka's behavior in pkg/process/namespace.go)
		inject_namespace(&mut manifest, &result.spec);

		let filename =
			format_filename_gtmpl(&manifest, &result.spec, &opts.format).map_err(|e| {
				// Template errors after validation are fatal (something very wrong)
				ExportError::Fatal(format!("Template rendering failed: {}", e))
			})?;

		// Split by / and sanitize each path component separately
		// Filter out empty components and <no value> placeholders
		let path_parts: Vec<String> = filename
			.split('/')
			.map(|part| part.trim())
			.filter(|part| !part.is_empty() && *part != "<no value>")
			.map(|part| sanitize_path_component(part))
			.filter(|part| !part.is_empty())
			.collect();

		if path_parts.is_empty() {
			return Err(ExportError::Fatal(format!(
				"Template produced empty filename for manifest: {}",
				serde_json::to_string(&manifest).unwrap_or_else(|_| "unknown".to_string())
			)));
		}

		// Join path components and add extension to the last component
		let mut relative_path = std::path::PathBuf::new();
		for (i, part) in path_parts.iter().enumerate() {
			if i == path_parts.len() - 1 {
				// Last component - add extension
				relative_path.push(format!("{}.{}", part, opts.extension));
			} else {
				// Directory component
				relative_path.push(part);
			}
		}

		let filepath = opts.output_dir.join(&relative_path);

		// Create parent directories if needed
		if let Some(parent) = filepath.parent() {
			fs::create_dir_all(parent)
				.map_err(|e| ExportError::EnvError(env.path.clone(), e.to_string()))?;
		}

		// Serialize manifest
		let content = if opts.extension == "json" {
			serde_json::to_string_pretty(&manifest)
				.map_err(|e| ExportError::EnvError(env.path.clone(), e.to_string()))?
		} else {
			serde_yaml::to_string(&manifest)
				.map_err(|e| ExportError::EnvError(env.path.clone(), e.to_string()))?
		};

		fs::write(&filepath, content)
			.map_err(|e| ExportError::EnvError(env.path.clone(), e.to_string()))?;

		// Track relative path for manifest.json
		files_written.push(relative_path);
	}

	Ok((files_written, env_namespace))
}

/// Export manifest file that maps exported files to their environment
/// Merges with existing manifest.json if present
fn export_manifest_file(output_dir: &PathBuf, results: &[ExportEnvResult]) -> Result<()> {
	let manifest_path = output_dir.join(MANIFEST_FILE);

	// Read existing manifest.json if it exists
	let mut file_to_env: HashMap<String, String> = if manifest_path.exists() {
		let content =
			fs::read_to_string(&manifest_path).context("reading existing manifest.json")?;
		serde_json::from_str(&content).context("parsing existing manifest.json")?
	} else {
		HashMap::new()
	};

	// Add new entries from successful exports
	for result in results {
		if result.error.is_none() {
			if let Some(ref namespace) = result.env_namespace {
				for file in &result.files_written {
					// Convert PathBuf to string with forward slashes (cross-platform)
					let file_str = file
						.components()
						.map(|c| c.as_os_str().to_string_lossy())
						.collect::<Vec<_>>()
						.join("/");
					file_to_env.insert(file_str, namespace.clone());
				}
			}
		}
	}

	// Write manifest.json
	let content =
		serde_json::to_string_pretty(&file_to_env).context("serializing manifest.json")?;
	fs::write(&manifest_path, content).context("writing manifest.json")?;

	Ok(())
}

/// Extract Kubernetes manifests from evaluation result
fn extract_manifests(value: &JsonValue) -> Result<Vec<JsonValue>> {
	let mut manifests = Vec::new();

	// Check if this is a Tanka environment object with a `data` field
	if let JsonValue::Object(obj) = value {
		if obj.contains_key("apiVersion") && obj.contains_key("kind") {
			if let Some(JsonValue::String(kind)) = obj.get("kind") {
				if kind == "Environment" {
					// This is a Tanka environment - extract from `data` field
					if let Some(data) = obj.get("data") {
						collect_manifests(data, &mut manifests);
						return Ok(manifests);
					}
				}
			}
		}
	}

	// Otherwise, collect manifests normally
	collect_manifests(value, &mut manifests);
	Ok(manifests)
}

/// Recursively collect Kubernetes manifests from a JSON value
fn collect_manifests(value: &JsonValue, manifests: &mut Vec<JsonValue>) {
	match value {
		JsonValue::Object(obj) => {
			// Check if this looks like a Kubernetes manifest (has apiVersion and kind)
			if obj.contains_key("apiVersion") && obj.contains_key("kind") {
				// Skip Tanka Environment objects
				if let Some(JsonValue::String(kind)) = obj.get("kind") {
					if kind == "Environment" {
						// Extract from data field if present
						if let Some(data) = obj.get("data") {
							collect_manifests(data, manifests);
						}
						return;
					}
				}
				manifests.push(value.clone());
			} else {
				// Recurse into object values
				for v in obj.values() {
					collect_manifests(v, manifests);
				}
			}
		}
		JsonValue::Array(arr) => {
			// Recurse into array elements
			for v in arr {
				collect_manifests(v, manifests);
			}
		}
		_ => {}
	}
}

/// Check if a Kubernetes kind is cluster-wide (not namespaced)
/// This list is from Tanka's pkg/process/namespace.go
fn is_cluster_wide_kind(kind: &str) -> bool {
	matches!(
		kind,
		"APIService"
			| "CertificateSigningRequest"
			| "ClusterRole"
			| "ClusterRoleBinding"
			| "ComponentStatus"
			| "CSIDriver"
			| "CSINode"
			| "CustomResourceDefinition"
			| "MutatingWebhookConfiguration"
			| "Namespace"
			| "Node" | "NodeMetrics"
			| "PersistentVolume"
			| "PodSecurityPolicy"
			| "PriorityClass"
			| "RuntimeClass"
			| "SelfSubjectAccessReview"
			| "SelfSubjectRulesReview"
			| "StorageClass"
			| "SubjectAccessReview"
			| "TokenReview"
			| "ValidatingWebhookConfiguration"
			| "VolumeAttachment"
	)
}

/// Inject namespace into a manifest if needed (matching Tanka's pkg/process/namespace.go)
fn inject_namespace(manifest: &mut JsonValue, env_spec: &Option<crate::spec::Environment>) {
	if let JsonValue::Object(ref mut obj) = manifest {
		// Get kind and check if it's cluster-wide
		let kind = obj.get("kind").and_then(|v| v.as_str()).unwrap_or("");
		let is_cluster_wide = is_cluster_wide_kind(kind);

		// Ensure metadata exists
		if !obj.contains_key("metadata") {
			obj.insert(
				"metadata".to_string(),
				JsonValue::Object(serde_json::Map::new()),
			);
		}

		if let Some(JsonValue::Object(ref mut metadata)) = obj.get_mut("metadata") {
			// Check for annotation override (tanka.dev/namespaced)
			let mut namespaced = !is_cluster_wide;
			if let Some(JsonValue::Object(annotations)) = metadata.get("annotations") {
				if let Some(JsonValue::String(ns_str)) = annotations.get("tanka.dev/namespaced") {
					namespaced = ns_str == "true";
				}
			}

			// Inject namespace if needed
			if namespaced {
				let has_namespace = metadata.contains_key("namespace")
					&& metadata
						.get("namespace")
						.and_then(|v| v.as_str())
						.map(|s| !s.is_empty())
						.unwrap_or(false);

				if !has_namespace {
					if let Some(env) = env_spec {
						if !env.spec.namespace.is_empty() {
							metadata.insert(
								"namespace".to_string(),
								JsonValue::String(env.spec.namespace.clone()),
							);
						}
					}
				}
			}
		}
	}
}

/// Sanitize a string for use as a path component
fn sanitize_path_component(s: &str) -> String {
	s.chars()
		.map(|c| {
			if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
				c
			} else {
				'-'
			}
		})
		.collect()
}

/// Convert serde_json::Value to gtmpl::Value
fn json_to_gtmpl(value: &JsonValue) -> Value {
	match value {
		JsonValue::Null => Value::Nil,
		JsonValue::Bool(b) => Value::Bool(*b),
		JsonValue::Number(n) => {
			if let Some(i) = n.as_i64() {
				Value::Number(i.into())
			} else if let Some(f) = n.as_f64() {
				Value::Number(f.into())
			} else {
				Value::Nil
			}
		}
		JsonValue::String(s) => Value::String(s.clone()),
		JsonValue::Array(arr) => Value::Array(arr.iter().map(json_to_gtmpl).collect()),
		JsonValue::Object(obj) => {
			let map: HashMap<String, Value> = obj
				.iter()
				.map(|(k, v)| (k.clone(), json_to_gtmpl(v)))
				.collect();
			Value::Map(map)
		}
	}
}

// Thread-local storage for env value during template rendering
thread_local! {
	static ENV_VALUE: std::cell::RefCell<Option<Value>> = std::cell::RefCell::new(None);
}

// Function to return the env value for gtmpl templates
fn env_func(_args: &[Value]) -> Result<Value, gtmpl::FuncError> {
	ENV_VALUE.with(|env| {
		env.borrow()
			.clone()
			.ok_or_else(|| gtmpl::FuncError::Generic("env not available".to_string()))
	})
}

/// Format filename using Go text/template (gtmpl)
fn format_filename_gtmpl(
	manifest: &JsonValue,
	env_spec: &Option<crate::spec::Environment>,
	format: &str,
) -> Result<String> {
	use gtmpl::{Context, Template};

	// Create template
	let mut tmpl = Template::default();

	// Register env function if environment spec is available
	if let Some(env) = env_spec {
		// Clone and ensure labels is always an empty map if None
		let mut env_clone = env.clone();
		if env_clone.metadata.labels.is_none() {
			env_clone.metadata.labels = Some(std::collections::BTreeMap::new());
		}

		let env_json = serde_json::to_value(&env_clone)?;
		let env_value = json_to_gtmpl(&env_json);

		// Store env value in thread-local storage
		ENV_VALUE.with(|cell| {
			*cell.borrow_mut() = Some(env_value);
		});

		// Register env function
		tmpl.add_func("env", env_func as gtmpl::Func);
	}

	// Parse the template
	tmpl.parse(format)?;

	// Create context with manifest fields
	// Ensure metadata.labels exists as empty object and inject namespace if needed
	let mut manifest_clone = manifest.clone();
	if let JsonValue::Object(ref mut obj) = manifest_clone {
		// Get kind and check if it's cluster-wide
		let kind = obj
			.get("kind")
			.and_then(|v| v.as_str())
			.unwrap_or("")
			.to_string();
		let is_cluster_wide = is_cluster_wide_kind(&kind);

		// Ensure metadata exists
		if !obj.contains_key("metadata") {
			obj.insert(
				"metadata".to_string(),
				JsonValue::Object(serde_json::Map::new()),
			);
		}

		if let Some(JsonValue::Object(ref mut metadata)) = obj.get_mut("metadata") {
			// Ensure labels exists as empty object if not present
			// This prevents template errors when accessing .metadata.labels.field
			if !metadata.contains_key("labels") {
				metadata.insert(
					"labels".to_string(),
					JsonValue::Object(serde_json::Map::new()),
				);
			}

			// Check for annotation override (tanka.dev/namespaced)
			let mut namespaced = !is_cluster_wide;
			if let Some(JsonValue::Object(annotations)) = metadata.get("annotations") {
				if let Some(JsonValue::String(ns_str)) = annotations.get("tanka.dev/namespaced") {
					namespaced = ns_str == "true";
				}
			}

			// Inject namespace if needed (matching Tanka's behavior)
			if namespaced {
				let has_namespace = metadata.contains_key("namespace")
					&& metadata
						.get("namespace")
						.and_then(|v| v.as_str())
						.map(|s| !s.is_empty())
						.unwrap_or(false);

				if !has_namespace {
					if let Some(env) = env_spec {
						if !env.spec.namespace.is_empty() {
							metadata.insert(
								"namespace".to_string(),
								JsonValue::String(env.spec.namespace.clone()),
							);
						}
					}
				}
			}
		}
	}

	let mut context_map = HashMap::new();
	if let JsonValue::Object(obj) = manifest_clone {
		for (key, value) in obj {
			context_map.insert(key.clone(), json_to_gtmpl(&value));
		}
	}

	let context = Context::from(Value::Map(context_map));

	// Render template
	let result = tmpl
		.render(&context)
		.map_err(|e| anyhow::anyhow!("Template error: {:?}", e))?;

	// Clean up thread-local storage
	ENV_VALUE.with(|cell| {
		*cell.borrow_mut() = None;
	});

	// Clean up empty segments (from missing optional fields)
	let cleaned: String = result
		.split('.')
		.filter(|s| !s.is_empty() && *s != "<no value>")
		.collect::<Vec<_>>()
		.join(".");

	if cleaned.is_empty() {
		bail!("Template produced empty filename");
	}

	Ok(cleaned)
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	fn setup_test_env(temp: &TempDir, name: &str, content: &str) -> PathBuf {
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		let env_path = root.join(format!("environments/{}", name));
		fs::create_dir_all(&env_path).unwrap();
		fs::write(env_path.join("main.jsonnet"), content).unwrap();

		env_path
	}

	#[test]
	fn test_validate_filename_template_valid() {
		// Default Go template format
		assert!(validate_filename_template(
			"{{.apiVersion}}.{{.kind}}-{{or .metadata.name .metadata.generateName}}"
		)
		.is_ok());

		// Simple format
		assert!(validate_filename_template("{{.kind}}-{{.metadata.name}}").is_ok());

		// Just kind
		assert!(validate_filename_template("{{.kind}}").is_ok());
	}

	#[test]
	fn test_validate_filename_template_invalid() {
		// Invalid Go template syntax
		assert!(validate_filename_template("{{.invalid syntax").is_err());

		// Unclosed braces
		assert!(validate_filename_template("{{.kind}").is_err());
	}

	#[test]
	fn test_format_filename_gtmpl_basic() {
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "my-config" }
		});

		let result =
			format_filename_gtmpl(&manifest, &None, "{{.kind}}-{{.metadata.name}}").unwrap();
		assert_eq!(result, "ConfigMap-my-config");
	}

	#[test]
	fn test_format_filename_gtmpl_with_or() {
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "my-config" }
		});

		let result = format_filename_gtmpl(
			&manifest,
			&None,
			"{{.kind}}-{{or .metadata.name .metadata.generateName}}",
		)
		.unwrap();
		assert_eq!(result, "ConfigMap-my-config");
	}

	#[test]
	fn test_format_filename_gtmpl_with_or_fallback() {
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "Pod",
			"metadata": { "generateName": "job-" }
		});

		let result = format_filename_gtmpl(
			&manifest,
			&None,
			"{{.kind}}-{{or .metadata.name .metadata.generateName}}",
		)
		.unwrap();
		assert_eq!(result, "Pod-job-");
	}

	#[test]
	fn test_format_filename_gtmpl_full_default() {
		let manifest = serde_json::json!({
			"apiVersion": "apps/v1",
			"kind": "Deployment",
			"metadata": { "name": "nginx" }
		});

		let result = format_filename_gtmpl(
			&manifest,
			&None,
			"{{.apiVersion}}.{{.kind}}-{{or .metadata.name .metadata.generateName}}",
		)
		.unwrap();
		assert_eq!(result, "apps/v1.Deployment-nginx");
	}

	#[test]
	fn test_json_to_gtmpl_object() {
		let json = serde_json::json!({
			"name": "test",
			"count": 42,
			"enabled": true
		});

		let gtmpl_val = json_to_gtmpl(&json);
		assert!(matches!(gtmpl_val, Value::Map(_)));
	}

	#[test]
	fn test_json_to_gtmpl_nested() {
		let json = serde_json::json!({
			"metadata": {
				"name": "test"
			}
		});

		let gtmpl_val = json_to_gtmpl(&json);
		if let Value::Map(map) = gtmpl_val {
			assert!(map.contains_key("metadata"));
		} else {
			panic!("Expected Map");
		}
	}

	#[test]
	fn test_extract_manifests_single() {
		let value = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "test" }
		});

		let manifests = extract_manifests(&value).unwrap();
		assert_eq!(manifests.len(), 1);
	}

	#[test]
	fn test_extract_manifests_nested() {
		let value = serde_json::json!({
			"configmap": {
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "test" }
			},
			"service": {
				"apiVersion": "v1",
				"kind": "Service",
				"metadata": { "name": "test-svc" }
			}
		});

		let manifests = extract_manifests(&value).unwrap();
		assert_eq!(manifests.len(), 2);
	}

	#[test]
	fn test_extract_manifests_array() {
		let value = serde_json::json!([
			{
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "test1" }
			},
			{
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "test2" }
			}
		]);

		let manifests = extract_manifests(&value).unwrap();
		assert_eq!(manifests.len(), 2);
	}

	#[test]
	fn test_sanitize_path_component() {
		assert_eq!(sanitize_path_component("hello-world"), "hello-world");
		assert_eq!(sanitize_path_component("hello/world"), "hello-world");
		assert_eq!(sanitize_path_component("hello:world"), "hello-world");
		assert_eq!(sanitize_path_component("my_app"), "my_app");
	}

	#[test]
	fn test_export_simple_env() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			"test",
			r#"{
				apiVersion: "v1",
				kind: "ConfigMap",
				metadata: { name: "test-config" },
				data: { key: "value" }
			}"#,
		);

		let output_dir = temp.path().join("output");
		let opts = ExportOpts {
			output_dir: output_dir.clone(),
			extension: "yaml".to_string(),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			parallelism: 1,
			eval_opts: EvalOpts::default(),
			name: None,
			recursive: true, // Allow single env
			skip_manifest: false,
		};

		let result = export(&[env_path.to_string_lossy().to_string()], opts).unwrap();

		assert_eq!(result.total_envs, 1);
		assert_eq!(result.successful, 1);
		assert_eq!(result.failed, 0);
	}

	#[test]
	fn test_export_json_format() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			"test",
			r#"{
				apiVersion: "v1",
				kind: "ConfigMap",
				metadata: { name: "test-config" },
				data: { key: "value" }
			}"#,
		);

		let output_dir = temp.path().join("output");
		let opts = ExportOpts {
			output_dir: output_dir.clone(),
			extension: "json".to_string(),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			parallelism: 1,
			eval_opts: EvalOpts::default(),
			name: None,
			recursive: true,
			skip_manifest: false,
		};

		let result = export(&[env_path.to_string_lossy().to_string()], opts).unwrap();

		assert_eq!(result.total_envs, 1);
		assert_eq!(result.successful, 1);

		// Verify file has .json extension
		let files: Vec<_> = result.results[0].files_written.iter().collect();
		assert!(files.iter().any(|f| f.extension().unwrap() == "json"));
	}

	#[test]
	fn test_export_empty_paths() {
		let opts = ExportOpts::default();
		let result = export(&[], opts).unwrap();

		assert_eq!(result.total_envs, 0);
		assert_eq!(result.successful, 0);
		assert_eq!(result.failed, 0);
	}

	#[test]
	fn test_export_multiple_manifests() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			"multi",
			r#"{
				configmap: {
					apiVersion: "v1",
					kind: "ConfigMap",
					metadata: { name: "config" },
					data: { key: "value" }
				},
				deployment: {
					apiVersion: "apps/v1",
					kind: "Deployment",
					metadata: { name: "app" },
					spec: {}
				}
			}"#,
		);

		let output_dir = temp.path().join("output");
		let opts = ExportOpts {
			output_dir: output_dir.clone(),
			extension: "yaml".to_string(),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			parallelism: 1,
			eval_opts: EvalOpts::default(),
			name: None,
			recursive: true,
			skip_manifest: false,
		};

		let result = export(&[env_path.to_string_lossy().to_string()], opts).unwrap();

		assert_eq!(result.total_envs, 1);
		assert_eq!(result.successful, 1);
		// Should have 2 files (one per manifest)
		assert_eq!(result.results[0].files_written.len(), 2);
	}

	#[test]
	fn test_extract_manifests_deeply_nested() {
		let value = serde_json::json!({
			"level1": {
				"level2": {
					"apiVersion": "v1",
					"kind": "ConfigMap",
					"metadata": { "name": "nested" }
				}
			}
		});

		let manifests = extract_manifests(&value).unwrap();
		assert_eq!(manifests.len(), 1);
	}

	#[test]
	fn test_extract_manifests_mixed() {
		// Mix of direct manifests and nested ones
		let value = serde_json::json!({
			"direct": {
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "direct" }
			},
			"nested": {
				"inner": {
					"apiVersion": "v1",
					"kind": "Secret",
					"metadata": { "name": "nested" }
				}
			}
		});

		let manifests = extract_manifests(&value).unwrap();
		assert_eq!(manifests.len(), 2);
	}

	#[test]
	fn test_sanitize_path_special_chars() {
		assert_eq!(sanitize_path_component("a/b\\c:d"), "a-b-c-d");
		assert_eq!(sanitize_path_component("test..path"), "test..path");
		assert_eq!(
			sanitize_path_component("normal-name_123"),
			"normal-name_123"
		);
	}

	#[test]
	fn test_export_opts_default() {
		let opts = ExportOpts::default();
		assert_eq!(opts.extension, "yaml");
		assert_eq!(opts.parallelism, 8);
		assert_eq!(
			opts.format,
			"{{.apiVersion}}.{{.kind}}-{{or .metadata.name .metadata.generateName}}"
		);
	}

	#[test]
	fn test_export_multi_env_without_recursive_fails() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create two environments
		let env1 = root.join("environments/env1");
		let env2 = root.join("environments/env2");
		fs::create_dir_all(&env1).unwrap();
		fs::create_dir_all(&env2).unwrap();
		fs::write(
			env1.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "c1" } }"#,
		)
		.unwrap();
		fs::write(
			env2.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "c2" } }"#,
		)
		.unwrap();

		let opts = ExportOpts {
			output_dir: temp.path().join("output"),
			recursive: false, // Not recursive
			name: None,       // No name filter
			..Default::default()
		};

		// Should fail with multiple environments
		let result = export(
			&[root.join("environments").to_string_lossy().to_string()],
			opts,
		);
		assert!(result.is_err());
		let err_msg = result.unwrap_err().to_string();
		assert!(err_msg.contains("Found 2 environments"));
	}

	#[test]
	fn test_export_multi_env_with_recursive_succeeds() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create two environments
		let env1 = root.join("environments/env1");
		let env2 = root.join("environments/env2");
		fs::create_dir_all(&env1).unwrap();
		fs::create_dir_all(&env2).unwrap();
		fs::write(
			env1.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "c1" } }"#,
		)
		.unwrap();
		fs::write(
			env2.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "c2" } }"#,
		)
		.unwrap();

		let opts = ExportOpts {
			output_dir: temp.path().join("output"),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			recursive: true, // Recursive mode
			..Default::default()
		};

		let result = export(
			&[root.join("environments").to_string_lossy().to_string()],
			opts,
		);
		assert!(result.is_ok());
		let result = result.unwrap();
		assert_eq!(result.total_envs, 2);
		assert_eq!(result.successful, 2);
	}

	// ==================== ISSUE 1: Go Template Compatibility Tests ====================

	#[test]
	fn test_gtmpl_nested_field_access() {
		// Test deeply nested field access like {{.metadata.labels.app}}
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "Service",
			"metadata": {
				"name": "my-service",
				"labels": {
					"app": "nginx",
					"tier": "frontend"
				}
			}
		});

		let result =
			format_filename_gtmpl(&manifest, &None, "{{.metadata.labels.app}}-{{.kind}}").unwrap();
		assert_eq!(result, "nginx-Service");
	}

	#[test]
	fn test_gtmpl_or_with_missing_first_field() {
		// Test {{or .a .b}} when first field is missing
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "Job",
			"metadata": {
				"generateName": "batch-job-"
			}
		});

		let result = format_filename_gtmpl(
			&manifest,
			&None,
			"{{.kind}}-{{or .metadata.name .metadata.generateName}}",
		)
		.unwrap();
		assert_eq!(result, "Job-batch-job-");
	}

	#[test]
	fn test_gtmpl_or_with_both_fields_present() {
		// Test {{or .a .b}} when both fields exist (should use first)
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "Pod",
			"metadata": {
				"name": "my-pod",
				"generateName": "should-not-use-"
			}
		});

		let result = format_filename_gtmpl(
			&manifest,
			&None,
			"{{.kind}}-{{or .metadata.name .metadata.generateName}}",
		)
		.unwrap();
		assert_eq!(result, "Pod-my-pod");
	}

	#[test]
	fn test_gtmpl_apiversion_with_slash() {
		// Test apiVersion like "apps/v1" or "networking.k8s.io/v1"
		let manifest = serde_json::json!({
			"apiVersion": "networking.k8s.io/v1",
			"kind": "Ingress",
			"metadata": { "name": "my-ingress" }
		});

		let result = format_filename_gtmpl(
			&manifest,
			&None,
			"{{.apiVersion}}.{{.kind}}-{{.metadata.name}}",
		)
		.unwrap();
		assert_eq!(result, "networking.k8s.io/v1.Ingress-my-ingress");
	}

	#[test]
	fn test_gtmpl_special_characters_in_name() {
		// Test names with special characters
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "my-config-map.v2" }
		});

		let result =
			format_filename_gtmpl(&manifest, &None, "{{.kind}}-{{.metadata.name}}").unwrap();
		assert_eq!(result, "ConfigMap-my-config-map.v2");
	}

	#[test]
	fn test_gtmpl_default_tanka_format() {
		// Test the exact default format Tanka uses
		let manifest = serde_json::json!({
			"apiVersion": "apps/v1",
			"kind": "Deployment",
			"metadata": { "name": "nginx-deployment" }
		});

		let result = format_filename_gtmpl(
			&manifest,
			&None,
			"{{.apiVersion}}.{{.kind}}-{{or .metadata.name .metadata.generateName}}",
		)
		.unwrap();
		assert_eq!(result, "apps/v1.Deployment-nginx-deployment");
	}

	// ==================== ISSUE 2: Fail-Fast Validation Tests ====================

	#[test]
	fn test_fail_fast_invalid_template_before_processing() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create a valid environment
		let env = root.join("environments/test");
		fs::create_dir_all(&env).unwrap();
		fs::write(
			env.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "test" } }"#,
		)
		.unwrap();

		// Use invalid template syntax
		let opts = ExportOpts {
			output_dir: temp.path().join("output"),
			format: "{{.invalid syntax".to_string(), // Invalid!
			recursive: true,
			..Default::default()
		};

		let result = export(&[env.to_string_lossy().to_string()], opts);

		// Should fail with template error, not evaluation error
		assert!(result.is_err());
		let err = result.unwrap_err().to_string();
		assert!(
			err.contains("template") || err.contains("Template"),
			"Error should mention template: {}",
			err
		);
	}

	#[test]
	fn test_fail_fast_unclosed_braces() {
		// gtmpl should reject obviously broken templates
		assert!(validate_filename_template("{{.kind}").is_err());
		assert!(validate_filename_template("{{.kind").is_err());
		// Note: {.kind}} is technically valid (literal "{" + ".kind}}")
	}

	#[test]
	fn test_fail_fast_valid_complex_templates() {
		// Various valid Go template patterns
		assert!(validate_filename_template("{{.apiVersion}}").is_ok());
		assert!(validate_filename_template("{{.kind}}-{{.metadata.name}}").is_ok());
		assert!(validate_filename_template("{{or .metadata.name .metadata.generateName}}").is_ok());
		assert!(validate_filename_template(
			"{{.apiVersion}}.{{.kind}}-{{or .metadata.name .metadata.generateName}}"
		)
		.is_ok());
	}

	// ==================== ISSUE 4: Multi-Environment Check Tests ====================

	#[test]
	fn test_multi_env_with_name_filter_succeeds() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create multiple environments
		let env1 = root.join("environments/prod-env");
		let env2 = root.join("environments/staging-env");
		fs::create_dir_all(&env1).unwrap();
		fs::create_dir_all(&env2).unwrap();
		fs::write(
			env1.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "prod" } }"#,
		)
		.unwrap();
		fs::write(
			env2.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "staging" } }"#,
		)
		.unwrap();

		// Use name filter to select only prod
		let opts = ExportOpts {
			output_dir: temp.path().join("output"),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			name: Some("prod".to_string()),
			recursive: false, // Not recursive, but name filter should work
			..Default::default()
		};

		let result = export(
			&[root.join("environments").to_string_lossy().to_string()],
			opts,
		);
		assert!(result.is_ok());
		let result = result.unwrap();
		assert_eq!(result.total_envs, 1);
		assert_eq!(result.successful, 1);
	}

	#[test]
	fn test_multi_env_name_filter_no_match() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create environments
		let env1 = root.join("environments/prod");
		let env2 = root.join("environments/staging");
		fs::create_dir_all(&env1).unwrap();
		fs::create_dir_all(&env2).unwrap();
		fs::write(
			env1.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "c1" } }"#,
		)
		.unwrap();
		fs::write(
			env2.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "c2" } }"#,
		)
		.unwrap();

		// Use name filter that matches nothing
		let opts = ExportOpts {
			output_dir: temp.path().join("output"),
			name: Some("nonexistent".to_string()),
			recursive: false,
			..Default::default()
		};

		let result = export(
			&[root.join("environments").to_string_lossy().to_string()],
			opts,
		);
		assert!(result.is_err());
		let err = result.unwrap_err().to_string();
		assert!(err.contains("No environments found"));
	}

	#[test]
	fn test_single_env_without_recursive_succeeds() {
		// Single environment should work without --recursive
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			"single",
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "test" } }"#,
		);

		let opts = ExportOpts {
			output_dir: temp.path().join("output"),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			recursive: false, // Not recursive
			name: None,
			..Default::default()
		};

		let result = export(&[env_path.to_string_lossy().to_string()], opts);
		assert!(result.is_ok());
		assert_eq!(result.unwrap().successful, 1);
	}

	// ==================== ISSUE 3 & 5: Error Handling Tests ====================

	#[test]
	fn test_export_continues_on_per_env_errors() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create one valid and one invalid environment
		let valid_env = root.join("environments/valid");
		let invalid_env = root.join("environments/invalid");
		fs::create_dir_all(&valid_env).unwrap();
		fs::create_dir_all(&invalid_env).unwrap();

		fs::write(
			valid_env.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "valid" } }"#,
		)
		.unwrap();
		// Invalid jsonnet syntax
		fs::write(invalid_env.join("main.jsonnet"), r#"{ invalid jsonnet }"#).unwrap();

		let opts = ExportOpts {
			output_dir: temp.path().join("output"),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			recursive: true,
			parallelism: 1, // Single thread to ensure predictable order
			..Default::default()
		};

		let result = export(
			&[root.join("environments").to_string_lossy().to_string()],
			opts,
		)
		.unwrap();

		// Should have processed both, with one failure
		assert_eq!(result.total_envs, 2);
		assert_eq!(result.successful, 1);
		assert_eq!(result.failed, 1);
	}

	#[test]
	fn test_export_result_contains_error_details() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create invalid environment
		let invalid_env = root.join("environments/broken");
		fs::create_dir_all(&invalid_env).unwrap();
		fs::write(invalid_env.join("main.jsonnet"), r#"syntax error here {"#).unwrap();

		let opts = ExportOpts {
			output_dir: temp.path().join("output"),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			recursive: true,
			..Default::default()
		};

		let result = export(&[invalid_env.to_string_lossy().to_string()], opts).unwrap();

		assert_eq!(result.failed, 1);
		assert!(result.results[0].error.is_some());
		// Error message should contain useful info
		let error_msg = result.results[0].error.as_ref().unwrap();
		assert!(!error_msg.is_empty());
	}

	// ==================== JSON to GTMPL Conversion Tests ====================

	#[test]
	fn test_json_to_gtmpl_all_types() {
		// Test all JSON types convert correctly
		let json = serde_json::json!({
			"string": "hello",
			"number_int": 42,
			"number_float": 3.14,
			"boolean": true,
			"null_value": null,
			"array": [1, 2, 3],
			"nested": {
				"key": "value"
			}
		});

		let gtmpl_val = json_to_gtmpl(&json);
		assert!(matches!(gtmpl_val, Value::Map(_)));

		if let Value::Map(map) = gtmpl_val {
			assert!(matches!(map.get("string"), Some(Value::String(_))));
			assert!(matches!(map.get("number_int"), Some(Value::Number(_))));
			assert!(matches!(map.get("boolean"), Some(Value::Bool(true))));
			assert!(matches!(map.get("null_value"), Some(Value::Nil)));
			assert!(matches!(map.get("array"), Some(Value::Array(_))));
			assert!(matches!(map.get("nested"), Some(Value::Map(_))));
		}
	}

	#[test]
	fn test_json_to_gtmpl_empty_values() {
		let json = serde_json::json!({
			"empty_string": "",
			"empty_array": [],
			"empty_object": {}
		});

		let gtmpl_val = json_to_gtmpl(&json);
		assert!(matches!(gtmpl_val, Value::Map(_)));
	}

	// ==================== Edge Cases ====================

	#[test]
	fn test_export_env_with_no_manifests() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp, "empty", r#"{}"#, // Empty object, no K8s manifests
		);

		let opts = ExportOpts {
			output_dir: temp.path().join("output"),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			recursive: true,
			..Default::default()
		};

		let result = export(&[env_path.to_string_lossy().to_string()], opts).unwrap();

		assert_eq!(result.successful, 1);
		assert_eq!(result.results[0].files_written.len(), 0);
	}

	#[test]
	fn test_extract_manifests_tanka_environment_wrapper() {
		// Test extracting from Tanka Environment wrapper object
		let value = serde_json::json!({
			"apiVersion": "tanka.dev/v1alpha1",
			"kind": "Environment",
			"metadata": { "name": "prod" },
			"data": {
				"configmap": {
					"apiVersion": "v1",
					"kind": "ConfigMap",
					"metadata": { "name": "app-config" }
				}
			}
		});

		let manifests = extract_manifests(&value).unwrap();
		assert_eq!(manifests.len(), 1);
		assert_eq!(manifests[0]["kind"], "ConfigMap");
	}

	#[test]
	fn test_sanitize_path_unicode() {
		// Note: Rust's is_alphanumeric() returns true for Unicode letters like é, ö
		// This is actually correct behavior - these are valid in paths
		assert_eq!(sanitize_path_component("héllo-wörld"), "héllo-wörld");
		// CJK characters are also alphanumeric in Unicode
		assert_eq!(sanitize_path_component("日本語"), "日本語");
		// But special chars like emojis should be replaced
		assert_eq!(sanitize_path_component("test🚀name"), "test-name");
	}

	#[test]
	fn test_sanitize_path_preserves_valid_chars() {
		// Valid chars: alphanumeric, -, _, .
		assert_eq!(
			sanitize_path_component("Valid-Name_123.yaml"),
			"Valid-Name_123.yaml"
		);
	}

	#[test]
	fn test_count_environment_objects_single() {
		let value = serde_json::json!({
			"apiVersion": "tanka.dev/v1alpha1",
			"kind": "Environment",
			"metadata": { "name": "prod" }
		});
		assert_eq!(count_environment_objects(&value), 1);
	}

	#[test]
	fn test_count_environment_objects_multiple() {
		let value = serde_json::json!({
			"env1": {
				"apiVersion": "tanka.dev/v1alpha1",
				"kind": "Environment",
				"metadata": { "name": "prod" }
			},
			"env2": {
				"apiVersion": "tanka.dev/v1alpha1",
				"kind": "Environment",
				"metadata": { "name": "staging" }
			}
		});
		assert_eq!(count_environment_objects(&value), 2);
	}

	#[test]
	fn test_count_environment_objects_nested() {
		let value = serde_json::json!({
			"level1": {
				"level2": {
					"apiVersion": "tanka.dev/v1alpha1",
					"kind": "Environment",
					"metadata": { "name": "nested" }
				}
			}
		});
		assert_eq!(count_environment_objects(&value), 1);
	}

	#[test]
	fn test_count_environment_objects_none() {
		let value = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "test" }
		});
		assert_eq!(count_environment_objects(&value), 0);
	}

	#[test]
	fn test_count_environment_objects_in_array() {
		let value = serde_json::json!([
			{
				"apiVersion": "tanka.dev/v1alpha1",
				"kind": "Environment",
				"metadata": { "name": "env1" }
			},
			{
				"apiVersion": "tanka.dev/v1alpha1",
				"kind": "Environment",
				"metadata": { "name": "env2" }
			}
		]);
		assert_eq!(count_environment_objects(&value), 2);
	}

	// ==================== Additional Edge Case Tests ====================

	#[test]
	fn test_export_with_recursive_flag_processes_all() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create 3 environments
		for i in 1..=3 {
			let env = root.join(format!("environments/env{}", i));
			fs::create_dir_all(&env).unwrap();
			fs::write(
				env.join("main.jsonnet"),
				format!(
					r#"{{ apiVersion: "v1", kind: "ConfigMap", metadata: {{ name: "config{}" }} }}"#,
					i
				),
			)
			.unwrap();
		}

		let opts = ExportOpts {
			output_dir: temp.path().join("output"),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			recursive: true,
			..Default::default()
		};

		let result = export(
			&[root.join("environments").to_string_lossy().to_string()],
			opts,
		)
		.unwrap();

		assert_eq!(result.total_envs, 3);
		assert_eq!(result.successful, 3);
		assert_eq!(result.failed, 0);
	}

	#[test]
	fn test_export_parallel_produces_consistent_results() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create multiple environments
		for i in 1..=5 {
			let env = root.join(format!("environments/env{}", i));
			fs::create_dir_all(&env).unwrap();
			fs::write(
				env.join("main.jsonnet"),
				format!(
					r#"{{ apiVersion: "v1", kind: "ConfigMap", metadata: {{ name: "config{}" }} }}"#,
					i
				),
			)
			.unwrap();
		}

		// Run with different parallelism levels
		for parallelism in [1, 2, 4] {
			let output_dir = temp.path().join(format!("output-p{}", parallelism));
			let opts = ExportOpts {
				output_dir: output_dir.clone(),
				format: "{{.kind}}-{{.metadata.name}}".to_string(),
				recursive: true,
				parallelism,
				..Default::default()
			};

			let result = export(
				&[root.join("environments").to_string_lossy().to_string()],
				opts,
			)
			.unwrap();

			assert_eq!(
				result.total_envs, 5,
				"parallelism {} should find all envs",
				parallelism
			);
			assert_eq!(
				result.successful, 5,
				"parallelism {} should succeed for all",
				parallelism
			);
		}
	}

	#[test]
	fn test_gtmpl_empty_metadata_name() {
		// Test when metadata.name is empty string
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "" }
		});

		// Should not fail, just produce empty segment
		let result = format_filename_gtmpl(&manifest, &None, "{{.kind}}-{{.metadata.name}}");
		assert!(result.is_ok());
	}

	#[test]
	fn test_gtmpl_missing_metadata() {
		// Test when metadata is completely missing
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap"
		});

		let result = format_filename_gtmpl(&manifest, &None, "{{.kind}}");
		assert!(result.is_ok());
		assert_eq!(result.unwrap(), "ConfigMap");
	}

	#[test]
	fn test_export_creates_nested_directories() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			"test",
			r#"{
				apiVersion: "v1",
				kind: "ConfigMap",
				metadata: { name: "test-config", namespace: "my-namespace" },
				data: { key: "value" }
			}"#,
		);

		let output_dir = temp.path().join("output");
		let opts = ExportOpts {
			output_dir: output_dir.clone(),
			format: "{{.metadata.namespace}}/{{.kind}}-{{.metadata.name}}".to_string(),
			recursive: true,
			..Default::default()
		};

		let result = export(&[env_path.to_string_lossy().to_string()], opts).unwrap();

		assert_eq!(result.successful, 1);
		// Verify nested directory was created (namespace becomes part of path after sanitization)
		let files = &result.results[0].files_written;
		assert_eq!(files.len(), 1);
	}

	#[test]
	fn test_export_yaml_vs_json_extension() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			"test",
			r#"{
				apiVersion: "v1",
				kind: "ConfigMap",
				metadata: { name: "test" },
				data: { key: "value" }
			}"#,
		);

		// Test YAML
		let yaml_output = temp.path().join("yaml-output");
		let yaml_opts = ExportOpts {
			output_dir: yaml_output.clone(),
			extension: "yaml".to_string(),
			format: "{{.kind}}".to_string(),
			recursive: true,
			..Default::default()
		};
		let yaml_result = export(&[env_path.to_string_lossy().to_string()], yaml_opts).unwrap();
		assert!(yaml_result.results[0].files_written[0]
			.to_string_lossy()
			.ends_with(".yaml"));

		// Test JSON
		let json_output = temp.path().join("json-output");
		let json_opts = ExportOpts {
			output_dir: json_output.clone(),
			extension: "json".to_string(),
			format: "{{.kind}}".to_string(),
			recursive: true,
			..Default::default()
		};
		let json_result = export(&[env_path.to_string_lossy().to_string()], json_opts).unwrap();
		assert!(json_result.results[0].files_written[0]
			.to_string_lossy()
			.ends_with(".json"));
	}

	#[test]
	fn test_gtmpl_env_function() {
		// Test that env variable works in templates
		use crate::spec::{Environment, Metadata, Spec};
		use std::collections::BTreeMap;

		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "test-config" }
		});

		let mut labels = BTreeMap::new();
		labels.insert("cluster_name".to_string(), "prod-cluster".to_string());
		labels.insert("team".to_string(), "platform".to_string());

		let env = Some(Environment {
			api_version: "tanka.dev/v1alpha1".to_string(),
			kind: "Environment".to_string(),
			metadata: Metadata {
				name: Some("test-env".to_string()),
				namespace: Some("default".to_string()),
				labels: Some(labels),
			},
			spec: Spec {
				api_server: None,
				context_names: None,
				namespace: "default".to_string(),
				diff_strategy: None,
				apply_strategy: None,
				inject_labels: None,
				resource_defaults: None,
				expect_versions: None,
				export_jsonnet_implementation: None,
			},
			data: None,
		});

		// Test accessing env.metadata.labels
		let result = format_filename_gtmpl(
			&manifest,
			&env,
			"{{env.metadata.labels.cluster_name}}/{{.kind}}-{{.metadata.name}}",
		)
		.unwrap();
		assert_eq!(result, "prod-cluster/ConfigMap-test-config");

		// Test accessing env.metadata.name
		let result2 =
			format_filename_gtmpl(&manifest, &env, "{{env.metadata.name}}/{{.kind}}").unwrap();
		assert_eq!(result2, "test-env/ConfigMap");
	}

	// ==================== Manifest.json Tests ====================

	#[test]
	fn test_manifest_json_generated() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			"test",
			r#"{
				apiVersion: "v1",
				kind: "ConfigMap",
				metadata: { name: "test-config" },
				data: { key: "value" }
			}"#,
		);

		let output_dir = temp.path().join("output");
		let opts = ExportOpts {
			output_dir: output_dir.clone(),
			extension: "yaml".to_string(),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			parallelism: 1,
			eval_opts: EvalOpts::default(),
			name: None,
			recursive: true,
			skip_manifest: false,
		};

		let _ = export(&[env_path.to_string_lossy().to_string()], opts).unwrap();

		// Check manifest.json exists
		let manifest_path = output_dir.join(MANIFEST_FILE);
		assert!(manifest_path.exists(), "manifest.json should exist");

		// Read and verify contents
		let manifest_content = fs::read_to_string(&manifest_path).unwrap();
		let manifest_map: HashMap<String, String> =
			serde_json::from_str(&manifest_content).unwrap();

		// Should have one entry
		assert_eq!(manifest_map.len(), 1);

		// Entry should map the file to the environment path
		let expected_file = "ConfigMap-test-config.yaml";
		assert!(
			manifest_map.contains_key(expected_file),
			"manifest.json should contain the exported file"
		);
	}

	#[test]
	fn test_manifest_json_skipped() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			"test",
			r#"{
				apiVersion: "v1",
				kind: "ConfigMap",
				metadata: { name: "test-config" },
				data: { key: "value" }
			}"#,
		);

		let output_dir = temp.path().join("output");
		let opts = ExportOpts {
			output_dir: output_dir.clone(),
			extension: "yaml".to_string(),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			parallelism: 1,
			eval_opts: EvalOpts::default(),
			name: None,
			recursive: true,
			skip_manifest: true,
		};

		let _ = export(&[env_path.to_string_lossy().to_string()], opts).unwrap();

		// Check manifest.json does not exist
		let manifest_path = output_dir.join(MANIFEST_FILE);
		assert!(
			!manifest_path.exists(),
			"manifest.json should not exist when skip_manifest is true"
		);
	}

	#[test]
	fn test_manifest_json_merges_with_existing() {
		let temp = TempDir::new().unwrap();
		let output_dir = temp.path().join("output");
		fs::create_dir_all(&output_dir).unwrap();

		// Create existing manifest.json
		let existing_manifest = serde_json::json!({
			"old-file.yaml": "old-env"
		});
		fs::write(
			output_dir.join(MANIFEST_FILE),
			serde_json::to_string_pretty(&existing_manifest).unwrap(),
		)
		.unwrap();

		// Export a new environment
		let env_path = setup_test_env(
			&temp,
			"test",
			r#"{
				apiVersion: "v1",
				kind: "ConfigMap",
				metadata: { name: "new-config" },
				data: { key: "value" }
			}"#,
		);

		let opts = ExportOpts {
			output_dir: output_dir.clone(),
			extension: "yaml".to_string(),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			parallelism: 1,
			eval_opts: EvalOpts::default(),
			name: None,
			recursive: true,
			skip_manifest: false,
		};

		let _ = export(&[env_path.to_string_lossy().to_string()], opts).unwrap();

		// Read manifest.json
		let manifest_content = fs::read_to_string(output_dir.join(MANIFEST_FILE)).unwrap();
		let manifest_map: HashMap<String, String> =
			serde_json::from_str(&manifest_content).unwrap();

		// Should have both entries
		assert_eq!(manifest_map.len(), 2);
		assert!(manifest_map.contains_key("old-file.yaml"));
		assert!(manifest_map.contains_key("ConfigMap-new-config.yaml"));
	}

	#[test]
	fn test_manifest_json_with_multiple_envs() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create two environments
		let env1 = root.join("environments/env1");
		let env2 = root.join("environments/env2");
		fs::create_dir_all(&env1).unwrap();
		fs::create_dir_all(&env2).unwrap();
		fs::write(
			env1.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "config1" } }"#,
		)
		.unwrap();
		fs::write(
			env2.join("main.jsonnet"),
			r#"{ apiVersion: "v1", kind: "Secret", metadata: { name: "secret1" } }"#,
		)
		.unwrap();

		let output_dir = temp.path().join("output");
		let opts = ExportOpts {
			output_dir: output_dir.clone(),
			format: "{{.kind}}-{{.metadata.name}}".to_string(),
			recursive: true,
			skip_manifest: false,
			..Default::default()
		};

		let _ = export(
			&[root.join("environments").to_string_lossy().to_string()],
			opts,
		)
		.unwrap();

		// Read manifest.json
		let manifest_path = output_dir.join(MANIFEST_FILE);
		assert!(manifest_path.exists());

		let manifest_content = fs::read_to_string(&manifest_path).unwrap();
		let manifest_map: HashMap<String, String> =
			serde_json::from_str(&manifest_content).unwrap();

		// Should have entries for both environments
		assert_eq!(manifest_map.len(), 2);
		assert!(manifest_map.contains_key("ConfigMap-config1.yaml"));
		assert!(manifest_map.contains_key("Secret-secret1.yaml"));
	}
}
