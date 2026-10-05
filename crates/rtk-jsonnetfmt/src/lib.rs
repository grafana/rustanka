//! Tanka-compatible Jsonnet discovery and go-jsonnet's lexical model.

pub mod files;
pub mod fodder;
pub mod lexer;
pub mod location;
pub mod string_util;
pub mod token;

pub use files::{find_files, find_files_all};

/// A go-jsonnet-compatible static error.
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
