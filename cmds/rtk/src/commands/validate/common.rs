//! Shared utilities for validate subcommands.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jrsonnet_evaluator::{
	manifest::JsonFormat, stack::set_stack_depth_limit, trace::PathResolver, FileImportResolver,
	State,
};
use jrsonnet_stdlib::ContextInitializer;
use walkdir::WalkDir;

/// Collect validation files (*.jsonnet, excluding *_test.jsonnet) from a directory.
pub fn collect_validation_files(dir: &Path) -> Result<Vec<PathBuf>> {
	let mut files = Vec::new();

	for entry in WalkDir::new(dir)
		.into_iter()
		.filter_map(|e| e.ok())
		.filter(|e| e.file_type().is_file())
	{
		let path = entry.path();
		let name = match path.file_name().and_then(|n| n.to_str()) {
			Some(n) => n,
			None => continue,
		};

		// Only .jsonnet files, not .libsonnet
		if !name.ends_with(".jsonnet") {
			continue;
		}

		// Exclude test files
		if name.ends_with("_test.jsonnet") {
			continue;
		}

		files.push(path.to_path_buf());
	}

	files.sort();
	Ok(files)
}

/// Collect test files (*_test.jsonnet) from a directory.
pub fn collect_test_files(dir: &Path) -> Result<Vec<PathBuf>> {
	let mut files = Vec::new();

	for entry in WalkDir::new(dir)
		.into_iter()
		.filter_map(|e| e.ok())
		.filter(|e| e.file_type().is_file())
	{
		let path = entry.path();
		let name = match path.file_name().and_then(|n| n.to_str()) {
			Some(n) => n,
			None => continue,
		};

		if name.ends_with("_test.jsonnet") {
			files.push(path.to_path_buf());
		}
	}

	files.sort();
	Ok(files)
}

/// For a test file like `foo_test.jsonnet`, find the corresponding validation file `foo.jsonnet`.
pub fn find_validation_file_for_test(test_file: &Path) -> Option<PathBuf> {
	let name = test_file.file_name()?.to_str()?;
	let base = name.strip_suffix("_test.jsonnet")?;
	let validation_name = format!("{}.jsonnet", base);
	let validation_path = test_file.with_file_name(validation_name);
	if validation_path.exists() {
		Some(validation_path)
	} else {
		None
	}
}

/// Evaluate a Jsonnet snippet in memory without writing temp files.
///
/// Creates a fresh jrsonnet State with the given import paths and evaluates the
/// snippet directly using `evaluate_snippet`. Each call gets its own State so
/// there are no import cache collisions.
pub fn eval_jsonnet_snippet(snippet: &str, import_paths: &[PathBuf]) -> Result<serde_json::Value> {
	set_stack_depth_limit(500);

	let context_init = ContextInitializer::new(PathResolver::Absolute);
	let import_resolver = FileImportResolver::new(import_paths.to_vec());

	let mut builder = State::builder();
	builder
		.import_resolver(import_resolver)
		.context_initializer(context_init);
	let state = builder.build();

	let _state_guard = state.enter();

	let result = state
		.evaluate_snippet("<validate>", snippet)
		.map_err(|e| anyhow::anyhow!("evaluation error:\n{}", e))?;

	let manifest = result
		.manifest(JsonFormat::default())
		.map_err(|e| anyhow::anyhow!("manifest error:\n{}", e))?;

	let value: serde_json::Value =
		serde_json::from_str(&manifest.to_string()).context("failed to parse result as JSON")?;

	Ok(value)
}

/// Evaluate a validation file's function against JSON data.
///
/// Imports the file at `test_file`, calls `function_name(data)` where data is the
/// provided JSON string (embedded directly since JSON is valid Jsonnet).
///
/// Returns Ok(None) if the function returns null (pass), Ok(Some(error)) if the
/// function returns a string, or Err if evaluation fails.
pub fn run_validation_function(
	test_file: &Path,
	function_name: &str,
	data_json: &str,
	import_paths: &[PathBuf],
) -> Result<Option<String>> {
	let test_file_abs = test_file
		.canonicalize()
		.with_context(|| format!("resolving path {}", test_file.display()))?;

	let snippet = format!(
		"local test = import '{}';\nlocal data = {};\ntest.{}(data)",
		test_file_abs.to_string_lossy().replace('\\', "/"),
		data_json,
		function_name,
	);

	let value = eval_jsonnet_snippet(&snippet, import_paths)?;

	match value {
		serde_json::Value::Null => Ok(None),
		serde_json::Value::String(s) => Ok(Some(s)),
		other => Ok(Some(format!("unexpected return type: {}", other))),
	}
}
