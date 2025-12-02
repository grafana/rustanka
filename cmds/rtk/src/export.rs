//! export - Export Tanka environments to files
//!
//! This module handles exporting multiple Tanka environments to files in parallel.
//! It evaluates environments and writes the resulting Kubernetes manifests to disk.

use anyhow::{Context, Result};
use handlebars::Handlebars;
use rayon::prelude::*;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::discover::{find_environments, DiscoveredEnv};
use crate::eval::{eval, EvalOpts};

/// Options for the export command
#[derive(Debug, Clone)]
pub struct ExportOpts {
	/// Output directory
	pub output_dir: PathBuf,
	/// File extension (yaml or json)
	pub extension: String,
	/// Filename format template
	pub format: String,
	/// Number of parallel workers
	pub parallelism: usize,
	/// Eval options to pass through
	pub eval_opts: EvalOpts,
}

impl Default for ExportOpts {
	fn default() -> Self {
		Self {
			output_dir: PathBuf::from("."),
			extension: "yaml".to_string(),
			format: "{{apiVersion}}.{{kind}}-{{name}}".to_string(),
			parallelism: 8,
			eval_opts: EvalOpts::default(),
		}
	}
}

/// Result of exporting a single environment
#[derive(Debug)]
pub struct ExportEnvResult {
	/// Path to the environment
	pub env_path: PathBuf,
	/// Files that were written
	pub files_written: Vec<PathBuf>,
	/// Any error that occurred
	pub error: Option<String>,
}

/// Result of the export operation
#[derive(Debug)]
pub struct ExportResult {
	/// Total environments processed
	pub total_envs: usize,
	/// Successfully exported environments
	pub successful: usize,
	/// Failed environments
	pub failed: usize,
	/// Results for each environment
	pub results: Vec<ExportEnvResult>,
}

/// Export environments from given paths to the output directory
pub fn export(paths: &[String], opts: ExportOpts) -> Result<ExportResult> {
	// Discover environments
	let envs = find_environments(paths)?;

	if envs.is_empty() {
		return Ok(ExportResult {
			total_envs: 0,
			successful: 0,
			failed: 0,
			results: vec![],
		});
	}

	// Create output directory
	fs::create_dir_all(&opts.output_dir)
		.context(format!("creating output directory {:?}", opts.output_dir))?;

	// Set up rayon thread pool
	let pool = rayon::ThreadPoolBuilder::new()
		.num_threads(opts.parallelism)
		.build()
		.context("building thread pool")?;

	// Process environments in parallel
	let results: Vec<ExportEnvResult> = pool.install(|| {
		envs.par_iter()
			.map(|env| export_single_env(env, &opts))
			.collect()
	});

	// Summarize results
	let successful = results.iter().filter(|r| r.error.is_none()).count();
	let failed = results.iter().filter(|r| r.error.is_some()).count();

	Ok(ExportResult {
		total_envs: envs.len(),
		successful,
		failed,
		results,
	})
}

/// Export a single environment
fn export_single_env(env: &DiscoveredEnv, opts: &ExportOpts) -> ExportEnvResult {
	let env_path = env.path.clone();

	match export_single_env_inner(env, opts) {
		Ok(files) => ExportEnvResult {
			env_path,
			files_written: files,
			error: None,
		},
		Err(e) => ExportEnvResult {
			env_path,
			files_written: vec![],
			error: Some(e.to_string()),
		},
	}
}

fn export_single_env_inner(env: &DiscoveredEnv, opts: &ExportOpts) -> Result<Vec<PathBuf>> {
	// Evaluate the environment
	let result = eval(env.path.to_string_lossy().as_ref(), opts.eval_opts.clone())?;

	// Extract Kubernetes manifests from the result
	let manifests = extract_manifests(&result.value)?;

	if manifests.is_empty() {
		return Ok(vec![]);
	}

	// Calculate output subdirectory based on environment path
	let env_name = get_env_name(&env.path, &result.spec);
	let output_subdir = opts.output_dir.join(&env_name);
	fs::create_dir_all(&output_subdir)?;

	// Set up handlebars for filename templating
	let mut hb = Handlebars::new();
	hb.set_strict_mode(false);
	hb.register_template_string("filename", &opts.format)
		.context("parsing filename format template")?;

	let mut files_written = Vec::new();

	// Write each manifest to a file
	for manifest in manifests {
		let filename = format_filename(&hb, &manifest, &opts.extension)?;
		let filepath = output_subdir.join(&filename);

		// Create parent directories if needed
		if let Some(parent) = filepath.parent() {
			fs::create_dir_all(parent)?;
		}

		// Serialize manifest
		let content = if opts.extension == "json" {
			serde_json::to_string_pretty(&manifest)?
		} else {
			serde_yaml::to_string(&manifest)?
		};

		fs::write(&filepath, content)?;
		files_written.push(filepath);
	}

	Ok(files_written)
}

/// Extract Kubernetes manifests from evaluation result
fn extract_manifests(value: &Value) -> Result<Vec<Value>> {
	let mut manifests = Vec::new();

	// Check if this is a Tanka environment object with a `data` field
	if let Value::Object(obj) = value {
		if obj.contains_key("apiVersion") && obj.contains_key("kind") {
			if let Some(Value::String(kind)) = obj.get("kind") {
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
fn collect_manifests(value: &Value, manifests: &mut Vec<Value>) {
	match value {
		Value::Object(obj) => {
			// Check if this looks like a Kubernetes manifest (has apiVersion and kind)
			if obj.contains_key("apiVersion") && obj.contains_key("kind") {
				// Skip Tanka Environment objects
				if let Some(Value::String(kind)) = obj.get("kind") {
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
		Value::Array(arr) => {
			// Recurse into array elements
			for v in arr {
				collect_manifests(v, manifests);
			}
		}
		_ => {}
	}
}

/// Get environment name for output directory
fn get_env_name(path: &Path, spec: &Option<crate::spec::Environment>) -> String {
	// Try to get name from spec
	if let Some(env) = spec {
		if let Some(ref name) = env.metadata.name {
			return sanitize_path_component(name);
		}
		if let Some(ref ns) = env.metadata.namespace {
			return sanitize_path_component(ns);
		}
	}

	// Fall back to directory name
	path.file_name()
		.and_then(|n| n.to_str())
		.map(sanitize_path_component)
		.unwrap_or_else(|| "unknown".to_string())
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

/// Format filename using handlebars template
fn format_filename(hb: &Handlebars, manifest: &Value, extension: &str) -> Result<String> {
	let obj = manifest.as_object().context("manifest is not an object")?;

	// Build template data
	let mut data: HashMap<String, String> = HashMap::new();

	// Add apiVersion and kind
	if let Some(Value::String(api_version)) = obj.get("apiVersion") {
		// Replace / with _ for filesystem safety
		data.insert("apiVersion".to_string(), api_version.replace('/', "_"));
	}
	if let Some(Value::String(kind)) = obj.get("kind") {
		data.insert("kind".to_string(), kind.clone());
	}

	// Add metadata fields
	if let Some(Value::Object(metadata)) = obj.get("metadata") {
		if let Some(Value::String(name)) = metadata.get("name") {
			data.insert("name".to_string(), name.clone());
		}
		if let Some(Value::String(generate_name)) = metadata.get("generateName") {
			data.insert("generateName".to_string(), generate_name.clone());
		}
		if let Some(Value::String(namespace)) = metadata.get("namespace") {
			data.insert("namespace".to_string(), namespace.clone());
		}
	}

	// Provide fallback for name
	if !data.contains_key("name") {
		if let Some(gn) = data.get("generateName") {
			data.insert("name".to_string(), gn.clone());
		} else {
			data.insert("name".to_string(), "unnamed".to_string());
		}
	}

	// Render template
	let filename = hb
		.render("filename", &data)
		.context("rendering filename template")?;

	// Sanitize and add extension
	let sanitized = sanitize_path_component(&filename);
	Ok(format!("{}.{}", sanitized, extension))
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
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
	fn test_format_filename() {
		let mut hb = Handlebars::new();
		hb.register_template_string("filename", "{{apiVersion}}.{{kind}}-{{name}}")
			.unwrap();

		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "my-config" }
		});

		let filename = format_filename(&hb, &manifest, "yaml").unwrap();
		assert_eq!(filename, "v1.ConfigMap-my-config.yaml");
	}

	#[test]
	fn test_format_filename_with_namespace_in_apiversion() {
		let mut hb = Handlebars::new();
		hb.register_template_string("filename", "{{apiVersion}}.{{kind}}-{{name}}")
			.unwrap();

		let manifest = serde_json::json!({
			"apiVersion": "apps/v1",
			"kind": "Deployment",
			"metadata": { "name": "my-app" }
		});

		let filename = format_filename(&hb, &manifest, "yaml").unwrap();
		assert_eq!(filename, "apps_v1.Deployment-my-app.yaml");
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
			format: "{{kind}}-{{name}}".to_string(),
			parallelism: 1,
			eval_opts: EvalOpts::default(),
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
			format: "{{kind}}-{{name}}".to_string(),
			parallelism: 1,
			eval_opts: EvalOpts::default(),
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
			format: "{{kind}}-{{name}}".to_string(),
			parallelism: 1,
			eval_opts: EvalOpts::default(),
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
	fn test_format_filename_missing_metadata() {
		let mut hb = Handlebars::new();
		hb.register_template_string("filename", "{{kind}}-{{name}}")
			.unwrap();

		// Manifest without metadata.name
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap"
		});

		let filename = format_filename(&hb, &manifest, "yaml").unwrap();
		assert_eq!(filename, "ConfigMap-unnamed.yaml");
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
		assert_eq!(opts.format, "{{apiVersion}}.{{kind}}-{{name}}");
	}
}
