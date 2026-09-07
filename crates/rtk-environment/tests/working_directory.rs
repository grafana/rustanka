//! Loading must not depend on the process working directory.
//!
//! This is its own test target because it changes the working directory, which
//! is process-wide: cargo builds each integration test as a separate binary, so
//! the single test below has the process to itself.

use std::fs;

use rtk_environments::Engine;

fn engine() -> Engine {
	Engine::new(rtk_jsonnet::Engine::new(rtk_jsonnet::Options::default()))
}

fn config_map(name: &str) -> String {
	format!("{{ cm: {{ apiVersion: 'v1', kind: 'ConfigMap', metadata: {{ name: '{name}' }} }} }}")
}

/// A project whose root is an entrypoint of its own, with an environment below
/// it, loaded from the project root.
///
/// The entrypoint is imported by a generated snippet, and a relative import is
/// resolved against the importing file before any import path. The snippet has
/// no file, so the working directory used to decide, and this loaded the root
/// entrypoint instead of the environment's — silently, since both evaluate.
#[test]
fn loads_the_environments_entrypoint_not_the_one_in_the_working_directory() {
	let temp = tempfile::tempdir().unwrap();
	let root = temp.path().canonicalize().unwrap();
	let environment = root.join("environments/foo");
	fs::create_dir_all(&environment).unwrap();
	fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
	fs::write(root.join("main.jsonnet"), config_map("from-the-root")).unwrap();
	fs::write(
		environment.join("main.jsonnet"),
		config_map("from-the-environment"),
	)
	.unwrap();
	fs::write(
		environment.join("spec.json"),
		r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{"namespace":"foo-ns"}}"#,
	)
	.unwrap();

	let previous = std::env::current_dir().unwrap();
	std::env::set_current_dir(&root).unwrap();

	let loaded = engine().load_single("environments/foo".as_ref(), None);
	let manifests = loaded
		.as_ref()
		.map(|loaded| engine().manifests(loaded, &[]));

	std::env::set_current_dir(previous).unwrap();

	let manifests = manifests
		.expect("the environment loads")
		.expect("its manifests are collected");
	let names = manifests
		.iter()
		.filter_map(|manifest| manifest["metadata"]["name"].as_str())
		.collect::<Vec<_>>();

	assert_eq!(names, vec!["from-the-environment"]);
}
