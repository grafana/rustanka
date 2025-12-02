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
	pub fn args_for_exec(&self, exec_name: &str) -> Vec<String> {
		let export_format_template = "{{ if not env.metadata.labels.fluxExport }}flux{{ else if eq env.metadata.labels.fluxExport \"true\" }}flux{{ else }}flux-disabled{{ end }}/{{ env.metadata.labels.cluster_name }}/{{ if .metadata.labels.fluxExportDir }}{{ .metadata.labels.fluxExportDir }}{{ else if env.metadata.labels.fluxExportDir }}{{ env.metadata.labels.fluxExportDir }}{{ else if .metadata.namespace }}{{.metadata.namespace}}{{ else }}_cluster{{ end }}/{{.kind}}-{{.metadata.name}}";

		let mut result = Vec::new();
		for arg in &self.args {
			let arg = arg.replace("{{EXEC_NAME}}", exec_name);
			if arg == "{{EXPORT_FORMAT}}" {
				// Expand to two separate arguments
				result.push("--format".to_string());
				result.push(export_format_template.to_string());
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

		let args = cmd.args_for_exec("rtk");
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

		let args = cmd.args_for_exec("test");
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

		let args = cmd.args_for_exec("rtk");
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

		let args = cmd.args_for_exec("rtk");
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
}
