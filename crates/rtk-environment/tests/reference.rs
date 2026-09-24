use std::fs;

use rtk_environments::Engine;
use rtk_jsonnet::Options;
use rtk_spec::canonical::{JsonentImplementationOrConfig, JsonnetImplementation};

fn reference_available() -> bool {
	match rtk_jsonnet_reference::Implementation::new() {
		Ok(_) => true,
		Err(
			rtk_jsonnet_reference::Error::Load { .. }
			| rtk_jsonnet_reference::Error::Version { .. },
		) => false,
		Err(error) => panic!("reference library has an incomplete API: {error}"),
	}
}

#[test]
fn project_selection_exports_with_the_reference_interpreter() {
	if !reference_available() {
		return;
	}
	let directory = tempfile::tempdir().unwrap();
	let environment = directory.path().join("environments/dev");
	fs::create_dir_all(&environment).unwrap();
	fs::write(directory.path().join("jsonnetfile.json"), "{}").unwrap();
	fs::write(
		directory.path().join("tkrc.yaml"),
		"spec:\n  jsonnetImplementation: c++\n",
	)
	.unwrap();
	fs::write(environment.join("spec.json"), r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{"namespace":"dev"}}"#).unwrap();
	fs::write(
		environment.join("main.jsonnet"),
		r"{apiVersion: 'v1', kind: 'ConfigMap', metadata: {name: 'demo'}, data: {hash: std.native('sha256')('foo')}}",
	)
	.unwrap();
	let engine = Engine::new(rtk_jsonnet::Engine::new(Options::default()));
	let loaded = engine.load_single(&environment, None).unwrap();
	let manifests = engine.manifests(&loaded, &[]).unwrap();
	assert_eq!(manifests.len(), 1);
	assert_eq!(manifests[0]["kind"], "ConfigMap");
	assert_eq!(
		manifests[0]["data"]["hash"],
		"2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae"
	);
}

#[test]
fn inline_environment_can_select_reference_for_export() {
	if !reference_available() {
		return;
	}
	let directory = tempfile::tempdir().unwrap();
	let environment = directory.path().join("environments/dev");
	fs::create_dir_all(&environment).unwrap();
	fs::write(directory.path().join("jsonnetfile.json"), "{}").unwrap();
	fs::write(
		environment.join("main.jsonnet"),
		r"{
        apiVersion: 'tanka.dev/v1alpha1', kind: 'Environment',
        metadata: {name: 'demo'},
        spec: {namespace: 'dev', exportJsonnetImplementation: 'c++'},
        data: {config: {apiVersion: 'v1', kind: 'ConfigMap', metadata: {name: 'demo'}, data: {value: 'reference'}}},
    }",
	)
	.unwrap();
	let engine = Engine::new(rtk_jsonnet::Engine::new(Options::default()));
	let loaded = engine.load_single(&environment, None).unwrap();
	let manifests = engine.manifests(&loaded, &[]).unwrap();
	assert_eq!(manifests.len(), 1);
	assert_eq!(manifests[0]["data"]["value"], "reference");
}

#[test]
fn explicit_reference_selection_is_not_overridden_by_project_configuration() {
	if !reference_available() {
		return;
	}
	let directory = tempfile::tempdir().unwrap();
	fs::write(directory.path().join("jsonnetfile.json"), "{}").unwrap();
	fs::write(
		directory.path().join("tkrc.yaml"),
		"spec:\n  jsonnetImplementation: jrsonnet\n",
	)
	.unwrap();
	fs::write(
		directory.path().join("main.jsonnet"),
		"{ good: 1, bad: error 'eager-reference' }",
	)
	.unwrap();
	let mut options = Options::default();
	options.rc.spec.jsonnet_implementation = Some(
		JsonentImplementationOrConfig::JsonnetImplementation(JsonnetImplementation::Reference),
	);
	let engine = Engine::new(rtk_jsonnet::Engine::new(options));
	let error = engine
		.load_single(directory.path(), None)
		.expect_err("explicit reference selection forces bad");
	assert!(error.to_string().contains("eager-reference"), "{error}");
}
