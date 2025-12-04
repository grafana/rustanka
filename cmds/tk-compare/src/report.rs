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
	pub exit_codes_consistent: bool, // True if exit codes were same across all runs
	pub stdout_matched: bool,
	pub stdout_similarity: Option<(f64, usize, usize)>, // (percentage, matched_lines, total_lines) - Line similarity
	pub semantic_similarity: Option<(f64, usize, usize)>, // (percentage, matched_lines, total_lines) - Semantic similarity (for export commands)
	pub is_export_command: bool, // True if this is an export/dir_compare command
	pub both_failed_unexpectedly: bool, // True if both commands failed but expect_error was false
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
		let _ = index; // Suppress unused variable warning
		println!("\n{}", "=== Command ===".bold());
		println!("Command: {}", self.command.cyan());

		// Exit code
		let exit_code_status = if self.both_failed_unexpectedly {
			"✗ BOTH FAILED (expected success)".red()
		} else if !self.exit_codes_consistent {
			"✗ INCONSISTENT ACROSS RUNS".red()
		} else if self.exit_code_matched {
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

		// Line Similarity
		let output_status = if self.stdout_matched {
			"✓ MATCHED".green()
		} else {
			"✗ MISMATCH".red()
		};
		if let Some((similarity, matched, total)) = self.stdout_similarity {
			println!(
				"Line Similarity: {} ({:.1}% similar: {}/{} matching)",
				output_status, similarity, matched, total
			);
		} else {
			println!("Line Similarity: {}", output_status);
		}

		// Semantic Similarity (for export commands)
		if self.is_export_command {
			if let Some((similarity, matched, total)) = self.semantic_similarity {
				let semantic_status = if matched == total {
					"✓ MATCHED".green()
				} else {
					"✗ MISMATCH".red()
				};
				println!(
					"Semantic Similarity: {} ({:.1}% similar: {}/{} matching)",
					semantic_status, similarity, matched, total
				);
			}
		}

		// Show runtime comparison regardless of result match
		if self.runs > 1 {
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
			let ratio = if exec1_ms > 0 {
				exec2_ms as f64 / exec1_ms as f64
			} else {
				0.0
			};

			print!("  Comparison (median): ");
			if ratio > 1.0 {
				println!(
					"{} is {:.2}x slower ({} vs {})",
					self.exec2_name,
					ratio,
					format_duration(self.exec2_stats.median),
					format_duration(self.exec1_stats.median)
				);
			} else if ratio < 1.0 && ratio > 0.0 {
				println!(
					"{} is {:.2}x faster ({} vs {})",
					self.exec2_name,
					1.0 / ratio,
					format_duration(self.exec2_stats.median),
					format_duration(self.exec1_stats.median)
				);
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
			let ratio = if exec1_ms > 0 {
				exec2_ms as f64 / exec1_ms as f64
			} else {
				0.0
			};

			if ratio > 1.0 {
				println!(
					"  {} is {:.2}x slower ({} vs {})",
					self.exec2_name,
					ratio,
					format_duration(self.exec2_stats.average),
					format_duration(self.exec1_stats.average)
				);
			} else if ratio < 1.0 && ratio > 0.0 {
				println!(
					"  {} is {:.2}x faster ({} vs {})",
					self.exec2_name,
					1.0 / ratio,
					format_duration(self.exec2_stats.average),
					format_duration(self.exec1_stats.average)
				);
			}
		}
		// End of runtime comparison

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
	let exit_code_matches = reports
		.iter()
		.filter(|r| r.exit_code_matched && r.exit_codes_consistent && !r.both_failed_unexpectedly)
		.count();
	let stdout_matches = reports.iter().filter(|r| r.stdout_matched).count();

	// Calculate output near-matches (>= 99.5% similarity)
	let stdout_near_matches = reports
		.iter()
		.filter(|r| {
			if let Some((similarity, _, _)) = r.stdout_similarity {
				similarity >= 99.5
			} else {
				false
			}
		})
		.count();

	println!("Total commands: {}", total);
	println!("Exit code matches: {}/{}", exit_code_matches, total);
	println!(
		"Line Similarity matches: {}/{} ({} near-matches at >= 99.5%)",
		stdout_matches, total, stdout_near_matches
	);

	let all_passed = exit_code_matches == total
		&& stdout_matches == total
		&& reports.iter().all(|r| r.exit_codes_consistent)
		&& !reports.iter().any(|r| r.both_failed_unexpectedly);

	if all_passed {
		println!("\n{}", "✓ All tests passed!".green().bold());
	} else {
		println!("\n{}", "✗ Some tests failed!".red().bold());
	}
}

/// Generate GitHub-compatible markdown comment for CI
pub fn generate_github_comment(reports: &[CommandReport], exec1_name: &str, exec2_name: &str) {
	println!("\n<!-- GITHUB_COMMENT_START -->");
	println!("## 🔬 Tanka Comparison Results");
	println!();
	println!(
		"Comparing [Grafana Tanka](https://github.com/grafana/tanka) `{}` with `{}` (rustanka):",
		exec1_name, exec2_name
	);
	println!();

	// Combined Correctness and Performance table
	println!("### Summary");
	println!();

	// Check if any reports are export commands to determine column layout
	let has_export_commands = reports.iter().any(|r| r.is_export_command);

	if has_export_commands {
		println!("| Command | Exit Code | Line Similarity | Semantic Similarity | Performance |");
		println!("|---------|-----------|-----------------|---------------------|-------------|");
	} else {
		println!("| Command | Exit Code | Line Similarity | Performance |");
		println!("|---------|-----------|-----------------|-------------|");
	}

	for report in reports {
		// Exit code status with emojis
		let exit_status = if report.both_failed_unexpectedly {
			"❌"
		} else if !report.exit_codes_consistent {
			"❌"
		} else if report.exit_code_matched {
			"✅"
		} else {
			"❌"
		};

		// Line Similarity status (three levels) with emojis
		let line_similarity_status = if report.stdout_matched {
			"✅".to_string()
		} else if let Some((similarity, _, _)) = report.stdout_similarity {
			if similarity >= 99.5 {
				format!("⚠️ {:.1}%", similarity)
			} else {
				format!("❌ {:.1}%", similarity)
			}
		} else {
			"❌".to_string()
		};

		// Semantic Similarity status (for export commands)
		let semantic_similarity_status = if report.is_export_command {
			if let Some((similarity, matched, total)) = report.semantic_similarity {
				if matched == total {
					"✅".to_string()
				} else if similarity >= 99.5 {
					format!("⚠️ {:.1}%", similarity)
				} else {
					format!("❌ {:.1}%", similarity)
				}
			} else {
				"N/A".to_string()
			}
		} else {
			"N/A".to_string()
		};

		// Performance status (show for all runs, use median for multiple runs, average for single run)
		let performance = {
			let exec1_ms = if report.runs > 1 {
				report.exec1_stats.median.as_millis()
			} else {
				report.exec1_stats.average.as_millis()
			};
			let exec2_ms = if report.runs > 1 {
				report.exec2_stats.median.as_millis()
			} else {
				report.exec2_stats.average.as_millis()
			};
			let ratio = if exec1_ms > 0 {
				exec2_ms as f64 / exec1_ms as f64
			} else {
				0.0
			};

			let exec2_time = if report.runs > 1 {
				format_duration(report.exec2_stats.median)
			} else {
				format_duration(report.exec2_stats.average)
			};
			let exec1_time = if report.runs > 1 {
				format_duration(report.exec1_stats.median)
			} else {
				format_duration(report.exec1_stats.average)
			};

			let (emoji, speed_text) = if ratio >= 0.9 && ratio <= 1.1 {
				("⚖️", format!("~equal ({} vs {})", exec2_time, exec1_time))
			} else if ratio > 1.1 {
				(
					"🐢",
					format!("{:.2}x slower ({} vs {})", ratio, exec2_time, exec1_time),
				)
			} else if ratio < 0.9 && ratio > 0.0 {
				let speedup = 1.0 / ratio;
				let emoji = if speedup >= 3.0 {
					"🚀"
				} else if speedup >= 1.5 {
					"🏎️"
				} else {
					"🐎"
				};
				(
					emoji,
					format!("{:.2}x faster ({} vs {})", speedup, exec2_time, exec1_time),
				)
			} else {
				("⚡", "same".to_string())
			};

			format!("{} {}", emoji, speed_text)
		};

		// Truncate command if too long
		let cmd = if report.command.len() > 50 {
			format!("{}...", &report.command[..47])
		} else {
			report.command.clone()
		};

		if has_export_commands {
			println!(
				"| `{}` | {} | {} | {} | {} |",
				cmd, exit_status, line_similarity_status, semantic_similarity_status, performance
			);
		} else {
			println!(
				"| `{}` | {} | {} | {} |",
				cmd, exit_status, line_similarity_status, performance
			);
		}
	}

	println!();

	// Overall summary stats
	let total = reports.len();
	let exit_code_matches = reports
		.iter()
		.filter(|r| r.exit_code_matched && r.exit_codes_consistent && !r.both_failed_unexpectedly)
		.count();
	let stdout_matches = reports.iter().filter(|r| r.stdout_matched).count();
	let stdout_near_matches = reports
		.iter()
		.filter(|r| {
			if let Some((similarity, _, _)) = r.stdout_similarity {
				similarity >= 99.5
			} else {
				false
			}
		})
		.count();

	println!("### Overall");
	println!("- Exit code matches: {}/{}", exit_code_matches, total);
	println!(
		"- Line Similarity matches: {}/{} ({} near-matches at >= 99.5%)",
		stdout_matches, total, stdout_near_matches
	);
	println!();

	let all_passed = exit_code_matches == total
		&& stdout_matches == total
		&& reports.iter().all(|r| r.exit_codes_consistent)
		&& !reports.iter().any(|r| r.both_failed_unexpectedly);

	if all_passed {
		println!("✅ **All tests passed!**");
	} else {
		println!("❌ **Some tests failed** - see full output for details");
	}

	println!();
	println!("---");
	println!("📎 View full comparison output in the workflow logs");
	println!("{}", "<!-- GITHUB_COMMENT_END -->");
}
