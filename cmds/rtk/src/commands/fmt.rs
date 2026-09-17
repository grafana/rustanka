//! Fmt command handler.
//!
//! `tk fmt` is `cmd/tk/fmt.go` → `tanka.FormatFiles` →
//! `formatter.Format(name, content, formatter.DefaultOptions())`, so the
//! formatting is [`rtk_jsonnetfmt::format_default`] and everything here is the
//! CLI surface around it: the order of operations, the streams and the exit
//! codes. **The order is part of the contract**, not a presentation choice, so
//! the steps below are numbered the way `docs/rtk-fmt-plan.md` numbers them and
//! the code follows them in that order.
//!
//! # The streams are the thing to be careful about
//!
//! This is where the command can be wrong while looking right. `--verbose` and
//! `--stdout` write to **stdout**; every summary line, `--verbose`'s single
//! trailing blank line and `--stdout`'s per-file separator go to **stderr**. A
//! test that merges the two cannot see any of it, which is why
//! `cmds/rtk/tests/fmt_parity_test.rs` captures them separately.
//!
//! Stdout goes through the caller's writer so that `rtk fmt --stdout … | head`
//! is a clean exit rather than a broken-pipe error; stderr is written directly,
//! as `tk` writes it.
//!
//! # Four behaviours that read like bugs
//!
//! Each is upstream's, each is what `tk fmt` does, and each has a test of its
//! own:
//!
//! - **`--test` beats `--stdout`.** The mode is chosen by testing `--test`
//!   first, so nothing is printed and nothing is written.
//! - **The output mode runs for every discovered file, changed or not.**
//!   `outFn` is called unconditionally in `FormatFiles`'s loop, so the default
//!   mode rewrites a file it did not change — moving its mtime — and `--stdout`
//!   prints files it did not change.
//! - **`-` is honoured only as the *sole* argument**, and that branch returns
//!   before the excludes are compiled. So `rtk fmt - --exclude '[a'` succeeds
//!   where `rtk fmt . --exclude '[a'` does not.
//! - **A file named twice is formatted twice**, discovery neither sorting nor
//!   deduplicating across arguments — see [`rtk_jsonnetfmt::files`], behaviour
//!   6. Whether it is *counted* twice then depends on the output mode, which is
//!   the part that surprises: under `--test` and `--stdout` both passes read
//!   the same unformatted bytes and both count, while the default mode writes
//!   the file on the first pass and so reads it back clean on the second and
//!   counts one. `tk` agrees, scenario for scenario.
//!
//! # This command is destructive, and does not iterate
//!
//! The default mode writes in place over whole trees, so twelve known
//! non-convergences of upstream's pipeline are now visible to users, and three
//! of them produce first-run output that looks broken — a leading blank line, a
//! body left unindented, a file starting with two spaces. That is what
//! `tk fmt` prints; `CLAUDE.md` has the account under "And that fodder move is
//! a second, worse non-convergence". **Do not add a convergence loop.**
//! Matching `tk fmt` on *one* run is the contract, and iterating diverges from
//! it on the first escaped field lookup or doubly-parenthesised expression in a
//! Grafana repo.

use std::{
	fs,
	io::{self, Read, Write},
	thread,
};

use anyhow::{Context, Result};
use clap::Args;

/// The diagnostic filename stdin is formatted under, from `cmd/tk/fmt.go`.
///
/// go-jsonnet's `DiagnosticFileName` never reaches the output, only the error
/// messages — so this is what a parse failure on piped input is reported
/// against.
const STDIN_NAME: &str = "<stdin>";

/// The stack the formatting runs on.
///
/// The port recurses to the nesting depth of its input in several independent
/// places — `Node::opening_fodder_mut`, the parser, every pass,
/// `FixIndentation::visit`, `Group::absorb` and the drop of the tree itself —
/// and there is no depth limit anywhere in `rtk-jsonnetfmt`. There must not be
/// one: a limit would refuse a file `tk fmt` formats, which is a divergence in
/// the one direction this project never accepts. Go grows a goroutine stack to
/// 1 GiB, so this matches it rather than guessing at a smaller number, and a
/// thread stack is reserved address space that only commits the pages it
/// touches.
///
/// It does **not** buy parity with `tk` at every depth, and the parity test
/// measured why: this port spends on the order of 20 KiB of stack per level of
/// nesting in an unoptimized build, so 1 GiB is about fifty thousand levels
/// there and more when optimized, while go-jsonnet's frames are far smaller
/// and reach deeper in the same gigabyte. Real Jsonnet nests to tens of levels,
/// so the gap is theoretical; raising this further is an arms race with no
/// natural stopping point, and reserving more than Go's own maximum would be
/// hard to justify.
///
/// Getting this wrong is worse than a wrong byte: the overflow prints
/// `thread 'rtk-fmt' has overflowed its stack` and aborts, so there is no exit
/// code, no summary, and the default output mode has already rewritten every
/// file it reached.
const STACK_SIZE: usize = 1024 * 1024 * 1024;

#[derive(Args)]
pub struct FmtArgs {
	/// Files or directories to format
	// `ArgsMin(1)`: `tk fmt` with no argument is an error, and no path is
	// defaulted in. `lint` carried a default of `"."` for a while and lost it
	// with this command, `tk lint` being `ArgsMin(1)` as well.
	#[arg(required = true)]
	pub paths: Vec<String>,

	/// Globs to exclude
	#[arg(short = 'e', long, default_values_t = rtk_gobwas_glob::TANKA_DEFAULT_EXCLUDES.map(String::from).to_vec())]
	pub exclude: Vec<String>,

	/// Print formatted contents to stdout instead of writing to disk
	#[arg(long)]
	pub stdout: bool,

	/// Exit with non-zero when changes would be made
	#[arg(short = 't', long)]
	pub test: bool,

	/// Print each checked file
	#[arg(short = 'v', long)]
	pub verbose: bool,
}

/// What happens to each formatted file, chosen once before the loop.
///
/// `tk` builds one `outFn` up front and calls it for every file, which is why
/// this is picked here rather than tested per file: the three-way choice is
/// what makes `--test` beat `--stdout`, and testing it once says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
	/// `--test`: nothing is printed and nothing is written.
	Discard,
	/// `--stdout`: `// {name}` and the contents, and a separator on stderr.
	Print,
	/// The default: written back in place.
	Write,
}

impl OutputMode {
	/// Step 4, and the whole of why `--test` beats `--stdout`: `--test` is
	/// tested first, so it answers before `--stdout` is looked at.
	fn chosen(args: &FmtArgs) -> Self {
		if args.test {
			Self::Discard
		} else if args.stdout {
			Self::Print
		} else {
			Self::Write
		}
	}
}

/// Whether the arguments ask for stdin.
///
/// `-` is honoured only as the **sole** argument, as `len(args) == 1 &&
/// args[0] == "-"` is. Among other paths it is an ordinary path and goes to
/// discovery, which fails to stat it.
fn is_stdin(paths: &[String]) -> bool {
	paths.len() == 1 && paths[0] == "-"
}

/// Run the fmt command.
///
/// Returns whether `--test` found something to change, which the caller turns
/// into [`crate::commands::diff::EXIT_CODE_DIFF_FOUND`] — the same 16 `tk`
/// exits with, and the same constant `diff` uses.
pub fn run<W: Write + Send>(args: FmtArgs, writer: W) -> Result<bool> {
	// Everything the command does happens on the big stack, stdin included: a
	// deeply nested file is as likely to arrive down a pipe as to be named.
	on_a_large_stack(move || format_files(&args, writer))
}

/// Run `work` on a thread with a stack that arbitrary nesting cannot exhaust.
///
/// A scoped thread rather than a plain one so that `work` may borrow — the
/// caller's writer is not `'static`. `crates/rtk-environment/src/export/mod.rs`
/// does the same thing for Jsonnet evaluation with a rayon pool; this is one
/// thread because the formatting loop is sequential, as `FormatFiles` is.
fn on_a_large_stack(work: impl FnOnce() -> Result<bool> + Send) -> Result<bool> {
	thread::scope(|scope| {
		let worker = thread::Builder::new()
			.name("rtk-fmt".to_owned())
			.stack_size(STACK_SIZE)
			.spawn_scoped(scope, work)
			.context("spawning the formatting thread")?;
		match worker.join() {
			Ok(result) => result,
			// A panic in the formatter is a bug rather than a diagnostic, so it
			// is re-raised here instead of being flattened into an error the
			// user would read as their file's fault.
			Err(panic) => std::panic::resume_unwind(panic),
		}
	})
}

/// The command proper, in the order `cmd/tk/fmt.go` does it.
fn format_files<W: Write>(args: &FmtArgs, mut writer: W) -> Result<bool> {
	// 2. Stdin, before anything else. Honoured only as the sole argument, and
	//    returning here is what makes a malformed `--exclude` irrelevant to it.
	if is_stdin(&args.paths) {
		return format_stdin(args, &mut writer);
	}

	// 3. Compile the excludes, before any file is read, so a malformed glob
	//    aborts the run with nothing touched. The message is gobwas' own, which
	//    `rtk-gobwas-glob` reproduces.
	let excludes = rtk_gobwas_glob::compile_all(&args.exclude)?;

	// Discovery, wrapped the way `tk` wraps it.
	let files =
		rtk_jsonnetfmt::find_files_all(&args.paths, &excludes).context("finding Jsonnet files")?;

	// 4. One output mode for the whole run, `--test` tested first.
	let mode = OutputMode::chosen(args);

	let mut changed: Vec<&str> = Vec::new();
	for file in &files {
		// Go reads bytes and converts; this refuses input that is not UTF-8,
		// which the lexer would refuse a moment later anyway.
		let content = fs::read_to_string(file).with_context(|| format!("reading {file}"))?;

		// A parse failure aborts the whole run: this returns before the
		// summary, so no count is printed, and whatever the loop has already
		// written stays written — exactly as `tk` leaves it.
		let formatted = rtk_jsonnetfmt::format_default(file, &content)?;

		// 5. `--verbose`, on **stdout**. `FormatFiles`'s `printFn` is
		//    `fmt.Println(i...)` over two operands, so the literal `"ok "`
		//    gains a second space and `"fmt"` does not.
		if formatted == content {
			if args.verbose {
				writeln!(writer, "ok  {file}")?;
			}
		} else {
			changed.push(file);
			if args.verbose {
				writeln!(writer, "fmt {file}")?;
			}
		}

		// The output mode runs for every file, changed or not.
		match mode {
			OutputMode::Discard => {}
			OutputMode::Print => {
				write!(writer, "// {file}\n{formatted}")?;
				// Go's `os.Stdout` is unbuffered, so each file reaches the
				// terminal before the separator that follows it does.
				writer.flush()?;
				eprintln!();
			}
			OutputMode::Write => {
				// Whole-file, as `os.WriteFile` does. Its `0644` only applies
				// to a file being created, and this one was just read, so the
				// existing mode is kept either way.
				fs::write(file, &formatted).with_context(|| format!("writing {file}"))?;
			}
		}
	}
	writer.flush()?;

	// `--verbose`'s one blank line, after the loop and on stderr — the only
	// part of `--verbose` that is not on stdout.
	if args.verbose {
		eprintln!();
	}

	// 6. The summary, on stderr, a three-way switch in this order.
	if args.test && !changed.is_empty() {
		eprintln!("The following files are not properly formatted:");
		for file in &changed {
			eprintln!("{file}");
		}
		return Ok(true);
	}
	if changed.is_empty() {
		// Also what `--test` prints when everything is clean, exiting 0.
		eprintln!("All discovered files are already formatted. No changes were made");
	} else {
		eprintln!("Formatted {} files", changed.len());
	}
	Ok(false)
}

/// The `-` branch: all of stdin, formatted to stdout, and nothing else.
///
/// No `// name` header, no summary and no trailing newline of its own —
/// `format` already guarantees one. With `--test` the output is printed
/// **anyway** and only then does the difference become exit 16.
fn format_stdin<W: Write>(args: &FmtArgs, writer: &mut W) -> Result<bool> {
	let mut content = String::new();
	io::stdin()
		.read_to_string(&mut content)
		.context("reading standard input")?;

	let formatted = rtk_jsonnetfmt::format_default(STDIN_NAME, &content)?;
	write!(writer, "{formatted}")?;
	writer.flush()?;

	Ok(args.test && formatted != content)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The CLI surface is graded end to end — streams, exit codes and all — by
	/// `cmds/rtk/tests/fmt_parity_test.rs`, which runs the binary. What is in
	/// here is what a subprocess cannot reach: the deciding functions called
	/// directly.
	fn args_for(paths: &[&str], test: bool, stdout: bool) -> FmtArgs {
		FmtArgs {
			paths: paths.iter().map(|path| (*path).to_owned()).collect(),
			exclude: Vec::new(),
			stdout,
			test,
			verbose: false,
		}
	}

	#[test]
	fn test_beats_stdout() {
		assert_eq!(
			OutputMode::chosen(&args_for(&["."], true, true)),
			OutputMode::Discard,
			"--test is tested first, so it answers before --stdout is looked at"
		);
		assert_eq!(
			OutputMode::chosen(&args_for(&["."], true, false)),
			OutputMode::Discard
		);
		assert_eq!(
			OutputMode::chosen(&args_for(&["."], false, true)),
			OutputMode::Print
		);
		assert_eq!(
			OutputMode::chosen(&args_for(&["."], false, false)),
			OutputMode::Write
		);
	}

	#[test]
	fn stdin_is_the_sole_argument_or_nothing() {
		let owned = |paths: &[&str]| -> Vec<String> {
			paths.iter().map(|path| (*path).to_owned()).collect()
		};
		assert!(is_stdin(&owned(&["-"])));
		assert!(!is_stdin(&owned(&["-", "."])));
		assert!(!is_stdin(&owned(&[".", "-"])));
		assert!(!is_stdin(&owned(&["."])));
		assert!(!is_stdin(&owned(&[])));
	}

	#[test]
	fn a_malformed_exclude_is_refused_before_a_file_is_read() {
		// The observable part — that nothing was rewritten — needs the
		// binary and lives in the parity test. What is here is that the
		// failure comes from compiling the glob at all, with gobwas' message.
		let directory = tempfile::tempdir().expect("a temporary directory");
		let file = directory.path().join("main.jsonnet");
		fs::write(&file, "{\"a\": 1}\n").expect("the fixture is written");

		let path = directory.path().to_string_lossy().into_owned();
		let args = FmtArgs {
			exclude: vec!["[a".to_owned()],
			..args_for(&[path.as_str()], false, false)
		};
		let error = format_files(&args, io::sink()).expect_err("a malformed glob is an error");
		assert!(
			error.to_string().contains("unexpected end of input"),
			"{error}"
		);
		assert_eq!(
			fs::read_to_string(&file).expect("the fixture is still there"),
			"{\"a\": 1}\n",
			"the run must abort before any file is read"
		);
	}
}
