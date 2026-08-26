//! Exports of the fixture projects under `testdata`.
//!
//! Where [`export`](super) builds its projects inline, these use fixtures on
//! disk: environments too involved to write out in a test, and the awkward cases
//! found by hand along the way.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rtk_environments::Engine;
use rtk_environments::export::{Error, Exported, MergeStrategy, Options};
use tempfile::TempDir;

/// Namespace and name, which most of these fixtures are exported by.
const BY_NAMESPACE: &str = "{{.metadata.namespace}}/{{.metadata.name}}";

fn testdata(fixture: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("testdata")
		.join(fixture)
}

fn export_error_fixture(fixture: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.unwrap()
		.parent()
		.unwrap()
		.join("test_fixtures/export_error_parity")
		.join(fixture)
}

/// What to export, and how.
///
/// Built by [`fixture`], which fills in what an export needs but a test rarely
/// cares about.
#[derive(Clone)]
struct Plan {
	paths: Vec<PathBuf>,
	options: Options,
	jsonnet: rtk_jsonnet::Options,
}

/// Export the fixture of this name.
fn fixture(name: &str) -> Plan {
	Plan {
		paths: vec![testdata(name)],
		options: Options {
			format: BY_NAMESPACE.to_owned(),
			parallelism: 1,
			..Options::default()
		},
		jsonnet: rtk_jsonnet::Options::default(),
	}
}

impl Plan {
	fn path(mut self, path: PathBuf) -> Plan {
		self.paths = vec![path];
		self
	}

	fn format(mut self, format: &str) -> Plan {
		format.clone_into(&mut self.options.format);
		self
	}

	fn recursive(mut self) -> Plan {
		self.options.recursive = true;
		self
	}

	fn parallelism(mut self, parallelism: usize) -> Plan {
		self.options.parallelism = parallelism;
		self
	}

	fn skip_manifest(mut self) -> Plan {
		self.options.skip_manifest = true;
		self
	}

	fn timing(mut self) -> Plan {
		self.options.timing = true;
		self
	}

	fn merging(mut self, strategy: MergeStrategy) -> Plan {
		self.options.merge_strategy = strategy;
		self
	}

	/// Name an environment as deleted, the way `manifest.json` names it: by its
	/// `metadata.namespace`, which is its entrypoint relative to the project
	/// root. tk's own `--merge-deleted-envs` test passes exactly this spelling.
	fn deleted(mut self, namespace: &str) -> Plan {
		self.options
			.merge_deleted_environments
			.push(namespace.to_owned());
		self
	}

	fn named(mut self, name: &str) -> Plan {
		self.options.name = Some(name.to_owned());
		self
	}

	/// The `deploymentName` and `serviceName` the static fixture is named by.
	fn resources(self, deployment: &str, service: &str) -> Plan {
		self.ext_code("deploymentName", &format!("'{deployment}'"))
			.ext_code("serviceName", &format!("'{service}'"))
	}

	fn ext_code(mut self, key: &str, value: &str) -> Plan {
		self.jsonnet.ext_code.insert(key.into(), value.into());
		self
	}

	fn tla_str(mut self, key: &str, value: &str) -> Plan {
		self.jsonnet
			.top_level_arguments
			.insert(key.into(), value.into());
		self
	}

	fn tla_code(mut self, key: &str, value: &str) -> Plan {
		self.jsonnet.top_level_code.insert(key.into(), value.into());
		self
	}
}

/// A directory exported into, possibly more than once.
struct Output {
	directory: TempDir,
}

impl Output {
	fn new() -> Output {
		Output {
			directory: tempfile::tempdir().expect("a temporary directory"),
		}
	}

	fn path(&self) -> &Path {
		self.directory.path()
	}

	fn export(&self, plan: Plan) -> Result<Exported, Error> {
		let options = Options {
			output_dir: self.path().to_path_buf(),
			..plan.options
		};

		Engine::new(rtk_jsonnet::Engine::new(plan.jsonnet)).export_bulk(plan.paths, &options)
	}

	/// Export, expecting it to work.
	fn exported(&self, plan: Plan) -> Exported {
		let exported = self.export(plan).expect("the export to succeed");
		assert_eq!(
			exported.failed(),
			0,
			"environments failed: {:?}",
			exported
				.reports
				.iter()
				.filter_map(|report| report.error.as_ref().map(|error| error.report()))
				.collect::<Vec<_>>()
		);
		exported
	}

	/// Every file written, relative to the output directory, sorted.
	fn files(&self) -> Vec<String> {
		let mut files: Vec<String> = self.contents().into_keys().collect();
		files.sort();
		files
	}

	/// Every file written, with its contents.
	fn contents(&self) -> BTreeMap<String, String> {
		let mut contents = BTreeMap::new();
		for entry in walkdir::WalkDir::new(self.path())
			.into_iter()
			.filter_map(Result::ok)
			.filter(|entry| entry.file_type().is_file())
		{
			let path = entry
				.path()
				.strip_prefix(self.path())
				.expect("below the output directory")
				.to_string_lossy()
				.into_owned();
			contents.insert(
				path,
				fs::read_to_string(entry.path()).expect("the contents"),
			);
		}
		contents
	}

	fn read(&self, path: &str) -> String {
		fs::read_to_string(self.path().join(path)).unwrap_or_else(|_| panic!("{path} to exist"))
	}

	/// `manifest.json`, mapping each exported file to the environment that wrote it.
	fn index(&self) -> BTreeMap<String, String> {
		serde_json::from_str(&self.read("manifest.json")).expect("a readable index")
	}
}

/// Everything `test-export-envs` exports, by namespace and name.
fn all_environments(deployment: &str, service: &str) -> Vec<String> {
	[
		"inline-namespace1/my-configmap.yaml",
		"inline-namespace1/my-deployment.yaml",
		"inline-namespace1/my-service.yaml",
		"inline-namespace2/my-deployment.yaml",
		"inline-namespace2/my-service.yaml",
		"manifest.json",
	]
	.iter()
	.map(|file| (*file).to_owned())
	.chain([
		format!("static/{deployment}.yaml"),
		format!("static/{service}.yaml"),
	])
	.collect::<std::collections::BTreeSet<_>>()
	.into_iter()
	.collect()
}

#[test]
fn exports_every_environment_in_a_project() {
	let output = Output::new();
	let exported = output.exported(
		fixture("test-export-envs")
			.recursive()
			.parallelism(8)
			.format(
				"{{env.metadata.labels.cluster_name}}/{{env.spec.namespace}}/{{.metadata.name}}",
			)
			.resources("initial-deployment", "initial-service"),
	);

	// One static environment and two declared inline in one file.
	assert_eq!(exported.successful(), 3);
	assert_eq!(
		output.files(),
		[
			"manifest.json",
			"my-cluster/inline-namespace1/my-configmap.yaml",
			"my-cluster/inline-namespace1/my-deployment.yaml",
			"my-cluster/inline-namespace1/my-service.yaml",
			"my-cluster2/inline-namespace2/my-deployment.yaml",
			"my-cluster2/inline-namespace2/my-service.yaml",
			"my-static-cluster/static/initial-deployment.yaml",
			"my-static-cluster/static/initial-service.yaml",
		]
	);

	// The index records which environment wrote each file, by entrypoint.
	let index = output.index();
	assert_eq!(index.len(), 7);
	assert!(
		index["my-cluster/inline-namespace1/my-configmap.yaml"]
			.ends_with("test-export-envs/inline-envs/main.jsonnet")
	);
	assert!(
		index["my-static-cluster/static/initial-deployment.yaml"]
			.ends_with("test-export-envs/static-env/main.jsonnet")
	);

	// Manifests are indented by two spaces, as tk indents them.
	assert!(
		output
			.read("my-static-cluster/static/initial-deployment.yaml")
			.contains("  name: initial-deployment"),
		"the indentation is no longer two spaces"
	);
}

#[test]
fn skipping_the_manifest_writes_no_index() {
	let output = Output::new();
	let exported = output.exported(
		fixture("test-export-envs")
			.recursive()
			.skip_manifest()
			.resources("test-deployment", "test-service"),
	);

	assert_eq!(exported.successful(), 3);

	let mut expected = all_environments("test-deployment", "test-service");
	expected.retain(|file| file != "manifest.json");
	assert_eq!(output.files(), expected);
	assert!(
		!output.path().join("manifest.json").exists(),
		"an index was written even though it was skipped"
	);
}

#[test]
fn refuses_to_export_into_a_directory_that_already_holds_one() {
	let output = Output::new();
	let plan = fixture("test-export-envs")
		.recursive()
		.resources("initial-deployment", "initial-service");

	output.exported(plan.clone());
	assert_eq!(
		output.files(),
		all_environments("initial-deployment", "initial-service")
	);

	// Exporting again over the top of it needs to be asked for.
	let error = output.export(plan).expect_err("the directory is not empty");
	assert!(
		error
			.to_string()
			.contains("not empty. Pass a different --merge-strategy"),
		"unexpected error: {error}"
	);
}

#[test]
fn merging_replaces_the_environments_it_exports_and_leaves_the_rest() {
	let output = Output::new();
	output.exported(
		fixture("test-export-envs")
			.recursive()
			.resources("initial-deployment", "initial-service"),
	);

	// Exporting the static environment on its own replaces its files, and only
	// its files: the inline environments keep theirs.
	let exported = output.exported(
		fixture("test-export-envs/static-env")
			.merging(MergeStrategy::ReplaceEnvironments)
			.resources("updated-deployment", "updated-service"),
	);
	assert_eq!(exported.successful(), 1);
	assert_eq!(
		output.files(),
		all_environments("updated-deployment", "updated-service")
	);
	assert!(
		output
			.read("static/updated-deployment.yaml")
			.contains("updated-deployment")
	);

	// An environment named as deleted has its files cleaned up as well, even
	// though it was not exported.
	let exported = output.exported(
		fixture("test-export-envs/static-env")
			.merging(MergeStrategy::ReplaceEnvironments)
			.deleted("test-export-envs/inline-envs/main.jsonnet")
			.resources("updated-again-deployment", "updated-again-service"),
	);
	assert_eq!(exported.successful(), 1);
	assert_eq!(
		output.files(),
		[
			"manifest.json",
			"static/updated-again-deployment.yaml",
			"static/updated-again-service.yaml",
		]
	);
	assert_eq!(output.index().len(), 2);
}

#[test]
fn re_exporting_unchanged_jsonnet_changes_nothing() {
	let output = Output::new();
	let plan = fixture("test-export-envs")
		.recursive()
		.parallelism(4)
		.resources("stable-deployment", "stable-service");

	output.exported(plan.clone());
	let first = output.contents();
	assert_eq!(first.len(), 8);

	let exported = output.exported(plan.merging(MergeStrategy::ReplaceEnvironments));
	assert_eq!(exported.successful(), 3);
	assert_eq!(
		first,
		output.contents(),
		"re-exporting the same environments changed something"
	);

	// Every file was recognised as already written, so none was written again.
	let unchanged: usize = exported.reports.iter().map(|report| report.unchanged).sum();
	assert_eq!(unchanged, 7);
}

#[test]
fn replacing_an_environment_deletes_what_it_no_longer_exports() {
	let output = Output::new();
	output.exported(fixture("test-export-envs/static-env").resources("resource-a", "resource-b"));
	assert_eq!(
		output.files(),
		[
			"manifest.json",
			"static/resource-a.yaml",
			"static/resource-b.yaml"
		]
	);

	output.exported(
		fixture("test-export-envs/static-env")
			.merging(MergeStrategy::ReplaceEnvironments)
			.resources("resource-c", "resource-d"),
	);
	assert_eq!(
		output.files(),
		[
			"manifest.json",
			"static/resource-c.yaml",
			"static/resource-d.yaml"
		],
		"the files the environment stopped exporting are still there"
	);
}

#[test]
fn refuses_to_overwrite_a_file_another_environment_wrote() {
	for strategy in [
		MergeStrategy::FailOnConflicts,
		MergeStrategy::ReplaceEnvironments,
	] {
		let output = Output::new();
		output.exported(fixture("test-export-conflict/env1"));

		// Replacing environments releases the files of the environments being
		// exported, which does not include this one.
		let error = output
			.export(fixture("test-export-conflict/env2").merging(strategy))
			.expect_err("the file belongs to another environment");
		assert!(
			error.to_string().contains("already exists"),
			"unexpected error for {strategy:?}: {error}"
		);
	}
}

#[test]
fn refuses_two_resources_of_one_environment_that_share_a_filename() {
	let output = Output::new();
	let error = output
		.export(fixture("test-export-conflict/env-duplicate"))
		.expect_err("two resources want the same file");

	assert!(
		error.to_string().contains("written by multiple"),
		"unexpected error: {error}"
	);
}

#[test]
fn releasing_an_environment_lets_another_take_over_its_files() {
	let output = Output::new();
	output.exported(fixture("test-export-conflict/env1"));

	let file = "default/test-deployment.yaml";
	let before = output.read(file);
	assert!(
		output
			.index()
			.values()
			.any(|owner| owner.contains("test-export-conflict/env1"))
	);

	// Naming the first environment as deleted hands its file over, rather than
	// refusing the second environment for wanting it.
	let exported = output.exported(
		fixture("test-export-conflict/env2")
			.merging(MergeStrategy::ReplaceEnvironments)
			.deleted("test-export-conflict/env1"),
	);
	assert_eq!(exported.successful(), 1);
	assert_eq!(
		before,
		output.read(file),
		"the file itself should not change"
	);

	let index = output.index();
	assert!(
		index
			.values()
			.any(|owner| owner.contains("test-export-conflict/env2"))
	);
	assert!(
		!index
			.values()
			.any(|owner| owner.contains("test-export-conflict/env1")),
		"the environment that gave the file up still owns it"
	);
}

#[test]
fn refuses_a_kubernetes_object_with_no_api_version() {
	let output = Output::new();
	let exported = output
		.export(fixture("test-export-invalid-k8s-object"))
		.expect("the export itself to run");

	assert_eq!(exported.failed(), 1);
	let error = exported.reports[0]
		.error
		.as_ref()
		.expect("the environment to fail");
	assert!(
		error.report().contains("apiVersion"),
		"unexpected error: {}",
		error.report()
	);
}

#[test]
fn refuses_a_kubernetes_object_without_metadata() {
	let output = Output::new();
	let exported = output
		.export(fixture("unused").path(export_error_fixture("metadata_missing")))
		.expect("the export reports an environment failure");

	assert_eq!(exported.failed(), 1);
	let error = exported.reports[0].error.as_ref().unwrap().to_string();
	assert!(
		error.contains("metadata: missing or not an object"),
		"unexpected error: {error}"
	);
	assert!(
		error.contains("metadata.name: missing or not of string type"),
		"unexpected error: {error}"
	);
	assert!(exported.reports[0].files.is_empty());
}

#[test]
fn finds_nothing_in_a_file_that_declares_no_environments() {
	let output = Output::new();
	let exported = output.exported(fixture("test-export-empty-inline-env"));

	assert!(exported.reports.is_empty(), "{:?}", exported.reports);
	assert_eq!(output.files(), Vec::<String>::new());
}

#[test]
fn takes_a_path_to_an_environment_or_to_its_entrypoint() {
	for path in [
		testdata("test-export-envs/static-env"),
		testdata("test-export-envs/static-env/main.jsonnet"),
	] {
		let output = Output::new();
		let exported = output.exported(
			fixture("unused")
				.path(path.clone())
				.resources("deployment", "service"),
		);

		assert_eq!(exported.successful(), 1, "for {}", path.display());
		assert_eq!(
			output.files(),
			[
				"manifest.json",
				"static/deployment.yaml",
				"static/service.yaml"
			],
			"for {}",
			path.display()
		);

		// Either way the index names the entrypoint, not what was asked for.
		for owner in output.index().values() {
			assert!(
				owner.ends_with("test-export-envs/static-env/main.jsonnet"),
				"unexpected owner: {owner}"
			);
		}
	}
}

/// Naming a file names the entrypoint, whatever it is called. tk reads the
/// same thing out of the path in `jpath.Filename`, and evaluates that file.
#[test]
fn takes_a_path_to_an_entrypoint_of_another_name() {
	let output = Output::new();
	let exported = output.exported(
		fixture("test-export-custom-entrypoint/custom.jsonnet").format("{{.metadata.name}}"),
	);

	assert_eq!(exported.successful(), 1);
	assert_eq!(output.files(), ["from-custom.yaml", "manifest.json"]);

	for owner in output.index().values() {
		assert!(
			owner.ends_with("test-export-custom-entrypoint/custom.jsonnet"),
			"the index should name the entrypoint that was exported: {owner}"
		);
	}
}

/// Walking finds environments by their default entrypoint and no other, as
/// tk's `FindFiles` does, so a custom one is reachable only by naming it.
#[test]
fn walking_does_not_find_an_entrypoint_of_another_name() {
	let output = Output::new();
	let exported = output.exported(
		fixture("test-export-custom-entrypoint")
			.recursive()
			.format("{{.metadata.name}}"),
	);

	assert_eq!(exported.successful(), 1);
	assert_eq!(output.files(), ["from-main.yaml", "manifest.json"]);
}

#[test]
fn selects_one_environment_by_its_exact_name() {
	// An inline environment is named by the Jsonnet declaring it.
	let output = Output::new();
	let exported = output.exported(
		fixture("test-export-envs")
			.recursive()
			.named("inline-namespace1")
			.format("{{env.spec.namespace}}/{{.metadata.name}}"),
	);
	assert_eq!(exported.successful(), 1);
	assert_eq!(
		output.files(),
		[
			"inline-namespace1/my-configmap.yaml",
			"inline-namespace1/my-deployment.yaml",
			"inline-namespace1/my-service.yaml",
			"manifest.json",
		]
	);

	// A static one is named after where it lives, relative to the project root,
	// and has to be named in full like any other.
	let output = Output::new();
	let exported = output.exported(
		fixture("test-export-envs")
			.recursive()
			.named("test-export-envs/static-env")
			.resources("path-filter-deployment", "path-filter-service"),
	);
	assert_eq!(exported.successful(), 1);
	assert_eq!(
		output.files(),
		[
			"manifest.json",
			"static/path-filter-deployment.yaml",
			"static/path-filter-service.yaml",
		]
	);
}

/// A recursive `--name` is an exact comparison in tk, so part of a name selects
/// nothing at all rather than everything it appears in.
#[test]
fn part_of_a_name_selects_nothing_recursively() {
	let output = Output::new();
	let exported = output.exported(fixture("test-export-envs").recursive().named("static-env"));

	assert_eq!(exported.successful(), 0);
	assert_eq!(output.files(), Vec::<String>::new());
}

/// The name is compared against the environment, never against where the
/// repository happens to be checked out.
#[test]
fn a_name_is_not_matched_against_the_path() {
	let output = Output::new();
	let exported = output.exported(fixture("test-export-envs").recursive().named("testdata"));

	assert_eq!(exported.successful(), 0);
	assert_eq!(output.files(), Vec::<String>::new());
}

/// A recursive export filters what it walked over, and tk is content for that
/// to leave nothing: it exports what survived and exits zero.
#[test]
fn a_recursive_export_matching_nothing_is_not_an_error() {
	let output = Output::new();
	let exported = output.exported(fixture("test-export-envs").recursive().named("nonexistent"));

	assert_eq!(exported.successful(), 0);
	assert_eq!(
		output.files(),
		Vec::<String>::new(),
		"an export that matched nothing wrote something"
	);
}

/// Asking for one environment and not finding it is a different matter.
#[test]
fn says_so_when_nothing_matches_what_was_asked_for() {
	let output = Output::new();
	let error = output
		.export(fixture("test-export-envs/inline-envs").named("nonexistent"))
		.expect_err("nothing matches");

	assert!(
		error.to_string().contains("no environments matched"),
		"unexpected error: {error}"
	);
	assert_eq!(
		output.files(),
		Vec::<String>::new(),
		"an export that matched nothing wrote something"
	);
}

#[test]
fn calls_an_entrypoint_that_takes_arguments_it_has_defaults_for() {
	let output = Output::new();
	let exported = output.exported(fixture("test-export-tla-defaults"));

	assert_eq!(exported.successful(), 1);
	assert_eq!(
		output.files(),
		[
			"manifest.json",
			"tla-test/tla-defaults-config.yaml",
			"tla-test/tla-defaults-deployment.yaml",
		]
	);
	assert!(
		output
			.read("tla-test/tla-defaults-config.yaml")
			.contains("mode: default"),
		"the defaults were not used"
	);
}

#[test]
fn passes_top_level_arguments_that_override_those_defaults() {
	let output = Output::new();
	let exported = output.exported(
		fixture("test-export-tla-defaults")
			.tla_str("mode", "production")
			.tla_code("replicas", "3"),
	);

	assert_eq!(exported.successful(), 1);
	assert!(
		output
			.read("tla-test/tla-defaults-config.yaml")
			.contains("mode: production")
	);
	assert!(
		output
			.read("tla-test/tla-defaults-deployment.yaml")
			.contains("replicas: 3")
	);
}

#[test]
fn exports_the_same_thing_however_many_environments_run_at_once() {
	let mut exports = Vec::new();

	for parallelism in [1, 8, 16] {
		let output = Output::new();
		let exported = output.exported(
			fixture("test-export-envs")
				.recursive()
				.parallelism(parallelism)
				.skip_manifest()
				.resources("parallel-deployment", "parallel-service"),
		);

		assert_eq!(exported.successful(), 3, "at parallelism {parallelism}");
		exports.push(output.contents());
	}

	assert_eq!(exports[0], exports[1]);
	assert_eq!(exports[1], exports[2]);
	assert!(
		exports[0]
			.values()
			.all(|manifest| manifest.contains("apiVersion:")),
		"something other than a manifest was written"
	);
}

#[test]
fn collects_timing_only_when_it_is_asked_for() {
	let output = Output::new();
	let exported = output.exported(
		fixture("test-export-envs")
			.recursive()
			.parallelism(4)
			.timing()
			.resources("timed-deployment", "timed-service"),
	);

	for report in &exported.reports {
		let timing = report.timing.expect("timing to be collected");
		assert_eq!(
			timing.manifests,
			report.files.len(),
			"{} manifests for {} files",
			timing.manifests,
			report.files.len()
		);
	}

	let output = Output::new();
	let exported = output.exported(
		fixture("test-export-envs")
			.recursive()
			.parallelism(4)
			.resources("untimed-deployment", "untimed-service"),
	);

	assert!(
		exported
			.reports
			.iter()
			.all(|report| report.timing.is_none()),
		"timing was collected without being asked for"
	);
}

/// A minimal case for `no such field: used`.
///
/// An assertion reading `self.used` from a merged object used to fail, the field
/// having gone missing on the way through.
#[test]
fn reads_a_field_of_an_object_merged_into_another() {
	let output = Output::new();
	let exported = output.exported(
		fixture("test-export-used-field-regression")
			.format(rtk_environments::export::DEFAULT_FORMAT),
	);

	assert_eq!(exported.successful(), 1);
	assert_eq!(
		output.files(),
		["manifest.json", "v1.ConfigMap-used-field-regression.yaml"]
	);
}
