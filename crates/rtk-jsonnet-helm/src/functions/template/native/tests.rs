use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use super::{Chart, Options};
use crate::functions::template::parse_helm_yaml_output;

fn fixture() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/native")
}

fn options() -> Options {
	serde_json::from_value(json!({
		"calledFrom": "/tmp/main.jsonnet",
		"namespace": "testing",
		"values": serde_json::from_str::<Value>(&fs::read_to_string(fixture().join("values.json")).unwrap()).unwrap(),
	})).unwrap()
}

#[test]
fn matches_helm_golden_with_helpers_values_hooks_and_crds() {
	let expected = parse_helm_yaml_output(
		&fs::read_to_string(fixture().join("helm.golden.yaml")).unwrap(),
		None,
	)
	.unwrap();
	let rendered = Chart::load(&fixture().join("chart"))
		.unwrap()
		.render("example", &options())
		.unwrap();
	assert_eq!(rendered, expected);
}

#[test]
fn excludes_hooks_and_crds_on_request() {
	let mut options = options();
	options.include_crds = false;
	options.no_hooks = true;
	let rendered = Chart::load(&fixture().join("chart"))
		.unwrap()
		.render("example", &options)
		.unwrap();
	assert_eq!(rendered.as_object().unwrap().len(), 1);
	assert_eq!(
		rendered["config_map_example_native_prototype"]["kind"],
		"ConfigMap"
	);
}

#[test]
fn rejects_unsupported_chart_features() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("Chart.yaml"),
		"apiVersion: v2\nname: example\nversion: 1.0.0\n",
	)
	.unwrap();
	for filename in ["values.schema.json", ".helmignore"] {
		fs::write(temp.path().join(filename), "{}").unwrap();
		assert!(
			Chart::load(temp.path())
				.err()
				.unwrap()
				.to_string()
				.contains(filename)
		);
		fs::remove_file(temp.path().join(filename)).unwrap();
	}
	fs::create_dir_all(temp.path().join("charts/dependency")).unwrap();
	assert!(
		Chart::load(temp.path())
			.err()
			.unwrap()
			.to_string()
			.contains("subcharts")
	);
}

#[test]
fn rejects_unresolved_namespace_and_api_versions() {
	let mut options = options();
	options.namespace = None;
	assert!(
		Chart::load(&fixture().join("chart"))
			.unwrap()
			.render("example", &options)
			.unwrap_err()
			.to_string()
			.contains("explicit namespace")
	);
	options.namespace = Some("testing".into());
	options.api_versions.push("apps/v1".into());
	assert!(
		Chart::load(&fixture().join("chart"))
			.unwrap()
			.render("example", &options)
			.unwrap_err()
			.to_string()
			.contains("apiVersions")
	);
}

#[test]
fn failed_include_does_not_leak_context_into_next_render() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("Chart.yaml"),
		"apiVersion: v2\nname: example\nversion: 1.0.0\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("templates")).unwrap();
	fs::write(
		temp.path().join("templates/value.yaml"),
		r#"{{ define "recursive" }}{{ include "recursive" . }}{{ end }}{{ include "recursive" . }}"#,
	)
	.unwrap();
	let error = Chart::load(temp.path())
		.unwrap()
		.render("example", &options())
		.unwrap_err();
	assert!(format!("{error:#}").contains("recursion exceeds"));
	assert!(super::SCOPES.with_borrow(Vec::is_empty));
	matches_helm_golden_with_helpers_values_hooks_and_crds();
}

#[test]
fn unsupported_functions_and_objects_fail() {
	let temp = tempfile::tempdir().unwrap();
	fs::write(
		temp.path().join("Chart.yaml"),
		"apiVersion: v2\nname: example\nversion: 1.0.0\n",
	)
	.unwrap();
	fs::create_dir(temp.path().join("templates")).unwrap();
	for source in [
		"{{ lookup \"v1\" \"Pod\" \"\" \"\" }}",
		"{{ .Capabilities }}",
		"{{ .Files }}",
	] {
		fs::write(temp.path().join("templates/value.yaml"), source).unwrap();
		assert!(
			Chart::load(temp.path())
				.unwrap()
				.render("example", &options())
				.is_err(),
			"{source}"
		);
	}
}

#[test]
#[ignore = "requires the Helm executable; compares the prototype with real Helm"]
fn differential_existing_charts() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_fixtures/golden_envs");
	for (environment, chart) in [
		("helm_template_env", "test-chart"),
		("helm_list_env", "list-chart"),
		("helm_merge_key_crd_env", "test-chart"),
		("helm_no_hooks_env", "hook-chart"),
	] {
		let path = root.join(environment).join("charts").join(chart);
		for include_crds in [false, true] {
			for no_hooks in [false, true] {
				let options: Options = serde_json::from_value(json!({"calledFrom": "/tmp/main.jsonnet", "namespace": "testing", "includeCrds": include_crds, "noHooks": no_hooks})).unwrap();
				let mut command = Command::new("helm");
				command
					.arg("template")
					.arg("example")
					.arg(&path)
					.args(["--namespace", "testing"]);
				if include_crds {
					command.arg("--include-crds");
				}
				if no_hooks {
					command.arg("--no-hooks");
				}
				let output = command.output().unwrap();
				assert!(
					output.status.success(),
					"{}",
					String::from_utf8_lossy(&output.stderr)
				);
				let expected =
					parse_helm_yaml_output(std::str::from_utf8(&output.stdout).unwrap(), None)
						.unwrap();
				let actual = Chart::load(&path)
					.unwrap()
					.render("example", &options)
					.unwrap();
				assert_eq!(
					actual, expected,
					"{environment}, includeCrds={include_crds}, noHooks={no_hooks}"
				);
			}
		}
	}
}
