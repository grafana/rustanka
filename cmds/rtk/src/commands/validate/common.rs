//! Shared utilities for validate subcommands.

use std::{
	cell::RefCell,
	collections::HashSet,
	path::{Path, PathBuf},
};

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

/// Build a fresh jrsonnet State configured for validation snippets.
///
/// The State has the rtk Tanka-compatible native functions registered (e.g.
/// `std.native('regexMatch')`) so validation files can use them.
fn build_state(import_paths: &[PathBuf]) -> State {
	set_stack_depth_limit(500);
	let context_init = ContextInitializer::new(PathResolver::Absolute);
	crate::jsonnet::evaluator::jrsonnet::JrsonnetEvaluator::register_native_functions(
		&context_init,
	);
	let mut builder = State::builder();
	builder
		.import_resolver(FileImportResolver::new(import_paths.to_vec()))
		.context_initializer(context_init);
	builder.build()
}

/// Evaluate a Jsonnet snippet inside a State, returning the JSON-encoded result.
fn run_snippet_in_state(
	state: &State,
	snippet: &str,
	name: &'static str,
) -> Result<serde_json::Value> {
	let _state_guard = state.enter();

	let result = state
		.evaluate_snippet(name, snippet)
		.map_err(|e| anyhow::anyhow!("evaluation error:\n{}", e))?;

	let manifest = result
		.manifest(JsonFormat::default())
		.map_err(|e| anyhow::anyhow!("manifest error:\n{}", e))?;

	let value: serde_json::Value =
		serde_json::from_str(&manifest.to_string()).context("failed to parse result as JSON")?;

	Ok(value)
}

/// Evaluate a Jsonnet snippet in memory without writing temp files.
///
/// Creates a fresh jrsonnet State with the given import paths and evaluates the
/// snippet directly using `evaluate_snippet`. Each call gets its own State so
/// there are no import cache collisions.
pub fn eval_jsonnet_snippet(snippet: &str, import_paths: &[PathBuf]) -> Result<serde_json::Value> {
	let state = build_state(import_paths);
	run_snippet_in_state(&state, snippet, "<validate>")
}

thread_local! {
	/// Per-worker-thread jrsonnet State. Reused across many `eval_jsonnet_snippet_pooled`
	/// calls so that parsed/evaluated imports (such as `common.libsonnet` and the
	/// validation files themselves) are cached.
	///
	/// Keyed by the import-paths to detect callers that need a fresh State for a
	/// different tests directory (rare in practice but kept correct).
	static POOLED_STATE: RefCell<Option<(Vec<PathBuf>, State)>> = const { RefCell::new(None) };
}

/// Evaluate a Jsonnet snippet using a thread-local pooled State.
///
/// Unlike [`eval_jsonnet_snippet`], the State is kept alive between calls on the
/// same thread so that jrsonnet's internal file/AST cache survives. This makes a
/// large difference when many small snippets share the same set of imports
/// (e.g., the validation manifests runner).
pub fn eval_jsonnet_snippet_pooled(
	snippet: &str,
	import_paths: &[PathBuf],
) -> Result<serde_json::Value> {
	POOLED_STATE.with(|cell| {
		let needs_new = match &*cell.borrow() {
			Some((paths, _)) => paths.as_slice() != import_paths,
			None => true,
		};
		if needs_new {
			let state = build_state(import_paths);
			*cell.borrow_mut() = Some((import_paths.to_vec(), state));
		}
		let state = cell
			.borrow()
			.as_ref()
			.map(|(_, s)| s.clone())
			.expect("just inserted");
		run_snippet_in_state(&state, snippet, "<validate>")
	})
}

/// Extract the optional `kinds: [...]` filter from a validation file.
///
/// Returns `None` if the validation file does not declare a `kinds` field
/// (meaning it applies to every manifest), or `Some(set)` of the kinds it
/// applies to. Errors if `kinds` exists but isn't an array of strings.
pub fn extract_kinds_filter(
	validation_file: &Path,
	import_paths: &[PathBuf],
) -> Result<Option<HashSet<String>>> {
	let file_abs = validation_file
		.canonicalize()
		.with_context(|| format!("resolving path {}", validation_file.display()))?;

	let snippet = format!(
		"local v = import '{}';\nif std.objectHas(v, 'kinds') then v.kinds else null",
		file_abs.to_string_lossy().replace('\\', "/"),
	);

	let value = eval_jsonnet_snippet(&snippet, import_paths)?;

	match value {
		serde_json::Value::Null => Ok(None),
		serde_json::Value::Array(arr) => {
			let mut kinds = HashSet::new();
			for item in &arr {
				match item.as_str() {
					Some(s) => {
						kinds.insert(s.to_string());
					}
					None => {
						anyhow::bail!(
							"{}: kinds must be an array of strings, got: {}",
							validation_file.display(),
							item
						);
					}
				}
			}
			Ok(Some(kinds))
		}
		other => {
			anyhow::bail!(
				"{}: kinds must be an array of strings, got: {}",
				validation_file.display(),
				other
			);
		}
	}
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
