use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

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

pub fn compare_directories(dir1: &str, dir2: &str) -> Result<bool> {
	// Get all files recursively from both directories
	let files1 = collect_files(dir1)?;
	let files2 = collect_files(dir2)?;

	// Compare file sets
	if files1.keys().collect::<Vec<_>>() != files2.keys().collect::<Vec<_>>() {
		return Ok(false);
	}

	// Compare file contents
	for (path, content1) in &files1 {
		if let Some(content2) = files2.get(path) {
			if content1 != content2 {
				return Ok(false);
			}
		} else {
			return Ok(false);
		}
	}

	Ok(true)
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
/// Returns true if both are valid JSON and structurally identical
pub fn compare_json(json1: &str, json2: &str) -> Result<bool> {
	// Parse both strings as JSON
	let value1: serde_json::Value =
		serde_json::from_str(json1).with_context(|| "Failed to parse first output as JSON")?;
	let value2: serde_json::Value =
		serde_json::from_str(json2).with_context(|| "Failed to parse second output as JSON")?;

	// Deep comparison of JSON structures
	Ok(value1 == value2)
}

/// Print JSON diff showing differences between two JSON values
pub fn print_json_diff(json1: &str, json2: &str, name1: &str, name2: &str, max_lines: usize) {
	match (
		serde_json::from_str::<serde_json::Value>(json1),
		serde_json::from_str::<serde_json::Value>(json2),
	) {
		(Ok(value1), Ok(value2)) => {
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
) {
	use serde_json::Value;

	if *line_count >= max_lines {
		return;
	}

	match (val1, val2) {
		(Value::Object(obj1), Value::Object(obj2)) => {
			// Check for keys only in obj1
			for (key, value1) in obj1.iter() {
				if *line_count >= max_lines {
					return;
				}
				let new_path = format!("{}.{}", path, key);
				if let Some(value2) = obj2.get(key) {
					print_json_diff_recursive(
						value1, value2, &new_path, name1, name2, line_count, max_lines,
					);
				} else {
					eprintln!(
						"  {} - only in {}: {}",
						new_path,
						name1,
						serde_json::to_string(value1).unwrap_or_default()
					);
					*line_count += 1;
				}
			}
			// Check for keys only in obj2
			for (key, value2) in obj2.iter() {
				if *line_count >= max_lines {
					return;
				}
				if !obj1.contains_key(key) {
					let new_path = format!("{}.{}", path, key);
					eprintln!(
						"  {} - only in {}: {}",
						new_path,
						name2,
						serde_json::to_string(value2).unwrap_or_default()
					);
					*line_count += 1;
				}
			}
		}
		(Value::Array(arr1), Value::Array(arr2)) => {
			let max_len = arr1.len().max(arr2.len());
			for i in 0..max_len {
				if *line_count >= max_lines {
					return;
				}
				let new_path = format!("{}[{}]", path, i);
				match (arr1.get(i), arr2.get(i)) {
					(Some(v1), Some(v2)) => {
						print_json_diff_recursive(
							v1, v2, &new_path, name1, name2, line_count, max_lines,
						);
					}
					(Some(v1), None) => {
						eprintln!(
							"  {} - only in {}: {}",
							new_path,
							name1,
							serde_json::to_string(v1).unwrap_or_default()
						);
						*line_count += 1;
					}
					(None, Some(v2)) => {
						eprintln!(
							"  {} - only in {}: {}",
							new_path,
							name2,
							serde_json::to_string(v2).unwrap_or_default()
						);
						*line_count += 1;
					}
					(None, None) => {}
				}
			}
		}
		(v1, v2) if v1 != v2 => {
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
		_ => {}
	}
}

/// Print unified diff between two strings
pub fn print_string_diff(str1: &str, str2: &str, name1: &str, name2: &str, max_lines: usize) {
	use similar::{ChangeTag, TextDiff};

	eprintln!("\n=== TEXT DIFF ===");
	let diff = TextDiff::from_lines(str1, str2);

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
				ChangeTag::Delete => format!("({}) ", name1),
				ChangeTag::Insert => format!("({}) ", name2),
				ChangeTag::Equal => String::new(),
			};
			eprint!("{}{}{}", sign, prefix, change);
			line_count += 1;
		}
	}
}
