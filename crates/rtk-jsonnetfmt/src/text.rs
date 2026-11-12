//! Byte-preserving Jsonnet text and Go-compatible UTF-8 decoding.

use std::fmt;

/// Go strings can hold arbitrary bytes. Token text and comments must retain
/// that distinction until the formatter explicitly decodes or writes them.
/// There is deliberately no implicit conversion to `str`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Text(Vec<u8>);

impl Text {
	pub fn as_bytes(&self) -> &[u8] {
		&self.0
	}

	pub fn as_str(&self) -> Result<&str, std::str::Utf8Error> {
		std::str::from_utf8(&self.0)
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}
}

impl fmt::Debug for Text {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self.as_str() {
			Ok(text) => text.fmt(f),
			Err(_) => self.0.fmt(f),
		}
	}
}

impl From<Vec<u8>> for Text {
	fn from(bytes: Vec<u8>) -> Self {
		Self(bytes)
	}
}

impl From<String> for Text {
	fn from(text: String) -> Self {
		Self(text.into_bytes())
	}
}

impl From<&str> for Text {
	fn from(text: &str) -> Self {
		Self(text.as_bytes().to_vec())
	}
}

impl From<&[u8]> for Text {
	fn from(bytes: &[u8]) -> Self {
		Self(bytes.to_vec())
	}
}

impl PartialEq<str> for Text {
	fn eq(&self, other: &str) -> bool {
		self.0 == other.as_bytes()
	}
}

impl PartialEq<&str> for Text {
	fn eq(&self, other: &&str) -> bool {
		self == *other
	}
}

/// `utf8.DecodeRuneInString`: every invalid byte consumes exactly one byte,
/// including incomplete sequences. Rust's `from_utf8_lossy` groups some of
/// those sequences, so it cannot implement this contract.
pub(crate) fn decode_rune(bytes: &[u8]) -> Option<(char, usize)> {
	let first = *bytes.first()?;
	let width = match first {
		0..=0x7f => return Some((char::from(first), 1)),
		0xc2..=0xdf => 2,
		0xe0..=0xef => 3,
		0xf0..=0xf4 => 4,
		_ => return Some((char::REPLACEMENT_CHARACTER, 1)),
	};
	if let Some(Ok(text)) = bytes.get(..width).map(std::str::from_utf8) {
		return Some((text.chars().next().expect("nonempty rune"), width));
	}
	Some((char::REPLACEMENT_CHARACTER, 1))
}

pub(crate) fn runes(mut bytes: &[u8]) -> impl Iterator<Item = char> + '_ {
	std::iter::from_fn(move || {
		let (rune, width) = decode_rune(bytes)?;
		bytes = &bytes[width..];
		Some(rune)
	})
}
