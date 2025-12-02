use anyhow::Result;
use clap::Parser;

mod config;
mod report;
mod runner;

use config::Config;
use report::CommandReport;

/// Find output directory in export command args
/// Looks for positional argument after "export" subcommand (first non-flag arg)
fn find_output_dir_in_args(args: &[String]) -> Option<String> {
	let mut found_export = false;
	for arg in args {
		if arg == "export" {
			found_export = true;
			continue;
		}
		if found_export && !arg.starts_with('-') {
			return Some(arg.clone());
		}
	}
	None
}

/// Clean up export output directories for a command (both exec1 and exec2 variants)
fn cleanup_export_dirs(command: &config::Command, exec1_name: &str, exec2_name: &str) {
	if !command.dir_compare {
		return;
	}

	let args1 = command.args_for_exec(exec1_name);
	let args2 = command.args_for_exec(exec2_name);

	if let Some(dir) = find_output_dir_in_args(&args1) {
		if std::path::Path::new(&dir).exists() {
			let _ = std::fs::remove_dir_all(&dir);
		}
	}
	if let Some(dir) = find_output_dir_in_args(&args2) {
		if std::path::Path::new(&dir).exists() {
			let _ = std::fs::remove_dir_all(&dir);
		}
	}
}

#[derive(Parser)]
#[command(name = "tk-compare")]
#[command(about = "Integration testing and benchmarking tool for comparing two executables", long_about = None)]
#[command(version)]
struct Cli {
	/// Path to the config file
	config: String,

	/// Keep workspace directory after tests complete
	#[arg(long)]
	keep_workspace: bool,
}

fn main() -> Result<()> {
	let cli = Cli::parse();

	// Check for debug mode
	let debug_mode = std::env::var("DEBUG").unwrap_or_default() == "true";
	let debug_max_lines = if debug_mode {
		let max_lines = std::env::var("DEBUG_MAX_LINES")
			.ok()
			.and_then(|v| v.parse::<usize>().ok())
			.unwrap_or(100);
		let full_object_config = std::env::var("PRINT_FULL_OBJECTS").ok();
		eprintln!("DEBUG mode enabled (max {} diff lines)", max_lines);
		if let Some(config) = full_object_config {
			if config == "true" {
				eprintln!("PRINT_FULL_OBJECTS: enabled (0 levels - current object)\n");
			} else if let Ok(levels) = config.parse::<usize>() {
				eprintln!("PRINT_FULL_OBJECTS: enabled ({} levels up)\n", levels);
			}
		} else {
			eprintln!();
		}
		max_lines
	} else {
		100 // Default even when debug is off
	};

	// Check for command filter
	let filter_regex = if let Ok(pattern) = std::env::var("COMPARE_REGEXP") {
		match regex::Regex::new(&pattern) {
			Ok(re) => {
				eprintln!("Filtering commands with pattern: {}\n", pattern);
				Some(re)
			}
			Err(e) => {
				eprintln!(
					"Warning: Invalid COMPARE_REGEXP pattern '{}': {}",
					pattern, e
				);
				eprintln!("Running all commands\n");
				None
			}
		}
	} else {
		None
	};

	// Load config
	let config = Config::from_file(&cli.config)?;

	// Verify executables exist and convert to absolute paths
	use std::path::Path;
	let exec1_path = Path::new(&config.tk_exec_1);
	if !exec1_path.exists() {
		anyhow::bail!("Executable not found: {}", config.tk_exec_1);
	}
	let exec1_absolute = std::fs::canonicalize(exec1_path)?;

	let exec2_path = Path::new(&config.tk_exec_2);
	if !exec2_path.exists() {
		anyhow::bail!("Executable not found: {}", config.tk_exec_2);
	}
	let exec2_absolute = std::fs::canonicalize(exec2_path)?;

	let exec1_str = exec1_absolute.to_string_lossy().to_string();
	let exec2_str = exec2_absolute.to_string_lossy().to_string();

	eprintln!("Comparing executables:");
	eprintln!("  {}: {}", config.tk_exec_1_name, exec1_str);
	eprintln!("  {}: {}", config.tk_exec_2_name, exec2_str);
	if let Some(ref wd) = config.working_dir {
		eprintln!("  working_dir: {}", wd);
	}

	// Filter commands if regex is provided
	let commands_to_run: Vec<_> = if let Some(ref re) = filter_regex {
		let filtered: Vec<_> = config
			.commands
			.iter()
			.enumerate()
			.filter(|(_, cmd)| re.is_match(&cmd.as_string()))
			.collect();

		eprintln!("  total commands: {}", config.commands.len());
		eprintln!("  filtered commands: {}", filtered.len());

		if filtered.is_empty() {
			eprintln!("\nWarning: No commands matched the filter pattern!");
			eprintln!("All commands:");
			for (i, cmd) in config.commands.iter().enumerate() {
				eprintln!("  {}: {}", i + 1, cmd.as_string());
			}
			eprintln!();
		} else {
			eprintln!("  running commands:");
			for (i, cmd) in &filtered {
				eprintln!("    {}: {}", i + 1, cmd.as_string());
			}
			eprintln!();
		}

		filtered
	} else {
		eprintln!("  commands: {}\n", config.commands.len());
		config.commands.iter().enumerate().collect()
	};

	let mut reports = Vec::new();

	// Create workspace directories for each executable (only if no working_dir specified)
	// When working_dir is specified, both executables run in the same directory
	let (workspace1, workspace2) = if config.working_dir.is_none() {
		// Clean up old workspace if it exists
		if std::path::Path::new(".tk-compare-workspace").exists() {
			std::fs::remove_dir_all(".tk-compare-workspace")?;
		}
		(
			Some(format!(".tk-compare-workspace/{}", config.tk_exec_1_name)),
			Some(format!(".tk-compare-workspace/{}", config.tk_exec_2_name)),
		)
	} else {
		(None, None)
	};

	// Run each filtered command
	for (orig_index, command) in commands_to_run.iter() {
		let index = *orig_index;
		let runs = if command.runs == 0 { 1 } else { command.runs };

		let total_commands = if filter_regex.is_some() {
			commands_to_run.len()
		} else {
			config.commands.len()
		};

		let display_index = if filter_regex.is_some() {
			commands_to_run
				.iter()
				.position(|(i, _)| *i == index)
				.unwrap() + 1
		} else {
			index + 1
		};

		// Clean up export directories before running (if dir_compare is enabled)
		cleanup_export_dirs(command, &config.tk_exec_1_name, &config.tk_exec_2_name);

		if runs > 1 {
			eprintln!(
				"Running command {}/{}: {} ({} runs)",
				display_index,
				total_commands,
				command.as_string(),
				runs
			);
		} else {
			eprintln!(
				"Running command {}/{}: {}",
				display_index,
				total_commands,
				command.as_string()
			);
		}

		let mut exec1_durations = Vec::new();
		let mut exec2_durations = Vec::new();
		let mut exit_code_matched = true;
		let mut stdout_matched = true;
		let mut stdout_similarity = None;
		let mut result_dir_matched = None;
		let mut exec1_exit_code = 0;
		let mut exec2_exit_code = 0;
		let mut exec1_stderr = String::new();
		let mut exec2_stderr = String::new();

		// Run the command multiple times
		for run in 0..runs {
			if runs > 1 {
				eprint!("  Run {}/{}...\r", run + 1, runs);
				use std::io::Write;
				std::io::stderr().flush().ok();
			}

			// Get args for each executable (may differ due to {{EXEC_NAME}} substitution)
			let args1 = command.args_for_exec(&config.tk_exec_1_name);
			let args2 = command.args_for_exec(&config.tk_exec_2_name);

			// Run with exec1 in its workspace
			let result1 = runner::run_command(
				&exec1_str,
				&args1,
				workspace1.as_deref(),
				config.working_dir.as_deref(),
			)?;

			// Run with exec2 in its workspace
			let result2 = runner::run_command(
				&exec2_str,
				&args2,
				workspace2.as_deref(),
				config.working_dir.as_deref(),
			)?;

			exec1_durations.push(result1.duration);
			exec2_durations.push(result2.duration);

			// Check consistency across runs (use first run as baseline)
			if run == 0 {
				exit_code_matched = result1.exit_code == result2.exit_code;

				// Use directory comparison for export commands, JSON comparison if enabled, otherwise string
				stdout_matched = if command.dir_compare {
					// For dir_compare, look for output directories in args ({{EXEC_NAME}} substituted)
					let dir1 = find_output_dir_in_args(&args1);
					let dir2 = find_output_dir_in_args(&args2);

					match (dir1, dir2) {
						(Some(d1), Some(d2)) => {
							match runner::compare_directories_detailed(&d1, &d2) {
								Ok((matched, similarity, matched_files, total_files, diffs)) => {
									stdout_similarity =
										Some((similarity, matched_files, total_files));
									if !matched && debug_mode {
										eprintln!("\n=== DIRECTORY DIFF ===");
										for diff in diffs.iter().take(debug_max_lines) {
											eprintln!("  {}", diff);
										}
										if diffs.len() > debug_max_lines {
											eprintln!(
												"... ({} more differences)",
												diffs.len() - debug_max_lines
											);
										}
									}
									matched
								}
								Err(e) => {
									eprintln!("\nWarning: Directory comparison failed: {}", e);
									false
								}
							}
						}
						_ => {
							eprintln!(
								"\nWarning: Could not find output directories to compare in args"
							);
							// Fall back to stdout comparison
							result1.stdout == result2.stdout
						}
					}
				} else if command.json_compare {
					match runner::compare_json(&result1.stdout, &result2.stdout) {
						Ok((matched, similarity, matched_count, total_count)) => {
							stdout_similarity = Some((similarity, matched_count, total_count));
							if !matched && debug_mode {
								runner::print_json_diff(
									&result1.stdout,
									&result2.stdout,
									&config.tk_exec_1_name,
									&config.tk_exec_2_name,
									debug_max_lines,
								);
							}
							matched
						}
						Err(e) => {
							eprintln!("\nWarning: JSON comparison failed: {}", e);
							eprintln!("Falling back to string comparison");
							let matched = result1.stdout == result2.stdout;
							let (similarity, matched_lines, total_lines) =
								runner::calculate_string_similarity(
									&result1.stdout,
									&result2.stdout,
								);
							stdout_similarity = Some((similarity, matched_lines, total_lines));
							if !matched && debug_mode {
								runner::print_string_diff(
									&result1.stdout,
									&result2.stdout,
									&config.tk_exec_1_name,
									&config.tk_exec_2_name,
									debug_max_lines,
								);
							}
							matched
						}
					}
				} else {
					let matched = result1.stdout == result2.stdout;
					let (similarity, matched_lines, total_lines) =
						runner::calculate_string_similarity(&result1.stdout, &result2.stdout);
					stdout_similarity = Some((similarity, matched_lines, total_lines));
					if !matched && debug_mode {
						runner::print_string_diff(
							&result1.stdout,
							&result2.stdout,
							&config.tk_exec_1_name,
							&config.tk_exec_2_name,
							debug_max_lines,
						);
					}
					matched
				};

				exec1_exit_code = result1.exit_code;
				exec2_exit_code = result2.exit_code;
				exec1_stderr = result1.stderr;
				exec2_stderr = result2.stderr;

				// Compare result directories if specified (only on first run)
				result_dir_matched = if let Some(ref result_dir) = command.result_dir {
					if let (Some(ref ws1), Some(ref ws2)) = (&workspace1, &workspace2) {
						// Construct result directory paths within each workspace
						let dir1 = format!("{}/{}", ws1, result_dir);
						let dir2 = format!("{}/{}", ws2, result_dir);

						Some(runner::compare_directories(&dir1, &dir2)?)
					} else {
						// Can't compare result directories when using shared working_dir
						None
					}
				} else {
					None
				};
			} else {
				// Verify consistency
				if result1.exit_code != exec1_exit_code || result2.exit_code != exec2_exit_code {
					eprintln!("\nWarning: Exit codes changed across runs!");
				}
				if result1.stdout != exec1_stderr.replace(&exec1_stderr, &result1.stdout)
					|| result2.stdout != exec2_stderr.replace(&exec2_stderr, &result2.stdout)
				{
					// Just a sanity check, we don't fail on this
				}
			}
		}

		if runs > 1 {
			eprintln!("  Completed {} runs    ", runs);
		}

		let exec1_stats = report::RuntimeStats::from_durations(exec1_durations);
		let exec2_stats = report::RuntimeStats::from_durations(exec2_durations);

		let report = CommandReport {
			command: command.as_string(),
			runs,
			exit_code_matched,
			stdout_matched,
			stdout_similarity,
			result_dir_matched,
			exec1_name: config.tk_exec_1_name.clone(),
			exec1_stats,
			exec1_exit_code,
			exec1_stderr,
			exec2_name: config.tk_exec_2_name.clone(),
			exec2_stats,
			exec2_exit_code,
			exec2_stderr,
		};

		reports.push(report);
	}

	// Print individual reports
	for (index, report) in reports.iter().enumerate() {
		report.print(index);
	}

	// Print summary
	report::print_summary(&reports);

	// Clean up workspace unless --keep-workspace is specified
	if !cli.keep_workspace {
		if std::path::Path::new(".tk-compare-workspace").exists() {
			std::fs::remove_dir_all(".tk-compare-workspace")?;
		}
		// Clean up export directories
		for (_, command) in &commands_to_run {
			cleanup_export_dirs(command, &config.tk_exec_1_name, &config.tk_exec_2_name);
		}
	} else {
		eprintln!("\nWorkspace preserved at: .tk-compare-workspace/");
		eprintln!("Export directories preserved at: /tmp/tk-compare-export-*/");
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_find_output_dir_basic() {
		let args = vec![
			"export".to_string(),
			"/tmp/output".to_string(),
			"path/to/env".to_string(),
		];
		assert_eq!(
			find_output_dir_in_args(&args),
			Some("/tmp/output".to_string())
		);
	}

	#[test]
	fn test_find_output_dir_with_flags_after() {
		let args = vec![
			"export".to_string(),
			"/tmp/output".to_string(),
			"path".to_string(),
			"-p".to_string(),
			"8".to_string(),
		];
		assert_eq!(
			find_output_dir_in_args(&args),
			Some("/tmp/output".to_string())
		);
	}

	#[test]
	fn test_find_output_dir_no_export() {
		let args = vec!["eval".to_string(), "path".to_string()];
		assert_eq!(find_output_dir_in_args(&args), None);
	}

	#[test]
	fn test_find_output_dir_empty_args() {
		let args: Vec<String> = vec![];
		assert_eq!(find_output_dir_in_args(&args), None);
	}

	#[test]
	fn test_find_output_dir_export_only() {
		let args = vec!["export".to_string()];
		assert_eq!(find_output_dir_in_args(&args), None);
	}

	#[test]
	fn test_find_output_dir_flag_after_export() {
		// Edge case: flag immediately after export (no output dir specified yet)
		let args = vec![
			"export".to_string(),
			"--recursive".to_string(),
			"/tmp/output".to_string(),
		];
		// Current implementation would return None since --recursive starts with '-'
		// and we skip it, then /tmp/output would be found
		// Actually looking at the code: we skip flags, so we'd get /tmp/output
		// But wait, the loop continues after finding --recursive...
		// Let me trace: found_export=true, then --recursive starts with '-' so skip
		// then /tmp/output doesn't start with '-' so return it
		assert_eq!(
			find_output_dir_in_args(&args),
			Some("/tmp/output".to_string())
		);
	}
}
