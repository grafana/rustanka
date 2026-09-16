//! A port of `internal/formatter/enforce_string_style.go`.
//!
//! Every quoted string is rewritten to the configured quote — `'` under
//! `Options::default` — by unescaping it and escaping it again for the quote
//! it is about to get. So the pass normalises
//! more than the quotes: a needless escape disappears (`"a\/b"` becomes
//! `'a/b'`), an escape for a character that does not need one collapses
//! (`"\u0041"` becomes `'A'`), and a character that does need one comes back
//! in upstream's own spelling (`"\u009F"` becomes `'\u009f'`, lower case,
//! because `StringEscape` formats with `%04x`).
//!
//! # The quotes decide, not the option
//!
//! A string whose text contains a `'` or a `"` takes whichever syntax avoids
//! escaping, whatever [`StringStyle`] says — that is the documented carve-out,
//! and it is why `'it\'s'` becomes `"it's"` under `StringStyle::Single`. Where
//! the text contains **both**, neither syntax avoids escaping and upstream
//! returns without touching the literal at all, so such a string keeps
//! whatever quote it was written with.
//!
//! Note the counting is done on the *unescaped* text, so `"a\'b"` counts one
//! single quote even though the source had no bare one.
//!
//! # Why this overrides `literal_string` and not `visit`
//!
//! `pass::base::import` reaches an import's filename through
//! [`AstPass::literal_string`](crate::pass::AstPass::literal_string) and
//! **not** through `visit`, so a pass that overrode `visit` would restyle every
//! string in the file except the one in `import "foo.libsonnet"`. Upstream
//! overrides the leaf hook for exactly that reason.
//!
//! # Three kinds are returned on, unexamined
//!
//! A `|||` block, `@'verbatim'` and `@"verbatim"` are each an early return.
//! For the verbatim kinds that is not a stylistic choice: the parser has
//! already collapsed their doubled quotes, so restyling one would mean
//! re-doubling them, and upstream does not try. The return is unconditional,
//! so a block string holding both kinds of quote is not even counted.
//!
//! # The panic is unreachable
//!
//! Upstream passes `lit.Loc()` to `StringUnescape` and then **discards the
//! error**, panicking with a fixed string. It cannot fire: the two kinds that
//! get here are exactly the two the parser validated with the same function
//! (`tokenStringToAst`'s `validate` branch). That discarded error is also why
//! no hook in [`crate::pass`] carries a location — see that module's second
//! departure.

use crate::{
	StringStyle,
	ast::{LiteralString, LiteralStringKind},
	location::LocationRange,
	pass::AstPass,
	string_util::{string_escape, string_unescape},
};

/// `formatter.EnforceStringStyle`.
///
/// Upstream holds the whole `Options` struct; this holds the one field it
/// reads, which says what the pass can depend on. `FormatNode` skips the pass
/// entirely for [`StringStyle::Leave`], so that variant never reaches here —
/// and if it did it would behave as [`StringStyle::Double`], since the test is
/// `== StringStyleSingle`. [`crate::format`] does the skipping.
#[derive(Debug, Clone, Copy)]
pub struct EnforceStringStyle {
	style: StringStyle,
}

impl EnforceStringStyle {
	pub fn new(style: StringStyle) -> Self {
		Self { style }
	}
}

impl AstPass for EnforceStringStyle {
	type Ctx = ();

	fn base_context(&mut self) {}

	fn literal_string(&mut self, node: &mut LiteralString, _ctx: &()) {
		// Upstream's three early returns, in its order. `fully_escaped` is the
		// same partition, but it is spelled out here because upstream spells
		// it out and because the reason differs per kind — see the module
		// documentation.
		match node.kind {
			LiteralStringKind::Block
			| LiteralStringKind::VerbatimDouble
			| LiteralStringKind::VerbatimSingle => return,
			LiteralStringKind::Single | LiteralStringKind::Double => {}
		}

		// The location is Go's `lit.Loc()`, whose error upstream throws away.
		// There is none on a `LiteralString` here for that reason, so this is
		// the zero value, and the `expect` is upstream's panic: the parser
		// validated these two kinds with this very function.
		let canonical = string_unescape(&LocationRange::default(), &node.value)
			.expect("Badly formatted string, should have been caught in lexer.");

		let num_single = canonical.matches('\'').count();
		let num_double = canonical.matches('"').count();
		if num_single > 0 && num_double > 0 {
			// Neither syntax avoids escaping, so the literal is left exactly
			// as written — including its kind, even where the option asked for
			// the other one.
			return;
		}

		// Upstream writes this as an assignment from the option followed by two
		// unconditional overrides. They collapse into one expression here
		// because the branch above has already returned for the only input
		// that could fire both — so at most one override applies, and the
		// order they are written in cannot matter.
		let use_single = if num_single > 0 {
			false
		} else if num_double > 0 {
			true
		} else {
			self.style == StringStyle::Single
		};

		node.value = string_escape(&canonical, use_single);
		node.kind = if use_single {
			LiteralStringKind::Single
		} else {
			LiteralStringKind::Double
		};
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		CommentStyle, Options, format, parser::snippet_to_raw_ast, pass::visit_file,
		unparse::Unparser,
	};

	/// This pass alone, at the default style, and then the unparser.
	///
	/// Running the one pass rather than [`format`] is what makes the
	/// assertions about *this* pass: `PrettyFieldNames` would unquote a field
	/// name before this pass ever saw it. The unparser reads only
	/// `pad_arrays` and `pad_objects`, so the defaults are what it wants.
	fn restyle(input: &str) -> String {
		let (mut node, mut final_fodder) =
			snippet_to_raw_ast("t.jsonnet", input).expect("the snippet parses");
		visit_file(
			&mut EnforceStringStyle::new(StringStyle::Single),
			&mut node,
			&mut final_fodder,
		);
		let mut unparser = Unparser::new(Options::default());
		unparser.unparse(&node, false);
		unparser.finish_file(&final_fodder);
		unparser.finish()
	}

	#[test]
	fn a_double_quoted_string_becomes_single_quoted() {
		assert_eq!(restyle("\"foo\""), "'foo'\n");
		assert_eq!(restyle("'foo'"), "'foo'\n");
		assert_eq!(restyle("\"\""), "''\n");
	}

	#[test]
	fn the_quote_the_text_contains_wins_over_the_option() {
		// `'it\'s'` is escaping the quote it chose, so the other one is used.
		assert_eq!(restyle("'it\\'s'"), "\"it's\"\n");
		// And already right, so nothing moves.
		assert_eq!(restyle("\"it's\""), "\"it's\"\n");
		// The other direction, which agrees with the option anyway.
		assert_eq!(restyle("\"say \\\"hi\\\"\""), "'say \"hi\"'\n");
		assert_eq!(restyle("'say \"hi\"'"), "'say \"hi\"'\n");
	}

	#[test]
	fn a_string_containing_both_quotes_is_left_alone() {
		// Neither syntax avoids escaping, so upstream returns before touching
		// the kind — which is why this stays double-quoted under
		// `StringStyle::Single`.
		assert_eq!(restyle("\"it's \\\"x\\\"\""), "\"it's \\\"x\\\"\"\n");
		assert_eq!(restyle("'it\\'s \"x\"'"), "'it\\'s \"x\"'\n");
	}

	#[test]
	fn a_needless_escape_is_dropped_even_when_the_kind_does_not_change() {
		// numSingle > 0 keeps this Double, but the round trip still rewrites
		// the value: `\'` is legal in a double-quoted string and is never
		// written back.
		assert_eq!(restyle("\"a\\'b\""), "\"a'b\"\n");
		// The mirror image, where the kind changes too.
		assert_eq!(restyle("'a\\\"b'"), "'a\"b'\n");
		// `\/` is accepted by StringUnescape (see json.org) and never emitted.
		assert_eq!(restyle("\"a\\/b\""), "'a/b'\n");
	}

	#[test]
	fn the_escapes_string_escape_writes_back_survive() {
		assert_eq!(restyle("\"a\\nb\""), "'a\\nb'\n");
		assert_eq!(restyle("\"a\\tb\""), "'a\\tb'\n");
		assert_eq!(restyle("\"a\\\\b\""), "'a\\\\b'\n");
		assert_eq!(restyle("\"\\u0000\""), "'\\u0000'\n");
	}

	#[test]
	fn a_unicode_escape_collapses_unless_string_escape_would_write_it() {
		assert_eq!(restyle("\"\\u0041\""), "'A'\n");
		assert_eq!(restyle("\"\\u00E9\""), "'\u{e9}'\n");
		// U+009F is in the C1 range StringEscape re-escapes, and it formats
		// with %04x — so an upper-case escape comes back lower-case.
		assert_eq!(restyle("\"\\u009F\""), "'\\u009f'\n");
	}

	#[test]
	fn the_three_unexamined_kinds_are_untouched() {
		assert_eq!(restyle("@\"foo\""), "@\"foo\"\n");
		assert_eq!(restyle("@'foo'"), "@'foo'\n");
		// A verbatim string's doubled quote has already been collapsed by the
		// parser, so restyling it would mean re-doubling. Upstream returns.
		assert_eq!(restyle("@'it''s'"), "@'it''s'\n");
		// The block early return is unconditional, so the quotes inside are
		// never even counted.
		assert_eq!(
			restyle("|||\n  it's \"x\"\n|||"),
			"|||\n  it's \"x\"\n|||\n"
		);
	}

	#[test]
	fn an_imports_filename_is_reached() {
		// The reason the pass overrides `literal_string`: `base::import`
		// reaches this literal through that hook and not through `visit`, so
		// overriding `visit` would miss every import in the file.
		assert_eq!(
			restyle("import \"foo.libsonnet\""),
			"import 'foo.libsonnet'\n"
		);
		assert_eq!(restyle("importstr \"a.txt\""), "importstr 'a.txt'\n");
		assert_eq!(restyle("importbin \"a.bin\""), "importbin 'a.bin'\n");
	}

	#[test]
	fn strings_in_every_other_position_are_reached() {
		assert_eq!(restyle("{ \"foo\": 1 }"), "{ 'foo': 1 }\n");
		assert_eq!(restyle("{ [\"foo\"]: 1 }"), "{ ['foo']: 1 }\n");
		assert_eq!(restyle("a[\"foo\"]"), "a['foo']\n");
		assert_eq!(restyle("[\"a\", { b: \"c\" }]"), "['a', { b: 'c' }]\n");
	}

	#[test]
	fn a_keyword_field_name_keeps_quotes_and_gets_the_style() {
		// The pipeline case, and what `tests/golden/issue195.jsonnet` pins:
		// `PrettyFieldNames` leaves a keyword quoted, so this pass is what
		// decides which quote it keeps.
		assert_eq!(
			format("t.jsonnet", "{ \"false\": 1 }", &Options::default()).expect("it parses"),
			"{ 'false': 1 }\n"
		);
	}

	#[test]
	fn leave_skips_the_pass_entirely() {
		// The gate is in `FormatNode`, not in the pass: with `Leave` the pass
		// is never constructed, so a double-quoted string survives.
		let options = Options {
			string_style: StringStyle::Leave,
			..Options::default()
		};
		assert_eq!(
			format("t.jsonnet", "\"foo\"", &options).expect("it parses"),
			"\"foo\"\n"
		);
	}

	#[test]
	fn double_is_the_mirror_of_single() {
		// NOT GRADED BY THE PASS ORACLE: the dumper runs every pass under
		// `DefaultOptions()`, so only `Single` is covered there, and `tk fmt`
		// reaches no other value. This is read off upstream — the option is
		// consulted once, as `StringStyle == StringStyleSingle`, and both
		// carve-outs override it either way.
		let options = Options {
			string_style: StringStyle::Double,
			comment_style: CommentStyle::Leave,
			pretty_field_names: false,
			..Options::default()
		};
		let double = |input: &str| format("t.jsonnet", input, &options).expect("it parses");
		assert_eq!(double("'foo'"), "\"foo\"\n");
		assert_eq!(double("\"foo\""), "\"foo\"\n");
		// The carve-out still wins.
		assert_eq!(double("'say \"hi\"'"), "'say \"hi\"'\n");
	}
}
