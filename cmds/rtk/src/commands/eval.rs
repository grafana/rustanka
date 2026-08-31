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
	let engine = rtk_environments::Engine::new(rtk_jsonnet::Engine::new(options));
	let value = engine
		.eval(entrypoint, expression)
		.map_err(super::common::environment_error)?;
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

	/// An environment whose `spec.json` sits beside its entrypoint.
	fn static_environment(spec: &str, contents: &str) -> (tempfile::TempDir, PathBuf) {
		let (directory, entrypoint) = entrypoint(contents);
		std::fs::write(directory.path().join("spec.json"), spec).unwrap();
		(directory, entrypoint)
	}

	fn eval(entrypoint: &Path, expression: Option<&str>) -> Result<serde_json::Value> {
		let mut output = Vec::new();
		run(
			entrypoint,
			rtk_jsonnet::Options::default(),
			expression,
			&mut output,
		)?;
		Ok(serde_json::from_slice(&output).unwrap())
	}

	#[test]
	fn test_eval_outputs_json_object() {
		let (_directory, entrypoint) = entrypoint(r#"{ name: "test", value: 42 }"#);
		let value = eval(&entrypoint, None).expect("eval should succeed");
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

	/// tk's `StaticLoader.Eval` hands a static environment its own spec, so a
	/// program reading it evaluates under `eval` exactly as it does under
	/// `export`.
	#[test]
	fn a_static_environment_can_read_its_own_spec() {
		let (_directory, entrypoint) = static_environment(
			r#"{
				"apiVersion": "tanka.dev/v1alpha1",
				"kind": "Environment",
				"metadata": {},
				"spec": { "apiServer": "https://example:6443", "namespace": "nsx" }
			}"#,
			"{ spec: std.extVar('tanka.dev/environment').spec }",
		);

		let value = eval(&entrypoint, None).expect("the extVar should be defined");
		let spec = &value["spec"];
		assert_eq!(spec["namespace"], "nsx");
		assert_eq!(spec["apiServer"], "https://example:6443");
		// Go marshals these whatever they hold, so both are always present.
		assert_eq!(spec["resourceDefaults"], serde_json::json!({}));
		assert_eq!(spec["expectVersions"], serde_json::json!({}));
	}

	/// And an inline one is told why it cannot, in tk's words.
	#[test]
	fn an_inline_environment_is_told_why_it_has_no_spec() {
		let (_directory, entrypoint) = entrypoint("{ spec: std.extVar('tanka.dev/environment') }");

		let error = eval(&entrypoint, None).expect_err("reading the extVar should fail");
		let report = format!("{error:#}");
		assert!(
			report.contains(
				"only supported for static environments. Directly access this data using \
				 standard Jsonnet instead."
			),
			"expected tk's explanation, got: {report}"
		);
	}

	/// The error costs nothing until something reads it.
	#[test]
	fn an_inline_environment_that_never_reads_it_is_unaffected() {
		let (_directory, entrypoint) = entrypoint("{ quiet: true }");
		let value = eval(&entrypoint, None).expect("eval should succeed");
		assert_eq!(value, serde_json::json!({ "quiet": true }));
	}

	/// tk's `PatternEvalScript`: a leading bracket indexes, anything else names.
	#[test]
	fn an_expression_selects_a_field_or_indexes() {
		let (_directory, entrypoint) = entrypoint("{ outer: { inner: [10, 20] } }");

		assert_eq!(
			eval(&entrypoint, Some("outer.inner")).unwrap(),
			serde_json::json!([10, 20])
		);
		assert_eq!(
			eval(&entrypoint, Some("[\"outer\"].inner[1]")).unwrap(),
			serde_json::json!(20)
		);
	}

	/// An expression should reach the spec too, being the same evaluation.
	#[test]
	fn an_expression_still_sees_the_environment() {
		let (_directory, entrypoint) = static_environment(
			r#"{
				"apiVersion": "tanka.dev/v1alpha1",
				"kind": "Environment",
				"metadata": {},
				"spec": { "namespace": "picked" }
			}"#,
			"{ here: std.extVar('tanka.dev/environment').spec.namespace }",
		);

		assert_eq!(
			eval(&entrypoint, Some("here")).unwrap(),
			serde_json::json!("picked")
		);
	}
}
