//! Byte boundaries at the string/comment phase; layout cleanup follows in #63.
use rtk_jsonnetfmt::{Options, StringStyle, format_bytes, format_default_bytes};

#[test]
fn go_decodes_each_invalid_byte_only_at_its_own_string_or_comment_boundary() {
	let cases: &[(&[u8], &[u8])] = &[
		(b"\"\xff\"", "'�'\n".as_bytes()),
		(b"\"\xe2\x82\"", "'��'\n".as_bytes()),
		(b"\"\xf0\x9f\x92\"", "'���'\n".as_bytes()),
		// Both quote kinds prevent EnforceStringStyle from rewriting the source.
		(b"\"\xff'\\\"\"", b"\"\xff'\\\"\"\n"),
		(b"// \xff\n1", "// �\n1\n".as_bytes()),
		// Single-line C comments retain bytes; paragraph C comments decode runes.
		(b"/* \xff */ 1", b"/* \xff */ 1\n"),
		// #63 removes this leading newline before the first paragraph.
		(b"/* \xe2\x82\n*/1", "\n/* ��\n*/\n1\n".as_bytes()),
		(b"@\"\xff\"", "@\"�\"\n".as_bytes()),
		(b"|||\n  \xe2\x82\n|||", "|||\n  ��\n|||\n".as_bytes()),
	];
	for &(input, expected) in cases {
		assert_eq!(
			format_default_bytes("<stdin>", input).unwrap(),
			expected,
			"{input:?}"
		);
	}
}

#[test]
fn a_leave_style_preserves_a_quoted_tokens_original_bytes() {
	let options = Options {
		string_style: StringStyle::Leave,
		..Options::default()
	};
	assert_eq!(
		format_bytes("f", b"\"\xe2\x82\"", &options).unwrap(),
		b"\"\xe2\x82\"\n"
	);
}

#[test]
fn parser_error_wrapping_preserves_truncated_utf8_and_raw_token_text() {
	let cases: &[(&[u8], &[u8])] = &[
		("\"é€\\uZZZZ\"".as_bytes(), b"<stdin>:1:1-14 <stdin>:1:1-14 Unicode escape sequence was malformed: \xc3\xa9\xe2\x82"),
		(b"1 \"\xff\"", b"<stdin>:1:3-6 Did not expect: (STRING_DOUBLE, \"\xff\")"),
		(b"{ \"\xff\": 1, \"\xff\": 2 }", b"<stdin>:1:11-14 Duplicate field: \xff"),
	];
	for &(input, expected) in cases {
		let error = format_default_bytes("<stdin>", input).unwrap_err();
		assert_eq!(error.message_bytes(), expected, "{input:?}");
		let mut output = Vec::new();
		error.write_to(&mut output).unwrap();
		assert_eq!(output, expected);
	}
}

#[cfg(unix)]
#[test]
fn native_filename_bytes_survive_lexer_and_parser_diagnostics() {
	use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};
	let path = Path::new(OsStr::from_bytes(b"native-\xff.jsonnet"));
	for input in [b"?".as_slice(), b"1 2", b"\"\\uZZZZ\""] {
		let error = format_default_bytes(path, input).unwrap_err();
		assert!(error.message_bytes().starts_with(b"native-\xff.jsonnet:1:"));
		assert!(
			!error
				.message_bytes()
				.windows(3)
				.any(|bytes| bytes == b"\xef\xbf\xbd")
		);
	}
}
