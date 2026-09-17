//! `rtk fmt` CLI parity tests.
//!
//! These grade the CLI surface rather than the formatter: the order of
//! operations, which stream each line lands on, what reaches disk, and what the
//! process exits with. The formatter itself is graded by
//! `crates/rtk-jsonnetfmt` — 138 corpus files, 15 fixtures and three oracles at
//! full parity — and nothing here re-grades it.
//!
//! # Why this runs the binary
//!
//! **The streams have to be captured separately.** `--verbose` and `--stdout`
//! write to stdout, while every summary line, `--verbose`'s trailing blank line
//! and `--stdout`'s per-file separator write to stderr. A test that merged them
//! could not tell a correct implementation from one that put the whole lot on
//! either stream. A subprocess is also the only way to see `ArgsMin(1)`, which
//! clap enforces before `run` is called, and the only way to see exit 16 rather
//! than the `bool` that produces it.
//!
//! Every expectation is written out in full rather than derived from the code
//! under test, so deleting the line that implements a behaviour fails a test
//! here.
//!
//! # The tk cross-check
//!
//! [`tk_agrees_on_the_streams_and_the_exit_codes`] puts the same scenarios
//! through `tk` and compares both streams, both exit codes and the files left
//! behind. It needs `tk` on `PATH` and **says loudly when it does not have
//! it**: a test that skips in silence is a test that measures nothing, which
//! this project has been bitten by more than once. `RTK_REQUIRE_TK=1` turns the
//! skip into a failure, which is what CI should set.
//!
//! The no-argument case is checked separately and more weakly, because rtk's
//! message and exit code there are clap's and tk's are go-clix's. What is
//! compared is that both refuse and that neither touches a file.

use std::{
	collections::BTreeMap,
	fs,
	io::Write,
	path::Path,
	process::{Command, Stdio},
};

/// Jsonnet that `tk fmt` rewrites: the field name loses its quotes and the
/// object gains its padding.
const UNFORMATTED: &str = "{\"a\": 1}\n";
/// What [`UNFORMATTED`] formats to.
const FORMATTED: &str = "{ a: 1 }\n";
/// Jsonnet that is already exactly `tk fmt`-clean.
const CLEAN: &str = "{ b: 2 }\n";

/// One run of one of the two binaries, with the streams kept apart.
struct Run {
	stdout: String,
	stderr: String,
	/// `None` when the process died on a signal — which is what a stack
	/// overflow looks like, so it is kept rather than flattened to a number.
	code: Option<i32>,
	status: String,
}

impl Run {
	/// Everything about the run, for an assertion message.
	fn describe(&self) -> String {
		format!(
			"status: {}\n--- stdout ({} bytes) ---\n{}\n--- stderr ({} bytes) ---\n{}\n---",
			self.status,
			self.stdout.len(),
			clip(&self.stdout),
			self.stderr.len(),
			clip(&self.stderr),
		)
	}
}

/// Keep a failure message readable when the output is a whole formatted file.
///
/// `get` rather than a slice, so that a cut landing inside a multi-byte
/// character gives up on clipping instead of panicking inside a message that is
/// already reporting a failure.
fn clip(text: &str) -> String {
	match text.get(..2000) {
		Some(head) => format!("{head}… [{} bytes elided]", text.len() - 2000),
		None => text.to_owned(),
	}
}

/// Run `binary` in `directory`, optionally feeding it stdin.
///
/// The environment is scrubbed of the three things that would otherwise let a
/// developer's shell write to the stderr being asserted: `RUST_LOG` and the two
/// OpenTelemetry endpoints `telemetry::init` looks for. Nothing being asserted
/// is a tracing event — the summary lines are `eprintln!`s — so the log level
/// cannot hide a real expectation.
fn run_binary(binary: &str, directory: &Path, arguments: &[&str], stdin: Option<&str>) -> Run {
	let mut command = Command::new(binary);
	command
		.current_dir(directory)
		.args(arguments)
		.env("RUST_LOG", "error")
		.env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
		.env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.stdin(if stdin.is_some() {
			Stdio::piped()
		} else {
			Stdio::null()
		});

	let mut child = command
		.spawn()
		.unwrap_or_else(|error| panic!("{binary} runs: {error}"));
	if let Some(input) = stdin {
		let mut pipe = child.stdin.take().expect("stdin was piped");
		// Ignored on purpose: a command that never reads stdin closes the pipe,
		// and the broken pipe is the answer rather than a test failure.
		let _ = pipe.write_all(input.as_bytes());
	}
	let output = child.wait_with_output().expect("the child is waited on");

	Run {
		stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
		stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
		code: output.status.code(),
		// `Display` rather than `Debug`: it names the signal, which is what a
		// stack overflow reports instead of an exit code.
		status: output.status.to_string(),
	}
}

/// `rtk fmt …`, from the binary this test builds against.
fn rtk_fmt(directory: &Path, arguments: &[&str], stdin: Option<&str>) -> Run {
	let mut all = vec!["fmt"];
	all.extend_from_slice(arguments);
	run_binary(env!("CARGO_BIN_EXE_rtk"), directory, &all, stdin)
}

/// A temporary tree holding the named files.
fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
	let directory = tempfile::Builder::new()
		.prefix("rtk-fmt")
		.tempdir()
		.expect("a temporary directory");
	for (name, content) in files {
		let path = directory.path().join(name);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).expect("the parent directory is created");
		}
		fs::write(&path, content).expect("the fixture is written");
	}
	directory
}

/// The two-file tree most of these tests use: one file to change, one not to.
///
/// The names decide the order, discovery walking lexically, so `a.jsonnet` is
/// always the one that changes and `b.jsonnet` the one that does not.
fn two_files() -> tempfile::TempDir {
	tree(&[("a.jsonnet", UNFORMATTED), ("b.jsonnet", CLEAN)])
}

fn read(directory: &Path, name: &str) -> String {
	fs::read_to_string(directory.join(name)).expect("the file is readable")
}

// ---------------------------------------------------------------------------
// 1. ArgsMin(1)
// ---------------------------------------------------------------------------

#[test]
fn no_argument_is_an_error_and_formats_nothing() {
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &[], None);

	assert_ne!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(outcome.stdout, "", "{}", outcome.describe());
	assert!(
		outcome.stderr.contains("PATHS"),
		"the error should name the missing argument\n{}",
		outcome.describe()
	);
	// The observable half of `ArgsMin(1)`: no path is defaulted in, so the tree
	// is untouched. A default of `"."` would have rewritten `a.jsonnet`.
	assert_eq!(read(directory.path(), "a.jsonnet"), UNFORMATTED);
	assert_eq!(read(directory.path(), "b.jsonnet"), CLEAN);
}

#[test]
fn lint_requires_a_path_too() {
	// `tk lint` is `ArgsMin(1)` as well, and `rtk lint` defaulted to `"."`
	// until this phase. The test lives here because this is the file with a
	// harness that can see an exit code coming out of clap.
	let directory = two_files();
	let outcome = run_binary(env!("CARGO_BIN_EXE_rtk"), directory.path(), &["lint"], None);

	assert_ne!(outcome.code, Some(0), "{}", outcome.describe());
	assert!(
		outcome.stderr.contains("PATHS"),
		"the error should name the missing argument\n{}",
		outcome.describe()
	);
}

// ---------------------------------------------------------------------------
// 2. Stdin, before anything else
// ---------------------------------------------------------------------------

#[test]
fn stdin_is_formatted_to_stdout_with_no_summary() {
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["-"], Some(UNFORMATTED));

	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	// A bare `fmt.Print`: no `// <stdin>` header, and no newline of its own.
	assert_eq!(outcome.stdout, FORMATTED, "{}", outcome.describe());
	// The stdin branch returns before there is a summary to print.
	assert_eq!(outcome.stderr, "", "{}", outcome.describe());
	// And `-` is the sole argument, so the tree is never discovered.
	assert_eq!(read(directory.path(), "a.jsonnet"), UNFORMATTED);
}

#[test]
fn stdin_with_test_prints_the_output_anyway_and_exits_16() {
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["--test", "-"], Some(UNFORMATTED));

	assert_eq!(outcome.code, Some(16), "{}", outcome.describe());
	assert_eq!(
		outcome.stdout,
		FORMATTED,
		"--test prints the formatted output anyway, and only then reports the difference\n{}",
		outcome.describe()
	);
	assert_eq!(outcome.stderr, "", "{}", outcome.describe());
}

#[test]
fn stdin_with_test_on_clean_input_exits_zero() {
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["--test", "-"], Some(CLEAN));

	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(outcome.stdout, CLEAN, "{}", outcome.describe());
	assert_eq!(outcome.stderr, "", "{}", outcome.describe());
}

#[test]
fn stdin_ignores_a_malformed_exclude_where_a_path_does_not() {
	// The stdin branch returns before the excludes are compiled, so the same
	// flag that aborts a path run is irrelevant here. The pair is the point:
	// neither half alone says the branch sits in the right place.
	let directory = two_files();
	let stdin = rtk_fmt(
		directory.path(),
		&["-", "--exclude", "[a"],
		Some(UNFORMATTED),
	);
	assert_eq!(stdin.code, Some(0), "{}", stdin.describe());
	assert_eq!(stdin.stdout, FORMATTED, "{}", stdin.describe());
	assert_eq!(stdin.stderr, "", "{}", stdin.describe());

	let path = rtk_fmt(directory.path(), &[".", "--exclude", "[a"], None);
	assert_ne!(path.code, Some(0), "{}", path.describe());
	assert!(
		path.stderr.contains("unexpected end of input"),
		"{}",
		path.describe()
	);
}

#[test]
fn a_dash_among_other_paths_is_an_ordinary_path() {
	// `-` is honoured only as the *sole* argument, so this one goes to
	// discovery, which cannot stat it.
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["-", "b.jsonnet"], None);

	assert_ne!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(outcome.stdout, "", "{}", outcome.describe());
	assert!(
		outcome.stderr.contains("finding Jsonnet files"),
		"a discovery failure is wrapped the way tk wraps it\n{}",
		outcome.describe()
	);
}

// ---------------------------------------------------------------------------
// 3. The excludes, compiled before any file is read
// ---------------------------------------------------------------------------

#[test]
fn a_malformed_exclude_aborts_before_any_file_is_read() {
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &[".", "--exclude", "[a"], None);

	assert_ne!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(outcome.stdout, "", "{}", outcome.describe());
	assert!(
		outcome.stderr.contains("unexpected end of input"),
		"the message is gobwas' own\n{}",
		outcome.describe()
	);
	assert!(
		!outcome.stderr.contains("Formatted"),
		"no summary is printed\n{}",
		outcome.describe()
	);
	// "Before any file is read" is observable only as this: the file that
	// would otherwise have been rewritten was not.
	assert_eq!(read(directory.path(), "a.jsonnet"), UNFORMATTED);
}

#[test]
fn the_default_excludes_still_apply() {
	// Not a new behaviour — `find_files` is graded in its own crate — but the
	// CLI is what supplies the default list, so it is worth one assertion that
	// it supplies it at all.
	let directory = tree(&[
		("a.jsonnet", UNFORMATTED),
		("vendor/v.jsonnet", UNFORMATTED),
	]);
	let outcome = rtk_fmt(directory.path(), &["."], None);

	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(
		outcome.stderr,
		"Formatted 1 files\n",
		"{}",
		outcome.describe()
	);
	assert_eq!(read(directory.path(), "a.jsonnet"), FORMATTED);
	assert_eq!(
		read(directory.path(), "vendor/v.jsonnet"),
		UNFORMATTED,
		"the default `vendor/**` exclude"
	);
}

// ---------------------------------------------------------------------------
// 4. The output modes
// ---------------------------------------------------------------------------

#[test]
fn the_default_mode_writes_in_place_and_says_how_many() {
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["."], None);

	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(outcome.stdout, "", "{}", outcome.describe());
	// `Formatted %v files` — plural whatever the count.
	assert_eq!(
		outcome.stderr,
		"Formatted 1 files\n",
		"{}",
		outcome.describe()
	);
	assert_eq!(read(directory.path(), "a.jsonnet"), FORMATTED);
	assert_eq!(read(directory.path(), "b.jsonnet"), CLEAN);
}

#[test]
fn a_clean_tree_says_no_changes_were_made() {
	let directory = tree(&[("b.jsonnet", CLEAN)]);
	let outcome = rtk_fmt(directory.path(), &["."], None);

	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(outcome.stdout, "", "{}", outcome.describe());
	assert_eq!(
		outcome.stderr,
		"All discovered files are already formatted. No changes were made\n",
		"{}",
		outcome.describe()
	);
}

#[test]
fn the_default_mode_rewrites_a_file_it_did_not_change() {
	// `outFn` is called unconditionally in `FormatFiles`'s loop, so an
	// unchanged file is written back and its mtime moves. It reads like a bug
	// and it is what `tk fmt` does.
	//
	// The mtime is put in the past with `touch` rather than by sleeping, so the
	// assertion does not depend on the filesystem's timestamp granularity.
	let directory = tree(&[("b.jsonnet", CLEAN)]);
	let file = directory.path().join("b.jsonnet");
	let path = file.to_string_lossy().into_owned();
	let touched = Command::new("touch")
		.args(["-t", "202001010000", path.as_str()])
		.status()
		.expect("touch runs");
	assert!(touched.success(), "touch sets the mtime");

	let before = fs::metadata(&file)
		.expect("the file exists")
		.modified()
		.expect("an mtime");

	let outcome = rtk_fmt(directory.path(), &["."], None);
	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(
		outcome.stderr,
		"All discovered files are already formatted. No changes were made\n",
		"the file counts as unchanged, and is written back anyway\n{}",
		outcome.describe()
	);

	let after = fs::metadata(&file)
		.expect("the file exists")
		.modified()
		.expect("an mtime");
	assert_ne!(
		before, after,
		"an unchanged file is still written back, so its mtime moves"
	);
	assert_eq!(read(directory.path(), "b.jsonnet"), CLEAN);
}

#[test]
fn stdout_mode_prints_every_file_and_writes_none() {
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["--stdout", "."], None);

	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(
		outcome.stdout,
		format!("// a.jsonnet\n{FORMATTED}// b.jsonnet\n{CLEAN}"),
		"both files are printed, the unchanged one included\n{}",
		outcome.describe()
	);
	// One blank line per file, on stderr, and then the summary.
	assert_eq!(
		outcome.stderr,
		"\n\nFormatted 1 files\n",
		"the separators are on stderr, not stdout\n{}",
		outcome.describe()
	);
	assert_eq!(
		read(directory.path(), "a.jsonnet"),
		UNFORMATTED,
		"--stdout writes nothing"
	);
}

#[test]
fn test_mode_lists_the_files_and_exits_16_without_writing() {
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["--test", "."], None);

	assert_eq!(outcome.code, Some(16), "{}", outcome.describe());
	assert_eq!(outcome.stdout, "", "{}", outcome.describe());
	assert_eq!(
		outcome.stderr,
		"The following files are not properly formatted:\na.jsonnet\n",
		"only the changed file is listed\n{}",
		outcome.describe()
	);
	assert_eq!(read(directory.path(), "a.jsonnet"), UNFORMATTED);
}

#[test]
fn test_mode_on_a_clean_tree_exits_zero_with_the_no_changes_summary() {
	let directory = tree(&[("b.jsonnet", CLEAN)]);
	let outcome = rtk_fmt(directory.path(), &["--test", "."], None);

	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(outcome.stdout, "", "{}", outcome.describe());
	assert_eq!(
		outcome.stderr,
		"All discovered files are already formatted. No changes were made\n",
		"--test prints the same summary a clean default run does\n{}",
		outcome.describe()
	);
}

#[test]
fn test_beats_stdout() {
	// `--test` is tested first, so nothing is printed and nothing is written —
	// not the `// name` header, not the contents, not the stderr separator.
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["--test", "--stdout", "."], None);

	assert_eq!(outcome.code, Some(16), "{}", outcome.describe());
	assert_eq!(
		outcome.stdout,
		"",
		"--stdout prints nothing at all when --test is given\n{}",
		outcome.describe()
	);
	assert_eq!(
		outcome.stderr,
		"The following files are not properly formatted:\na.jsonnet\n",
		"and there is no per-file separator either\n{}",
		outcome.describe()
	);
	assert_eq!(read(directory.path(), "a.jsonnet"), UNFORMATTED);
}

// ---------------------------------------------------------------------------
// 5. --verbose, on stdout
// ---------------------------------------------------------------------------

#[test]
fn verbose_goes_to_stdout_and_only_its_blank_line_to_stderr() {
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["--verbose", "."], None);

	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	// `fmt` then one space; `"ok "` then `Println`'s space, so two.
	assert_eq!(
		outcome.stdout,
		"fmt a.jsonnet\nok  b.jsonnet\n",
		"the per-file lines are on stdout, and `ok` has two spaces\n{}",
		outcome.describe()
	);
	assert_eq!(
		outcome.stderr,
		"\nFormatted 1 files\n",
		"one blank line after the loop, and it is the only part on stderr\n{}",
		outcome.describe()
	);
}

#[test]
fn without_verbose_there_is_no_blank_line_on_stderr() {
	// The blank line belongs to `--verbose`. Asserted because an
	// unconditional one would pass every other test in this file.
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["."], None);

	assert_eq!(
		outcome.stderr,
		"Formatted 1 files\n",
		"{}",
		outcome.describe()
	);
	assert!(!outcome.stderr.starts_with('\n'), "{}", outcome.describe());
}

#[test]
fn verbose_and_stdout_share_stdout_in_loop_order() {
	// Both write to stdout, so their interleaving is observable: a file's
	// `fmt` line comes before that file's contents. This is the one ordering
	// in the CLI surface the plan does not spell out — `printFn` before
	// `outFn` within `FormatFiles`'s loop — so it is pinned here and
	// cross-checked against `tk` below.
	let directory = two_files();
	let outcome = rtk_fmt(directory.path(), &["--verbose", "--stdout", "."], None);

	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(
		outcome.stdout,
		format!("fmt a.jsonnet\n// a.jsonnet\n{FORMATTED}ok  b.jsonnet\n// b.jsonnet\n{CLEAN}"),
		"{}",
		outcome.describe()
	);
	assert_eq!(
		outcome.stderr,
		"\n\n\nFormatted 1 files\n",
		"two separators, then verbose's blank line, then the summary\n{}",
		outcome.describe()
	);
}

// ---------------------------------------------------------------------------
// 6. Discovery quirks the CLI is responsible for exposing
// ---------------------------------------------------------------------------

#[test]
fn a_file_named_twice_is_formatted_twice() {
	// Discovery neither sorts nor deduplicates across arguments, so the file is
	// read, formatted and handed to the output mode once per argument, and the
	// count is the length of the changed list rather than of a set.
	let directory = two_files();
	let test = rtk_fmt(
		directory.path(),
		&["--test", "a.jsonnet", "a.jsonnet"],
		None,
	);
	assert_eq!(test.code, Some(16), "{}", test.describe());
	assert_eq!(
		test.stderr,
		"The following files are not properly formatted:\na.jsonnet\na.jsonnet\n",
		"the same path is listed once per argument\n{}",
		test.describe()
	);

	// --stdout does not write, so both passes read the same unformatted bytes
	// and both count.
	let directory = two_files();
	let printed = rtk_fmt(
		directory.path(),
		&["--stdout", "a.jsonnet", "a.jsonnet"],
		None,
	);
	assert_eq!(printed.code, Some(0), "{}", printed.describe());
	assert_eq!(
		printed.stdout,
		format!("// a.jsonnet\n{FORMATTED}// a.jsonnet\n{FORMATTED}"),
		"printed once per argument\n{}",
		printed.describe()
	);
	assert_eq!(
		printed.stderr,
		"\n\nFormatted 2 files\n",
		"counted twice, because nothing was written between the two reads\n{}",
		printed.describe()
	);

	// The default mode is the case that reads like a bug and is not: the first
	// pass *writes* the formatted file, so the second pass reads it back clean
	// and does not count. So the count doubles only in a mode that leaves the
	// file alone — `tk` agrees, and `named-twice-writing` in the cross-check
	// below is what says so.
	let directory = two_files();
	let write = rtk_fmt(directory.path(), &["a.jsonnet", "a.jsonnet"], None);
	assert_eq!(write.code, Some(0), "{}", write.describe());
	assert_eq!(
		write.stderr,
		"Formatted 1 files\n",
		"the second pass reads what the first pass wrote\n{}",
		write.describe()
	);
	assert_eq!(read(directory.path(), "a.jsonnet"), FORMATTED);
}

#[test]
fn a_named_file_bypasses_the_excludes() {
	// `FindFiles` returns a named regular file before any glob runs, so this
	// formats a vendored file the default excludes would otherwise hide.
	let directory = tree(&[("vendor/v.jsonnet", UNFORMATTED)]);
	let outcome = rtk_fmt(directory.path(), &["vendor/v.jsonnet"], None);

	assert_eq!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(
		outcome.stderr,
		"Formatted 1 files\n",
		"{}",
		outcome.describe()
	);
	assert_eq!(read(directory.path(), "vendor/v.jsonnet"), FORMATTED);
}

// ---------------------------------------------------------------------------
// A parse failure aborts the whole run
// ---------------------------------------------------------------------------

#[test]
fn a_parse_failure_aborts_the_run_before_the_summary() {
	// Lexically `a_good` is formatted and written, and then `b_bad` fails — so
	// this also pins that the run dies part way with what it has already
	// written left written, which is `tk`'s behaviour.
	let directory = tree(&[("a_good.jsonnet", UNFORMATTED), ("b_bad.jsonnet", "{")]);
	let outcome = rtk_fmt(directory.path(), &["."], None);

	assert_ne!(outcome.code, Some(0), "{}", outcome.describe());
	assert_eq!(outcome.stdout, "", "{}", outcome.describe());
	assert!(
		outcome.stderr.contains("b_bad.jsonnet:"),
		"the message is go-jsonnet's, located in the offending file\n{}",
		outcome.describe()
	);
	assert!(
		!outcome.stderr.contains("Formatted") && !outcome.stderr.contains("All discovered"),
		"the run returns before the summary, so no count is printed\n{}",
		outcome.describe()
	);
	assert_eq!(
		read(directory.path(), "a_good.jsonnet"),
		FORMATTED,
		"what was written before the failure stays written"
	);
}

// ---------------------------------------------------------------------------
// The large stack
// ---------------------------------------------------------------------------

/// How deep the nesting test goes.
///
/// Two-sided, and **measured rather than guessed at**: 50,000 overflowed even
/// the command's 1 GiB in an unoptimized build, which puts the port at roughly
/// 20 KiB of stack per level of nesting — the parser alone spends several
/// frames per level, and each of twelve passes, `FixIndentation` and the drop
/// of the tree spend at least one. So 1 GiB buys about fifty thousand levels
/// unoptimized and more when optimized, and this sits an order of magnitude
/// inside it while still being one to two orders of magnitude past what an
/// 8 MiB default survives — which is around 400 levels unoptimized.
///
/// Both margins matter. Too deep and the test fails on the command's own stack
/// rather than grading it; too shallow and it would pass with the large-stack
/// thread deleted.
const NESTING_DEPTH: usize = 10_000;

#[test]
fn deeply_nested_input_formats_rather_than_overflowing_the_stack() {
	// Without the large-stack thread this is `thread '…' has overflowed its
	// stack` and a SIGABRT: no exit code, and in the default output mode some
	// files already rewritten. A depth *limit* is not the alternative — it
	// would refuse a file `tk fmt` formats, which is a divergence in the one
	// direction this project never accepts.
	let deep = format!(
		"{}{}\n",
		"[".repeat(NESTING_DEPTH),
		"]".repeat(NESTING_DEPTH)
	);
	let directory = tree(&[("deep.jsonnet", deep.as_str())]);
	let outcome = rtk_fmt(directory.path(), &["--stdout", "deep.jsonnet"], None);

	assert_eq!(
		outcome.code,
		Some(0),
		"a {NESTING_DEPTH}-deep file must format rather than die — status {}, stderr: {}",
		outcome.status,
		clip(&outcome.stderr)
	);
	// Nested arrays on one line have no fodder to move, so the formatter's
	// answer is the input back.
	assert!(
		outcome.stdout == format!("// deep.jsonnet\n{deep}"),
		"the deep file should come back unchanged; got {} bytes of stdout and {} of stderr:\n{}",
		outcome.stdout.len(),
		outcome.stderr.len(),
		clip(&outcome.stderr)
	);
}

// ---------------------------------------------------------------------------
// The tk cross-check
// ---------------------------------------------------------------------------

/// A scenario both binaries are put through, compared stream for stream.
struct Scenario {
	name: &'static str,
	arguments: &'static [&'static str],
	stdin: Option<&'static str>,
}

const SCENARIOS: &[Scenario] = &[
	Scenario {
		name: "default",
		arguments: &["."],
		stdin: None,
	},
	Scenario {
		name: "verbose",
		arguments: &["--verbose", "."],
		stdin: None,
	},
	Scenario {
		name: "stdout",
		arguments: &["--stdout", "."],
		stdin: None,
	},
	Scenario {
		name: "verbose-and-stdout",
		arguments: &["--verbose", "--stdout", "."],
		stdin: None,
	},
	Scenario {
		name: "test",
		arguments: &["--test", "."],
		stdin: None,
	},
	Scenario {
		name: "test-and-stdout",
		arguments: &["--test", "--stdout", "."],
		stdin: None,
	},
	Scenario {
		name: "test-and-verbose",
		arguments: &["--test", "--verbose", "."],
		stdin: None,
	},
	Scenario {
		name: "clean-file-only",
		arguments: &["b.jsonnet"],
		stdin: None,
	},
	Scenario {
		name: "named-twice",
		arguments: &["--test", "a.jsonnet", "a.jsonnet"],
		stdin: None,
	},
	Scenario {
		name: "named-twice-writing",
		arguments: &["a.jsonnet", "a.jsonnet"],
		stdin: None,
	},
	Scenario {
		name: "named-twice-stdout",
		arguments: &["--stdout", "a.jsonnet", "a.jsonnet"],
		stdin: None,
	},
	Scenario {
		name: "stdin",
		arguments: &["-"],
		stdin: Some(UNFORMATTED),
	},
	Scenario {
		name: "stdin-test",
		arguments: &["--test", "-"],
		stdin: Some(UNFORMATTED),
	},
	Scenario {
		name: "stdin-test-clean",
		arguments: &["--test", "-"],
		stdin: Some(CLEAN),
	},
	Scenario {
		name: "stdin-malformed-exclude",
		arguments: &["-", "--exclude", "[a"],
		stdin: Some(UNFORMATTED),
	},
];

/// The files a run left behind, so two trees can be compared.
fn contents(directory: &Path) -> BTreeMap<String, String> {
	let mut files = BTreeMap::new();
	for entry in walkdir::WalkDir::new(directory).sort_by_file_name() {
		let entry = entry.expect("the tree is walkable");
		if entry.file_type().is_file() {
			let name = entry
				.path()
				.strip_prefix(directory)
				.expect("below the directory")
				.to_string_lossy()
				.into_owned();
			files.insert(name, fs::read_to_string(entry.path()).unwrap_or_default());
		}
	}
	files
}

/// Whether `tk` can be run at all.
fn tk_available() -> bool {
	Command::new("tk")
		.arg("--version")
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.status()
		.is_ok()
}

/// Whether a missing `tk` is a failure rather than a skip.
///
/// CI sets it in the job that installs tk, so a tk that half-failed to install
/// cannot turn these two tests into green no-ops.
fn tk_required() -> bool {
	std::env::var("RTK_REQUIRE_TK").is_ok_and(|value| value == "1")
}

#[test]
fn tk_agrees_on_the_streams_and_the_exit_codes() {
	if !tk_available() {
		assert!(
			!tk_required(),
			"RTK_REQUIRE_TK=1, but `tk` is not on PATH — nothing was compared"
		);
		println!(
			"SKIPPED: `tk` is not on PATH, so 0 of {} scenarios were compared against it. \
			 Set RTK_REQUIRE_TK=1 to make this a failure.",
			SCENARIOS.len()
		);
		return;
	}

	let mut mismatches = Vec::new();
	for scenario in SCENARIOS {
		let ours = two_files();
		let theirs = two_files();
		let rtk = rtk_fmt(ours.path(), scenario.arguments, scenario.stdin);
		let mut arguments = vec!["fmt"];
		arguments.extend_from_slice(scenario.arguments);
		let tk = run_binary("tk", theirs.path(), &arguments, scenario.stdin);

		let mut differences = Vec::new();
		if rtk.stdout != tk.stdout {
			differences.push(format!(
				"stdout:\n  rtk: {:?}\n  tk:  {:?}",
				clip(&rtk.stdout),
				clip(&tk.stdout)
			));
		}
		if rtk.stderr != tk.stderr {
			differences.push(format!(
				"stderr:\n  rtk: {:?}\n  tk:  {:?}",
				clip(&rtk.stderr),
				clip(&tk.stderr)
			));
		}
		if rtk.code != tk.code {
			differences.push(format!(
				"exit code:\n  rtk: {:?} ({})\n  tk:  {:?} ({})",
				rtk.code, rtk.status, tk.code, tk.status
			));
		}
		let (our_files, their_files) = (contents(ours.path()), contents(theirs.path()));
		if our_files != their_files {
			differences.push(format!(
				"files:\n  rtk: {our_files:?}\n  tk:  {their_files:?}"
			));
		}

		if differences.is_empty() {
			println!("✓ {}", scenario.name);
		} else {
			println!("✗ {}", scenario.name);
			mismatches.push(format!(
				"=== {} (fmt {}) ===\n{}",
				scenario.name,
				scenario.arguments.join(" "),
				differences.join("\n")
			));
		}
	}

	println!(
		"compared {} of {} scenarios against tk without a difference",
		SCENARIOS.len() - mismatches.len(),
		SCENARIOS.len()
	);
	assert!(
		mismatches.is_empty(),
		"\n{} of {} scenarios differ from tk:\n\n{}",
		mismatches.len(),
		SCENARIOS.len(),
		mismatches.join("\n\n")
	);
}

#[test]
fn tk_also_refuses_to_run_without_a_path() {
	// Compared more weakly than the scenarios above, and deliberately: rtk's
	// message and exit code here are clap's, tk's are go-clix's, and neither
	// is the other's. What has to agree is that both refuse and that neither
	// formats anything.
	if !tk_available() {
		// Honours RTK_REQUIRE_TK for the same reason the test above does: this
		// one used to skip unconditionally, so in the CI job that has tk a
		// failed install would have silently dropped it.
		assert!(
			!tk_required(),
			"RTK_REQUIRE_TK=1, but `tk` is not on PATH — nothing was compared"
		);
		println!("SKIPPED: `tk` is not on PATH, so its no-argument behaviour was not compared.");
		return;
	}

	let theirs = two_files();
	let untouched = two_files();
	let tk = run_binary("tk", theirs.path(), &["fmt"], None);
	assert_ne!(tk.code, Some(0), "tk fmt with no path\n{}", tk.describe());
	assert_eq!(
		contents(theirs.path()),
		contents(untouched.path()),
		"tk formatted something despite refusing"
	);
}
