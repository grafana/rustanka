//! End-to-end exports, from a directory of Jsonnet to a directory of YAML.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rtk_environments::export::{Error as ExportError, Exported, MergeStrategy, Options};
use rtk_environments::{Engine, Search};
use tempfile::TempDir;

/// A project to export, written out to a temporary directory.
struct Project {
	directory: TempDir,
}

impl Project {
	fn new() -> Project {
		let directory = tempfile::tempdir().expect("a temporary directory");
		fs::write(directory.path().join("jsonnetfile.json"), "{}").expect("a project marker");
		Project { directory }
	}

	fn path(&self) -> &Path {
		self.directory.path()
	}

	/// Write a file, creating its parents.
	fn write(&self, path: &str, contents: &str) -> &Project {
		let path = self.path().join(path);
		fs::create_dir_all(path.parent().expect("a parent")).expect("the parent directory");
		fs::write(path, contents).expect("the file");
		self
	}

	fn output(&self) -> PathBuf {
		self.path().join("out")
	}

	/// Every exported file, relative to the output directory, with its contents.
	fn exported(&self) -> BTreeMap<String, String> {
		fn collect(root: &Path, directory: &Path, exported: &mut BTreeMap<String, String>) {
			let Ok(entries) = fs::read_dir(directory) else {
				return;
			};
			for entry in entries.flatten() {
				let path = entry.path();
				if path.is_dir() {
					collect(root, &path, exported);
					continue;
				}
				let relative = path
					.strip_prefix(root)
					.expect("below the output directory")
					.to_string_lossy()
					.into_owned();
				exported.insert(relative, fs::read_to_string(&path).expect("the contents"));
			}
		}

		let mut exported = BTreeMap::new();
		collect(&self.output(), &self.output(), &mut exported);
		exported
	}
}

fn engine() -> Engine {
	Engine::new(rtk_jsonnet::Engine::new(rtk_jsonnet::Options::default()))
}

fn options(project: &Project) -> Options {
	Options {
		output_dir: project.output(),
		..Options::default()
	}
}

/// A static environment: `spec.json` plus an entrypoint that evaluates to
/// manifests.
fn static_environment(project: &Project, name: &str, spec: &str, main: &str) {
	project.write(&format!("{name}/spec.json"), spec);
	project.write(&format!("{name}/main.jsonnet"), main);
}

const CONFIG_MAP: &str = r"{
	apiVersion: 'v1',
	kind: 'ConfigMap',
	metadata: { name: 'settings' },
	data: { key: 'value' },
}";

#[test]
fn exports_a_static_environment() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{"namespace":"demo"}}"#,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the export succeeds");

	assert_eq!(exported.successful(), 1);
	assert_eq!(exported.failed(), 0);

	let files = project.exported();
	assert_eq!(
		files.keys().collect::<Vec<_>>(),
		vec!["manifest.json", "v1.ConfigMap-settings.yaml"]
	);
	assert_eq!(
		files["v1.ConfigMap-settings.yaml"],
		"apiVersion: v1\ndata:\n  key: value\nkind: ConfigMap\nmetadata:\n  name: settings\n  \
		 namespace: demo\n"
	);

	// The index maps the file back to the environment that produced it.
	let index: BTreeMap<String, String> =
		serde_json::from_str(&files["manifest.json"]).expect("a valid index");
	assert_eq!(index.len(), 1);
	let (file, environment) = index.iter().next().expect("one entry");
	assert_eq!(file, "v1.ConfigMap-settings.yaml");
	assert!(
		environment.ends_with("environments/demo/main.jsonnet"),
		"unexpected environment: {environment}"
	);
}

#[test]
fn loads_processed_manifests_without_exporting() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{"namespace":"demo"}}"#,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);
	let engine = engine();
	let loaded = engine
		.load_single(&project.path().join("environments/demo"), None)
		.expect("the environment loads");
	let manifests = engine
		.manifests(&loaded, &[])
		.expect("the manifests process");

	assert!(loaded.spec().is_some());
	assert_eq!(manifests.len(), 1);
	assert_eq!(manifests[0]["metadata"]["namespace"], "demo");
	assert_eq!(
		rtk_environments::export::serialize_manifest(&manifests[0])
			.expect("the manifest serializes"),
		"apiVersion: v1\ndata:\n  key: value\nkind: ConfigMap\nmetadata:\n  name: settings\n  namespace: demo\n"
	);
	assert!(!project.output().exists());
}

/// tk's `Load` refuses a nested `Environment` once the manifests are processed,
/// so `show`, `diff` and `apply` all reject one — while `export` keeps it. Both
/// halves matter: rtk showed the rest and dropped the Environment in silence.
#[test]
fn a_nested_environment_is_refused_for_everything_but_export() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{"namespace":"demo"}}"#,
		&format!(
			r"{{
				config: {CONFIG_MAP},
				nested: {{
					apiVersion: 'tanka.dev/v1alpha1',
					kind: 'Environment',
					metadata: {{ name: 'inner' }},
					spec: {{ namespace: 'inner-ns' }},
					data: {{}},
				}},
			}}"
		),
	);
	let engine = engine();
	let loaded = engine
		.load_single(&project.path().join("environments/demo"), None)
		.expect("the environment loads");

	let error = engine
		.manifests(&loaded, &[])
		.expect_err("the manifests path refuses it");
	assert_eq!(
		error.to_string(),
		"found a tanka Environment resource. Check that you aren't using a spec.json and inline environments simultaneously"
	);

	// Exporting the same environment keeps both objects, as tk does.
	let exported = engine
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the export succeeds");
	assert_eq!(exported.successful(), 1);
	assert_eq!(
		project
			.exported()
			.keys()
			.filter(|name| name.ends_with(".yaml"))
			.count(),
		2
	);
}

#[test]
fn loads_a_bare_jsonnet_entrypoint() {
	let project = Project::new();
	project.write("bare/main.jsonnet", &format!("{{ config: {CONFIG_MAP} }}"));
	let engine = engine();
	let loaded = engine
		.load_single(&project.path().join("bare/main.jsonnet"), None)
		.expect("the entrypoint loads");
	let manifests = engine
		.manifests(&loaded, &["ConfigMap/settings".to_owned()])
		.expect("the manifests process");

	assert!(loaded.spec().is_none());
	assert_eq!(manifests.len(), 1);
	assert_eq!(
		manifests[0]["metadata"],
		serde_json::json!({"name": "settings"})
	);
}

#[test]
fn defaults_the_namespace_the_way_tanka_does() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);

	engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the export succeeds");

	assert!(
		project.exported()["v1.ConfigMap-settings.yaml"].contains("namespace: default"),
		"a spec without a namespace still namespaces its resources"
	);
}

#[test]
fn leaves_cluster_wide_resources_alone() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{"namespace":"demo"}}"#,
		r"{
			namespace: { apiVersion: 'v1', kind: 'Namespace', metadata: { name: 'demo' } },
			role: {
				apiVersion: 'rbac.authorization.k8s.io/v1',
				kind: 'ClusterRole',
				metadata: { name: 'reader' },
			},
			opted_in: {
				apiVersion: 'v1',
				kind: 'Namespace',
				metadata: {
					name: 'opted-in',
					annotations: { 'tanka.dev/namespaced': 'true' },
				},
			},
		}",
	);

	engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the export succeeds");

	let files = project.exported();
	assert!(!files["v1.Namespace-demo.yaml"].contains("namespace:"));
	assert!(
		!files["rbac.authorization.k8s.io-v1.ClusterRole-reader.yaml"].contains("namespace: demo")
	);
	// The annotation overrides the kind's default.
	assert!(files["v1.Namespace-opted-in.yaml"].contains("namespace: demo"));
}

#[test]
fn expands_lists() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{"namespace":"demo"}}"#,
		r"{
			list: {
				apiVersion: 'v1',
				kind: 'List',
				'$rtk.dev/originalManifest':: 'user value',
				'$rtk.dev/processedManifest':: 'user value',
				items: [
					{ apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'first' } },
					{ apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'second' } },
				],
			},
		}",
	);

	engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the export succeeds");

	let files = project.exported();
	assert!(files.contains_key("v1.ConfigMap-first.yaml"));
	assert!(files.contains_key("v1.ConfigMap-second.yaml"));
	assert!(!files.keys().any(|file| file.contains("List")));
}

#[test]
fn rejects_kubernetes_objects_without_an_api_version() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		r"{ broken: { kind: 'ConfigMap', metadata: { name: 'oops' } } }",
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the export itself runs");

	let error = exported.reports[0]
		.error
		.as_ref()
		.expect("the environment failed");
	assert_eq!(
		error.to_string(),
		r#"found invalid Kubernetes object (at .broken): missing attribute "apiVersion""#
	);
}

#[test]
fn exports_inline_environments_recursively() {
	let project = Project::new();
	project.write(
		"environments/inline/main.jsonnet",
		r"{
			dev: {
				apiVersion: 'tanka.dev/v1alpha1',
				kind: 'Environment',
				metadata: { name: 'dev' },
				spec:
					if std.objectHas(self.data.config, 'metadata')
					then { namespace: 'dev' }
					else { namespace: 'wrong' },
				data: {
					config: {
						apiVersion: 'v1',
						kind: 'ConfigMap',
						metadata: { name: 'settings' },
					},
				},
			},
			prod: {
				apiVersion: 'tanka.dev/v1alpha1',
				kind: 'Environment',
				metadata: { name: 'prod' },
				spec: { namespace: 'prod' },
				data: {
					config: {
						apiVersion: 'v1',
						kind: 'ConfigMap',
						metadata: { name: 'settings' },
					},
				},
			},
		}",
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/inline")],
			&Options {
				recursive: true,
				// The two environments produce the same resource, so they need
				// separate directories, as tk's docs suggest.
				format: "{{env.spec.namespace}}/{{.kind}}-{{.metadata.name}}".to_owned(),
				..options(&project)
			},
		)
		.expect("the export succeeds");

	assert_eq!(exported.successful(), 2);
	let files = project.exported();
	assert!(files.contains_key("dev/ConfigMap-settings.yaml"));
	assert!(files.contains_key("prod/ConfigMap-settings.yaml"));
	assert!(files["dev/ConfigMap-settings.yaml"].contains("namespace: dev"));
	assert!(files["prod/ConfigMap-settings.yaml"].contains("namespace: prod"));
}

#[test]
fn refuses_to_export_an_ambiguous_set_of_environments() {
	let project = Project::new();
	for name in ["one", "two"] {
		static_environment(
			&project,
			&format!("environments/{name}"),
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
			&format!("{{ config: {CONFIG_MAP} }}"),
		);
	}

	let error = engine()
		.export_bulk(
			vec![project.path().join("environments")],
			&options(&project),
		)
		.expect_err("two environments without --recursive is ambiguous");

	assert!(
		error.to_string().starts_with("found 2 environments."),
		"unexpected error: {error}"
	);
	assert!(error.fatal());
	// Nothing was exported before the refusal.
	assert!(project.exported().is_empty());
}

/// Selecting by name is a substring match without `--recursive`, so a name that
/// several environments contain is still ambiguous and still refused. tk says
/// the same thing, from `ErrMultipleEnvs`.
#[test]
fn refuses_a_name_several_inline_environments_share() {
	let project = Project::new();
	let declare = |name: &str| {
		format!(
			r"{{
				apiVersion: 'tanka.dev/v1alpha1',
				kind: 'Environment',
				metadata: {{ name: '{name}' }},
				spec: {{ namespace: '{name}' }},
				data: {{ config: {CONFIG_MAP} }},
			}}"
		)
	};
	// Neither name matches in full, so there is nothing to break the tie.
	project.write(
		"environments/several/main.jsonnet",
		&format!(
			"{{ first: {}, second: {} }}",
			declare("base-one"),
			declare("base-two")
		),
	);

	let error = engine()
		.export_bulk(
			vec![project.path().join("environments/several")],
			&Options {
				name: Some("base".to_owned()),
				..options(&project)
			},
		)
		.expect_err("two environments contain the name");

	let message = error.to_string();
	assert!(
		message.contains("matching \"base\". Provide a more specific name"),
		"unexpected error: {message}"
	);
	assert!(
		message.contains("base-one") && message.contains("base-two"),
		"the error should list what it found: {message}"
	);
	assert!(error.fatal());
	assert!(project.exported().is_empty());
}

/// Among several substring matches, the one that matches in full wins, so
/// `--name base` takes `base` rather than refusing it alongside
/// `base-extended`. tk prefers a full match the same way.
#[test]
fn prefers_the_environment_whose_name_matches_in_full() {
	let project = Project::new();
	let declare = |name: &str, resource: &str| {
		format!(
			r"{{
				apiVersion: 'tanka.dev/v1alpha1',
				kind: 'Environment',
				metadata: {{ name: '{name}' }},
				spec: {{ namespace: '{name}' }},
				data: {{ config: {{
					apiVersion: 'v1',
					kind: 'ConfigMap',
					metadata: {{ name: '{resource}' }},
				}} }},
			}}"
		)
	};
	project.write(
		"environments/several/main.jsonnet",
		&format!(
			"{{ first: {}, second: {} }}",
			declare("base", "exact"),
			declare("base-extended", "substring")
		),
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/several")],
			&Options {
				name: Some("base".to_owned()),
				..options(&project)
			},
		)
		.expect("the full match is unambiguous");

	assert_eq!(exported.successful(), 1);
	assert_eq!(
		project.exported().keys().collect::<Vec<_>>(),
		["manifest.json", "v1.ConfigMap-exact.yaml"]
	);
}

/// A static environment is named after where it lives rather than by the
/// Jsonnet it would be picked out of, and tk's `StaticLoader` takes no notice
/// of `--name` at all.
#[test]
fn a_name_does_not_filter_a_static_environment() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{"namespace":"demo"}}"#,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&Options {
				name: Some("nothing-like-it".to_owned()),
				..options(&project)
			},
		)
		.expect("a static environment ignores the name");

	assert_eq!(exported.successful(), 1);
}

#[test]
fn selects_environments_by_name_and_labels() {
	let project = Project::new();
	for (name, tier) in [("one", "test"), ("two", "prod")] {
		static_environment(
			&project,
			&format!("environments/{name}"),
			&format!(
				r#"{{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{{"labels":{{"tier":"{tier}"}}}},"spec":{{"namespace":"{name}"}}}}"#
			),
			&format!("{{ config: {CONFIG_MAP} }}"),
		);
	}

	// A static environment is named after where it lives, and a recursive
	// `--name` is exact, so it is selected by that name in full.
	let by_name = engine()
		.export_bulk(
			vec![project.path().join("environments")],
			&Options {
				name: Some("environments/one".to_owned()),
				recursive: true,
				..options(&project)
			},
		)
		.expect("the export succeeds");
	assert_eq!(by_name.successful(), 1);
	assert!(project.exported()["v1.ConfigMap-settings.yaml"].contains("namespace: one"));

	let by_selector = engine()
		.export_bulk(
			vec![project.path().join("environments")],
			&Options {
				selector: Some("tier in (prod)".to_owned()),
				output_dir: project.path().join("out-selected"),
				..options(&project)
			},
		)
		.expect("the export succeeds");
	assert_eq!(by_selector.successful(), 1);
	assert!(
		by_selector.reports[0].identifier.contains("two"),
		"the selector picked the wrong environment: {}",
		by_selector.reports[0].identifier
	);
}

#[test]
fn filters_resources_by_target() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		r"{
			config: { apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'settings' } },
			secret: { apiVersion: 'v1', kind: 'Secret', metadata: { name: 'settings' } },
		}",
	);

	engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&Options {
				targets: vec!["configmap/.*".to_owned()],
				..options(&project)
			},
		)
		.expect("the export succeeds");

	let files = project.exported();
	assert!(files.contains_key("v1.ConfigMap-settings.yaml"));
	assert!(!files.contains_key("v1.Secret-settings.yaml"));
}

#[test]
fn target_filtering_does_not_hide_evaluation_failures() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		r"{
			config: { apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'settings' } },
			secret: {
				apiVersion: 'v1',
				kind: 'Secret',
				metadata: { name: 'broken' },
				data: error 'forced before target filtering',
			},
		}",
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&Options {
				targets: vec!["configmap/.*".to_owned()],
				..options(&project)
			},
		)
		.expect("the bulk export reports per-environment failures");

	assert_eq!(exported.failed(), 1);
	assert!(
		exported.reports[0]
			.error
			.as_ref()
			.expect("the evaluation error")
			.to_string()
			.contains("forced before target filtering")
	);
}

#[test]
fn nested_object_assertions_are_forced_before_export() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		r"{
			config: {
				apiVersion: 'v1',
				kind: 'ConfigMap',
				metadata: { name: 'broken' },
				data: { assert false: 'nested assertion forced' },
			},
		}",
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the bulk export reports per-environment failures");
	let error = exported.reports[0]
		.error
		.as_ref()
		.expect("the assertion error")
		.to_string();
	assert!(error.contains("nested assertion forced"), "{error}");
}

/// A container key is never special, whatever it is called. The value has to be
/// something walkable: tk refuses an export that reaches a bare string, so the
/// reserved name is exercised where a name can actually appear.
#[test]
fn reserved_processing_key_in_a_container_is_ordinary_user_data() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		&format!(r#"{{ "$rtk.dev/processedManifest": {CONFIG_MAP} }}"#),
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the export succeeds");

	assert_eq!(exported.successful(), 1);
	assert!(
		project
			.exported()
			.contains_key("v1.ConfigMap-settings.yaml")
	);
}

#[test]
fn evaluation_failure_precedes_invalid_manifest_diagnostics() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		r"{
			broken: {
				apiVersion: 'v1',
				kind: 'ConfigMap',
				data: error 'forced before manifest validation',
			},
		}",
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the bulk export reports per-environment failures");
	let error = exported.reports[0]
		.error
		.as_ref()
		.expect("the evaluation error")
		.to_string();
	assert!(
		error.contains("forced before manifest validation"),
		"{error}"
	);
}

#[test]
fn injects_labels_and_resource_defaults() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{
			"apiVersion": "tanka.dev/v1alpha1",
			"kind": "Environment",
			"metadata": {},
			"spec": {
				"namespace": "demo",
				"injectLabels": true,
				"resourceDefaults": {
					"annotations": { "owner": "platform" },
					"labels": { "managed-by": "rtk" }
				}
			}
		}"#,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);

	engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the export succeeds");

	let exported = &project.exported()["v1.ConfigMap-settings.yaml"];
	assert!(exported.contains("owner: platform"));
	assert!(exported.contains("managed-by: rtk"));
	// The environment label is a 48-character hash of the environment's identity.
	let label = exported
		.lines()
		.find_map(|line| line.trim().strip_prefix("tanka.dev/environment: "))
		.expect("the environment label");
	assert_eq!(label.len(), 48, "unexpected label: {label}");
}

#[test]
fn refuses_a_non_empty_output_directory_unless_merging() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);
	fs::create_dir_all(project.output()).expect("the output directory");
	fs::write(project.output().join("leftover.yaml"), "{}").expect("a leftover file");

	let error = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect_err("the directory is not empty");
	assert!(
		error.to_string().contains("not empty"),
		"unexpected error: {error}"
	);

	// The merge strategies allow it.
	engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&Options {
				merge_strategy: MergeStrategy::FailOnConflicts,
				..options(&project)
			},
		)
		.expect("failing only on conflicts allows a non-empty directory");
	assert!(
		project
			.exported()
			.contains_key("v1.ConfigMap-settings.yaml")
	);
}

#[test]
fn replacing_environments_cleans_up_what_they_no_longer_export() {
	let project = Project::new();
	let spec =
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#;
	static_environment(
		&project,
		"environments/demo",
		spec,
		r"{
			first: { apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'first' } },
			second: { apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'second' } },
		}",
	);

	engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the first export succeeds");
	assert!(project.exported().contains_key("v1.ConfigMap-second.yaml"));

	// The environment stops producing the second resource.
	project.write(
		"environments/demo/main.jsonnet",
		r"{ first: { apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'first' } } }",
	);

	engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&Options {
				merge_strategy: MergeStrategy::ReplaceEnvironments,
				..options(&project)
			},
		)
		.expect("the second export succeeds");

	let files = project.exported();
	assert!(files.contains_key("v1.ConfigMap-first.yaml"));
	assert!(
		!files.contains_key("v1.ConfigMap-second.yaml"),
		"a resource the environment no longer exports should be cleaned up"
	);
	let index: BTreeMap<String, String> =
		serde_json::from_str(&files["manifest.json"]).expect("a valid index");
	assert_eq!(index.len(), 1, "the index should have forgotten it too");
}

#[test]
fn refuses_to_overwrite_another_environments_files() {
	let project = Project::new();
	let spec =
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#;
	for name in ["one", "two"] {
		static_environment(
			&project,
			&format!("environments/{name}"),
			spec,
			&format!("{{ config: {CONFIG_MAP} }}"),
		);
	}

	engine()
		.export_bulk(
			vec![project.path().join("environments/one")],
			&options(&project),
		)
		.expect("the first environment exports");

	let error = engine()
		.export_bulk(
			vec![project.path().join("environments/two")],
			&Options {
				merge_strategy: MergeStrategy::FailOnConflicts,
				..options(&project)
			},
		)
		.expect_err("the second environment wants the same file");

	assert!(
		error
			.to_string()
			.contains("already exists from environment"),
		"unexpected error: {error}"
	);
}

#[test]
fn skips_writing_files_that_have_not_changed() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);

	let first = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the first export succeeds");
	assert_eq!(first.reports[0].unchanged, 0);

	let second = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&Options {
				merge_strategy: MergeStrategy::ReplaceEnvironments,
				..options(&project)
			},
		)
		.expect("the second export succeeds");
	assert_eq!(
		second.reports[0].unchanged, 1,
		"an unchanged file should not be rewritten"
	);
}

#[test]
fn exports_many_environments_while_streaming_discovery() {
	let project = Project::new();
	for index in 0..12 {
		static_environment(
			&project,
			&format!("environments/env{index:02}"),
			&format!(
				r#"{{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{{}},"spec":{{"namespace":"ns{index:02}"}}}}"#
			),
			// Enough manifests per environment to span several chunks.
			r"{
				configs: {
					['config%d' % index]: {
						apiVersion: 'v1',
						kind: 'ConfigMap',
						metadata: { name: 'config%d' % index },
						data: { index: '%d' % index },
					}
					for index in std.range(0, 299)
				},
			}",
		);
	}

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments")],
			&Options {
				recursive: true,
				format: "{{env.spec.namespace}}/{{.kind}}-{{.metadata.name}}".to_owned(),
				parallelism: 4,
				timing: true,
				..options(&project)
			},
		)
		.expect("the export succeeds");

	assert_eq!(exported.successful(), 12);
	assert_eq!(exported.files().count(), 12 * 300);

	// Reports come back in discovery order, whatever order the work finished in.
	// Discovery walks the filesystem, which has an order of its own.
	let discovered: Vec<PathBuf> = engine()
		.discover(vec![project.path().join("environments")], Search::Tree)
		.map(|discovered| {
			discovered
				.expect("discovery succeeds")
				.path
				.as_ref()
				.clone()
		})
		.collect();
	let sources: Vec<PathBuf> = exported
		.reports
		.iter()
		.map(|report| report.source.as_ref().clone())
		.collect();
	assert_eq!(sources, discovered);

	for report in &exported.reports {
		assert_eq!(report.files.len(), 300);
		let timing = report.timing.expect("timing was requested");
		assert_eq!(timing.manifests, 300);
	}

	let files = project.exported();
	assert_eq!(files.len(), 12 * 300 + 1);
	assert!(files.contains_key("ns00/ConfigMap-config0.yaml"));
	assert!(files.contains_key("ns11/ConfigMap-config299.yaml"));
}

#[test]
fn exports_a_single_loaded_environment() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{"namespace":"demo"}}"#,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);

	let engine = engine();
	let source = project.path().join("environments/demo");
	let discovered = engine
		.discover(vec![source.clone()], Search::Environment)
		.next()
		.expect("an environment")
		.expect("discovery succeeds");
	let environment = engine.load(&discovered).expect("the environment evaluates");

	let report = engine
		.export_single(&environment, &source, &options(&project))
		.expect("the export succeeds");

	assert!(!report.failed());
	assert_eq!(
		report.files,
		vec![PathBuf::from("v1.ConfigMap-settings.yaml")]
	);
	assert!(
		project.exported()["v1.ConfigMap-settings.yaml"].contains("namespace: demo"),
		"the environment's spec should have been applied"
	);
}

#[test]
fn numbers_are_formatted_the_way_tanka_formats_them() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		r"{
			config: {
				apiVersion: 'v1',
				kind: 'ConfigMap',
				metadata: { name: 'numbers' },
				data: {
					whole: 3.0,
					huge: 1e100,
					ratio: 0.1,
					negative: -0,
				},
			},
		}",
	);

	engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the export succeeds");

	// Verified byte for byte against tk. Note `huge`: the value survives, where
	// deserializing through serde's data model would have saturated it to
	// `i64::MAX`. Its shortest round-tripping form is `1e+100`; deriving a
	// mantissa through floating-point division used to corrupt it. And
	// `negative`: a negative zero cannot be written as an integer without
	// losing its sign, so it stays a float, spelled as go-yaml spells one.
	assert_eq!(
		project.exported()["v1.ConfigMap-numbers.yaml"],
		"apiVersion: v1\ndata:\n  huge: 1e+100\n  negative: -0\n  \
		 ratio: 0.1\n  whole: 3\nkind: ConfigMap\nmetadata:\n  name: numbers\n  \
		 namespace: default\n"
	);
}

/// The export has to work whether or not it is called from inside a Tokio
/// runtime, and whichever kind it is: writing files needs a runtime, and the
/// three ways of getting one behave differently (see `writer::drive`).
#[test]
fn exports_from_inside_any_runtime() {
	fn export(project: &Project, output: &str) {
		let exported = engine()
			.export_bulk(
				vec![project.path().join("environments/demo")],
				&Options {
					output_dir: project.path().join(output),
					..options(project)
				},
			)
			.expect("the export succeeds");
		assert_eq!(exported.successful(), 1);
		assert!(
			project
				.path()
				.join(output)
				.join("v1.ConfigMap-settings.yaml")
				.exists()
		);
	}

	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);

	// No runtime at all.
	export(&project, "out-none");

	// A multi-threaded runtime, from one of its workers: the export hands the
	// worker's other work off while it blocks.
	let multi_thread = tokio::runtime::Builder::new_multi_thread()
		.worker_threads(2)
		.enable_all()
		.build()
		.expect("a runtime");
	multi_thread.block_on(async {
		let project = &project;
		tokio::task::spawn_blocking(|| ())
			.await
			.expect("the pool works");
		export(project, "out-multi-worker");
	});

	// And from `block_on` itself, where blocking is allowed but hands nothing off.
	multi_thread.block_on(async { export(&project, "out-multi-block-on") });

	// A current-thread runtime, where blocking is not allowed at all, so the
	// export runs on a thread of its own.
	tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.expect("a runtime")
		.block_on(async { export(&project, "out-current-thread") });
}

/// An environment that fails does not take the others down with it.
#[test]
fn reports_failures_per_environment() {
	let project = Project::new();
	let spec =
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#;
	static_environment(
		&project,
		"environments/good",
		spec,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);
	static_environment(
		&project,
		"environments/bad",
		spec,
		r"{ boom: error 'this environment does not evaluate' }",
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments")],
			&Options {
				recursive: true,
				format: "{{env.metadata.name}}/{{.kind}}-{{.metadata.name}}".to_owned(),
				..options(&project)
			},
		)
		.expect("the export itself runs");

	assert_eq!(exported.successful(), 1);
	assert_eq!(exported.failed(), 1);

	let failed = exported
		.reports
		.iter()
		.find(|report| report.failed())
		.expect("one environment failed");
	assert!(failed.source.ends_with("environments/bad"));
	let error = failed.error.as_ref().expect("an error");
	assert!(
		error.to_string().contains("does not evaluate"),
		"unexpected error: {error}"
	);
	assert!(!error.fatal(), "one bad environment is not fatal");

	// The good environment still exported, and the index only mentions it.
	let files = project.exported();
	assert!(files.contains_key("environments-good/ConfigMap-settings.yaml"));
	let index: BTreeMap<String, String> =
		serde_json::from_str(&files["manifest.json"]).expect("a valid index");
	assert_eq!(index.len(), 1);
}

/// A project holding one environment that exports and some that cannot even be
/// discovered.
///
/// Only an inline entrypoint can fail that way: a static environment is
/// discovered by reading `spec.json`, and its Jsonnet is not evaluated until
/// much later, where a failure belongs to that one environment.
fn project_with_broken_discovery(broken: &[&str]) -> Project {
	let project = Project::new();
	static_environment(
		&project,
		"environments/good",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		&format!("{{ config: {CONFIG_MAP} }}"),
	);
	for name in broken {
		project.write(
			&format!("environments/{name}/main.jsonnet"),
			&format!("error 'cannot discover {name}'"),
		);
	}
	project
}

/// Export the named directories, in that order.
///
/// Discovery walks in `readdir` order, which is nobody's idea of an order, so
/// these name what they want rather than relying on it.
fn export_in_order(project: &Project, environments: &[&str]) -> Result<Exported, ExportError> {
	engine().export_bulk(
		environments
			.iter()
			.map(|name| project.path().join("environments").join(name))
			.collect(),
		&Options {
			recursive: true,
			// One at a time, so that "after the failure" means something.
			parallelism: 1,
			format: "{{env.metadata.name}}/{{.kind}}-{{.metadata.name}}".to_owned(),
			..options(project)
		},
	)
}

/// A directory that cannot be discovered stops the export, but what was written
/// before it is still recorded: files on disk that the index does not mention
/// are what make the *next* export go wrong.
#[test]
fn a_discovery_failure_still_records_what_was_exported() {
	let project = project_with_broken_discovery(&["broken"]);

	let error = export_in_order(&project, &["good", "broken"]).expect_err("discovery fails");
	assert!(
		error.to_string().contains("cannot discover broken"),
		"unexpected error: {error}"
	);

	let files = project.exported();
	assert!(
		files.contains_key("environments-good/ConfigMap-settings.yaml"),
		"the good environment should have been exported: {:?}",
		files.keys().collect::<Vec<_>>()
	);
	let index: BTreeMap<String, String> =
		serde_json::from_str(&files["manifest.json"]).expect("a valid index");
	assert_eq!(
		index.keys().collect::<Vec<_>>(),
		["environments-good/ConfigMap-settings.yaml"],
		"the index should describe the directory it was left with"
	);
}

/// The failure reported is the first in discovery order.
///
/// `par_bridge` serializes pulling from the iterator, and discovery is what
/// fails, so failures are usually produced in order anyway — but rayon promises
/// nothing about which of several errors a `Result` collect keeps, and this is
/// now decided on sorted results rather than by whichever worker recorded its
/// failure first.
#[test]
fn a_discovery_failure_is_reported_the_same_way_every_time() {
	let broken = ["first-broken", "second-broken", "third-broken"];

	for _ in 0..8 {
		let project = project_with_broken_discovery(&broken);
		let error = engine()
			.export_bulk(
				std::iter::once("good".to_owned())
					.chain(broken.iter().map(|name| (*name).to_owned()))
					.map(|name| project.path().join("environments").join(name))
					.collect(),
				&Options {
					recursive: true,
					format: "{{env.metadata.name}}/{{.kind}}-{{.metadata.name}}".to_owned(),
					..options(&project)
				},
			)
			.expect_err("discovery fails");

		assert!(
			error.to_string().contains("cannot discover first-broken"),
			"the first failure in discovery order should be the one reported: {error}"
		);
	}
}

/// tk's `isKubernetesManifest` requires `apiVersion` and `kind` to be strings,
/// so an object carrying a numeric `kind` is not a manifest — and, having no
/// walkable fields either, fails the export rather than being written out as it
/// stands.
#[test]
fn refuses_a_manifest_whose_kind_is_not_a_string() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
		r"{
			odd: {
				apiVersion: 'v1',
				kind: 12,
				metadata: { name: 'odd' },
			},
		}",
	);

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&Options {
				format: "{{.metadata.name}}".to_owned(),
				..options(&project)
			},
		)
		.expect("the bulk export reports per-environment failures");

	assert_eq!(exported.successful(), 0);
	let error = exported.reports[0]
		.error
		.as_ref()
		.expect("the extraction error")
		.to_string();
	assert!(
		error.contains(
			r#"found invalid Kubernetes object (at .odd): attribute "kind" is not a string, it is a float64"#
		),
		"{error}"
	);
	// The object is not written out as the manifest it appeared to be. rtk still
	// leaves an index describing the directory, where tk writes nothing at all.
	assert!(
		!project.exported().contains_key("odd.yaml"),
		"{:?}",
		project.exported().keys().collect::<Vec<_>>()
	);
}

#[test]
fn passes_top_level_arguments_to_an_inline_environment() {
	let project = Project::new();
	project.write(
		"environments/demo/main.jsonnet",
		r"function(tier, replicas) {
			apiVersion: 'tanka.dev/v1alpha1',
			kind: 'Environment',
			metadata: { name: 'demo' },
			spec: { namespace: 'demo' },
			data: {
				config: {
					apiVersion: 'v1',
					kind: 'ConfigMap',
					metadata: { name: 'settings' },
					data: { tier: tier, replicas: std.toString(replicas.count) },
				},
			},
		}",
	);

	// An entrypoint taking top level arguments is a function, so importing it is
	// not enough — it has to be called with them. An inline environment is
	// selected by name from inside Jsonnet, and that selection used to be handed
	// the function itself, matching nothing and quietly exporting nothing at all.
	let mut jsonnet = rtk_jsonnet::Options::default();
	jsonnet
		.top_level_arguments
		.insert("tier".into(), "prod".into());
	jsonnet
		.top_level_code
		.insert("replicas".into(), r#"{"count":3}"#.into());

	let engine = Engine::new(rtk_jsonnet::Engine::new(jsonnet));
	let exported = engine
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&options(&project),
		)
		.expect("the export to succeed");

	assert_eq!(exported.failed(), 0);
	assert_eq!(
		exported.reports.first().map(|report| report.files.len()),
		Some(1),
		"the environment exported nothing"
	);

	let exported = project.exported();
	let manifest = exported
		.get("v1.ConfigMap-settings.yaml")
		.expect("the ConfigMap");
	assert!(manifest.contains("tier: prod"), "unexpected: {manifest}");
	assert!(
		manifest.contains(r#"replicas: "3""#),
		"unexpected: {manifest}"
	);
}

#[test]
fn names_exported_files_by_the_extension_it_was_given() {
	for extension in ["yaml", "yml", "json"] {
		let project = Project::new();
		static_environment(
			&project,
			"environments/demo",
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
			CONFIG_MAP,
		);

		let exported = engine()
			.export_bulk(
				vec![project.path().join("environments/demo")],
				&Options {
					extension: extension.to_owned(),
					..options(&project)
				},
			)
			.expect("the export succeeds");

		assert_eq!(exported.failed(), 0);
		assert_eq!(
			exported.reports[0].files,
			[PathBuf::from(format!("v1.ConfigMap-settings.{extension}"))]
		);

		// Only the name changes: what tk writes is YAML whatever it is called.
		let exported = project.exported();
		let manifest = &exported[&format!("v1.ConfigMap-settings.{extension}")];
		assert!(manifest.starts_with("apiVersion: v1\n"), "{manifest}");
	}
}

/// An environment that fails part way through a chunk still reports the files it
/// already wrote, so they reach `manifest.json`.
///
/// The writer used to build its list locally and drop it when a write failed,
/// losing up to a whole chunk of files that were already on disk — files that
/// `fail-on-conflicts` then cannot protect and `replace-envs` will not prune.
///
/// The failure is arranged by putting a directory where the second manifest's
/// file has to go, which no amount of retrying will make writable.
#[test]
fn a_failure_part_way_through_still_records_what_reached_disk() {
	let project = Project::new();
	static_environment(
		&project,
		"environments/demo",
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{"namespace":"demo"}}"#,
		r"{
			first: { apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'aaa' } },
			second: { apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'bbb' } },
		}",
	);
	// Manifests are written in sorted order, so `aaa` lands before `bbb` fails.
	fs::create_dir_all(project.output().join("bbb.yaml")).expect("the obstruction");

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments/demo")],
			&Options {
				format: "{{.metadata.name}}".to_owned(),
				// The obstruction makes the output directory non-empty.
				merge_strategy: MergeStrategy::FailOnConflicts,
				..options(&project)
			},
		)
		.expect("the export itself to run");

	let report = &exported.reports[0];
	assert!(report.failed(), "the obstructed write should have failed");
	assert_eq!(
		report.files,
		vec![PathBuf::from("aaa.yaml")],
		"the file written before the failure should still be reported"
	);

	// And it is recorded, so the next export knows who owns it.
	let index: BTreeMap<String, String> = serde_json::from_str(
		&fs::read_to_string(project.output().join("manifest.json")).expect("an index"),
	)
	.expect("valid JSON");
	assert_eq!(
		index.get("aaa.yaml").map(String::as_str),
		Some("environments/demo/main.jsonnet")
	);
}

#[test]
fn stops_the_whole_export_once_one_environment_cannot_be_written() {
	let project = Project::new();

	// Every environment exports one resource, and the one from `b` has no name to
	// build a filename out of. Rendering nothing is not something to skip past, so
	// it stops the export — and whichever environments had not started by then are
	// reported as never having had their turn.
	for (name, resource) in [
		("a", "a-resource"),
		("b", ""),
		("c", "c-resource"),
		("d", "d-resource"),
	] {
		project.write(
			&format!("environments/{name}/main.jsonnet"),
			&format!(
				r"{{
					apiVersion: 'tanka.dev/v1alpha1',
					kind: 'Environment',
					metadata: {{ name: '{name}' }},
					spec: {{ namespace: '{name}' }},
					data: {{
						resource: {{
							apiVersion: 'v1',
							kind: 'ConfigMap',
							metadata: {{ name: '{resource}' }},
						}},
					}},
				}}"
			),
		);
	}

	let exported = engine()
		.export_bulk(
			vec![project.path().join("environments")],
			&Options {
				recursive: true,
				parallelism: 1,
				format: "{{.metadata.name}}".to_owned(),
				..options(&project)
			},
		)
		.expect("the export itself to run");

	let fatal: Vec<&rtk_environments::export::Report> = exported
		.reports
		.iter()
		.filter(|report| {
			report
				.error
				.as_ref()
				.is_some_and(|error| error.report().contains("rendered nothing"))
		})
		.collect();
	assert_eq!(fatal.len(), 1, "expected exactly one environment to fail");
	assert!(fatal[0].error.as_ref().expect("an error").fatal());

	// The rest were either already done or never started; none of them failed on
	// their own account.
	let skipped = exported
		.reports
		.iter()
		.filter(|report| report.error.as_ref().is_some_and(|error| error.skipped()))
		.count();
	assert!(skipped > 0, "nothing was reported as skipped: {exported:?}");
	assert_eq!(exported.failed(), 1 + skipped);
}
