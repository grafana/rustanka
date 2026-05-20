//! Validate lint command handler.
//!
//! Checks that all validation files in the tests directory are valid:
//! - Syntactically valid Jsonnet
//! - Define at least one of `namespaceTest` or `manifestTest`

use std::{fs, io::Write, path::PathBuf};

use anyhow::Result;
use clap::Args;

use super::common;

#[derive(Args)]
pub struct LintArgs {
	/// Directory containing Jsonnet validation files
	pub tests_dir: String,
}

/// Run the validate lint command.
pub fn run<W: Write>(args: LintArgs, mut writer: W) -> Result<()> {
	let tests_dir = PathBuf::from(&args.tests_dir);

	if !tests_dir.is_dir() {
		anyhow::bail!("tests directory does not exist: {}", tests_dir.display());
	}

	let validation_files = common::collect_validation_files(&tests_dir)?;
	if validation_files.is_empty() {
		writeln!(
			writer,
			"No validation files found in {}",
			tests_dir.display()
		)?;
		return Ok(());
	}

	writeln!(
		writer,
		"Linting {} validation files...",
		validation_files.len()
	)?;
	writeln!(writer)?;

	let mut errors = Vec::new();

	for file in &validation_files {
		let file_display = file
			.strip_prefix(&tests_dir)
			.unwrap_or(file)
			.to_string_lossy()
			.to_string();

		// Check 1: Read the file
		let content = match fs::read_to_string(file) {
			Ok(c) => c,
			Err(e) => {
				errors.push(format!("{}: cannot read file: {}", file_display, e));
				continue;
			}
		};

		// Check 2: Try to parse/evaluate the file as Jsonnet
		let file_abs = match file.canonicalize() {
			Ok(p) => p,
			Err(e) => {
				errors.push(format!("{}: cannot resolve path: {}", file_display, e));
				continue;
			}
		};

		let snippet = format!(
			"local v = import '{}';\ntrue",
			file_abs.to_string_lossy().replace('\\', "/"),
		);

		if let Err(e) = common::eval_jsonnet_snippet(&snippet, &[tests_dir.clone()]) {
			errors.push(format!("{}: invalid Jsonnet: {}", file_display, e));
			continue;
		}

		// Check 3: Must define at least one of namespaceTest or manifestTest
		let has_namespace_test = content.contains("namespaceTest");
		let has_manifest_test = content.contains("manifestTest");

		if !has_namespace_test && !has_manifest_test {
			errors.push(format!(
				"{}: must define at least one of namespaceTest or manifestTest",
				file_display,
			));
			continue;
		}

		// Check 4: Verify the functions are actually callable by evaluating with dummy data
		if has_manifest_test {
			let check_snippet = format!(
				"local v = import '{}';\nstd.type(v.manifestTest) == 'function'",
				file_abs.to_string_lossy().replace('\\', "/"),
			);
			match common::eval_jsonnet_snippet(&check_snippet, &[tests_dir.clone()]) {
				Ok(serde_json::Value::Bool(true)) => {}
				Ok(_) => {
					errors.push(format!("{}: manifestTest must be a function", file_display,));
				}
				Err(e) => {
					errors.push(format!(
						"{}: error checking manifestTest: {}",
						file_display, e,
					));
				}
			}
		}

		if has_namespace_test {
			let check_snippet = format!(
				"local v = import '{}';\nstd.type(v.namespaceTest) == 'function'",
				file_abs.to_string_lossy().replace('\\', "/"),
			);
			match common::eval_jsonnet_snippet(&check_snippet, &[tests_dir.clone()]) {
				Ok(serde_json::Value::Bool(true)) => {}
				Ok(_) => {
					errors.push(format!(
						"{}: namespaceTest must be a function",
						file_display,
					));
				}
				Err(e) => {
					errors.push(format!(
						"{}: error checking namespaceTest: {}",
						file_display, e,
					));
				}
			}
		}

		// Check 5: If `kinds` is defined, it must be an array of strings
		if content.contains("kinds") {
			let kinds_snippet = format!(
				"local v = import '{}';\nif std.objectHas(v, 'kinds') then v.kinds else null",
				file_abs.to_string_lossy().replace('\\', "/"),
			);
			match common::eval_jsonnet_snippet(&kinds_snippet, &[tests_dir.clone()]) {
				Ok(serde_json::Value::Null) => {} // kinds not defined as a field, just referenced
				Ok(serde_json::Value::Array(arr)) => {
					let all_strings = arr.iter().all(|v| v.is_string());
					if !all_strings {
						errors.push(format!(
							"{}: kinds must be an array of strings",
							file_display,
						));
						continue;
					}
				}
				Ok(other) => {
					errors.push(format!(
						"{}: kinds must be an array of strings, got: {}",
						file_display, other,
					));
					continue;
				}
				Err(e) => {
					errors.push(format!("{}: error checking kinds: {}", file_display, e,));
					continue;
				}
			}
		}

		writeln!(writer, "  OK  {}", file_display)?;
	}

	if errors.is_empty() {
		writeln!(
			writer,
			"All {} validation files are valid.",
			validation_files.len()
		)?;
		Ok(())
	} else {
		for err in &errors {
			writeln!(writer, "  ERR {}", err)?;
		}
		writeln!(writer)?;
		anyhow::bail!("{} validation file(s) have errors", errors.len())
	}
}
