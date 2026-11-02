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
pub mod token;
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
}

impl Error {
	pub fn message(&self) -> &str {
		&self.message
	}

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

	pub fn with_context(location: &location::LocationRange, message: &str, context: &str) -> Self {
		Self::from_static(location, &format!("{message} while {context}"))
	}

	#[must_use]
	pub fn in_context(mut self, context: &str) -> Self {
		self.message.push_str(" while ");
		self.message.push_str(context);
		self
	}
}

/// Parse and unparse without applying formatting passes.
pub fn format(filename: &str, input: &str, options: &Options) -> Result<String, Error> {
	let (node, final_fodder) = parser::snippet_to_raw_ast(filename, input)?;
	let mut unparser = unparse::Unparser::new(options.clone());
	unparser.unparse(&node, false);
	unparser.finish_file(&final_fodder);
	Ok(unparser.finish())
}

pub fn format_default(filename: &str, input: &str) -> Result<String, Error> {
	format(filename, input, &Options::default())
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
