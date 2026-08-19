//! Eval command handler.

use std::{
	io::Write,
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Args;

#[derive(Args)]
pub struct EvalArgs {
	/// Path to evaluate
	pub path: PathBuf,

	/// Evaluate expression on output of jsonnet
	#[arg(short = 'e', long)]
	pub eval: Option<String>,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,
}

/// Run the eval command with injected dependencies.
pub fn run<W: Write>(
	entrypoint: &Path,
	options: rtk_jsonnet::Options,
	expression: Option<&str>,
	mut writer: W,
) -> Result<()> {
	let jpath = rtk_jsonnet::jpath::JPath::resolve(entrypoint)?;
	let engine = rtk_jsonnet::Engine::new(options.clone());
	let mut evaluator = engine.create_evaluator();
	options
		.apply(&mut evaluator)
		.map_err(|error| anyhow::anyhow!(error.to_string()))?;
	evaluator
		.with_import_paths(jpath.import_paths)
		.map_err(|error| anyhow::anyhow!(error.to_string()))?;
	let evaluation = match expression {
		Some(expression) => {
			let separator = if expression.starts_with('[') { "" } else { "." };
			let entrypoint = serde_json::to_string(&jpath.entrypoint.to_string_lossy())?;
			let snippet = format!("local main = import {entrypoint}; main{separator}{expression}");
			evaluator.evaluate_snippet(snippet)
		}
		None => evaluator.evaluate_file(jpath.entrypoint),
	}
	.map_err(|error| anyhow::anyhow!(error.to_string()))?;
	let value: serde_json::Value = evaluation
		.into_value()
		.deserialize()
		.map_err(|error| anyhow::anyhow!(error.to_string()))?;
	let output = serde_json::to_string_pretty(&value).context("serializing evaluation")?;
	write!(writer, "{}", output)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{commands::common::BrokenPipeGuard, test_utils::BrokenPipeWriter};
	use assert_matches::assert_matches;

	fn entrypoint(contents: &str) -> (tempfile::TempDir, PathBuf) {
		let directory = tempfile::tempdir().unwrap();
		std::fs::write(directory.path().join("jsonnetfile.json"), "{}").unwrap();
		let entrypoint = directory.path().join("main.jsonnet");
		std::fs::write(&entrypoint, contents).unwrap();
		(directory, entrypoint)
	}

	#[test]
	fn test_eval_outputs_json_object() {
		let (_directory, entrypoint) = entrypoint(r#"{ name: "test", value: 42 }"#);
		let mut output = Vec::new();
		run(
			&entrypoint,
			rtk_jsonnet::Options::default(),
			None,
			&mut output,
		)
		.expect("eval should succeed");
		let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
		assert_eq!(value, serde_json::json!({"name": "test", "value": 42}));
	}

	#[test]
	fn test_eval_exits_cleanly_on_broken_pipe() {
		let (_directory, entrypoint) = entrypoint(r#"{ name: "test" }"#);
		// Wrap BrokenPipeWriter with BrokenPipeGuard to test the guard handles broken pipes
		let writer = BrokenPipeGuard::new(BrokenPipeWriter);
		let write_result = run(&entrypoint, rtk_jsonnet::Options::default(), None, writer);
		assert_matches!(write_result, Ok(()));
	}
}
