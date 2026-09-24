//! Lint command handler.

use std::fs;
use std::io::Write;

use anyhow::Result;
use clap::Args;
use jrsonnet_lint::{apply_fixes, lint_snippet, Diagnostic, LintConfig, ParseError};

/// Exit code when lint problems were found (for process::exit)
pub const EXIT_CODE_PROBLEMS: i32 = 2;

#[derive(Args)]
pub struct LintArgs {
	/// Files or directories to lint
	// `ArgsMin(1)`, as `tk lint` is. This used to default to `"."`, which was
	// a divergence carried with a note pointing at the `fmt` CLI; the two
	// commands share Tanka's discovery and now share its argument rule too.
	#[arg(required = true)]
	pub paths: Vec<String>,

	/// Globs to exclude
	#[arg(short = 'e', long, default_values_t = rtk_gobwas_glob::TANKA_DEFAULT_EXCLUDES.map(String::from).to_vec())]
	pub exclude: Vec<String>,

	/// Amount of workers
	#[arg(short = 'n', long, default_value = "4")]
	pub parallelism: i32,

	/// Disable specific checks (comma-separated). Valid: unused_locals
	#[arg(long = "disable-checks", value_name = "CHECKS", value_delimiter = ',')]
	pub disable_checks: Vec<String>,

	/// Automatically fix lint issues where possible
	#[arg(long = "fix")]
	pub fix: bool,
}

/// Run the lint command.
pub fn run<W: Write>(args: LintArgs, _writer: W) -> Result<()> {
	let config = LintConfig::default()
		.with_disabled_checks(&args.disable_checks)
		.map_err(anyhow::Error::msg)?;

	// `tk lint` and `tk fmt` share Tanka's `jsonnet.FindFiles`, so rtk shares
	// the port of it. What this replaced got three things wrong: it pruned
	// excluded directories (`FindFiles` returns `nil`, not `fs.SkipDir`, so a
	// `**/vendor/**` exclude walks the whole tree and discards it file by
	// file), it followed symlinks where `filepath.WalkDir` does not, and it
	// sniffed at the exclude patterns instead of compiling them — so `*` did
	// not cross `/` and anything but the four default patterns was ignored.
	let excludes = rtk_gobwas_glob::compile_all(&args.exclude)?;
	let all_files = rtk_jsonnetfmt::find_files_all(&args.paths, &excludes)?;

	let mut had_problems = false;
	for file in &all_files {
		let code = fs::read_to_string(file)?;
		let (diagnostics, parse_errors) = lint_snippet(&code, &config);
		for e in parse_errors {
			emit_parse_error(file, &code, &e);
			had_problems = true;
		}

		if args.fix {
			let fixed_code = apply_fixes(&code, &diagnostics);
			if fixed_code != code {
				fs::write(file, &fixed_code)?;
			}
			// Report any diagnostics that could not be auto-fixed
			for d in diagnostics.iter().filter(|d| d.fix.is_none()) {
				emit_diagnostic(file, &code, d);
				had_problems = true;
			}
		} else {
			for d in diagnostics {
				emit_diagnostic(file, &code, &d);
				had_problems = true;
			}
		}
	}

	if had_problems {
		eprintln!("Problems found!");
		std::process::exit(EXIT_CODE_PROBLEMS);
	}
	Ok(())
}

fn emit_parse_error(filename: &str, code: &str, e: &ParseError) {
	let start: usize = e.range.start().into();
	let (line, col) = offset_to_line_col(code, start);
	eprintln!("{}:{}:{}: parse error: {}", filename, line, col, e.message);
}

fn emit_diagnostic(filename: &str, code: &str, d: &Diagnostic) {
	let start: usize = d.range.start().into();
	let (line, col) = offset_to_line_col(code, start);
	eprintln!("{}:{}:{}: [{}] {}", filename, line, col, d.check, d.message);
}

fn offset_to_line_col(code: &str, offset: usize) -> (u32, u32) {
	let mut line = 1u32;
	let mut col = 1u32;
	for (i, c) in code.char_indices() {
		if i >= offset {
			break;
		}
		if c == '\n' {
			line += 1;
			col = 1;
		} else {
			col += 1;
		}
	}
	(line, col)
}

#[cfg(test)]
mod tests {
	use std::io::sink;

	use super::*;

	#[test]
	fn run_rejects_invalid_disable_checks() {
		let args = LintArgs {
			paths: vec![".".to_string()],
			exclude: vec![],
			parallelism: 4,
			disable_checks: vec!["no_such_check".to_string()],
			fix: false,
		};
		let result = run(args, sink());
		assert!(result.is_err());
		let err = result.unwrap_err().to_string();
		assert!(err.contains("unknown check"));
		assert!(err.contains("no_such_check"));
	}

	#[test]
	fn run_accepts_valid_disable_checks() {
		let dir = tempfile::tempdir().unwrap();
		let args = LintArgs {
			paths: vec![dir.path().to_string_lossy().to_string()],
			exclude: vec![],
			parallelism: 4,
			disable_checks: vec!["unused_locals".to_string()],
			fix: false,
		};
		let result = run(args, sink());
		assert!(result.is_ok());
	}

	#[test]
	fn run_on_nonexistent_path_errors() {
		let args = LintArgs {
			paths: vec!["/nonexistent/path/for/rtk/lint/test".to_string()],
			exclude: vec![],
			parallelism: 4,
			disable_checks: vec![],
			fix: false,
		};
		let result = run(args, sink());
		assert!(result.is_err());
	}

	#[test]
	fn run_on_dir_with_clean_jsonnet_returns_ok() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("main.jsonnet");
		std::fs::write(&path, "local x = 1; x\n").unwrap();
		let args = LintArgs {
			paths: vec![path.to_string_lossy().to_string()],
			exclude: vec![],
			parallelism: 4,
			disable_checks: vec![],
			fix: false,
		};
		let result = run(args, sink());
		assert!(result.is_ok());
	}

	#[test]
	fn run_fix_writes_back_fixed_file() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("main.jsonnet");
		std::fs::write(&path, "local x = 1;\nlocal y = 2;\ny\n").unwrap();
		let args = LintArgs {
			paths: vec![path.to_string_lossy().to_string()],
			exclude: vec![],
			parallelism: 4,
			disable_checks: vec![],
			fix: true,
		};
		let result = run(args, sink());
		assert!(result.is_ok());
		let content = std::fs::read_to_string(&path).unwrap();
		assert_eq!(content, "local y = 2;\ny\n", "file should be fixed");
	}

	#[test]
	fn run_rejects_an_uncompilable_exclude_pattern() {
		// `tk lint` returns whatever `glob.Compile` says, before it looks at a
		// single file. The message is gobwas' own.
		let args = LintArgs {
			paths: vec![".".to_string()],
			exclude: vec!["[a".to_string()],
			parallelism: 4,
			disable_checks: vec![],
			fix: false,
		};
		let err = run(args, sink()).expect_err("a malformed glob is an error");
		assert!(err.to_string().contains("unexpected end of input"), "{err}");
	}

	#[test]
	fn run_does_not_prune_an_excluded_directory() {
		// The behaviour the hand-rolled exclude matcher got wrong: `FindFiles`
		// skips directories before consulting the excludes, so a pattern that
		// matches a directory's own path prunes nothing under it.
		let dir = tempfile::Builder::new()
			.prefix("rtk-lint-exclude")
			.tempdir()
			.unwrap();
		std::fs::create_dir(dir.path().join("lib")).unwrap();
		std::fs::write(dir.path().join("lib/main.jsonnet"), "local x = 1;\nx\n").unwrap();

		let args = LintArgs {
			paths: vec![dir.path().to_string_lossy().to_string()],
			exclude: vec!["**/lib".to_string()],
			parallelism: 4,
			disable_checks: vec![],
			fix: true,
		};
		assert!(run(args, sink()).is_ok());
	}
}
