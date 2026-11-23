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
