//! Lint command handler.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use jrsonnet_lint::{fix_ranges, lint_snippet, Diagnostic, LintConfig, ParseError};

use super::fmt::PathIoError;

/// Exit code when lint problems were found (for process::exit)
pub const EXIT_CODE_PROBLEMS: i32 = 2;

#[derive(Args)]
pub struct LintArgs {
	/// Files or directories to lint
	// `ArgsMin(1)`, as `tk lint` is. This used to default to `"."`, which was
	// a divergence carried with a note pointing at the `fmt` CLI; the two
	// commands share Tanka's discovery and now share its argument rule too.
	#[arg(required = true)]
	pub paths: Vec<PathBuf>,

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
		let code =
			LintSource::new(fs::read(file).map_err(|error| PathIoError::new("open", file, error))?);
		let (diagnostics, parse_errors) = lint_snippet(&code.decoded, &config);
		for e in parse_errors {
			emit_parse_error(file, &code, &e)?;
			had_problems = true;
		}

		if args.fix {
			let fixed_code = code.apply_fixes(&diagnostics);
			if fixed_code != code.original {
				fs::write(file, &fixed_code)
					.map_err(|error| PathIoError::new("open", file, error))?;
			}
			// Report any diagnostics that could not be auto-fixed
			for d in diagnostics.iter().filter(|d| d.fix.is_none()) {
				emit_diagnostic(file, &code, d)?;
				had_problems = true;
			}
		} else {
			for d in diagnostics {
				emit_diagnostic(file, &code, &d)?;
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

fn emit_parse_error(filename: &Path, code: &LintSource, e: &ParseError) -> io::Result<()> {
	let start: usize = e.range.start().into();
	let (line, col) = code.line_col(start);
	let mut stderr = io::stderr().lock();
	stderr.write_all(filename.as_os_str().as_encoded_bytes())?;
	writeln!(stderr, ":{line}:{col}: parse error: {}", e.message)
}

fn emit_diagnostic(filename: &Path, code: &LintSource, d: &Diagnostic) -> io::Result<()> {
	let start: usize = d.range.start().into();
	let (line, col) = code.line_col(start);
	let mut stderr = io::stderr().lock();
	stderr.write_all(filename.as_os_str().as_encoded_bytes())?;
	writeln!(stderr, ":{line}:{col}: [{}] {}", d.check, d.message)
}

/// The linter parses Unicode text; retain the bytes and a map back to them so
/// accepting Go-style invalid runes never changes unrelated content in --fix.
struct LintSource {
	original: Vec<u8>,
	decoded: String,
	original_offsets: Vec<usize>,
}

impl LintSource {
	fn new(original: Vec<u8>) -> Self {
		let mut decoded = String::with_capacity(original.len());
		let mut original_offsets = vec![0];
		let mut offset = 0;
		while offset < original.len() {
			match std::str::from_utf8(&original[offset..]) {
				Ok(valid) => {
					decoded.push_str(valid);
					original_offsets.extend(offset + 1..=original.len());
					break;
				}
				Err(error) => {
					let valid_end = offset + error.valid_up_to();
					decoded.push_str(
						std::str::from_utf8(&original[offset..valid_end])
							.expect("validated prefix"),
					);
					original_offsets.extend(offset + 1..=valid_end);
					offset = valid_end;
					// utf8.DecodeRuneInString consumes one byte on every error,
					// including a truncated sequence. from_utf8_lossy does not.
					decoded.push(char::REPLACEMENT_CHARACTER);
					original_offsets.extend([offset, offset, offset + 1]);
					offset += 1;
				}
			}
		}
		Self {
			original,
			decoded,
			original_offsets,
		}
	}

	fn apply_fixes(&self, diagnostics: &[Diagnostic]) -> Vec<u8> {
		let mut fixed = self.original.clone();
		for range in fix_ranges(&self.decoded, diagnostics).into_iter().rev() {
			fixed.drain(self.original_offsets[range.start]..self.original_offsets[range.end]);
		}
		fixed
	}

	fn line_col(&self, decoded_offset: usize) -> (usize, usize) {
		let prefix = &self.original[..self.original_offsets[decoded_offset]];
		let line = prefix.iter().filter(|&&byte| byte == b'\n').count() + 1;
		let col = prefix
			.iter()
			.rposition(|&byte| byte == b'\n')
			.map_or(prefix.len() + 1, |newline| prefix.len() - newline);
		(line, col)
	}
}

#[cfg(test)]
mod tests {
	use std::io::sink;

	use super::*;

	#[test]
	fn malformed_runes_have_individual_boundaries_and_original_columns() {
		let source = LintSource::new(b"'\xe2\x82' + foo".to_vec());
		assert_eq!(source.decoded, "'��' + foo");
		assert_eq!(source.line_col(source.decoded.find("foo").unwrap()), (1, 8));
		assert_eq!(source.original_offsets.len(), source.decoded.len() + 1);
	}

	#[test]
	fn byte_fixes_preserve_unedited_invalid_sequences_and_real_replacement_runes() {
		let source = LintSource::new(
			b"//\xff\nlocal unused = '\xe2\x82';\n['\xff', '\xef\xbf\xbd']\n".to_vec(),
		);
		let (diagnostics, errors) = lint_snippet(&source.decoded, &LintConfig::default());
		assert!(errors.is_empty());
		assert_eq!(diagnostics.len(), 1);
		assert_eq!(
			source.apply_fixes(&diagnostics),
			b"//\xff\n['\xff', '\xef\xbf\xbd']\n"
		);
	}

	#[test]
	fn run_rejects_invalid_disable_checks() {
		let args = LintArgs {
			paths: vec![".".into()],
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
			paths: vec![dir.path().to_path_buf()],
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
			paths: vec!["/nonexistent/path/for/rtk/lint/test".into()],
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
			paths: vec![path.clone()],
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
			paths: vec![path.clone()],
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
			paths: vec![".".into()],
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
			paths: vec![dir.path().to_path_buf()],
			exclude: vec!["**/lib".to_string()],
			parallelism: 4,
			disable_checks: vec![],
			fix: true,
		};
		assert!(run(args, sink()).is_ok());
	}
}
