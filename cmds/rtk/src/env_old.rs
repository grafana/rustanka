use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tabwriter::TabWriter;

use crate::spec::Environment;

/// List environments in the given path
pub fn list_envs(path: Option<String>, json: bool) -> Result<()> {
    let search_path = if let Some(p) = path {
        PathBuf::from(p)
    } else {
        std::env::current_dir()?
    };

    // Find all environments recursively
    // For compatibility with tk, we need to strip "ksonnet/" from paths
    let envs = find_environments_recursive(&search_path, &search_path)?;

    if json {
        // Normalize environments for JSON output (convert null to {} for certain fields)
        let mut normalized_envs = envs;
        for env in &mut normalized_envs {
            // Convert null resourceDefaults and expectVersions to empty objects
            if env.spec.resource_defaults.is_none() {
                env.spec.resource_defaults = Some(serde_json::json!({}));
            }
            if env.spec.expect_versions.is_none() {
                env.spec.expect_versions = Some(serde_json::json!({}));
            }
        }

        // Output as JSON array
        let json_output = serde_json::to_string(&normalized_envs)?;
        println!("{}", json_output);
    } else {
        if envs.is_empty() {
            println!("NAME\tNAMESPACE\tSERVER");
            println!("No environments found in {}", search_path.display());
        } else {
            // Use tabwriter for automatic column alignment
            let mut tw = TabWriter::new(std::io::stdout())
                .padding(4); // 4 spaces between columns (same as Go's tabwriter)

            writeln!(tw, "NAME\tNAMESPACE\tSERVER")?;
            for env in envs {
                let name = env.metadata.name.unwrap_or_else(|| "unnamed".to_string());
                let namespace = env.spec.namespace;
                let server = env.spec.api_server.unwrap_or_else(|| "-".to_string());
                writeln!(tw, "{}\t{}\t{}", name, namespace, server)?;
            }
            tw.flush()?;
        }
    }

    Ok(())
}

/// Find all environments recursively by looking for main.jsonnet files
fn find_environments_recursive(root: &Path, original_search_path: &Path) -> Result<Vec<Environment>> {
    let mut environments = Vec::new();

    // Step 1: Find all main.jsonnet files recursively
    let main_jsonnet_files = find_main_jsonnet_files(root)?;

    // Step 2: For each main.jsonnet, determine if it's static or inline
    for main_file in main_jsonnet_files {
        let dir = main_file.parent().context("No parent directory")?;
        let spec_file = dir.join("spec.json");

        if spec_file.exists() {
            // Static environment - load from spec.json (returns 1 env)
            if let Ok(mut env) = load_env(dir) {
                // For static environments, TK uses the directory structure for the name
                // not the metadata.name from spec.json
                set_static_env_name(&mut env, dir)?;
                // Set metadata.namespace to path to main.jsonnet
                set_metadata_namespace(&mut env, dir, original_search_path)?;
                environments.push(env);
            }
        } else {
            // Inline environment - evaluate Jsonnet (can return multiple envs)
            match load_inline_envs(dir) {
                Ok(mut envs) => {
                    // Update metadata.name for inline environments too
                    for env in &mut envs {
                        update_env_name(env, dir, original_search_path)?;
                    }
                    environments.append(&mut envs);
                }
                Err(e) => {
                    // Silently skip environments that fail to load
                    eprintln!("Warning: failed to load inline environment at {}: {}", dir.display(), e);
                }
            }
        }
    }

    Ok(environments)
}

/// Set static environment name based on directory structure
/// TK uses the directory path, not the metadata.name from spec.json
fn set_static_env_name(env: &mut Environment, dir: &Path) -> Result<()> {
    let dir_str = dir.to_string_lossy();

    // Find "environments/" in the path and use everything from there
    let env_name = if let Some(pos) = dir_str.find("environments/") {
        &dir_str[pos..]
    } else if dir_str.contains("ksonnet/") {
        // Fallback: strip ksonnet/ prefix
        dir_str.strip_prefix("ksonnet/").unwrap_or(&dir_str)
    } else {
        &dir_str
    };

    env.metadata.name = Some(env_name.to_string());
    Ok(())
}

/// Set metadata.namespace to the path to main.jsonnet
fn set_metadata_namespace(env: &mut Environment, dir: &Path, _original_search_path: &Path) -> Result<()> {
    // Get the full path to the environment directory relative to ksonnet/
    let dir_str = dir.to_string_lossy();

    // Strip everything before "environments/" and use that as the base
    let namespace_path = if let Some(pos) = dir_str.find("environments/") {
        &dir_str[pos..]
    } else if dir_str.contains("ksonnet/") {
        // Fallback: strip ksonnet/ prefix
        dir_str.strip_prefix("ksonnet/").unwrap_or(&dir_str)
    } else {
        &dir_str
    };

    // metadata.namespace should be: full_path_to_env_dir/main.jsonnet
    env.metadata.namespace = Some(format!("{}/main.jsonnet", namespace_path));
    Ok(())
}

/// Update environment name to match tk's convention
/// tk strips "ksonnet/" and uses "environments/" prefix
fn update_env_name(env: &mut Environment, dir: &Path, original_search_path: &Path) -> Result<()> {
    if let Some(current_name) = &env.metadata.name {
        // If the name already starts with "environments/", don't modify it
        // (inline environments from Jsonnet may already have full paths)
        if current_name.starts_with("environments/") {
            return Ok(());
        }

        // Get the relative path from original search path
        if let Ok(rel_from_search) = dir.strip_prefix(original_search_path) {
            // Build the path: search_path + "/" + env_name
            let search_path_str = original_search_path.to_string_lossy();

            // Strip "ksonnet/" prefix if present and replace with "environments/"
            let normalized_path = if search_path_str.starts_with("ksonnet/") {
                search_path_str.strip_prefix("ksonnet/").unwrap_or(&search_path_str)
            } else if search_path_str.starts_with("ksonnet") && search_path_str.len() == 7 {
                "environments"
            } else {
                &search_path_str
            };

            // Remove trailing slash if present
            let normalized_path = normalized_path.trim_end_matches('/');

            // If rel_from_search is empty (we're in the search directory itself)
            // For static environments: normalized_path already contains the full path
            // For inline environments: we need to append current_name
            let full_name = if rel_from_search.as_os_str().is_empty() {
                // Check if this is a static environment by seeing if the dir name matches current_name
                let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if dir_name == current_name {
                    // Static environment: directory name is the same as environment name
                    normalized_path.to_string()
                } else {
                    // Inline environment: append the environment name
                    format!("{}/{}", normalized_path, current_name)
                }
            } else {
                // Get parent of the environment directory
                if let Some(parent) = rel_from_search.parent() {
                    if parent.as_os_str().is_empty() {
                        format!("{}/{}", normalized_path, current_name)
                    } else {
                        let parent_str = parent.to_string_lossy();
                        // Ensure we don't create double slashes
                        let parent_clean = parent_str.trim_start_matches('/');
                        format!("{}/{}/{}", normalized_path, parent_clean, current_name)
                    }
                } else {
                    format!("{}/{}", normalized_path, current_name)
                }
            };

            env.metadata.name = Some(full_name);
        }
    }
    Ok(())
}

/// Recursively find all main.jsonnet files
fn find_main_jsonnet_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut results = Vec::new();
    find_main_jsonnet_files_impl(dir, &mut results)?;
    Ok(results)
}

fn find_main_jsonnet_files_impl(dir: &Path, results: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }

    // Check if this directory contains main.jsonnet
    let main_file = dir.join("main.jsonnet");
    if main_file.exists() {
        results.push(main_file);
        // Don't recurse into subdirectories if we found a main.jsonnet
        return Ok(());
    }

    // Otherwise, recurse into subdirectories
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            find_main_jsonnet_files_impl(&path, results)?;
        }
    }

    Ok(())
}

/// Load inline environments by evaluating Jsonnet
fn load_inline_envs(dir: &Path) -> Result<Vec<Environment>> {
    use jrsonnet_evaluator::State;
    use jrsonnet_evaluator::manifest::JsonFormat;

    let main_path = dir.join("main.jsonnet");
    if !main_path.exists() {
        return Ok(Vec::new());
    }

    // Create jrsonnet state
    let mut builder = State::builder();

    // Set up import resolver with library paths
    let mut import_paths = Vec::new();
    import_paths.push(dir.to_path_buf());

    // Find project root and add lib/vendor directories
    if let Some(root) = find_project_root(dir) {
        let lib_dir = root.join("lib");
        if lib_dir.exists() && lib_dir.is_dir() {
            import_paths.push(lib_dir);
        }

        let vendor_dir = root.join("vendor");
        if vendor_dir.exists() && vendor_dir.is_dir() {
            import_paths.push(vendor_dir);
        }
    }

    let import_resolver = jrsonnet_evaluator::FileImportResolver::new(import_paths);
    builder.import_resolver(import_resolver);

    // Add standard library
    use jrsonnet_evaluator::trace::PathResolver;
    let ctx_init = jrsonnet_stdlib::ContextInitializer::new(PathResolver::new_cwd_fallback());
    builder.context_initializer(ctx_init);

    let state = builder.build();

    // Create eval script that extracts only Environment metadata (without .data)
    // This is crucial to avoid evaluating all Kubernetes manifests
    let eval_script = format!(r#"
local noDataEnv(object) =
  std.prune(
    if std.isObject(object)
    then
      if std.objectHas(object, 'apiVersion')
         && std.objectHas(object, 'kind')
      then
        if object.kind == 'Environment'
        then object {{ data+:: {{}} }}
        else {{}}
      else
        std.mapWithKey(
          function(key, obj)
            noDataEnv(obj),
          object
        )
    else if std.isArray(object)
    then
      std.map(
        function(obj)
          noDataEnv(obj),
        object
      )
    else {{}}
  );

local main = (import '{}');
noDataEnv(main)
"#, main_path.file_name().unwrap().to_string_lossy());

    // Evaluate the script
    let result = state.evaluate_snippet("<metadata-eval>", &eval_script)
        .map_err(|e| anyhow::anyhow!("Failed to evaluate Jsonnet at {}: {}", main_path.display(), e))?;

    // Manifest to JSON
    let manifest_format = JsonFormat::cli(2);
    let json_str = result.manifest(&manifest_format)
        .map_err(|e| anyhow::anyhow!("Failed to manifest Jsonnet: {}", e))?;


    // Parse JSON
    let json_value: serde_json::Value = serde_json::from_str(&json_str)
        .context("Failed to parse manifested JSON")?;

    // Extract all Environment objects
    extract_inline_environments(&json_value)
}

/// Extract Environment objects from Jsonnet output
fn extract_inline_environments(value: &serde_json::Value) -> Result<Vec<Environment>> {
    let mut environments = Vec::new();

    match value {
        serde_json::Value::Object(obj) => {
            // Check if this is a single Environment object
            if obj.contains_key("apiVersion") && obj.contains_key("kind") {
                if let Some(kind) = obj.get("kind").and_then(|v| v.as_str()) {
                    if kind == "Environment" {
                        if let Ok(env) = serde_json::from_value::<Environment>(value.clone()) {
                            environments.push(env);
                            return Ok(environments);
                        }
                    }
                }
            }

            // Otherwise, it's an object where each value might be an Environment
            for (key, val) in obj {
                let mut extracted = extract_inline_environments(val)?;

                // If the environment doesn't have a name, use the key
                for env in &mut extracted {
                    if env.metadata.name.is_none() {
                        env.metadata.name = Some(key.clone());
                    }
                }

                environments.append(&mut extracted);
            }
        }
        serde_json::Value::Array(arr) => {
            // Array of environments
            for val in arr {
                environments.append(&mut extract_inline_environments(val)?);
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

/// Load a static environment from a directory (reads spec.json)
fn load_env(path: &Path) -> Result<Environment> {
    let spec_path = path.join("spec.json");
    let content = fs::read_to_string(&spec_path)
        .with_context(|| format!("Failed to read {}", spec_path.display()))?;
    let env: Environment = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", spec_path.display()))?;
    Ok(env)
}
