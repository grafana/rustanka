use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
	pub tk_exec_1: String,
	#[serde(default = "default_exec1_name")]
	pub tk_exec_1_name: String,
	pub tk_exec_2: String,
	#[serde(default = "default_exec2_name")]
	pub tk_exec_2_name: String,
	#[serde(default)]
	pub working_dir: Option<String>,
	pub commands: Vec<Command>,
}

fn default_exec1_name() -> String {
	"exec1".to_string()
}

fn default_exec2_name() -> String {
	"exec2".to_string()
}

#[derive(Debug, Deserialize)]
pub struct Command {
	pub args: Vec<String>,
	#[serde(default)]
	pub name: Option<String>,
	#[serde(default = "default_runs")]
	pub runs: usize,
	#[serde(default)]
	pub json_compare: bool,
	/// Compare output directories instead of stdout (for export commands)
	#[serde(default)]
	pub dir_compare: bool,
	/// Expect both commands to fail - if false (default) and both commands fail, it's a test failure
	#[serde(default)]
	pub expect_error: bool,
}

fn default_runs() -> usize {
	1
}

impl Config {
	pub fn from_file(path: &str) -> Result<Self> {
		let contents = std::fs::read_to_string(path)
			.with_context(|| format!("Failed to read config file: {}", path))?;
		let mut config: Config = toml::from_str(&contents)
			.with_context(|| format!("Failed to parse config file: {}", path))?;

		// Expand environment variables in string fields
		config.tk_exec_1 = expand_env_vars(&config.tk_exec_1);
		config.tk_exec_2 = expand_env_vars(&config.tk_exec_2);
		if let Some(ref wd) = config.working_dir {
			config.working_dir = Some(expand_env_vars(wd));
		}

		Ok(config)
	}
}

/// Recursively collect all main.jsonnet files in a directory
/// Returns paths relative to the working directory (i.e., environments/prod/main.jsonnet)
fn collect_main_jsonnet_files(
	dir: &std::path::Path,
	working_dir: &std::path::Path,
) -> std::io::Result<Vec<String>> {
	let mut files = Vec::new();

	if dir.is_dir() {
		let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();

		// Sort entries for consistent ordering
		entries.sort_by_key(|e| e.path());

		for entry in entries {
			let path = entry.path();
			if path.is_dir() {
				// Recursively collect from subdirectories
				files.extend(collect_main_jsonnet_files(&path, working_dir)?);
			} else if let Some(name) = path.file_name() {
				if name == "main.jsonnet" {
					// Convert to path relative to working directory
					if let Ok(rel_path) = path.strip_prefix(working_dir) {
						if let Some(path_str) = rel_path.to_str() {
							files.push(path_str.to_string());
						}
					}
				}
			}
		}
	}

	Ok(files)
}

/// Expand environment variables in a string
/// Supports ${VAR} and $VAR syntax
fn expand_env_vars(s: &str) -> String {
	let mut result = s.to_string();

	// Handle ${VAR} syntax
	while let Some(start) = result.find("${") {
		if let Some(end) = result[start..].find('}') {
			let var_name = &result[start + 2..start + end];
			let value = std::env::var(var_name).unwrap_or_default();
			result.replace_range(start..start + end + 1, &value);
		} else {
			break;
		}
	}

	// Handle $VAR syntax (word boundary terminated)
	let chars = result.chars().collect::<Vec<_>>();
	let mut i = 0;
	let mut new_result = String::new();

	while i < chars.len() {
		if chars[i] == '$'
			&& i + 1 < chars.len()
			&& (chars[i + 1].is_alphabetic() || chars[i + 1] == '_')
		{
			let var_start = i + 1;
			let mut var_end = var_start;
			while var_end < chars.len()
				&& (chars[var_end].is_alphanumeric() || chars[var_end] == '_')
			{
				var_end += 1;
			}
			let var_name: String = chars[var_start..var_end].iter().collect();
			let value = std::env::var(&var_name).unwrap_or_default();
			new_result.push_str(&value);
			i = var_end;
		} else {
			new_result.push(chars[i]);
			i += 1;
		}
	}

	new_result
}

impl Command {
	pub fn as_string(&self) -> String {
		self.args.join(" ")
	}

	pub fn display_name(&self) -> String {
		self.name.clone().unwrap_or_else(|| self.as_string())
	}

	/// Get args with placeholders substituted for a specific executable
	/// Supports {{EXEC_NAME}} placeholder which gets replaced with the executable name
	/// Supports {{EXPORT_FORMAT}} which expands to two args: --format and the template value
	/// Supports {{LIST_MAIN_FILES}} which expands to all main.jsonnet files in <working_dir>
	pub fn args_for_exec(&self, exec_name: &str, working_dir: Option<&str>) -> Vec<String> {
		let export_format_template = "{{ if not env.metadata.labels.fluxExport }}flux{{ else if eq env.metadata.labels.fluxExport \"true\" }}flux{{ else }}flux-disabled{{ end }}/{{ env.metadata.labels.cluster_name }}/{{ if .metadata.labels.fluxExportDir }}{{ .metadata.labels.fluxExportDir }}{{ else if env.metadata.labels.fluxExportDir }}{{ env.metadata.labels.fluxExportDir }}{{ else if .metadata.namespace }}{{.metadata.namespace}}{{ else }}_cluster{{ end }}/{{.kind}}-{{.metadata.name}}";

		let mut result = Vec::new();
		for arg in &self.args {
			let arg = arg.replace("{{EXEC_NAME}}", exec_name);
			if arg == "{{EXPORT_FORMAT}}" {
				// Expand to two separate arguments
				result.push("--format".to_string());
				result.push(export_format_template.to_string());
			} else if arg == "{{LIST_MAIN_FILES}}" {
				// Expand to all main.jsonnet files in <working_dir>
				if let Some(wd) = working_dir {
					let working_path = std::path::Path::new(wd);
					if working_path.exists() && working_path.is_dir() {
						if let Ok(entries) = collect_main_jsonnet_files(working_path, working_path)
						{
							for file in entries {
								result.push(file);
							}
						}
					}
				}
			} else {
				result.push(arg);
			}
		}
		result
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_args_for_exec_basic() {
		let cmd = Command {
			args: vec![
				"export".to_string(),
				"/tmp/{{EXEC_NAME}}/out".to_string(),
				"path".to_string(),
			],
			name: None,
			runs: 1,
			json_compare: false,
			dir_compare: true,
			expect_error: false,
		};

		let args = cmd.args_for_exec("rtk", None);
		assert_eq!(args, vec!["export", "/tmp/rtk/out", "path"]);
	}

	#[test]
	fn test_args_for_exec_multiple_placeholders() {
		let cmd = Command {
			args: vec!["/{{EXEC_NAME}}/{{EXEC_NAME}}".to_string()],
			name: None,
			runs: 1,
			json_compare: false,
			dir_compare: false,
			expect_error: false,
		};

		let args = cmd.args_for_exec("test", None);
		assert_eq!(args, vec!["/test/test"]);
	}

	#[test]
	fn test_args_for_exec_no_placeholder() {
		let cmd = Command {
			args: vec!["eval".to_string(), "path".to_string()],
			name: None,
			runs: 1,
			json_compare: true,
			dir_compare: false,
			expect_error: false,
		};

		let args = cmd.args_for_exec("rtk", None);
		assert_eq!(args, vec!["eval", "path"]);
	}

	#[test]
	fn test_args_for_exec_export_format() {
		let cmd = Command {
			args: vec![
				"export".to_string(),
				"/tmp/out".to_string(),
				"{{EXPORT_FORMAT}}".to_string(),
				"path".to_string(),
			],
			name: None,
			runs: 1,
			json_compare: false,
			dir_compare: true,
			expect_error: false,
		};

		let args = cmd.args_for_exec("rtk", None);
		assert_eq!(args.len(), 5);
		assert_eq!(args[0], "export");
		assert_eq!(args[1], "/tmp/out");
		assert_eq!(args[2], "--format");
		assert!(args[3].contains("flux"));
		assert_eq!(args[4], "path");
	}

	#[test]
	fn test_as_string() {
		let cmd = Command {
			args: vec!["env".to_string(), "list".to_string(), "--json".to_string()],
			name: None,
			runs: 1,
			json_compare: false,
			dir_compare: false,
			expect_error: false,
		};

		assert_eq!(cmd.as_string(), "env list --json");
	}

	#[test]
	fn test_display_name_with_name() {
		let cmd = Command {
			args: vec!["eval".to_string(), "path".to_string()],
			name: Some("Test Command".to_string()),
			runs: 1,
			json_compare: false,
			dir_compare: false,
			expect_error: false,
		};

		assert_eq!(cmd.display_name(), "Test Command");
	}

	#[test]
	fn test_display_name_without_name() {
		let cmd = Command {
			args: vec!["eval".to_string(), "path".to_string()],
			name: None,
			runs: 1,
			json_compare: false,
			dir_compare: false,
			expect_error: false,
		};

		assert_eq!(cmd.display_name(), "eval path");
	}

	#[test]
	fn test_args_for_exec_list_main_files() {
		// Create a temporary directory structure with main.jsonnet files
		let temp_dir = std::env::temp_dir().join(format!("tk_compare_test_{}", std::process::id()));
		let env1_dir = temp_dir.join("environments").join("env1");
		let env2_dir = temp_dir.join("environments").join("env2");
		let lib_dir = temp_dir.join("lib");

		// Clean up if exists from previous test
		let _ = std::fs::remove_dir_all(&temp_dir);

		// Create directory structure
		std::fs::create_dir_all(&env1_dir).unwrap();
		std::fs::create_dir_all(&env2_dir).unwrap();
		std::fs::create_dir_all(&lib_dir).unwrap();

		// Create test files
		std::fs::write(env1_dir.join("main.jsonnet"), "").unwrap();
		std::fs::write(env2_dir.join("main.jsonnet"), "").unwrap();
		std::fs::write(env1_dir.join("spec.json"), "").unwrap(); // Should be ignored
		std::fs::write(lib_dir.join("helper.libsonnet"), "").unwrap(); // Should be ignored

		let cmd = Command {
			args: vec![
				"tool".to_string(),
				"importers".to_string(),
				"{{LIST_MAIN_FILES}}".to_string(),
			],
			name: None,
			runs: 1,
			json_compare: false,
			dir_compare: false,
			expect_error: false,
		};

		let args = cmd.args_for_exec("rtk", Some(temp_dir.to_str().unwrap()));

		// Should have tool, importers, and 2 main.jsonnet files
		assert_eq!(args.len(), 4);
		assert_eq!(args[0], "tool");
		assert_eq!(args[1], "importers");

		// Check that all main.jsonnet files are included with relative paths
		let file_args: Vec<_> = args[2..].iter().map(|s| s.as_str()).collect();
		assert!(file_args
			.iter()
			.any(|f| f.contains("environments/env1/main.jsonnet")
				|| f.contains("environments\\env1\\main.jsonnet")));
		assert!(file_args
			.iter()
			.any(|f| f.contains("environments/env2/main.jsonnet")
				|| f.contains("environments\\env2\\main.jsonnet")));

		// Verify spec.json and helper.libsonnet are not included
		assert!(!file_args.iter().any(|f| f.contains("spec.json")));
		assert!(!file_args.iter().any(|f| f.contains("helper.libsonnet")));

		// Clean up
		let _ = std::fs::remove_dir_all(&temp_dir);
	}

	#[test]
	fn test_args_for_exec_list_main_files_no_working_dir() {
		let cmd = Command {
			args: vec![
				"tool".to_string(),
				"importers".to_string(),
				"{{LIST_MAIN_FILES}}".to_string(),
			],
			name: None,
			runs: 1,
			json_compare: false,
			dir_compare: false,
			expect_error: false,
		};

		// When no working_dir is provided, {{LIST_MAIN_FILES}} should expand to nothing
		let args = cmd.args_for_exec("rtk", None);
		assert_eq!(args, vec!["tool", "importers"]);
	}
}
