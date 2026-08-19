use std::{fs, path::PathBuf};

use rtk_jsonnet::importers::{ImporterIndex, TargetFile};

fn root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/findImporters")
}

fn abs_path(path: &str) -> PathBuf {
	root().join(path).canonicalize().unwrap()
}

fn find_importers(targets: &[TargetFile]) -> Vec<PathBuf> {
	ImporterIndex::build(root())
		.unwrap()
		.find_importers(targets)
		.unwrap()
}

fn existing(path: &str) -> TargetFile {
	TargetFile::Existing(abs_path(path))
}

fn deleted(path: &str) -> TargetFile {
	format!("deleted:{path}").parse().unwrap()
}

/// Sort expectations the same way find_importers sorts its results.
fn sorted(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
	paths.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
	paths
}

#[test]
fn test_no_files() {
	assert_eq!(find_importers(&[]), Vec::<PathBuf>::new());
}

#[test]
fn indexes_files_in_directories_that_also_have_subdirectories() {
	let root = tempfile::tempdir().unwrap();
	fs::write(root.path().join("jsonnetfile.json"), "{}").unwrap();
	fs::write(root.path().join("common.libsonnet"), "{}").unwrap();
	fs::create_dir_all(root.path().join("environment/nested")).unwrap();
	fs::write(
		root.path().join("environment/main.jsonnet"),
		"import '../common.libsonnet'",
	)
	.unwrap();

	let index = ImporterIndex::build(root.path()).unwrap();
	let importers = index
		.find_importers(&[TargetFile::Existing(root.path().join("common.libsonnet"))])
		.unwrap();
	assert_eq!(
		importers,
		vec![root.path().join("environment/main.jsonnet")]
	);
}

#[test]
fn test_invalid_file() {
	let invalid_file = root().join("does-not-exist.jsonnet");
	let result = ImporterIndex::build(root())
		.unwrap()
		.find_importers(&[TargetFile::Existing(invalid_file)]);
	assert!(result.is_err());
	assert!(result.unwrap_err().to_string().contains("does not exist"));
}

#[test]
fn test_target_file_parsing() {
	assert_eq!(
		"deleted:foo/bar.jsonnet".parse::<TargetFile>().unwrap(),
		TargetFile::Deleted(PathBuf::from("foo/bar.jsonnet"))
	);
	assert_eq!(
		"foo/bar.jsonnet".parse::<TargetFile>().unwrap(),
		TargetFile::Existing(PathBuf::from("foo/bar.jsonnet"))
	);
}

#[test]
fn test_project_with_no_imports() {
	let file = "environments/no-imports/main.jsonnet";
	let result = find_importers(&[existing(file)]);
	assert_eq!(result, vec![abs_path(file)]); // itself only
}

#[test]
fn test_local_import() {
	let result = find_importers(&[existing(
		"environments/imports-locals-and-vendored/local-file1.libsonnet",
	)]);
	assert_eq!(
		result,
		vec![abs_path(
			"environments/imports-locals-and-vendored/main.jsonnet"
		)]
	);
}

#[test]
fn test_local_import_with_relative_path() {
	let result = find_importers(&[existing(
		"environments/imports-locals-and-vendored/local-file2.libsonnet",
	)]);
	assert_eq!(
		result,
		vec![abs_path(
			"environments/imports-locals-and-vendored/main.jsonnet"
		)]
	);
}

#[test]
fn test_lib_imported_through_chain() {
	let result = find_importers(&[existing("lib/lib1/main.libsonnet")]);
	assert_eq!(
		result,
		vec![abs_path(
			"environments/imports-lib-and-vendored-through-chain/main.jsonnet"
		)]
	);
}

#[test]
fn test_vendored_lib_imported_through_chain_and_directly() {
	let result = find_importers(&[existing("vendor/vendored/main.libsonnet")]);
	let expected = sorted(vec![
		abs_path("environments/imports-lib-and-vendored-through-chain/main.jsonnet"),
		abs_path("environments/imports-locals-and-vendored/main.jsonnet"),
		abs_path("environments/imports-symlinked-vendor/main.jsonnet"),
	]);
	assert_eq!(result, expected);
}

#[test]
fn test_vendored_lib_found_through_symlink() {
	let result = find_importers(&[existing("vendor/vendor-symlinked/main.libsonnet")]);
	let expected = sorted(vec![
		abs_path("environments/imports-lib-and-vendored-through-chain/main.jsonnet"),
		abs_path("environments/imports-locals-and-vendored/main.jsonnet"),
		abs_path("environments/imports-symlinked-vendor/main.jsonnet"),
	]);
	assert_eq!(result, expected);
}

#[test]
fn test_text_file() {
	let result = find_importers(&[existing("vendor/vendored/text-file.txt")]);
	let expected = sorted(vec![
		abs_path("environments/imports-lib-and-vendored-through-chain/main.jsonnet"),
		abs_path("environments/imports-locals-and-vendored/main.jsonnet"),
		abs_path("environments/imports-symlinked-vendor/main.jsonnet"),
	]);
	assert_eq!(result, expected);
}

#[test]
fn test_relative_imported_environment() {
	let result = find_importers(&[existing("environments/relative-imported/main.jsonnet")]);
	let expected = sorted(vec![
		abs_path("environments/relative-import/main.jsonnet"),
		abs_path("environments/relative-imported/main.jsonnet"), // itself, it's a main file
	]);
	assert_eq!(result, expected);
}

#[test]
fn test_relative_imported_environment_with_doubled_dotdot() {
	let result = find_importers(&[existing("environments/relative-imported2/main.jsonnet")]);
	let expected = sorted(vec![
		abs_path("environments/relative-import/main.jsonnet"),
		abs_path("environments/relative-imported2/main.jsonnet"), // itself, it's a main file
	]);
	assert_eq!(result, expected);
}

#[test]
fn test_relative_imported_text_file() {
	let result = find_importers(&[existing("other-files/test.txt")]);
	assert_eq!(
		result,
		vec![abs_path("environments/relative-import/main.jsonnet")]
	);
}

#[test]
fn test_relative_imported_text_file_with_doubled_dotdot() {
	let result = find_importers(&[existing("other-files/test2.txt")]);
	assert_eq!(
		result,
		vec![abs_path("environments/relative-import/main.jsonnet")]
	);
}

#[test]
fn test_vendor_override_in_env_override_vendor_used() {
	let result = find_importers(&[existing(
		"environments/vendor-override-in-env/vendor/vendor-override-in-env/main.libsonnet",
	)]);
	assert_eq!(
		result,
		vec![abs_path("environments/vendor-override-in-env/main.jsonnet")]
	);
}

#[test]
fn test_vendor_override_in_env_global_vendor_unused() {
	let result = find_importers(&[existing("vendor/vendor-override-in-env/main.libsonnet")]);
	assert_eq!(result, Vec::<PathBuf>::new());
}

#[test]
fn test_imported_file_in_lib_relative_to_env() {
	let result = find_importers(&[existing(
		"environments/lib-import-relative-to-env/file-to-import.libsonnet",
	)]);
	assert_eq!(
		result,
		vec![abs_path(
			"environments/lib-import-relative-to-env/folder1/folder2/main.jsonnet"
		)]
	);
}

#[test]
fn test_unused_deleted_file() {
	let result = find_importers(&[deleted("vendor/deleted-vendor/main.libsonnet")]);
	assert_eq!(result, Vec::<PathBuf>::new());
}

#[test]
fn test_deleted_local_path_that_is_still_potentially_imported() {
	let result = find_importers(&[deleted(
		"environments/using-deleted-stuff/my-import-dir/main.libsonnet",
	)]);
	assert_eq!(
		result,
		vec![abs_path("environments/using-deleted-stuff/main.jsonnet")]
	);
}

#[test]
fn test_deleted_lib_that_is_still_potentially_imported() {
	let result = find_importers(&[deleted("lib/my-import-dir/main.libsonnet")]);
	assert_eq!(
		result,
		vec![abs_path("environments/using-deleted-stuff/main.jsonnet")]
	);
}

#[test]
fn test_deleted_vendor_that_is_still_potentially_imported() {
	let result = find_importers(&[deleted("vendor/my-import-dir/main.libsonnet")]);
	assert_eq!(
		result,
		vec![abs_path("environments/using-deleted-stuff/main.jsonnet")]
	);
}

#[test]
fn test_deleted_dir_in_environment() {
	let result = find_importers(&[deleted(
		"environments/no-imports/deleted-dir/deleted-file.libsonnet",
	)]);
	assert_eq!(
		result,
		vec![abs_path("environments/no-imports/main.jsonnet")]
	);
}

#[test]
fn test_imports_through_a_main_file_are_followed() {
	let result = find_importers(&[existing(
		"environments/import-other-main-file/env2/file.libsonnet",
	)]);
	let expected = sorted(vec![
		abs_path("environments/import-other-main-file/env1/main.jsonnet"),
		abs_path("environments/import-other-main-file/env2/main.jsonnet"),
	]);
	assert_eq!(result, expected);
}

#[test]
fn test_lib_file_imports_environment_file() {
	let result = find_importers(&[existing(
		"environments/lib-imports-environment/config.jsonnet",
	)]);
	let expected = sorted(vec![
		abs_path("environments/lib-imports-environment/main.jsonnet"),
		abs_path("environments/uses-lib-that-imports-env/main.jsonnet"),
	]);
	assert_eq!(result, expected);
}

#[test]
fn test_complex_transitive_chain_env1_lib1_env2_lib3_env3() {
	let result = find_importers(&[existing("environments/chain-env1/config.jsonnet")]);
	let expected = sorted(vec![
		abs_path("environments/chain-env1/main.jsonnet"), // direct env importer
		abs_path("environments/chain-env2/main.jsonnet"), // via lib1
		abs_path("environments/chain-env3/main.jsonnet"), // via lib1->env2->lib3
	]);
	assert_eq!(result, expected);
}

#[test]
fn test_relative_import_from_lib_to_env_should_not_match_as_lib_vendor() {
	// Search for an environment file that is imported by a lib file using a relative path
	// starting with ../. The lib file should NOT be found as an importer because relative
	// imports starting with ../ should only match via the relative import check, not the
	// lib/vendor check.
	//
	// Without the ../-guard on the lib/vendor check, lib/internal-alerting/main.libsonnet
	// is incorrectly matched as an importer (lib.join("../environments/...") resolves back
	// into environments/). Even though non-main files are filtered from the final result,
	// the incorrect match propagates transitively: environments importing that lib file
	// (like test-env-imports-lib) would be incorrectly included.
	let result = find_importers(&[existing("environments/relative-import-target/main.jsonnet")]);
	let expected = vec![
		abs_path("environments/relative-import-target/main.jsonnet"), // itself, it's a main file
	];
	let incorrectly_included =
		result.contains(&abs_path("environments/test-env-imports-lib/main.jsonnet"));
	assert!(
		!incorrectly_included,
		"Environment importing lib file with relative import to ../environments/ should NOT be included. Result: {result:?}"
	);
	assert_eq!(
		result, expected,
		"lib file with relative import starting with ../ should not match via lib/vendor check"
	);
}

#[test]
fn test_helm_chart_values_file_finds_environment() {
	let result = find_importers(&[existing(
		"environments/uses-helm-chart/charts/my-chart/values.yaml",
	)]);
	assert_eq!(
		result,
		vec![abs_path("environments/uses-helm-chart/main.jsonnet")]
	);
}

#[test]
fn test_helm_chart_template_file_finds_environment() {
	let result = find_importers(&[existing(
		"environments/uses-helm-chart/charts/my-chart/templates/deployment.yaml",
	)]);
	assert_eq!(
		result,
		vec![abs_path("environments/uses-helm-chart/main.jsonnet")]
	);
}

#[test]
fn test_kustomize_file_finds_environment() {
	let result = find_importers(&[existing(
		"environments/uses-kustomize/kustomize/deployment.yaml",
	)]);
	assert_eq!(
		result,
		vec![abs_path("environments/uses-kustomize/main.jsonnet")]
	);
}

#[test]
fn test_helm_chart_dynamic_version_finds_environment() {
	let result = find_importers(&[existing(
		"environments/uses-helm-chart-dynamic/charts/my-dynamic-chart-1.0.0/values.yaml",
	)]);
	assert_eq!(
		result,
		vec![abs_path(
			"environments/uses-helm-chart-dynamic/main.jsonnet"
		)]
	);
}

// I have no idea why trufflehog is this dense
#[test]
fn test_helm_chart_readme_in_lib_is_ignored() { // trufflehog:ignore
	// README.md inside a chart directory in lib/ should NOT trigger importers,
	// because .md files are not relevant to Helm template output.
	let result = find_importers(&[existing("lib/helm-chart-lib/charts/my-lib-chart/README.md")]);
	assert_eq!(result, Vec::<PathBuf>::new());
}

#[test]
fn test_helm_chart_yaml_in_lib_finds_environment() {
	// values.yaml inside a chart directory in lib/ SHOULD trigger importers.
	let result = find_importers(&[existing(
		"lib/helm-chart-lib/charts/my-lib-chart/values.yaml",
	)]);
	assert_eq!(
		result,
		vec![abs_path("environments/uses-helm-chart-in-lib/main.jsonnet")]
	);
}

#[test]
fn test_helm_chart_non_yaml_file_finds_environment() {
	// Non-yaml files (e.g., .txt configs embedded in configmaps) should also trigger importers.
	let result = find_importers(&[existing(
		"environments/uses-helm-chart/charts/my-chart/config.txt",
	)]);
	assert_eq!(
		result,
		vec![abs_path("environments/uses-helm-chart/main.jsonnet")]
	);
}
