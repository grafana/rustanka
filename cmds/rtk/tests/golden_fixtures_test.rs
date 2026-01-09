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
			// Only show changed lines with line numbers
			for (idx, group) in diff.grouped_ops(3).iter().enumerate() {
				if idx > 0 {
					diff_output.push_str("...\n");
				}
				for op in group {
					for change in diff.iter_changes(op) {
						let (sign, line_num) = match change.tag() {
							ChangeTag::Delete => ("-", change.old_index().map(|i| i + 1)),
							ChangeTag::Insert => ("+", change.new_index().map(|i| i + 1)),
							ChangeTag::Equal => continue, // Skip unchanged lines
						};
						let line_str = line_num.map(|n| format!("{:>5}", n)).unwrap_or_default();
						diff_output.push_str(&format!("{} {}| {}", sign, line_str, change));
					}
				}
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
fn test_yaml_output_env_jrsonnet_export_matches_golden() {
	run_golden_test(
		"golden_envs/yaml_output_env_jrsonnet",
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

/// Test case for object key ordering differences between tk and rtk
/// This reproduces the issue where object keys with numeric strings sort
/// differently: tk sorts as strings ("67" > "100"), rtk may sort numerically
/// This affects:
/// - Command-line arguments built from object fields
/// - Config hashes (since underlying content order differs)
#[test]
fn test_object_ordering_env_export_matches_golden() {
	run_golden_test(
		"golden_envs/object_ordering_env",
		"{{.metadata.namespace}}/{{.metadata.name}}",
	);
}

/// Test case for empty/null field handling differences between tk and rtk
/// This reproduces issues where:
/// - PodDisruptionBudget matchLabels: tk has {name: x}, rtk has {}
/// - Ingress annotations may be empty vs populated differently
/// - Service selectors may be missing fields
#[test]
fn test_empty_fields_env_export_matches_golden() {
	run_golden_test(
		"golden_envs/empty_fields_env",
		"{{.metadata.namespace}}/{{.metadata.name}}",
	);
}

/// Test case for multiline string wrapping differences between tk and rtk
/// This reproduces issues where:
/// - Long shell commands break at different positions (e.g., before |)
/// - ScaledObject PromQL queries have different line breaks
/// - Long command-line args wrap differently in YAML output
#[test]
fn test_multiline_strings_env_export_matches_golden() {
	run_golden_test(
		"golden_envs/multiline_strings_env",
		"{{.metadata.namespace}}/{{.metadata.name}}",
	);
}

/// Test case for conditional evaluation and null handling differences
/// This reproduces issues where:
/// - Resources get "--no-value-" in filename when metadata.name evaluates to null/empty
/// - PodDisruptionBudget matchLabels are empty when they should have values
/// - Service selector/ports are missing when they should be present
/// These issues suggest differences in how conditionals or null values are evaluated
#[test]
fn test_conditional_eval_env_export_matches_golden() {
	run_golden_test(
		"golden_envs/conditional_eval_env",
		"{{.metadata.namespace}}/{{.metadata.name}}",
	);
}

/// Test case for YAML line wrapping at specific character positions
/// This reproduces issues where:
/// - Shell commands like 'du -sh /data/wal/ | cut' wrap before '|' in rtk but after space in tk
/// - PromQL queries have ') * 100' on different lines between tk and rtk
/// - Complex nested structures wrap at different points
#[test]
fn test_yaml_line_wrapping_env_export_matches_golden() {
	run_golden_test(
		"golden_envs/yaml_line_wrapping_env",
		"{{.metadata.namespace}}/{{.metadata.name}}",
	);
}

/// Test case for conditional config generation patterns
/// This reproduces issues where ConfigMap data is empty in rtk but populated in tk
/// Tests various patterns:
/// - Hidden field (::) exposure and access
/// - Conditional object field inclusion
/// - Self-referential config with hidden fields
/// - Mixin patterns common in Grafana jsonnet
/// - Object merging with hidden fields (like the -gf Loki configs)
#[test]
fn test_conditional_config_env_export_matches_golden() {
	run_golden_test(
		"golden_envs/conditional_config_env",
		"{{.metadata.namespace}}/{{.metadata.name}}",
	);
}

/// Test case for eager error evaluation in nested std.mergePatch calls
/// This reproduces the issue where rtk evaluates error statements too eagerly
/// when using nested std.mergePatch patterns. The exact pattern is:
/// 1. thor-query-engine.libsonnet defines: loki.querier.storage_start: error '...'
/// 2. loki-overrides.libsonnet does: querier: std.mergePatch(super.querier + {...}, {...})
///    but does NOT null out the error field
/// 3. dev-overrides.libsonnet does: loki:: std.mergePatch(super.loki + {...}, {...})
/// 4. global-release-configs.libsonnet nulls the error field, but AFTER dev-overrides
///
/// tk lazily evaluates and only triggers the error if the field is accessed.
/// rtk eagerly evaluates during std.mergePatch, causing the error to trigger.
#[test]
fn test_eager_error_eval_env_export_matches_golden() {
	run_golden_test(
		"golden_envs/eager_error_eval_env",
		"{{.metadata.namespace}}/{{.metadata.name}}",
	);
}
