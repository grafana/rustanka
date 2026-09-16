//! A port of `internal/formatter/enforce_max_blank_lines.go`.
//!
//! Eight lines of Go: clamp `blanks` to `MaxBlankLines` — 2 under
//! `Options::default` — on every fodder element that is not an interstitial.
//! Blank lines are a *count* on one element rather than a run of elements, so
//! a file with ten blank lines in it has one element carrying `blanks = 9`,
//! and clamping it is the whole pass.
//!
//! # The interstitial guard is not about blanks
//!
//! An interstitial cannot carry blanks at all: [`FodderElement::new`] panics
//! on one that tries, because a comment *within* a line has no vertical space
//! to describe. So the `Kind != FodderInterstitial` test can never change an
//! answer, and it is reproduced only because upstream writes it.
//!
//! [`FodderElement::new`]: crate::fodder::FodderElement::new
//!
//! # All of the interest is in which elements it reaches
//!
//! This pass overrides `FodderElement`, so it sees exactly what
//! [`crate::pass::base`] walks and nothing else — which puts the four slots
//! the traversal skips out of its reach, the same way they are out of
//! `EnforceCommentStyle`'s. Two of the four can hold blanks, and `tk fmt`
//! leaves those runs exactly as written:
//!
//! - `{ a: 'b'` … blank lines … `in super }` — `InSuper`'s `in_fodder`.
//! - `a.` … blank lines … `b` — `Index`'s `right_bracket_fodder`, doubling as
//!   the fodder before an identifier.
//!
//! The other two cannot: `Apply`'s `tail_strict_fodder` is skipped only when
//! `tailstrict` is absent, and then there is no token for fodder to precede;
//! a `Parameter`'s `eq_fodder` is skipped only without a default, and then
//! there is no `=`. So the hole is exactly two slots wide.
//!
//! `FixIndentation` reaches all four, because it is not an `AstPass` and walks
//! the tree itself. That asymmetry is why a blank run in `in super` survives
//! this pass and is still re-indented.
//!
//! # What grades it
//!
//! `testdata/pass-snippets.json`, and nothing whatsoever before Phase 2d
//! added those: the pass oracle puts this pass at **0 changed cells of 138**
//! corpus files and it changed **none** of the 113 snippets that existed.
//! Jsonnet already formatted by `tk fmt` has no run of three blank lines in it
//! by definition, so no breadth corpus of real files can contain the input
//! this pass exists for. The same argument that made `EnforceCommentStyle`
//! ungradeable in Phase 2c, one phase later.

use crate::{
	fodder::{FodderElement, FodderKind},
	pass::AstPass,
};

/// `formatter.EnforceMaxBlankLines`.
///
/// Upstream carries the whole `Options`; this carries the one field it reads,
/// which is what says what the pass can depend on. Whether it runs at all is
/// [`crate::format`]'s business, as it is `FormatNode`'s — the gate there is
/// `MaxBlankLines > 0`.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnforceMaxBlankLines {
	max_blank_lines: usize,
}

impl EnforceMaxBlankLines {
	pub fn new(max_blank_lines: usize) -> Self {
		Self { max_blank_lines }
	}
}

impl AstPass for EnforceMaxBlankLines {
	type Ctx = ();

	fn base_context(&mut self) {}

	fn fodder_element(&mut self, element: &mut FodderElement, _ctx: &()) {
		// Upstream does not call the base hook here, and there is nothing to
		// call: a fodder element is a leaf.
		if element.kind != FodderKind::Interstitial {
			element.blanks = element.blanks.min(self.max_blank_lines);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		CommentStyle, Options, StringStyle, format,
		parser::snippet_to_raw_ast,
		pass::{base, visit_file},
	};

	/// Every fodder element the traversal reaches, as `kind:blanks`.
	#[derive(Debug, Default)]
	struct Blanks(Vec<String>);

	impl AstPass for Blanks {
		type Ctx = ();

		fn base_context(&mut self) {}

		fn fodder_element(&mut self, element: &mut FodderElement, ctx: &()) {
			self.0
				.push(format!("{}:{}", element.kind.name(), element.blanks));
			base::fodder_element(self, element, ctx);
		}
	}

	fn blanks_at(max: usize, input: &str) -> Vec<String> {
		let (mut node, mut final_fodder) =
			snippet_to_raw_ast("t.jsonnet", input).expect("the snippet parses");
		visit_file(
			&mut EnforceMaxBlankLines::new(max),
			&mut node,
			&mut final_fodder,
		);
		let mut collector = Blanks::default();
		visit_file(&mut collector, &mut node, &mut final_fodder);
		collector.0
	}

	fn blanks(input: &str) -> Vec<String> {
		blanks_at(2, input)
	}

	/// The whole pipeline, for the slots the traversal cannot reach — asking
	/// the collector above would be circular, since it has the same holes.
	///
	/// The other layout passes are on, because a blank-line question is a
	/// layout question; the representation passes are set to `Leave` so a
	/// quote or a comment marker cannot show up in an assertion about blanks.
	fn formatted(input: &str) -> String {
		let options = Options {
			string_style: StringStyle::Leave,
			comment_style: CommentStyle::Leave,
			..Options::default()
		};
		format("t.jsonnet", input, &options).expect("the snippet parses")
	}

	#[test]
	fn a_run_longer_than_the_limit_is_clamped() {
		// Four newlines between the fields: one line end carrying blanks = 3.
		assert_eq!(
			blanks("{\n  a: 1,\n\n\n\n  b: 2,\n}"),
			["LineEnd:0", "LineEnd:2", "LineEnd:0"]
		);
	}

	#[test]
	fn a_run_at_the_limit_is_left_alone() {
		// The comparison is strict, so `blanks == max` is not clamped. This is
		// the assertion that says the pass is not off by one.
		assert_eq!(
			blanks("{\n  a: 1,\n\n\n  b: 2,\n}"),
			["LineEnd:0", "LineEnd:2", "LineEnd:0"]
		);
		assert_eq!(
			blanks("{\n  a: 1,\n\n  b: 2,\n}"),
			["LineEnd:0", "LineEnd:1", "LineEnd:0"]
		);
	}

	#[test]
	fn a_long_run_collapses_to_the_limit_and_not_to_a_multiple_of_it() {
		assert_eq!(
			blanks("{\n  a: 1,\n\n\n\n\n\n\n\n  b: 2,\n}"),
			["LineEnd:0", "LineEnd:2", "LineEnd:0"]
		);
	}

	#[test]
	fn a_paragraph_is_clamped_like_a_line_end() {
		// A paragraph's blanks are the blank lines *after* its comment, and
		// the kind guard excludes only interstitials.
		assert_eq!(blanks("// c\n\n\n\n1"), ["Paragraph:2"]);
	}

	#[test]
	fn a_run_of_newlines_at_end_of_file_never_becomes_fodder_at_all() {
		// The lexer's main loop tests for end of input *before* it adds the
		// line end it has just measured, so final whitespace is discarded
		// rather than kept. So there is nothing here for this pass to clamp —
		// and nothing for `removeExtraTrailingNewlines` at step 14 to zero
		// either. That step can only ever fire on a file whose final fodder
		// ends in a comment, which is the case below.
		assert!(blanks("1\n\n\n\n").is_empty());
		assert_eq!(formatted("1\n\n\n\n"), "1\n");
	}

	#[test]
	fn the_final_fodder_is_reached_when_a_comment_holds_it_open() {
		// A comment's own fodder element carries the blanks that follow it,
		// and those are measured by `lex_until_newline` before the main loop
		// ever reaches end of input. So this is the one shape in which
		// trailing blank lines survive lexing.
		//
		// Two elements, not one: the newline after `1` is its own line end,
		// added by the main loop before the comment is reached.
		assert_eq!(blanks("1\n// c\n\n\n\n"), ["LineEnd:0", "Paragraph:2"]);
		// `base::file` visits the final fodder after the root node, so this
		// pass clamps 3 to 2 — and then step 14 zeroes even those.
		assert_eq!(formatted("1\n// c\n\n\n\n"), "1\n// c\n");
	}

	#[test]
	fn the_opening_fodder_is_reached_but_the_pipeline_deletes_it_first() {
		// In isolation the leading run is clamped.
		assert_eq!(blanks("\n\n\n\n1"), ["LineEnd:2"]);
		// In the pipeline `removeInitialNewlines` runs at step 2 and this pass
		// at step 3, so there is nothing left to clamp. That is what the order
		// buys, and reversing it would leave two blank lines at the top of
		// every file that had any.
		assert_eq!(formatted("\n\n\n\n1"), "1\n");
	}

	#[test]
	fn an_interstitial_carries_no_blanks_to_clamp() {
		// The model forbids them, so the kind guard can never change an
		// answer. Asserted so that a port which drops the guard is still seen
		// to be equivalent rather than merely untested. A single-line C
		// comment is an interstitial wherever it sits, so this is a blank run,
		// an interstitial and another blank run in one fodder.
		assert_eq!(
			blanks("[\n\n\n\n  /* c */\n\n\n\n  1,\n]"),
			["LineEnd:2", "Interstitial:0", "LineEnd:2", "LineEnd:0"]
		);
	}

	#[test]
	fn a_run_in_an_unvisited_slot_survives() {
		// `InSuper`'s `in_fodder` is never walked, so `tk fmt` leaves four
		// blank lines in place here.
		assert!(formatted("{ a: 'b'\n\n\n\n\n  in super }").contains("\n\n\n\n\n"));
		// `Index` skips `right_bracket_fodder` when the index is an
		// identifier, and that slot doubles as the fodder before it.
		assert!(formatted("a.\n\n\n\n\n  b").contains("\n\n\n\n\n"));
		// But `super_index` visits `id_fodder` either way, which is what makes
		// the two above holes rather than a rule.
		assert!(!formatted("{ a: super.\n\n\n\n\n  b }").contains("\n\n\n\n\n"));
	}

	#[test]
	fn the_two_other_unvisited_slots_cannot_hold_blanks() {
		// `eq_fodder` is visited where there is a default — and without one
		// there is no `=` for fodder to precede at all.
		assert_eq!(blanks("function(x\n\n\n\n  = 1) x"), ["LineEnd:2"]);
		// Same for `tail_strict_fodder`: skipped only when `tailstrict` is
		// absent, and then the slot is empty.
		assert_eq!(blanks("f(1)\n\n\n\ntailstrict"), ["LineEnd:2"]);
	}

	#[test]
	fn a_limit_of_zero_removes_every_blank_line() {
		// Not reachable through `tk fmt`, which passes 2, and not graded by
		// the pass oracle either — the dumper runs every pass under
		// `DefaultOptions()`. Read off upstream: `FormatNode` gates the pass
		// on `MaxBlankLines > 0`, so 0 means the pass never runs and blank
		// lines are kept. Running it anyway is what this asserts.
		assert_eq!(
			blanks_at(0, "{\n  a: 1,\n\n\n  b: 2,\n}"),
			["LineEnd:0", "LineEnd:0", "LineEnd:0"]
		);
	}
}
