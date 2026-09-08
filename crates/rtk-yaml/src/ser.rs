//! Single-pass YAML serializer with optional anchors for Rc/Arc/Weak,
//! order preservation (uses the iterator order of your types), simple
//! style controls (block strings & flow containers), and special
//! float handling for NaN/±Inf. No intermediate YAML DOM is built.
//!
//! Usage example:
//!
//! use serde::Serialize;
//! use std::rc::Rc;
//! use rtk_yaml::{to_string, RcAnchor, LitStr, FlowSeq};
//!
//! #[derive(Serialize)]
//! struct Cfg {
//!     name: String,
//!     ports: FlowSeq<Vec<u16>>,   // render `[8080, 8081]`
//!     note: LitStr<'static>,      // render as `|` block
//!     data: RcAnchor<Vec<i32>>,   // first sight => &a1
//!     alias: RcAnchor<Vec<i32>>,  // later sight => *a1
//! }
//!
//! fn main() {
//!     let shared = Rc::new(vec![1,2,3]);
//!     let cfg = Cfg {
//!         name: "demo".into(),
//!         ports: FlowSeq(vec![8080, 8081]),
//!         note: LitStr("line 1\nline 2"),
//!         data: RcAnchor(shared.clone()),
//!         alias: RcAnchor(shared),
//!     };
//!     println!("{}", to_string(&cfg).unwrap());
//! }

use serde::ser::{
	self, Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
	SerializeTuple, SerializeTupleStruct, SerializeTupleVariant, Serializer,
};
use std::collections::HashMap;
use std::fmt::{self, Write};
use std::rc::{Rc, Weak as RcWeak};
use std::sync::{Arc, Weak as ArcWeak};

use crate::serializer_options::{
	ChompIndicator, FOLDED_WRAP_CHARS, MIN_FOLD_CHARS, SerializerOptions,
};
pub struct RcAnchor<T>(pub Rc<T>);
pub struct ArcAnchor<T>(pub Arc<T>);
pub struct RcWeakAnchor<T>(pub RcWeak<T>);
pub struct ArcWeakAnchor<T>(pub ArcWeak<T>);
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use nohash_hasher::BuildNoHashHasher;

// ------------------------------------------------------------
// Public API
// ------------------------------------------------------------

pub use crate::ser_error::Error;
use crate::ser_quoting::{is_plain_block_value_safe, is_plain_safe, is_plain_value_safe};

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// The sentinel struct and field name used by [`RawScalar`].
///
/// It is public so serializer adapters can implement the same protocol without
/// depending on the wrapper type. Both the struct and its sole field must use
/// this exact name.
pub const RAW_SCALAR_TOKEN: &str = "$rtk_yaml::private::RawScalar";

/// YAML plain-scalar text that is emitted without quoting or reformatting.
///
/// This is intended for scalar spellings that have already been selected by a
/// compatible implementation, such as an exact numeric representation. The
/// text is restricted to a conservative, flow-safe plain-scalar subset; it is
/// validated both here and by the YAML serializer's sentinel handler.
///
/// Serializers that do not understand this protocol see a namespaced one-field
/// struct, not an ordinary string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawScalar<'a>(pub &'a str);

/// Force a sequence to be emitted in flow style: `[a, b, c]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowSeq<T>(pub T);
/// Force a mapping to be emitted in flow style: `{k1: v1, k2: v2}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowMap<T>(pub T);

/// Attach an inline YAML comment to a value when serializing.
///
/// This wrapper lets you annotate a scalar with an inline YAML comment that is
/// emitted after the value when using block style. The typical form is:
/// `value # comment`. This is the most useful when deserializing the anchor
/// reference (human reader may want short comment what is the reference about)
///
/// Behavior
/// - Block style (default): the comment appears after the scalar on the same line.
/// - Flow style (inside `[ ... ]` or `{ ... }`): comments are suppressed to keep
///   the flow representation compact and unambiguous.
/// - Complex values (sequences/maps/structs): the comment is ignored; only the
///   inner value is serialized to preserve indentation and layout.
/// - Newlines in comments are sanitized to spaces so the comment remains on a
///   single line (e.g., "a\nb" becomes "a b").
/// - Deserialization of `Commented<T>` ignores comments: it behaves like `T` and
///   produces an empty comment string.
///
/// Examples
///
/// Basic scalar with a comment in block style:
/// ```rust
/// use serde::Serialize;
///
/// // Re-exported from the crate root
/// use rtk_yaml::Commented;
///
/// let out = rtk_yaml::to_string(&Commented(42, "answer".to_string())).unwrap();
/// assert_eq!(out, "42 # answer\n");
/// ```
///
/// As a mapping value, still inline:
/// ```rust
/// use serde::Serialize;
/// use rtk_yaml::Commented;
///
/// #[derive(Serialize)]
/// struct S { n: Commented<i32> }
///
/// let s = S { n: Commented(5, "send five starships first".into()) };
/// let out = rtk_yaml::to_string(&s).unwrap();
/// assert_eq!(out, "n: 5 # send five starships first\n");
/// ```
///
/// *Important*: Comments are suppressed in flow contexts (no `#` appears), and
/// ignored for complex inner values. Value with `Commented` wrapper will be
/// deserializaed correctly as well, but deserialization of comment is
/// currently not supported.
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commented<T>(pub T, pub String);

/// Force a YAML block literal string using the `|` style.
///
/// Emits the inner `&str` as a block scalar that preserves newlines exactly
/// as written. Each line is indented one level deeper than the surrounding
/// indentation where the value appears.
///
/// In short: use [LitStr] (|) to preserve line breaks exactly; use [FoldStr] (>) when you want
/// readers to display line breaks as spaces (soft-wrapped paragraphs).
///
/// See also: [FoldStr], [LitString], [FoldString].
///
/// Behavior
/// - Uses YAML's literal block style: a leading `|` followed by newline.
/// - Newlines are preserved verbatim by YAML consumers.
/// - Indentation is handled automatically by the serializer.
/// - Works in mapping values, sequence items, and at the top level.
///
/// Examples
///
/// Top-level literal block string:
/// ```rust
/// let long = "line 1\nline 2\n".repeat(20);
/// let out = rtk_yaml::to_string(&rtk_yaml::LitStr(&long)).unwrap();
/// assert!(out.starts_with("|\n  "));
/// ```
///
/// As a mapping value (string without trailing newline uses strip indicator `|-`):
/// ```rust
/// use serde::Serialize;
/// #[derive(Serialize)]
/// struct S { note: rtk_yaml::LitStr<'static> }
/// let s = S { note: rtk_yaml::LitStr("a\nb") };
/// let out = rtk_yaml::to_string(&s).unwrap();
/// assert_eq!(out, "note: |-\n  a\n  b\n");
/// ```
#[derive(Clone, Copy)]
pub struct LitStr<'a>(pub &'a str);

/// Owned-string variant of [LitStr] that forces a YAML block literal string using the `|` style.
///
/// This works the same as [LitStr] but takes ownership of a String. Useful when you already
/// have an owned String and want to avoid borrowing lifetimes.
///
/// See also: [FoldStr], [FoldString].
///
/// Example (string without trailing newline uses strip indicator `|-`):
/// ```rust
/// let out = rtk_yaml::to_string(&rtk_yaml::LitString("line 1\nline 2".to_string())).unwrap();
/// assert_eq!(out, "|-\n  line 1\n  line 2\n");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LitString(pub String);

/// Force a YAML folded block string using the `>` style.
///
/// Emits the inner `&str` as a block scalar that suggests folding line breaks
/// to spaces for display by YAML consumers (empty lines are kept as paragraph
/// breaks). The serializer writes each line on its own; the folding behavior is
/// applied by consumers of the YAML, not during serialization.
///
/// In short: use [FoldStr] (>) for human-readable paragraphs that may soft-wrap; use
/// [LitStr] (|) when you need to preserve line breaks exactly as written.
///
/// See also: [LitStr], [LitString], [FoldString].
///
/// Behavior
/// - Uses YAML's folded block style: a leading `>` followed by newline.
/// - Intended for human-readable paragraphs where soft-wrapping is desirable.
/// - Indentation is handled automatically by the serializer.
/// - Works in mapping values, sequence items, and at the top level.
///
/// Examples
///
/// Top-level folded block string:
/// ```rust
/// let out = rtk_yaml::to_string(&rtk_yaml::FoldStr("line 1\nline 2")).unwrap();
/// assert_eq!(out, ">\n  line 1\n  line 2\n");
/// ```
///
/// As a mapping value:
/// ```rust
/// use serde::Serialize;
/// #[derive(Serialize)]
/// struct S { note: rtk_yaml::FoldStr<'static> }
/// let s = S { note: rtk_yaml::FoldStr("a\nb") };
/// let out = rtk_yaml::to_string(&s).unwrap();
/// assert_eq!(out, "note: >\n  a\n  b\n");
/// ```
#[derive(Clone, Copy)]
pub struct FoldStr<'a>(pub &'a str);

/// Owned-string variant of [FoldStr] that forces a YAML folded block string using the `>` style.
///
/// Same behavior as [FoldStr] but owns a String.
///
/// See also: [LitStr], [LitString].
///
/// Example
/// ```rust
/// let out = rtk_yaml::to_string(&rtk_yaml::FoldString("line 1\nline 2".to_string())).unwrap();
/// assert_eq!(out, ">\n  line 1\n  line 2\n");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoldString(pub String);

// ------------------------------------------------------------
// Internal wrappers -> shape the stream Serde produces so our
// serializer can intercept them in a single pass.
// ------------------------------------------------------------

// Strong: "__yaml_anchor" tuple-struct => [ptr, value]
#[allow(dead_code)]
struct RcStrongPayload<'a, T>(&'a Rc<T>);
#[allow(dead_code)]
struct ArcStrongPayload<'a, T>(&'a Arc<T>);

// Weak: "__yaml_weak_anchor" tuple-struct => [ptr, present, value]
#[allow(dead_code)]
struct RcWeakPayload<'a, T>(&'a RcWeak<T>);
#[allow(dead_code)]
struct ArcWeakPayload<'a, T>(&'a ArcWeak<T>);

// Flow hints and block-string hints: we use newtype-struct names.
const NAME_TUPLE_ANCHOR: &str = "__yaml_anchor";
const NAME_TUPLE_WEAK: &str = "__yaml_weak_anchor";
const NAME_FLOW_SEQ: &str = "__yaml_flow_seq";
const NAME_FLOW_MAP: &str = "__yaml_flow_map";
const NAME_LIT_STR: &str = "__yaml_lit_str";
const NAME_FOLD_STR: &str = "__yaml_fold_str";
const NAME_TUPLE_COMMENTED: &str = "__yaml_commented";

// Top-level newtype wrappers for strong/weak simply wrap the real payloads.
impl<T: Serialize> Serialize for RcAnchor<T> {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		// delegate to tuple-struct the serializer knows how to intercept
		let mut ts = s.serialize_tuple_struct(NAME_TUPLE_ANCHOR, 2)?;
		let ptr = Rc::as_ptr(&self.0) as *const T as usize;
		ts.serialize_field(&ptr)?;
		ts.serialize_field(&*self.0)?;
		ts.end()
	}
}
impl<T: Serialize> Serialize for ArcAnchor<T> {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		let mut ts = s.serialize_tuple_struct(NAME_TUPLE_ANCHOR, 2)?;
		let ptr = Arc::as_ptr(&self.0) as *const T as usize;
		ts.serialize_field(&ptr)?;
		ts.serialize_field(&*self.0)?;
		ts.end()
	}
}
impl<T: Serialize> Serialize for RcWeakAnchor<T> {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		let up = self.0.upgrade();
		let mut ts = s.serialize_tuple_struct(NAME_TUPLE_WEAK, 3)?;
		let ptr = self.0.as_ptr() as *const T as usize;
		ts.serialize_field(&ptr)?;
		ts.serialize_field(&up.is_some())?;
		if let Some(rc) = up {
			ts.serialize_field(&*rc)?;
		} else {
			ts.serialize_field(&())?; // ignored by our serializer
		}
		ts.end()
	}
}
impl<T: Serialize> Serialize for ArcWeakAnchor<T> {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		let up = self.0.upgrade();
		let mut ts = s.serialize_tuple_struct(NAME_TUPLE_WEAK, 3)?;
		let ptr = self.0.as_ptr() as *const T as usize;
		ts.serialize_field(&ptr)?;
		ts.serialize_field(&up.is_some())?;
		if let Some(arc) = up {
			ts.serialize_field(&*arc)?;
		} else {
			ts.serialize_field(&())?;
		}
		ts.end()
	}
}

// Hints for flow / block strings.
impl<T: Serialize> Serialize for FlowSeq<T> {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		s.serialize_newtype_struct(NAME_FLOW_SEQ, &self.0)
	}
}
impl<T: Serialize> Serialize for FlowMap<T> {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		s.serialize_newtype_struct(NAME_FLOW_MAP, &self.0)
	}
}

impl Serialize for RawScalar<'_> {
	fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
		validate_raw_plain_scalar(self.0).map_err(ser::Error::custom)?;

		let mut state = serializer.serialize_struct(RAW_SCALAR_TOKEN, 1)?;
		state.serialize_field(RAW_SCALAR_TOKEN, self.0)?;
		state.end()
	}
}

impl<T: Serialize> Serialize for Commented<T> {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		// Represent as a special tuple-struct with two fields: (comment, value)
		// so the serializer can stage the comment before serializing the value.
		let mut ts = s.serialize_tuple_struct(NAME_TUPLE_COMMENTED, 2)?;
		ts.serialize_field(&self.1)?; // comment first
		ts.serialize_field(&self.0)?; // then value
		ts.end()
	}
}

impl<'a> Serialize for LitStr<'a> {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		// Always delegate decision to the YAML serializer so it can apply options.
		s.serialize_newtype_struct(NAME_LIT_STR, &self.0)
	}
}
impl Serialize for LitString {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		s.serialize_newtype_struct(NAME_LIT_STR, &self.0)
	}
}
impl<'a> Serialize for FoldStr<'a> {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		s.serialize_newtype_struct(NAME_FOLD_STR, &self.0)
	}
}
impl Serialize for FoldString {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		s.serialize_newtype_struct(NAME_FOLD_STR, &self.0)
	}
}

// ------------------------------------------------------------
// Core serializer
// ------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingFlow {
	AnySeq,
	AnyMap,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum StrStyle {
	Literal, // |
	Folded,  // >
}

// Numeric anchor id used internally.
type AnchorId = u32;

/// Core YAML serializer used by `to_string`, `to_fmt_writer`, and `to_io_writer` (and their `_with_options` variants).
///
/// This type implements `serde::Serializer` and writes YAML to a `fmt::Write`.
/// It manages indentation, flow/block styles, and YAML anchors/aliases.
pub struct YamlSer<'a, W: Write> {
	/// Destination writer where YAML text is emitted.
	out: &'a mut W,
	/// Spaces per indentation level for block-style collections.
	indent_step: usize,
	/// Spaces per indentation level for sequences (arrays). If None, uses indent_step.
	indent_array: Option<usize>,
	/// Threshold for downgrading block-string wrappers to plain scalars.
	min_fold_chars: usize,
	/// Wrap width for folded block scalars ('>').
	folded_wrap_col: usize,
	/// Current nesting depth (used for indentation).
	depth: usize,
	/// Whether the cursor is at the start of a line.
	at_line_start: bool,

	// Anchors:
	/// Map from pointer identity to anchor id.
	anchors: HashMap<usize, AnchorId, BuildNoHashHasher<usize>>,
	/// Next numeric id to use when generating anchor names (1-based).
	next_anchor_id: AnchorId,
	/// If set, the next scalar/complex node to be emitted will be prefixed with this `&anchor`.
	pending_anchor_id: Option<AnchorId>,
	/// Optional custom anchor-name generator supplied by the caller.
	anchor_gen: Option<fn(usize) -> String>,
	/// Cache of custom anchor names when generator is present (index = id-1).
	custom_anchor_names: Option<Vec<String>>,

	// Style flags:
	/// Pending flow-style hint captured from wrapper types.
	pending_flow: Option<PendingFlow>,
	/// Number of nested flow containers we are currently inside (>0 means in-flow).
	in_flow: usize,
	/// Pending block-string style hint (literal `|` or folded `>`).
	pending_str_style: Option<StrStyle>,
	/// Whether the pending block-string style was selected automatically (prefer_block_scalars)
	/// as opposed to being requested explicitly by wrapper types (LitStr/FoldStr variants).
	pending_str_from_auto: bool,
	/// Pending inline comment to be appended after the next scalar (block style only).
	pending_inline_comment: Option<String>,
	/// If true, emit YAML tags for simple enums that serialize to a single scalar.
	tagged_enums: bool,
	/// When the previous token was a list item dash ("- ") and the next node is a mapping,
	/// emit the first key inline on the same line ("- key: value").
	pending_inline_map: bool,
	/// After writing a mapping key and ':', defer writing the following space until we know
	/// whether the value is a scalar (space) or a complex node (newline with no space).
	pending_space_after_colon: bool,
	/// If the previous sequence element after a dash turned out to be a mapping (inline first key),
	/// indent subsequent dashes by one level to satisfy tests expecting "\n  -".
	inline_map_after_dash: bool,
	/// If a sequence element starts with a dash on this depth, capture that depth so
	/// struct-variant mappings emitted immediately after can indent their fields correctly.
	after_dash_depth: Option<usize>,
	/// Count of consecutive inline dashes (for `- - - map:` style nested sequences).
	/// Used to calculate proper indentation when a map follows multiple inline dashes.
	inline_dash_count: usize,
	/// Current block map indentation depth (for aligning sequences under a map key).
	current_map_depth: Option<usize>,
	/// Extra spaces to add to indentation (used for aligning inline map fields after array dash when indent doesn't divide evenly).
	indent_offset: usize,
	/// When true, strings containing newlines are automatically serialized using
	/// YAML literal block scalar style (`|`) instead of quoted strings with escape sequences.
	prefer_block_scalars: bool,
	/// When true, empty maps are serialized as `{}` instead of being left empty/null.
	empty_map_as_braces: bool,
	/// When true, empty arrays are serialized as `[]` instead of being left empty.
	empty_array_as_brackets: bool,
	/// Maximum line width for automatic line wrapping of long strings.
	/// When Some(width), long strings are wrapped using folded block style.
	line_width: Option<usize>,
	/// Threshold for scientific notation of large numbers. When Some(threshold), numbers >= threshold
	/// use scientific notation (e.g., 1e+11). When None, plain decimal notation is used.
	scientific_notation_threshold: Option<u64>,
	/// Threshold for scientific notation of small numbers. When Some(threshold), numbers with
	/// abs > 0 and < threshold use scientific notation (e.g., 2e-05). When None, plain decimal is used.
	scientific_notation_small_threshold: Option<f64>,
	go_style_negative_zero: bool,
	/// Override for block scalar chomp indicator. When Some, forces the specified
	/// chomp behavior instead of auto-detecting based on trailing newlines.
	block_scalar_chomp: Option<ChompIndicator>,
	/// Length of the last serialized map key (for accurate line width calculation).
	last_key_len: usize,
	/// When true, string keys that look like numbers are quoted in the output.
	quote_numeric_strings: bool,
	/// When true, string keys that look like booleans (y, n, yes, no, on, off, true, false)
	/// are quoted in the output to match Go yaml.v3 behavior.
	quote_ambiguous_keys: bool,
	/// Override for block scalar indentation in sequences. When Some(n), block scalar
	/// body in a sequence uses exactly n as the depth multiplier with indent_step.
	block_scalar_indent_in_seq: Option<usize>,
	/// True when we're serializing a value directly after `- ` (direct sequence item).
	/// Used for line wrapping first_line_offset calculation to use dash prefix instead of key prefix.
	at_direct_seq_item: bool,
	/// When true, always use double quotes for strings that need quoting,
	/// instead of preferring single quotes when possible.
	prefer_double_quotes: bool,
}

impl<'a, W: Write> YamlSer<'a, W> {
	/// Construct a `YamlSer` that writes to `out`.
	/// Called by `to_writer`/`to_string` entry points.
	pub fn new(out: &'a mut W) -> Self {
		Self {
			out,
			indent_step: 2,
			indent_array: None,
			min_fold_chars: MIN_FOLD_CHARS,
			folded_wrap_col: FOLDED_WRAP_CHARS,
			depth: 0,
			at_line_start: true,
			anchors: HashMap::with_hasher(BuildNoHashHasher::default()),
			next_anchor_id: 1,
			pending_anchor_id: None,
			anchor_gen: None,
			custom_anchor_names: None,
			pending_flow: None,
			in_flow: 0,
			pending_str_style: None,
			pending_str_from_auto: false,
			pending_inline_comment: None,
			tagged_enums: false,
			pending_inline_map: false,
			pending_space_after_colon: false,
			inline_map_after_dash: false,
			after_dash_depth: None,
			inline_dash_count: 0,
			current_map_depth: None,
			indent_offset: 0,
			prefer_block_scalars: false,
			empty_map_as_braces: false,
			empty_array_as_brackets: false,
			line_width: None,
			scientific_notation_threshold: Some(1_000_000),
			scientific_notation_small_threshold: None,
			go_style_negative_zero: false,
			block_scalar_chomp: None,
			last_key_len: 0,
			quote_numeric_strings: false,
			quote_ambiguous_keys: false,
			block_scalar_indent_in_seq: None,
			at_direct_seq_item: false,
			prefer_double_quotes: false,
		}
	}
	/// Construct a `YamlSer` with a specific indentation step.
	/// Typically used internally by tests or convenience wrappers.
	pub fn with_indent(out: &'a mut W, indent_step: usize) -> Self {
		let mut s = Self::new(out);
		s.indent_step = indent_step;
		s.indent_array = None;
		s
	}
	/// Construct a `YamlSer` from user-supplied [`SerializerOptions`].
	/// Used by `to_writer_with_options`.
	pub fn with_options(out: &'a mut W, options: &mut SerializerOptions) -> Self {
		let mut s = Self::new(out);
		s.indent_step = options.indent_step;
		s.indent_array = options.indent_array;
		s.min_fold_chars = options.min_fold_chars;
		s.folded_wrap_col = options.folded_wrap_chars;
		s.anchor_gen = options.anchor_generator.take();
		s.tagged_enums = options.tagged_enums;
		s.prefer_block_scalars = options.prefer_block_scalars;
		s.empty_map_as_braces = options.empty_map_as_braces;
		s.empty_array_as_brackets = options.empty_array_as_brackets;
		s.line_width = options.line_width;
		s.scientific_notation_threshold = options.scientific_notation_threshold;
		s.scientific_notation_small_threshold = options.scientific_notation_small_threshold;
		s.go_style_negative_zero = options.go_style_negative_zero;
		s.block_scalar_chomp = options.block_scalar_chomp;
		s.quote_numeric_strings = options.quote_numeric_strings;
		s.quote_ambiguous_keys = options.quote_ambiguous_keys;
		s.block_scalar_indent_in_seq = options.block_scalar_indent_in_seq;
		s.prefer_double_quotes = options.prefer_double_quotes;
		s
	}

	// -------- helpers --------

	/// Called at the end of emitting a scalar in block style: appends a pending inline
	/// comment (if any) and then emits a newline. In flow style, comments are suppressed.
	#[inline]
	fn write_end_of_scalar(&mut self) -> Result<()> {
		if self.in_flow == 0 {
			if let Some(c) = self.pending_inline_comment.take() {
				self.out.write_str(" # ")?;
				self.out.write_str(&c)?;
			}
			self.newline()?;
		}
		Ok(())
	}

	fn write_raw_scalar(&mut self, value: &str) -> Result<()> {
		validate_raw_plain_scalar(value).map_err(Error::from)?;
		self.write_space_if_pending()?;
		self.write_scalar_prefix_if_anchor()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		self.out.write_str(value)?;
		self.write_end_of_scalar()
	}

	/// Allocate (or get existing) anchor id for a pointer identity.
	/// Returns `(id, is_new)`.
	#[inline]
	fn alloc_anchor_for(&mut self, ptr: usize) -> (AnchorId, bool) {
		match self.anchors.entry(ptr) {
			std::collections::hash_map::Entry::Occupied(e) => (*e.get(), false),
			std::collections::hash_map::Entry::Vacant(v) => {
				let id = self.next_anchor_id;
				self.next_anchor_id = self.next_anchor_id.saturating_add(1);
				if let Some(generator) = self.anchor_gen {
					let name = generator(id as usize);
					self.custom_anchor_names
						.get_or_insert_with(Vec::new)
						.push(name);
				}
				v.insert(id);
				(id, true)
			}
		}
	}

	/// Resolve an anchor name for `id` and write it.
	#[inline]
	fn write_anchor_name(&mut self, id: AnchorId) -> Result<()> {
		if let Some(names) = &self.custom_anchor_names {
			// ids are 1-based; vec is 0-based
			let idx = id as usize - 1;
			if let Some(name) = names.get(idx) {
				self.out.write_str(name)?;
			} else {
				// Fallback if generator vector is out of sync
				write!(self.out, "a{}", id)?;
			}
		} else {
			write!(self.out, "a{}", id)?;
		}
		Ok(())
	}

	/// If a mapping key has just been written (':' emitted) and we determined the value is a scalar,
	/// insert a single space before the scalar and clear the pending flag.
	#[inline]
	fn write_space_if_pending(&mut self) -> Result<()> {
		if self.pending_space_after_colon {
			self.out.write_char(' ')?;
			self.pending_space_after_colon = false;
		}
		Ok(())
	}

	/// Ensure indentation is written if we are at the start of a line.
	/// Internal: called by most emitters before writing tokens.
	#[inline]
	fn write_indent(&mut self, depth: usize) -> Result<()> {
		if self.at_line_start {
			for _k in 0..(self.indent_step * depth + self.indent_offset) {
				self.out.write_char(' ')?;
			}
			self.at_line_start = false;
		}
		Ok(())
	}

	/// Write a specific number of spaces for indentation (ignores indent_step).
	/// Used when block_scalar_indent_in_seq provides an absolute value.
	#[inline]
	fn write_indent_spaces(&mut self, spaces: usize) -> Result<()> {
		if self.at_line_start {
			for _ in 0..spaces {
				self.out.write_char(' ')?;
			}
			self.at_line_start = false;
		}
		Ok(())
	}

	/// Ensure indentation is written for sequences (arrays) if we are at the start of a line.
	/// Uses indent_array if set, otherwise falls back to indent_step.
	/// When indent_array is 0, uses indent_step for base indentation (no additional array nesting).
	#[inline]
	fn write_indent_seq(&mut self, depth: usize) -> Result<()> {
		if self.at_line_start {
			let step = match self.indent_array {
				Some(0) => {
					// When indent_array is 0, use indent_step for the base indentation
					// This means array items align with their parent context
					self.indent_step
				}
				Some(n) => n,
				None => self.indent_step,
			};
			for _k in 0..step * depth {
				self.out.write_char(' ')?;
			}
			self.at_line_start = false;
		}
		Ok(())
	}

	/// Emit a newline and mark the next write position as line start.
	/// Internal utility used after finishing a top-level token.
	#[inline]
	fn newline(&mut self) -> Result<()> {
		self.out.write_char('\n')?;
		self.at_line_start = true;
		Ok(())
	}

	/// Write a folded block string body, wrapping to the specified column width.
	/// Preserves blank lines between paragraphs. Each emitted line is indented
	/// exactly at `indent` depth. Wrapping prefers breaking at the last whitespace not
	/// exceeding the limit; if none is present, performs a hard break.
	fn write_folded_block_with_width(
		&mut self,
		s: &str,
		indent: usize,
		wrap_col: usize,
	) -> Result<()> {
		// Precompute indent prefix for this block body and reuse it for each emitted line.
		let mut indent_buf: String = String::new();
		let spaces = self.indent_step * indent;
		if spaces > 0 {
			indent_buf.reserve(spaces);
			for _ in 0..spaces {
				indent_buf.push(' ');
			}
		}
		let indent_str = indent_buf.as_str();

		for line in s.split('\n') {
			if line.is_empty() {
				// Preserve empty lines between paragraphs
				self.out.write_str(indent_str)?;
				self.at_line_start = false;
				self.newline()?;
				continue;
			}
			let mut start = 0; // byte index
			let mut last_space_byte: Option<usize> = None;
			let mut col = 0usize; // column in chars
			for (i, ch) in line.char_indices() {
				// track potential break positions
				if ch.is_whitespace() {
					last_space_byte = Some(i);
				}
				col += 1;
				if col > wrap_col {
					let break_at = last_space_byte.unwrap_or(i);
					// Emit [start, break_at)
					self.out.write_str(indent_str)?;
					self.at_line_start = false;
					// Safety: break_at is on char boundary
					let slice = &line[start..break_at];
					self.out.write_str(slice)?;
					self.newline()?;
					// Advance start: skip the whitespace if we broke at space
					start = if let Some(sp) = last_space_byte {
						sp + 1
					} else {
						break_at
					};
					// Reset trackers starting at new segment. We intentionally do not try
					// to recompute `col` relative to the current `i` because `start` may
					// have advanced past `i` when we broke at the current whitespace.
					// Starting a fresh column count avoids invalid slice ranges.
					last_space_byte = None;
					col = 0;
				}
			}
			// Emit the tail if any
			if start < line.len() {
				self.out.write_str(indent_str)?;
				self.at_line_start = false;
				self.out.write_str(&line[start..])?;
				self.newline()?;
			} else {
				// If start == line.len(), the line ended exactly at a wrap boundary; still emit an empty line
				self.out.write_str(indent_str)?;
				self.at_line_start = false;
				self.newline()?;
			}
		}
		Ok(())
	}

	/// Write a folded block string body using the configured `folded_wrap_col`.
	fn write_folded_block(&mut self, s: &str, indent: usize) -> Result<()> {
		self.write_folded_block_with_width(s, indent, self.folded_wrap_col)
	}

	/// Write a folded block string body using absolute spaces for indentation.
	fn write_folded_block_with_spaces(&mut self, s: &str, spaces: usize) -> Result<()> {
		// Similar to write_folded_block_with_width but uses absolute spaces
		let wrap_col = self.folded_wrap_col;
		let mut indent_buf: String = String::new();
		if spaces > 0 {
			indent_buf.reserve(spaces);
			for _ in 0..spaces {
				indent_buf.push(' ');
			}
		}
		let indent_str = indent_buf.as_str();

		for line in s.lines() {
			// Blank lines in the original content become blank lines in output
			if line.is_empty() {
				self.newline()?;
				continue;
			}
			// For non-blank lines, wrap at wrap_col characters per emitted line.
			let mut start = 0;
			while start < line.len() {
				let remaining = &line[start..];
				if remaining.len() <= wrap_col {
					// Fits without wrapping
					self.out.write_str(indent_str)?;
					self.at_line_start = false;
					self.out.write_str(remaining)?;
					self.newline()?;
					break;
				}
				// Wrapping needed; find break point
				let end = start + wrap_col;
				// Search backwards from `end` for last whitespace in the slice
				let break_at = line[start..end].rfind(char::is_whitespace);
				let break_pos = if let Some(offset) = break_at {
					start + offset
				} else {
					// No whitespace found; hard break at wrap_col
					end
				};
				self.out.write_str(indent_str)?;
				self.at_line_start = false;
				self.out.write_str(&line[start..break_pos])?;
				self.newline()?;
				// Skip past any whitespace character used as break point
				start = break_pos;
				while start < line.len() && line[start..].starts_with(char::is_whitespace) {
					start += 1;
				}
			}
			// If the line was entirely processed (start == line.len()), we're done
			if start >= line.len() {
				continue;
			}
			// Handle remaining content after last break
			if start < line.len() {
				self.out.write_str(indent_str)?;
				self.at_line_start = false;
				self.out.write_str(&line[start..])?;
				self.newline()?;
			}
		}
		Ok(())
	}

	/// Write a scalar either as plain or as double-quoted with minimal escapes.
	/// Called by most `serialize_*` primitive methods.
	fn write_plain_or_quoted(&mut self, s: &str) -> Result<()> {
		if is_plain_safe(s) {
			self.out.write_str(s)?;
			Ok(())
		} else {
			self.write_quoted(s)
		}
	}

	/// Write a quoted string, choosing between single and double quotes (matching Go's yaml.v3).
	/// - Double quotes: for strings that look like other YAML types (empty, bool, number, null)
	///   or when escaping is needed (control chars, etc.)
	/// - Single quotes: for strings that just need quoting due to special syntax chars
	///   (internal single quotes are escaped by doubling: ' → '')
	fn write_quoted(&mut self, s: &str) -> Result<()> {
		let needs_escaping = s.chars().any(|c| {
			c.is_control()
                || matches!(
                    c,
                    '\n' | '\r'
                        | '\t'
                        | '\0'
                        | '\u{7}'
                        | '\u{8}'
                        | '\u{b}'
                        | '\u{c}'
                        | '\u{1b}'
                        | '\u{FEFF}'
                        | '\u{0085}'
                        | '\u{2028}'
                        | '\u{2029}'
                )
                // High-unicode characters (non-BMP, >= U+10000) need double quotes
                // Go yaml.v2 escapes these with \U and 8 hex digits
                || (c as u32) >= 0x10000
		});

		// Check if string looks like another YAML type (needs double quotes to disambiguate)
		let looks_like_other_type = s.is_empty()
			|| s == "~"
			|| s.eq_ignore_ascii_case("null")
			|| s.eq_ignore_ascii_case("true")
			|| s.eq_ignore_ascii_case("false")
			|| s.eq_ignore_ascii_case("yes")
			|| s.eq_ignore_ascii_case("no")
			|| s.eq_ignore_ascii_case("on")
			|| s.eq_ignore_ascii_case("off")
			|| s.parse::<i64>().is_ok()
			|| s.parse::<f64>().is_ok_and(|f| f.is_finite())
			|| crate::ser_quoting::looks_like_date_or_timestamp(s);

		// Use single quotes unless:
		// 1. String needs escaping (control chars can't be escaped in single quotes)
		// 2. String looks like another YAML type (Go uses double quotes for those)
		// 3. prefer_double_quotes option is enabled (matches Go yaml.v2 behavior)
		// Internal single quotes are escaped by doubling (' → '')
		let use_single_quotes =
			!needs_escaping && !looks_like_other_type && !self.prefer_double_quotes;
		if use_single_quotes {
			// Use single quotes - escape internal ' by doubling to ''
			self.out.write_char('\'')?;
			for ch in s.chars() {
				if ch == '\'' {
					self.out.write_str("''")?;
				} else {
					self.out.write_char(ch)?;
				}
			}
			self.out.write_char('\'')?;
		} else {
			// Use double quotes with escaping as needed
			self.out.write_char('"')?;
			for ch in s.chars() {
				match ch {
					'\\' => self.out.write_str("\\\\")?,
					'"' => self.out.write_str("\\\"")?,
					// YAML named escapes for common control characters
					'\0' => self.out.write_str("\\0")?,
					'\u{7}' => self.out.write_str("\\a")?,
					'\u{8}' => self.out.write_str("\\b")?,
					'\t' => self.out.write_str("\\t")?,
					'\n' => self.out.write_str("\\n")?,
					'\u{b}' => self.out.write_str("\\v")?,
					'\u{c}' => self.out.write_str("\\f")?,
					'\r' => self.out.write_str("\\r")?,
					'\u{1b}' => self.out.write_str("\\e")?,
					// Unicode BOM should use the standard \u escape rather than Rust's \u{...}
					'\u{FEFF}' => self.out.write_str("\\uFEFF")?,
					// YAML named escapes for Unicode separators
					'\u{0085}' => self.out.write_str("\\N")?,
					'\u{2028}' => self.out.write_str("\\L")?,
					'\u{2029}' => self.out.write_str("\\P")?,
					c if (c as u32) <= 0xFF
						&& (c.is_control() || (0x7F..=0x9F).contains(&(c as u32))) =>
					{
						write!(self.out, "\\x{:02X}", c as u32)?
					}
					c if (c as u32) <= 0xFFFF
						&& (c.is_control() || (0x7F..=0x9F).contains(&(c as u32))) =>
					{
						write!(self.out, "\\u{:04X}", c as u32)?
					}
					// Non-BMP characters (>= U+10000) - escape with \U and 8 hex digits
					// This matches Go yaml.v3's behavior for characters like emoji
					c if (c as u32) >= 0x10000 => write!(self.out, "\\U{:08X}", c as u32)?,
					c => self.out.write_char(c)?,
				}
			}
			self.out.write_char('"')?;
		}
		Ok(())
	}

	/// Write a quoted string with line wrapping for long strings.
	/// This matches Go's yaml.v3 behavior for strings that need quoting:
	/// they use quoted style with continuation lines instead of folded block style.
	///
	/// In YAML, multi-line quoted scalars fold newlines to spaces, so we can
	/// safely insert newlines at word boundaries within the quoted content.
	///
	/// `continuation_indent` is the number of spaces to indent continuation lines.
	/// `first_line_offset` is extra characters already on the first line (e.g., key + `: `).
	fn write_quoted_with_wrap(
		&mut self,
		s: &str,
		continuation_indent: usize,
		wrap_col: usize,
		first_line_offset: usize,
	) -> Result<()> {
		// Determine quote style (same logic as write_quoted)
		let needs_escaping = s.chars().any(|c| {
			c.is_control()
                || matches!(
                    c,
                    '\n' | '\r'
                        | '\t'
                        | '\0'
                        | '\u{7}'
                        | '\u{8}'
                        | '\u{b}'
                        | '\u{c}'
                        | '\u{1b}'
                        | '\u{FEFF}'
                        | '\u{0085}'
                        | '\u{2028}'
                        | '\u{2029}'
                )
                // High-unicode characters (non-BMP, >= U+10000) need double quotes
                // Go yaml.v2 escapes these with \U and 8 hex digits
                || (c as u32) >= 0x10000
		});

		// Check if string looks like another YAML type (needs double quotes to disambiguate)
		let looks_like_other_type = s.is_empty()
			|| s == "~"
			|| s.eq_ignore_ascii_case("null")
			|| s.eq_ignore_ascii_case("true")
			|| s.eq_ignore_ascii_case("false")
			|| s.eq_ignore_ascii_case("yes")
			|| s.eq_ignore_ascii_case("no")
			|| s.eq_ignore_ascii_case("on")
			|| s.eq_ignore_ascii_case("off")
			|| s.parse::<i64>().is_ok()
			|| s.parse::<f64>().is_ok_and(|f| f.is_finite())
			|| crate::ser_quoting::looks_like_date_or_timestamp(s);

		// Use single quotes unless:
		// 1. Escaping needed
		// 2. Type-ambiguous
		// 3. prefer_double_quotes option is enabled (matches Go yaml.v2 behavior)
		// Internal single quotes are escaped by doubling (' → '')
		let use_single_quotes =
			!needs_escaping && !looks_like_other_type && !self.prefer_double_quotes;

		// Precompute indent string for continuation lines
		let mut indent_buf = String::new();
		for _ in 0..continuation_indent {
			indent_buf.push(' ');
		}
		let indent_str = indent_buf.as_str();

		if use_single_quotes {
			// Go yaml.v2 line wrapping behavior (from emitterc.go):
			// At each SPACE character, check if:
			//   - column > width AND previous char was NOT space AND next char is NOT space
			// If all conditions met: break (write newline+indent instead of space).
			// For single-quoted strings, we don't need to escape leading spaces with \.

			self.out.write_char('\'')?;
			let mut column = first_line_offset + 1; // +1 for opening quote

			// Track if previous character was a space
			let mut spaces = false;

			let chars: Vec<char> = s.chars().collect();
			for (i, &ch) in chars.iter().enumerate() {
				if ch == ' ' {
					// Space character - check if we should break
					// Key condition from go-yaml: !is_space(value, i+1) - don't break if next is also space
					let next_is_space = i + 1 < chars.len() && chars[i + 1] == ' ';
					let should_break = !spaces
						&& column > wrap_col
						&& i > 0 && i < chars.len() - 1
						&& !next_is_space;

					if should_break {
						// Break here - write newline and indent instead of space
						self.newline()?;
						self.out.write_str(indent_str)?;
						self.at_line_start = false;
						column = continuation_indent;
						// Skip this space (it's consumed by the break)
					} else {
						// Write the space normally
						self.out.write_char(' ')?;
						column += 1;
					}
					spaces = true;
				} else if ch == '\'' {
					// Escape single quotes by doubling them
					self.out.write_str("''")?;
					column += 2;
					spaces = false;
				} else {
					// Non-space character - just write it
					self.out.write_char(ch)?;
					column += 1;
					spaces = false;
				}
			}

			self.out.write_char('\'')?;
		} else {
			// Double-quoted style - need to escape content first, then split
			// Use the original space-based splitting approach which handles
			// consecutive spaces correctly with \ escaping at continuation start
			let mut escaped = String::new();
			for ch in s.chars() {
				match ch {
					'\\' => escaped.push_str("\\\\"),
					'"' => escaped.push_str("\\\""),
					'\0' => escaped.push_str("\\0"),
					'\u{7}' => escaped.push_str("\\a"),
					'\u{8}' => escaped.push_str("\\b"),
					'\t' => escaped.push_str("\\t"),
					'\n' => escaped.push_str("\\n"),
					'\u{b}' => escaped.push_str("\\v"),
					'\u{c}' => escaped.push_str("\\f"),
					'\r' => escaped.push_str("\\r"),
					'\u{1b}' => escaped.push_str("\\e"),
					'\u{FEFF}' => escaped.push_str("\\uFEFF"),
					'\u{0085}' => escaped.push_str("\\N"),
					'\u{2028}' => escaped.push_str("\\L"),
					'\u{2029}' => escaped.push_str("\\P"),
					c if (c as u32) <= 0xFF
						&& (c.is_control() || (0x7F..=0x9F).contains(&(c as u32))) =>
					{
						use std::fmt::Write;
						write!(escaped, "\\x{:02X}", c as u32).unwrap();
					}
					c if (c as u32) <= 0xFFFF
						&& (c.is_control() || (0x7F..=0x9F).contains(&(c as u32))) =>
					{
						use std::fmt::Write;
						write!(escaped, "\\u{:04X}", c as u32).unwrap();
					}
					// Non-BMP characters (>= U+10000) - escape with \U and 8 hex digits
					// This matches Go yaml.v3's behavior for characters like emoji
					c if (c as u32) >= 0x10000 => {
						use std::fmt::Write;
						write!(escaped, "\\U{:08X}", c as u32).unwrap();
					}
					c => escaped.push(c),
				}
			}

			// Go yaml.v2 line wrapping behavior (from emitterc.go):
			// At each SPACE character, check if column > width AND previous char was NOT space.
			// If yes: break (write newline+indent), and if next char is also space, write \ to escape.
			// The space itself is "consumed" by the break.

			self.out.write_char('"')?;
			let mut column = first_line_offset + 1; // +1 for the opening quote

			// Track if previous character was a space (for break decision)
			let mut spaces = false;

			// Must iterate over characters (not bytes) to handle multi-byte UTF-8 correctly
			let chars: Vec<char> = escaped.chars().collect();
			for (i, &ch) in chars.iter().enumerate() {
				if ch == ' ' {
					// Space character - check if we should break
					// Note: for double-quoted, we DO break even if next is space,
					// but escape the next space with \
					let should_break = !spaces && column > wrap_col && i > 0 && i < chars.len() - 1;

					if should_break {
						// Break here - write newline and indent instead of space
						self.newline()?;
						self.out.write_str(indent_str)?;
						self.at_line_start = false;
						column = continuation_indent;

						// If next char is also a space, escape it with \
						if i + 1 < chars.len() && chars[i + 1] == ' ' {
							self.out.write_char('\\')?;
							column += 1;
						}

						// Skip this space (it's consumed by the break)
					} else {
						// Write the space normally
						self.out.write_char(' ')?;
						column += 1;
					}
					spaces = true;
				} else {
					// Non-space character - write it
					// For multi-byte UTF-8 chars, this writes the whole character correctly
					self.out.write_char(ch)?;
					// Column tracking: multi-byte chars still take 1 column visually in most cases
					// (though some emoji are wider - we ignore that complexity for now)
					column += 1;
					spaces = false;
				}
			}

			self.out.write_char('"')?;
		}

		Ok(())
	}

	/// Write a plain scalar with line wrapping for long strings.
	/// This matches Go's yaml.v2 behavior where plain scalars are wrapped
	/// without converting to folded block style.
	///
	/// In YAML, plain scalars can span multiple lines where newlines are folded to spaces.
	/// `continuation_indent` is the number of spaces to indent continuation lines.
	/// `first_line_offset` is extra characters already on the first line (e.g., key + `: `).
	fn write_plain_with_wrap(
		&mut self,
		s: &str,
		continuation_indent: usize,
		wrap_col: usize,
		first_line_offset: usize,
	) -> Result<()> {
		// Precompute indent string for continuation lines
		let mut indent_buf = String::new();
		for _ in 0..continuation_indent {
			indent_buf.push(' ');
		}
		let indent_str = indent_buf.as_str();

		// Go yaml.v2's exact condition for breaking at a space (from emitterc.go):
		// allow_breaks && !spaces && column > best_width && !is_space(value, i+1)
		//
		// This means break at a space if:
		// 1. Previous char was NOT a space (!spaces)
		// 2. Column already exceeds best_width
		// 3. NEXT char is NOT a space (so we're at end of space run, or single space)
		//
		// We process character-by-character to match Go's exact behavior.
		let mut column = first_line_offset;
		let mut spaces = false; // Was previous character a space?
		let chars: Vec<char> = s.chars().collect();
		let mut i = 0;

		while i < chars.len() {
			let c = chars[i];
			let next_is_space = i + 1 < chars.len() && chars[i + 1] == ' ';

			if c == ' ' {
				// Go's break condition at a space:
				// !spaces && column > best_width && !is_space(next)
				if !spaces && column > wrap_col && !next_is_space {
					// Break: emit newline + indent, consume all spaces
					self.newline()?;
					self.out.write_str(indent_str)?;
					self.at_line_start = false;
					column = continuation_indent;
					// Skip all consecutive spaces
					while i < chars.len() && chars[i] == ' ' {
						i += 1;
					}
					continue;
				} else {
					// No break, write the space
					self.out.write_char(c)?;
					column += 1;
					spaces = true;
				}
			} else {
				self.out.write_char(c)?;
				column += 1;
				spaces = false;
			}
			i += 1;
		}

		Ok(())
	}

	/// Like `write_plain_or_quoted`, but intended for VALUE position where ':' is allowed.
	/// Uses stricter quoting in flow style (commas/brackets are structural) and more
	/// permissive quoting in block style (matching Go's yaml.v3 behavior).
	#[inline]
	fn write_plain_or_quoted_value(&mut self, s: &str) -> Result<()> {
		// In block style, we can be more permissive (commas and brackets are allowed)
		// In flow style, we need stricter quoting
		let is_safe = if self.in_flow == 0 {
			is_plain_block_value_safe(s)
		} else {
			is_plain_value_safe(s)
		};

		if is_safe {
			self.out.write_str(s)?;
			Ok(())
		} else {
			// Force quoted style for unsafe value tokens
			self.write_quoted(s)
		}
	}

	/// Serialize a tagged scalar of the form `!!Type value` using plain or quoted style for
	/// the value depending on its content.
	fn serialize_tagged_scalar(&mut self, enum_name: &str, variant: &str) -> Result<()> {
		self.write_scalar_prefix_if_anchor()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		self.out.write_str("!!")?;
		self.out.write_str(enum_name)?;
		self.out.write_char(' ')?;
		self.write_plain_or_quoted_value(variant)?;
		self.write_end_of_scalar()
	}

	/// If an anchor is pending for the next scalar, emit `&name ` prefix.
	/// Used for in-flow scalars.
	#[inline]
	fn write_scalar_prefix_if_anchor(&mut self) -> Result<()> {
		if let Some(id) = self.pending_anchor_id.take() {
			if self.at_line_start {
				self.write_indent(self.depth)?;
			}
			self.out.write_char('&')?;
			self.write_anchor_name(id)?;
			self.out.write_char(' ')?;
		}
		Ok(())
	}

	/// If an anchor is pending for the next complex node (seq/map),
	/// emit it on its own line before the node.
	#[inline]
	fn write_anchor_for_complex_node(&mut self) -> Result<()> {
		if let Some(id) = self.pending_anchor_id.take() {
			if self.at_line_start {
				self.write_indent(self.depth)?;
			}
			self.write_space_if_pending()?;
			self.out.write_char('&')?;
			self.write_anchor_name(id)?;
			self.newline()?;
		}
		Ok(())
	}

	/// Emit an alias `*name`. Adds a newline in block style.
	/// Used when a previously defined anchor is referenced again.
	#[inline]
	fn write_alias_id(&mut self, id: AnchorId) -> Result<()> {
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		self.write_space_if_pending()?;
		self.out.write_char('*')?;
		self.write_anchor_name(id)?;
		// Use the shared end-of-scalar path so pending inline comments are appended in block style
		self.write_end_of_scalar()?;
		Ok(())
	}

	/// Determine whether the next sequence should be emitted in flow style.
	/// Consumes any pending flow hint.
	#[inline]
	fn take_flow_for_seq(&mut self) -> bool {
		if self.in_flow > 0 {
			true
		} else {
			matches!(self.pending_flow.take(), Some(PendingFlow::AnySeq))
		}
	}
	/// Determine whether the next mapping should be emitted in flow style.
	/// Consumes any pending flow hint.
	#[inline]
	fn take_flow_for_map(&mut self) -> bool {
		if self.in_flow > 0 {
			true
		} else {
			matches!(self.pending_flow.take(), Some(PendingFlow::AnyMap))
		}
	}

	/// Temporarily mark that we are inside a flow container while running `f`.
	/// Ensures proper comma insertion and line handling for nested flow nodes.
	#[inline]
	fn with_in_flow<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
		self.in_flow += 1;
		let r = f(self);
		self.in_flow -= 1;
		r
	}
}

// ------------------------------------------------------------
// Impl Serializer for YamlSer
// ------------------------------------------------------------

impl<'a, 'b, W: Write> Serializer for &'a mut YamlSer<'b, W> {
	type Ok = ();
	type Error = Error;

	type SerializeSeq = SeqSer<'a, 'b, W>;
	type SerializeTuple = SeqSer<'a, 'b, W>;
	type SerializeTupleStruct = TupleSer<'a, 'b, W>;
	type SerializeTupleVariant = TupleVariantSer<'a, 'b, W>;
	type SerializeMap = MapSer<'a, 'b, W>;
	type SerializeStruct = StructSer<'a, 'b, W>;
	type SerializeStructVariant = StructVariantSer<'a, 'b, W>;

	// -------- Scalars --------

	fn serialize_bool(self, v: bool) -> Result<()> {
		self.write_space_if_pending()?;
		self.write_scalar_prefix_if_anchor()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		self.out.write_str(if v { "true" } else { "false" })?;
		self.write_end_of_scalar()?;
		Ok(())
	}

	fn serialize_i8(self, v: i8) -> Result<()> {
		self.serialize_i64(v as i64)
	}
	fn serialize_i16(self, v: i16) -> Result<()> {
		self.serialize_i64(v as i64)
	}
	fn serialize_i32(self, v: i32) -> Result<()> {
		self.serialize_i64(v as i64)
	}
	fn serialize_i64(self, v: i64) -> Result<()> {
		self.write_space_if_pending()?;
		self.write_scalar_prefix_if_anchor()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		// Use scientific notation for large integers when threshold is set
		if let Some(threshold) = self.scientific_notation_threshold {
			if v.unsigned_abs() >= threshold {
				// Format as scientific notation to match Go's yaml.v3
				// Go uses "+07" for positive exponents, Rust uses "7", so we format manually
				write_go_scientific(self.out, v as f64)?;
			} else {
				write!(self.out, "{}", v)?;
			}
		} else {
			write!(self.out, "{}", v)?;
		}
		self.write_end_of_scalar()?;
		Ok(())
	}

	fn serialize_i128(self, v: i128) -> Result<()> {
		self.write_space_if_pending()?;
		self.write_scalar_prefix_if_anchor()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		write!(self.out, "{}", v)?;
		self.write_end_of_scalar()?;
		Ok(())
	}

	fn serialize_u8(self, v: u8) -> Result<()> {
		self.serialize_u64(v as u64)
	}
	fn serialize_u16(self, v: u16) -> Result<()> {
		self.serialize_u64(v as u64)
	}
	fn serialize_u32(self, v: u32) -> Result<()> {
		self.serialize_u64(v as u64)
	}
	fn serialize_u64(self, v: u64) -> Result<()> {
		self.write_space_if_pending()?;
		self.write_scalar_prefix_if_anchor()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		// Use scientific notation for large integers when threshold is set
		if let Some(threshold) = self.scientific_notation_threshold {
			if v >= threshold {
				// Format as scientific notation to match Go's yaml.v3
				// Go uses "+07" for positive exponents, Rust uses "7", so we format manually
				write_go_scientific(self.out, v as f64)?;
			} else {
				write!(self.out, "{}", v)?;
			}
		} else {
			write!(self.out, "{}", v)?;
		}
		self.write_end_of_scalar()?;
		Ok(())
	}

	fn serialize_u128(self, v: u128) -> Result<()> {
		self.write_space_if_pending()?;
		self.write_scalar_prefix_if_anchor()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		write!(self.out, "{}", v)?;
		self.write_end_of_scalar()?;
		Ok(())
	}

	fn serialize_f32(self, v: f32) -> Result<()> {
		self.serialize_f64(v as f64)
	}
	fn serialize_f64(self, v: f64) -> Result<()> {
		self.write_space_if_pending()?;
		self.write_scalar_prefix_if_anchor()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		if v.is_nan() {
			self.out.write_str(".nan")?;
		} else if v.is_infinite() {
			if v.is_sign_positive() {
				self.out.write_str(".inf")?;
			} else {
				self.out.write_str("-.inf")?;
			}
		} else if self.go_style_negative_zero && v == 0.0 && v.is_sign_negative() {
			// Ahead of the threshold branches, so the spelling does not depend
			// on how scientific notation happens to be configured.
			self.out.write_str("-0")?;
		} else if let Some(threshold) = self.scientific_notation_threshold {
			// Apply threshold to floats: use scientific notation for large values
			// This matches Go yaml.v3 behavior where floats >= 1 million use scientific notation
			let abs_v = v.abs();
			if abs_v >= threshold as f64 {
				// Format as scientific notation to match Go's yaml.v3
				// Go uses "+06" for positive exponents, Rust uses "6", so we format manually
				write_go_scientific(self.out, v)?;
			} else if let Some(small_threshold) = self.scientific_notation_small_threshold {
				// Check for small numbers that should use scientific notation
				// This matches Go yaml.v3 behavior where small floats like 0.00002 become 2e-05
				if abs_v > 0.0 && abs_v < small_threshold {
					write_go_scientific(self.out, v)?;
				} else {
					// Below large threshold and above small threshold: use ryu for fast formatting
					let mut buf = ryu::Buffer::new();
					let s = buf.format(v);
					if !s.contains('.') && !s.contains('e') && !s.contains('E') {
						self.out.write_str(s)?;
						self.out.write_str(".0")?;
					} else {
						self.out.write_str(s)?;
					}
				}
			} else {
				// Below threshold: use ryu for fast formatting
				let mut buf = ryu::Buffer::new();
				let s = buf.format(v);
				if !s.contains('.') && !s.contains('e') && !s.contains('E') {
					self.out.write_str(s)?;
					self.out.write_str(".0")?;
				} else {
					self.out.write_str(s)?;
				}
			}
		} else {
			// Avoid scientific notation - use plain decimal format
			// Check if it's a whole number (no fractional part)
			if v.fract() == 0.0 && v.abs() < 1e16 {
				// Write as integer-like format with .0 suffix
				write!(self.out, "{:.0}.0", v)?;
			} else {
				// Write with enough precision to round-trip, avoiding scientific notation
				// Use a reasonable precision that avoids trailing zeros
				let formatted = format!("{:.15}", v);
				let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
				if trimmed.contains('.') {
					self.out.write_str(trimmed)?;
				} else {
					self.out.write_str(trimmed)?;
					self.out.write_str(".0")?;
				}
			}
		}
		self.write_end_of_scalar()?;
		Ok(())
	}

	fn serialize_char(self, v: char) -> Result<()> {
		self.write_space_if_pending()?;
		let mut buf = [0u8; 4];
		self.serialize_str(v.encode_utf8(&mut buf))
	}

	fn serialize_str(self, v: &str) -> Result<()> {
		// If no explicit style pending, and option is enabled, auto-select block style
		// similar to LitStr/FoldStr wrappers to improve compatibility with Go's yaml.v3.
		// However, DISABLE auto block scalars when the string needs quoting as a value
		// (per ser_quoting::is_plain_value_safe), unless the only reason it needs quoting
		// is the presence of newlines themselves. This ensures cases like "hey:\n" remain
		// quoted (because trimmed value ends with ':'), even when prefer_block_scalars=true.
		// Skip block scalar auto-selection if string contains tabs - Go yaml.v2 falls back
		// to double-quoted style for strings with tabs to ensure proper escape sequences.
		// Also skip if content has characters that need escaping (BOM, non-BMP Unicode like emoji,
		// control chars, etc.) - Go yaml.v2 uses double-quoted style for such content.
		if self.pending_str_style.is_none()
			&& self.prefer_block_scalars
			&& self.in_flow == 0
			&& !v.contains('\t')
			&& !has_trailing_whitespace(v)
			&& !has_chars_needing_escape(v)
		{
			if v.contains('\n') {
				// Use block scalar style for all multiline strings. Block scalars can contain
				// ANY content (including YAML-like syntax like "- item" or "key: value")
				// because the content is literal text, not parsed as YAML structure.
				// This matches Go yaml.v3 behavior which uses block scalar for multiline strings
				// that don't have trailing whitespace on lines.
				self.pending_str_style = Some(StrStyle::Literal);
				self.pending_str_from_auto = true;
			} else {
				// Single-line string. If it needs quoting as a value, don't auto-fold.
				// When line_width is None, don't auto-fold - this matches Go yaml.v3 behavior
				// where Marshal() with no line width doesn't fold long strings.
				// When line_width is set, also skip auto-fold here - let the line_width
				// wrapping logic handle long strings with plain scalar style instead.
				// (We only auto-fold multiline strings, not single-line ones)
			}
		}
		if let Some(style) = self.pending_str_style.take() {
			// Emit block string. If we are a mapping value, YAML requires a space after ':'.
			// Insert it now if pending.
			self.write_space_if_pending()?;
			// Determine base indentation for block scalar body.
			// Prefer current_map_depth if set (we're in a nested map context), then
			// after_dash_depth (we're directly in a sequence context), then depth.
			let base = if let Some(d) = self.current_map_depth {
				d
			} else if let Some(d) = self.after_dash_depth {
				d
			} else {
				self.depth
			};
			if self.at_line_start {
				self.write_indent(base)?;
			}
			match style {
				StrStyle::Literal => {
					// Determine trailing newline count to select chomp indicator:
					//  - 0 → "|-" (strip)
					//  - 1 → "|" (clip)
					//  - >=2 → "|+" (keep)
					// If block_scalar_chomp is set, use that override instead.
					let content = v.trim_end_matches('\n');
					let trailing_nl = v.len() - content.len();

					// Check if we need an explicit indentation indicator.
					// YAML requires this when the first line of the content starts with
					// whitespace (space or tab) or if the content starts with an empty line.
					let first_line = content.split('\n').next().unwrap_or("");
					let needs_indent_indicator = first_line.is_empty()
						|| first_line.starts_with(' ')
						|| first_line.starts_with('\t');

					// Determine effective chomp indicator and trailing newlines to emit
					let (chomp_char, effective_trailing_nl) = match self.block_scalar_chomp {
						Some(ChompIndicator::Strip) => ("-", 0),
						Some(ChompIndicator::Clip) => ("", 1),
						Some(ChompIndicator::Keep) => ("+", trailing_nl.max(2)),
						None => {
							// go-yaml v2 uses keep chomping (+) when content is empty
							// (the string consists only of trailing newlines).
							// This produces output like |2+ for "\n" instead of |2
							if content.is_empty() && trailing_nl > 0 {
								// For empty content, use trailing_nl directly (not max(2))
								// to preserve the exact number of trailing newlines
								("+", trailing_nl)
							} else {
								match trailing_nl {
									0 => ("-", 0),
									1 => ("", 1),
									_ => ("+", trailing_nl),
								}
							}
						}
					};

					// Write block scalar header: | + optional indent indicator + chomp
					self.out.write_char('|')?;
					if needs_indent_indicator {
						// Use indent_step as the indentation width indicator
						write!(self.out, "{}", self.indent_step)?;
					}
					self.out.write_str(chomp_char)?;
					self.newline()?;

					// Emit body lines. For non-empty content, write each line exactly once.
					// For keep chomping (>=2), append (effective_trailing_nl - 1) visual empty lines.
					// Special case: empty original content with at least one trailing newline
					// should produce a single empty content line (tests expect this for "\n").
					// Determine indentation for the body relative to the header line base.
					// The block scalar body must be indented at least one more level than the
					// header line. When block_scalar_indent_in_seq is set and we're a DIRECT
					// item in a sequence (not inside an object), use absolute spaces.
					let spaces = if self.after_dash_depth.is_some()
						&& self.current_map_depth.is_none()
						&& self.block_scalar_indent_in_seq.is_some()
					{
						self.block_scalar_indent_in_seq.unwrap()
					} else {
						self.indent_step * (base + 1)
					};
					// Precompute body indent string once for the entire block
					let mut indent_buf: String = String::new();
					if spaces > 0 {
						indent_buf.reserve(spaces);
						for _ in 0..spaces {
							indent_buf.push(' ');
						}
					}
					let indent_str = indent_buf.as_str();

					if content.is_empty() {
						// For empty content, output empty lines (no indentation) for each trailing newline
						// Go yaml.v2 outputs truly empty lines, not indented ones
						for _ in 0..effective_trailing_nl {
							self.newline()?;
						}
					} else {
						for line in content.split('\n') {
							// Only write indentation for non-empty lines to avoid trailing whitespace
							if !line.is_empty() {
								self.out.write_str(indent_str)?;
								self.at_line_start = false;
								self.out.write_str(line)?;
							}
							self.newline()?;
						}
						if effective_trailing_nl >= 2 {
							for _ in 0..(effective_trailing_nl - 1) {
								// Trailing empty lines also don't get indentation
								self.newline()?;
							}
						}
					}
				}
				StrStyle::Folded => {
					// Determine chomp indicator for folded style.
					// If block_scalar_chomp is set, use that override.
					// Otherwise, use auto-detection for auto-selected folded style,
					// or plain '>' for explicit FoldStr/FoldString wrappers.
					let chomp_str = if let Some(chomp) = self.block_scalar_chomp {
						match chomp {
							ChompIndicator::Strip => ">-",
							ChompIndicator::Clip => ">",
							ChompIndicator::Keep => ">+",
						}
					} else if self.pending_str_from_auto {
						// Auto-selected folded style: choose chomping based on trailing newlines
						// to preserve exact content on round-trip.
						let content = v.trim_end_matches('\n');
						let trailing_nl = v.len() - content.len();
						match trailing_nl {
							0 => ">-",
							1 => ">",
							_ => ">+",
						}
					} else {
						// Explicit FoldStr/FoldString wrappers historically used plain '>'
						// regardless of trailing newline; keep that behavior for compatibility.
						">"
					};
					self.out.write_str(chomp_str)?;
					self.newline()?;
					// Same body indentation rule as literal: one level deeper than the header base.
					// When block_scalar_indent_in_seq is set and we're a DIRECT item in a sequence
					// (not inside an object), use that as absolute spaces.
					if self.after_dash_depth.is_some()
						&& self.current_map_depth.is_none()
						&& self.block_scalar_indent_in_seq.is_some()
					{
						self.write_folded_block_with_spaces(
							v,
							self.block_scalar_indent_in_seq.unwrap(),
						)?;
					} else {
						self.write_folded_block(v, base + 1)?;
					}
				}
			}
			// reset auto flag after using pending style
			self.pending_str_from_auto = false;
			return Ok(());
		}

		// Auto-detect block scalars for multi-line strings when prefer_block_scalars is enabled
		// and we're not inside a flow context (flow style doesn't support block scalars).
		// Skip if string contains tabs - Go yaml.v2 falls back to double-quoted for tabs.
		// Skip if string has trailing whitespace on lines - Go yaml.v3 uses double-quoted for those.
		// Also skip if content has characters that need escaping (BOM, non-BMP Unicode like emoji,
		// control chars, etc.) - Go yaml.v2 uses double-quoted style for such content.
		if self.prefer_block_scalars
			&& self.in_flow == 0
			&& v.contains('\n')
			&& !v.contains('\t')
			&& !has_trailing_whitespace(v)
			&& !has_chars_needing_escape(v)
		{
			self.write_space_if_pending()?;
			// Prefer current_map_depth if set (we're in a nested map context), then
			// after_dash_depth (we're directly in a sequence context), then depth.
			let base = if let Some(d) = self.current_map_depth {
				d
			} else if let Some(d) = self.after_dash_depth {
				d
			} else {
				self.depth
			};
			if self.at_line_start {
				self.write_indent(base)?;
			}

			// Check if we need an explicit indentation indicator.
			// YAML requires this when the first line of the content starts with
			// whitespace (space or tab) or if the content starts with an empty line.
			let content_trimmed = v.trim_end_matches('\n');
			let first_line = content_trimmed.split('\n').next().unwrap_or("");
			let needs_indent_indicator = first_line.is_empty()
				|| first_line.starts_with(' ')
				|| first_line.starts_with('\t');

			// Determine chomp indicator: use override if set, otherwise auto-detect
			let chomp_char = if let Some(chomp) = self.block_scalar_chomp {
				match chomp {
					ChompIndicator::Strip => "-",
					ChompIndicator::Clip => "",
					ChompIndicator::Keep => "+",
				}
			} else {
				// Use strip indicator (-) if string doesn't end with newline,
				// otherwise use clip (no indicator) which preserves one trailing newline
				if v.ends_with('\n') { "" } else { "-" }
			};

			// Write block scalar header: | + optional indent indicator + chomp
			self.out.write_char('|')?;
			if needs_indent_indicator {
				write!(self.out, "{}", self.indent_step)?;
			}
			self.out.write_str(chomp_char)?;
			self.newline()?;

			// Literal block body indents one level deeper than the base indentation
			// When block_scalar_indent_in_seq is set and we're a DIRECT item in a sequence
			// (not inside an object within the sequence), use that as absolute spaces;
			// otherwise use depth-based calculation.
			let use_absolute_spaces = self.after_dash_depth.is_some()
				&& self.current_map_depth.is_none()
				&& self.block_scalar_indent_in_seq.is_some();
			let body_spaces = if use_absolute_spaces {
				self.block_scalar_indent_in_seq.unwrap()
			} else {
				0 // unused when use_absolute_spaces is false
			};
			let body_depth = if use_absolute_spaces {
				0 // unused when use_absolute_spaces is true
			} else if self.after_dash_depth.is_some() {
				self.depth + 1
			} else {
				self.current_map_depth
					.map(|d| d + 1)
					.unwrap_or(self.depth + 1)
			};

			// For strip chomp, emit content without trailing newlines
			// For clip chomp, emit content (YAML block scalar adds one trailing newline implicitly)
			// For keep chomp, emit all trailing newlines as extra blank lines
			let content = v.trim_end_matches('\n');
			let trailing_nl = v.len() - content.len();

			// Determine effective trailing newline count based on chomp mode
			let effective_trailing_nl = match self.block_scalar_chomp {
				Some(ChompIndicator::Strip) => 0,
				Some(ChompIndicator::Keep) => trailing_nl.max(2),
				Some(ChompIndicator::Clip) | None => {
					// Clip: YAML implicitly adds one trailing newline
					// If original had trailing newline(s), we emit the lines without them
					// and YAML adds back exactly one
					if trailing_nl >= 1 { 1 } else { 0 }
				}
			};

			// Emit content lines
			for line in content.split('\n') {
				// Empty lines should have no indentation (just the newline)
				if !line.is_empty() {
					if use_absolute_spaces {
						self.write_indent_spaces(body_spaces)?;
					} else {
						self.write_indent(body_depth)?;
					}
				}
				self.out.write_str(line)?;
				self.newline()?;
			}

			// For keep chomp with multiple trailing newlines, emit extra blank lines
			if effective_trailing_nl >= 2 {
				for _ in 0..(effective_trailing_nl - 1) {
					self.newline()?;
				}
			}

			return Ok(());
		}

		// Automatic line wrapping for long strings when line_width is set
		// This matches Go's yaml.v3 behavior with encoder.SetWidth()
		if let Some(max_width) = self.line_width {
			// Only wrap in block context (not in flow style)
			// Wrap strings that either:
			// 1. Don't contain newlines (single-line strings)
			// 2. Contain tabs (forces quoted style - can't use block scalars)
			// 3. Have trailing whitespace on lines (Go yaml.v3 uses double-quoted)
			// 4. Block scalars are disabled (prefer_block_scalars: false)
			// Strings with newlines but no tabs/trailing-ws AND block scalars enabled
			// should use block scalars (handled above).
			// Also use quoted wrap for strings with characters needing escape (BOM, emoji, etc.).
			let needs_quoted_wrap = !v.contains('\n')
				|| v.contains('\t')
				|| has_trailing_whitespace(v)
				|| !self.prefer_block_scalars
				|| has_chars_needing_escape(v);
			if self.in_flow == 0 && needs_quoted_wrap {
				// Calculate continuation indent based on context:
				// - In a map value context: continuation aligns past the key position
				// - After sequence dash (but not in nested map): continuation aligns with content after `- `
				//
				// For YAML to correctly parse multi-line plain scalars, continuation
				// lines must be indented more than the key/indicator position.
				//
				// Prioritize current_map_depth when set (we're in a map value context),
				// even if after_dash_depth is also set (nested map inside sequence item).
				let continuation_indent = if let Some(map_depth) = self.current_map_depth {
					// In map value context, indent one level deeper than the key
					(map_depth + 1) * self.indent_step
				} else if let Some(d) = self.after_dash_depth {
					// After `- `, continuation aligns with content start (column after `- `)
					d * self.indent_step + 2
				} else {
					// Fallback: indent one level deeper than current depth
					(self.depth + 1) * self.indent_step
				};

				// First line offset accounts for the key prefix on the first line.
				// For direct sequence items (right after `- `), use dash-based offset
				// even if current_map_depth is set (from parent map context).
				let first_line_offset = if self.at_direct_seq_item {
					// Direct sequence item: indent + `- ` (2)
					let dash_depth = self.after_dash_depth.unwrap_or(0);
					let indent = dash_depth * self.indent_step;
					indent + 2
				} else if let Some(map_depth) = self.current_map_depth {
					// Map value: indent + key + `: ` (2)
					let indent = map_depth * self.indent_step;
					// Use actual key length, or estimate 10 if not available
					let key_len = if self.last_key_len > 0 {
						self.last_key_len
					} else {
						10
					};
					indent + key_len + 2
				} else if let Some(dash_depth) = self.after_dash_depth {
					// Sequence item: indent + `- ` (2)
					let indent = dash_depth * self.indent_step;
					indent + 2
				} else {
					0
				};

				// Calculate content length, accounting for escape sequences in double-quoted strings.
				// Strings with characters needing escape (emoji, BOM, control chars, etc.) will
				// use double-quoted style where escape sequences expand the output length.
				// For example, emoji 🚨 (1 char) becomes \U0001F6A8 (10 chars).
				// This ensures wrapping decisions are based on actual output line length.
				let content_length = if has_chars_needing_escape(v) {
					// Will use double-quoted style with escaping
					escaped_double_quoted_length(v) + 2 // +2 for surrounding quotes
				} else {
					// Plain or single-quoted style, use char count + potential quotes
					v.chars().count() + 2 // +2 for surrounding quotes (conservative estimate)
				};

				// Go yaml.v2 wrapping behavior: wrap if total line exceeds line_width.
				// The total includes indent + key + content for both plain and quoted scalars.
				let total_line_length = first_line_offset + content_length;
				let should_wrap = total_line_length > max_width;

				if should_wrap && max_width > 0 {
					self.write_space_if_pending()?;
					// Use same priority as continuation_indent: current_map_depth > after_dash_depth > depth
					let base = if let Some(map_depth) = self.current_map_depth {
						map_depth
					} else if let Some(d) = self.after_dash_depth {
						d
					} else {
						self.depth
					};
					if self.at_line_start {
						self.write_indent(base)?;
					}

					// Check if the string needs quoting
					// For long strings that exceed line_width, Go yaml.v2 uses folded scalars
					// (plain multiline) even if they contain single quotes in the middle.
					// Only use quoted style if the string has other unsafe characteristics.
					let is_plain_safe = is_plain_block_value_safe(v);

					// Special case: strings that only need quoting because of single quotes
					// should use folded scalars when they exceed line_width, matching Go yaml.v3 behavior.
					// HOWEVER, if the string also contains `: ` (colon-space), it MUST be quoted
					// because colon-space is a mapping indicator in YAML.
					// This handles cases like: "Set to true for 'ACCEPT_AUTOMATIC' or false for 'ACCEPT_MANUAL'"
					// but NOT: "EmptyDir represents... a pod's lifetime. More info: https://..."
					let first_char = v.chars().next();
					let starts_with_letter = first_char.map_or(false, |c| c.is_ascii_alphabetic());
					let has_colon_space = v.contains(": ");
					// String can use folded scalar if:
					// - starts with a letter (not with a single quote or other special char)
					// - contains single quotes (the reason is_plain_block_value_safe returns false)
					// - does NOT contain `: ` (colon-space requires quoting, matches Go yaml.v3)
					// - no control chars, tabs, or newlines
					// - no trailing whitespace (trailing space requires quoting in YAML)
					let can_use_folded_with_quotes =
						v.contains('\'')
							&& !v.starts_with('\'')
							&& starts_with_letter && !has_colon_space
							&& !v.chars().any(|c| c.is_control() || c == '\t' || c == '\n')
							&& !v.ends_with(' ') && !v.ends_with('\t');
					let only_single_quotes = !is_plain_safe && can_use_folded_with_quotes;

					// Use folded scalar (plain multiline) for:
					// 1. Plain-safe strings
					// 2. Strings that only need quoting because of single quotes in the middle
					// This matches Go yaml.v2 behavior for long strings
					if is_plain_safe || only_single_quotes {
						self.write_plain_with_wrap(
							v,
							continuation_indent,
							max_width,
							first_line_offset,
						)?;
						self.write_end_of_scalar()?;
						return Ok(());
					} else {
						// String needs quoting for other reasons - use write_quoted_with_wrap
						self.write_quoted_with_wrap(
							v,
							continuation_indent,
							max_width,
							first_line_offset,
						)?;
						self.write_end_of_scalar()?;
						return Ok(());
					}
				}
			}
		}

		self.write_space_if_pending()?;
		self.write_scalar_prefix_if_anchor()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		// Special-case: prefer single-quoted style for select 1-char punctuation to
		// match expected YAML output and Go yaml.v3 behavior ('.', '#', '-', '*').
		if v.len() == 1 {
			if let Some(ch) = v.chars().next() {
				if ch == '.' || ch == '#' || ch == '-' || ch == '*' {
					self.out.write_char('\'')?;
					self.out.write_char(ch)?;
					self.out.write_char('\'')?;
					self.write_end_of_scalar()?;
					return Ok(());
				}
			}
		}
		self.write_plain_or_quoted_value(v)?;
		self.write_end_of_scalar()?;
		Ok(())
	}

	fn serialize_bytes(self, v: &[u8]) -> Result<()> {
		// Two behaviors are required by tests:
		// - Top-level &[u8] should serialize as a block sequence of integers.
		// - Fields using #[serde(with = "serde_bytes")] should serialize as a tagged !!binary
		//   base64 scalar inline after "key: ". The latter ends up calling serialize_bytes in
		//   value position (mid-line), whereas plain Vec<u8> without serde_bytes goes through
		//   serialize_seq instead. Distinguish by whether we are at the start of a line.
		if self.at_line_start {
			// Top-level or start-of-line: emit as sequence of numbers
			let mut seq = self.serialize_seq(Some(v.len()))?;
			for b in v {
				serde::ser::SerializeSeq::serialize_element(&mut seq, b)?;
			}
			return serde::ser::SerializeSeq::end(seq);
		}

		// Inline value position: emit !!binary with base64.
		self.write_space_if_pending()?;
		self.write_scalar_prefix_if_anchor()?;
		// No indent needed mid-line; mirror serialize_str behavior.
		self.out.write_str("!!binary ")?;
		let mut s = String::new();
		B64.encode_string(v, &mut s);
		self.out.write_str(&s)?;
		self.write_end_of_scalar()?;
		Ok(())
	}

	fn serialize_none(self) -> Result<()> {
		self.write_space_if_pending()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		self.out.write_str("null")?;
		self.write_end_of_scalar()?;
		Ok(())
	}

	fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<()> {
		value.serialize(self)
	}

	fn serialize_unit(self) -> Result<()> {
		self.write_space_if_pending()?;
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		self.out.write_str("null")?;
		self.write_end_of_scalar()?;
		Ok(())
	}

	fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
		self.serialize_unit()
	}

	fn serialize_unit_variant(
		self,
		name: &'static str,
		_variant_index: u32,
		variant: &'static str,
	) -> Result<()> {
		// If we are in a mapping value position, insert the deferred space after ':'
		self.write_space_if_pending()?;
		if self.tagged_enums {
			self.serialize_tagged_scalar(name, variant)
		} else {
			self.serialize_str(variant)
		}
	}

	fn serialize_newtype_struct<T: ?Sized + Serialize>(
		self,
		name: &'static str,
		value: &T,
	) -> Result<()> {
		// Flow hints & block-string hints:
		match name {
			NAME_FLOW_SEQ => {
				self.pending_flow = Some(PendingFlow::AnySeq);
				return value.serialize(self);
			}
			NAME_FLOW_MAP => {
				self.pending_flow = Some(PendingFlow::AnyMap);
				return value.serialize(self);
			}
			NAME_LIT_STR => {
				// Always use literal block style for LitStr/LitString wrappers.
				// Choose chomping based on trailing newlines during actual emission.
				// Capture the inner string first.
				let mut cap = StrCapture::default();
				value.serialize(&mut cap)?;
				let s = cap.finish()?;
				self.pending_str_style = Some(StrStyle::Literal);
				return self.serialize_str(&s);
			}
			NAME_FOLD_STR => {
				let mut cap = StrCapture::default();
				value.serialize(&mut cap)?;
				let s = cap.finish()?;
				let is_multiline = s.contains('\n');
				if !is_multiline && s.len() < self.min_fold_chars {
					return self.serialize_str(&s);
				}
				self.pending_str_style = Some(StrStyle::Folded);
				return self.serialize_str(&s);
			}
			_ => {}
		}
		// default: ignore the name, serialize the inner as-is
		value.serialize(self)
	}

	fn serialize_newtype_variant<T: ?Sized + Serialize>(
		self,
		_name: &'static str,
		_variant_index: u32,
		variant: &'static str,
		value: &T,
	) -> Result<()> {
		// If we are the value of a mapping key, YAML forbids "key: Variant: value" inline.
		// Emit the variant mapping on the next line indented one level. Also, do not insert
		// a space after the colon when the value may itself be a mapping; instead, defer
		// space insertion to the value serializer via pending_space_after_colon.
		if self.pending_space_after_colon {
			// consume the pending space request and start a new line
			self.pending_space_after_colon = false;
			self.newline()?;
			// When used as a mapping value, indent relative to the parent mapping's base,
			// not the serializer's current depth (which may still be the outer level).
			let base = self.current_map_depth.unwrap_or(self.depth);
			self.write_indent(base + 1)?;
			self.write_plain_or_quoted(variant)?;
			// Write ':' without trailing space, then mark that a space may be needed
			// if the following value is a scalar.
			self.out.write_str(":")?;
			self.pending_space_after_colon = true;
			self.at_line_start = false;
			// Ensure that if the value is another variant or a mapping/sequence,
			// it indents under this variant label rather than the parent map key.
			let prev_map_depth = self.current_map_depth.replace(base + 1);
			let res = value.serialize(&mut *self);
			self.current_map_depth = prev_map_depth;
			return res;
		}
		// Otherwise (top-level or sequence context).
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		self.write_plain_or_quoted(variant)?;
		// Write ':' without a space and defer spacing/newline to the value serializer.
		self.out.write_str(":")?;
		self.pending_space_after_colon = true;
		self.at_line_start = false;
		value.serialize(&mut *self)
	}

	// -------- Collections --------

	fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq> {
		// Clear at_direct_seq_item when entering a nested sequence - we're no longer at a direct scalar position
		self.at_direct_seq_item = false;
		let flow = self.take_flow_for_seq();
		if flow {
			self.write_scalar_prefix_if_anchor()?;
			// Ensure a space after a preceding colon when this sequence is a mapping value.
			self.write_space_if_pending()?;
			if self.at_line_start {
				self.write_indent(self.depth)?;
			}
			self.out.write_str("[")?;
			self.at_line_start = false;
			let depth_next = self.depth; // inline
			Ok(SeqSer {
				ser: self,
				depth: depth_next,
				flow: true,
				first: true,
				deferred_newline: false,
			})
		} else {
			// Block sequence. Decide indentation based on whether this is after a map key or after a list dash.
			let was_inline_value = !self.at_line_start;

			// For block sequences nested under another dash, keep the first inner dash inline.
			// Style expectations in tests prefer the compact form:
			// - - 1
			// instead of:
			// -
			//   - 1
			let inline_first = (!self.at_line_start)
				&& self.after_dash_depth.is_some()
				&& !self.pending_space_after_colon;
			// If we are a mapping value (space after colon was pending), we will handle
			// the newline later in SeqSer::serialize_element to keep empty sequences inline.
			self.write_anchor_for_complex_node()?;
			// Track if we need to defer the newline for potential empty sequences
			let mut deferred_newline = false;
			if inline_first {
				// Keep staged inline (pending_inline_map) so the child can inline its first dash.
				// Ensure we stay mid-line so the child can emit its first dash inline.
				self.at_line_start = false;
			} else if was_inline_value {
				// Mid-line start. If we are here due to a map value (after ':'), move to next line.
				// If we are here due to a list dash, keep inline.
				self.pending_space_after_colon = false;
				if !self.at_line_start {
					// If empty_array_as_brackets is enabled, defer the newline until we know
					// if the array has entries. Empty arrays should emit [] on the same line.
					if self.empty_array_as_brackets {
						deferred_newline = true;
					} else {
						self.newline()?;
					}
				}
			}
			// Indentation policy mirrors serialize_map:
			// - After a list dash inline_first: base is dash depth; indent one level deeper.
			// - As a value after a map key: base is current_map_depth (if set), indent one level deeper.
			// - Otherwise (top-level or already at line start): base is current depth.
			let base = if inline_first {
				self.after_dash_depth.unwrap_or(self.depth)
			} else if was_inline_value && self.current_map_depth.is_some() {
				// Guarded by is_some(); use unwrap_or to avoid panicking unwrap
				self.current_map_depth.unwrap_or(self.depth)
			} else {
				self.depth
			};
			// Determine the depth for this sequence:
			// - inline_first (nested sequence like `- - map:`): always increment depth by 1,
			//   so subsequent items of the inner sequence are properly indented.
			// - was_inline_value (array after map key like `key: [...]`): with indent_array=0,
			//   keep at same depth; otherwise increment.
			// - Top-level: use base depth.
			let depth_next = if inline_first {
				// Nested sequences always need one more level of indentation
				base + 1
			} else if was_inline_value {
				// Array values respect indent_array setting
				if self.indent_array == Some(0) {
					base
				} else {
					base + 1
				}
			} else {
				base
			};
			// Starting a complex (block) sequence: drop any staged inline comment.
			self.pending_inline_comment = None;
			Ok(SeqSer {
				ser: self,
				depth: depth_next,
				flow: false,
				first: true,
				deferred_newline,
			})
		}
	}

	fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple> {
		self.serialize_seq(Some(len))
	}

	fn serialize_tuple_struct(
		self,
		name: &'static str,
		_len: usize,
	) -> Result<Self::SerializeTupleStruct> {
		if name == NAME_TUPLE_ANCHOR {
			Ok(TupleSer::anchor_strong(self))
		} else if name == NAME_TUPLE_WEAK {
			Ok(TupleSer::anchor_weak(self))
		} else if name == NAME_TUPLE_COMMENTED {
			Ok(TupleSer::commented(self))
		} else {
			// Treat as normal block sequence
			Ok(TupleSer::normal(self))
		}
	}

	fn serialize_tuple_variant(
		self,
		_name: &'static str,
		_variant_index: u32,
		variant: &'static str,
		_len: usize,
	) -> Result<Self::SerializeTupleVariant> {
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		self.write_plain_or_quoted(variant)?;
		self.out.write_str(":\n")?;
		self.at_line_start = true;
		let depth_next = self.depth + 1;
		Ok(TupleVariantSer {
			ser: self,
			depth: depth_next,
		})
	}

	fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap> {
		// Clear at_direct_seq_item when entering a map - we're no longer at a direct scalar position
		self.at_direct_seq_item = false;
		let flow = self.take_flow_for_map();
		if flow {
			self.write_scalar_prefix_if_anchor()?;
			// Ensure a space after a preceding colon when this mapping is a value.
			self.write_space_if_pending()?;
			if self.at_line_start {
				self.write_indent(self.depth)?;
			}
			self.out.write_str("{")?;
			self.at_line_start = false;
			let depth_next = self.depth;
			Ok(MapSer {
				ser: self,
				depth: depth_next,
				flow: true,
				first: true,
				last_key_complex: false,
				deferred_newline: false,
			})
		} else {
			let inline_first = self.pending_inline_map;
			// We only consider "value position" when immediately after a mapping colon.
			let was_inline_value = self.pending_space_after_colon;
			self.write_anchor_for_complex_node()?;
			// Track if we need to defer the newline for potential empty maps
			let mut deferred_newline = false;
			if inline_first {
				// Suppress newline after a list dash for inline map first key.
				self.pending_inline_map = false;
				// Mark that this sequence element is a mapping printed inline after a dash.
				self.inline_map_after_dash = true;
			} else if was_inline_value {
				// Map used as a value after "key: ". If an anchor was emitted, we are already at
				// the start of a new line due to write_anchor_for_complex_node() -> newline().
				// Only add a newline if we are not already at line start (i.e., no anchor emitted).
				self.pending_space_after_colon = false;
				if !self.at_line_start {
					// If empty_map_as_braces is enabled, defer the newline until we know
					// if the map has entries. Empty maps should emit {} on the same line.
					if self.empty_map_as_braces {
						deferred_newline = true;
					} else {
						self.newline()?;
					}
				}
			}
			// Indentation rules:
			// - Top-level (at line start, not after dash): use current depth.
			// - After dash inline first key or as a value: indent one level deeper for subsequent lines.
			// Use the current mapping's depth as base only when we are in a VALUE position.
			// For complex KEYS (non-scalar), keep using the current serializer depth so that
			// subsequent key lines indent relative to the "? " line, not the parent map's base.
			let base = if inline_first {
				self.after_dash_depth.unwrap_or(self.depth)
			} else if was_inline_value && self.current_map_depth.is_some() {
				self.current_map_depth.unwrap_or(self.depth)
			} else {
				self.depth
			};
			let depth_next = if inline_first {
				// When a map is inline after a dash, subsequent fields should align with the first field.
				// For nested sequences like `- - map:`, the first field position is:
				//   array_step * outer_depth + 2 * inline_dash_count
				// where outer_depth is the outermost sequence's depth.
				// Since nested sequences increment depth by 1 for each level, we need to
				// subtract (inline_dash_count - 1) from base to get the outer depth.
				let array_step = match self.indent_array {
					Some(0) => self.indent_step, // When indent_array is 0, use indent_step for base
					Some(n) => n,
					None => self.indent_step,
				};
				// Account for all inline dashes (e.g., "- - - map:" has 3 dashes = 6 chars)
				let dash_chars = 2 * self.inline_dash_count;
				// Adjust base to get the outermost sequence's depth
				let outer_depth = base.saturating_sub(self.inline_dash_count.saturating_sub(1));
				let target_spaces = array_step * outer_depth + dash_chars;
				// Calculate depth and offset to achieve target_spaces
				let depth = target_spaces / self.indent_step;
				let offset = target_spaces % self.indent_step;
				// Clear any previous offset first
				self.indent_offset = 0;
				// Only set offset for immediate inline map fields after dash
				if offset > 0 {
					self.indent_offset = offset;
				}
				// Reset inline dash count after consuming it
				self.inline_dash_count = 0;
				depth
			} else if was_inline_value {
				base + 1
			} else {
				base
			};
			Ok(MapSer {
				ser: self,
				depth: depth_next,
				flow: false,
				first: true,
				last_key_complex: false,
				deferred_newline,
			})
		}
	}

	fn serialize_struct(self, name: &'static str, len: usize) -> Result<Self::SerializeStruct> {
		if name == RAW_SCALAR_TOKEN {
			if len != 1 {
				return Err(Error::unexpected(
					"raw scalar sentinel must contain one field",
				));
			}
			Ok(StructSer::RawScalar(RawScalarSer {
				ser: self,
				value: None,
			}))
		} else {
			self.serialize_map(Some(len)).map(StructSer::Map)
		}
	}

	fn serialize_struct_variant(
		self,
		_name: &'static str,
		_variant_index: u32,
		variant: &'static str,
		_len: usize,
	) -> Result<Self::SerializeStructVariant> {
		// If we are the value of a mapping key, YAML forbids keeping a nested mapping
		// on the same line (e.g., "key: Variant:"). Move the variant mapping to the next line
		// indented under the parent mapping's base depth.
		let _was_inline_value = !self.at_line_start;
		if self.pending_space_after_colon {
			// Value position after a map key: start the variant mapping on the next line.
			self.pending_space_after_colon = false;
			self.newline()?;
			// Indent the variant name one level under the parent mapping.
			let base = self.current_map_depth.unwrap_or(self.depth) + 1;
			self.write_indent(base)?;
			self.write_plain_or_quoted(variant)?;
			self.out.write_str(":\n")?;
			self.at_line_start = true;
			// Fields indent one more level under the variant label.
			let depth_next = base + 1;
			return Ok(StructVariantSer {
				ser: self,
				depth: depth_next,
			});
		}
		// Otherwise (top-level or sequence context), emit the variant name at current depth.
		if self.at_line_start {
			self.write_indent(self.depth)?;
		}
		self.write_plain_or_quoted(variant)?;
		self.out.write_str(":\n")?;
		self.at_line_start = true;
		// Default indentation for fields under a plain variant line.
		let mut depth_next = self.depth + 1;
		// If this variant follows a list dash, indent two levels under the dash (one for the element, one for the mapping).
		if let Some(d) = self.after_dash_depth.take() {
			depth_next = d + 2;
			self.pending_inline_map = false;
		}
		Ok(StructVariantSer {
			ser: self,
			depth: depth_next,
		})
	}
}

// ------------------------------------------------------------
// Seq / Tuple serializers
// ------------------------------------------------------------

/// Serializer for sequences and tuples.
///
/// Created by `YamlSer::serialize_seq`/`serialize_tuple`. Holds a mutable
/// reference to the parent serializer and formatting state for the sequence.
pub struct SeqSer<'a, 'b, W: Write> {
	/// Parent YAML serializer.
	ser: &'a mut YamlSer<'b, W>,
	/// Target indentation depth for block-style items.
	depth: usize,
	/// Whether the sequence is being written in flow style (`[a, b]`).
	flow: bool,
	/// Whether the next element is the first (comma handling in flow style).
	first: bool,
	/// Whether we deferred writing a newline for empty sequence detection (when empty_array_as_brackets is true).
	deferred_newline: bool,
}

impl<'a, 'b, W: Write> SerializeTuple for SeqSer<'a, 'b, W> {
	type Ok = ();
	type Error = Error;

	fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<()> {
		SerializeSeq::serialize_element(self, v)
	}
	fn end(self) -> Result<()> {
		SerializeSeq::end(self)
	}
}

// Re-implement SerializeSeq for SeqSer with correct end.
impl<'a, 'b, W: Write> SerializeSeq for SeqSer<'a, 'b, W> {
	type Ok = ();
	type Error = Error;

	fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<()> {
		// If we deferred a newline for empty sequence detection, write it now since the sequence isn't empty
		if self.deferred_newline {
			self.ser.newline()?;
			self.deferred_newline = false;
		}
		if self.flow {
			if !self.first {
				self.ser.out.write_str(", ")?;
			}
			self.ser.with_in_flow(|s| v.serialize(s))?;
		} else {
			// If we are the value of a mapping key, we deferred the newline until we knew the
			// sequence is non-empty. Insert it now before emitting the first dash.
			if self.first && self.ser.pending_space_after_colon {
				self.ser.pending_space_after_colon = false;
				if !self.ser.at_line_start {
					self.ser.newline()?;
				}
			}
			// If previous element was an inline map after a dash, just clear the flag; do not change depth.
			if !self.first && self.ser.inline_map_after_dash {
				self.ser.inline_map_after_dash = false;
			}
			if self.first && (!self.ser.at_line_start || self.ser.pending_inline_map) {
				// Inline the first element of this nested sequence right after the outer dash
				// (either we are already mid-line, or the parent staged inline via pending_inline_map).
				// Do not write indentation here.
				// Increment inline dash count to track consecutive inline dashes (e.g., "- - - map:").
				self.ser.inline_dash_count += 1;
			} else {
				self.ser.write_indent_seq(self.depth)?;
				// Reset inline dash count - this dash starts a new line.
				self.ser.inline_dash_count = 1;
			}
			self.ser.out.write_str("- ")?;
			self.ser.at_line_start = false;
			if self.first && self.ser.inline_map_after_dash {
				// We consumed the inline-after-dash behavior for this child sequence.
				self.ser.inline_map_after_dash = false;
			}
			// Capture the dash's indentation depth for potential struct-variant that follows.
			self.ser.after_dash_depth = Some(self.depth);
			// Hint to emit first key/element of a following mapping/sequence inline on the same line.
			self.ser.pending_inline_map = true;
			// Mark that we're at a direct sequence item position (right after "- ")
			// This is used for line wrapping first_line_offset calculation
			self.ser.at_direct_seq_item = true;
			v.serialize(&mut *self.ser)?;
			self.ser.at_direct_seq_item = false;
		}
		self.first = false;
		Ok(())
	}

	fn end(self) -> Result<()> {
		if self.flow {
			let me = self;
			me.ser.out.write_str("]")?;
			if me.ser.in_flow == 0 {
				me.ser.newline()?;
			}
		} else if self.first {
			// Empty block-style sequence: emit [] when option is enabled, otherwise just newline
			if self.ser.empty_array_as_brackets {
				// If we deferred a newline (value position after "key:"), we're still on the same line
				// So we can emit " []" directly after the colon
				if self.deferred_newline {
					// Write space after the pending colon, then []
					self.ser.out.write_str(" []")?;
					self.ser.newline()?;
				} else {
					// Top-level or other position: emit [] on its own line
					self.ser.write_space_if_pending()?;
					self.ser.out.write_str("[]")?;
					self.ser.newline()?;
				}
			} else {
				// Original behavior: empty sequence ends with a newline
				self.ser.newline()?;
			}
		}
		Ok(())
	}
}

// Tuple-struct serializer (normal or anchor payload)
/// Serializer for tuple-structs.
///
/// Used for three shapes:
/// - Normal tuple-structs (treated like sequences in block style),
/// - Internal strong-anchor payloads (`__yaml_anchor`),
/// - Internal weak-anchor payloads (`__yaml_weak_anchor`).
pub struct TupleSer<'a, 'b, W: Write> {
	/// Parent YAML serializer.
	ser: &'a mut YamlSer<'b, W>,
	/// Variant describing how to interpret fields.
	kind: TupleKind,
	/// Current field index being serialized.
	idx: usize,
	/// For normal tuples: target indentation depth.
	/// For weak/strong: temporary storage (ptr id or state).
	depth_for_normal: usize,

	// ---- Extra fields for refactoring/perf/correctness ----
	/// For strong anchors: if Some(id) then we must emit an alias instead of a definition at field #2.
	strong_alias_id: Option<AnchorId>,
	/// For weak anchors: whether the `present` flag was true.
	weak_present: bool,
	/// Skip serializing the 3rd field (value) in weak case if present==false.
	skip_third: bool,
	/// For weak anchors: hold alias id if value should be emitted as alias in field #3.
	weak_alias_id: Option<AnchorId>,
	/// For commented wrapper: captured comment text from field #0.
	comment_text: Option<String>,
}
enum TupleKind {
	Normal,       // treat as block seq
	AnchorStrong, // [ptr, value]
	AnchorWeak,   // [ptr, present, value]
	Commented,    // [comment, value]
}
impl<'a, 'b, W: Write> TupleSer<'a, 'b, W> {
	/// Create a tuple serializer for normal tuple-structs.
	fn normal(ser: &'a mut YamlSer<'b, W>) -> Self {
		let depth_next = ser.depth + 1;
		Self {
			ser,
			kind: TupleKind::Normal,
			idx: 0,
			depth_for_normal: depth_next,
			strong_alias_id: None,
			weak_present: false,
			skip_third: false,
			weak_alias_id: None,
			comment_text: None,
		}
	}
	/// Create a tuple serializer for internal strong-anchor payloads.
	fn anchor_strong(ser: &'a mut YamlSer<'b, W>) -> Self {
		Self {
			ser,
			kind: TupleKind::AnchorStrong,
			idx: 0,
			depth_for_normal: 0,
			strong_alias_id: None,
			weak_present: false,
			skip_third: false,
			weak_alias_id: None,
			comment_text: None,
		}
	}
	/// Create a tuple serializer for internal weak-anchor payloads.
	fn anchor_weak(ser: &'a mut YamlSer<'b, W>) -> Self {
		Self {
			ser,
			kind: TupleKind::AnchorWeak,
			idx: 0,
			depth_for_normal: 0,
			strong_alias_id: None,
			weak_present: false,
			skip_third: false,
			weak_alias_id: None,
			comment_text: None,
		}
	}
	/// Create a tuple serializer for internal commented wrapper.
	fn commented(ser: &'a mut YamlSer<'b, W>) -> Self {
		Self {
			ser,
			kind: TupleKind::Commented,
			idx: 0,
			depth_for_normal: 0,
			strong_alias_id: None,
			weak_present: false,
			skip_third: false,
			weak_alias_id: None,
			comment_text: None,
		}
	}
}

impl<'a, 'b, W: Write> SerializeTupleStruct for TupleSer<'a, 'b, W> {
	type Ok = ();
	type Error = Error;

	fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
		match self.kind {
			TupleKind::Normal => {
				if self.idx == 0 {
					self.ser.write_anchor_for_complex_node()?;
					if !self.ser.at_line_start {
						self.ser.newline()?;
					}
				}
				self.ser.write_indent_seq(self.ser.depth + 1)?;
				self.ser.out.write_str("- ")?;
				self.ser.at_line_start = false;
				value.serialize(&mut *self.ser)?;
			}
			TupleKind::AnchorStrong => {
				match self.idx {
					0 => {
						// capture ptr, decide define vs alias
						let mut cap = UsizeCapture::default();
						value.serialize(&mut cap)?;
						let ptr = cap.finish()?;
						let (id, fresh) = self.ser.alloc_anchor_for(ptr);
						if fresh {
							self.ser.pending_anchor_id = Some(id); // define before value
							self.strong_alias_id = None;
						} else {
							self.strong_alias_id = Some(id); // alias instead of value
						}
					}
					1 => {
						if let Some(id) = self.strong_alias_id.take() {
							// Already defined earlier -> emit alias
							self.ser.write_alias_id(id)?;
						} else {
							// First sight -> serialize value; pending_anchor_id (if any) will be emitted
							value.serialize(&mut *self.ser)?;
						}
					}
					_ => return Err(Error::unexpected("unexpected field in __yaml_anchor")),
				}
			}
			TupleKind::AnchorWeak => {
				match self.idx {
					0 => {
						let mut cap = UsizeCapture::default();
						value.serialize(&mut cap)?;
						let ptr = cap.finish()?;
						self.depth_for_normal = ptr; // store ptr for fields #2/#3
					}
					1 => {
						let mut bc = BoolCapture::default();
						value.serialize(&mut bc)?;
						self.weak_present = bc.finish()?;
						if !self.weak_present {
							// present == false: emit null and skip field #3
							if self.ser.at_line_start {
								self.ser.write_indent(self.ser.depth)?;
							}
							self.ser.out.write_str("null")?;
							// Use shared end-of-scalar so pending inline comments (if any) are appended
							self.ser.write_end_of_scalar()?;
							self.skip_third = true;
						} else {
							let ptr = self.depth_for_normal;
							let (id, fresh) = self.ser.alloc_anchor_for(ptr);
							if fresh {
								self.ser.pending_anchor_id = Some(id); // define before value
								self.weak_alias_id = None;
							} else {
								self.weak_alias_id = Some(id); // alias in field #3
							}
						}
					}
					2 => {
						if self.skip_third {
							// nothing to do
						} else if let Some(id) = self.weak_alias_id.take() {
							self.ser.write_alias_id(id)?;
						} else {
							// definition path: pending_anchor_id (if any) will be placed automatically
							value.serialize(&mut *self.ser)?;
						}
					}
					_ => return Err(Error::unexpected("unexpected field in __yaml_weak_anchor")),
				}
			}
			TupleKind::Commented => {
				match self.idx {
					0 => {
						// Capture comment string
						let mut sc = StrCapture::default();
						value.serialize(&mut sc)?;
						self.comment_text = Some(sc.finish()?);
					}
					1 => {
						let comment = self.comment_text.take().unwrap_or_default();
						if self.ser.in_flow == 0 {
							// Stage the comment so scalar/alias serializers append it inline via write_end_of_scalar.
							if !comment.is_empty() {
								let sanitized = comment.replace('\n', " ");
								self.ser.pending_inline_comment = Some(sanitized);
							}
							// Serialize the inner value as-is. Complex values will ignore the comment (it will be cleared).
							value.serialize(&mut *self.ser)?;
							// Ensure no leftover staged comment leaks to subsequent tokens.
							self.ser.pending_inline_comment = None;
						} else {
							// Inside a flow context: serialize value and suppress comments.
							value.serialize(&mut *self.ser)?;
						}
					}
					_ => return Err(Error::unexpected("unexpected field in __yaml_commented")),
				}
			}
		}
		self.idx += 1;
		Ok(())
	}

	fn end(self) -> Result<()> {
		Ok(())
	}
}

// Tuple variant (enum Variant: ( ... ))
/// Serializer for tuple variants (enum Variant: ( ... )).
///
/// Created by `YamlSer::serialize_tuple_variant` to emit the variant name
/// followed by a block sequence of fields.
pub struct TupleVariantSer<'a, 'b, W: Write> {
	/// Parent YAML serializer.
	ser: &'a mut YamlSer<'b, W>,
	/// Target indentation depth for the fields.
	depth: usize,
}
impl<'a, 'b, W: Write> SerializeTupleVariant for TupleVariantSer<'a, 'b, W> {
	type Ok = ();
	type Error = Error;

	fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
		self.ser.write_indent_seq(self.depth)?;
		self.ser.out.write_str("- ")?;
		self.ser.at_line_start = false;
		value.serialize(&mut *self.ser)
	}
	fn end(self) -> Result<()> {
		Ok(())
	}
}

// ------------------------------------------------------------
// Map / Struct serializers
// ------------------------------------------------------------

/// Dispatcher for ordinary structs and the [`RawScalar`] sentinel struct.
pub enum StructSer<'a, 'b, W: Write> {
	Map(MapSer<'a, 'b, W>),
	RawScalar(RawScalarSer<'a, 'b, W>),
}

impl<'a, 'b, W: Write> SerializeStruct for StructSer<'a, 'b, W> {
	type Ok = ();
	type Error = Error;

	fn serialize_field<T: ?Sized + Serialize>(
		&mut self,
		key: &'static str,
		value: &T,
	) -> Result<()> {
		match self {
			StructSer::Map(map) => SerializeStruct::serialize_field(map, key, value),
			StructSer::RawScalar(raw) => raw.serialize_field(key, value),
		}
	}

	fn end(self) -> Result<()> {
		match self {
			StructSer::Map(map) => SerializeStruct::end(map),
			StructSer::RawScalar(raw) => raw.end(),
		}
	}
}

pub struct RawScalarSer<'a, 'b, W: Write> {
	ser: &'a mut YamlSer<'b, W>,
	value: Option<String>,
}

impl<'a, 'b, W: Write> RawScalarSer<'a, 'b, W> {
	fn serialize_field<T: ?Sized + Serialize>(
		&mut self,
		key: &'static str,
		value: &T,
	) -> Result<()> {
		if key != RAW_SCALAR_TOKEN {
			return Err(Error::unexpected("invalid raw scalar sentinel field"));
		}
		if self.value.is_some() {
			return Err(Error::unexpected("duplicate raw scalar sentinel field"));
		}

		let mut capture = StrCapture::default();
		value.serialize(&mut capture)?;
		let value = capture.finish()?;
		validate_raw_plain_scalar(&value).map_err(Error::from)?;
		self.value = Some(value);
		Ok(())
	}

	fn end(self) -> Result<()> {
		let value = self
			.value
			.ok_or_else(|| Error::unexpected("missing raw scalar sentinel field"))?;
		self.ser.write_raw_scalar(&value)
	}
}

/// Serializer for maps and structs.
///
/// Created by `YamlSer::serialize_map`/`serialize_struct`. Manages indentation
/// and flow/block style for key-value pairs.
pub struct MapSer<'a, 'b, W: Write> {
	/// Parent YAML serializer.
	ser: &'a mut YamlSer<'b, W>,
	/// Target indentation depth for block-style entries.
	depth: usize,
	/// Whether the mapping is in flow style (`{k: v}`).
	flow: bool,
	/// Whether the next entry is the first (comma handling in flow style).
	first: bool,
	/// Whether the most recently serialized key was a complex (non-scalar) node.
	last_key_complex: bool,
	/// Whether we deferred writing a newline for empty map detection (when empty_map_as_braces is true).
	deferred_newline: bool,
}

impl<'a, 'b, W: Write> SerializeMap for MapSer<'a, 'b, W> {
	type Ok = ();
	type Error = Error;

	fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<()> {
		// If we deferred a newline for empty map detection, write it now since the map isn't empty
		if self.deferred_newline {
			self.ser.newline()?;
			self.deferred_newline = false;
		}
		if self.flow {
			if !self.first {
				self.ser.out.write_str(", ")?;
			}
			let text = scalar_key_to_string(key)?;
			// Quote numeric or ambiguous keys in flow style too
			let should_quote = (self.ser.quote_numeric_strings && looks_like_number(&text))
				|| (self.ser.quote_ambiguous_keys && looks_like_ambiguous_boolean(&text));
			if should_quote {
				self.ser.out.write_str("\"")?;
				self.ser.out.write_str(&text)?;
				self.ser.out.write_str("\"")?;
			} else {
				self.ser.out.write_str(&text)?;
			}
			self.ser.out.write_str(": ")?;
			self.ser.at_line_start = false;
			self.last_key_complex = false;
		} else {
			// If we deferred a newline for empty map detection, write it now since the map isn't empty
			if self.deferred_newline {
				self.ser.newline()?;
				self.deferred_newline = false;
			}
			match scalar_key_to_string(key) {
				Ok(text) => {
					if !self.ser.at_line_start {
						self.ser.write_space_if_pending()?;
					}
					self.ser.write_indent(self.depth)?;
					// Quote numeric string keys if option is enabled
					// Quote ambiguous keys (y, n, yes, no, on, off, true, false) if option is enabled
					let should_quote = (self.ser.quote_numeric_strings && looks_like_number(&text))
						|| (self.ser.quote_ambiguous_keys && looks_like_ambiguous_boolean(&text));
					if should_quote {
						self.ser.out.write_str("\"")?;
						self.ser.out.write_str(&text)?;
						self.ser.out.write_str("\"")?;
					} else {
						self.ser.out.write_str(&text)?;
					}
					// Store key length for accurate line width calculation in values
					self.ser.last_key_len = text.len();
					// Defer the decision to put a space vs. newline until we see the value type.
					self.ser.out.write_str(":")?;
					self.ser.pending_space_after_colon = true;
					self.ser.at_line_start = false;
					self.last_key_complex = false;
				}
				Err(Error::Unexpected { msg }) if msg == "non-scalar key" => {
					if !self.ser.at_line_start {
						self.ser.write_space_if_pending()?;
					}
					self.ser.write_anchor_for_complex_node()?;
					self.ser.write_indent(self.depth)?;
					self.ser.out.write_str("? ")?;
					self.ser.at_line_start = false;

					let saved_depth = self.ser.depth;
					let saved_current_map_depth = self.ser.current_map_depth;
					let saved_pending_inline_map = self.ser.pending_inline_map;
					let saved_inline_map_after_dash = self.ser.inline_map_after_dash;
					let saved_after_dash_depth = self.ser.after_dash_depth;

					self.ser.pending_inline_map = true;
					self.ser.depth = self.depth;
					// Provide a base depth for nested maps within this complex key so that
					// continuation lines indent one level deeper than the parent mapping.
					self.ser.current_map_depth = Some(self.depth);
					self.ser.after_dash_depth = None;
					key.serialize(&mut *self.ser)?;

					self.ser.depth = saved_depth;
					self.ser.current_map_depth = saved_current_map_depth;
					self.ser.pending_inline_map = saved_pending_inline_map;
					self.ser.inline_map_after_dash = saved_inline_map_after_dash;
					self.ser.after_dash_depth = saved_after_dash_depth;
					self.last_key_complex = true;
				}
				Err(e) => return Err(e),
			}
		}
		Ok(())
	}

	fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
		if self.flow {
			self.ser.with_in_flow(|s| value.serialize(s))?;
		} else {
			let saved_pending_inline_map = self.ser.pending_inline_map;
			let saved_depth = self.ser.depth;
			if self.last_key_complex {
				self.ser.write_indent(self.depth)?;
				self.ser.out.write_str(":")?;
				self.ser.pending_space_after_colon = true;
				self.ser.pending_inline_map = true;
				self.ser.at_line_start = false;
				self.ser.depth = self.depth;
			}
			let prev_map_depth = self.ser.current_map_depth.replace(self.depth);
			// Save and clear indent_offset for nested structures
			let prev_offset = self.ser.indent_offset;
			self.ser.indent_offset = 0;
			let result = value.serialize(&mut *self.ser);
			self.ser.current_map_depth = prev_map_depth;
			// Restore offset only if we're still at the same map level
			self.ser.indent_offset = prev_offset;
			// Always restore the parent's pending_inline_map to avoid leaking inline hints
			// across sibling values (e.g., after finishing a sequence value like `groups`).
			self.ser.pending_inline_map = saved_pending_inline_map;
			if self.last_key_complex {
				self.ser.depth = saved_depth;
				self.last_key_complex = false;
			}
			result?;
		}
		self.first = false;
		Ok(())
	}

	fn end(self) -> Result<()> {
		if self.flow {
			self.ser.out.write_str("}")?;
			if self.ser.in_flow == 0 {
				self.ser.newline()?;
			}
		} else if self.first {
			// Empty block map: emit {} when option is enabled, otherwise just newline
			if self.ser.empty_map_as_braces {
				// If we deferred a newline (value position after "key:"), we're still on the same line
				// So we can emit " {}" directly after the colon
				if self.deferred_newline {
					// Write space after the pending colon, then {}
					self.ser.out.write_str(" {}")?;
					self.ser.pending_space_after_colon = false;
					self.ser.newline()?;
				} else {
					// Top-level or other position: emit {} on its own line
					self.ser.write_space_if_pending()?;
					self.ser.out.write_str("{}")?;
					self.ser.newline()?;
				}
			} else {
				// Original behavior: empty map ends with a newline
				self.ser.newline()?;
			}
		}
		// Clear indent offset when exiting map
		self.ser.indent_offset = 0;
		Ok(())
	}
}
impl<'a, 'b, W: Write> SerializeStruct for MapSer<'a, 'b, W> {
	type Ok = ();
	type Error = Error;

	fn serialize_field<T: ?Sized + Serialize>(
		&mut self,
		key: &'static str,
		value: &T,
	) -> Result<()> {
		SerializeMap::serialize_key(self, &key)?;
		SerializeMap::serialize_value(self, value)
	}
	fn end(self) -> Result<()> {
		SerializeMap::end(self)
	}
}

/// Serializer for struct variants (enum Variant: { ... }).
///
/// Created by `YamlSer::serialize_struct_variant` to emit the variant name
/// followed by a block mapping of fields.
pub struct StructVariantSer<'a, 'b, W: Write> {
	/// Parent YAML serializer.
	ser: &'a mut YamlSer<'b, W>,
	/// Target indentation depth for the fields.
	depth: usize,
}
impl<'a, 'b, W: Write> SerializeStructVariant for StructVariantSer<'a, 'b, W> {
	type Ok = ();
	type Error = Error;

	fn serialize_field<T: ?Sized + Serialize>(
		&mut self,
		key: &'static str,
		value: &T,
	) -> Result<()> {
		let text = scalar_key_to_string(&key)?;
		self.ser.write_indent(self.depth)?;
		self.ser.out.write_str(&text)?;
		// Defer spacing/newline decision to the value serializer similarly to map entries.
		self.ser.out.write_str(":")?;
		self.ser.pending_space_after_colon = true;
		self.ser.at_line_start = false;
		// Ensure nested mappings/collections used as this field's value indent relative to this struct variant.
		let prev_map_depth = self.ser.current_map_depth.replace(self.depth);
		let result = value.serialize(&mut *self.ser);
		self.ser.current_map_depth = prev_map_depth;
		result
	}
	fn end(self) -> Result<()> {
		Ok(())
	}
}

// ------------------------------------------------------------
// Helpers used for extracting ptr/bool inside tuple payloads
// ------------------------------------------------------------

/// Minimal serializer that captures a numeric `usize` from a serialized field.
///
/// Used internally to read the raw pointer value encoded as the first field
/// of our internal anchor tuple payloads.
#[derive(Default)]
struct UsizeCapture {
	v: Option<usize>,
}
impl<'a> Serializer for &'a mut UsizeCapture {
	type Ok = ();
	type Error = Error;

	type SerializeSeq = ser::Impossible<(), Error>;
	type SerializeTuple = ser::Impossible<(), Error>;
	type SerializeTupleStruct = ser::Impossible<(), Error>;
	type SerializeTupleVariant = ser::Impossible<(), Error>;
	type SerializeMap = ser::Impossible<(), Error>;
	type SerializeStruct = ser::Impossible<(), Error>;
	type SerializeStructVariant = ser::Impossible<(), Error>;

	fn serialize_i8(self, v: i8) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_i16(self, v: i16) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_i32(self, v: i32) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_i64(self, v: i64) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_u8(self, v: u8) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_u16(self, v: u16) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_u32(self, v: u32) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_u64(self, v: u64) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_f32(self, v: f32) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_f64(self, v: f64) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_bool(self, v: bool) -> Result<()> {
		self.v = Some(v as usize);
		Ok(())
	}
	fn serialize_char(self, _v: char) -> Result<()> {
		Err(Error::unexpected("ptr expects number"))
	}
	fn serialize_str(self, _v: &str) -> Result<()> {
		Err(Error::unexpected("ptr expects number"))
	}
	fn serialize_bytes(self, _v: &[u8]) -> Result<()> {
		Err(Error::unexpected("ptr expects number"))
	}
	fn serialize_none(self) -> Result<()> {
		Err(Error::unexpected("ptr cannot be none"))
	}
	fn serialize_some<T: ?Sized + Serialize>(self, _value: &T) -> Result<()> {
		Err(Error::unexpected("ptr not option"))
	}
	fn serialize_unit(self) -> Result<()> {
		Err(Error::unexpected("ptr cannot be unit"))
	}
	fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
		unexpected_e()
	}
	fn serialize_unit_variant(self, _name: &'static str, _i: u32, _v: &'static str) -> Result<()> {
		unexpected_e()
	}
	fn serialize_newtype_struct<T: ?Sized + Serialize>(
		self,
		_name: &'static str,
		_value: &T,
	) -> Result<()> {
		unexpected_e()
	}
	fn serialize_newtype_variant<T: ?Sized + Serialize>(
		self,
		_name: &'static str,
		_i: u32,
		_v: &'static str,
		_value: &T,
	) -> Result<()> {
		unexpected_e()
	}
	fn serialize_seq(self, _len: Option<usize>) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_tuple(self, _len: usize) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_tuple_struct(
		self,
		_name: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_tuple_variant(
		self,
		_name: &'static str,
		_i: u32,
		_v: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_map(self, _len: Option<usize>) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_struct(
		self,
		_name: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_struct_variant(
		self,
		_name: &'static str,
		_i: u32,
		_v: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn collect_str<T: ?Sized + fmt::Display>(self, _value: &T) -> Result<()> {
		unexpected_e()
	}
	fn is_human_readable(&self) -> bool {
		true
	}
}
impl UsizeCapture {
	fn finish(self) -> Result<usize> {
		self.v
			.ok_or_else(|| Error::unexpected("missing numeric ptr"))
	}
}

/// Minimal serializer that captures a boolean from a serialized field.
///
/// Used internally to read the `present` flag from weak-anchor payloads.
#[derive(Default)]
struct BoolCapture {
	v: Option<bool>,
}
impl<'a> Serializer for &'a mut BoolCapture {
	type Ok = ();
	type Error = Error;

	type SerializeSeq = ser::Impossible<(), Error>;
	type SerializeTuple = ser::Impossible<(), Error>;
	type SerializeTupleStruct = ser::Impossible<(), Error>;
	type SerializeTupleVariant = ser::Impossible<(), Error>;
	type SerializeMap = ser::Impossible<(), Error>;
	type SerializeStruct = ser::Impossible<(), Error>;
	type SerializeStructVariant = ser::Impossible<(), Error>;

	fn serialize_bool(self, v: bool) -> Result<()> {
		self.v = Some(v);
		Ok(())
	}
	fn serialize_i8(self, _v: i8) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_i16(self, _v: i16) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_i32(self, _v: i32) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_i64(self, _v: i64) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_u8(self, _v: u8) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_u16(self, _v: u16) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_u32(self, _v: u32) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_u64(self, _v: u64) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_f32(self, _v: f32) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_f64(self, _v: f64) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_char(self, _c: char) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_str(self, _v: &str) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_bytes(self, _v: &[u8]) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_none(self) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_some<T: ?Sized + Serialize>(self, _v: &T) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_unit(self) -> Result<()> {
		Err(Error::unexpected("bool expected"))
	}
	fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
		unexpected_e()
	}
	fn serialize_unit_variant(self, _name: &'static str, _i: u32, _v: &'static str) -> Result<()> {
		unexpected_e()
	}
	fn serialize_newtype_struct<T: ?Sized + Serialize>(
		self,
		_name: &'static str,
		_value: &T,
	) -> Result<()> {
		unexpected_e()
	}
	fn serialize_newtype_variant<T: ?Sized + Serialize>(
		self,
		_name: &'static str,
		_i: u32,
		_v: &'static str,
		_value: &T,
	) -> Result<()> {
		unexpected_e()
	}
	fn serialize_seq(self, _len: Option<usize>) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_tuple(self, _len: usize) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_tuple_struct(
		self,
		_name: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_tuple_variant(
		self,
		_name: &'static str,
		_i: u32,
		_v: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_map(self, _len: Option<usize>) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_struct(
		self,
		_name: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_struct_variant(
		self,
		_name: &'static str,
		_i: u32,
		_v: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn collect_str<T: ?Sized + fmt::Display>(self, _value: &T) -> Result<()> {
		unexpected_e()
	}
	fn is_human_readable(&self) -> bool {
		true
	}
}
impl BoolCapture {
	fn finish(self) -> Result<bool> {
		self.v.ok_or_else(|| Error::unexpected("missing bool"))
	}
}

/// Minimal serializer that captures a string from a serialized field.
///
/// Used internally to read the comment text for the Commented wrapper.
#[derive(Default)]
struct StrCapture {
	s: Option<String>,
}
impl<'a> Serializer for &'a mut StrCapture {
	type Ok = ();
	type Error = Error;

	type SerializeSeq = ser::Impossible<(), Error>;
	type SerializeTuple = ser::Impossible<(), Error>;
	type SerializeTupleStruct = ser::Impossible<(), Error>;
	type SerializeTupleVariant = ser::Impossible<(), Error>;
	type SerializeMap = ser::Impossible<(), Error>;
	type SerializeStruct = ser::Impossible<(), Error>;
	type SerializeStructVariant = ser::Impossible<(), Error>;

	fn serialize_str(self, v: &str) -> Result<()> {
		self.s = Some(v.to_string());
		Ok(())
	}

	fn serialize_bool(self, _v: bool) -> Result<()> {
		unexpected_e()
	}
	fn serialize_i8(self, _v: i8) -> Result<()> {
		unexpected_e()
	}
	fn serialize_i16(self, _v: i16) -> Result<()> {
		unexpected_e()
	}
	fn serialize_i32(self, _v: i32) -> Result<()> {
		unexpected_e()
	}
	fn serialize_i64(self, _v: i64) -> Result<()> {
		unexpected_e()
	}
	fn serialize_i128(self, _v: i128) -> Result<()> {
		unexpected_e()
	}
	fn serialize_u8(self, _v: u8) -> Result<()> {
		unexpected_e()
	}
	fn serialize_u16(self, _v: u16) -> Result<()> {
		unexpected_e()
	}
	fn serialize_u32(self, _v: u32) -> Result<()> {
		unexpected_e()
	}
	fn serialize_u64(self, _v: u64) -> Result<()> {
		unexpected_e()
	}
	fn serialize_u128(self, _v: u128) -> Result<()> {
		unexpected_e()
	}
	fn serialize_f32(self, _v: f32) -> Result<()> {
		unexpected_e()
	}
	fn serialize_f64(self, _v: f64) -> Result<()> {
		unexpected_e()
	}
	fn serialize_char(self, _c: char) -> Result<()> {
		unexpected_e()
	}
	fn serialize_bytes(self, _v: &[u8]) -> Result<()> {
		unexpected_e()
	}
	fn serialize_none(self) -> Result<()> {
		unexpected_e()
	}
	fn serialize_some<T: ?Sized + Serialize>(self, _value: &T) -> Result<()> {
		unexpected_e()
	}
	fn serialize_unit(self) -> Result<()> {
		unexpected_e()
	}
	fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
		unexpected_e()
	}
	fn serialize_unit_variant(self, _name: &'static str, _i: u32, _v: &'static str) -> Result<()> {
		unexpected_e()
	}
	fn serialize_newtype_struct<T: ?Sized + Serialize>(
		self,
		_name: &'static str,
		_value: &T,
	) -> Result<()> {
		unexpected_e()
	}
	fn serialize_newtype_variant<T: ?Sized + Serialize>(
		self,
		_name: &'static str,
		_i: u32,
		_v: &'static str,
		_value: &T,
	) -> Result<()> {
		unexpected_e()
	}
	fn serialize_seq(self, _len: Option<usize>) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_tuple(self, _len: usize) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_tuple_struct(
		self,
		_name: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_tuple_variant(
		self,
		_name: &'static str,
		_i: u32,
		_v: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_map(self, _len: Option<usize>) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_struct(
		self,
		_name: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn serialize_struct_variant(
		self,
		_name: &'static str,
		_i: u32,
		_v: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		unexpected()
	}
	fn collect_str<T: ?Sized + fmt::Display>(self, _value: &T) -> Result<()> {
		unexpected_e()
	}
	fn is_human_readable(&self) -> bool {
		true
	}
}
impl StrCapture {
	fn finish(self) -> Result<String> {
		self.s.ok_or_else(|| Error::unexpected("missing string"))
	}
}

// ------------------------------------------------------------
// String helpers
// ------------------------------------------------------------

fn validate_raw_plain_scalar(value: &str) -> std::result::Result<(), &'static str> {
	if value.is_empty() {
		return Err("raw scalar must not be empty");
	}
	if value.trim() != value {
		return Err("raw scalar must not have leading or trailing whitespace");
	}
	if value == "---" || value == "..." {
		return Err("raw scalar must not be a YAML document marker");
	}
	if value.chars().any(|character| {
		character.is_control()
			|| matches!(
				character,
				'\u{85}' | '\u{2028}' | '\u{2029}' | '[' | ']' | '{' | '}' | ','
			)
	}) {
		return Err("raw scalar contains characters unsafe in a flow scalar");
	}
	if value.contains(": ") || value.contains(" #") || value.ends_with(':') {
		return Err("raw scalar contains YAML mapping or comment syntax");
	}

	let first = value.as_bytes()[0];
	if matches!(
		first,
		b'#' | b'&' | b'*' | b'!' | b'|' | b'>' | b'\'' | b'"' | b'%' | b'@' | b'`'
	) {
		return Err("raw scalar begins with a YAML indicator");
	}
	if matches!(first, b'-' | b'?' | b':')
		&& value.as_bytes().get(1).is_none_or(u8::is_ascii_whitespace)
	{
		return Err("raw scalar begins with a reserved YAML indicator");
	}

	Ok(())
}

/// Check if any line in the string has trailing whitespace (space or tab).
/// Go's yaml.v3 falls back to double-quoted strings for values with trailing
/// whitespace on lines, so we should do the same for compatibility.
fn has_trailing_whitespace(s: &str) -> bool {
	s.lines()
		.any(|line| line.ends_with(' ') || line.ends_with('\t'))
}

/// Check if a string contains characters that require escaping in YAML.
/// These characters cannot be represented in literal block style and require
/// double-quoted style. This includes BOM (U+FEFF), control characters,
/// Unicode line/paragraph separators, and non-BMP characters.
/// Used to determine when to fall back from block scalar to double-quoted style.
fn has_chars_needing_escape(s: &str) -> bool {
	s.chars().any(|c| {
		// BOM (Byte Order Mark) requires \uFEFF escape
		c == '\u{FEFF}'
        // NEL (Next Line) requires \N escape
        || c == '\u{0085}'
        // Line Separator requires \L escape
        || c == '\u{2028}'
        // Paragraph Separator requires \P escape
        || c == '\u{2029}'
        // Non-BMP characters (>= U+10000) require \U escape (e.g., emoji)
        || (c as u32) >= 0x10000
        // Control characters (except tab and newline which are handled separately)
        || (c.is_control() && !matches!(c, '\n' | '\t'))
	})
}

/// Calculate the length of a string when escaped for double-quoted YAML output.
/// This accounts for escape sequences that expand single characters into multiple:
/// - Backslash, quotes, and control chars: 2 chars (e.g., \\ or \n)
/// - BOM, NEL, line/paragraph separators: 2 chars (\N, \L, \P) or 6 chars (\uFEFF)
/// - Non-BMP characters (emoji): 10 chars (\U00XXXXXX)
/// - Control chars in 0x00-0xFF range: 4 chars (\xXX)
/// - Control chars in 0x100-0xFFFF range: 6 chars (\uXXXX)
fn escaped_double_quoted_length(s: &str) -> usize {
	s.chars()
		.map(|ch| match ch {
			'\\' | '"' | '\0' | '\u{7}' | '\u{8}' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r'
			| '\u{1b}' => 2, // Simple escapes like \\, \", \0, \a, \b, \t, \n, \v, \f, \r, \e
			'\u{0085}' | '\u{2028}' | '\u{2029}' => 2, // \N, \L, \P
			'\u{FEFF}' => 6,                           // \uFEFF
			c if (c as u32) >= 0x10000 => 10,          // \U00XXXXXX (non-BMP)
			c if (c as u32) <= 0xFF && (c.is_control() || (0x7F..=0x9F).contains(&(c as u32))) => {
				4 // \xXX
			}
			c if (c as u32) <= 0xFFFF
				&& (c.is_control() || (0x7F..=0x9F).contains(&(c as u32))) =>
			{
				6 // \uXXXX
			}
			_ => 1, // Regular character
		})
		.sum()
}

/// Write `v` in scientific notation the way Go's `strconv.FormatFloat(v, 'g', -1, 64)`
/// does, which is what `gopkg.in/yaml.v2` and `yaml.v3` emit.
///
/// The mantissa is taken from the shortest decimal representation that round-trips
/// (via `ryu`) and the decimal point is moved *textually*. Deriving it
/// arithmetically instead — `v / 10f64.powi(v.log10().floor())` — is wrong twice
/// over: `10f64.powi(n)` is only exactly representable for `n` up to 22, so larger
/// exponents divide by an approximation, and `log10().floor()` can land a digit out
/// at the boundaries, leaving a mantissa outside `[1, 10)`. Both are visible at
/// ordinary values: `1e100` came out as `0.9999999999999998e+100`, and 2^53 as
/// `9.007199254740993e+15`, one too many.
///
/// The exponent is at least two digits wide, as Go's is: `1e+06`, `1e-05`,
/// `1e+100`.
fn write_go_scientific<W: Write>(out: &mut W, v: f64) -> fmt::Result {
	if v == 0.0 {
		// Signed zero keeps its sign, as Go's does.
		return out.write_str(if v.is_sign_negative() {
			"-0e+00"
		} else {
			"0e+00"
		});
	}

	let mut buffer = ryu::Buffer::new();
	let shortest = buffer.format_finite(v);

	let (sign, shortest) = match shortest.strip_prefix('-') {
		Some(rest) => ("-", rest),
		None => ("", shortest),
	};

	// ryu emits either `123.456` or `1.23456e78`; split the exponent off first.
	let (mantissa, exponent) = match shortest.split_once(['e', 'E']) {
		Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().unwrap_or(0)),
		None => (shortest, 0),
	};

	// Digits without the point, and where the point sat among them.
	let (integer, fraction) = match mantissa.split_once('.') {
		Some((integer, fraction)) => (integer, fraction),
		None => (mantissa, ""),
	};
	let mut digits = String::with_capacity(integer.len() + fraction.len());
	digits.push_str(integer);
	digits.push_str(fraction);
	let mut point = integer.len() as i32 + exponent;

	// Leading zeros move the point rather than counting as significant.
	let leading = digits.len() - digits.trim_start_matches('0').len();
	digits.drain(..leading);
	point -= leading as i32;

	let trailing = digits.len() - digits.trim_end_matches('0').len();
	digits.truncate(digits.len() - trailing);

	debug_assert!(!digits.is_empty(), "a non-zero float has a non-zero digit");

	out.write_str(sign)?;
	out.write_str(&digits[..1])?;
	if digits.len() > 1 {
		out.write_str(".")?;
		out.write_str(&digits[1..])?;
	}

	// `point` counts digits before the point; the exponent is one less.
	let exponent = point - 1;
	let sign = if exponent < 0 { '-' } else { '+' };
	write!(out, "e{sign}{:02}", exponent.abs())
}

/// Check if a string looks like a number (all digits, possibly with leading zeros).
/// Used to determine if numeric string keys like "12345" should be quoted.
fn looks_like_number(s: &str) -> bool {
	!s.is_empty() && (s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok_and(|f| f.is_finite()))
}

/// Check if a string looks like a YAML 1.1 boolean value.
/// These values (y, n, yes, no, on, off, true, false) are ambiguous and
/// should be quoted when used as map keys to match Go yaml.v3 behavior.
fn looks_like_ambiguous_boolean(s: &str) -> bool {
	matches!(
		s.to_lowercase().as_str(),
		"y" | "n" | "yes" | "no" | "on" | "off" | "true" | "false"
	)
}

// ------------------------------------------------------------
// Key scalar helper
// ------------------------------------------------------------

/// Serialize a key using a restricted scalar-only serializer into a `String`.
///
/// Called by map/struct serializers to ensure YAML keys are scalars.
fn scalar_key_to_string<K: Serialize + ?Sized>(key: &K) -> Result<String> {
	let mut s = String::new();
	{
		let mut ks = KeyScalarSink { s: &mut s };
		key.serialize(&mut ks)?;
	}
	Ok(s)
}

struct KeyScalarSink<'a> {
	s: &'a mut String,
}

impl<'a> Serializer for &'a mut KeyScalarSink<'a> {
	type Ok = ();
	type Error = Error;

	type SerializeSeq = ser::Impossible<(), Error>;
	type SerializeTuple = ser::Impossible<(), Error>;
	type SerializeTupleStruct = ser::Impossible<(), Error>;
	type SerializeTupleVariant = ser::Impossible<(), Error>;
	type SerializeMap = ser::Impossible<(), Error>;
	type SerializeStruct = ser::Impossible<(), Error>;
	type SerializeStructVariant = ser::Impossible<(), Error>;

	fn serialize_bool(self, v: bool) -> Result<()> {
		self.s.push_str(if v { "true" } else { "false" });
		Ok(())
	}
	fn serialize_i64(self, v: i64) -> Result<()> {
		let _ = write!(self.s, "{}", v);
		Ok(())
	}
	fn serialize_i32(self, v: i32) -> Result<()> {
		self.serialize_i64(v as i64)
	}
	fn serialize_i16(self, v: i16) -> Result<()> {
		self.serialize_i64(v as i64)
	}
	fn serialize_i8(self, v: i8) -> Result<()> {
		self.serialize_i64(v as i64)
	}
	fn serialize_i128(self, v: i128) -> Result<()> {
		let _ = write!(self.s, "{}", v);
		Ok(())
	}
	fn serialize_u64(self, v: u64) -> Result<()> {
		let _ = write!(self.s, "{}", v);
		Ok(())
	}
	fn serialize_u32(self, v: u32) -> Result<()> {
		self.serialize_u64(v as u64)
	}
	fn serialize_u16(self, v: u16) -> Result<()> {
		self.serialize_u64(v as u64)
	}
	fn serialize_u8(self, v: u8) -> Result<()> {
		self.serialize_u64(v as u64)
	}
	fn serialize_u128(self, v: u128) -> Result<()> {
		let _ = write!(self.s, "{}", v);
		Ok(())
	}
	fn serialize_f32(self, v: f32) -> Result<()> {
		let v = v as f64;
		if v.is_nan() {
			self.s.push_str(".nan");
		} else if v.is_infinite() {
			if v.is_sign_positive() {
				self.s.push_str(".inf");
			} else {
				self.s.push_str("-.inf");
			}
		} else {
			let mut buf = ryu::Buffer::new();
			let s = buf.format(v);
			if !s.contains('.') && !s.contains('e') && !s.contains('E') {
				self.s.push_str(s);
				self.s.push_str(".0");
			} else {
				self.s.push_str(s);
			}
		}
		Ok(())
	}
	fn serialize_f64(self, v: f64) -> Result<()> {
		if v.is_nan() {
			self.s.push_str(".nan");
		} else if v.is_infinite() {
			if v.is_sign_positive() {
				self.s.push_str(".inf");
			} else {
				self.s.push_str("-.inf");
			}
		} else {
			let mut buf = ryu::Buffer::new();
			let s = buf.format(v);
			if !s.contains('.') && !s.contains('e') && !s.contains('E') {
				self.s.push_str(s);
				self.s.push_str(".0");
			} else {
				self.s.push_str(s);
			}
		}
		Ok(())
	}
	fn serialize_char(self, v: char) -> Result<()> {
		let mut buf = [0u8; 4];
		self.serialize_str(v.encode_utf8(&mut buf))
	}
	fn serialize_str(self, v: &str) -> Result<()> {
		if is_plain_safe(v) {
			self.s.push_str(v);
		} else {
			// Check if string needs escaping (control chars, etc.)
			let needs_escaping = v.chars().any(|c| {
				c.is_control()
					|| matches!(
						c,
						'\n' | '\r'
							| '\t' | '\0' | '\u{7}'
							| '\u{8}' | '\u{b}' | '\u{c}'
							| '\u{1b}' | '\u{FEFF}'
							| '\u{0085}' | '\u{2028}'
							| '\u{2029}'
					)
			});

			// Check if string looks like another YAML type (needs double quotes to disambiguate)
			let looks_like_other_type = v.is_empty()
				|| v == "~" || v.eq_ignore_ascii_case("null")
				|| v.eq_ignore_ascii_case("true")
				|| v.eq_ignore_ascii_case("false");

			// Use single quotes unless escaping needed or type-ambiguous
			// This matches Go yaml.v3 behavior for keys like '@type'
			if !needs_escaping && !looks_like_other_type {
				// Use single quotes - escape internal ' by doubling to ''
				self.s.push('\'');
				for ch in v.chars() {
					if ch == '\'' {
						self.s.push_str("''");
					} else {
						self.s.push(ch);
					}
				}
				self.s.push('\'');
			} else {
				// Use double quotes with escaping
				self.s.push('"');
				for ch in v.chars() {
					match ch {
						'\\' => self.s.push_str("\\\\"),
						'"' => self.s.push_str("\\\""),
						'\n' => self.s.push_str("\\n"),
						'\r' => self.s.push_str("\\r"),
						'\t' => self.s.push_str("\\t"),
						c if c.is_control() => {
							use std::fmt::Write as _;
							let _ = write!(self.s, "\\u{:04X}", c as u32);
						}
						c => self.s.push(c),
					}
				}
				self.s.push('"');
			}
		}
		Ok(())
	}
	fn serialize_bytes(self, _v: &[u8]) -> Result<()> {
		non_scalar_key_e()
	}
	fn serialize_none(self) -> Result<()> {
		self.s.push_str("null");
		Ok(())
	}
	fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<()> {
		v.serialize(self)
	}
	fn serialize_unit(self) -> Result<()> {
		self.s.push_str("null");
		Ok(())
	}
	fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
		self.serialize_unit()
	}
	fn serialize_unit_variant(
		self,
		_name: &'static str,
		_idx: u32,
		variant: &'static str,
	) -> Result<()> {
		self.serialize_str(variant)
	}
	fn serialize_newtype_struct<T: ?Sized + Serialize>(
		self,
		_name: &'static str,
		_value: &T,
	) -> Result<()> {
		non_scalar_key_e()
	}
	fn serialize_newtype_variant<T: ?Sized + Serialize>(
		self,
		_: &'static str,
		_: u32,
		_: &'static str,
		_: &T,
	) -> Result<()> {
		non_scalar_key_e()
	}
	fn serialize_seq(self, _len: Option<usize>) -> Result<ser::Impossible<(), Error>> {
		non_scalar_key()
	}
	fn serialize_tuple(self, _len: usize) -> Result<ser::Impossible<(), Error>> {
		non_scalar_key()
	}
	fn serialize_tuple_struct(
		self,
		_: &'static str,
		_: usize,
	) -> Result<ser::Impossible<(), Error>> {
		non_scalar_key()
	}
	fn serialize_tuple_variant(
		self,
		_: &'static str,
		_: u32,
		_: &'static str,
		_: usize,
	) -> Result<ser::Impossible<(), Error>> {
		non_scalar_key()
	}
	fn serialize_map(self, _len: Option<usize>) -> Result<ser::Impossible<(), Error>> {
		non_scalar_key()
	}
	fn serialize_struct(
		self,
		_name: &'static str,
		_len: usize,
	) -> Result<ser::Impossible<(), Error>> {
		non_scalar_key()
	}
	fn serialize_struct_variant(
		self,
		_: &'static str,
		_: u32,
		_: &'static str,
		_: usize,
	) -> Result<ser::Impossible<(), Error>> {
		non_scalar_key()
	}
	fn collect_str<T: ?Sized + fmt::Display>(self, v: &T) -> Result<()> {
		self.serialize_str(&v.to_string())
	}
	fn is_human_readable(&self) -> bool {
		true
	}
}

fn unexpected() -> Result<ser::Impossible<(), Error>> {
	Err(Error::unexpected("unexpected"))
}

fn unexpected_e() -> Result<()> {
	Err(Error::unexpected("unexpected"))
}

fn non_scalar_key() -> Result<ser::Impossible<(), Error>> {
	Err(Error::unexpected("non-scalar key"))
}

fn non_scalar_key_e() -> Result<()> {
	Err(Error::unexpected("non-scalar key"))
}
