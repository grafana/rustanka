//! The source-preserving front end used by `jsonnetfmt`.
//!
//! At this point in the review stack formatting is deliberately only a
//! parse/unparse round trip. Formatting passes land in the following PRs.

pub mod ast;
pub mod files;
pub mod fodder;
pub mod lexer;
pub mod location;
pub mod parser;
pub mod string_util;
pub mod text;
pub mod token;
pub use text::Text;
pub mod unparse;

pub use files::{find_files, find_files_all};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringStyle {
	Double,
	Single,
	Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStyle {
	Hash,
	Slash,
	Leave,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Options {
	pub indent: usize,
	pub max_blank_lines: usize,
	pub string_style: StringStyle,
	pub comment_style: CommentStyle,
	pub pretty_field_names: bool,
	pub pad_arrays: bool,
	pub pad_objects: bool,
	pub sort_imports: bool,
	pub use_implicit_plus: bool,
	pub strip_everything: bool,
	pub strip_comments: bool,
	pub strip_all_but_comments: bool,
}

impl Default for Options {
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

/// Parse and unparse without applying formatting passes.
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
	let (node, final_fodder) = parser::snippet_bytes_to_raw_ast(filename, input)?;
	let mut unparser = unparse::Unparser::new(options.clone());
	unparser.unparse(&node, false);
	unparser.finish_file(&final_fodder);
	Ok(unparser.finish_bytes())
}

pub fn format_default(filename: &str, input: &str) -> Result<String, Error> {
	format(filename, input, &Options::default())
}

/// Format arbitrary bytes with the default options.
pub fn format_default_bytes(
	filename: impl AsRef<std::path::Path>,
	input: &[u8],
) -> Result<Vec<u8>, Error> {
	format_bytes(filename, input, &Options::default())
}

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
}
