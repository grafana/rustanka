use std::{
	collections::HashMap,
	path::PathBuf,
	process::{Command as ProcessCommand, Stdio},
	time::{Duration, Instant},
};

use anyhow::{Context, Result};

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
/// Returns (matched, line_similarity_percentage, matched_lines, total_lines, differences, file_diffs)
///
/// Similarity calculation:
/// - Line similarity: Percentage of lines that match exactly (byte-for-byte)
/// - Missing/extra files count their lines toward total_lines but not matched_lines
pub fn compare_directories_detailed(
	dir1: &str,
	dir2: &str,
) -> Result<(bool, f64, usize, usize, Vec<String>, Vec<FileDiff>)> {
	let files1 = collect_files(dir1)?;
	let files2 = collect_files(dir2)?;

	let mut diffs = Vec::new();
	let mut file_diffs = Vec::new();
	let mut total_lines = 0; // Total lines across all files
	let mut matched_lines = 0; // Exact line-by-line matches

	// Get all unique file paths
	let all_paths: std::collections::HashSet<_> = files1.keys().chain(files2.keys()).collect();

	for path in &all_paths {
		match (files1.get(*path), files2.get(*path)) {
			(Some(content1), Some(content2)) => {
				let text1 = String::from_utf8_lossy(content1);
				let text2 = String::from_utf8_lossy(content2);
				let lines1: Vec<&str> = text1.lines().collect();
				let lines2: Vec<&str> = text2.lines().collect();

				// Count total lines (use the max of the two files)
				let file_total_lines = lines1.len().max(lines2.len());
				total_lines += file_total_lines;

				// Check exact match first
				let exact_match = content1 == content2;

				if exact_match {
					// All lines match exactly
					matched_lines += file_total_lines;
				} else {
					// Count matching lines
					let min_len = lines1.len().min(lines2.len());
					let matching = lines1
						.iter()
						.zip(lines2.iter())
						.take(min_len)
						.filter(|(a, b)| a == b)
						.count();

					matched_lines += matching;

					let diff_lines = lines1.len().abs_diff(lines2.len()).max(
						lines1
							.iter()
							.zip(lines2.iter())
							.filter(|(a, b)| a != b)
							.count(),
					);

					diffs.push(format!(
						"{}: content differs (~{} line differences)",
						path, diff_lines
					));

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
			(Some(content1), None) => {
				let text1 = String::from_utf8_lossy(content1);
				let lines1 = text1.lines().count();
				total_lines += lines1;
				// No matching lines
				diffs.push(format!("{}: only in first directory", path));
				file_diffs.push(FileDiff {
					path: path.to_string(),
					kind: FileDiffKind::OnlyInFirst,
				});
			}
			(None, Some(content2)) => {
				let text2 = String::from_utf8_lossy(content2);
				let lines2 = text2.lines().count();
				total_lines += lines2;
				// No matching lines
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

	// Calculate line-based similarity (exact byte-for-byte matches)
	let line_similarity = if total_lines > 0 {
		(matched_lines as f64 / total_lines as f64) * 100.0
	} else {
		100.0
	};

	Ok((
		matched,
		line_similarity,
		matched_lines,
		total_lines,
		diffs,
		file_diffs,
	))
}

fn collect_files(dir: &str) -> Result<HashMap<String, Vec<u8>>> {
	use std::{collections::HashMap, fs};

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
	use std::fs;

	use tempfile::tempdir;

	use super::*;

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
		assert_eq!(result.1, 100.0); // 100% line similarity
		assert_eq!(result.2, 1); // 1 matched line
		assert_eq!(result.3, 1); // 1 total line
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
		assert_eq!(result.1, 0.0); // 0% line similarity
		assert_eq!(result.2, 0); // 0 matched lines
		assert_eq!(result.3, 1); // 1 total line
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
					  // Line-based similarity: 2 matched lines / 3 total lines = 66.67%
		assert!((result.1 - 66.67).abs() < 0.1); // ~66.67% line similarity
		assert_eq!(result.2, 2); // 2 matched lines - line1 and line3
		assert_eq!(result.3, 3); // 3 total lines
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
		assert_eq!(result.1, 0.0); // 0% line similarity
		assert_eq!(result.2, 0); // 0 matched lines
		assert_eq!(result.3, 2); // 2 total lines (1 + 1)
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
		assert_eq!(result.1, 100.0); // 100% line similarity
		assert_eq!(result.2, 0); // 0 matched lines
		assert_eq!(result.3, 0); // 0 total lines
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
		assert_eq!(result.1, 100.0); // 100% line similarity
		assert_eq!(result.2, 1); // 1 matched line
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
		assert_eq!(result.1, 100.0); // 100% line similarity
		assert_eq!(result.2, 5); // 5 matched lines - 3 + 2
		assert_eq!(result.3, 5); // 5 total lines
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
					  // Line-based similarity: 4 matched lines / 5 total lines = 80%
		assert_eq!(result.1, 80.0); // 80% line similarity
		assert_eq!(result.2, 4); // 4 matched lines - 3 from file1 + 1 from file2
		assert_eq!(result.3, 5); // 5 total lines
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
					  // Line-based similarity: 1 matched line / 2 total lines = 50%
		assert_eq!(result.1, 50.0); // 50% line similarity
		assert_eq!(result.2, 1); // 1 matched line - from file1
		assert_eq!(result.3, 2); // 2 total lines
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
}
