//! Integration tests for the validate command (manifests, lint, test, environments).

use std::{fs, path::PathBuf};

use rtk::commands::validate::{environments, lint, manifests, test as validate_test};

fn testdata_path(subpath: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("tests/testdata/validate")
		.join(subpath)
}

// ---------------------------------------------------------------------------
// validate manifests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod manifests_tests {
	use super::*;

	#[test]
	fn test_manifests_mixed_pass_and_fail() {
		// check_labels.jsonnet: manifestTest that checks for labels
		// deployment.yaml has no labels -> should fail
		// configmap.yaml has labels -> should pass
		// check_namespace.jsonnet: namespaceTest that checks non-empty -> should pass
		let args = manifests::ManifestsArgs {
			export_dir: testdata_path("manifests_test/export")
				.to_string_lossy()
				.to_string(),
			recursive: false,
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		// Should fail because deployment.yaml is missing labels
		assert!(result.is_err(), "should fail: {}", output_str);
		assert!(
			output_str.contains("FAIL"),
			"should contain FAIL: {}",
			output_str
		);
		assert!(
			output_str.contains("missing labels"),
			"should mention missing labels: {}",
			output_str
		);
		// The configmap with labels should pass check_labels
		// The namespace test should pass
		assert!(
			output_str.contains("passed"),
			"should have some passes: {}",
			output_str
		);
	}

	#[test]
	fn test_manifests_all_pass() {
		// Use a custom export dir with only the labeled configmap
		let temp = tempfile::TempDir::new().unwrap();
		let export_dir = temp.path().join("export");
		std::fs::create_dir_all(&export_dir).unwrap();

		// Write a manifest that has labels
		std::fs::write(
			export_dir.join("configmap.yaml"),
			"apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: test\n  namespace: default\n  labels:\n    app: test\ndata:\n  key: value\n",
		).unwrap();

		let args = manifests::ManifestsArgs {
			export_dir: export_dir.to_string_lossy().to_string(),
			recursive: false,
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok(), "should pass: {}", output_str);
		assert!(
			!output_str.contains("FAIL"),
			"should not contain FAIL: {}",
			output_str
		);
		assert!(
			output_str.contains("0 failed"),
			"should have 0 failed: {}",
			output_str
		);
	}

	#[test]
	fn test_manifests_log_slowest() {
		let temp = tempfile::TempDir::new().unwrap();
		let export_dir = temp.path().join("export");
		std::fs::create_dir_all(&export_dir).unwrap();

		std::fs::write(
			export_dir.join("configmap.yaml"),
			"apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: test\n  namespace: default\n  labels:\n    app: test\ndata:\n  key: value\n",
		).unwrap();

		let args = manifests::ManifestsArgs {
			export_dir: export_dir.to_string_lossy().to_string(),
			recursive: false,
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: Some(5),
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok(), "should pass: {}", output_str);
		assert!(
			output_str.contains("Slowest work items:"),
			"should contain slowest section: {}",
			output_str
		);
		// Should list work items with timing and kind (one eval per namespace)
		assert!(
			output_str.contains("[namespace]"),
			"should contain test kind labels: {}",
			output_str
		);
	}

	/// Reproduces a parsing regression where an indented `---` inside a YAML
	/// block-literal scalar (e.g. a `ConfigMap` embedding Prometheus alert rules
	/// in `data.rules: |`) is misinterpreted as a document boundary, splitting
	/// the document mid-scalar. The split halves then fail to parse.
	#[test]
	fn test_manifests_yaml_dashes_inside_block_scalar() {
		let temp = tempfile::TempDir::new().unwrap();
		let export_dir = temp.path().join("ns");
		std::fs::create_dir_all(&export_dir).unwrap();

		let yaml = "apiVersion: v1\n\
		            kind: ConfigMap\n\
		            metadata:\n  \
		              name: cm-with-rules\n  \
		              namespace: default\n  \
		              labels:\n    \
		                app: test\n\
		            data:\n  \
		              rules: |\n    \
		                ---\n    \
		                alert: SomeAlert\n    \
		                expr: up == 0\n";
		std::fs::write(export_dir.join("ConfigMap-cm-with-rules.yaml"), yaml).unwrap();

		let args = manifests::ManifestsArgs {
			export_dir: export_dir.to_string_lossy().to_string(),
			recursive: false,
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(
			result.is_ok(),
			"indented `---` inside a block scalar must not split documents: {}",
			output_str
		);
	}

	/// Reproduces parsing failure seen with CustomResourceDefinition-scaledjobs.keda.sh.yaml:
	/// "mapping values are not allowed in this context at line 3, column 45"
	/// Trigger: unquoted scalar containing ": " is interpreted as start of a new mapping.
	#[test]
	fn test_manifests_yaml_mapping_values_error() {
		let temp = tempfile::TempDir::new().unwrap();
		let export_dir = temp
			.path()
			.join("flux")
			.join("pop-prod-aws-oregon-0")
			.join("_cluster");
		std::fs::create_dir_all(&export_dir).unwrap();

		// Unquoted value "something: else" makes the parser see a mapping after the colon
		let yaml = r#"apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: scaledjobs.keda.sh
  annotations:
    description: Schedule in cron format e.g. 0 0 * * * or: 5m
"#;
		std::fs::write(
			export_dir.join("CustomResourceDefinition-scaledjobs.keda.sh.yaml"),
			yaml,
		)
		.unwrap();

		let args = manifests::ManifestsArgs {
			export_dir: temp.path().to_string_lossy().to_string(),
			recursive: true,
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let _output_str = String::from_utf8(output).unwrap();

		let err = result.unwrap_err();
		// Full chain (anyhow): top-level + "Caused by: ..." for each cause
		let err_str = format!("{:#}", err);

		assert!(
			err_str.contains("CustomResourceDefinition-scaledjobs.keda.sh.yaml"),
			"should mention the file: {}",
			err_str
		);
		assert!(
			err_str.contains("mapping values are not allowed in this context"),
			"should report mapping values error (like real CRD parse failure): {}",
			err_str
		);
	}

	/// The real CustomResourceDefinition-scaledjobs.keda.sh.yaml from kube-manifests
	/// previously failed to parse with "mapping values are not allowed in this
	/// context at line 3, column 45". The root cause was an indented `---` deep
	/// inside the schema description being misread as a YAML document boundary,
	/// which split the file into two halves that no longer parsed individually.
	/// After fixing `is_document_boundary_line` to require column 0, the real
	/// file parses cleanly.
	#[test]
	fn test_manifests_yaml_real_scaledjobs_crd_parses() {
		let export_dir = testdata_path("manifests_test/export_crd_failure");
		if !export_dir.exists() {
			return;
		}

		let args = manifests::ManifestsArgs {
			export_dir: export_dir.to_string_lossy().to_string(),
			recursive: false,
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(
			result.is_ok(),
			"real scaledjobs CRD must parse without error: {}",
			output_str
		);
	}

	#[test]
	fn test_manifests_no_manifests() {
		let temp = tempfile::TempDir::new().unwrap();
		let empty_dir = temp.path().join("empty");
		std::fs::create_dir_all(&empty_dir).unwrap();

		let args = manifests::ManifestsArgs {
			export_dir: empty_dir.to_string_lossy().to_string(),
			recursive: false,
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok());
		assert!(
			output_str.contains("No manifests found"),
			"should report no manifests: {}",
			output_str
		);
	}

	#[test]
	fn test_manifests_no_validation_files() {
		let temp = tempfile::TempDir::new().unwrap();
		let empty_tests = temp.path().join("tests");
		std::fs::create_dir_all(&empty_tests).unwrap();

		let args = manifests::ManifestsArgs {
			export_dir: testdata_path("manifests_test/export")
				.to_string_lossy()
				.to_string(),
			recursive: false,
			tests_dir: empty_tests.to_string_lossy().to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok());
		assert!(
			output_str.contains("No validation files found"),
			"should report no validation files: {}",
			output_str
		);
	}

	#[test]
	fn test_manifests_ignores_libsonnet_and_test_files() {
		// Create a tests dir with a .libsonnet, a _test.jsonnet, and a valid .jsonnet
		let temp = tempfile::TempDir::new().unwrap();
		let tests_dir = temp.path().join("tests");
		std::fs::create_dir_all(&tests_dir).unwrap();

		// This should be picked up
		std::fs::write(
			tests_dir.join("check_labels.jsonnet"),
			"{\n  manifestTest(manifest)::\n    null,\n}\n",
		)
		.unwrap();
		// These should NOT be picked up
		std::fs::write(tests_dir.join("helper.libsonnet"), "{ helper: true }").unwrap();
		std::fs::write(tests_dir.join("check_labels_test.jsonnet"), "[]").unwrap();

		let args = manifests::ManifestsArgs {
			export_dir: testdata_path("manifests_test/export")
				.to_string_lossy()
				.to_string(),
			recursive: false,
			tests_dir: tests_dir.to_string_lossy().to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok(), "should pass: {}", output_str);
		// Should only find the 1 validation file
		assert!(
			output_str.contains("Found 1 validation files"),
			"should find exactly 1 validation file: {}",
			output_str
		);
	}

	#[test]
	fn test_manifests_kinds_filter() {
		// Create a tests dir with a validation file that has kinds: ["Deployment"]
		let temp = tempfile::TempDir::new().unwrap();
		let tests_dir = temp.path().join("tests");
		let export_dir = temp.path().join("export");
		std::fs::create_dir_all(&tests_dir).unwrap();
		std::fs::create_dir_all(&export_dir).unwrap();

		// Validation file that only applies to Deployments
		std::fs::write(
			tests_dir.join("check_replicas.jsonnet"),
			"{\n  kinds: std.set(['Deployment']),\n  manifestTest(manifest)::\n    if manifest.spec.replicas > 0 then null\n    else 'must have > 0 replicas',\n}\n",
		).unwrap();

		// A Deployment with 0 replicas (should fail)
		std::fs::write(
			export_dir.join("deployment.yaml"),
			"apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: bad-deploy\n  namespace: default\nspec:\n  replicas: 0\n",
		).unwrap();

		// A ConfigMap (should be skipped by kinds filter)
		std::fs::write(
			export_dir.join("configmap.yaml"),
			"apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: my-config\n  namespace: default\ndata:\n  key: value\n",
		).unwrap();

		let args = manifests::ManifestsArgs {
			export_dir: export_dir.to_string_lossy().to_string(),
			recursive: false,
			tests_dir: tests_dir.to_string_lossy().to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		// Should fail because Deployment has 0 replicas
		assert!(result.is_err(), "should fail: {}", output_str);
		assert!(
			output_str.contains("FAIL") && output_str.contains("deployment.yaml"),
			"should fail on deployment: {}",
			output_str
		);
		// Should have only 1 test run (the Deployment), not 2
		assert!(
			output_str.contains("1 failed, 1 total"),
			"should run only 1 manifestTest (Deployment only): {}",
			output_str
		);
	}

	#[test]
	fn test_manifests_kinds_filter_all_pass() {
		// kinds filter excludes the problematic manifests
		let temp = tempfile::TempDir::new().unwrap();
		let tests_dir = temp.path().join("tests");
		let export_dir = temp.path().join("export");
		std::fs::create_dir_all(&tests_dir).unwrap();
		std::fs::create_dir_all(&export_dir).unwrap();

		// Validation file that only applies to StatefulSets
		std::fs::write(
			tests_dir.join("check_statefulset.jsonnet"),
			"{\n  kinds: std.set(['StatefulSet']),\n  manifestTest(manifest)::\n    'always fails',\n}\n",
		).unwrap();

		// Only a ConfigMap in export (no StatefulSets)
		std::fs::write(
			export_dir.join("configmap.yaml"),
			"apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: my-config\n  namespace: default\ndata:\n  key: value\n",
		).unwrap();

		let args = manifests::ManifestsArgs {
			export_dir: export_dir.to_string_lossy().to_string(),
			recursive: false,
			tests_dir: tests_dir.to_string_lossy().to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		// No manifests match the kinds filter, so 0 tests run, all pass
		assert!(result.is_ok(), "should pass: {}", output_str);
		assert!(
			output_str.contains("0 failed"),
			"should have 0 failed: {}",
			output_str
		);
	}

	#[test]
	fn test_manifests_nonexistent_export_dir() {
		let args = manifests::ManifestsArgs {
			export_dir: "/nonexistent/dir".to_string(),
			recursive: false,
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: None,
		};

		let mut output = Vec::new();
		let result = manifests::run(args, &mut output);
		assert!(result.is_err());
		assert!(result.unwrap_err().to_string().contains("does not exist"));
	}
}

// ---------------------------------------------------------------------------
// validate lint
// ---------------------------------------------------------------------------
#[cfg(test)]
mod lint_tests {
	use super::*;

	#[test]
	fn test_lint_valid_files() {
		let args = lint::LintArgs {
			tests_dir: testdata_path("lint_test/valid")
				.to_string_lossy()
				.to_string(),
		};

		let mut output = Vec::new();
		let result = lint::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok(), "should pass: {}", output_str);
		assert!(
			output_str.contains("OK"),
			"should have OK entries: {}",
			output_str
		);
		assert!(
			output_str.contains("All 3 validation files are valid"),
			"should report all valid: {}",
			output_str
		);
	}

	#[test]
	fn test_lint_invalid_syntax() {
		let args = lint::LintArgs {
			tests_dir: testdata_path("lint_test/invalid")
				.to_string_lossy()
				.to_string(),
		};

		let mut output = Vec::new();
		let result = lint::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_err(), "should fail: {}", output_str);
		assert!(
			output_str.contains("ERR"),
			"should have ERR entries: {}",
			output_str
		);
		// syntax_error.jsonnet should fail with invalid Jsonnet
		assert!(
			output_str.contains("syntax_error.jsonnet"),
			"should mention syntax_error.jsonnet: {}",
			output_str
		);
	}

	#[test]
	fn test_lint_missing_functions() {
		let args = lint::LintArgs {
			tests_dir: testdata_path("lint_test/invalid")
				.to_string_lossy()
				.to_string(),
		};

		let mut output = Vec::new();
		let result = lint::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_err());
		// missing_functions.jsonnet should fail because it defines neither test function
		assert!(
			output_str.contains("missing_functions.jsonnet")
				&& output_str.contains("must define at least one"),
			"should mention missing functions: {}",
			output_str
		);
	}

	#[test]
	fn test_lint_not_a_function() {
		let args = lint::LintArgs {
			tests_dir: testdata_path("lint_test/invalid")
				.to_string_lossy()
				.to_string(),
		};

		let mut output = Vec::new();
		let result = lint::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_err());
		// not_a_function.jsonnet defines manifestTest as a string
		assert!(
			output_str.contains("not_a_function.jsonnet")
				&& output_str.contains("must be a function"),
			"should mention not a function: {}",
			output_str
		);
	}

	#[test]
	fn test_lint_bad_kinds() {
		let args = lint::LintArgs {
			tests_dir: testdata_path("lint_test/invalid")
				.to_string_lossy()
				.to_string(),
		};

		let mut output = Vec::new();
		let result = lint::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_err());
		assert!(
			output_str.contains("bad_kinds.jsonnet")
				&& output_str.contains("kinds must be an array of strings"),
			"should report bad kinds: {}",
			output_str
		);
	}

	#[test]
	fn test_lint_empty_dir() {
		let temp = tempfile::TempDir::new().unwrap();

		let args = lint::LintArgs {
			tests_dir: temp.path().to_string_lossy().to_string(),
		};

		let mut output = Vec::new();
		let result = lint::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok());
		assert!(
			output_str.contains("No validation files found"),
			"should report no files: {}",
			output_str
		);
	}

	#[test]
	fn test_lint_nonexistent_dir() {
		let args = lint::LintArgs {
			tests_dir: "/nonexistent/dir".to_string(),
		};

		let mut output = Vec::new();
		let result = lint::run(args, &mut output);
		assert!(result.is_err());
		assert!(result.unwrap_err().to_string().contains("does not exist"));
	}
}

// ---------------------------------------------------------------------------
// validate test
// ---------------------------------------------------------------------------
#[cfg(test)]
mod test_runner_tests {
	use super::*;

	#[test]
	fn test_runner_all_pass() {
		let args = validate_test::TestArgs {
			tests_dir: testdata_path("test_runner").to_string_lossy().to_string(),
		};

		let mut output = Vec::new();
		let result = validate_test::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok(), "all tests should pass: {}", output_str);
		assert!(
			output_str.contains("PASS"),
			"should have PASS entries: {}",
			output_str
		);
		assert!(
			output_str.contains("0 failed"),
			"should have 0 failed: {}",
			output_str
		);
	}

	#[test]
	fn test_runner_with_failures() {
		// Create a test with a wrong expected error to force a failure
		let temp = tempfile::TempDir::new().unwrap();
		let tests_dir = temp.path();

		// Validation file
		std::fs::write(
			tests_dir.join("check.jsonnet"),
			"{\n  manifestTest(manifest)::\n    if std.objectHas(manifest.metadata, 'labels') then null\n    else 'missing labels',\n}\n",
		).unwrap();

		// Test file with a wrong expectation
		std::fs::write(
			tests_dir.join("check_test.jsonnet"),
			r#"[
  {
    name: "should pass but expects error",
    input: { apiVersion: "v1", kind: "ConfigMap", metadata: { name: "t", labels: { app: "x" } } },
    testType: "manifestTest",
    expectedError: "this error should not appear",
  },
]
"#,
		)
		.unwrap();

		let args = validate_test::TestArgs {
			tests_dir: tests_dir.to_string_lossy().to_string(),
		};

		let mut output = Vec::new();
		let result = validate_test::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_err(), "should fail: {}", output_str);
		assert!(
			output_str.contains("FAIL"),
			"should have FAIL entries: {}",
			output_str
		);
		assert!(
			output_str.contains("expected error"),
			"should explain the mismatch: {}",
			output_str
		);
	}

	#[test]
	fn test_runner_no_test_files() {
		let temp = tempfile::TempDir::new().unwrap();

		let args = validate_test::TestArgs {
			tests_dir: temp.path().to_string_lossy().to_string(),
		};

		let mut output = Vec::new();
		let result = validate_test::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok());
		assert!(
			output_str.contains("No test files"),
			"should report no test files: {}",
			output_str
		);
	}

	#[test]
	fn test_runner_missing_validation_file() {
		// _test.jsonnet without corresponding .jsonnet
		let temp = tempfile::TempDir::new().unwrap();
		std::fs::write(temp.path().join("orphan_test.jsonnet"), "[]").unwrap();

		let args = validate_test::TestArgs {
			tests_dir: temp.path().to_string_lossy().to_string(),
		};

		let mut output = Vec::new();
		let result = validate_test::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(
			result.is_ok(),
			"should not error, just skip: {}",
			output_str
		);
		assert!(
			output_str.contains("SKIP") && output_str.contains("no corresponding validation file"),
			"should skip with message: {}",
			output_str
		);
	}

	#[test]
	fn test_runner_nonexistent_dir() {
		let args = validate_test::TestArgs {
			tests_dir: "/nonexistent/dir".to_string(),
		};

		let mut output = Vec::new();
		let result = validate_test::run(args, &mut output);
		assert!(result.is_err());
		assert!(result.unwrap_err().to_string().contains("does not exist"));
	}

	#[test]
	fn test_runner_namespace_test_cases() {
		// Specifically test namespaceTest test cases
		let args = validate_test::TestArgs {
			tests_dir: testdata_path("test_runner").to_string_lossy().to_string(),
		};

		let mut output = Vec::new();
		let result = validate_test::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok(), "all tests should pass: {}", output_str);
		// Should see results for both the labels and namespace test files
		assert!(
			output_str.contains("check_labels_test.jsonnet"),
			"should run label tests: {}",
			output_str
		);
		assert!(
			output_str.contains("check_namespace_test.jsonnet"),
			"should run namespace tests: {}",
			output_str
		);
	}
}

// ---------------------------------------------------------------------------
// validate environments
// ---------------------------------------------------------------------------
#[cfg(test)]
mod environments_tests {
	use super::*;
	use rtk::jsonnet::evaluator::EvaluatorImplementation;

	fn default_jsonnet_args() -> rtk::commands::JsonnetArgs {
		rtk::commands::JsonnetArgs {
			ext_code: vec![],
			ext_str: vec![],
			implementation: EvaluatorImplementation::default(),
			max_stack: 500,
			tla_code: vec![],
			tla_str: vec![],
		}
	}

	fn export_testdata_path(subpath: &str) -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("testdata")
			.join(subpath)
	}

	#[test]
	fn test_environments_exports_and_validates() {
		let temp = tempfile::TempDir::new().unwrap();
		fs::write(temp.path().join("jsonnetfile.json"), "{}").unwrap();
		let env_dir = temp.path().join("env");
		fs::create_dir_all(&env_dir).unwrap();
		fs::write(
			env_dir.join("spec.json"),
			r#"{
  "apiVersion": "tanka.dev/v1alpha1",
  "kind": "Environment",
  "metadata": { "name": "test" },
  "spec": { "namespace": "default" }
}"#,
		)
		.unwrap();
		fs::write(
			env_dir.join("main.jsonnet"),
			r#"{
  cm: {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: { name: 'ok', namespace: 'default', labels: { app: 'x' } },
  },
}"#,
		)
		.unwrap();

		let args = environments::EnvironmentsArgs {
			environments: vec![env_dir],
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: None,
			target: vec![],
			jsonnet: default_jsonnet_args(),
		};

		let mut output = Vec::new();
		let result = environments::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(result.is_ok(), "should pass: {}", output_str);
		assert!(
			output_str.contains("Exporting 1 environment(s) in memory"),
			"should report in-memory export: {}",
			output_str
		);
		assert!(
			output_str.contains("0 failed"),
			"should have no failures: {}",
			output_str
		);
	}

	#[test]
	fn test_environments_warns_on_namespace_test() {
		let temp = tempfile::TempDir::new().unwrap();
		fs::write(temp.path().join("jsonnetfile.json"), "{}").unwrap();
		let env_dir = temp.path().join("env");
		fs::create_dir_all(&env_dir).unwrap();
		fs::write(
			env_dir.join("spec.json"),
			r#"{
  "apiVersion": "tanka.dev/v1alpha1",
  "kind": "Environment",
  "metadata": { "name": "test" },
  "spec": { "namespace": "default" }
}"#,
		)
		.unwrap();
		fs::write(
			env_dir.join("main.jsonnet"),
			r#"{
  cm: {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: { name: 'ok', namespace: 'default' },
  },
}"#,
		)
		.unwrap();

		let args = environments::EnvironmentsArgs {
			environments: vec![env_dir],
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: None,
			target: vec![],
			jsonnet: default_jsonnet_args(),
		};

		let mut output = Vec::new();
		let _ = environments::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		assert!(
			output_str.contains("namespaceTest")
				&& output_str.contains("rtk export")
				&& output_str.contains("rtk validate manifests"),
			"should warn about namespaceTest accuracy: {}",
			output_str
		);
	}

	#[test]
	fn test_environments_real_fixture() {
		let mut jsonnet = default_jsonnet_args();
		jsonnet.ext_str = vec![
			("deploymentName".into(), "dep".into()),
			("serviceName".into(), "svc".into()),
		];
		let args = environments::EnvironmentsArgs {
			environments: vec![export_testdata_path("test-export-envs/static-env")],
			tests_dir: testdata_path("manifests_test/validations")
				.to_string_lossy()
				.to_string(),
			log_slowest: None,
			target: vec![],
			jsonnet,
		};

		let mut output = Vec::new();
		let result = environments::run(args, &mut output);
		let output_str = String::from_utf8(output).unwrap();

		// static-env Deployment/Service lack labels; check_labels should fail
		assert!(
			result.is_err(),
			"should fail without labels: {}",
			output_str
		);
		assert!(
			output_str.contains("missing labels"),
			"should report label failure: {}",
			output_str
		);
	}
}
