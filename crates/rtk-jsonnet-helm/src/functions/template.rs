use std::borrow::Cow;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;

use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::Context;
use rtk_jsonnet_core::EvaluatorError as _;
use rustc_hash::{FxBuildHasher, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::State;

#[derive(Debug)]
pub struct Function {
	state: &'static State,
}

impl Function {
	pub(crate) fn new(state: &'static State) -> Function {
		Function { state }
	}
}

impl<E> jsonnet::Function<E> for Function
where
	E: jsonnet::Evaluator<Context = E> + Context<Evaluator = E>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(3, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["name", "chart", "opts"])
	}

	fn call<'b>(&self, evaluator: &E, arguments: E::Arguments) -> Result<E::Value, E::Error> {
		let (name, chart, options) = <(String, String, Options)>::deserialize(arguments)?;

		let called_from = &options.called_from;

		let mut chart_path = {
			if called_from.is_empty() {
				return Err(E::Error::custom("calledFrom cannot be an empty string"));
			}

			let called_from_path: &Path = called_from.as_ref();

			let Some(called_from_dir) = called_from_path.parent() else {
				return Err(E::Error::custom(format!(
					"calledFrom has no parent directory: {called_from}"
				)));
			};

			if !called_from_dir.exists() {
				return Err(E::Error::custom(format!(
					"calledFrom directory does not exist: {}",
					called_from_dir.display()
				)));
			}

			let chart_relative = if chart.starts_with('/') {
				Cow::Owned(format!(".{chart}"))
			} else {
				Cow::Borrowed(chart.as_str())
			};
			let chart_absolute = called_from_dir.join(&*chart_relative);

			if !chart_absolute.exists() {
				return Err(E::Error::custom(format!(
					"chart path does not exist: {}",
					chart_absolute.display()
				)));
			}

			chart_absolute
		};

		// Benchmarking escape hatch: when RTK_HELM_DISABLE_MEMOIZATION is set,
		// bypass the in-memory cache entirely so every helmTemplate call invokes
		// helm. Used to measure the true cost of helm rendering without any
		// deduplication.
		let cache_disabled = env::var_os("RTK_HELM_DISABLE_MEMOIZATION").is_some();

		let cache_key = if cache_disabled {
			None
		} else {
			chart_path.push("Chart.yaml");
			let chart_meta = fs::read_to_string(&chart_path).ok();
			chart_path.pop();

			let cache_key = State::cache_key(&name, &chart_path, chart_meta.as_deref(), &options);

			let cache_read = self
				.state
				.template_cache
				.read()
				.unwrap_or_else(std::sync::PoisonError::into_inner);

			let cached_json = cache_read.get(&cache_key).cloned();
			drop(cache_read);
			if let Some(cached_json) = cached_json {
				let serializer = evaluator.create_serializer();
				let value = cached_json.serialize(serializer)?;
				return Ok(value);
			}

			Some(cache_key)
		};

		let mut command = Command::new("helm");
		command.arg("template");
		command.arg(&name);
		command.arg(chart_path);

		if let Some(namespace) = options.namespace.as_ref() {
			command.arg("--namespace");
			command.arg(namespace);
		}

		if options.include_crds {
			command.arg("--include-crds");
		}

		if options.no_hooks {
			command.arg("--no-hooks");
		}

		for api_version in &options.api_versions {
			command.arg("--api-versions");
			command.arg(api_version);
		}

		if options.values.is_some() {
			command.arg("--values=-");
			command.stdin(Stdio::piped());
		}

		command.stdout(Stdio::piped());
		command.stderr(Stdio::piped());

		let mut child = command
			.spawn()
			.map_err(|error| E::Error::custom(format!("failed to execute helm: {error}")))?;

		if let Some(json) = options.values.as_ref() {
			let mut stdin = child
				.stdin
				.take()
				.ok_or_else(|| E::Error::custom("failed to capture helm stdin"))?;
			serde_json::to_writer(&mut stdin, json).map_err(|error| {
				E::Error::custom(format!("failed to write values to helm stdin: {error}"))
			})?;
			stdin.flush().map_err(|error| {
				E::Error::custom(format!("failed to write values to helm stdin: {error}"))
			})?;
		}

		let (status, stdout_buffer, stderr_buffer) =
			drain_output(child).map_err(E::Error::custom)?;

		if !status.success() {
			let stderr = String::from_utf8_lossy(&stderr_buffer);
			return Err(E::Error::custom(format!("helm template failed: {stderr}")));
		}

		let yaml_content = String::from_utf8(stdout_buffer)
			.map_err(|error| E::Error::custom(format!("invalid UTF-8 in helm output: {error}")))?;

		let value = parse_helm_yaml_output(&yaml_content, options.name_format.as_deref())
			.map_err(E::Error::custom)?;

		let serializer = evaluator.create_serializer();
		let evaluator_value = value.serialize(serializer)?;

		if let Some(cache_key) = cache_key {
			let mut write = self
				.state
				.template_cache
				.write()
				.unwrap_or_else(std::sync::PoisonError::into_inner);

			write.insert(cache_key, value);
		}

		Ok(evaluator_value)
	}
}

#[derive(Debug, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct Options {
	#[serde(default)]
	api_versions: Vec<String>,
	called_from: String,
	#[serde(default)]
	no_hooks: bool,
	namespace: Option<String>,
	name_format: Option<String>,
	#[serde(default = "default_true")]
	include_crds: bool,
	values: Option<serde_json::Value>,
}

const fn default_true() -> bool {
	true
}

fn drain_output(mut child: Child) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), String> {
	let mut stdout = child
		.stdout
		.take()
		.ok_or_else(|| "failed to capture helm stdout".to_owned())?;
	let mut stderr = child
		.stderr
		.take()
		.ok_or_else(|| "failed to capture helm stderr".to_owned())?;

	let stdout_handle = thread::Builder::new()
		.name("helm-stdout".to_owned())
		.spawn(move || {
			let mut output = Vec::with_capacity(4096);
			stdout.read_to_end(&mut output).map(|_| output)
		})
		.map_err(|error| format!("failed to spawn helm stdout thread: {error}"))?;
	let stderr_handle = thread::Builder::new()
		.name("helm-stderr".to_owned())
		.spawn(move || {
			let mut output = Vec::with_capacity(4096);
			stderr.read_to_end(&mut output).map(|_| output)
		})
		.map_err(|error| format!("failed to spawn helm stderr thread: {error}"))?;

	let status = child.wait();
	let stdout = stdout_handle.join();
	let stderr = stderr_handle.join();

	let status = status.map_err(|error| format!("failed to wait for helm: {error}"))?;
	let stdout = stdout
		.map_err(|_| "failed to join helm stdout thread".to_owned())?
		.map_err(|error| format!("failed to read helm stdout: {error}"))?;
	let stderr = stderr
		.map_err(|_| "failed to join helm stderr thread".to_owned())?
		.map_err(|error| format!("failed to read helm stderr: {error}"))?;

	Ok((status, stdout, stderr))
}

fn parse_helm_yaml_output(
	yaml_content: &str,
	name_format: Option<&str>,
) -> Result<serde_json::Value, String> {
	let parse_options = serde_saphyr::Options {
		legacy_octal_numbers: true,
		budget: None,
		..Default::default()
	};
	let documents =
		serde_saphyr::from_multiple_with_options::<serde_json::Value>(yaml_content, parse_options)
			.map_err(|error| format!("failed to parse helm output: {error}"))?;

	let mut output = serde_json::Map::with_capacity(documents.len());
	let mut seen_keys = FxHashSet::with_capacity_and_hasher(documents.len(), FxBuildHasher);
	for document in documents {
		let serde_json::Value::Object(document) = document else {
			continue;
		};

		let base_key = manifest_key(&document, name_format);
		let mut final_key = base_key.clone();
		let mut counter = 2;
		while seen_keys.contains(&final_key) {
			final_key = format!("{base_key}_{counter}");
			counter += 1;
		}
		seen_keys.insert(final_key.clone());
		output.insert(final_key, serde_json::Value::Object(document));
	}

	Ok(serde_json::Value::Object(output))
}

fn manifest_key(
	document: &serde_json::Map<String, serde_json::Value>,
	name_format: Option<&str>,
) -> String {
	let use_namespace = name_format.is_some_and(|format| {
		format.contains("metadata.namespace") || format.contains(".or .metadata.namespace")
	});
	let kind = document
		.get("kind")
		.and_then(serde_json::Value::as_str)
		.map_or_else(|| "unknown".to_owned(), to_snake_case);
	let Some(metadata) = document
		.get("metadata")
		.and_then(serde_json::Value::as_object)
	else {
		return "unknown".to_owned();
	};
	let name = metadata
		.get("name")
		.and_then(serde_json::Value::as_str)
		.map_or_else(|| "unknown".to_owned(), to_snake_case);

	if use_namespace {
		let namespace = metadata
			.get("namespace")
			.and_then(serde_json::Value::as_str)
			.map_or_else(|| "cluster".to_owned(), to_snake_case);
		format!("{namespace}_{kind}_{name}")
	} else {
		format!("{kind}_{name}")
	}
}

/// Convert a string to snake_case (lowercase with underscores)
/// Matches Go Tanka's naming behavior which inserts underscores:
/// - Before uppercase letters (CamelCase -> camel_case)
/// - Between letter-digit-letter sequences (k8s -> k_8s)
///
/// Does not insert an underscore when a digit is at a word boundary (`flux2` stays `flux2`).
fn to_snake_case(s: &str) -> String {
	let mut result = String::new();
	let chars: Vec<char> = s.chars().collect();

	for (i, &ch) in chars.iter().enumerate() {
		if ch.is_uppercase() {
			// Add underscore before uppercase letters (except at start)
			if !result.is_empty() {
				result.push('_');
			}
			// to_lowercase() always returns at least one char, but use unwrap_or for safety
			result.push(ch.to_lowercase().next().unwrap_or(ch));
		} else if ch == '-' {
			// Replace hyphens with underscores
			result.push('_');
		} else if ch.is_ascii_digit() {
			// Add underscore between letter and digit ONLY if there's a letter eventually
			// after the consecutive digits. This matches Go Tanka:
			// - k8s -> k_8s (letter after digit)
			// - o11y -> o_11y (letter eventually after digits)
			// - flux2 -> flux2 (no letter after digit, at end or before hyphen)
			let prev_is_letter = i > 0 && chars[i - 1].is_ascii_alphabetic();
			if prev_is_letter {
				// Look ahead past all consecutive digits to see if there's a letter
				let has_letter_after_digits = chars[i..]
					.iter()
					.find(|character| !character.is_ascii_digit())
					.is_some_and(char::is_ascii_alphabetic);
				if has_letter_after_digits {
					result.push('_');
				}
			}
			result.push(ch);
		} else {
			result.push(ch);
		}
	}

	result
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::{Options, parse_helm_yaml_output};

	#[test]
	fn include_crds_defaults_to_true_and_accepts_false() {
		let defaults: Options = serde_json::from_value(json!({
			"calledFrom": "/tmp/main.jsonnet"
		}))
		.unwrap();
		assert!(defaults.include_crds);

		let disabled: Options = serde_json::from_value(json!({
			"calledFrom": "/tmp/main.jsonnet",
			"includeCrds": false
		}))
		.unwrap();
		assert!(!disabled.include_crds);
	}

	#[test]
	fn suffixes_duplicate_keys_across_documents() {
		let output = parse_helm_yaml_output(
			r"
kind: ConfigMap
metadata:
  name: settings
---
kind: ConfigMap
metadata:
  name: settings
---
kind: ConfigMap
metadata:
  name: settings
",
			None,
		)
		.unwrap();
		let keys: Vec<_> = output
			.as_object()
			.unwrap()
			.keys()
			.map(String::as_str)
			.collect();
		assert_eq!(
			keys,
			[
				"config_map_settings",
				"config_map_settings_2",
				"config_map_settings_3"
			]
		);
	}

	#[test]
	fn duplicate_suffixes_do_not_overwrite_existing_keys() {
		let output = parse_helm_yaml_output(
			r"
kind: ConfigMap
metadata:
  name: settings
---
kind: ConfigMap
metadata:
  name: settings_2
---
kind: ConfigMap
metadata:
  name: settings
",
			None,
		)
		.unwrap();
		let keys: Vec<_> = output
			.as_object()
			.unwrap()
			.keys()
			.map(String::as_str)
			.collect();
		assert_eq!(
			keys,
			[
				"config_map_settings",
				"config_map_settings_2",
				"config_map_settings_3"
			]
		);
	}

	#[test]
	fn namespace_aware_keys_default_to_cluster() {
		let output = parse_helm_yaml_output(
			r"
kind: Service
metadata:
  name: api
  namespace: production
---
kind: Service
metadata:
  name: api
",
			Some("{{ .metadata.namespace }}"),
		)
		.unwrap();
		let keys: Vec<_> = output
			.as_object()
			.unwrap()
			.keys()
			.map(String::as_str)
			.collect();
		assert_eq!(keys, ["production_service_api", "cluster_service_api"]);
	}
}
