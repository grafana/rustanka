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
	fs::write(temp.path().join("values.schema.json"), "{}").unwrap();
	assert!(
		Chart::load(temp.path())
			.err()
			.unwrap()
			.to_string()
			.contains("values.schema.json")
	);
	fs::remove_file(temp.path().join("values.schema.json")).unwrap();
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

#[test]
fn loop_assignments_accumulate_and_declarations_stay_local() {
	let mut template = gtmpl_ng::Template::default();
	super::functions::install(&mut template);
	template.parse(r"{{ $sum := 0 }}{{ range $i := until 4 }}{{ $sum = add $sum $i }}{{ $local := $i }}{{ end }}{{ $sum }}|{{ $i := 99 }}{{ range $i = until 3 }}{{ end }}{{ $i }}|{{ range until 0 }}bad{{ else }}empty{{ end }}|{{ range until 1 }}ok{{ else }}bad{{ end }}|{{ range $i := until 0 }}bad{{ else }}{{ len $i }}{{ end }}").unwrap();
	assert_eq!(
		template.render(&gtmpl_ng::Context::empty()).unwrap(),
		"6|2|empty|ok|0"
	);
	template.parse("{{ $missing = 1 }}").unwrap();
	assert!(template.render(&gtmpl_ng::Context::empty()).is_err());
}

#[test]
fn ranges_sort_maps_and_initialize_empty_assignment_targets() {
	let mut template = gtmpl_ng::Template::default();
	super::functions::install(&mut template);
	template.parse(r"{{ range $key, $value := . }}{{ $key }}={{ $value }};{{ end }}{{ $i := 9 }}{{ range $i = until 0 }}bad{{ end }}{{ $i }}").unwrap();
	let context = gtmpl_ng::Context::from(super::from_json(&json!({"z": 3, "a": 1, "b": 2})));
	assert_eq!(template.render(&context).unwrap(), "a=1;b=2;z=3;[]");
}

#[test]
#[ignore = "requires Helm and exercises the full CPU-heavy benchmark chart"]
fn heavy_benchmark_chart_matches_helm() {
	let chart = Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("../../rtk-benchmarks/helm-template/charts/bench-chart");
	let output = Command::new("helm")
		.args(["template", "bench"])
		.arg(&chart)
		.args(["--namespace", "bench"])
		.output()
		.unwrap();
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	let expected =
		parse_helm_yaml_output(std::str::from_utf8(&output.stdout).unwrap(), None).unwrap();
	let options =
		serde_json::from_value(json!({"calledFrom": "/tmp/main.jsonnet", "namespace": "bench"}))
			.unwrap();
	let actual = Chart::load(&chart)
		.unwrap()
		.render("bench", &options)
		.unwrap();
	assert_eq!(actual, expected);
}

struct IgnoreFixture(tempfile::TempDir);

impl IgnoreFixture {
	fn new() -> Self {
		let temp = tempfile::tempdir().unwrap();
		fs::write(
			temp.path().join("Chart.yaml"),
			"apiVersion: v2\nname: example\nversion: 1.0.0\n",
		)
		.unwrap();
		for directory in [
			"templates/discard",
			"templates/nested/discard",
			"templates/.hidden",
			"crds/discard",
		] {
			fs::create_dir_all(temp.path().join(directory)).unwrap();
			fs::write(
				temp.path().join(directory).join("bad.yaml"),
				"{{ undefined_function }}",
			)
			.unwrap();
		}
		fs::write(
			temp.path().join("templates/.bad.yaml"),
			"{{ undefined_function }}",
		)
		.unwrap();
		fs::write(
			temp.path().join("templates/excluded.yaml"),
			"{{ undefined_function }}",
		)
		.unwrap();
		fs::write(
			temp.path().join("templates/kept.yaml"),
			"apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: kept\n",
		)
		.unwrap();
		fs::write(temp.path().join("crds/excluded.yaml"), "not: [valid yaml").unwrap();
		fs::write(
		temp.path().join(".helmignore"),
		"\u{feff}# ignored chart files\r\n  discard/  \r\n/templates/excluded.yaml\ncrds/excluded.yaml\n",
	)
	.unwrap();
		Self(temp)
	}

	fn path(&self) -> &Path {
		self.0.path()
	}

	fn render(&self) -> Value {
		Chart::load(self.path())
			.unwrap()
			.render("example", &options())
			.unwrap()
	}
}

#[test]
fn helmignore_skips_invalid_templates_crds_and_directories() {
	let chart = IgnoreFixture::new();
	assert_eq!(
		chart.render(),
		json!({"config_map_kept": {"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": "kept"}}})
	);
}

#[test]
fn helmignore_can_exclude_values_schema_and_subcharts() {
	let chart = IgnoreFixture::new();
	fs::write(chart.path().join("values.yaml"), "not: [valid yaml").unwrap();
	fs::write(chart.path().join("values.schema.json"), "not json").unwrap();
	fs::create_dir_all(chart.path().join("charts/dependency")).unwrap();
	fs::write(
		chart.path().join("charts/dependency/Chart.yaml"),
		"not: [valid yaml",
	)
	.unwrap();
	let rules = fs::read_to_string(chart.path().join(".helmignore")).unwrap();
	fs::write(
		chart.path().join(".helmignore"),
		rules + "values.yaml\n/values.schema.json\n/charts/\n",
	)
	.unwrap();
	assert_eq!(chart.render().as_object().unwrap().len(), 1);
}

#[test]
fn helmignore_rejects_invalid_rules_and_ignored_metadata() {
	let chart = IgnoreFixture::new();
	for rule in ["**", "[z-", "Chart.yaml"] {
		fs::write(chart.path().join(".helmignore"), rule).unwrap();
		let error = Chart::load(chart.path()).err().unwrap();
		assert!(format!("{error:#}").contains(if rule == "Chart.yaml" {
			"Chart.yaml is required"
		} else {
			".helmignore"
		}));
	}
}

#[test]
#[ignore = "requires Helm; compares ignored templates, CRDs, values, schemas and subcharts"]
fn helmignore_matches_real_helm() {
	let chart = IgnoreFixture::new();
	for extra_rules in [
		"",
		"values.yaml\n",
		"values.yaml\n/values.schema.json\n/charts/\n",
	] {
		if !extra_rules.is_empty() {
			fs::write(chart.path().join("values.yaml"), "not: [valid yaml").unwrap();
		}
		if extra_rules.contains("schema") {
			fs::write(chart.path().join("values.schema.json"), "not json").unwrap();
			fs::create_dir_all(chart.path().join("charts/dependency")).unwrap();
			fs::write(
				chart.path().join("charts/dependency/Chart.yaml"),
				"not: [valid yaml",
			)
			.unwrap();
		}
		fs::write(chart.path().join(".helmignore"), format!("\u{feff}# ignored files\r\n discard/ \r\n/templates/excluded.yaml\ncrds/excluded.yaml\n{extra_rules}")).unwrap();
		let output = Command::new("helm")
			.args(["template", "example"])
			.arg(chart.path())
			.args(["--namespace", "testing", "--include-crds"])
			.output()
			.unwrap();
		assert!(
			output.status.success(),
			"{}",
			String::from_utf8_lossy(&output.stderr)
		);
		let expected =
			parse_helm_yaml_output(&String::from_utf8(output.stdout).unwrap(), None).unwrap();
		assert_eq!(chart.render(), expected, "extra rules {extra_rules:?}");
	}
}
