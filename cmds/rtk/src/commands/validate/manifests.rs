//! Validate manifests command handler.
//!
//! Validates exported manifests against Jsonnet validation files.
//!
//! Validation files are `<name>.jsonnet` files (excluding `*_test.jsonnet`) that define
//! one or both of:
//! - `namespaceTest(manifests)` - receives all manifests in a namespace, returns null on success or an error string
//! - `manifestTest(manifest)` - receives a single manifest, returns null on success or an error string

use std::{
	collections::{BTreeMap, BinaryHeap, HashSet},
	fs,
	io::Write,
	path::{Path, PathBuf},
	sync::Mutex,
	time::{Duration, Instant},
};

use anyhow::{Context, Result};
use clap::Args;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use tracing::debug;
use walkdir::WalkDir;

use super::common;

#[derive(Args)]
pub struct ManifestsArgs {
	/// Export directory containing manifests
	pub export_dir: String,

	/// Look recursively for manifests in subdirectories
	#[arg(short = 'r', long)]
	pub recursive: bool,

	/// Directory containing Jsonnet validation files
	#[arg(long)]
	pub tests_dir: String,

	/// Log the N slowest work items at the end of the run
	#[arg(long)]
	pub log_slowest: Option<usize>,
}

/// A parsed manifest with its source file path and namespace.
struct ParsedManifest {
	/// Relative path of the source YAML file
	source_file: String,
	/// Kubernetes resource kind (e.g. "Deployment", "ConfigMap")
	kind: String,
	/// Kubernetes namespace from the manifest (or empty string if none)
	namespace: String,
	/// The manifest as a JSON value
	value: serde_json::Value,
}

/// Pre-processed info about a validation file.
struct ValidationFileInfo {
	/// Display name (relative to tests dir)
	display_name: String,
	/// Absolute path string for use in Jsonnet import statements
	abs_import_path: String,
	/// Whether the file defines namespaceTest
	has_namespace_test: bool,
	/// Whether the file defines manifestTest
	has_manifest_test: bool,
	/// Optional kinds filter for manifestTest
	kinds_filter: Option<HashSet<String>>,
}

/// Descriptor for one element of the eval result array (one test run).
struct ResultDescriptor {
	validation_file_name: String,
	subject: String,
	test_kind: &'static str,
}

/// A batched work item: one eval per namespace running namespaceTest(allManifests) and all applicable manifest tests.
struct BatchWorkItem {
	/// Jsonnet snippet to evaluate (returns a JSON array)
	snippet: String,
	/// Human-readable subject for slowest report (namespace name)
	subject: String,
	/// One descriptor per array element, in order
	result_descriptors: Vec<ResultDescriptor>,
}

/// An entry in the slowest-items tracker.
#[derive(Eq, PartialEq)]
struct SlowEntry {
	duration: Duration,
	test_kind: &'static str,
	subject: String,
	validation_file_names: Vec<String>,
}

impl Ord for SlowEntry {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		// Min-heap: the smallest duration is "greatest" so it gets popped first
		other.duration.cmp(&self.duration)
	}
}

impl PartialOrd for SlowEntry {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

/// Tracks the N slowest work items using a bounded min-heap.
struct SlowestTracker {
	max: usize,
	heap: Mutex<BinaryHeap<SlowEntry>>,
}

impl SlowestTracker {
	fn new(max: usize) -> Self {
		Self {
			max,
			heap: Mutex::new(BinaryHeap::with_capacity(max + 1)),
		}
	}

	fn record(&self, entry: SlowEntry) {
		let mut heap = self.heap.lock().unwrap();
		heap.push(entry);
		if heap.len() > self.max {
			heap.pop(); // drop the smallest (fastest)
		}
	}

	/// Drain entries sorted slowest-first.
	fn into_sorted(self) -> Vec<SlowEntry> {
		let mut entries: Vec<_> = self.heap.into_inner().unwrap().into_vec();
		entries.sort_by(|a, b| b.duration.cmp(&a.duration));
		entries
	}
}

/// A test result for a single test execution.
struct TestResult {
	/// Name of the test file
	test_file: String,
	/// What was tested (manifest path or namespace name)
	subject: String,
	/// Whether it's a namespace test or manifest test
	test_kind: &'static str,
	/// null = pass, string = error message
	error: Option<String>,
}

/// Run the validate manifests command.
pub fn run<W: Write>(args: ManifestsArgs, mut writer: W) -> Result<()> {
	let export_dir = PathBuf::from(&args.export_dir);
	let tests_dir = PathBuf::from(&args.tests_dir);

	debug!(
		export_dir = %export_dir.display(),
		tests_dir = %tests_dir.display(),
		recursive = args.recursive,
		"starting validate manifests"
	);

	if !export_dir.is_dir() {
		anyhow::bail!("export directory does not exist: {}", export_dir.display());
	}
	if !tests_dir.is_dir() {
		anyhow::bail!("tests directory does not exist: {}", tests_dir.display());
	}

	// Step 1: Collect and parse all manifests from the export directory
	let manifests = collect_manifests(&export_dir, args.recursive)?;
	if manifests.is_empty() {
		writeln!(writer, "No manifests found in {}", export_dir.display())?;
		return Ok(());
	}
	writeln!(writer, "Found {} manifests", manifests.len())?;

	debug!(
		manifest_count = manifests.len(),
		"collected manifests from export directory"
	);

	// Step 2: Collect validation files (*.jsonnet, excluding *_test.jsonnet)
	let validation_files = common::collect_validation_files(&tests_dir)?;
	if validation_files.is_empty() {
		writeln!(
			writer,
			"No validation files found in {}",
			tests_dir.display()
		)?;
		return Ok(());
	}
	writeln!(writer, "Found {} validation files", validation_files.len())?;
	writeln!(writer)?;

	debug!(
		validation_files = ?validation_files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(),
		"collected validation files"
	);

	// Step 3: Group manifests by namespace for namespace tests
	let by_namespace = group_by_namespace(&manifests);

	debug!(
		namespaces = ?by_namespace.keys().collect::<Vec<_>>(),
		"grouped manifests by namespace"
	);

	// Step 4: Pre-process validation files (extract kinds, resolve paths)
	let import_paths = vec![tests_dir.clone()];
	let mut validation_infos = Vec::new();

	for validation_file in &validation_files {
		let file_display = validation_file
			.strip_prefix(&tests_dir)
			.unwrap_or(validation_file)
			.to_string_lossy()
			.to_string();

		debug!(validation_file = %file_display, "processing validation file");

		let content = fs::read_to_string(validation_file)
			.with_context(|| format!("reading validation file {}", validation_file.display()))?;

		let has_namespace_test = content.contains("namespaceTest");
		let has_manifest_test = content.contains("manifestTest");

		debug!(
			validation_file = %file_display,
			has_namespace_test,
			has_manifest_test,
			"detected test functions"
		);

		if !has_namespace_test && !has_manifest_test {
			writeln!(
				writer,
				"WARN: {} defines neither namespaceTest nor manifestTest, skipping",
				file_display
			)?;
			continue;
		}

		let abs_import_path = validation_file
			.canonicalize()
			.with_context(|| format!("resolving path {}", validation_file.display()))?
			.to_string_lossy()
			.replace('\\', "/");

		let kinds_filter = if has_manifest_test {
			extract_kinds_filter(validation_file, &import_paths)?
		} else {
			None
		};

		debug!(
			validation_file = %file_display,
			kinds_filter = ?kinds_filter,
			"resolved validation file"
		);

		validation_infos.push(ValidationFileInfo {
			display_name: file_display,
			abs_import_path,
			has_namespace_test,
			has_manifest_test,
			kinds_filter,
		});
	}

	// Step 5: Build one work item per namespace — one eval runs namespaceTest(allManifests) and all applicable manifest tests
	let ns_validations: Vec<&ValidationFileInfo> = validation_infos
		.iter()
		.filter(|v| v.has_namespace_test)
		.collect();
	let manifest_validations: Vec<&ValidationFileInfo> = validation_infos
		.iter()
		.filter(|v| v.has_manifest_test)
		.collect();

	let mut work_items: Vec<BatchWorkItem> = Vec::new();
	for (namespace, ns_manifests) in &by_namespace {
		let ns_display = if namespace.is_empty() {
			"(cluster-scoped)".to_string()
		} else {
			namespace.clone()
		};

		let manifests_json: Vec<&serde_json::Value> =
			ns_manifests.iter().map(|m| &m.value).collect();
		let all_manifests_json = serde_json::to_string(&manifests_json)?;

		let mut snippet = format!("local allManifests = {};\n[\n", all_manifests_json);
		let mut result_descriptors: Vec<ResultDescriptor> = Vec::new();

		// namespaceTest(allManifests) for each validation file that has it (only for named namespaces)
		if !namespace.is_empty() {
			for v in &ns_validations {
				snippet.push_str(&format!(
					"  (import '{}').namespaceTest(allManifests),\n",
					v.abs_import_path
				));
				result_descriptors.push(ResultDescriptor {
					validation_file_name: v.display_name.clone(),
					subject: ns_display.clone(),
					test_kind: "namespaceTest",
				});
			}
		}

		// manifestTest(allManifests[i]) for each manifest in this namespace, for each applicable validation file
		for (idx, manifest) in ns_manifests.iter().enumerate() {
			let applicable: Vec<&ValidationFileInfo> = manifest_validations
				.iter()
				.filter(|v| match &v.kinds_filter {
					Some(kinds) => kinds.contains(&manifest.kind),
					None => true,
				})
				.copied()
				.collect();

			for v in &applicable {
				snippet.push_str(&format!(
					"  (import '{}').manifestTest(allManifests[{}]),\n",
					v.abs_import_path, idx
				));
				result_descriptors.push(ResultDescriptor {
					validation_file_name: v.display_name.clone(),
					subject: manifest.source_file.clone(),
					test_kind: "manifestTest",
				});
			}
		}

		if result_descriptors.is_empty() {
			continue;
		}

		snippet.push(']');
		work_items.push(BatchWorkItem {
			snippet,
			subject: ns_display,
			result_descriptors,
		});
	}

	// Sort heaviest items first so rayon picks them up early
	work_items.sort_by(|a, b| b.snippet.len().cmp(&a.snippet.len()));

	debug!(
		work_item_count = work_items.len(),
		"running batched tests in parallel"
	);

	// Execute all batched work items in parallel
	let slowest_tracker = args.log_slowest.map(SlowestTracker::new);
	let results: Vec<TestResult> = work_items
		.into_par_iter()
		.flat_map_iter(|item| {
			let start = slowest_tracker.as_ref().map(|_| Instant::now());

			let eval_result = common::eval_jsonnet_snippet(&item.snippet, &import_paths);

			if let (Some(tracker), Some(start)) = (&slowest_tracker, start) {
				let mut validation_file_names: Vec<String> = item
					.result_descriptors
					.iter()
					.map(|d| d.validation_file_name.clone())
					.collect::<HashSet<_>>()
					.into_iter()
					.collect();
				validation_file_names.sort();
				tracker.record(SlowEntry {
					duration: start.elapsed(),
					test_kind: "namespace",
					subject: item.subject.clone(),
					validation_file_names,
				});
			}

			let descriptors = item.result_descriptors;
			match eval_result {
				Ok(serde_json::Value::Array(arr)) => arr
					.into_iter()
					.zip(descriptors.iter())
					.map(|(result_value, desc)| {
						let error = match result_value {
							serde_json::Value::Null => None,
							serde_json::Value::String(s) => Some(s),
							other => Some(format!("unexpected return type: {}", other)),
						};
						TestResult {
							test_file: desc.validation_file_name.clone(),
							subject: desc.subject.clone(),
							test_kind: desc.test_kind,
							error,
						}
					})
					.collect::<Vec<_>>(),
				Ok(other) => descriptors
					.into_iter()
					.map(|desc| TestResult {
						test_file: desc.validation_file_name,
						subject: desc.subject,
						test_kind: desc.test_kind,
						error: Some(format!("unexpected result type: {}", other)),
					})
					.collect(),
				Err(e) => descriptors
					.into_iter()
					.map(|desc| TestResult {
						test_file: desc.validation_file_name,
						subject: desc.subject,
						test_kind: desc.test_kind,
						error: Some(format!("evaluation error: {}", e)),
					})
					.collect(),
			}
		})
		.collect();

	// Step 5: Report results
	let passed = results.iter().filter(|r| r.error.is_none()).count();
	let failed = results.iter().filter(|r| r.error.is_some()).count();

	debug!(
		passed,
		failed,
		total = results.len(),
		"test execution complete"
	);

	for result in &results {
		if let Some(ref error) = result.error {
			writeln!(
				writer,
				"FAIL  {} | {} [{}]: {}",
				result.test_file, result.subject, result.test_kind, error
			)?;
		}
	}

	if failed > 0 {
		writeln!(writer)?;
	}

	writeln!(
		writer,
		"Results: {} passed, {} failed, {} total",
		passed,
		failed,
		results.len()
	)?;

	// Log slowest work items if requested
	if let Some(tracker) = slowest_tracker {
		let entries = tracker.into_sorted();
		if !entries.is_empty() {
			writeln!(writer)?;
			writeln!(writer, "Slowest work items:")?;
			for entry in &entries {
				writeln!(
					writer,
					"  {:>8.1?}  [{}] {} for {}",
					entry.duration,
					entry.test_kind,
					entry.validation_file_names.join(", "),
					entry.subject,
				)?;
			}
		}
	}

	if failed > 0 {
		anyhow::bail!("{} test(s) failed", failed);
	}

	Ok(())
}

/// Collect all YAML manifest files from the export directory.
fn collect_manifests(export_dir: &Path, recursive: bool) -> Result<Vec<ParsedManifest>> {
	let mut manifests = Vec::new();

	let walker = if recursive {
		WalkDir::new(export_dir)
	} else {
		WalkDir::new(export_dir).max_depth(1)
	};

	for entry in walker
		.into_iter()
		.filter_map(|e| e.ok())
		.filter(|e| e.file_type().is_file())
	{
		let path = entry.path();
		let ext = path.extension().and_then(|e| e.to_str());

		match ext {
			Some("yaml") | Some("yml") | Some("json") => {}
			_ => continue,
		}

		// Skip manifest.json tracking file
		if path.file_name().and_then(|n| n.to_str()) == Some("manifest.json") {
			continue;
		}

		let content = fs::read_to_string(path)
			.with_context(|| format!("reading manifest file {}", path.display()))?;

		let relative = path
			.strip_prefix(export_dir)
			.unwrap_or(path)
			.to_string_lossy()
			.to_string();

		// Parse YAML documents (a file may contain multiple documents separated by ---)
		let values = parse_yaml_manifests(&content, &relative)?;
		debug!(file = %relative, document_count = values.len(), "parsed manifest file");

		for value in values {
			let kind = value
				.get("kind")
				.and_then(|v| v.as_str())
				.unwrap_or("")
				.to_string();

			let namespace = value
				.pointer("/metadata/namespace")
				.and_then(|v| v.as_str())
				.unwrap_or("")
				.to_string();

			manifests.push(ParsedManifest {
				source_file: relative.clone(),
				kind,
				namespace,
				value,
			});
		}
	}

	Ok(manifests)
}

/// Returns true if this line is a YAML document start marker ("---" at line start, optionally followed by whitespace).
fn is_document_boundary_line(line: &str) -> bool {
	let trimmed = line.trim_start();
	trimmed == "---"
		|| (trimmed.starts_with("---") && trimmed[3..].chars().all(|c| c.is_whitespace()))
}

/// Parser options for manifest YAML. Use the same permissive options as helm/kustomize
/// parsing so CRDs and other Kubernetes YAML (valid per Go yaml / kubectl) parse correctly.
fn manifest_yaml_options() -> serde_saphyr::Options {
	serde_saphyr::Options {
		legacy_octal_numbers: true,
		budget: None,
		..Default::default()
	}
}

/// Parse YAML content into JSON values, handling multi-document YAML.
/// Splits only on document boundaries (line starting with "---"), not on "---" inside scalars.
/// Uses Options so valid Kubernetes YAML (e.g. CRDs with colons in block scalars) parses like kubectl/Go yaml.
fn parse_yaml_manifests(content: &str, file_path: &str) -> Result<Vec<serde_json::Value>> {
	let options = manifest_yaml_options();
	let mut results = Vec::new();
	let mut doc_lines: Vec<&str> = Vec::new();

	for line in content.split_inclusive('\n') {
		if is_document_boundary_line(line.trim_end()) {
			if !doc_lines.is_empty() {
				let doc_content = doc_lines.join("");
				let trimmed = doc_content.trim();
				let stripped = trimmed.strip_prefix("---").unwrap_or(trimmed).trim();
				if !stripped.is_empty() {
					let value: serde_json::Value =
						serde_saphyr::from_str_with_options(stripped, options.clone())
							.with_context(|| format!("parsing YAML in {}", file_path))?;
					if !value.is_null() {
						results.push(value);
					}
				}
				doc_lines.clear();
			}
			doc_lines.push(line);
		} else {
			doc_lines.push(line);
		}
	}

	if !doc_lines.is_empty() {
		let doc_content = doc_lines.join("");
		let trimmed = doc_content.trim();
		let stripped = trimmed.strip_prefix("---").unwrap_or(trimmed).trim();
		if !stripped.is_empty() {
			let value: serde_json::Value =
				serde_saphyr::from_str_with_options(stripped, options)
					.with_context(|| format!("parsing YAML in {}", file_path))?;
			if !value.is_null() {
				results.push(value);
			}
		}
	}

	Ok(results)
}

/// Extract the optional `kinds` set from a validation file.
///
/// If the file defines a `kinds` field (array of strings), returns the set.
/// If `kinds` is not defined, returns None (all kinds match).
fn extract_kinds_filter(
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

	let value = common::eval_jsonnet_snippet(&snippet, import_paths)?;

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

/// Group manifests by namespace.
fn group_by_namespace(manifests: &[ParsedManifest]) -> BTreeMap<String, Vec<&ParsedManifest>> {
	let mut grouped: BTreeMap<String, Vec<&ParsedManifest>> = BTreeMap::new();
	for manifest in manifests {
		grouped
			.entry(manifest.namespace.clone())
			.or_default()
			.push(manifest);
	}
	grouped
}
