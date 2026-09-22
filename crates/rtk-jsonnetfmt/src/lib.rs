//! `jsonnetfmt`, as `tk fmt` runs it.
//!
//! `tk fmt` is `cmd/tk/fmt.go` → `tanka.FormatFiles` →
//! `formatter.Format(name, content, formatter.DefaultOptions())` from
//! go-jsonnet. There is no Tanka-specific formatting logic at all, so the whole
//! task is reproducing go-jsonnet's formatter under one fixed option set —
//! byte for byte, because formatting is destructive and in place. A near-miss
//! formatter rewrites files `tk` would have left alone, in a user's working
//! tree.
//!
//! # Not the other formatter
//!
//! `crates/jrsonnet-formatter` exists and `cmds/jrsonnet-fmt` drives it, but it
//! is **not** `jsonnetfmt`: it is a dprint-based, width-driven pretty-printer
//! that re-lays-out from scratch. `jsonnetfmt` is *fodder-preserving* — it
//! keeps the author's line structure and only normalises indentation, trailing
//! commas, redundant parens, quote style, comment style, blank-line runs and
//! import order. These are different algorithms, not different settings, which
//! is why this crate exists rather than a flag on that one.
//!
//! # State
//!
//! [`format`] parses, runs every step of `FormatNode`, and unparses.
//! **The port is complete**: the lexer, the AST,
//! the parser, the unparser, the [`pass`] traversal, all twelve passes, all
//! three of `FormatNode`'s non-pass steps, the CLI that drives them, and
//! acceptance against the real `tk` over 3,016 files of real Grafana Jsonnet —
//! 1,452 of them vendored — at byte parity, with nothing rewritten that `tk fmt`
//! had already formatted. `quarantine.toml` is empty.
//!
//! `testdata/corpus-baseline.toml` counts the files, and Phase 4 made that
//! count two counts. The breadth corpus is now **857 files in two sets**: the
//! 138 in-repo files every phase up to 3 reported as "138 of 138", and 719 more
//! from go-jsonnet's own root `testdata/` — Jsonnet written by people testing a
//! parser rather than a formatter, which is the breadth the in-repo set most
//! lacked, most of it having been `tk fmt`-clean before any pass existed. The
//! two sets are stored differently because the second set's *inputs* are not in
//! this repository; that file explains why, and why the in-repo set did not
//! simply grow.
//!
//! `cmds/rtk/src/commands/fmt.rs` is the caller, and it calls
//! [`format_default`] and [`find_files_all`] and nothing else. Two things it
//! owns are worth knowing from in here: it runs the formatting on a thread with
//! a 1 GiB stack, because this crate recurses to the nesting depth of its input
//! in several places and has no depth limit anywhere — deliberately, since a
//! limit would refuse a file `tk fmt` formats — and it does **not** iterate to
//! convergence, because matching `tk fmt` on one run is the contract and
//! twelve inputs do not settle in one. `tests/idempotence.rs` lists them.
//!
//! Three more, found by Phase 4's corpus set, are worse than unsettled: their
//! first-run output does not parse, so a convergence loop would not have saved
//! them either. `PrettyFieldNames` promotes a computed field inside an object
//! comprehension, which a comprehension may not have, and a `|||` block of
//! nothing but newlines loses the indent that held it together. Both are
//! upstream's, both are confirmed against the real `tk` on both runs, and
//! `tests/corpus.rs`'s `OUTPUT_DOES_NOT_REPARSE` pins them.
//!
//! One thing here is not a formatter at all. [`go_sort`] is a port of
//! `sort.Slice`, needed because [`sort_imports`] sorts by a key two imports
//! can share and Go's tie order is not Rust's — measured, not assumed.

pub mod ast;
pub mod files;
pub mod fix_indentation;
pub mod fodder;
pub mod go_sort;
pub mod lexer;
pub mod location;
pub mod parser;
pub mod pass;
pub mod passes;
pub mod sort_imports;
pub mod string_util;
pub mod token;
pub mod unparse;

pub use files::{find_files, find_files_all};

/// How the reformatter rewrites string literals.
///
/// Strings containing a `'` or a `"` use whichever syntax avoids escaping,
/// whatever this says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringStyle {
	/// `"this"`.
	Double,
	/// `'this'`.
	Single,
	/// Left as found.
	Leave,
}

/// How the reformatter rewrites comments.
///
/// A `#!` hashbang is always left alone, whatever this says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStyle {
	/// `#`.
	Hash,
	/// `//`.
	Slash,
	/// Left as found.
	Leave,
}

/// go-jsonnet's `formatter.Options`, less the three strip flags.
///
/// `tk` exposes none of these, so `Options::default` — go-jsonnet's
/// `DefaultOptions()` — is the only configuration that has to be correct. The
/// rest are carried because the passes are shared and because upstream's own
/// `TestFormatNoImplicitPlus` runs with `use_implicit_plus` off — which is the
/// only way to reach [`passes::AddPlusObject`].
///
/// # Why `StripEverything`, `StripComments` and `StripAllButComments` are not here
///
/// They were, as three `pub` bools that nothing read. Phase 3 is the first
/// time `Options` is handed to a caller, which is the moment a flag that
/// promises to strip comments and silently keeps them stops being harmless, so
/// they were removed rather than left to be found. Four things decided it:
///
/// - Nothing implements them. `format` has no step 9, and an absent field is
///   the only honest way to say so.
/// - `DefaultOptions()` skips all three, `tk` exposes no way to ask for them,
///   and no phase of the fmt port plan schedules them.
/// - The pass oracle deliberately does **not** dump them: two of the three
///   rewrite every tree they touch, which was about 270 of 356 changed cells
///   and most of a 23 MB file. So the passes would land ungraded, which is the
///   one thing this port does not do.
/// - Nothing in `tk` parity needs them, and parity is the whole contract.
///
/// The route back, should a caller ever want them: they are **one
/// `if / else if / else if` chain** at step 9 of `FormatNode`
/// (`internal/formatter/jsonnetfmt.go:178-184`) — mutually exclusive and
/// first-wins, *not* three independent `if`s — and adding their names to
/// `passNames` in `testdata/generate/_staged/passdump.go` is what makes the
/// oracle grade them.
#[derive(Debug, Clone, PartialEq, Eq)]
// A faithful port of a Go struct of plain bools; grouping them would hide the
// correspondence.
#[allow(clippy::struct_excessive_bools)]
pub struct Options {
	/// Spaces per level of indentation.
	pub indent: usize,
	/// Maximum run of consecutive blank lines.
	pub max_blank_lines: usize,
	pub string_style: StringStyle,
	pub comment_style: CommentStyle,
	/// Wrap field names in quotes only where they are needed.
	pub pretty_field_names: bool,
	/// Write arrays as `[ this ]` rather than `[this]`.
	pub pad_arrays: bool,
	/// Write objects as `{ this }` rather than `{this}`.
	pub pad_objects: bool,
	/// Sort the imports at the top of the file into groups by filename.
	pub sort_imports: bool,
	/// Remove the `+` where it is not required.
	pub use_implicit_plus: bool,
}

impl Default for Options {
	/// go-jsonnet's `DefaultOptions()`, which is what `tanka.Format` passes.
	fn default() -> Self {
		Self {
			indent: 2,
			max_blank_lines: 2,
			string_style: StringStyle::Single,
			comment_style: CommentStyle::Slash,
			use_implicit_plus: true,
			pretty_field_names: true,
			pad_arrays: false,
			pad_objects: true,
			sort_imports: true,
		}
	}
}

/// A parse failure, carrying go-jsonnet's message.
///
/// The text is part of the contract: a parse failure propagates out of
/// `FormatFiles` and aborts the run, and it is what upstream's own harness
/// folds into its goldens via `coalesceError`. The shape is
/// `<file>:<line>:<col> <message>`, from `staticError.Error()`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Error {
	message: String,
}

impl Error {
	/// The message, without the `Display` wrapper.
	pub fn message(&self) -> &str {
		&self.message
	}

	/// Go's `staticError.Error()`: `"{loc} {msg}"`.
	///
	/// The space is unconditional, and the location renders as the empty string
	/// when unset — so an error with no position starts with one, exactly as
	/// Go's does. That is reproduced rather than tidied because the goldens
	/// record it.
	pub fn from_static(location: &location::LocationRange, message: &str) -> Self {
		let rendered = if location.is_set() {
			location.to_string()
		} else {
			String::new()
		};
		Self {
			message: format!("{rendered} {message}"),
		}
	}

	/// Go's `staticError.WithContext`: `"{msg} while {context}"`.
	///
	/// The parser wraps with this, so the context lands inside the message and
	/// before the location, not after it.
	pub fn with_context(location: &location::LocationRange, message: &str, context: &str) -> Self {
		Self::from_static(location, &format!("{message} while {context}"))
	}

	/// `staticError.WithContext` applied to an error that already exists.
	///
	/// Go keeps the location and the message apart and renders them as
	/// `"{loc} {msg}"` only at the end, so wrapping the message leaves the
	/// location in front. This type keeps the rendered string instead — but
	/// since the location is a *prefix* of it, appending here gives the same
	/// answer as wrapping there. That equivalence is why the parser can
	/// propagate context without the error carrying its location separately.
	#[must_use]
	pub fn in_context(mut self, context: &str) -> Self {
		self.message.push_str(" while ");
		self.message.push_str(context);
		self
	}
}

/// Format `input` the way `jsonnetfmt` would.
///
/// `filename` is go-jsonnet's `DiagnosticFileName`: it never reaches the
/// output, only the error messages.
///
/// # Current behaviour
///
/// All fourteen steps of `FormatNode`, in upstream's order — read off
/// `FormatNode` rather than inferred:
///
/// | # | pass | state |
/// | --- | --- | --- |
/// | 1 | [`sortImports`](ast::Node::sort_imports) — not a visitor | **runs** |
/// | 2 | [`removeInitialNewlines`](ast::Node::remove_initial_newlines) | **runs** |
/// | 3 | [`EnforceMaxBlankLines`](passes::EnforceMaxBlankLines) | **runs** |
/// | 4 | [`FixNewlines`](passes::FixNewlines) | **runs** |
/// | 5 | [`FixTrailingCommas`](passes::FixTrailingCommas) | **runs** |
/// | 6 | [`FixParens`](passes::FixParens) | **runs** |
/// | 7 | [`RemovePlusObject`](passes::RemovePlusObject) or [`AddPlusObject`](passes::AddPlusObject) | **runs** |
/// | 8 | [`NoRedundantSliceColon`](passes::NoRedundantSliceColon) | **runs** |
/// | 9 | the three strip passes | **not ported** — see [`Options`] |
/// | 10 | [`PrettyFieldNames`](passes::PrettyFieldNames) | **runs** |
/// | 11 | [`EnforceStringStyle`](passes::EnforceStringStyle) | **runs** |
/// | 12 | [`EnforceCommentStyle`](passes::EnforceCommentStyle) | **runs** |
/// | 13 | [`FixIndentation`](fix_indentation::FixIndentation) | **runs** |
/// | 14 | [`removeExtraTrailingNewlines`](fodder::Fodder::remove_extra_trailing_newlines) | **runs** |
///
/// A file that does not parse fails here with go-jsonnet's message, which is
/// what `tk fmt` prints before aborting the whole run.
///
/// Step 7 is one `if` with two branches rather than two steps, and
/// `Options::default` takes the first — so `AddPlusObject` is reached only
/// through `use_implicit_plus: false`, which nothing but upstream's own
/// regression cases passes. Step 6 running before step 7 is load-bearing:
/// `((e))` is collapsed before `AddPlusObject` inserts any parentheses, and
/// the ones it inserts are never collapsed again.
///
/// **Four of the fourteen steps are not passes**, each for its own reason.
/// Step 1 is a free function over the whole file that rebuilds the top of the
/// tree rather than rewriting nodes, so [`pass`] does not apply to it at all;
/// it is [`ast::Node::sort_imports`] here, and
/// [`sort_imports`](crate::sort_imports) carries the account.
///
/// Steps 2 and 14 are unexported four-line functions in `jsonnetfmt.go`,
/// which is why the staged pass dumper cannot reach either — see
/// [`ast::Node::remove_initial_newlines`] for what that means for how they
/// are graded. Their **position** is the load-bearing part: step 2 runs
/// before `EnforceMaxBlankLines` at step 3, so a blank run at the top of a
/// file is deleted rather than clamped to two; step 14 runs after
/// `FixIndentation` at step 13, so it zeroes the blanks of whatever element
/// indentation left at the end of the file.
///
/// Step 13 is not a pass either, for a different reason: `FormatNode`
/// constructs a [`fix_indentation::FixIndentation`] and calls `VisitFile` on
/// it directly rather than through upstream's `visitFile` helper, so it has
/// its own walk and is not an implementation of [`pass::AstPass`] at all. It
/// therefore reaches the four fodder slots [`pass::base`] skips, which is why
/// a comment in `x in super` survives `EnforceCommentStyle` and is still
/// re-indented.
pub fn format(filename: &str, input: &str, options: &Options) -> Result<String, Error> {
	let (mut node, mut final_fodder) = parser::snippet_to_raw_ast(filename, input)?;

	// Step 1, and the only step that runs before `removeInitialNewlines` — so
	// the fodder it divides still carries a file's leading blank run.
	if options.sort_imports {
		node.sort_imports();
	}
	node.remove_initial_newlines();
	if options.max_blank_lines > 0 {
		pass::visit_file(
			&mut passes::EnforceMaxBlankLines::new(options.max_blank_lines),
			&mut node,
			&mut final_fodder,
		);
	}
	pass::visit_file(&mut passes::FixNewlines, &mut node, &mut final_fodder);
	pass::visit_file(&mut passes::FixTrailingCommas, &mut node, &mut final_fodder);
	pass::visit_file(&mut passes::FixParens, &mut node, &mut final_fodder);
	// The two branches of one `if`, not consecutive steps. `tk fmt` always
	// takes the first: `use_implicit_plus` is on in `DefaultOptions()`, and
	// `AddPlusObject` is reached only by upstream's own `TestFormatNoImplicitPlus`
	// and the `no_implicit_plus/` fixtures that port it.
	//
	// `FixParens` running first is load-bearing and must not be reordered:
	// `((e))` is collapsed before any parentheses are inserted here, and the
	// ones inserted here are never collapsed again.
	if options.use_implicit_plus {
		pass::visit_file(&mut passes::RemovePlusObject, &mut node, &mut final_fodder);
	} else {
		pass::visit_file(&mut passes::AddPlusObject, &mut node, &mut final_fodder);
	}
	pass::visit_file(
		&mut passes::NoRedundantSliceColon,
		&mut node,
		&mut final_fodder,
	);
	if options.pretty_field_names {
		pass::visit_file(&mut passes::PrettyFieldNames, &mut node, &mut final_fodder);
	}
	// Both gates are upstream's, and both are here rather than in the pass:
	// `Leave` means the pass is never constructed. Note `EnforceStringStyle`
	// would behave as `Double` if it were, since it asks only whether the
	// style is `Single`.
	if options.string_style != StringStyle::Leave {
		pass::visit_file(
			&mut passes::EnforceStringStyle::new(options.string_style),
			&mut node,
			&mut final_fodder,
		);
	}
	if options.comment_style != CommentStyle::Leave {
		pass::visit_file(
			&mut passes::EnforceCommentStyle::new(options.comment_style),
			&mut node,
			&mut final_fodder,
		);
	}

	// Not a pass: `FormatNode` constructs this one and calls `VisitFile` on it
	// directly, bypassing the `pass.ASTPass` machinery.
	if options.indent > 0 {
		fix_indentation::FixIndentation::new(options).visit_file(&mut node, &mut final_fodder);
	}
	final_fodder.remove_extra_trailing_newlines();

	let mut unparser = unparse::Unparser::new(options.clone());
	unparser.unparse(&node, false);
	unparser.finish_file(&final_fodder);
	Ok(unparser.finish())
}

/// Format with `Options::default` — Tanka's `Format`.
pub fn format_default(filename: &str, input: &str) -> Result<String, Error> {
	format(filename, input, &Options::default())
}

/// Upstream's `coalesceError`: the formatted output, or the error message in
/// its place.
///
/// go-jsonnet's own formatter tests grade broken input this way, so the
/// fixtures can hold either kind of answer in one file.
pub fn coalesce_error(result: Result<String, Error>) -> String {
	match result {
		Ok(out) => out,
		Err(err) => err.message().to_owned(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn default_options_are_go_jsonnets() {
		let options = Options::default();
		assert_eq!(options.indent, 2);
		assert_eq!(options.max_blank_lines, 2);
		assert_eq!(options.string_style, StringStyle::Single);
		assert_eq!(options.comment_style, CommentStyle::Slash);
		assert!(options.use_implicit_plus);
		assert!(options.pretty_field_names);
		assert!(!options.pad_arrays);
		assert!(options.pad_objects);
		assert!(options.sort_imports);
	}

	#[test]
	fn coalesce_error_prefers_the_message() {
		let err = Error {
			message: "f.jsonnet:1:6 Unterminated String".to_owned(),
		};
		assert_eq!(
			coalesce_error(Err(err)),
			"f.jsonnet:1:6 Unterminated String"
		);
		assert_eq!(coalesce_error(Ok("x\n".to_owned())), "x\n");
	}
}
