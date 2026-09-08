fn parse_yaml11_bool(s: &str) -> Result<bool, ()> {
	match s.trim().to_ascii_lowercase().as_str() {
		"true" | "yes" | "y" | "on" => Ok(true),
		"false" | "no" | "n" | "off" => Ok(false),
		_ => Err(()),
	}
}

fn parses_as_integer(s: &str) -> bool {
	let value = s.trim();
	let unsigned = value
		.strip_prefix('+')
		.or_else(|| value.strip_prefix('-'))
		.unwrap_or(value);
	let (radix, digits) = if let Some(rest) = unsigned.strip_prefix("0x") {
		(16, rest)
	} else if let Some(rest) = unsigned.strip_prefix("0o") {
		(8, rest)
	} else if let Some(rest) = unsigned.strip_prefix("0b") {
		(2, rest)
	} else if unsigned.len() > 1 && unsigned.starts_with('0') {
		(8, unsigned)
	} else {
		(10, unsigned)
	};
	!digits.is_empty()
		&& digits
			.chars()
			.filter(|character| *character != '_')
			.all(|character| character.is_digit(radix))
}

fn parses_as_finite_float(s: &str) -> bool {
	let normalized = s.replace('_', "");
	normalized
		.parse::<f64>()
		.is_ok_and(|value| value.is_finite())
}

/// Returns true if `s` can be emitted as a plain scalar without quoting.
/// Internal heuristic used by `write_plain_or_quoted`.
#[inline]
pub(crate) fn is_plain_safe(s: &str) -> bool {
	if s.is_empty() {
		return false;
	}
	if s == "~"
		|| s.eq_ignore_ascii_case("null")
		|| s.eq_ignore_ascii_case("true")
		|| s.eq_ignore_ascii_case("false")
	{
		return false;
	}
	let bytes = s.as_bytes();
	if bytes[0].is_ascii_whitespace()
		|| matches!(
			bytes[0],
			b'[' | b']'
				| b'{' | b'}'
				| b'#' | b'&'
				| b'*' | b'!'
				| b'|' | b'>'
				| b'\'' | b'"'
				| b'%' | b'@'
				| b'`'
		) {
		return false;
	}

	// Handle '-', '?', and ':' specially: only quote if they're alone or followed by whitespace
	// This matches Go's yaml.v2/v3 behavior where "-foo", "?bar", ":9121" are safe plain scalars
	// but "- foo" would be a sequence indicator, ": foo" would be a mapping value
	if bytes[0] == b'-' || bytes[0] == b'?' || bytes[0] == b':' {
		// Single character '-', '?', or ':' needs quoting
		if bytes.len() == 1 {
			return false;
		}
		// Followed by whitespace needs quoting (sequence/mapping/value indicator)
		if bytes[1].is_ascii_whitespace() {
			return false;
		}
		// "-foo", "?bar", ":9121" is safe - continue to other checks
	}

	// Colon is only problematic when followed by whitespace (mapping indicator)
	// or at the end of the string. This matches Go yaml.v2/v3 behavior.
	if s.contains(": ") || s.trim().ends_with(':') {
		return false;
	}

	// '#' only starts a comment when preceded by whitespace
	if s.contains(" #") {
		return false;
	}

	!contains_any_or_is_control(s, &[','])
}

/// Returns true if `s` can be emitted as a plain scalar in VALUE position without quoting.
/// This is slightly more permissive than `is_plain_safe` for keys: it allows ':' inside values.
/// Additionally, we make this stricter for strings that appear inside flow-style sequences/maps
/// where certain characters would break parsing (e.g., commas and brackets) or where the token
/// could be misinterpreted as a number or boolean.
#[inline]
pub(crate) fn is_plain_value_safe(s: &str) -> bool {
	if s.is_empty() {
		return false;
	}
	// Nulls and YAML 1.2 booleans
	if s == "~" || s.eq_ignore_ascii_case("null") {
		return false;
	}
	// ISO8601 timestamps need quoting to avoid being parsed as dates
	if looks_like_timestamp(s) {
		return false;
	}
	// Numeric-looking tokens: quote them to preserve strings
	// Use parsing as a heuristic; if it parses as a number, don't allow plain style
	if parses_as_integer(s) {
		return false;
	}
	// Only treat as float if it parses to a finite value - overflow to infinity
	// (e.g., "6e50317") should be treated as a plain string, matching Go yaml.v2
	if parses_as_finite_float(s) {
		return false;
	}
	if parse_yaml11_bool(s).is_ok() {
		return false;
	}

	// Special float tokens per YAML
	let bytes = s.as_bytes();
	if bytes[0].is_ascii_whitespace()
		|| matches!(
			bytes[0],
			b'[' | b']'
				| b'{' | b'}'
				| b'#' | b'&'
				| b'*' | b'!'
				| b'|' | b'>'
				| b'\'' | b'"'
				| b'%' | b'@'
				| b'`'
		) {
		return false;
	}

	// Handle '-', '?', and ':' specially: only quote if they're alone or followed by whitespace
	// This matches Go's yaml.v2/v3 behavior where "-foo", "?bar", ":9121" are safe plain scalars
	// but "- foo" would be a sequence indicator, ": foo" would be a mapping value
	if bytes[0] == b'-' || bytes[0] == b'?' || bytes[0] == b':' {
		// Single character '-', '?', or ':' needs quoting
		if bytes.len() == 1 {
			return false;
		}
		// Followed by whitespace needs quoting (sequence/mapping/value indicator)
		if bytes[1].is_ascii_whitespace() {
			return false;
		}
		// "-foo", "?bar", ":9121" is safe - continue to other checks
	}

	// Yet while colon is ok, colon after whitespace is not.
	if s.contains(": ") || s.trim().ends_with(':') {
		// We only need to check for space as CR, LF and TAB are control characters and will
		// trigger escape on their own anyway.
		return false;
	}

	// In flow style, commas and brackets/braces are structural; quote strings containing them.
	// '#' only starts a comment when preceded by whitespace, so " #" needs quoting
	// but "...#anchor" (URL fragments) are safe unquoted.
	if s.contains(" #") {
		return false;
	}
	!contains_any_or_is_control(s, &[',', '[', ']', '{', '}'])
}

/// Returns true if `s` can be emitted as a plain scalar in block-style VALUE position.
/// This is more permissive than `is_plain_value_safe` because in block style, commas
/// and brackets WITHIN values are allowed (they're only structural in flow style).
/// However, standalone structural characters (just ",", "[", etc.) still need quoting.
/// This matches Go's yaml.v3 behavior.
#[inline]
pub(crate) fn is_plain_block_value_safe(s: &str) -> bool {
	if s.is_empty() {
		return false;
	}
	// Nulls and YAML 1.2 booleans
	if s == "~" || s.eq_ignore_ascii_case("null") {
		return false;
	}
	// ISO8601 timestamps need quoting to avoid being parsed as dates
	// Matches patterns like: 2025-07-03T15:30:00Z, 2025-07-03T15:30:00+00:00
	if looks_like_timestamp(s) {
		return false;
	}
	// Numeric-looking tokens: quote them to preserve strings
	if parses_as_integer(s) {
		return false;
	}
	// Only treat as float if it parses to a finite value - overflow to infinity
	// (e.g., "6e50317") should be treated as a plain string, matching Go yaml.v2
	if parses_as_finite_float(s) {
		return false;
	}
	if parse_yaml11_bool(s).is_ok() {
		return false;
	}

	let bytes = s.as_bytes();

	// Standalone structural characters need quoting even in block style
	// (they can confuse the parser at value position)
	if bytes.len() == 1 && matches!(bytes[0], b',' | b'[' | b']' | b'{' | b'}') {
		return false;
	}

	// Note: Unlike flow style, block style allows double quotes within values.
	// Go's yaml.v3 does NOT quote strings containing double quotes in block style.
	// Example: -forward.selectors={__name__="target_info"} is valid unquoted.

	// Trailing whitespace requires quoting (Go yaml.v2 behavior)
	if bytes.last().is_some_and(|b| b.is_ascii_whitespace()) {
		return false;
	}

	if bytes[0].is_ascii_whitespace()
		|| matches!(
			bytes[0],
			b'[' | b']'
				| b'{' | b'}'
				| b'#' | b'&'
				| b'*' | b'!'
				| b'|' | b'>'
				| b'\'' | b'"'
				| b'%' | b'@'
				| b'`'
		) {
		return false;
	}

	// Handle '-', '?', and ':' specially: only quote if they're alone or followed by whitespace
	// This matches Go's yaml.v2/v3 behavior where "-foo", "?bar", ":9121" are safe plain scalars
	if bytes[0] == b'-' || bytes[0] == b'?' || bytes[0] == b':' {
		if bytes.len() == 1 {
			return false;
		}
		if bytes[1].is_ascii_whitespace() {
			return false;
		}
	}

	// Colon after whitespace would start a mapping value
	// Strings ending with colon (after trimming whitespace) could be confused as mapping keys
	if s.contains(": ") || s.trim().ends_with(':') {
		return false;
	}

	// In block style, commas and brackets WITHIN values are allowed.
	// '#' only starts a comment when preceded by whitespace, so " #" needs quoting
	// but "...#anchor" (URL fragments) are safe unquoted. This matches Go yaml.v2 behavior.
	if s.contains(" #") {
		return false;
	}

	// Note: Go yaml.v2 does NOT quote strings ending with commas (e.g., "hello,")
	// This differs from yaml.v3 behavior. Since tk uses yaml.v2 for exports, don't quote these.

	!contains_control_or_non_bmp(s)
}

fn contains_any_or_is_control(string: &str, values: &[char]) -> bool {
	string
		.chars()
		.any(|x| values.iter().any(|v| &x == v || x.is_control()))
}

/// Returns true if `s` contains control characters OR non-BMP characters (>= U+10000).
/// Go yaml.v2 escapes non-BMP characters (like emoji) with \U, so they need quoting.
fn contains_control_or_non_bmp(string: &str) -> bool {
	string
		.chars()
		.any(|c| c.is_control() || (c as u32) >= 0x10000)
}

/// Returns true if `s` looks like an ISO8601 date or timestamp that YAML parsers might
/// interpret as a date/time. These need to be quoted to preserve them as strings.
/// Matches patterns like: 2021-07-19, 2025-07-03T15:30:00Z, 2025-07-03T15:30:00+00:00
#[inline]
pub(crate) fn looks_like_date_or_timestamp(s: &str) -> bool {
	looks_like_timestamp(s)
}

#[inline]
fn looks_like_timestamp(s: &str) -> bool {
	let bytes = s.as_bytes();
	// Check for YYYY-MM-DD date pattern (10 chars)
	if bytes.len() == 10 {
		return bytes[4] == b'-'
			&& bytes[7] == b'-'
			&& bytes[0..4].iter().all(|b| b.is_ascii_digit())
			&& bytes[5..7].iter().all(|b| b.is_ascii_digit())
			&& bytes[8..10].iter().all(|b| b.is_ascii_digit());
	}
	// Check for HH:MM time pattern (5 chars) - matches Go yaml.v2 behavior
	// This pattern like "00:00", "12:34" should be quoted to avoid ambiguity
	if bytes.len() == 5 {
		return bytes[2] == b':'
			&& bytes[0..2].iter().all(|b| b.is_ascii_digit())
			&& bytes[3..5].iter().all(|b| b.is_ascii_digit());
	}
	// Must be at least "YYYY-MM-DDTHH:MM:SSZ" (20 chars) for full timestamp with timezone
	// Go yaml.v2 does NOT quote timestamps without timezone suffix like "2025-11-03T15:00:00"
	// It only quotes timestamps with Z or +/-offset suffix
	if bytes.len() < 20 {
		return false;
	}
	// Check for YYYY-MM-DDTHH:MM:SS pattern first
	// Position: 0123456789...
	//           YYYY-MM-DDTHH:MM:SS
	let has_timestamp_prefix = bytes[4] == b'-'
		&& bytes[7] == b'-'
		&& bytes[10] == b'T'
		&& bytes[13] == b':'
		&& bytes[16] == b':'
		&& bytes[0..4].iter().all(|b| b.is_ascii_digit())
		&& bytes[5..7].iter().all(|b| b.is_ascii_digit())
		&& bytes[8..10].iter().all(|b| b.is_ascii_digit())
		&& bytes[11..13].iter().all(|b| b.is_ascii_digit())
		&& bytes[14..16].iter().all(|b| b.is_ascii_digit())
		&& bytes[17..19].iter().all(|b| b.is_ascii_digit());

	if !has_timestamp_prefix {
		return false;
	}

	// Must have timezone suffix to be considered a timestamp by Go yaml.v2
	// Check for Z suffix (e.g., "2025-07-03T15:30:00Z")
	// or +/-offset suffix (e.g., "2025-07-03T15:30:00+00:00")
	bytes[19] == b'Z' || bytes[19] == b'+' || bytes[19] == b'-'
}
