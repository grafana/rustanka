//! Tanka-compatible Jsonnet discovery and formatting.
//!
//! The formatter uses a Go-shaped, source-preserving AST and go-jsonnet's
//! ordered formatting passes. [`format_default_bytes`] accepts byte input and
//! native diagnostic paths, and [`Error::message_bytes`] retains exact errors.
//! [`format_default`] is the UTF-8 convenience interface.

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
pub mod text;
pub mod token;
pub mod unparse;
pub use text::Text;

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
/// Tanka never enables these options. The formatter and pass oracle therefore
/// omit them, rather than exposing flags that have no effect. Supporting them
/// requires implementing their transformations and adding live comparisons.
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

/// A go-jsonnet-compatible static error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Error {
	message: String,
	raw_message: Option<Vec<u8>>,
}

impl Error {
	fn with_value(location: &location::LocationRange, prefix: &str, value: &[u8]) -> Self {
		let mut message = prefix.as_bytes().to_vec();
		message.extend_from_slice(value);
		Self::from_static_bytes(location, &message)
	}

	/// Human-readable diagnostic. Use `message_bytes` or `write_to` for exact
	/// Tanka output: malformed escapes can produce invalid UTF-8 diagnostics.
	pub fn message(&self) -> &str {
		&self.message
	}

	/// Exact diagnostic bytes, without a trailing newline.
	pub fn message_bytes(&self) -> &[u8] {
		self.raw_message
			.as_deref()
			.unwrap_or(self.message.as_bytes())
	}

	/// Write the exact diagnostic without UTF-8 replacement or a trailing newline.
	pub fn write_to(&self, output: &mut impl std::io::Write) -> std::io::Result<()> {
		output.write_all(self.message_bytes())
	}

	pub fn from_static_bytes(location: &location::LocationRange, message: &[u8]) -> Self {
		let mut raw = if location.is_set() {
			location.render_bytes()
		} else {
			Vec::new()
		};
		raw.push(b' ');
		raw.extend_from_slice(message);
		match String::from_utf8(raw) {
			Ok(message) => Self {
				message,
				raw_message: None,
			},
			Err(invalid) => {
				let raw = invalid.into_bytes();
				Self {
					message: String::from_utf8_lossy(&raw).into_owned(),
					raw_message: Some(raw),
				}
			}
		}
	}

	pub fn from_static(location: &location::LocationRange, message: &str) -> Self {
		Self::from_static_bytes(location, message.as_bytes())
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
		if let Some(raw) = &mut self.raw_message {
			raw.extend_from_slice(b" while ");
			raw.extend_from_slice(context.as_bytes());
		}
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
	Ok(
		String::from_utf8(format_bytes(filename, input.as_bytes(), options)?)
			.expect("UTF-8 input produces UTF-8 output"),
	)
}

/// Format arbitrary source bytes, retaining native diagnostic filename bytes.
pub fn format_bytes(
	filename: impl AsRef<std::path::Path>,
	input: &[u8],
	options: &Options,
) -> Result<Vec<u8>, Error> {
	let (mut node, mut final_fodder) = parser::snippet_bytes_to_raw_ast(filename, input)?;

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
	Ok(unparser.finish_bytes())
}

/// Format with `Options::default` — Tanka's `Format`.
pub fn format_default(filename: &str, input: &str) -> Result<String, Error> {
	format(filename, input, &Options::default())
}

/// Format arbitrary bytes with Tanka's default options.
pub fn format_default_bytes(
	filename: impl AsRef<std::path::Path>,
	input: &[u8],
) -> Result<Vec<u8>, Error> {
	format_bytes(filename, input, &Options::default())
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
			raw_message: None,
		};
		assert_eq!(
			coalesce_error(Err(err)),
			"f.jsonnet:1:6 Unterminated String"
		);
		assert_eq!(coalesce_error(Ok("x\n".to_owned())), "x\n");
	}
}
