//! Eval command handler.

use std::{
	io::Write,
	path::{Path, PathBuf},
};

use crate::jsonnet::evaluator::{
	DefaultEvaluator, Evaluator, EvaluatorOptions, GlobalEvaluatorOptions,
};
use anyhow::Result;
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
	global_opts: GlobalEvaluatorOptions,
	opts: EvaluatorOptions,
	mut writer: W,
) -> Result<()> {
	let evaluator = DefaultEvaluator::new(global_opts);
	let result = evaluator.eval_file(entrypoint, &opts)?;
	let output = serde_json::to_string_pretty(&result.value)?;
	write!(writer, "{}", output)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use assert_matches::assert_matches;
	use indoc::indoc;

	use super::*;
	use crate::{
		commands::common::BrokenPipeGuard,
		jsonnet::evaluator::JrsonnetEvaluator,
		test_utils::{BrokenPipeWriter, MemoryImportResolver},
	};

	const ENTRYPOINT: &str = "/test/main.jsonnet";

	#[test]
	fn test_eval_outputs_json_object() {
		let resolver = MemoryImportResolver::new().with_file(
			ENTRYPOINT,
			indoc! {r#"
				{
					name: "test",
					value: 42,
				}
			"#},
		);

		let evaluator = JrsonnetEvaluator::new(GlobalEvaluatorOptions::default());
		let result = evaluator
			.eval_snippet_with_import_resolver(
				format!("(import {:?})", ENTRYPOINT),
				resolver,
				&EvaluatorOptions::default(),
			)
			.expect("eval should succeed");

		assert_eq!(
			result.value,
			serde_json::json!({
				"name": "test",
				"value": 42
			})
		);
	}

	#[test]
	fn test_eval_exits_cleanly_on_broken_pipe() {
		let resolver = MemoryImportResolver::new().with_file(
			ENTRYPOINT,
			indoc! {r#"
				{
					name: "test",
				}
			"#},
		);

		let evaluator = JrsonnetEvaluator::new(GlobalEvaluatorOptions::default());
		let result = evaluator
			.eval_snippet_with_import_resolver(
				format!("(import {:?})", ENTRYPOINT),
				resolver,
				&EvaluatorOptions::default(),
			)
			.expect("eval should succeed");

		let output = serde_json::to_string_pretty(&result.value).expect("serialize");
		// Wrap BrokenPipeWriter with BrokenPipeGuard to test the guard handles broken pipes
		let mut writer = BrokenPipeGuard::new(BrokenPipeWriter);
		let write_result = write!(writer, "{}", output);
		assert_matches!(write_result, Ok(()));
	}
}
