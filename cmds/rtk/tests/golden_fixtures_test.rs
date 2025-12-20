use rtk::discover::find_environments;
use rtk::eval::EvalOpts;
use rtk::export::{export, ExportOpts};
use similar::{ChangeTag, TextDiff};
use std::fs;
use std::path::{Path, PathBuf};

/// Helper function to get absolute path to test_fixtures
fn fixtures_path(subpath: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.unwrap()
		.parent()
		.unwrap()
		.join("test_fixtures")
		.join(subpath)
}

/// Recursively collect all files in a directory with their relative paths
fn collect_files(dir: &Path) -> std::collections::HashMap<String, String> {
	let mut files = std::collections::HashMap::new();
	if !dir.exists() {
		return files;
	}
	for entry in walkdir::WalkDir::new(dir) {
		let entry = entry.unwrap();
		if entry.file_type().is_file() {
			let rel_path = entry
				.path()
				.strip_prefix(dir)
				.unwrap()
				.to_string_lossy()
				.to_string();
			let content = fs::read_to_string(entry.path()).unwrap();
			files.insert(rel_path, content);
		}
	}
	files
}

/// Run a golden test comparing rtk export output against tk-generated golden files
fn run_golden_test(fixture_name: &str, format: &str) {
	let temp_dir = tempfile::TempDir::new().unwrap();
	let output_dir = temp_dir.path();

	let env_path = fixtures_path(fixture_name);
	let golden_dir = env_path.join("golden");

	assert!(
		golden_dir.exists(),
		"Golden directory does not exist at {:?}. Run 'make update-golden-fixtures' to generate it.",
		golden_dir
	);

	let envs = find_environments(&[env_path.to_string_lossy().to_string()]).unwrap();
	assert_eq!(envs.len(), 1, "Should find exactly 1 environment");

	let opts = ExportOpts {
		output_dir: output_dir.to_path_buf(),
		extension: "golden".to_string(),
		format: format.to_string(),
		parallelism: 1,
		eval_opts: EvalOpts::default(),
		name: None,
		recursive: false,
		skip_manifest: true,
		..Default::default()
	};

	let result = export(&[env_path.to_string_lossy().to_string()], opts).unwrap();

	assert_eq!(result.successful, 1, "Should export 1 environment");
	assert_eq!(result.failed, 0, "Should have no failures");

	let golden_files: std::collections::HashMap<_, _> = collect_files(&golden_dir)
		.into_iter()
		.filter(|(k, _)| k != "manifest.json")
		.collect();
	let output_files = collect_files(output_dir);

	let golden_keys: std::collections::HashSet<_> = golden_files.keys().collect();
	let output_keys: std::collections::HashSet<_> = output_files.keys().collect();

	assert_eq!(
		golden_keys, output_keys,
		"File sets should match.\nGolden: {:?}\nOutput: {:?}",
		golden_keys, output_keys
	);

	let mut all_failures = Vec::new();
	let mut sorted_paths: Vec<_> = golden_files.keys().collect();
	sorted_paths.sort();

	for path in sorted_paths {
		let golden_content = golden_files.get(path).unwrap();
		let output_content = output_files.get(path).unwrap();
		if golden_content != output_content {
			let diff = TextDiff::from_lines(golden_content, output_content);
			let mut diff_output = String::new();
			for change in diff.iter_all_changes() {
				let sign = match change.tag() {
					ChangeTag::Delete => "-",
					ChangeTag::Insert => "+",
					ChangeTag::Equal => " ",
				};
				diff_output.push_str(&format!("{}{}", sign, change));
			}
			all_failures.push(format!(
				"=== {} ===\n--- golden (expected)\n+++ output (actual)\n\n{}",
				path, diff_output
			));
		}
	}

	if !all_failures.is_empty() {
		panic!(
			"Content mismatch for {} file(s):\n\n{}",
			all_failures.len(),
			all_failures.join("\n\n")
		);
	}
}

#[test]
fn test_yaml_output_env_export_matches_golden() {
	run_golden_test(
		"golden_envs/yaml_output_env",
		"{{.metadata.namespace}}/{{.metadata.name}}",
	);
}

#[test]
fn test_static_exporter_env_export_matches_golden() {
	run_golden_test(
		"golden_envs/static_exporter_env",
		"{{.metadata.namespace}}/{{.metadata.name}}",
	);
}

/// Test case for nested YAML block scalar indentation/chomping
/// This reproduces the issue where rtk uses different block scalar formatting
/// than tk (Go's yaml.v3), specifically:
/// - rtk uses `|` (keep final newline) while tk uses `|-` (strip final newline)
/// - This affects ConfigMaps with nested YAML content like queries.yaml
#[test]
fn test_nested_block_scalar_env_export_matches_golden() {
	run_golden_test(
		"golden_envs/nested_block_scalar_env",
		"{{.metadata.namespace}}/{{.metadata.name}}",
	);
}
