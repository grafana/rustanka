use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tabwriter::TabWriter;

use crate::spec::Environment;

/// List environments in the given path
pub fn list_envs(path: Option<String>, json: bool) -> Result<()> {
	let search_path = path
		.map(PathBuf::from)
		.unwrap_or_else(|| std::env::current_dir().unwrap());
	let mut envs = find_environments(&search_path, &search_path)?;

	if json {
		// Normalize: convert null resourceDefaults and expectVersions to empty objects
		for env in &mut envs {
			env.spec
				.resource_defaults
				.get_or_insert_with(|| serde_json::json!({}));
			env.spec
				.expect_versions
				.get_or_insert_with(|| serde_json::json!({}));
		}
		println!("{}", serde_json::to_string(&envs)?);
	} else {
		print_table(&envs, &search_path)?;
	}

	Ok(())
}

fn print_table(envs: &[Environment], search_path: &Path) -> Result<()> {
	let mut tw = TabWriter::new(std::io::stdout()).padding(4);
	writeln!(tw, "NAME\tNAMESPACE\tSERVER")?;

	if envs.is_empty() {
		writeln!(tw, "No environments found in {}", search_path.display())?;
	} else {
		for env in envs {
			writeln!(
				tw,
				"{}\t{}\t{}",
				env.metadata.name.as_deref().unwrap_or("unnamed"),
				&env.spec.namespace,
				env.spec.api_server.as_deref().unwrap_or("-")
			)?;
		}
	}
	tw.flush()?;
	Ok(())
}

/// Find all environments recursively
fn find_environments(root: &Path, original_path: &Path) -> Result<Vec<Environment>> {
	let main_files = find_main_jsonnet_files(root)?;
	let mut environments = Vec::new();

	for main_file in main_files {
		let dir = main_file.parent().context("No parent directory")?;
		let spec_file = dir.join("spec.json");

		if spec_file.exists() {
			// Static environment
			if let Ok(mut env) = load_static_env(dir) {
				set_env_metadata(&mut env, dir, original_path)?;
				environments.push(env);
			}
		} else {
			// Inline environment
			match load_inline_envs(dir) {
				Ok(mut envs) => {
					for env in &mut envs {
						// Inline envs may already have full paths from Jsonnet
						// Only update name if it doesn't start with "environments/"
						let should_update_name = if let Some(name) = &env.metadata.name {
							!name.starts_with("environments/")
						} else {
							true
						};

						if should_update_name {
							set_env_metadata(env, dir, original_path)?;
						} else {
							// Still set namespace even if name is preserved
							set_env_namespace(env, dir)?;
						}

						// Set exportJsonnetImplementation for inline environments
						env.spec.export_jsonnet_implementation =
							Some("binary:/usr/local/bin/jrsonnet".to_string());
					}
					environments.extend(envs);
				}
				Err(_) => continue,
			}
		}
	}

	Ok(environments)
}

/// Set environment metadata (name and namespace)
fn set_env_metadata(env: &mut Environment, dir: &Path, _original_path: &Path) -> Result<()> {
	let dir_str = dir.to_string_lossy();

	// Extract path starting from "environments/"
	let env_path = dir_str
		.find("environments/")
		.map(|pos| &dir_str[pos..])
		.or_else(|| dir_str.strip_prefix("ksonnet/"))
		.unwrap_or(&dir_str);

	env.metadata.name = Some(env_path.to_string());
	env.metadata.namespace = Some(format!("{}/main.jsonnet", env_path));

	Ok(())
}

/// Set only the namespace without changing the name
fn set_env_namespace(env: &mut Environment, dir: &Path) -> Result<()> {
	let dir_str = dir.to_string_lossy();

	// Extract path starting from "environments/"
	let env_path = dir_str
		.find("environments/")
		.map(|pos| &dir_str[pos..])
		.or_else(|| dir_str.strip_prefix("ksonnet/"))
		.unwrap_or(&dir_str);

	env.metadata.namespace = Some(format!("{}/main.jsonnet", env_path));

	Ok(())
}

/// Recursively find all main.jsonnet files
fn find_main_jsonnet_files(dir: &Path) -> Result<Vec<PathBuf>> {
	let mut results = Vec::new();
	find_main_jsonnet_impl(dir, &mut results)?;
	Ok(results)
}

fn find_main_jsonnet_impl(dir: &Path, results: &mut Vec<PathBuf>) -> Result<()> {
	if !dir.is_dir() {
		return Ok(());
	}

	let main_file = dir.join("main.jsonnet");
	if main_file.exists() {
		results.push(main_file);
		return Ok(()); // Don't recurse into subdirectories
	}

	for entry in fs::read_dir(dir)? {
		let path = entry?.path();
		if path.is_dir() {
			find_main_jsonnet_impl(&path, results)?;
		}
	}

	Ok(())
}

/// Load inline environments by evaluating Jsonnet
fn load_inline_envs(dir: &Path) -> Result<Vec<Environment>> {
	use jrsonnet_evaluator::{manifest::JsonFormat, State};

	let main_path = dir.join("main.jsonnet");
	if !main_path.exists() {
		return Ok(Vec::new());
	}

	let mut import_paths = vec![dir.to_path_buf()];

	// Add lib and vendor directories if they exist
	if let Some(root) = find_project_root(dir) {
		for subdir in &["lib", "vendor"] {
			let path = root.join(subdir);
			if path.is_dir() {
				import_paths.push(path);
			}
		}
	}

	let import_resolver = jrsonnet_evaluator::FileImportResolver::new(import_paths);
	let mut builder = State::builder();
	builder.import_resolver(import_resolver);

	use jrsonnet_evaluator::trace::PathResolver;
	let ctx_init = jrsonnet_stdlib::ContextInitializer::new(PathResolver::new_cwd_fallback());
	builder.context_initializer(ctx_init);

	let state = builder.build();

	// Evaluate with noDataEnv wrapper to strip out .data field
	let eval_script = format!(
		r#"
local noDataEnv(object) =
  std.prune(
    if std.isObject(object)
    then
      if std.objectHas(object, 'apiVersion') && std.objectHas(object, 'kind')
      then
        if object.kind == 'Environment'
        then object {{ data+:: {{}} }}
        else {{}}
      else
        std.mapWithKey(function(key, obj) noDataEnv(obj), object)
    else if std.isArray(object)
    then
      std.map(function(obj) noDataEnv(obj), object)
    else {{}}
  );

local main = (import '{}');
noDataEnv(main)
"#,
		main_path.file_name().unwrap().to_string_lossy()
	);

	let result = state
		.evaluate_snippet("<metadata-eval>", &eval_script)
		.map_err(|e| {
			anyhow::anyhow!(
				"Failed to evaluate Jsonnet at {}: {}",
				main_path.display(),
				e
			)
		})?;

	let json_str = result
		.manifest(JsonFormat::cli(2))
		.map_err(|e| anyhow::anyhow!("Failed to manifest Jsonnet: {}", e))?;

	let json_value: serde_json::Value =
		serde_json::from_str(&json_str).context("Failed to parse manifested JSON")?;

	extract_environments(&json_value)
}

/// Extract Environment objects from Jsonnet output
fn extract_environments(value: &serde_json::Value) -> Result<Vec<Environment>> {
	let mut environments = Vec::new();

	match value {
		serde_json::Value::Object(obj) => {
			// Check if this is a single Environment object
			if obj.contains_key("apiVersion") && obj.contains_key("kind") {
				if let Some("Environment") = obj.get("kind").and_then(|v| v.as_str()) {
					if let Ok(env) = serde_json::from_value::<Environment>(value.clone()) {
						environments.push(env);
						return Ok(environments);
					}
				}
			}

			// Otherwise, recursively extract from each field
			for (key, val) in obj {
				let mut extracted = extract_environments(val)?;
				for env in &mut extracted {
					if env.metadata.name.is_none() {
						env.metadata.name = Some(key.clone());
					}
				}
				environments.extend(extracted);
			}
		}
		serde_json::Value::Array(arr) => {
			for val in arr {
				environments.extend(extract_environments(val)?);
			}
		}
		_ => {}
	}

	Ok(environments)
}

/// Find the project root by looking for jsonnetfile.json or tkrc.yaml
fn find_project_root(start_path: &Path) -> Option<PathBuf> {
	let mut current = start_path;
	loop {
		if current.join("jsonnetfile.json").exists() || current.join("tkrc.yaml").exists() {
			return Some(current.to_path_buf());
		}
		current = current.parent()?;
	}
}

/// Load a static environment from spec.json
fn load_static_env(path: &Path) -> Result<Environment> {
	let spec_path = path.join("spec.json");
	let content = fs::read_to_string(&spec_path)
		.with_context(|| format!("Failed to read {}", spec_path.display()))?;
	serde_json::from_str(&content)
		.with_context(|| format!("Failed to parse {}", spec_path.display()))
}
