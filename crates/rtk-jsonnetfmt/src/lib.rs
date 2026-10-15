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
//! [`format`] parses, runs the passes that have landed, and unparses. Phases
//! 0, 1, 2a, 2b and 2c of `docs/rtk-fmt-plan.md` are done: the lexer, the AST,
//! the parser, the unparser, the [`pass`] traversal and five of the twelve
//! passes. The other seven are no-ops, so a file that needs one of them comes
//! back unformatted; `quarantine.toml` names the fixtures that leaves failing
//! and `testdata/corpus-baseline.toml` counts the files.

pub mod ast;
pub mod files;
pub mod fodder;
pub mod lexer;
pub mod location;
pub mod parser;
pub mod pass;
pub mod passes;
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

/// go-jsonnet's `formatter.Options`.
///
/// `tk` exposes none of these, so `Options::default` — go-jsonnet's
/// `DefaultOptions()` — is the only configuration that has to be correct. The
/// rest are carried because the passes are shared and because the upstream
/// regression tests for `FixParens` run with `use_implicit_plus` off.
#[derive(Debug, Clone, PartialEq, Eq)]
// A faithful port of a Go struct of eight bools; grouping them would hide the
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
	pub strip_everything: bool,
	pub strip_comments: bool,
	pub strip_all_but_comments: bool,
}

impl Default for Options {
	/// go-jsonnet's `DefaultOptions()`, which is what `tanka.Format` passes.
	///
	/// The three `strip_*` fields are absent from `DefaultOptions()` and so
	/// take Go's zero value.
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
			strip_everything: false,
			strip_comments: false,
			strip_all_but_comments: false,
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
/// `FormatNode`'s pipeline, with the seven passes that have not landed yet
/// missing from it. The order below is upstream's, read off `FormatNode`
/// rather than inferred, and the gaps are marked so the shape of what is left
/// stays visible:
///
/// | # | pass | state |
/// | --- | --- | --- |
/// | 1 | `SortImports` (a free function, not a visitor) | Phase 2f |
/// | 2 | `removeInitialNewlines` | Phase 2d |
/// | 3 | `EnforceMaxBlankLines` | Phase 2d |
/// | 4 | `FixNewlines` | Phase 2d |
/// | 5 | [`FixTrailingCommas`](passes::FixTrailingCommas) | **runs** |
/// | 6 | `FixParens` | Phase 2e |
/// | 7 | `RemovePlusObject` / `AddPlusObject` | Phase 2e |
/// | 8 | [`NoRedundantSliceColon`](passes::NoRedundantSliceColon) | **runs** |
/// | 9 | the three strip passes | skipped under `Options::default` |
/// | 10 | [`PrettyFieldNames`](passes::PrettyFieldNames) | **runs** |
/// | 11 | [`EnforceStringStyle`](passes::EnforceStringStyle) | **runs** |
/// | 12 | [`EnforceCommentStyle`](passes::EnforceCommentStyle) | **runs** |
/// | 13 | `FixIndentation` | Phase 2d |
/// | 14 | `removeExtraTrailingNewlines` | Phase 2d |
///
/// A file that needs one of the missing passes comes back unformatted. What
/// is already real is the refusal: a file that does not parse fails here with
/// go-jsonnet's message, which is what `tk fmt` prints before aborting the
/// whole run.
pub fn format(filename: &str, input: &str, options: &Options) -> Result<String, Error> {
	let (mut node, mut final_fodder) = parser::snippet_to_raw_ast(filename, input)?;

	pass::visit_file(&mut passes::FixTrailingCommas, &mut node, &mut final_fodder);
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
		assert!(!options.strip_everything);
		assert!(!options.strip_comments);
		assert!(!options.strip_all_but_comments);
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
