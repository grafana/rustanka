//! Serializer options for YAML emission.
//!
//! Controls indentation and optional anchor name generation for the serializer.
//!
//! Example: use 4-space indentation and a custom anchor naming scheme.
//!
//! ```rust
//! use serde::Serialize;
//!
//! #[derive(Serialize)]
//! struct Item { a: i32, b: bool }
//!
//! let mut buf = String::new();
//! let opts = rtk_yaml::SerializerOptions {
//!     indent_step: 4,
//!     anchor_generator: Some(|id| format!("id{}/", id)),
//!     ..Default::default()
//! };
//! rtk_yaml::to_fmt_writer_with_options(&mut buf, &Item { a: 1, b: true }, opts).unwrap();
//! assert!(buf.contains("a: 1"));
//! ```

use crate::ser_error::Error;

/// Controls the chomping behavior for block scalars.
///
/// In YAML block scalars (`|` or `>`), the chomp indicator determines how trailing
/// newlines are handled:
/// - `Strip` (`-`): Remove all trailing newlines from the content
/// - `Clip` (default): Keep exactly one trailing newline
/// - `Keep` (`+`): Preserve all trailing newlines
///
/// See [YAML 1.2 Spec - Chomping Indicator](https://yaml.org/spec/1.2.2/#8112-chomping-indicator)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChompIndicator {
	/// Strip (`-`): Remove all trailing newlines (e.g., `|-` or `>-`)
	Strip,
	/// Clip (no indicator): Keep exactly one trailing newline (e.g., `|` or `>`)
	Clip,
	/// Keep (`+`): Preserve all trailing newlines (e.g., `|+` or `>+`)
	Keep,
}

#[derive(Clone, Copy)]
pub struct SerializerOptions {
	/// Number of spaces to indent per nesting level when emitting block-style collections.
	pub indent_step: usize,
	/// Number of spaces to indent array items (sequences). If `None`, uses `indent_step`.
	/// This allows reproducing Go's YAML behavior where arrays are indented 2 spaces
	/// while objects are indented 4 spaces.
	pub indent_array: Option<usize>,
	/// Optional custom anchor-name generator.
	///
	/// Receives a monotonically increasing `usize` id (starting at 1) and returns the
	/// anchor name to emit. If `None`, the built-in generator yields names like `a1`, `a2`, ...
	pub anchor_generator: Option<fn(usize) -> String>,
	/// Threshold for block-string wrappers ([crate::LitStr]/[crate::FoldStr] and owned variants
	/// [crate::LitString]/[crate::FoldString]).
	///
	/// If the string contains a newline, block style is always used. Otherwise, when the
	/// string is single-line and its length is strictly less than this threshold, the
	/// serializer emits a normal YAML scalar (no block style). Longer strings use block
	/// styles `|` or `>` depending on the wrapper. See the type docs for
	/// [crate::LitStr], [crate::FoldStr], [crate::LitString] and [crate::FoldString] for
	/// examples.
	pub min_fold_chars: usize,
	/// Maximum width (in characters) for lines in folded block scalars (`>`).
	///
	/// Lines are wrapped at whitespace so that each emitted line is at most this many
	/// characters long (excluding indentation). If no whitespace is present within the
	/// limit, a hard break is performed.
	pub folded_wrap_chars: usize,
	/// When enabled, serialize simple enums that become a single scalar (unit variants)
	/// using YAML tags, e.g. `!!Enum Variant` instead of a plain scalar `Variant`.
	/// Deserializer does not need this setting as both cases will be understood.
	pub tagged_enums: bool,
	/// When enabled, strings containing newlines are automatically serialized using
	/// YAML literal block scalar style (`|`) instead of quoted strings with escape sequences.
	/// This matches Go's yaml.v3 behavior.
	///
	/// Default: `false` (preserves backwards compatibility)
	pub prefer_block_scalars: bool,
	/// When enabled, empty maps are serialized as `{}` instead of being left empty/null.
	/// This matches Go's yaml.v3 behavior.
	///
	/// Default: `false` (preserves backwards compatibility)
	pub empty_map_as_braces: bool,
	/// When enabled, empty arrays/sequences are serialized as `[]` instead of being left empty.
	/// This matches Go's yaml.v3 behavior.
	///
	/// Default: `false` (preserves backwards compatibility)
	pub empty_array_as_brackets: bool,
	/// Maximum line width for automatic line wrapping.
	///
	/// When set to `Some(width)`, long string values that exceed the specified width
	/// (after accounting for indentation) will be automatically wrapped using YAML
	/// folded block scalar style (`>`). This matches Go's yaml.v3 behavior with
	/// `encoder.SetWidth(80)`.
	///
	/// - `None`: No automatic wrapping (default, preserves backwards compatibility)
	/// - `Some(80)`: Wrap lines at 80 characters (common default, matches Go yaml.v3)
	///
	/// Note: This only affects block-style output. Flow-style contexts (inline
	/// sequences/maps) do not support block scalars and will use quoted strings.
	pub line_width: Option<usize>,
	/// Threshold for scientific notation serialization of large numbers.
	///
	/// When set to `Some(threshold)`, numbers with absolute value >= threshold will be
	/// serialized using scientific notation (e.g., `1e+11` instead of `100000000000`).
	///
	/// When set to `None`, scientific notation is disabled for large numbers.
	///
	/// Default: `Some(1_000_000)` (preserves backwards compatibility with Go yaml.v3 behavior)
	pub scientific_notation_threshold: Option<u64>,
	/// Threshold for scientific notation serialization of small numbers.
	///
	/// When set to `Some(threshold)`, numbers with absolute value > 0 and < threshold will be
	/// serialized using scientific notation (e.g., `2e-05` instead of `0.00002`).
	///
	/// This matches Go yaml.v3 behavior where small floats like 0.00002 are rendered as 2e-05.
	/// Go yaml.v3 uses a threshold of 0.0001 (1e-4).
	///
	/// When set to `None`, scientific notation is disabled for small numbers.
	///
	/// Default: `None` (preserves backwards compatibility)
	pub scientific_notation_small_threshold: Option<f64>,
	/// Spell a negative zero the way Go does, as `-0` rather than `-0.0`.
	///
	/// A negative zero is the one float whose sign cannot survive being written
	/// as an integer, so it has to be emitted as a float — and Go's
	/// `strconv.FormatFloat(f, 'g', -1, 64)`, which `gopkg.in/yaml.v2` and
	/// `yaml.v3` use, writes it without a fractional part. The shortest
	/// round-tripping representation this crate otherwise uses gives `-0.0`.
	///
	/// Both parse back to a negative zero; this only decides the bytes.
	///
	/// Default: `false` (preserves backwards compatibility)
	pub go_style_negative_zero: bool,
	/// Controls the chomp indicator for block scalars.
	///
	/// - `None` (default): Auto-detect based on trailing newlines (current behavior)
	///   - 0 trailing newlines → `|-` (strip)
	///   - 1 trailing newline → `|` (clip)
	///   - 2+ trailing newlines → `|+` (keep)
	/// - `Some(ChompIndicator::Strip)`: Always use `|-` (matches Go yaml.v3 for strings without trailing newlines)
	/// - `Some(ChompIndicator::Clip)`: Always use `|`
	/// - `Some(ChompIndicator::Keep)`: Always use `|+`
	///
	/// When a fixed chomp indicator is specified, the content is adjusted to ensure
	/// round-trip correctness. For example, with `Strip`, trailing newlines are removed
	/// from the content before emitting.
	pub block_scalar_chomp: Option<ChompIndicator>,
	/// When enabled, string keys that look like numbers (e.g., "12345", "77387")
	/// are quoted in the YAML output. This matches Go's yaml.v3 behavior.
	///
	/// Default: `false` (preserves backwards compatibility)
	pub quote_numeric_strings: bool,
	/// Controls the absolute indentation (in spaces) for block scalar content when
	/// the block scalar is a direct item in a sequence (array).
	///
	/// When `None` (default), block scalar body is indented based on the sequence's depth
	/// plus one level (multiplied by `indent_step`). When `Some(n)`, block scalar body
	/// uses exactly `n` spaces of indentation, regardless of `indent_step`.
	///
	/// This is useful for matching Go yaml.v3's behavior where block scalars in sequences
	/// have minimal indentation (e.g., `Some(2)` for 2-space indent).
	///
	/// Default: `None` (standard indentation based on nesting depth)
	pub block_scalar_indent_in_seq: Option<usize>,
	/// When enabled, map keys that could be interpreted as YAML 1.1 booleans
	/// are quoted. This includes: `y`, `Y`, `n`, `N`, `yes`, `Yes`, `YES`,
	/// `no`, `No`, `NO`, `on`, `On`, `ON`, `off`, `Off`, `OFF`, `true`, `True`,
	/// `TRUE`, `false`, `False`, `FALSE`.
	///
	/// This matches Go's yaml.v3 behavior which quotes these keys to prevent
	/// ambiguity with boolean values.
	///
	/// Default: `false` (preserves backwards compatibility)
	pub quote_ambiguous_keys: bool,
	/// When enabled, always use double quotes for strings that need quoting,
	/// instead of preferring single quotes when possible.
	///
	/// By default, the serializer uses single quotes for strings that don't
	/// need escape sequences (control chars, etc.), since single quotes are
	/// more readable and only require doubling internal quotes (' → '').
	///
	/// When this option is enabled, double quotes are always used, which
	/// matches Go's yaml.v2 behavior.
	///
	/// Default: `false` (preserves backwards compatibility, preferring single quotes)
	pub prefer_double_quotes: bool,
}

// Below this length, block-string wrappers serialize as regular scalars
// instead of YAML block styles. This keeps short values compact.
pub(crate) const MIN_FOLD_CHARS: usize = 32;
/// Maximum width (in characters) for lines inside folded block scalars.
/// Lines will be wrapped at whitespace so that each emitted line is at most
/// this many characters long (excluding indentation). If no whitespace is
/// available within the limit, a hard break is performed.
pub(crate) const FOLDED_WRAP_CHARS: usize = 80;

impl SerializerOptions {
	pub(crate) fn consistent(&self) -> Result<(), Error> {
		if self.indent_step == 0 {
			return Err(Error::InvalidOptions(
				"Invalid indent step must be positive".to_string(),
			));
		}
		Ok(())
	}
}

impl Default for SerializerOptions {
	fn default() -> Self {
		// Defaults mirror internal constants used by the serializer.
		Self {
			indent_step: 2,
			indent_array: None,
			anchor_generator: None,
			min_fold_chars: MIN_FOLD_CHARS,
			folded_wrap_chars: FOLDED_WRAP_CHARS,
			tagged_enums: false,
			prefer_block_scalars: false,
			empty_map_as_braces: false,
			empty_array_as_brackets: false,
			line_width: None,
			scientific_notation_threshold: Some(1_000_000),
			scientific_notation_small_threshold: None,
			go_style_negative_zero: false,
			block_scalar_chomp: None,
			quote_numeric_strings: false,
			block_scalar_indent_in_seq: None,
			quote_ambiguous_keys: false,
			prefer_double_quotes: false,
		}
	}
}

impl SerializerOptions {
	/// Formatting used by Tanka manifest export.
	pub fn tanka_v2() -> Self {
		Self {
			indent_step: 2,
			indent_array: Some(0),
			prefer_block_scalars: true,
			empty_map_as_braces: true,
			empty_array_as_brackets: true,
			line_width: Some(80),
			// 1 million, and small floats like 0.00001, become exponents.
			scientific_notation_threshold: Some(1_000_000),
			scientific_notation_small_threshold: Some(0.0001),
			// `y`, `n`, `yes`, `no`, `12`, `12.5` and friends stay quoted, as
			// go-yaml v3 quotes them.
			quote_ambiguous_keys: true,
			quote_numeric_strings: true,
			// A negative zero has to stay a float, and go-yaml writes one as `-0`.
			go_style_negative_zero: true,
			..Default::default()
		}
	}
	/// Formatting used by Tanka’s `manifestYamlFromJson` native function.
	pub fn tanka_v3() -> Self {
		Self {
			indent_step: 4,
			indent_array: None,
			prefer_block_scalars: true,
			empty_map_as_braces: true,
			empty_array_as_brackets: true,
			block_scalar_indent_in_seq: Some(2),
			line_width: None,
			scientific_notation_threshold: Some(1_000_000),
			scientific_notation_small_threshold: Some(0.0001),
			quote_numeric_strings: true,
			..Default::default()
		}
	}
}
