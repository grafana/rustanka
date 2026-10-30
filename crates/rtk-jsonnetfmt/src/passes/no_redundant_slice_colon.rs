//! A port of `internal/formatter/no_redundant_slice_colon.go`.
//!
//! `a[1::]` is written back as `a[1:]`, because the unparser emits the step
//! colon only where there is a step *or* fodder before it. Dropping the colon
//! would drop that fodder with it, so this pass moves it on to the `]` first.
//!
//! # What can actually be in that slot
//!
//! Fodder records line ends, blank counts, indents and comments — never plain
//! spaces — so a redundant colon usually has nothing before it and this pass
//! does nothing. It earns its keep on a comment or a newline:
//! `a[1:2 /* c */:]` becomes `a[1:2  /* c */]`, with the comment kept.
//!
//! Note `a[::]` does not reach it at all: the lexer makes `::` a single
//! operator token, so the parser's `::` branch never assigns
//! `step_colon_fodder` and `a[::]` produces exactly the tree `a[:]` does. The
//! step colon has to be a separate token — which takes something between the
//! two colons — for there to be any fodder to move.

use crate::{
	ast::Slice,
	pass::{AstPass, base},
};

/// `formatter.NoRedundantSliceColon`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoRedundantSliceColon;

impl AstPass for NoRedundantSliceColon {
	type Ctx = ();

	fn base_context(&mut self) {}

	fn slice(&mut self, node: &mut Slice, ctx: &()) {
		if node.step.is_none() && !node.step_colon_fodder.is_empty() {
			let Slice {
				step_colon_fodder,
				right_bracket_fodder,
				..
			} = node;
			right_bracket_fodder.move_front(step_colon_fodder);
		}
		base::slice(self, node, ctx);
	}
}

#[cfg(test)]
mod tests {
	use crate::format_default;

	fn format(input: &str) -> String {
		format_default("t.jsonnet", input).expect("the snippet parses")
	}

	#[test]
	fn a_step_colon_with_nothing_before_it_is_already_gone() {
		// `::` is one operator token, so the parser never fills the slot and
		// `a[::]` is the same tree as `a[:]`. There is nothing for this pass
		// to do, which is why it is written in terms of the fodder rather than
		// in terms of the source.
		assert_eq!(format("a[::]"), "a[:]\n");
		assert_eq!(format("a[1:2:]"), "a[1:2]\n");
		assert_eq!(format("a[1:2:3]"), "a[1:2:3]\n");
	}

	#[test]
	fn a_comment_before_a_redundant_step_colon_survives_it() {
		// Without the pass the colon would be written, because the unparser
		// emits it whenever the slot is non-empty; with it, the comment moves
		// on to the `]` and the colon goes.
		assert_eq!(format("a[1:2 /* c */:]"), "a[1:2/* c */]\n");
	}

	#[test]
	fn a_step_keeps_its_colon_and_its_fodder() {
		assert_eq!(format("a[1:2 /* c */:3]"), "a[1:2/* c */:3]\n");
	}
}
