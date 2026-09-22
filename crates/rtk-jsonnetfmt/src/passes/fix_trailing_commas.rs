//! A port of `internal/formatter/fix_trailing_commas.go`.
//!
//! A trailing comma belongs on a list that is split over several lines, and
//! nowhere else. "Split over several lines" is decided by
//! [`Fodder::contains_newline`] on the fodder before the closing bracket or
//! before the last comma — not by counting characters, because a
//! fodder-preserving formatter has no line width to reason about.
//!
//! # A comprehension is the opposite case
//!
//! `[e for x in y]` and `{ [k]: v for x in y }` allow a comma where an array's
//! trailing comma would sit, and it is **always** removed, however the
//! comprehension is laid out. So the two shapes get different helpers:
//! `fix_comma` for a list, `remove_comma` for a comprehension. The fodder that
//! was before the comma moves to the front of whatever token now comes next —
//! the closing bracket for a list, the `for` for a comprehension — so a
//! comment written before the comma survives the comma's removal.
//!
//! # An empty list returns before the traversal
//!
//! Upstream's `Array` and `Object` hooks `return` when there is nothing in
//! them, *before* calling the base traversal, rather than skipping only the
//! comma work. So the close fodder of `[ /* c */ ]` is not visited by this
//! pass at all. It makes no difference to this pass, which overrides no fodder
//! hook, and it is reproduced because the traversal is what later passes are
//! built on.

use crate::{
	ast::{Array, ArrayComp, Object, ObjectComp},
	fodder::Fodder,
	pass::{AstPass, base},
};

/// `formatter.FixTrailingCommas`.
#[derive(Debug, Default, Clone, Copy)]
pub struct FixTrailingCommas;

impl FixTrailingCommas {
	/// Upstream's `fixComma`: a trailing comma iff the list spans lines.
	///
	/// Associated rather than a method because upstream's is a method that
	/// never touches its receiver; there is no pass state to reach.
	///
	/// The middle case is the subtle one. Where the comma is needed *and* the
	/// fodder before it contains a newline, the comma is kept but that fodder
	/// is moved on to the closing bracket — so
	///
	/// ```text
	/// [
	///   1
	///   ,
	/// ]
	/// ```
	///
	/// puts the comma back against the `1` instead of leaving it stranded on a
	/// line of its own.
	fn fix_comma(
		last_comma_fodder: &mut Fodder,
		trailing_comma: &mut bool,
		close_fodder: &mut Fodder,
	) {
		let need_comma = close_fodder.contains_newline() || last_comma_fodder.contains_newline();
		if *trailing_comma {
			if !need_comma {
				// Remove it, but keep its fodder.
				*trailing_comma = false;
				close_fodder.move_front(last_comma_fodder);
			} else if last_comma_fodder.contains_newline() {
				// The comma is needed, but a newline currently separates it
				// from the element it follows.
				close_fodder.move_front(last_comma_fodder);
			}
		} else if need_comma {
			// There was no comma, but there is a newline before the closing
			// bracket, so add one.
			*trailing_comma = true;
		}
	}

	/// Upstream's `removeComma`: a comprehension never keeps one.
	fn remove_comma(
		last_comma_fodder: &mut Fodder,
		trailing_comma: &mut bool,
		close_fodder: &mut Fodder,
	) {
		if *trailing_comma {
			// Remove it, but keep its fodder.
			*trailing_comma = false;
			close_fodder.move_front(last_comma_fodder);
		}
	}
}

impl AstPass for FixTrailingCommas {
	type Ctx = ();

	fn base_context(&mut self) {}

	fn array(&mut self, node: &mut Array, ctx: &()) {
		{
			let Array {
				elements,
				close_fodder,
				trailing_comma,
			} = node;
			// No comma present, and none can be added.
			let Some(last) = elements.last_mut() else {
				return;
			};
			Self::fix_comma(&mut last.comma_fodder, trailing_comma, close_fodder);
		}
		base::array(self, node, ctx);
	}

	fn array_comp(&mut self, node: &mut ArrayComp, ctx: &()) {
		Self::remove_comma(
			&mut node.trailing_comma_fodder,
			&mut node.trailing_comma,
			&mut node.spec.for_fodder,
		);
		base::array_comp(self, node, ctx);
	}

	fn object(&mut self, node: &mut Object, ctx: &()) {
		{
			let Object {
				fields,
				close_fodder,
				trailing_comma,
			} = node;
			// No comma present, and none can be added.
			let Some(last) = fields.last_mut() else {
				return;
			};
			Self::fix_comma(&mut last.comma_fodder, trailing_comma, close_fodder);
		}
		base::object(self, node, ctx);
	}

	fn object_comp(&mut self, node: &mut ObjectComp, ctx: &()) {
		{
			let ObjectComp {
				fields,
				spec,
				trailing_comma,
				..
			} = node;
			// Upstream indexes the last field with no emptiness check and
			// would panic on an object comprehension with no fields; the
			// parser cannot produce one, since the computed field is what
			// makes it a comprehension. Skipping is the same behaviour for
			// every tree that can actually reach here.
			if let Some(last) = fields.last_mut() {
				Self::remove_comma(&mut last.comma_fodder, trailing_comma, &mut spec.for_fodder);
			}
		}
		base::object_comp(self, node, ctx);
	}
}

#[cfg(test)]
mod tests {
	use crate::format_default;

	/// These expectations are derived by reading upstream, which this project's
	/// own record says is not good enough on its own: of 16 lexer expectations
	/// derived that way, 2 were wrong, and both were about fodder the model
	/// *composes* rather than reads — which is all this pass does.
	/// `testdata/pass-snippets.json` carries the same cases for
	/// `make update-fmt-pass-oracle` to answer from Go, and
	/// `tests/pass_parity.rs` grades them node for node. Until that oracle is
	/// generated these are the weaker of the two graders, not the authority.
	fn format(input: &str) -> String {
		format_default("t.jsonnet", input).expect("the snippet parses")
	}

	#[test]
	fn a_comma_is_removed_from_a_list_on_one_line() {
		assert_eq!(format("[1,]"), "[1]\n");
		assert_eq!(format("{a:1,}"), "{ a: 1 }\n");
	}

	#[test]
	fn a_comma_is_added_to_a_list_split_over_lines() {
		assert_eq!(format("[\n  1\n]"), "[\n  1,\n]\n");
		assert_eq!(format("{\n  a: 1\n}"), "{\n  a: 1,\n}\n");
	}

	#[test]
	fn removing_a_comma_keeps_the_fodder_that_was_before_it() {
		// The comment moves on to the `]`, where the unparser writes it with
		// the single space an interstitial after a token gets.
		assert_eq!(format("[1 /* c */,]"), "[1 /* c */]\n");
	}

	#[test]
	fn a_comma_stranded_on_its_own_line_is_pulled_back() {
		// The comma is needed, but a newline separates it from the element, so
		// that newline moves on to the `]` — and merges with the one already
		// there, because a `LineEnd` may not follow a `LineEnd`.
		assert_eq!(format("[\n  1\n  ,\n]"), "[\n  1,\n]\n");
	}

	#[test]
	fn an_empty_list_is_left_alone() {
		// Including the one split over lines: there is no comma to move and
		// none can be added, so the newline before the `]` stays put. Note
		// `pad_objects` does not reach `{}` either — the unparser's trailing
		// space needs something to have been written inside the braces.
		assert_eq!(format("[]"), "[]\n");
		assert_eq!(format("{}"), "{}\n");
		assert_eq!(format("[\n]"), "[\n]\n");
	}

	#[test]
	fn a_comprehension_never_keeps_its_comma() {
		assert_eq!(format("[x, for x in [1]]"), "[x for x in [1]]\n");
		assert_eq!(
			format("{ [k]: k, for k in ['a'] }"),
			"{ [k]: k for k in ['a'] }\n"
		);
	}

	#[test]
	fn a_comprehension_comma_keeps_its_fodder_too() {
		// The comment moves to the front of the `for`'s fodder rather than
		// disappearing with the comma.
		assert_eq!(
			format("[x /* c */, for x in [1]]"),
			"[x /* c */ for x in [1]]\n"
		);
	}
}
