//! Show command handler.
//!
//! Evaluates a Tanka environment and outputs the resulting Kubernetes manifests
//! as YAML. This is equivalent to `tk show`.

use std::{
	io::Write,
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Args;
use rtk_environments::export::{serialize_manifest, Error as EnvironmentError};
use rtk_environments::Engine;
use rtk_jsonnet::{EvaluationValue, Options as JsonnetOptions};
use tracing::instrument;

#[derive(Args)]
pub struct ShowArgs {
	/// Path to the Tanka environment
	pub path: PathBuf,

	/// Allow redirecting output to a file or a pipe
	#[arg(long)]
	pub dangerous_allow_redirect: bool,

	/// String that only a single inline environment contains in its name
	#[arg(long)]
	pub name: Option<String>,

	/// Regex filter on '<kind>/<name>'. See https://tanka.dev/output-filtering
	#[arg(short = 't', long)]
	pub target: Vec<String>,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,
}

/// Options for the show operation.
#[derive(Default)]
pub struct ShowOpts {
	/// Target filters.
	pub target: Vec<String>,
	/// Filter environments by name (exact match first, then substring).
	pub name: Option<String>,
}

/// Run the show command.
pub fn run<W: Write>(args: ShowArgs, mut writer: W) -> Result<()> {
	// Check redirect safety (matches tk behavior)
	let is_terminal = std::io::IsTerminal::is_terminal(&std::io::stdout());
	let allow_redirect_env = std::env::var("TANKA_DANGEROUS_ALLOW_REDIRECT")
		.map(|v| v == "true")
		.unwrap_or(false);
	let allow_redirect = allow_redirect_env || args.dangerous_allow_redirect;

	if !is_terminal && !allow_redirect {
		eprintln!(
			"Redirection of the output of rtk show is discouraged and disabled by default.
If you want to export .yaml files for use with other tools, try 'rtk export'.
Otherwise run:
  rtk show --dangerous-allow-redirect
or set the environment variable
  TANKA_DANGEROUS_ALLOW_REDIRECT=true
to bypass this check."
		);
		return Ok(());
	}

	let jsonnet = args.jsonnet.into_options();
	let opts = ShowOpts {
		target: args.target,
		name: args.name,
	};
	let output = show_environment(&args.path, jsonnet, opts)?;

	write!(writer, "{}", output)?;
	Ok(())
}

/// Show an environment and return the YAML output.
#[instrument(skip_all, fields(path = %path.display()))]
pub fn show_environment(path: &Path, jsonnet: JsonnetOptions, opts: ShowOpts) -> Result<String> {
	let engine = Engine::new(rtk_jsonnet::Engine::new(jsonnet));
	let environment = engine
		.load_single(path, opts.name.as_deref())
		.map_err(environment_error)?;
	let manifests = engine
		.manifests(&environment, &opts.target)
		.map_err(environment_error)?;
	tracing::debug!(manifest_count = manifests.len(), "found manifests to show");
	manifests_to_yaml(manifests)
}

fn environment_error(error: EnvironmentError) -> anyhow::Error {
	anyhow::anyhow!(error.report())
}

/// Convert evaluated manifests directly to a YAML stream.
fn manifests_to_yaml(manifests: Vec<EvaluationValue>) -> Result<String> {
	let mut output = String::new();
	for (i, manifest) in manifests.iter().enumerate() {
		if i > 0 {
			output.push_str("---\n");
		}
		let yaml = serialize_manifest(manifest).context("serializing manifest to YAML")?;
		output.push_str(&yaml);
	}
	Ok(output)
}

#[cfg(test)]
mod tests {
	use std::{fs, path::PathBuf};

	use tempfile::TempDir;

	use super::*;

	fn setup_test_env(temp: &TempDir, main_content: &str) -> PathBuf {
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), r#"{"version": 1}"#).unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), main_content).unwrap();
		root.join("env")
	}

	#[test]
	fn test_show_single_manifest() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{
				apiVersion: 'v1',
				kind: 'ConfigMap',
				metadata: { name: 'test-cm', namespace: 'default' },
				data: { key: 'value' }
			}"#,
		);

		let output =
			show_environment(&env_path, JsonnetOptions::default(), ShowOpts::default()).unwrap();

		assert!(output.contains("apiVersion: v1"));
		assert!(output.contains("kind: ConfigMap"));
		assert!(output.contains("name: test-cm"));
	}

	#[test]
	fn test_show_multiple_manifests() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{
				cm: {
					apiVersion: 'v1',
					kind: 'ConfigMap',
					metadata: { name: 'cm1' },
				},
				secret: {
					apiVersion: 'v1',
					kind: 'Secret',
					metadata: { name: 'secret1' },
				}
			}"#,
		);

		let output =
			show_environment(&env_path, JsonnetOptions::default(), ShowOpts::default()).unwrap();

		// Should have document separator between manifests
		assert!(output.contains("---"));
		assert!(output.contains("kind: ConfigMap"));
		assert!(output.contains("kind: Secret"));
	}

	#[test]
	fn test_show_with_target_filter() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{
				cm: {
					apiVersion: 'v1',
					kind: 'ConfigMap',
					metadata: { name: 'cm1' },
				},
				secret: {
					apiVersion: 'v1',
					kind: 'Secret',
					metadata: { name: 'secret1' },
				}
			}"#,
		);

		let output = show_environment(
			&env_path,
			JsonnetOptions::default(),
			ShowOpts {
				target: vec!["ConfigMap/.*".to_string()],
				..Default::default()
			},
		)
		.unwrap();

		assert!(output.contains("kind: ConfigMap"));
		assert!(!output.contains("kind: Secret"));
	}

	#[test]
	fn test_show_inline_environment() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{
				apiVersion: 'tanka.dev/v1alpha1',
				kind: 'Environment',
				metadata: { name: 'my-env' },
				spec: { namespace: 'default' },
				data: {
					cm: {
						apiVersion: 'v1',
						kind: 'ConfigMap',
						metadata: { name: 'inline-cm' },
					}
				}
			}"#,
		);

		let output =
			show_environment(&env_path, JsonnetOptions::default(), ShowOpts::default()).unwrap();

		assert!(output.contains("kind: ConfigMap"));
		assert!(output.contains("name: inline-cm"));
	}

	#[test]
	fn test_show_injects_namespace_from_spec() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), r#"{"version": 1}"#).unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(
			root.join("env/main.jsonnet"),
			r#"{
				apiVersion: 'v1',
				kind: 'ConfigMap',
				metadata: { name: 'test-cm' },
				data: { key: 'value' }
			}"#,
		)
		.unwrap();
		fs::write(
			root.join("env/spec.json"),
			r#"{
				"apiVersion": "tanka.dev/v1alpha1",
				"kind": "Environment",
				"metadata": { "name": "test-env" },
				"spec": { "namespace": "my-namespace", "injectLabels": true }
			}"#,
		)
		.unwrap();

		let output = show_environment(
			&root.join("env"),
			JsonnetOptions::default(),
			ShowOpts::default(),
		)
		.unwrap();

		assert!(
			output.contains("namespace: my-namespace"),
			"Expected namespace injection from spec.json, got:\n{}",
			output
		);
	}
}
