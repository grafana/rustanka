//! Validate manifests command handler.
//!
//! Validates exported manifests against Jsonnet validation files.
//!
//! Validation files are `<name>.jsonnet` files (excluding `*_test.jsonnet`) that define
//! one or both of:
//! - `namespaceTest(manifests)` - receives all manifests in a namespace, returns null on success or an error string
//! - `manifestTest(manifest)` - receives a single manifest, returns null on success or an error string

use std::{
	collections::{BTreeMap, BinaryHeap, HashMap, HashSet},
	fs,
	io::Write,
	path::{Path, PathBuf},
	sync::{Arc, Mutex},
	time::{Duration, Instant},
};

use anyhow::{Context, Result};
use clap::Args;
use rayon::iter::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator};
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

	let t_start = Instant::now();

	if !export_dir.is_dir() {
		anyhow::bail!("export directory does not exist: {}", export_dir.display());
	}
	if !tests_dir.is_dir() {
		anyhow::bail!("tests directory does not exist: {}", tests_dir.display());
	}

	// Step 1: Collect and parse all manifests from the export directory
	let t_collect = Instant::now();
	let manifests = collect_manifests(&export_dir, args.recursive)?;
	debug!(
		elapsed_ms = t_collect.elapsed().as_millis() as u64,
		"collect_manifests"
	);
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
		namespaces = ?by_namespace
			.keys()
			.map(|k| format!("{}/{}", k.parent, k.namespace))
			.collect::<Vec<_>>(),
		"grouped manifests by namespace"
	);

	// Step 4: Pre-process validation files (extract kinds, resolve paths)
	let t_preprocess = Instant::now();
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
			common::extract_kinds_filter(validation_file, &import_paths)?
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

	debug!(
		elapsed_ms = t_preprocess.elapsed().as_millis() as u64,
		"pre-process validations"
	);

	// Step 5: Build one work item per namespace — one eval runs namespaceTest(allManifests) and all applicable manifest tests
	let t_build = Instant::now();
	let ns_validations: Vec<&ValidationFileInfo> = validation_infos
		.iter()
		.filter(|v| v.has_namespace_test)
		.collect();
	let manifest_validations: Vec<&ValidationFileInfo> = validation_infos
		.iter()
		.filter(|v| v.has_manifest_test)
		.collect();

	// Serialize per-namespace manifests as JSON and stash them in an in-memory
	// resolver. Each work item imports a virtual path like
	// `/__rtk_validate__/ns_<idx>.json`; jrsonnet's per-State file cache (with the
	// thread-local pooled State) parses each namespace's manifests at most once
	// per worker thread, even when many work items reference it. This avoids the
	// disk write/read round-trip the previous on-disk implementation needed.
	let by_namespace_vec: Vec<(&NamespaceGroupKey, &Vec<&ParsedManifest>)> =
		by_namespace.iter().collect();
	let ns_entries: Vec<(PathBuf, Vec<u8>)> = (0..by_namespace_vec.len())
		.into_par_iter()
		.map(|ns_idx| -> Result<(PathBuf, Vec<u8>)> {
			let (_, ns_manifests) = by_namespace_vec[ns_idx];
			let manifests_json: Vec<&serde_json::Value> =
				ns_manifests.iter().map(|m| &m.value).collect();
			let json_bytes = serde_json::to_vec(&manifests_json)?;
			let virtual_path = PathBuf::from(format!("/__rtk_validate__/ns_{}.json", ns_idx));
			Ok((virtual_path, json_bytes))
		})
		.collect::<Result<Vec<_>>>()?;
	let ns_file_paths: Vec<String> = ns_entries
		.iter()
		.map(|(p, _)| p.to_string_lossy().replace('\\', "/"))
		.collect();
	let mut memory_map: HashMap<PathBuf, Vec<u8>> = HashMap::with_capacity(ns_entries.len());
	for (path, bytes) in ns_entries {
		memory_map.insert(path, bytes);
	}
	let memory: common::MemoryFiles = Arc::new(memory_map);

	let mut work_items: Vec<BatchWorkItem> = Vec::new();
	for (ns_idx, (key, ns_manifests)) in by_namespace_vec.iter().enumerate() {
		let ns_display = if key.namespace.is_empty() {
			"(cluster-scoped)".to_string()
		} else {
			key.namespace.clone()
		};
		// `namespace` is the empty-string sentinel used by validations to skip
		// namespaceTest for cluster-scoped resources.
		let namespace = &key.namespace;
		let ns_file_str = &ns_file_paths[ns_idx];

		// Heuristic: count the number of manifestTest calls this namespace would emit
		// in a single batched work item. If that exceeds `split_threshold`, split the
		// namespace into per-validation work items so rayon can fan the work out.
		// Small namespaces stay batched because the per-eval setup cost would dominate.
		//
		// The exact number isn't critical; smaller values add more per-eval overhead
		// while larger values leave longer serial tails.
		const SPLIT_THRESHOLD: usize = 512;
		let manifest_test_calls: usize = ns_manifests
			.iter()
			.flat_map(|m| {
				manifest_validations
					.iter()
					.filter(|v| match &v.kinds_filter {
						Some(kinds) => kinds.contains(&m.kind),
						None => true,
					})
			})
			.count();

		if manifest_test_calls <= SPLIT_THRESHOLD {
			// Build snippet with hoisted function references. Each validation
			// file's `manifestTest` / `namespaceTest` is bound once (via
			// `local`), then call sites in the result array reference the
			// local. This avoids re-resolving `(import 'X').yyTest` at every
			// call site, which matters in batched namespaces that emit
			// hundreds of explicit call sites.
			let mut head = format!("local ms = import '{}';\n", ns_file_str);
			let mut body = String::from("[\n");
			let mut result_descriptors: Vec<ResultDescriptor> = Vec::new();
			// Maps abs_import_path -> (ns_local_name, mft_local_name). Each
			// validation file gets at most one local per kind of test it
			// defines, regardless of how many call sites use it.
			let mut local_names: std::collections::HashMap<
				String,
				(Option<String>, Option<String>),
			> = std::collections::HashMap::new();
			let mut next_idx = 0usize;
			fn ensure_local(
				head: &mut String,
				local_names: &mut std::collections::HashMap<
					String,
					(Option<String>, Option<String>),
				>,
				next_idx: &mut usize,
				abs_import_path: &str,
				kind: &'static str,
			) -> String {
				let entry = local_names.entry(abs_import_path.to_string()).or_default();
				let slot = match kind {
					"namespaceTest" => &mut entry.0,
					"manifestTest" => &mut entry.1,
					_ => unreachable!(),
				};
				if let Some(existing) = slot {
					return existing.clone();
				}
				let name = format!("__rtk_{}_{}", *next_idx, kind);
				*next_idx += 1;
				head.push_str(&format!(
					"local {} = (import '{}').{};\n",
					name, abs_import_path, kind
				));
				*slot = Some(name.clone());
				name
			}

			if !namespace.is_empty() {
				for v in &ns_validations {
					let local_name = ensure_local(
						&mut head,
						&mut local_names,
						&mut next_idx,
						&v.abs_import_path,
						"namespaceTest",
					);
					body.push_str(&format!("  {}(ms),\n", local_name));
					result_descriptors.push(ResultDescriptor {
						validation_file_name: v.display_name.clone(),
						subject: ns_display.clone(),
						test_kind: "namespaceTest",
					});
				}
			}

			for (idx, manifest) in ns_manifests.iter().enumerate() {
				for v in &manifest_validations {
					if let Some(kinds) = &v.kinds_filter {
						if !kinds.contains(&manifest.kind) {
							continue;
						}
					}
					let local_name = ensure_local(
						&mut head,
						&mut local_names,
						&mut next_idx,
						&v.abs_import_path,
						"manifestTest",
					);
					body.push_str(&format!("  {}(ms[{}]),\n", local_name, idx));
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

			body.push(']');
			let snippet = head + &body;

			work_items.push(BatchWorkItem {
				snippet,
				subject: ns_display,
				result_descriptors,
			});
		} else {
			// Big namespace: emit one work item per validation file so rayon can spread
			// the work across cores.
			if !namespace.is_empty() {
				for v in &ns_validations {
					let snippet = format!(
						"local ms = import '{}';\n[(import '{}').namespaceTest(ms)]\n",
						ns_file_str, v.abs_import_path
					);
					work_items.push(BatchWorkItem {
						snippet,
						subject: format!("{} / {}", ns_display, v.display_name),
						result_descriptors: vec![ResultDescriptor {
							validation_file_name: v.display_name.clone(),
							subject: ns_display.clone(),
							test_kind: "namespaceTest",
						}],
					});
				}
			}

			for v in &manifest_validations {
				let applicable: Vec<(usize, &ParsedManifest)> = ns_manifests
					.iter()
					.enumerate()
					.filter_map(|(idx, m)| match &v.kinds_filter {
						Some(kinds) if !kinds.contains(&m.kind) => None,
						_ => Some((idx, *m)),
					})
					.collect();
				if applicable.is_empty() {
					continue;
				}

				let mut idx_list = String::with_capacity(applicable.len() * 4 + 2);
				idx_list.push('[');
				for (i, (idx, _)) in applicable.iter().enumerate() {
					if i > 0 {
						idx_list.push(',');
					}
					use std::fmt::Write as _;
					write!(idx_list, "{}", idx).expect("write to string");
				}
				idx_list.push(']');

				let snippet = format!(
					"local ms = import '{}';\nlocal v = import '{}';\n[v.manifestTest(ms[i]) for i in {}]\n",
					ns_file_str, v.abs_import_path, idx_list
				);

				let result_descriptors: Vec<ResultDescriptor> = applicable
					.iter()
					.map(|(_, m)| ResultDescriptor {
						validation_file_name: v.display_name.clone(),
						subject: m.source_file.clone(),
						test_kind: "manifestTest",
					})
					.collect();

				work_items.push(BatchWorkItem {
					snippet,
					subject: format!("{} / {}", ns_display, v.display_name),
					result_descriptors,
				});
			}
		}
	}

	// Sort heaviest items first so rayon picks them up early
	work_items.sort_by(|a, b| b.snippet.len().cmp(&a.snippet.len()));

	debug!(
		work_item_count = work_items.len(),
		"running batched tests in parallel"
	);

	{
		let total_snippet_bytes: usize = work_items.iter().map(|w| w.snippet.len()).sum();
		debug!(
			elapsed_ms = t_build.elapsed().as_millis() as u64,
			work_item_count = work_items.len(),
			total_snippet_bytes,
			"built work items"
		);
	}

	// Execute all batched work items in parallel
	let t_eval = Instant::now();
	let slowest_tracker = args.log_slowest.map(SlowestTracker::new);
	let results: Vec<TestResult> = work_items
		.into_par_iter()
		.flat_map_iter(|item| {
			let start = slowest_tracker.as_ref().map(|_| Instant::now());

			let eval_result = common::eval_jsonnet_snippet_array_pooled(
				&item.snippet,
				&import_paths,
				Some(&memory),
			);

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
				Ok(elements) => {
					// Pair per-element results with descriptors. We expect the
					// arrays to be the same length; if jrsonnet returned fewer
					// elements than we requested we still report what we have
					// and fall through with synthesized failures for the rest.
					let mut out: Vec<TestResult> = Vec::with_capacity(descriptors.len());
					let mut elem_iter = elements.into_iter();
					for desc in descriptors {
						let error = match elem_iter.next() {
							Some(Ok(serde_json::Value::Null)) => None,
							Some(Ok(serde_json::Value::String(s))) => Some(s),
							Some(Ok(other)) => {
								Some(format!("unexpected return type: {}", other))
							}
							Some(Err(msg)) => Some(msg),
							None => Some(
								"missing result element (jrsonnet returned fewer elements than expected)"
									.to_string(),
							),
						};
						out.push(TestResult {
							test_file: desc.validation_file_name,
							subject: desc.subject,
							test_kind: desc.test_kind,
							error,
						});
					}
					out
				}
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

	debug!(
		elapsed_ms = t_eval.elapsed().as_millis() as u64,
		"parallel eval"
	);

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

	debug!(elapsed_ms = t_start.elapsed().as_millis() as u64, "total");

	if failed > 0 {
		anyhow::bail!("{} test(s) failed", failed);
	}

	Ok(())
}

/// Collect all YAML manifest files from the export directory.
///
/// File walking is sequential (cheap), but reading + parsing each file is fanned
/// out to rayon since YAML parsing of large ConfigMaps (dashboards, helm values,
/// etc.) is the dominant cost for sizeable export directories.
fn collect_manifests(export_dir: &Path, recursive: bool) -> Result<Vec<ParsedManifest>> {
	let walker = if recursive {
		WalkDir::new(export_dir)
	} else {
		WalkDir::new(export_dir).max_depth(1)
	};

	let mut paths: Vec<PathBuf> = Vec::new();
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
		if path.file_name().and_then(|n| n.to_str()) == Some("manifest.json") {
			continue;
		}
		paths.push(path.to_path_buf());
	}

	let manifests: Vec<ParsedManifest> = paths
		.par_iter()
		.map(|path| -> Result<Vec<ParsedManifest>> {
			let content = fs::read_to_string(path)
				.with_context(|| format!("reading manifest file {}", path.display()))?;

			let relative = path
				.strip_prefix(export_dir)
				.unwrap_or(path)
				.to_string_lossy()
				.to_string();

			let values = parse_yaml_manifests(&content, &relative)?;
			debug!(file = %relative, document_count = values.len(), "parsed manifest file");

			let mut out = Vec::with_capacity(values.len());
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
				out.push(ParsedManifest {
					source_file: relative.clone(),
					kind,
					namespace,
					value,
				});
			}
			Ok(out)
		})
		.collect::<Result<Vec<Vec<ParsedManifest>>>>()?
		.into_iter()
		.flatten()
		.collect();

	Ok(manifests)
}

/// Returns true if this line is a YAML document start marker.
///
/// Per the YAML 1.2 spec the `---` directive end marker must start at column 0
/// (no indentation). Indented `---` is part of a block scalar or list and must
/// not be treated as a document boundary, otherwise valid manifests like
/// `ConfigMap`s embedding rule files in `data.rules: |` fail to parse.
fn is_document_boundary_line(line: &str) -> bool {
	if !line.starts_with("---") {
		return false;
	}
	line[3..].chars().all(|c| c.is_whitespace())
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

/// Group manifests for `namespaceTest` evaluation.
///
/// Manifests are grouped by the directory containing the manifest file together
/// with the manifest's `metadata.namespace`. This keeps semantically distinct
/// namespaces (e.g. the same `karpenter` namespace exported from many clusters
/// into different subdirectories) in their own group rather than collapsing
/// them into one giant work item.
///
/// The map key is `(parent_directory, namespace)`. The display string returned
/// in the `String` half of the entry is just the namespace name so user-facing
/// output is unchanged for single-cluster exports.
fn group_by_namespace(
	manifests: &[ParsedManifest],
) -> BTreeMap<NamespaceGroupKey, Vec<&ParsedManifest>> {
	let mut grouped: BTreeMap<NamespaceGroupKey, Vec<&ParsedManifest>> = BTreeMap::new();
	for manifest in manifests {
		let parent = std::path::Path::new(&manifest.source_file)
			.parent()
			.map(|p| p.to_string_lossy().to_string())
			.unwrap_or_default();
		let key = NamespaceGroupKey {
			parent,
			namespace: manifest.namespace.clone(),
		};
		grouped.entry(key).or_default().push(manifest);
	}
	grouped
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct NamespaceGroupKey {
	parent: String,
	namespace: String,
}
