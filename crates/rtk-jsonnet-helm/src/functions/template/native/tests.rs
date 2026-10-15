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
fn validates_schema_and_dependency_metadata() {
	let chart = FeatureChart::new("v2");
	chart.write(
		"values.schema.json",
		r#"{"type":"object","required":["requiredValue"]}"#,
	);
	let error = Chart::load(chart.path())
		.unwrap()
		.render("example", &chart.options(json!({})))
		.unwrap_err();
	assert!(format!("{error:#}").contains("requiredValue"));
	fs::remove_file(chart.path().join("values.schema.json")).unwrap();
	chart.write("charts/dependency/README.md", "incomplete dependency");
	assert!(
		format!("{:#}", Chart::load(chart.path()).err().unwrap())
			.contains("Chart.yaml is required")
	);
}

#[test]
fn resolves_an_implicit_namespace_without_helm() {
	let mut options = options();
	options.namespace = None;
	assert!(
		Chart::load(&fixture().join("chart"))
			.unwrap()
			.render("example", &options)
			.is_ok()
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
fn non_manifest_template_output_is_rejected() {
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
			.args([
				"--namespace",
				"testing",
				"--include-crds",
				"--kube-version",
				"1.34.0",
			])
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

struct FeatureChart {
	directory: tempfile::TempDir,
}

impl FeatureChart {
	fn new(version: &str) -> Self {
		let chart = Self {
			directory: tempfile::tempdir().unwrap(),
		};
		chart.write(
			"Chart.yaml",
			&format!("apiVersion: {version}\nname: parent\nversion: 1.0.0\n"),
		);
		chart
	}
	fn path(&self) -> &Path {
		self.directory.path()
	}
	fn write(&self, name: &str, content: &str) {
		let path = self.path().join(name);
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, content).unwrap();
	}
	fn options(&self, values: Value) -> Options {
		serde_json::from_value(json!({"calledFrom": self.path().join("main.jsonnet"), "namespace": "testing", "values": values})).unwrap()
	}
	fn render(&self, values: Value) -> Value {
		Chart::load(self.path())
			.unwrap()
			.render("example", &self.options(values))
			.unwrap()
	}
	fn dependency(&self) {
		self.write("Chart.yaml", "apiVersion: v2\nname: parent\nversion: 1.0.0\ndependencies:\n- name: child\n  version: 1.0.0\n  alias: renamed\n  condition: renamed.enabled\n  tags: [optional]\n  import-values:\n  - child: exports.settings\n    parent: settings\n");
		self.write("values.yaml", "global:\n  shared: parent\nsettings:\n  preferred: parent\nrenamed:\n  nested:\n    parent: true\n");
		self.write(
			"charts/child/Chart.yaml",
			"apiVersion: v1\nname: child\nversion: 1.0.0\n",
		);
		self.write("charts/child/values.yaml", "enabled: true\nvalue: original\nglobal:\n  local: child\n  shared: child\nnested:\n  remove: true\n  keep: true\nexports:\n  settings:\n    imported: child\n    preferred: child\n");
		self.write("charts/child/templates/config.yaml", "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {{ .Chart.Name }}\ndata:\n  value: {{ .Values.value | quote }}\n  global: {{ .Values.global.shared | quote }}\n  local: {{ .Values.global.local | quote }}\n  nested: {{ toJson .Values.nested | quote }}\n  helper: {{ include \"shared.name\" . | quote }}\n");
		self.write(
			"charts/child/templates/_helpers.tpl",
			"{{ define \"shared.name\" }}child{{ end }}",
		);
		self.write(
			"templates/_helpers.tpl",
			"{{ define \"shared.name\" }}parent{{ end }}",
		);
		self.write("templates/config.yaml", "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: parent\ndata:\n  imported: {{ .Values.settings.imported | quote }}\n  preferred: {{ .Values.settings.preferred | quote }}\n  childValue: {{ .Subcharts.renamed.Values.value | quote }}\n");
	}
	fn helm(&self, values: &Value, api_versions: &[String]) -> Value {
		let values_file = tempfile::NamedTempFile::new().unwrap();
		fs::write(values_file.path(), serde_json::to_vec(values).unwrap()).unwrap();
		let mut command = Command::new("helm");
		command
			.args(["template", "example"])
			.arg(self.path())
			.args([
				"--namespace",
				"testing",
				"--include-crds",
				"--kube-version",
				"1.34.0",
				"--values",
			])
			.arg(values_file.path());
		for version in api_versions {
			command.args(["--api-versions", version]);
		}
		let output = command.output().unwrap();
		assert!(
			output.status.success(),
			"{}",
			String::from_utf8_lossy(&output.stderr)
		);
		parse_helm_yaml_output(&String::from_utf8(output.stdout).unwrap(), None).unwrap()
	}
}

#[test]
fn subcharts_inherit_values_globals_aliases_and_imports() {
	let chart = FeatureChart::new("v2");
	chart.dependency();
	let actual =
		chart.render(json!({"renamed": {"value": "override", "nested": {"remove": null}}}));
	assert_eq!(
		actual["config_map_renamed"]["data"],
		json!({"value":"override", "global":"parent", "local":"child", "nested":"{\"keep\":true,\"parent\":true}", "helper":"parent"})
	);
	assert_eq!(
		actual["config_map_parent"]["data"],
		json!({"imported":"child", "preferred":"parent", "childValue":"override"})
	);
}

#[test]
fn dependency_conditions_override_tags() {
	let chart = FeatureChart::new("v2");
	chart.dependency();
	// A disabled dependency contributes no templates, so the parent cannot refer to its helpers or Subcharts scope.
	chart.write(
		"templates/config.yaml",
		"apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: parent\n",
	);
	assert_eq!(
		chart
			.render(json!({"tags": {"optional": false}, "renamed": {"enabled": true}}))
			.as_object()
			.unwrap()
			.len(),
		2
	);
	assert_eq!(
		chart
			.render(json!({"tags": {"optional": true}, "renamed": {"enabled": false}}))
			.as_object()
			.unwrap()
			.len(),
		1
	);
}

#[test]
fn v1_charts_and_library_helpers_render() {
	let chart = FeatureChart::new("v1");
	chart.write(
		"requirements.yaml",
		"dependencies:\n- name: helpers\n  version: 1.0.0\n",
	);
	chart.write(
		"charts/helpers/Chart.yaml",
		"apiVersion: v2\nname: helpers\nversion: 1.0.0\ntype: library\n",
	);
	chart.write(
		"charts/helpers/templates/_helpers.tpl",
		"{{ define \"library.name\" }}from-library{{ end }}",
	);
	chart.write(
		"charts/helpers/templates/ignored.yaml",
		"{{ unsupported_library_template }}",
	);
	chart.write(
		"templates/config.yaml",
		"apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {{ include \"library.name\" . }}\n",
	);
	assert_eq!(chart.render(json!({})).as_object().unwrap().len(), 1);
	assert_eq!(
		chart.render(json!({}))["config_map_from_library"]["metadata"]["name"],
		"from-library"
	);
}

#[test]
fn values_schema_checks_merged_root_and_child_values() {
	let chart = FeatureChart::new("v2");
	chart.dependency();
	chart.write(
		"charts/child/values.schema.json",
		r#"{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}"#,
	);
	assert!(
		Chart::load(chart.path())
			.unwrap()
			.render("example", &chart.options(json!({"renamed":{"value":42}})))
			.is_err()
	);
	assert_eq!(
		chart.render(json!({"renamed":{"value":"valid"}}))["config_map_renamed"]["data"]["value"],
		"valid"
	);
	chart.write(
		"templates/config.yaml",
		"apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: parent\n",
	);
	assert!(
		Chart::load(chart.path())
			.unwrap()
			.render(
				"example",
				&chart.options(json!({"renamed":{"enabled":false,"value":42}}))
			)
			.is_ok()
	);
}

#[test]
fn native_functions_capabilities_and_files_are_scoped() {
	let chart = FeatureChart::new("v2");
	chart.write("config.txt", "parent file\n");
	chart.write("templates/config.yaml", "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: parent\ndata:\n  api: {{ .Capabilities.APIVersions.Has \"custom.example/v1\" | quote }}\n  builtin: {{ .Capabilities.APIVersions.Has \"apps/v1\" | quote }}\n  file: {{ .Files.Get \"config.txt\" | trim | quote }}\n  funcs: {{ printf \"%s/%s/%s/%s/%t/%t\" (substr 1 3 \"abcd\") (replace \"a\" \"b\" \"aa\") (first (list \"first\" \"last\")) (toString 42) (typeIs \"string\" \"x\") (semverCompare \">=1.20.0 <2.0.0\" .Capabilities.KubeVersion.Version) | quote }}\n  lookup: {{ empty (lookup \"v1\" \"Secret\" \"testing\" \"missing\") | quote }}\n");
	let mut options = chart.options(json!({}));
	options.api_versions.push("custom.example/v1".into());
	let actual = Chart::load(chart.path())
		.unwrap()
		.render("example", &options)
		.unwrap();
	assert_eq!(
		actual["config_map_parent"]["data"],
		json!({"api":"true", "builtin":"true", "file":"parent file", "funcs":"bc/bb/first/42/true/true", "lookup":"true"})
	);
}

#[test]
#[ignore = "requires Helm; compares dependencies, schemas, v1/library charts, files, capabilities and functions"]
fn expanded_features_match_real_helm() {
	let chart = FeatureChart::new("v2");
	chart.dependency();
	let values = json!({"renamed":{"value":"override", "nested":{"remove":null}}});
	assert_eq!(chart.render(values.clone()), chart.helm(&values, &[]));
	let chart = FeatureChart::new("v1");
	chart.write("templates/config.yaml", "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: v1\ndata:\n  kube: {{ .Capabilities.KubeVersion.Version | quote }}\n");
	assert_eq!(chart.render(json!({})), chart.helm(&json!({}), &[]));
}

impl FeatureChart {
	fn pack(&self, relative: &str, name: &str) -> Vec<u8> {
		let directory = self.path().join(relative);
		let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
		let mut builder = tar::Builder::new(encoder);
		for entry in walkdir::WalkDir::new(&directory).sort_by_file_name() {
			let entry = entry.unwrap();
			if entry.file_type().is_file() {
				let path = entry.path().strip_prefix(&directory).unwrap();
				let bytes = fs::read(entry.path()).unwrap();
				let mut header = tar::Header::new_gnu();
				header.set_size(bytes.len() as u64);
				header.set_mode(0o644);
				header.set_cksum();
				builder
					.append_data(&mut header, Path::new(name).join(path), &bytes[..])
					.unwrap();
			}
		}
		builder.into_inner().unwrap().finish().unwrap()
	}
}

#[test]
fn packed_dependencies_and_root_archives_render() {
	let chart = FeatureChart::new("v2");
	chart.dependency();
	let expected = chart.render(json!({}));
	let archive = chart.pack("charts/child", "child");
	fs::remove_dir_all(chart.path().join("charts/child")).unwrap();
	fs::write(chart.path().join("charts/child-1.0.0.tgz"), archive).unwrap();
	assert_eq!(chart.render(json!({})), expected);
	let archive = tempfile::NamedTempFile::new().unwrap();
	fs::write(archive.path(), chart.pack("", "parent")).unwrap();
	assert_eq!(
		Chart::load(archive.path())
			.unwrap()
			.render("example", &chart.options(json!({})))
			.unwrap(),
		expected
	);
}

#[test]
fn archive_paths_cannot_escape_the_chart() {
	let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
	let mut builder = tar::Builder::new(encoder);
	let mut header = tar::Header::new_gnu();
	let name = b"parent/../escaped";
	header.as_mut_bytes()[..name.len()].copy_from_slice(name);
	header.set_size(1);
	header.set_mode(0o644);
	header.set_cksum();
	builder.append(&header, &b"x"[..]).unwrap();
	let bytes = builder.into_inner().unwrap().finish().unwrap();
	assert!(
		Chart::archive(&bytes)
			.unwrap_err()
			.to_string()
			.contains("invalid chart archive path")
	);
}

#[test]
fn semver_constraints_accept_sprig_versions_and_zero_major_carets() {
	for (constraint, version, expected) in [
		(">=1.20.0 <2.0.0", "v1.37.0", true),
		(">=1.20.0-0", "v1.20.0-beta.1", true),
		(">=1.20.0", "v1.21.0-beta.1", false),
		("^0.1.2", "0.1.3", true),
		("^0.1.2", "0.2.0", false),
		("^0.0.2", "0.0.3", false),
		("1.2.x || >=2.0.0", "1.2.3", true),
		("1.2 - 1.4", "1.3.0", true),
	] {
		assert_eq!(
			super::versions::Version::parse(version)
				.unwrap()
				.matches(constraint)
				.unwrap(),
			expected,
			"{constraint}, {version}"
		);
	}
}

#[test]
#[ignore = "requires Helm; checks method receivers, file globs, library helpers and packed dependencies"]
fn native_methods_and_archives_match_real_helm() {
	let chart = FeatureChart::new("v2");
	chart.write("configs/a.txt", "alpha\n");
	chart.write("configs/nested/b.txt", "beta\n");
	chart.write(
		"templates/config.yaml",
		r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: methods
data:
  config: {{ (.Files.Glob "configs/**").AsConfig | quote }}
  secrets: {{ (.Files.Glob "configs/*").AsSecrets | quote }}
  getter: {{ with .Files }}{{ .Get "configs/a.txt" | trim | quote }}{{ end }}
  alias: {{ $apis := .Capabilities.APIVersions }}{{ $apis.Has "apps/v1" | quote }}
  literal: {{ ".Files.Get }} {{ .Capabilities.APIVersions.Has" | quote }}
  negative: {{ -3 | quote }}
  major: {{ (semver "1.2.3").Major | quote }}
"#,
	);
	assert_eq!(chart.render(json!({})), chart.helm(&json!({}), &[]));
	let chart = FeatureChart::new("v2");
	chart.dependency();
	chart.write("charts/child/config.txt", "child file\n");
	chart.write(
		"templates/config.yaml",
		r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: parent
data:
  child: {{ (index .Subcharts "renamed").Files.Get "config.txt" | trim | quote }}
"#,
	);
	assert_eq!(chart.render(json!({})), chart.helm(&json!({}), &[]));
	let archive = chart.pack("charts/child", "child");
	fs::remove_dir_all(chart.path().join("charts/child")).unwrap();
	fs::write(chart.path().join("charts/child.tgz"), archive).unwrap();
	assert_eq!(chart.render(json!({})), chart.helm(&json!({}), &[]));
}

#[test]
#[ignore = "requires Helm and RTK_NATIVE_CHART_CORPUS; smoke-tests an external chart corpus"]
fn external_chart_corpus_renders_with_native_helm() {
	let root = PathBuf::from(
		std::env::var_os("RTK_NATIVE_CHART_CORPUS").expect("RTK_NATIVE_CHART_CORPUS is required"),
	);
	let limit: usize = std::env::var("RTK_NATIVE_CHART_LIMIT")
		.ok()
		.and_then(|value| value.parse().ok())
		.unwrap_or(30);
	let mut paths = Vec::new();
	for entry in walkdir::WalkDir::new(&root).sort_by_file_name() {
		let entry = entry.unwrap();
		if entry.file_type().is_file() && entry.file_name() == "Chart.yaml" {
			let path = entry.path().parent().unwrap();
			if path
				.strip_prefix(&root)
				.unwrap()
				.components()
				.filter(|part| part.as_os_str() == "charts")
				.count() > 1
			{
				continue;
			}
			paths.push(path.to_owned());
		}
	}
	let filter = std::env::var("RTK_NATIVE_CHART_FILTER")
		.ok()
		.map(|pattern| regex::Regex::new(&pattern).unwrap());
	if let Some(filter) = filter {
		paths.retain(|path| filter.is_match(&path.to_string_lossy()));
	}
	let mut failures = Vec::new();
	let mut successful = 0;
	for path in paths.into_iter().take(limit) {
		let helm = Command::new("helm")
			.args(["template", "example"])
			.arg(&path)
			.args([
				"--namespace",
				"testing",
				"--include-crds",
				"--kube-version",
				"1.34.0",
			])
			.output()
			.unwrap();
		if !helm.status.success() {
			continue;
		}
		let options: Options = serde_json::from_value(
			json!({"calledFrom": path.join("main.jsonnet"), "namespace":"testing"}),
		)
		.unwrap();
		match Chart::load(&path).and_then(|chart| chart.render("example", &options)) {
			Ok(_) => {
				successful += 1;
				eprintln!("native render passed: {}", path.display());
			}
			Err(error) => failures.push(format!("{}: {error:#}", path.display())),
		}
	}
	assert!(
		failures.is_empty(),
		"{} charts failed:\n{}",
		failures.len(),
		failures.join("\n")
	);
	assert!(successful > 0, "no charts rendered successfully");
}

#[test]
fn sprig_dictionary_and_string_operations() {
	let chart = FeatureChart::new("v2");
	chart.write("templates/config.yaml", r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: functions
data:
  nested: {{ dig "outer" "inner" "fallback" (dict "outer" (dict "inner" "found")) | quote }}
  missing: {{ dig "absent" "inner" "fallback" (dict "outer" (dict)) | quote }}
  merge: {{ merge (dict "nested" (dict "first" "a")) (dict "nested" (dict "first" "b" "second" "c")) | toJson | quote }}
  overwrite: {{ mergeOverwrite (dict "nested" (dict "first" "a")) (dict "nested" (dict "first" "b" "second" "c")) | toJson | quote }}
  substring: {{ substr 2 -1 "abcdef" | quote }}
  prefix: {{ trimPrefix "aa" "aaaa" | quote }}
  suffix: {{ trimSuffix "aa" "aaaa" | quote }}
  sorted: {{ list "b" "a" | sortAlpha | join "," | quote }}
  json: {{ dict "markup" "<>&" | toJson | quote }}
  raw: {{ dict "markup" "<>&" | toRawJson | quote }}
"#);
	let output = chart.render(json!({}));
	let data = &output["config_map_functions"]["data"];
	assert_eq!(data["nested"], "found");
	assert_eq!(data["missing"], "fallback");
	assert_eq!(data["merge"], r#"{"nested":{"first":"a","second":"c"}}"#);
	assert_eq!(
		data["overwrite"],
		r#"{"nested":{"first":"b","second":"c"}}"#
	);
	assert_eq!(data["substring"], "cdef");
	assert_eq!(data["prefix"], "aa");
	assert_eq!(data["suffix"], "aa");
	assert_eq!(data["sorted"], "a,b");
	assert_eq!(data["json"], r#"{"markup":"\u003c\u003e\u0026"}"#);
	assert_eq!(data["raw"], r#"{"markup":"<>&"}"#);
}

#[test]
#[ignore = "requires Helm"]
fn sprig_dictionary_operations_match_helm() {
	let chart = FeatureChart::new("v2");
	chart.write(
		"templates/config.yaml",
		r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: merge
data:
  merge: {{ merge (dict "a" (dict "first" 1)) (dict "a" (dict "second" 2)) | toJson | quote }}
  dig: {{ dig "a" "b" "fallback" (dict "a" (dict "b" "found")) | quote }}
"#,
	);
	assert_eq!(chart.render(json!({})), chart.helm(&json!({}), &[]));
}

#[test]
fn helm_nil_lists_and_raw_literals() {
	let chart = FeatureChart::new("v2");
	chart.write(
		"templates/config.yaml",
		r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: syntax
data:
  slash: {{ "\\" | quote }}
  raw: {{ regexMatch `^v[0-9]+\.[0-9]+$` "v1.2" | quote }}
  member: {{ has "present" (tuple "present" "other") | quote }}
  absent: {{ has "absent" .Values.missing | quote }}
  nil: {{ kindIs "invalid" .Values.missing | quote }}
  fallback: {{ default "fallback" | quote }}
  float: {{ float64 .Values.missing | quote }}
  integer: {{ int64 "invalid" | quote }}
  root: {{ .missing | default "fallback" | quote }}
  optional: {{ (.Values.missing).field | default "fallback" | quote }}
"#,
	);
	assert_eq!(
		chart.render(json!({}))["config_map_syntax"]["data"],
		json!({"slash":"\\", "raw":"true", "member":"true", "absent":"false", "nil":"true", "fallback":"fallback", "optional":"fallback", "root":"fallback", "float":"0", "integer":"0"})
	);
}

#[test]
fn custom_certificate_and_empty_yaml_helpers() {
	let chart = FeatureChart::new("v2");
	chart.write(
		"templates/config.yaml",
		r#"{{ $ca := genCA "testing" 1 }}
{{ $copy := buildCustomCert ($ca.Cert | b64enc) ($ca.Key | b64enc) }}
apiVersion: v1
kind: ConfigMap
metadata:
  name: conversions
data:
  cert: {{ eq $ca.Cert $copy.Cert | quote }}
  key: {{ eq $ca.Key $copy.Key | quote }}
  empty: {{ "" | fromYaml | toJson | quote }}
  first: {{ "one: 1\n---\ntwo: 2\n" | fromYaml | toJson | quote }}
---# Helm splits separators before YAML parsing.
apiVersion: v1
kind: ConfigMap
metadata:
  name: second
"#,
	);
	let output = chart.render(json!({}));
	assert_eq!(
		output["config_map_conversions"]["data"],
		json!({"cert":"true", "key":"true", "empty":"{}", "first":"{\"one\":1}"})
	);
	assert!(output.get("config_map_second").is_some());
}

#[test]
fn dictionary_aliases_and_helpers_keep_reference_identity() {
	let chart = FeatureChart::new("v2");
	chart.write(
		"templates/_helpers.tpl",
		r#"{{define "mutate"}}{{ $_ := set . "value" "helper" }}{{end}}"#,
	);
	chart.write(
		"templates/config.yaml",
		r#"{{ $dict := dict "nested" (dict "before" true) }}
{{ $alias := $dict.nested }}{{ $_ := set $alias "after" true }}
{{ $_ := include "mutate" $alias }}{{ $_ := unset $alias "before" }}
{{ $copy := deepCopy $dict }}{{ $_ := set $copy.nested "value" "copy" }}
{{ $merged := dict "nested" (dict "retained" true) }}
{{ $_ := merge $dict $merged }}
apiVersion: v1
kind: ConfigMap
metadata:
  name: references
data:
  original: {{ $dict | toJson | quote }}
  alias: {{ $alias | toJson | quote }}
  copy: {{ $copy | toJson | quote }}
"#,
	);
	let data = chart.render(json!({}))["config_map_references"]["data"].clone();
	assert_eq!(
		data,
		json!({"original":r#"{"nested":{"after":true,"retained":true,"value":"helper"}}"#, "alias":r#"{"after":true,"retained":true,"value":"helper"}"#, "copy":r#"{"nested":{"after":true,"value":"copy"}}"#})
	);
}

#[test]
#[ignore = "requires Helm"]
fn profile_templates_mutate_values_before_manifests_match_helm() {
	let chart = FeatureChart::new("v2");
	chart.write("templates/zzz_profile.yaml", r#"{{ $defaults := dict "image" "registry/operator:latest" }}{{ $_ := set . "Values" (mergeOverwrite $defaults .Values) }}"#);
	chart.write(
		"templates/config.yaml",
		r#"{{ $_ := set . "path" "/api/" }}
apiVersion: v1
kind: ConfigMap
metadata:
  name: profile
data:
  image: {{ .Values.image | quote }}
  path: {{ .path | quote }}
"#,
	);
	assert_eq!(chart.render(json!({})), chart.helm(&json!({}), &[]));
}

#[test]
#[ignore = "requires Helm"]
fn production_helper_regressions_match_helm() {
	let chart = FeatureChart::new("v2");
	chart.write(
		"templates/config.yaml",
		r#"{{ $_ := set .Values.AsMap "added" "present" }}
apiVersion: v1
kind: ConfigMap
metadata:
  name: regressions
data:
  alias: {{ .Values.added | quote }}
  empty: {{ get (dict) "missing" | upper | quote }}
  yaml: {{ "key: first\nkey: last\n" | fromYaml | toJson | quote }}
  array: {{ "" | fromYamlArray | toJson | quote }}
  random: {{ randInt 4 5 | quote }}
  url: {{ urlParse "https://user:password@example.com:8443/a%20path?x=1#section" | toJson | quote }}
  regex: {{ regexMatch `\$(?:__(?:env|file|vault))?{[^}]+}` "$__env{PASSWORD}" | quote }}
"#,
	);
	for values in [json!({}), json!({"seed":true})] {
		assert_eq!(chart.render(values.clone()), chart.helm(&values, &[]));
	}
}
