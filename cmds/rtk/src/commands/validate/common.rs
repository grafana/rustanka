//! Shared utilities for validate subcommands.

use std::{
	cell::RefCell,
	collections::{HashMap, HashSet},
	fs,
	path::{Path, PathBuf},
	sync::Arc,
};

use anyhow::{Context, Result};
use jrsonnet_evaluator::{
	manifest::JsonFormat, stack::set_stack_depth_limit, trace::PathResolver, AsPathLike,
	FileImportResolver, ImportResolver, State, Val,
};
use jrsonnet_gcmodule::Acyclic;
use jrsonnet_parser::{SourceFile, SourcePath};
use jrsonnet_stdlib::ContextInitializer;
use walkdir::WalkDir;

/// Map of virtual file paths → contents, shared across worker threads.
pub type MemoryFiles = Arc<HashMap<PathBuf, Vec<u8>>>;

/// Import resolver that serves a fixed set of in-memory files and otherwise
/// delegates to a wrapped [`FileImportResolver`].
///
/// Used by the manifests validator so per-namespace manifest JSON can be
/// imported without first materializing it on disk. Virtual paths must be
/// absolute and unambiguous; they're matched verbatim against the imported
/// path (after resolving from the importing file's directory if relative).
#[derive(Acyclic)]
struct MemoryFileImportResolver {
	memory: MemoryFiles,
	inner: FileImportResolver,
}

impl ImportResolver for MemoryFileImportResolver {
	fn resolve_from(
		&self,
		from: &SourcePath,
		path: &dyn AsPathLike,
	) -> jrsonnet_evaluator::error::Result<SourcePath> {
		let resolve_path = path.as_path();
		let path_ref: &Path = resolve_path.as_ref();
		if path_ref.is_absolute() && self.memory.contains_key(path_ref) {
			return Ok(SourcePath::new(SourceFile::new(path_ref.to_path_buf())));
		}
		self.inner.resolve_from(from, path)
	}

	fn load_file_contents(
		&self,
		resolved: &SourcePath,
	) -> jrsonnet_evaluator::error::Result<Vec<u8>> {
		if let Some(file) = resolved.downcast_ref::<SourceFile>() {
			if let Some(bytes) = self.memory.get(file.path()) {
				return Ok(bytes.clone());
			}
		}
		self.inner.load_file_contents(resolved)
	}
}

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

/// Returns true if any validation file in `dir` defines `namespaceTest`.
pub fn any_validation_defines_namespace_test(dir: &Path) -> Result<bool> {
	for path in collect_validation_files(dir)? {
		let content = fs::read_to_string(&path)
			.with_context(|| format!("reading validation file {}", path.display()))?;
		if content.contains("namespaceTest") {
			return Ok(true);
		}
	}
	Ok(false)
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
///
/// When `memory` is `Some`, imports of paths present in the map are served from
/// memory; everything else falls through to the file-based resolver.
fn build_state(import_paths: &[PathBuf], memory: Option<MemoryFiles>) -> State {
	set_stack_depth_limit(500);
	let context_init = ContextInitializer::new(PathResolver::Absolute);
	crate::jsonnet::evaluator::jrsonnet::JrsonnetEvaluator::register_native_functions(
		&context_init,
	);
	let mut builder = State::builder();
	let inner = FileImportResolver::new(import_paths.to_vec());
	match memory {
		Some(memory) => {
			builder.import_resolver(MemoryFileImportResolver { memory, inner });
		}
		None => {
			builder.import_resolver(inner);
		}
	}
	builder.context_initializer(context_init);
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
	let state = build_state(import_paths, None);
	run_snippet_in_state(&state, snippet, "<validate>")
}

struct PooledStateEntry {
	import_paths: Vec<PathBuf>,
	memory: Option<MemoryFiles>,
	state: State,
}

thread_local! {
	/// Per-worker-thread jrsonnet State. Reused across many `eval_jsonnet_snippet_pooled`
	/// calls so that parsed/evaluated imports (such as `common.libsonnet` and the
	/// validation files themselves) are cached.
	///
	/// Keyed by the import-paths and memory-map identity to detect callers that
	/// need a fresh State.
	static POOLED_STATE: RefCell<Option<PooledStateEntry>> = const { RefCell::new(None) };
}

/// Per-element evaluation result.
///
/// `Ok(value)` is a successfully manifested element; `Err(message)` is the
/// per-element error string (e.g. a type error inside one of many `manifestTest`
/// calls). Used when the validate manifests runner needs to attribute errors to
/// the specific element/manifest that failed instead of failing the whole batch.
pub type ElementResult = std::result::Result<serde_json::Value, String>;

/// Evaluate a Jsonnet snippet that is expected to return an array, manifesting
/// each element independently.
///
/// If the snippet's top-level value is an array, each element is forced and
/// manifested separately. Forcing/manifest failures on a single element become
/// `Err(message)` for that index only; other elements still produce values.
/// If the snippet doesn't return an array, the function falls back to
/// manifesting the whole value as before and returns a single-element vector.
///
/// Errors at this function's `Result` level mean the snippet itself failed
/// to evaluate (e.g. parse error in the snippet) — distinct from per-element
/// runtime errors which are reported through `ElementResult`.
pub fn eval_jsonnet_snippet_array_pooled(
	snippet: &str,
	import_paths: &[PathBuf],
	memory: Option<&MemoryFiles>,
) -> Result<Vec<ElementResult>> {
	with_pooled_state(import_paths, memory, |state| {
		let _state_guard = state.enter();

		let result = state
			.evaluate_snippet("<validate>", snippet)
			.map_err(|e| anyhow::anyhow!("evaluation error:\n{}", e))?;

		let arr = match result {
			Val::Arr(arr) => arr,
			other => {
				let manifest = other
					.manifest(JsonFormat::default())
					.map_err(|e| anyhow::anyhow!("manifest error:\n{}", e))?;
				let value: serde_json::Value = serde_json::from_str(&manifest.to_string())
					.context("failed to parse result as JSON")?;
				return Ok(vec![Ok(value)]);
			}
		};

		let mut out: Vec<ElementResult> = Vec::with_capacity(arr.len());
		for idx in 0..arr.len() {
			let elem_result: ElementResult = match arr.get(idx) {
				Ok(Some(val)) => match val.manifest(JsonFormat::default()) {
					Ok(rendered) => match serde_json::from_str::<serde_json::Value>(&rendered) {
						Ok(v) => Ok(v),
						Err(e) => Err(format!("failed to parse element as JSON: {e}")),
					},
					Err(e) => Err(format!("manifest error:\n{e}")),
				},
				Ok(None) => Err("element out of bounds (jrsonnet bug?)".to_string()),
				Err(e) => Err(format!("evaluation error:\n{e}")),
			};
			out.push(elem_result);
		}
		Ok(out)
	})
}

/// Run `f` against the thread-local pooled State, rebuilding it if `import_paths`
/// or the memory-backed imports changed.
fn with_pooled_state<R>(
	import_paths: &[PathBuf],
	memory: Option<&MemoryFiles>,
	f: impl FnOnce(&State) -> R,
) -> R {
	POOLED_STATE.with(|cell| {
		let needs_new = match &*cell.borrow() {
			Some(entry) => {
				entry.import_paths.as_slice() != import_paths
					|| match (&entry.memory, memory) {
						(Some(a), Some(b)) => !Arc::ptr_eq(a, b),
						(None, None) => false,
						_ => true,
					}
			}
			None => true,
		};
		if needs_new {
			let memory_clone = memory.cloned();
			let state = build_state(import_paths, memory_clone.clone());
			*cell.borrow_mut() = Some(PooledStateEntry {
				import_paths: import_paths.to_vec(),
				memory: memory_clone,
				state,
			});
		}
		let state = cell
			.borrow()
			.as_ref()
			.map(|e| e.state.clone())
			.expect("just inserted");
		f(&state)
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
