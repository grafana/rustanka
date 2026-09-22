//! A port of go-jsonnet's `internal/parser/string_util.go`.
//!
//! Two functions, both of which the formatter reaches:
//!
//! - [`string_unescape`] is called by the parser to *validate* every single-
//!   and double-quoted string. Its result is thrown away there; only whether it
//!   failed, and the message if it did, reach the output.
//! - [`string_escape`] is `EnforceStringStyle`'s half of the same job, and
//!   lands with that pass.
//!
//! One upstream bug is reproduced deliberately. Both "malformed" messages
//! interpolate `s[0:4]` — the first four bytes of the **whole string**, not of
//! the offending escape — so `'ab\uZZZZ'` reports `ab\u`. It is quoted in
//! `formatter/testdata` goldens by way of parse errors, so fixing it here would
//! be a divergence.

use std::fmt::Write as _;

use crate::{Error, location::LocationRange};

/// Go's `StringUnescape`: resolve the escape sequences of a string literal.
///
/// Indices are **byte** offsets throughout, as Go's are, because the error
/// messages and the surrogate-pair lookahead are both defined in terms of
/// them.
pub fn string_unescape(location: &LocationRange, text: &str) -> Result<String, Error> {
	let bytes = text.as_bytes();
	let mut out = String::with_capacity(text.len());
	let mut index = 0;

	// Go decodes a rune at a time with `utf8.DecodeRuneInString`. Slicing a
	// `&str` at a non-boundary would panic here, so the walk steps by whole
	// characters, which is the same thing for well-formed UTF-8.
	while index < bytes.len() {
		let current = next_char(text, index);
		index += current.len_utf8();

		if current != '\\' {
			out.push(current);
			continue;
		}

		if index >= bytes.len() {
			return Err(Error::from_static(
				location,
				"Truncated escape sequence in string literal.",
			));
		}

		let escaped = next_char(text, index);
		index += escaped.len_utf8();

		match escaped {
			'"' => out.push('"'),
			'\'' => out.push('\''),
			'\\' => out.push('\\'),
			// See json.org: `\/` is a valid escape.
			'/' => out.push('/'),
			'b' => out.push('\u{8}'),
			'f' => out.push('\u{c}'),
			'n' => out.push('\n'),
			'r' => out.push('\r'),
			't' => out.push('\t'),
			'u' => {
				let code = read_hex4(text, &mut index, location, false)?;
				let code = if is_surrogate(code) {
					read_low_surrogate(text, &mut index, location, code)?
				} else {
					code
				};
				// Every path above yields either a non-surrogate or the
				// replacement character, so this cannot fail; Go's
				// `buf.WriteRune` would write U+FFFD in the same situation.
				out.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
			}
			other => {
				return Err(Error::from_static(
					location,
					&format!("Unknown escape sequence in string literal: \\{other}"),
				));
			}
		}
	}

	Ok(out)
}

/// The character at `index`, which the caller has established is in bounds and
/// on a character boundary.
fn next_char(text: &str, index: usize) -> char {
	text[index..]
		.chars()
		.next()
		.expect("the caller checked there is a character here")
}

/// Read four hex digits as a UTF-16 code unit, advancing `index` past them.
///
/// Works on **bytes**, as Go does. Slicing the `&str` instead would panic
/// whenever the four bytes split a multi-byte character — `'\u12€x'` is enough
/// to do it — and a formatter that panics on a malformed escape is worse than
/// one that reports it.
///
/// `low` only selects which of the two upstream messages to use; they differ by
/// one word.
fn read_hex4(
	text: &str,
	index: &mut usize,
	location: &LocationRange,
	low: bool,
) -> Result<u32, Error> {
	let bytes = text.as_bytes();
	if *index + 4 > bytes.len() {
		return Err(Error::from_static(
			location,
			"Truncated unicode escape sequence in string literal.",
		));
	}

	let digits = &bytes[*index..*index + 4];
	// Go uses `hex.DecodeString`, which refuses anything that is not four hex
	// digits. `from_str_radix` alone would accept a leading `+`.
	if !digits.iter().all(u8::is_ascii_hexdigit) {
		// Upstream interpolates the first four bytes of the **whole string**
		// rather than the offending escape, so `'ab\uZZZZ'` reports `ab\u`.
		// Reproduced, not fixed.
		//
		// Lossily, because those four bytes need not be valid UTF-8 on their
		// own and a Rust `String` has to be. Go emits the partial sequence
		// raw; reaching that difference takes a malformed escape in a string
		// whose first four bytes split a character.
		let prefix = String::from_utf8_lossy(&bytes[..4]);
		let what = if low {
			"Unicode low surrogate"
		} else {
			"Unicode"
		};
		return Err(Error::from_static(
			location,
			&format!("{what} escape sequence was malformed: {prefix}"),
		));
	}

	let code = digits
		.iter()
		.fold(0u32, |code, digit| code * 16 + hex_value(*digit));

	*index += 4;
	Ok(code)
}

/// The value of one ASCII hex digit, which the caller has already validated.
fn hex_value(digit: u8) -> u32 {
	u32::from(match digit {
		b'0'..=b'9' => digit - b'0',
		b'a'..=b'f' => digit - b'a' + 10,
		b'A'..=b'F' => digit - b'A' + 10,
		_ => unreachable!("the caller validated this is a hex digit"),
	})
}

/// `utf16.IsSurrogate`.
fn is_surrogate(code: u32) -> bool {
	(0xd800..0xe000).contains(&code)
}

/// Consume the `\uXXXX` that must follow a high surrogate, and combine them.
fn read_low_surrogate(
	text: &str,
	index: &mut usize,
	location: &LocationRange,
	high: u32,
) -> Result<u32, Error> {
	let bytes = text.as_bytes();
	// Six bytes: `\uXXXX`.
	if *index + 6 > bytes.len() {
		return Err(Error::from_static(
			location,
			"Truncated unicode surrogate pair escape sequence in string literal.",
		));
	}
	// On bytes rather than as a `&str`, for the reason `read_hex4` gives.
	if &bytes[*index..*index + 2] != b"\\u" {
		return Err(Error::from_static(
			location,
			"Unicode surrogate pair escape sequence missing low surrogate in string literal.",
		));
	}
	*index += 2;

	let low = read_hex4(text, index, location, true)?;
	Ok(decode_surrogate_pair(high, low))
}

/// `utf16.DecodeRune`: the replacement character where the pair is not a valid
/// one, which is how Go signals it rather than failing.
fn decode_surrogate_pair(high: u32, low: u32) -> u32 {
	const HIGH_START: u32 = 0xd800;
	const LOW_START: u32 = 0xdc00;
	const LOW_END: u32 = 0xe000;
	const BASE: u32 = 0x1_0000;

	if (HIGH_START..LOW_START).contains(&high) && (LOW_START..LOW_END).contains(&low) {
		return (high - HIGH_START) * 0x400 + (low - LOW_START) + BASE;
	}
	u32::from(char::REPLACEMENT_CHARACTER)
}

/// Go's `StringEscape`: the inverse, used by `EnforceStringStyle`.
///
/// `single` says which quote the result will be wrapped in, and so which one
/// has to be escaped. Note that it escapes **only** that quote, which is what
/// lets the pass pick whichever syntax avoids escaping.
pub fn string_escape(text: &str, single: bool) -> String {
	let mut out = String::with_capacity(text.len());
	for c in text.chars() {
		match c {
			'"' => {
				if !single {
					out.push('\\');
				}
				out.push(c);
			}
			'\'' => {
				if single {
					out.push('\\');
				}
				out.push(c);
			}
			'\\' => out.push_str("\\\\"),
			'\u{8}' => out.push_str("\\b"),
			'\u{c}' => out.push_str("\\f"),
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			'\0' => out.push_str("\\u0000"),
			// C1 controls are escaped along with C0, which is wider than JSON
			// requires and is what go-jsonnet does.
			_ if c < '\u{20}' || ('\u{7f}'..='\u{9f}').contains(&c) => {
				let _ = write!(out, "\\u{:04x}", u32::from(c));
			}
			_ => out.push(c),
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::location::Location;

	fn loc() -> LocationRange {
		LocationRange::point("f.jsonnet", Location { line: 1, column: 1 })
	}

	fn unescape(text: &str) -> Result<String, Error> {
		string_unescape(&loc(), text)
	}

	#[test]
	fn the_simple_escapes_are_all_resolved() {
		assert_eq!(
			unescape(r#"a\"b\'c\\d\/e\bf\fg\nh\ri\tj"#).expect("valid"),
			"a\"b'c\\d/e\u{8}f\u{c}g\nh\ri\tj"
		);
	}

	#[test]
	fn a_unicode_escape_becomes_its_character() {
		assert_eq!(unescape(r"A").expect("valid"), "A");
		assert_eq!(unescape(r"é").expect("valid"), "é");
	}

	#[test]
	fn a_surrogate_pair_is_combined() {
		// U+1F600, which only fits in UTF-16 as a pair.
		assert_eq!(unescape(r"😀").expect("valid"), "😀");
	}

	#[test]
	fn an_invalid_surrogate_pair_becomes_the_replacement_character() {
		// Go's utf16.DecodeRune answers U+FFFD rather than failing, so the
		// string parses and carries the replacement character.
		assert_eq!(unescape(r"\ud83d\ud83d").expect("valid"), "\u{fffd}");
	}

	#[test]
	fn a_lone_backslash_is_truncated() {
		assert_eq!(
			unescape("a\\").expect_err("truncated").message(),
			"f.jsonnet:1:1 Truncated escape sequence in string literal."
		);
	}

	#[test]
	fn an_unknown_escape_names_the_character() {
		assert_eq!(
			unescape(r"\q").expect_err("unknown").message(),
			"f.jsonnet:1:1 Unknown escape sequence in string literal: \\q"
		);
	}

	#[test]
	fn a_short_unicode_escape_is_truncated() {
		assert_eq!(
			unescape(r"\u00").expect_err("truncated").message(),
			"f.jsonnet:1:1 Truncated unicode escape sequence in string literal."
		);
	}

	#[test]
	fn a_malformed_unicode_escape_reports_the_start_of_the_string() {
		// Upstream's bug, reproduced: the message shows the first four bytes of
		// the whole string — here `ab\u` — and not the escape that failed.
		assert_eq!(
			unescape(r"ab\uZZZZ").expect_err("malformed").message(),
			"f.jsonnet:1:1 Unicode escape sequence was malformed: ab\\u"
		);
	}

	#[test]
	fn a_malformed_low_surrogate_says_so() {
		assert_eq!(
			unescape(r"\ud83d\uZZZZ").expect_err("malformed").message(),
			"f.jsonnet:1:1 Unicode low surrogate escape sequence was malformed: \\ud8"
		);
	}

	#[test]
	fn a_high_surrogate_without_a_pair_is_rejected() {
		assert_eq!(
			unescape(r"\ud83dx").expect_err("truncated").message(),
			"f.jsonnet:1:1 Truncated unicode surrogate pair escape sequence in string literal."
		);
		assert_eq!(
			unescape(r"\ud83dxxxxxx")
				.expect_err("no low surrogate")
				.message(),
			"f.jsonnet:1:1 Unicode surrogate pair escape sequence missing low surrogate in string literal."
		);
	}

	#[test]
	fn escaping_only_touches_the_quote_that_will_wrap_it() {
		// This is what lets EnforceStringStyle pick the syntax that avoids
		// escaping altogether.
		assert_eq!(string_escape("a'b\"c", true), "a\\'b\"c");
		assert_eq!(string_escape("a'b\"c", false), "a'b\\\"c");
	}

	#[test]
	fn escaping_covers_the_control_characters() {
		assert_eq!(string_escape("\n\t\r\\", true), "\\n\\t\\r\\\\");
		assert_eq!(string_escape("\0", true), "\\u0000");
		assert_eq!(string_escape("\u{1}", true), "\\u0001");
		// C1, which is wider than JSON requires.
		assert_eq!(string_escape("\u{7f}", true), "\\u007f");
		assert_eq!(string_escape("\u{9f}", true), "\\u009f");
		assert_eq!(string_escape("\u{a0}", true), "\u{a0}");
	}

	#[test]
	fn unescape_and_escape_round_trip_for_the_simple_cases() {
		let original = "a'b\"c\nd\\e";
		let escaped = string_escape(original, true);
		assert_eq!(unescape(&escaped).expect("valid"), original);
	}
}
