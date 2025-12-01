use anyhow::Result;
use clap::Parser;

mod config;
mod report;
mod runner;

use config::Config;
use report::CommandReport;

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
		eprintln!("DEBUG mode enabled (max {} diff lines)\n", max_lines);
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

			// Run with exec1 in its workspace
			let result1 = runner::run_command(
				&exec1_str,
				&command.args,
				workspace1.as_deref(),
				config.working_dir.as_deref(),
			)?;

			// Run with exec2 in its workspace
			let result2 = runner::run_command(
				&exec2_str,
				&command.args,
				workspace2.as_deref(),
				config.working_dir.as_deref(),
			)?;

			exec1_durations.push(result1.duration);
			exec2_durations.push(result2.duration);

			// Check consistency across runs (use first run as baseline)
			if run == 0 {
				exit_code_matched = result1.exit_code == result2.exit_code;

				// Use JSON comparison if enabled, otherwise use string comparison
				stdout_matched = if command.json_compare {
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
	} else {
		eprintln!("\nWorkspace preserved at: .tk-compare-workspace/");
	}

	Ok(())
}
