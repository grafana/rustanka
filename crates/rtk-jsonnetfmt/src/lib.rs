//! Tanka-compatible Jsonnet discovery and go-jsonnet's lexical model.

pub mod files;
pub mod fodder;
pub mod lexer;
pub mod location;
pub mod string_util;
pub mod text;
pub mod token;
pub use text::Text;

pub use files::{find_files, find_files_all};

/// A go-jsonnet-compatible static error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Error {
	message: String,
	raw_message: Option<Vec<u8>>,
}

impl Error {
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

	fn from_static_bytes(location: &location::LocationRange, message: &[u8]) -> Self {
		let mut error = Self::from_static(location, "");
		let mut raw = error.message.into_bytes();
		raw.extend_from_slice(message);
		match String::from_utf8(raw) {
			Ok(message) => error.message = message,
			Err(invalid) => {
				let raw = invalid.into_bytes();
				error.message = String::from_utf8_lossy(&raw).into_owned();
				error.raw_message = Some(raw);
			}
		}
		error
	}

	pub fn from_static(location: &location::LocationRange, message: &str) -> Self {
		let rendered = if location.is_set() {
			location.to_string()
		} else {
			String::new()
		};
		Self {
			message: format!("{rendered} {message}"),
			raw_message: None,
		}
	}

	pub fn with_context(location: &location::LocationRange, message: &str, context: &str) -> Self {
		Self::from_static(location, &format!("{message} while {context}"))
	}

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
