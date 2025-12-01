use colored::Colorize;
use std::time::Duration;

/// Format a duration in a human-readable way
fn format_duration(duration: Duration) -> String {
	let millis = duration.as_millis();

	if millis < 1000 {
		format!("{}ms", millis)
	} else if millis < 60_000 {
		let secs = millis as f64 / 1000.0;
		format!("{:.2}s", secs)
	} else if millis < 3_600_000 {
		let mins = millis / 60_000;
		let secs = (millis % 60_000) as f64 / 1000.0;
		format!("{}m {:.1}s", mins, secs)
	} else {
		let hours = millis / 3_600_000;
		let mins = (millis % 3_600_000) / 60_000;
		let secs = (millis % 60_000) as f64 / 1000.0;
		format!("{}h {}m {:.1}s", hours, mins, secs)
	}
}

#[derive(Debug)]
pub struct RuntimeStats {
	pub min: Duration,
	pub max: Duration,
	pub median: Duration,
	pub average: Duration,
}

impl RuntimeStats {
	pub fn from_durations(mut durations: Vec<Duration>) -> Self {
		durations.sort();
		let min = *durations.first().unwrap_or(&Duration::ZERO);
		let max = *durations.last().unwrap_or(&Duration::ZERO);

		let median = if durations.is_empty() {
			Duration::ZERO
		} else if durations.len() % 2 == 0 {
			let mid = durations.len() / 2;
			(durations[mid - 1] + durations[mid]) / 2
		} else {
			durations[durations.len() / 2]
		};

		let total: Duration = durations.iter().sum();
		let average = if durations.is_empty() {
			Duration::ZERO
		} else {
			total / durations.len() as u32
		};

		Self {
			min,
			max,
			median,
			average,
		}
	}
}

#[derive(Debug)]
pub struct CommandReport {
	pub command: String,
	pub runs: usize,
	pub exit_code_matched: bool,
	pub stdout_matched: bool,
	pub result_dir_matched: Option<bool>,
	pub exec1_name: String,
	pub exec1_stats: RuntimeStats,
	pub exec1_exit_code: i32,
	pub exec1_stderr: String,
	pub exec2_name: String,
	pub exec2_stats: RuntimeStats,
	pub exec2_exit_code: i32,
	pub exec2_stderr: String,
}

impl CommandReport {
	pub fn print(&self, index: usize) {
		println!("\n{}", format!("=== Command {} ===", index + 1).bold());
		println!("Command: {}", self.command.cyan());

		// Exit code
		let exit_code_status = if self.exit_code_matched {
			"✓ MATCHED".green()
		} else {
			"✗ MISMATCH".red()
		};
		println!(
			"Exit Code: {} ({}: {}, {}: {})",
			exit_code_status,
			self.exec1_name,
			self.exec1_exit_code,
			self.exec2_name,
			self.exec2_exit_code
		);

		// Stdout
		let stdout_status = if self.stdout_matched {
			"✓ MATCHED".green()
		} else {
			"✗ MISMATCH".red()
		};
		println!("Stdout: {}", stdout_status);

		// Result dir
		if let Some(result_dir_matched) = self.result_dir_matched {
			let result_dir_status = if result_dir_matched {
				"✓ MATCHED".green()
			} else {
				"✗ MISMATCH".red()
			};
			println!("Result Dir: {}", result_dir_status);
		} else {
			println!("Result Dir: {}", "N/A".yellow());
		}

		// Only show runtime comparison if results match
		let results_match = self.exit_code_matched
			&& self.stdout_matched
			&& self.result_dir_matched.unwrap_or(true);

		if !results_match {
			println!(
				"\n{}",
				"⚠ Skipping runtime comparison due to result mismatch".yellow()
			);
			// Still show stderr if present
		} else if self.runs > 1 {
			println!("Runtime (across {} runs):", self.runs);
			println!("  {}:", self.exec1_name);
			println!("    min:     {}", format_duration(self.exec1_stats.min));
			println!("    max:     {}", format_duration(self.exec1_stats.max));
			println!("    median:  {}", format_duration(self.exec1_stats.median));
			println!("    average: {}", format_duration(self.exec1_stats.average));
			println!("  {}:", self.exec2_name);
			println!("    min:     {}", format_duration(self.exec2_stats.min));
			println!("    max:     {}", format_duration(self.exec2_stats.max));
			println!("    median:  {}", format_duration(self.exec2_stats.median));
			println!("    average: {}", format_duration(self.exec2_stats.average));

			// Compare based on median
			let exec1_ms = self.exec1_stats.median.as_millis();
			let exec2_ms = self.exec2_stats.median.as_millis();
			let ratio = if exec2_ms > 0 {
				exec1_ms as f64 / exec2_ms as f64
			} else {
				0.0
			};

			print!("  Comparison (median): ");
			if ratio > 1.0 {
				println!("{} is {:.2}x slower", self.exec1_name, ratio);
			} else if ratio < 1.0 && ratio > 0.0 {
				println!("{} is {:.2}x faster", self.exec1_name, 1.0 / ratio);
			} else {
				println!("same");
			}
		} else {
			println!("Runtime:");
			println!(
				"  {}: {}",
				self.exec1_name,
				format_duration(self.exec1_stats.average)
			);
			println!(
				"  {}: {}",
				self.exec2_name,
				format_duration(self.exec2_stats.average)
			);

			let exec1_ms = self.exec1_stats.average.as_millis();
			let exec2_ms = self.exec2_stats.average.as_millis();
			let ratio = if exec2_ms > 0 {
				exec1_ms as f64 / exec2_ms as f64
			} else {
				0.0
			};

			if ratio > 1.0 {
				println!("  {} is {:.2}x slower", self.exec1_name, ratio);
			} else if ratio < 1.0 && ratio > 0.0 {
				println!("  {} is {:.2}x faster", self.exec1_name, 1.0 / ratio);
			}
		}
		// End of runtime comparison (only shown when results match)

		// Stderr output
		if !self.exec1_stderr.is_empty() {
			println!("\n{}", format!("{} stderr:", self.exec1_name).yellow());
			println!("{}", self.exec1_stderr);
		}

		if !self.exec2_stderr.is_empty() {
			println!("\n{}", format!("{} stderr:", self.exec2_name).yellow());
			println!("{}", self.exec2_stderr);
		}
	}
}

pub fn print_summary(reports: &[CommandReport]) {
	println!("\n{}", "=== SUMMARY ===".bold());

	let total = reports.len();
	let exit_code_matches = reports.iter().filter(|r| r.exit_code_matched).count();
	let stdout_matches = reports.iter().filter(|r| r.stdout_matched).count();
	let result_dir_total = reports
		.iter()
		.filter(|r| r.result_dir_matched.is_some())
		.count();
	let result_dir_matches = reports
		.iter()
		.filter(|r| r.result_dir_matched == Some(true))
		.count();

	println!("Total commands: {}", total);
	println!("Exit code matches: {}/{}", exit_code_matches, total);
	println!("Stdout matches: {}/{}", stdout_matches, total);
	if result_dir_total > 0 {
		println!(
			"Result dir matches: {}/{}",
			result_dir_matches, result_dir_total
		);
	}

	let all_passed = exit_code_matches == total
		&& stdout_matches == total
		&& (result_dir_total == 0 || result_dir_matches == result_dir_total);

	if all_passed {
		println!("\n{}", "✓ All tests passed!".green().bold());
	} else {
		println!("\n{}", "✗ Some tests failed!".red().bold());
	}
}
