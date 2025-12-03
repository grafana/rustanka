use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

/// Check if a file path indicates a YAML file
fn is_yaml_file(path: &str) -> bool {
	path.ends_with(".yaml") || path.ends_with(".yml")
}

/// Check if a key should be ignored during semantic comparison
/// These are computed hashes that differ due to formatting differences
fn is_ignored_key(key: &str) -> bool {
	key.ends_with("-hash")
		|| key.ends_with("_hash")
		|| key == "config_hash"
		|| key == "tanka.dev/environment" // Tanka environment hash label
}

/// Normalize a floating point number to handle precision differences
/// e.g., 0.10000000000000001 -> 0.1
fn normalize_float(f: f64) -> f64 {
	// Round to 10 decimal places to handle floating point precision issues
	(f * 1e10).round() / 1e10
}

/// Normalize a string value for comparison
/// - Trims trailing newlines (handles \n\n vs \n differences)
/// - Normalizes floating point precision in string values (e.g., "0.10000000000000001" -> "0.1")
fn normalize_string(s: &str) -> String {
	let trimmed = s.trim_end_matches('\n');

	// Check if this looks like a command-line arg with a float (e.g., "-rate=0.10000000000000001")
	if let Some(eq_pos) = trimmed.find('=') {
		let (prefix, value) = trimmed.split_at(eq_pos + 1);
		if let Ok(f) = value.parse::<f64>() {
			let normalized = normalize_float(f);
			// Format without unnecessary precision
			if normalized.fract() == 0.0 {
				return format!("{}{}", prefix, normalized as i64);
			}
			return format!("{}{}", prefix, normalized);
		}
	}

	// Check if the entire string is a float
	if let Ok(f) = trimmed.parse::<f64>() {
		let normalized = normalize_float(f);
		if normalized.fract() == 0.0 {
			return (normalized as i64).to_string();
		}
		return normalized.to_string();
	}

	trimmed.to_string()
}

/// Normalize a YAML value for semantic comparison
/// - Removes ignored keys (config_hash, etc.)
/// - Normalizes floating point precision
/// - Normalizes trailing newlines in strings
fn normalize_yaml_value(value: serde_yaml::Value) -> serde_yaml::Value {
	use serde_yaml::Value;

	match value {
		Value::Mapping(map) => {
			let normalized: serde_yaml::Mapping = map
				.into_iter()
				.filter(|(k, _)| {
					// Filter out ignored keys
					if let Value::String(key) = k {
						!is_ignored_key(key)
					} else {
						true
					}
				})
				.map(|(k, v)| (normalize_yaml_value(k), normalize_yaml_value(v)))
				.collect();
			Value::Mapping(normalized)
		}
		Value::Sequence(seq) => {
			Value::Sequence(seq.into_iter().map(normalize_yaml_value).collect())
		}
		Value::Number(n) => {
			if let Some(f) = n.as_f64() {
				// Normalize floating point precision
				let normalized = normalize_float(f);
				// Check if it's actually an integer
				if normalized.fract() == 0.0 && normalized.abs() < i64::MAX as f64 {
					Value::Number(serde_yaml::Number::from(normalized as i64))
				} else {
					Value::Number(serde_yaml::Number::from(normalized))
				}
			} else {
				Value::Number(n)
			}
		}
		Value::String(s) => {
			// Normalize trailing newlines
			Value::String(normalize_string(&s))
		}
		other => other,
	}
}

/// Compare two YAML strings semantically, handling multi-document YAML
/// Returns true if semantically equivalent, false otherwise
///
/// This comparison ignores:
/// - config_hash and similar computed hash fields
/// - Floating point precision differences (0.1 vs 0.10000000000000001)
/// - Trailing newline differences in strings
pub fn compare_yaml_docs_semantically(yaml1: &str, yaml2: &str) -> Result<bool> {
	// Handle multi-document YAML (separated by ---)
	let docs1: Result<Vec<serde_yaml::Value>, _> = serde_yaml::Deserializer::from_str(yaml1)
		.map(|d| serde_yaml::Value::deserialize(d))
		.collect();
	let docs2: Result<Vec<serde_yaml::Value>, _> = serde_yaml::Deserializer::from_str(yaml2)
		.map(|d| serde_yaml::Value::deserialize(d))
		.collect();

	match (docs1, docs2) {
		(Ok(v1), Ok(v2)) => {
			// Normalize both document lists before comparison
			let normalized1: Vec<_> = v1.into_iter().map(normalize_yaml_value).collect();
			let normalized2: Vec<_> = v2.into_iter().map(normalize_yaml_value).collect();
			Ok(normalized1 == normalized2)
		}
		(Err(e), _) => Err(anyhow::anyhow!("Failed to parse first YAML: {}", e)),
		(_, Err(e)) => Err(anyhow::anyhow!("Failed to parse second YAML: {}", e)),
	}
}

#[derive(Debug)]
pub struct RunResult {
	pub exit_code: i32,
	pub stdout: String,
	pub stderr: String,
	pub duration: Duration,
}

pub fn run_command(
	executable: &str,
	args: &[String],
	workspace_dir: Option<&str>,
	working_dir: Option<&str>,
) -> Result<RunResult> {
	let start = Instant::now();

	let mut cmd = ProcessCommand::new(executable);
	cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

	// Determine the actual working directory
	let actual_working_dir = match (workspace_dir, working_dir) {
		(Some(ws), Some(wd)) => {
			// If both are specified, combine them: workspace_dir/working_dir
			let combined = format!("{}/{}", ws, wd);
			std::fs::create_dir_all(&combined)?;
			Some(combined)
		}
		(Some(ws), None) => {
			// Only workspace directory
			std::fs::create_dir_all(ws)?;
			Some(ws.to_string())
		}
		(None, Some(wd)) => {
			// Only working directory (no workspace isolation)
			Some(wd.to_string())
		}
		(None, None) => None,
	};

	if let Some(dir) = &actual_working_dir {
		cmd.current_dir(dir);
	}

	let output = cmd
		.output()
		.with_context(|| format!("Failed to execute command: {} {:?}", executable, args))?;

	let duration = start.elapsed();

	let exit_code = output.status.code().unwrap_or(-1);
	let stdout = String::from_utf8_lossy(&output.stdout).to_string();
	let stderr = String::from_utf8_lossy(&output.stderr).to_string();

	Ok(RunResult {
		exit_code,
		stdout,
		stderr,
		duration,
	})
}

/// File diff information for detailed output
#[derive(Debug)]
pub struct FileDiff {
	pub path: String,
	pub kind: FileDiffKind,
}

#[derive(Debug)]
pub enum FileDiffKind {
	ContentDiffers {
		content1: String,
		content2: String,
		diff_lines: usize,
	},
	OnlyInFirst,
	OnlyInSecond,
}

/// Compare two directories and return detailed results
/// Returns (matched, similarity_percentage, matched_files, total_files, differences, file_diffs)
///
/// Similarity calculation:
/// - Percentage of files that are semantically equivalent (matched_files / total_files)
/// - Missing/extra files count toward total_files but not matched_files
/// - YAML files use semantic comparison (ignoring cosmetic differences)
pub fn compare_directories_detailed(
	dir1: &str,
	dir2: &str,
) -> Result<(bool, f64, usize, usize, Vec<String>, Vec<FileDiff>)> {
	let files1 = collect_files(dir1)?;
	let files2 = collect_files(dir2)?;

	let mut diffs = Vec::new();
	let mut file_diffs = Vec::new();
	let mut matched_files = 0;
	let mut has_missing_or_extra_files = false;

	// Get all unique file paths
	let all_paths: std::collections::HashSet<_> = files1.keys().chain(files2.keys()).collect();
	let total_files = all_paths.len();

	for path in &all_paths {
		match (files1.get(*path), files2.get(*path)) {
			(Some(content1), Some(content2)) => {
				let text1 = String::from_utf8_lossy(content1);
				let text2 = String::from_utf8_lossy(content2);
				let lines1: Vec<&str> = text1.lines().collect();
				let lines2: Vec<&str> = text2.lines().collect();

				// For YAML files, use semantic comparison
				let semantically_equal = if is_yaml_file(path) {
					compare_yaml_docs_semantically(&text1, &text2).unwrap_or(false)
				} else {
					false
				};

				if content1 == content2 {
					matched_files += 1;
				} else if semantically_equal {
					// YAML files are semantically equal despite text differences
					matched_files += 1;
				} else {
					let diff_lines = lines1.len().abs_diff(lines2.len()).max(
						lines1
							.iter()
							.zip(lines2.iter())
							.filter(|(a, b)| a != b)
							.count(),
					);

					// For YAML files, indicate semantic comparison was attempted
					let diff_msg = if is_yaml_file(path) {
						format!(
							"{}: content differs (~{} line differences, SEMANTIC MISMATCH)",
							path, diff_lines
						)
					} else {
						format!(
							"{}: content differs (~{} line differences)",
							path, diff_lines
						)
					};
					diffs.push(diff_msg);

					// Store full content for detailed diff printing
					file_diffs.push(FileDiff {
						path: path.to_string(),
						kind: FileDiffKind::ContentDiffers {
							content1: text1.to_string(),
							content2: text2.to_string(),
							diff_lines,
						},
					});
				}
			}
			(Some(_), None) => {
				has_missing_or_extra_files = true;
				diffs.push(format!("{}: only in first directory", path));
				file_diffs.push(FileDiff {
					path: path.to_string(),
					kind: FileDiffKind::OnlyInFirst,
				});
			}
			(None, Some(_)) => {
				has_missing_or_extra_files = true;
				diffs.push(format!("{}: only in second directory", path));
				file_diffs.push(FileDiff {
					path: path.to_string(),
					kind: FileDiffKind::OnlyInSecond,
				});
			}
			(None, None) => unreachable!(),
		}
	}

	let matched = diffs.is_empty();

	// Calculate file-based similarity (more meaningful for semantic YAML comparison)
	// This shows percentage of files that are semantically equivalent
	// Note: We calculate similarity even when there are missing/extra files
	// The similarity reflects what percentage of all files matched
	let similarity = if total_files > 0 {
		(matched_files as f64 / total_files as f64) * 100.0
	} else {
		// No files in either directory
		100.0
	};

	// Suppress unused variable warning
	let _ = has_missing_or_extra_files;

	Ok((
		matched,
		similarity,
		matched_files,
		total_files,
		diffs,
		file_diffs,
	))
}

fn collect_files(dir: &str) -> Result<HashMap<String, Vec<u8>>> {
	use std::collections::HashMap;
	use std::fs;

	let mut files = HashMap::new();
	let base_path = PathBuf::from(dir);

	if !base_path.exists() {
		return Ok(files);
	}

	fn visit_dirs(
		dir: &PathBuf,
		base: &PathBuf,
		files: &mut HashMap<String, Vec<u8>>,
	) -> Result<()> {
		if dir.is_dir() {
			for entry in fs::read_dir(dir)? {
				let entry = entry?;
				let path = entry.path();
				if path.is_dir() {
					visit_dirs(&path, base, files)?;
				} else {
					let relative_path = path
						.strip_prefix(base)
						.unwrap()
						.to_string_lossy()
						.to_string();
					let content = fs::read(&path)?;
					files.insert(relative_path, content);
				}
			}
		}
		Ok(())
	}

	visit_dirs(&base_path, &base_path, &mut files)?;
	Ok(files)
}

/// Compare two strings as JSON, performing deep structural comparison
/// Returns (matched, similarity_percentage, matched_count, total_count)
pub fn compare_json(json1: &str, json2: &str) -> Result<(bool, f64, usize, usize)> {
	// Parse both strings as JSON
	let value1: serde_json::Value =
		serde_json::from_str(json1).with_context(|| "Failed to parse first output as JSON")?;
	let value2: serde_json::Value =
		serde_json::from_str(json2).with_context(|| "Failed to parse second output as JSON")?;

	// Calculate similarity
	let (matched, total) = calculate_json_similarity(&value1, &value2);
	let similarity = if total > 0 {
		(matched as f64 / total as f64) * 100.0
	} else {
		100.0
	};

	// Deep comparison of JSON structures
	Ok((value1 == value2, similarity, matched, total))
}

/// Calculate similarity between two JSON values
/// Returns (matched_count, total_count) of JSON paths
fn calculate_json_similarity(val1: &serde_json::Value, val2: &serde_json::Value) -> (usize, usize) {
	use serde_json::Value;

	match (val1, val2) {
		(Value::Object(obj1), Value::Object(obj2)) => {
			let mut matched = 0;
			let mut total = 0;

			// Get all unique keys
			let all_keys: std::collections::HashSet<_> = obj1.keys().chain(obj2.keys()).collect();

			for key in all_keys {
				total += 1;
				match (obj1.get(key), obj2.get(key)) {
					(Some(v1), Some(v2)) => {
						let (sub_matched, sub_total) = calculate_json_similarity(v1, v2);
						if sub_total == 0 {
							// Leaf node
							if v1 == v2 {
								matched += 1;
							}
						} else {
							// Add sub-tree stats
							matched += sub_matched;
							total += sub_total - 1; // -1 because we already counted this key
						}
					}
					_ => {} // Key only in one object, already counted in total
				}
			}

			(matched, total)
		}
		(Value::Array(arr1), Value::Array(arr2)) => {
			let max_len = arr1.len().max(arr2.len());
			let mut matched = 0;
			let mut total = max_len;

			for i in 0..max_len {
				match (arr1.get(i), arr2.get(i)) {
					(Some(v1), Some(v2)) => {
						let (sub_matched, sub_total) = calculate_json_similarity(v1, v2);
						if sub_total == 0 {
							// Leaf node
							if v1 == v2 {
								matched += 1;
							}
						} else {
							// Add sub-tree stats
							matched += sub_matched;
							total += sub_total - 1; // -1 because we already counted this index
						}
					}
					_ => {} // Index only in one array
				}
			}

			(matched, total)
		}
		(_v1, _v2) => {
			// Leaf values - return 0 total to indicate this is a leaf
			(0, 0)
		}
	}
}

/// Calculate similarity between two strings (line-based)
/// Returns (similarity_percentage, matched_lines, total_lines)
pub fn calculate_string_similarity(str1: &str, str2: &str) -> (f64, usize, usize) {
	let lines1: Vec<&str> = str1.lines().collect();
	let lines2: Vec<&str> = str2.lines().collect();

	let max_lines = lines1.len().max(lines2.len());
	if max_lines == 0 {
		return (100.0, 0, 0);
	}

	let mut matching_lines = 0;
	for i in 0..max_lines {
		if lines1.get(i) == lines2.get(i) {
			matching_lines += 1;
		}
	}

	let similarity = (matching_lines as f64 / max_lines as f64) * 100.0;
	(similarity, matching_lines, max_lines)
}

/// Print JSON diff showing differences between two JSON values
pub fn print_json_diff(json1: &str, json2: &str, name1: &str, name2: &str, max_lines: usize) {
	match (
		serde_json::from_str::<serde_json::Value>(json1),
		serde_json::from_str::<serde_json::Value>(json2),
	) {
		(Ok(value1), Ok(value2)) => {
			let full_object_levels = std::env::var("PRINT_FULL_OBJECTS").ok().and_then(|v| {
				if v == "true" {
					Some(0)
				} else {
					v.parse::<usize>().ok()
				}
			});

			eprintln!("\n=== JSON DIFF ===");
			let mut line_count = 0;
			print_json_diff_recursive(
				&value1,
				&value2,
				"$",
				name1,
				name2,
				&mut line_count,
				max_lines,
				full_object_levels,
				&[],
			);
			if line_count >= max_lines {
				eprintln!("... (truncated, {} lines shown)", max_lines);
			}
		}
		_ => {
			eprintln!("\n=== Failed to parse JSON for diff ===");
		}
	}
}

fn print_json_diff_recursive(
	val1: &serde_json::Value,
	val2: &serde_json::Value,
	path: &str,
	name1: &str,
	name2: &str,
	line_count: &mut usize,
	max_lines: usize,
	full_object_levels: Option<usize>,
	parent_stack: &[(&serde_json::Value, &serde_json::Value)],
) {
	use serde_json::Value;

	if *line_count >= max_lines {
		return;
	}

	// Helper to print with context
	let print_with_context = |path: &str, name: &str, val: &Value, line_count: &mut usize| {
		if let Some(levels) = full_object_levels {
			eprintln!("  {} - only in {}", path, name);

			// Go up 'levels' in the parent stack
			let context_val = if levels == 0 {
				val
			} else if levels > parent_stack.len() {
				// If requesting more levels than available, use the root
				if name == name1 {
					parent_stack.first().map(|(v1, _)| v1).unwrap_or(&val)
				} else {
					parent_stack.first().map(|(_, v2)| v2).unwrap_or(&val)
				}
			} else {
				// Go up the specified number of levels
				let idx = parent_stack.len() - levels;
				if name == name1 {
					&parent_stack[idx].0
				} else {
					&parent_stack[idx].1
				}
			};

			eprintln!("    Context ({} levels up) from {}:", levels, name);
			eprintln!(
				"{}",
				serde_json::to_string_pretty(context_val).unwrap_or_default()
			);
		} else {
			eprintln!(
				"  {} - only in {}: {}",
				path,
				name,
				serde_json::to_string(val).unwrap_or_default()
			);
		}
		*line_count += 1;
	};

	match (val1, val2) {
		(Value::Object(obj1), Value::Object(obj2)) => {
			// Build new parent stack with current values
			let mut new_stack = parent_stack.to_vec();
			new_stack.push((val1, val2));

			// Check for keys only in obj1
			for (key, value1) in obj1.iter() {
				if *line_count >= max_lines {
					return;
				}
				let new_path = format!("{}.{}", path, key);
				if let Some(value2) = obj2.get(key) {
					print_json_diff_recursive(
						value1,
						value2,
						&new_path,
						name1,
						name2,
						line_count,
						max_lines,
						full_object_levels,
						&new_stack,
					);
				} else {
					print_with_context(&new_path, name1, value1, line_count);
				}
			}
			// Check for keys only in obj2
			for (key, value2) in obj2.iter() {
				if *line_count >= max_lines {
					return;
				}
				if !obj1.contains_key(key) {
					let new_path = format!("{}.{}", path, key);
					print_with_context(&new_path, name2, value2, line_count);
				}
			}
		}
		(Value::Array(arr1), Value::Array(arr2)) => {
			// Build new parent stack with current values
			let mut new_stack = parent_stack.to_vec();
			new_stack.push((val1, val2));

			let max_len = arr1.len().max(arr2.len());
			for i in 0..max_len {
				if *line_count >= max_lines {
					return;
				}
				let new_path = format!("{}[{}]", path, i);
				match (arr1.get(i), arr2.get(i)) {
					(Some(v1), Some(v2)) => {
						print_json_diff_recursive(
							v1,
							v2,
							&new_path,
							name1,
							name2,
							line_count,
							max_lines,
							full_object_levels,
							&new_stack,
						);
					}
					(Some(v1), None) => {
						print_with_context(&new_path, name1, v1, line_count);
					}
					(None, Some(v2)) => {
						print_with_context(&new_path, name2, v2, line_count);
					}
					(None, None) => {}
				}
			}
		}
		(v1, v2) if v1 != v2 => {
			if let Some(levels) = full_object_levels {
				eprintln!("  {} - values differ", path);

				// Get context for val1
				let context_val1 = if levels == 0 {
					v1
				} else if levels > parent_stack.len() {
					parent_stack.first().map(|(v1, _)| v1).unwrap_or(&v1)
				} else {
					let idx = parent_stack.len() - levels;
					&parent_stack[idx].0
				};

				// Get context for val2
				let context_val2 = if levels == 0 {
					v2
				} else if levels > parent_stack.len() {
					parent_stack.first().map(|(_, v2)| v2).unwrap_or(&v2)
				} else {
					let idx = parent_stack.len() - levels;
					&parent_stack[idx].1
				};

				eprintln!("    Context ({} levels up) from {}:", levels, name1);
				eprintln!(
					"{}",
					serde_json::to_string_pretty(context_val1).unwrap_or_default()
				);
				eprintln!("    Context ({} levels up) from {}:", levels, name2);
				eprintln!(
					"{}",
					serde_json::to_string_pretty(context_val2).unwrap_or_default()
				);
				*line_count += 1;
			} else {
				if *line_count + 2 <= max_lines {
					eprintln!(
						"  {} - {}: {}",
						path,
						name1,
						serde_json::to_string(v1).unwrap_or_default()
					);
					eprintln!(
						"  {} - {}: {}",
						path,
						name2,
						serde_json::to_string(v2).unwrap_or_default()
					);
					*line_count += 2;
				} else if *line_count + 1 <= max_lines {
					eprintln!(
						"  {} - {}: {}",
						path,
						name1,
						serde_json::to_string(v1).unwrap_or_default()
					);
					*line_count += 1;
				}
			}
		}
		_ => {}
	}
}

/// Print unified diff between two strings
pub fn print_string_diff(str1: &str, str2: &str, name1: &str, name2: &str, max_lines: usize) {
	use similar::{ChangeTag, TextDiff};

	eprintln!("\n=== TEXT DIFF ===");
	let diff = TextDiff::from_lines(str1, str2);

	// Calculate padding for name alignment
	let max_name_len = name1.len().max(name2.len());

	let mut line_count = 0;
	for change in diff.iter_all_changes() {
		if line_count >= max_lines {
			eprintln!("... (truncated, {} lines shown)", max_lines);
			break;
		}

		let sign = match change.tag() {
			ChangeTag::Delete => "-",
			ChangeTag::Insert => "+",
			ChangeTag::Equal => " ",
		};

		if change.tag() != ChangeTag::Equal {
			let prefix = match change.tag() {
				ChangeTag::Delete => format!("({:width$}) ", name1, width = max_name_len),
				ChangeTag::Insert => format!("({:width$}) ", name2, width = max_name_len),
				ChangeTag::Equal => String::new(),
			};
			eprint!("{}{}{}", sign, prefix, change);
			line_count += 1;
		}
	}
}

/// Print detailed file diffs for directory comparison
pub fn print_directory_file_diffs(
	file_diffs: &[FileDiff],
	name1: &str,
	name2: &str,
	max_lines: usize,
) {
	use similar::{ChangeTag, TextDiff};

	eprintln!("\n=== DIRECTORY FILE DIFFS ===");

	// Calculate padding for name alignment
	let max_name_len = name1.len().max(name2.len());

	let mut total_line_count = 0;
	for file_diff in file_diffs {
		if total_line_count >= max_lines {
			eprintln!("... (truncated, {} lines shown)", max_lines);
			break;
		}

		match &file_diff.kind {
			FileDiffKind::ContentDiffers {
				content1,
				content2,
				diff_lines,
			} => {
				eprintln!(
					"\nFile: {} (~{} line differences)",
					file_diff.path, diff_lines
				);

				let diff = TextDiff::from_lines(content1, content2);
				let mut file_line_count = 0;

				for change in diff.iter_all_changes() {
					if total_line_count >= max_lines {
						eprintln!("  ... (truncated, {} lines shown total)", max_lines);
						return;
					}

					let sign = match change.tag() {
						ChangeTag::Delete => "-",
						ChangeTag::Insert => "+",
						ChangeTag::Equal => " ",
					};

					if change.tag() != ChangeTag::Equal {
						let prefix = match change.tag() {
							ChangeTag::Delete => {
								format!("({:width$}) ", name1, width = max_name_len)
							}
							ChangeTag::Insert => {
								format!("({:width$}) ", name2, width = max_name_len)
							}
							ChangeTag::Equal => String::new(),
						};
						eprint!("  {}{}{}", sign, prefix, change);
						file_line_count += 1;
						total_line_count += 1;
					}
				}

				if file_line_count == 0 {
					eprintln!("  (no visible differences in first {} lines)", max_lines);
				}
			}
			FileDiffKind::OnlyInFirst => {
				eprintln!("\nFile: {} - only in {} directory", file_diff.path, name1);
				total_line_count += 1;
			}
			FileDiffKind::OnlyInSecond => {
				eprintln!("\nFile: {} - only in {} directory", file_diff.path, name2);
				total_line_count += 1;
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::tempdir;

	#[test]
	fn test_compare_directories_identical() {
		let dir = tempdir().unwrap();
		let dir1 = dir.path().join("a");
		let dir2 = dir.path().join("b");
		fs::create_dir_all(&dir1).unwrap();
		fs::create_dir_all(&dir2).unwrap();

		fs::write(dir1.join("file.txt"), "hello").unwrap();
		fs::write(dir2.join("file.txt"), "hello").unwrap();

		let result =
			compare_directories_detailed(dir1.to_str().unwrap(), dir2.to_str().unwrap()).unwrap();
		assert!(result.0); // matched
		assert_eq!(result.1, 100.0); // 100% similarity
		assert_eq!(result.2, 1); // 1 matched file
		assert_eq!(result.3, 1); // 1 total file
		assert!(result.4.is_empty()); // no diffs
		assert!(result.5.is_empty()); // no file_diffs
	}

	#[test]
	fn test_compare_directories_different_content() {
		let dir = tempdir().unwrap();
		let dir1 = dir.path().join("a");
		let dir2 = dir.path().join("b");
		fs::create_dir_all(&dir1).unwrap();
		fs::create_dir_all(&dir2).unwrap();

		fs::write(dir1.join("file.txt"), "hello").unwrap();
		fs::write(dir2.join("file.txt"), "world").unwrap();

		let result =
			compare_directories_detailed(dir1.to_str().unwrap(), dir2.to_str().unwrap()).unwrap();
		assert!(!result.0); // not matched
		assert_eq!(result.1, 0.0); // 0% similarity (both are 1 line, 0 match)
		assert_eq!(result.2, 0); // 0 matched files
		assert_eq!(result.3, 1); // 1 total file
		assert_eq!(result.4.len(), 1); // 1 diff
		assert_eq!(result.5.len(), 1); // 1 file_diff
	}

	#[test]
	fn test_compare_directories_different_content_partial_match() {
		let dir = tempdir().unwrap();
		let dir1 = dir.path().join("a");
		let dir2 = dir.path().join("b");
		fs::create_dir_all(&dir1).unwrap();
		fs::create_dir_all(&dir2).unwrap();

		fs::write(dir1.join("file.txt"), "line1\nline2\nline3").unwrap();
		fs::write(dir2.join("file.txt"), "line1\ndifferent\nline3").unwrap();

		let result =
			compare_directories_detailed(dir1.to_str().unwrap(), dir2.to_str().unwrap()).unwrap();
		assert!(!result.0); // not matched
					  // File-based similarity: 0 matched files / 1 total file = 0%
		assert_eq!(result.1, 0.0);
		assert_eq!(result.2, 0); // 0 matched files
		assert_eq!(result.3, 1); // 1 total file
		assert_eq!(result.4.len(), 1); // 1 diff
		assert_eq!(result.5.len(), 1); // 1 file_diff
	}

	#[test]
	fn test_compare_directories_missing_file() {
		let dir = tempdir().unwrap();
		let dir1 = dir.path().join("a");
		let dir2 = dir.path().join("b");
		fs::create_dir_all(&dir1).unwrap();
		fs::create_dir_all(&dir2).unwrap();

		fs::write(dir1.join("file1.txt"), "hello").unwrap();
		fs::write(dir2.join("file2.txt"), "world").unwrap();

		let result =
			compare_directories_detailed(dir1.to_str().unwrap(), dir2.to_str().unwrap()).unwrap();
		assert!(!result.0); // not matched
		assert_eq!(result.1, 0.0); // 0% similarity (files don't match)
		assert_eq!(result.2, 0); // 0 matched files
		assert_eq!(result.3, 2); // 2 total files
		assert_eq!(result.4.len(), 2); // 2 diffs (one in each dir only)
		assert_eq!(result.5.len(), 2); // 2 file_diffs
	}

	#[test]
	fn test_compare_directories_empty() {
		let dir = tempdir().unwrap();
		let dir1 = dir.path().join("a");
		let dir2 = dir.path().join("b");
		fs::create_dir_all(&dir1).unwrap();
		fs::create_dir_all(&dir2).unwrap();

		let result =
			compare_directories_detailed(dir1.to_str().unwrap(), dir2.to_str().unwrap()).unwrap();
		assert!(result.0); // matched (both empty)
		assert_eq!(result.1, 100.0); // 100% similarity
		assert_eq!(result.2, 0); // 0 matched files
		assert_eq!(result.3, 0); // 0 total files
		assert!(result.5.is_empty()); // no file_diffs
	}

	#[test]
	fn test_compare_directories_nested() {
		let dir = tempdir().unwrap();
		let dir1 = dir.path().join("a");
		let dir2 = dir.path().join("b");
		fs::create_dir_all(dir1.join("sub")).unwrap();
		fs::create_dir_all(dir2.join("sub")).unwrap();

		fs::write(dir1.join("sub/file.txt"), "hello").unwrap();
		fs::write(dir2.join("sub/file.txt"), "hello").unwrap();

		let result =
			compare_directories_detailed(dir1.to_str().unwrap(), dir2.to_str().unwrap()).unwrap();
		assert!(result.0); // matched
		assert_eq!(result.1, 100.0); // 100% similarity (all files match)
		assert_eq!(result.2, 1); // 1 matched file
		assert!(result.5.is_empty()); // no file_diffs
	}

	#[test]
	fn test_compare_directories_multiple_matching_files() {
		let dir = tempdir().unwrap();
		let dir1 = dir.path().join("a");
		let dir2 = dir.path().join("b");
		fs::create_dir_all(&dir1).unwrap();
		fs::create_dir_all(&dir2).unwrap();

		// Create multiple files with multiple lines
		fs::write(dir1.join("file1.txt"), "line1\nline2\nline3").unwrap();
		fs::write(dir2.join("file1.txt"), "line1\nline2\nline3").unwrap();
		fs::write(dir1.join("file2.txt"), "content\nhere").unwrap();
		fs::write(dir2.join("file2.txt"), "content\nhere").unwrap();

		let result =
			compare_directories_detailed(dir1.to_str().unwrap(), dir2.to_str().unwrap()).unwrap();
		assert!(result.0); // matched
		assert_eq!(result.1, 100.0); // 100% similarity (all files match)
		assert_eq!(result.2, 2); // 2 matched files
		assert_eq!(result.3, 2); // 2 total files
		assert!(result.4.is_empty()); // no diffs
		assert!(result.5.is_empty()); // no file_diffs
	}

	#[test]
	fn test_compare_directories_multiple_files_with_differences() {
		let dir = tempdir().unwrap();
		let dir1 = dir.path().join("a");
		let dir2 = dir.path().join("b");
		fs::create_dir_all(&dir1).unwrap();
		fs::create_dir_all(&dir2).unwrap();

		// file1: 3 lines, all match
		fs::write(dir1.join("file1.txt"), "line1\nline2\nline3").unwrap();
		fs::write(dir2.join("file1.txt"), "line1\nline2\nline3").unwrap();
		// file2: 2 lines, 1 matches, 1 doesn't
		fs::write(dir1.join("file2.txt"), "match\ndiff1").unwrap();
		fs::write(dir2.join("file2.txt"), "match\ndiff2").unwrap();

		let result =
			compare_directories_detailed(dir1.to_str().unwrap(), dir2.to_str().unwrap()).unwrap();
		assert!(!result.0); // not matched
					  // File-based similarity: 1 matched file / 2 total files = 50%
		assert_eq!(result.1, 50.0);
		assert_eq!(result.2, 1); // 1 matched file (file1)
		assert_eq!(result.3, 2); // 2 total files
		assert_eq!(result.4.len(), 1); // 1 diff (file2)
		assert_eq!(result.5.len(), 1); // 1 file_diff
	}

	#[test]
	fn test_compare_directories_mixed_content() {
		let dir = tempdir().unwrap();
		let dir1 = dir.path().join("a");
		let dir2 = dir.path().join("b");
		fs::create_dir_all(&dir1).unwrap();
		fs::create_dir_all(&dir2).unwrap();

		// Two files: one matches, one doesn't
		fs::write(dir1.join("file1.txt"), "hello").unwrap();
		fs::write(dir2.join("file1.txt"), "hello").unwrap();
		fs::write(dir1.join("file2.txt"), "world").unwrap();
		fs::write(dir2.join("file2.txt"), "different").unwrap();

		let result =
			compare_directories_detailed(dir1.to_str().unwrap(), dir2.to_str().unwrap()).unwrap();
		assert!(!result.0); // not matched
					  // file1: 1 line matches, file2: 0 lines match = 1/2 = 50%
		assert_eq!(result.1, 50.0);
		assert_eq!(result.2, 1); // 1 matched file
		assert_eq!(result.3, 2); // 2 total files
		assert_eq!(result.4.len(), 1); // 1 diff
		assert_eq!(result.5.len(), 1); // 1 file_diff
	}

	#[test]
	fn test_compare_json_identical() {
		let json1 = r#"{"a": 1, "b": 2}"#;
		let json2 = r#"{"a": 1, "b": 2}"#;

		let result = compare_json(json1, json2).unwrap();
		assert!(result.0); // matched
		assert_eq!(result.1, 100.0); // 100% similarity
	}

	#[test]
	fn test_compare_json_different() {
		let json1 = r#"{"a": 1, "b": 2}"#;
		let json2 = r#"{"a": 1, "b": 3}"#;

		let result = compare_json(json1, json2).unwrap();
		assert!(!result.0); // not matched
	}

	#[test]
	fn test_calculate_string_similarity_identical() {
		let (similarity, matched, total) =
			calculate_string_similarity("hello\nworld", "hello\nworld");
		assert_eq!(similarity, 100.0);
		assert_eq!(matched, 2);
		assert_eq!(total, 2);
	}

	#[test]
	fn test_calculate_string_similarity_different() {
		let (similarity, matched, total) =
			calculate_string_similarity("hello\nworld", "hello\nrust");
		assert_eq!(similarity, 50.0);
		assert_eq!(matched, 1);
		assert_eq!(total, 2);
	}

	#[test]
	fn test_calculate_string_similarity_empty() {
		let (similarity, _, _) = calculate_string_similarity("", "");
		assert_eq!(similarity, 100.0);
	}

	// ==================== YAML Semantic Comparison Tests ====================

	#[test]
	fn test_is_ignored_key() {
		// Should ignore config_hash variants
		assert!(is_ignored_key("config_hash"));
		assert!(is_ignored_key("config-hash"));
		assert!(is_ignored_key("mimir-config-exporter-hash"));
		assert!(is_ignored_key("some_hash"));
		// Should ignore tanka.dev/environment
		assert!(is_ignored_key("tanka.dev/environment"));

		// Should NOT ignore regular keys
		assert!(!is_ignored_key("name"));
		assert!(!is_ignored_key("hash")); // exact match only for suffix
		assert!(!is_ignored_key("hashcode"));
		assert!(!is_ignored_key("config"));
		assert!(!is_ignored_key("environment")); // only tanka.dev/environment
	}

	#[test]
	fn test_normalize_float_precision() {
		// Google's jsonnet bug: 0.1 becomes 0.10000000000000001
		assert_eq!(normalize_float(0.10000000000000001), 0.1);
		assert_eq!(normalize_float(0.1), 0.1);

		// Other precision issues
		assert_eq!(normalize_float(0.30000000000000004), 0.3);
		assert_eq!(normalize_float(0.7000000000000001), 0.7);

		// Should preserve correct values
		assert_eq!(normalize_float(1.0), 1.0);
		assert_eq!(normalize_float(0.5), 0.5);
		assert_eq!(normalize_float(123.456), 123.456);
	}

	#[test]
	fn test_normalize_string_trailing_newlines() {
		// Should normalize trailing newlines
		assert_eq!(normalize_string("hello\n\n"), "hello");
		assert_eq!(normalize_string("hello\n"), "hello");
		assert_eq!(normalize_string("hello"), "hello");

		// Should preserve internal newlines
		assert_eq!(normalize_string("hello\nworld\n"), "hello\nworld");
		assert_eq!(normalize_string("hello\n\nworld\n\n"), "hello\n\nworld");
	}

	#[test]
	fn test_yaml_semantic_identical() {
		let yaml1 = r#"
name: test
value: 123
"#;
		let yaml2 = r#"
name: test
value: 123
"#;
		assert!(compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_yaml_semantic_different_formatting() {
		// Same content, different formatting (quotes, spacing)
		let yaml1 = r#"name: test
value: "hello world""#;
		let yaml2 = r#"name: 'test'
value: hello world"#;
		assert!(compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_yaml_semantic_ignores_config_hash() {
		let yaml1 = r#"
metadata:
  annotations:
    config_hash: abc123
name: test
"#;
		let yaml2 = r#"
metadata:
  annotations:
    config_hash: def456
name: test
"#;
		assert!(compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_yaml_semantic_ignores_suffix_hash() {
		let yaml1 = r#"
metadata:
  annotations:
    mimir-config-exporter-hash: 2f8fdac13552ab53351b0a4f63520bf1
name: deployment
"#;
		let yaml2 = r#"
metadata:
  annotations:
    mimir-config-exporter-hash: 3b584294d0e5d33091e89350e81f2365
name: deployment
"#;
		assert!(compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_yaml_semantic_float_precision() {
		// Google's jsonnet floating point bug
		let yaml1 = r#"
args:
  - -sample-rate=0.10000000000000001
value: 0.30000000000000004
"#;
		let yaml2 = r#"
args:
  - -sample-rate=0.1
value: 0.3
"#;
		assert!(compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_yaml_semantic_trailing_newlines() {
		let yaml1 = r#"
data:
  config: |
    hello
    world

"#;
		let yaml2 = r#"
data:
  config: |
    hello
    world
"#;
		assert!(compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_yaml_semantic_multi_document() {
		let yaml1 = r#"---
name: doc1
---
name: doc2
"#;
		let yaml2 = r#"---
name: doc1
---
name: doc2
"#;
		assert!(compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_yaml_semantic_actually_different() {
		let yaml1 = r#"
name: test
value: 123
"#;
		let yaml2 = r#"
name: test
value: 456
"#;
		assert!(!compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_yaml_semantic_different_keys() {
		let yaml1 = r#"
name: test
"#;
		let yaml2 = r#"
name: test
extra: value
"#;
		assert!(!compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_yaml_semantic_nested_structures() {
		let yaml1 = r#"
metadata:
  labels:
    app: test
    config_hash: hash1
spec:
  containers:
    - name: main
      args:
        - -rate=0.10000000000000001
"#;
		let yaml2 = r#"
metadata:
  labels:
    app: test
    config_hash: hash2
spec:
  containers:
    - name: main
      args:
        - -rate=0.1
"#;
		assert!(compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_yaml_semantic_complex_embedded_content() {
		// Simulates ConfigMap with embedded config that has trailing newline diff
		let yaml1 = r#"
apiVersion: v1
kind: ConfigMap
metadata:
  name: test
  annotations:
    config-hash: abc123
data:
  httpd.conf: |
    ServerRoot "/usr/local/apache2"
    Listen 80

"#;
		let yaml2 = r#"
apiVersion: v1
kind: ConfigMap
metadata:
  name: test
  annotations:
    config-hash: def456
data:
  httpd.conf: |
    ServerRoot "/usr/local/apache2"
    Listen 80
"#;
		assert!(compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_is_yaml_file() {
		assert!(is_yaml_file("test.yaml"));
		assert!(is_yaml_file("test.yml"));
		assert!(is_yaml_file("/path/to/config.yaml"));
		assert!(is_yaml_file("ConfigMap-test.yaml"));

		assert!(!is_yaml_file("test.json"));
		assert!(!is_yaml_file("test.txt"));
		assert!(!is_yaml_file("manifest.json"));
		assert!(!is_yaml_file("yaml")); // no extension
	}

	#[test]
	fn test_normalize_yaml_value_removes_hash_keys() {
		use serde_yaml::Value;

		let yaml = r#"
metadata:
  name: test
  annotations:
    config_hash: abc123
    other-hash: def456
    regular: value
"#;
		let parsed: Value = serde_yaml::from_str(yaml).unwrap();
		let normalized = normalize_yaml_value(parsed);

		// Check that hash keys are removed
		if let Value::Mapping(map) = normalized {
			if let Some(Value::Mapping(metadata)) = map.get(&Value::String("metadata".to_string()))
			{
				if let Some(Value::Mapping(annotations)) =
					metadata.get(&Value::String("annotations".to_string()))
				{
					assert!(!annotations.contains_key(&Value::String("config_hash".to_string())));
					assert!(!annotations.contains_key(&Value::String("other-hash".to_string())));
					assert!(annotations.contains_key(&Value::String("regular".to_string())));
				} else {
					panic!("annotations not found");
				}
			} else {
				panic!("metadata not found");
			}
		} else {
			panic!("expected mapping");
		}
	}

	#[test]
	fn test_yaml_semantic_quoting_difference() {
		// Test that unquoted and single-quoted values are semantically equal
		let yaml1 = r#"
data:
  - 100*( 1-( sum by (env)
"#;
		let yaml2 = r#"
data:
  - '100*( 1-( sum by (env)'
"#;
		assert!(compare_yaml_docs_semantically(yaml1, yaml2).unwrap());
	}

	#[test]
	fn test_compare_directories_with_yaml_semantic() {
		let dir = tempdir().unwrap();
		let dir1 = dir.path().join("a");
		let dir2 = dir.path().join("b");
		fs::create_dir_all(&dir1).unwrap();
		fs::create_dir_all(&dir2).unwrap();

		// YAML files with same semantic content but different formatting
		let yaml1 = r#"name: test
config_hash: abc123
value: 0.10000000000000001
"#;
		let yaml2 = r#"name: test
config_hash: def456
value: 0.1
"#;
		fs::write(dir1.join("config.yaml"), yaml1).unwrap();
		fs::write(dir2.join("config.yaml"), yaml2).unwrap();

		let result =
			compare_directories_detailed(dir1.to_str().unwrap(), dir2.to_str().unwrap()).unwrap();

		// Should match semantically
		assert!(result.0); // matched
		assert_eq!(result.1, 100.0); // 100% similarity
		assert_eq!(result.2, 1); // 1 matched file
	}
}
