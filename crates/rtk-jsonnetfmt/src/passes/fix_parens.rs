//! A port of `internal/formatter/fix_parens.go`: `((e))` becomes `(e)`.
//!
//! Step 6 of `FormatNode`, and the first of Phase 2e's three. It is also the
//! first pass in this crate that changes the *structure* of the tree rather
//! than its fodder, which is what makes this phase different: the two passes
//! after it can change what a file evaluates to, and this one is the reason
//! the parens they insert are never collapsed again — it runs before them and
//! `FormatNode` never comes back.
//!
//! # Overriding `visit` rather than `parens`
//!
//! Go overrides `Parens(p, node *ast.Parens, ctx)`, and its `*ast.Parens`
//! embeds `NodeBase` — so it can reach the node's own fodder, which the pass
//! needs for `FodderMoveFront(openFodder(node), …)`. [`crate::ast`] keeps that
//! fodder on [`Node`] instead of in every variant (see that module's second
//! departure), so [`crate::pass::AstPass::parens`] is handed a
//! [`Parens`](crate::ast::Parens) that does not carry it and cannot do the
//! job. This overrides [`crate::pass::AstPass::visit`], which does get the
//! whole node.
//!
//! The move is behaviour-identical. `Base.Visit` visits the node's open fodder
//! and *then* dispatches to `p.Parens`, so upstream's collapse happens after
//! that fodder visit and this one happens before it — but `FixParens` does not
//! override the fodder hooks at all, so the base traversal over fodder is a
//! no-op either way and nothing can observe the difference. The resulting tree
//! is what the oracle grades, and it is the same tree.
//!
//! `openFodder(node)` is likewise exactly `node.fodder` here:
//! `leftRecursive` has no `*ast.Parens` case, so `leftRecursiveDeep` on a
//! Parens returns the Parens. [`Node::opening_fodder_mut`] is still what is
//! called, because that is the function upstream calls.
//!
//! # It is an `if`, not a loop, and so `jsonnetfmt` is not a fixed point
//!
//! The second non-convergence this port has found, after Phase 2c's — an
//! index whose field name is written with a unicode escape, which
//! `PrettyFieldNames` at step 10 refuses to promote and `EnforceStringStyle`
//! at step 11 then unescapes. Upstream splices out **one** level of parens and
//! then hands the walk the node that took the inner one's place:
//!
//! ```text
//! (((1)))  ->  ((1))  ->  (1)
//! ```
//!
//! `P1.Inner` becomes `P3`, and `Base.Parens` then visits `P3` — whose inner
//! is a literal — rather than re-examining `P1`. So four levels halve to two
//! and three become two. This is what `tk fmt` prints, it is pinned by the
//! `fix_parens/triple` and `fix_parens/quad` snippets, and it must **not** be
//! answered with a convergence loop: `rtk fmt` matching `tk fmt` on one run is
//! the contract, and iterating would diverge on the first file in a Grafana
//! repo that has a doubly-parenthesised expression.
//!
//! No corpus golden reaches it, which is why `tests/corpus.rs`'s idempotence
//! allow-list is still empty.
//!
//! # Where the fodder goes, which is outward
//!
//! Both moves are `FodderMoveFront`, so the inner Parens' fodder is
//! *prepended* to the outer's rather than appended. A comment written between
//! the two `(` therefore comes out in front of the surviving one:
//!
//! ```text
//! ( /* a */ (1))   ->   /* a */ (1)
//! ```
//!
//! and with both slots occupied the two comments swap order, because the
//! inner's goes in front:
//!
//! ```text
//! /* x */ ( /* a */ (1))   ->   /* a */ /* x */ (1)
//! ```
//!
//! Upstream's, not a port artefact, and pinned by two snippets.

use crate::{
	ast::{Node, NodeKind},
	pass::{AstPass, base},
};

/// `formatter.FixParens`.
#[derive(Debug, Default, Clone, Copy)]
pub struct FixParens;

impl AstPass for FixParens {
	type Ctx = ();

	fn base_context(&mut self) {}

	fn visit(&mut self, node: &mut Node, ctx: &()) {
		// Upstream's `innerParens, ok := node.Inner.(*ast.Parens)`, in the one
		// shape Rust will give: the payload has to be *owned* to move the
		// grandchild box out of it, and it cannot be owned while `node.kind`
		// still holds it.
		let redundant = matches!(&node.kind, NodeKind::Parens(outer)
			if matches!(outer.inner.kind, NodeKind::Parens(_)));

		if redundant {
			// `LiteralNull` is a placeholder for the two statements it takes to
			// put the rebuilt Parens back; nothing observes it.
			let NodeKind::Parens(mut outer) =
				std::mem::replace(&mut node.kind, NodeKind::LiteralNull)
			else {
				unreachable!("just matched a Parens")
			};
			let inner_node = *outer.inner;
			let mut inner_fodder = inner_node.fodder;
			let NodeKind::Parens(inner) = inner_node.kind else {
				unreachable!("just matched a Parens inside a Parens")
			};
			let mut inner_close_fodder = inner.close_fodder;

			// `node.Inner = innerParens.Inner`, so the grandchild takes the
			// inner Parens' place and the inner Parens is gone.
			outer.inner = inner.inner;
			outer.close_fodder.move_front(&mut inner_close_fodder);
			node.kind = NodeKind::Parens(outer);

			// `FodderMoveFront(openFodder(node), &innerParens.Fodder)`. Upstream
			// does this before the close-fodder move; the two slots are
			// disjoint, so the order cannot matter.
			node.opening_fodder_mut().move_front(&mut inner_fodder);
		}

		base::visit(self, node, ctx);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Options, format, parser::snippet_to_raw_ast, pass::visit_file};

	/// Parse, and optionally run the pass.
	///
	/// Nothing below asserts on formatted text, and that is 2d's lesson applied:
	/// a hand-written *answer* is what this project has measured itself
	/// unreliable at, and how a comment is spaced on the page is the unparser's
	/// business rather than this pass's. Every assertion here is the pass's own
	/// postcondition, read off the tree. The one pipeline answer gets a test
	/// that says it is one.
	fn parse(input: &str, run: bool) -> Node {
		let (mut node, mut final_fodder) =
			snippet_to_raw_ast("t.jsonnet", input).expect("the snippet parses");
		if run {
			visit_file(&mut FixParens, &mut node, &mut final_fodder);
		}
		node
	}

	/// How deep the Parens nesting is at the root.
	fn depth(input: &str) -> usize {
		let node = parse(input, true);
		let mut levels = 0;
		let mut cursor = &node;
		while let NodeKind::Parens(parens) = &cursor.kind {
			levels += 1;
			cursor = &parens.inner;
		}
		levels
	}

	/// The root Parens' three fodder slots, in the notation the fodder oracles
	/// compare in: its own, its inner node's, and the one before its `)`.
	fn slots(input: &str, run: bool) -> (Vec<String>, Vec<String>, Vec<String>) {
		let node = parse(input, run);
		let NodeKind::Parens(parens) = &node.kind else {
			panic!("the root of {input:?} is not a Parens")
		};
		(
			node.fodder.describe(),
			parens.inner.fodder.describe(),
			parens.close_fodder.describe(),
		)
	}

	#[test]
	fn one_pair_of_parens_is_left_alone() {
		assert_eq!(depth("1"), 0);
		assert_eq!(depth("(1)"), 1);
	}

	#[test]
	fn a_redundant_pair_goes() {
		assert_eq!(depth("((1))"), 1);
	}

	#[test]
	fn only_one_level_goes_per_run() {
		// The `if` is not a loop, and the walk continues from the node that
		// replaced the inner Parens rather than re-examining the outer one. So
		// three levels become two and four halve to two, and `jsonnetfmt` is a
		// fixed point on neither. Do **not** answer this with a convergence
		// loop — see the module documentation.
		assert_eq!(depth("(((1)))"), 2);
		assert_eq!(depth("((((1))))"), 2);
		assert_eq!(depth("(((((1)))))"), 3);
	}

	#[test]
	fn the_inner_parens_fodder_moves_to_the_outer_node() {
		// Before: the comment between the two `(` is the inner Parens' own
		// fodder, so the root's is empty. After: it is the root's, which puts
		// it outside the surviving paren.
		let (before_open, before_inner, _) = slots("( /* a */ (1))", false);
		assert!(before_open.is_empty());
		assert_eq!(before_inner.len(), 1);

		let (after_open, after_inner, _) = slots("( /* a */ (1))", true);
		assert_eq!(after_open, before_inner);
		// The inner is now the literal, which carried nothing.
		assert!(after_inner.is_empty());
	}

	#[test]
	fn the_move_is_to_the_front_so_two_comments_swap() {
		// `FodderMoveFront` prepends, so the inner's fodder lands *before* the
		// outer's although it was written after it. Asserting the order of the
		// two elements says that without saying anything about spacing.
		let (open, _, _) = slots("/* x */ ( /* a */ (1))", true);
		assert_eq!(open.len(), 2);
		assert!(open[0].contains("/* a */"), "{open:?}");
		assert!(open[1].contains("/* x */"), "{open:?}");
	}

	#[test]
	fn the_close_fodder_is_moved_the_same_way_round() {
		let (_, _, close) = slots("((1 /* a */) /* b */)", true);
		assert_eq!(close.len(), 2);
		assert!(close[0].contains("/* a */"), "{close:?}");
		assert!(close[1].contains("/* b */"), "{close:?}");
	}

	#[test]
	fn an_inner_that_is_not_a_parens_is_left_alone() {
		// The outer's inner is a Binary whose *left* is a Parens, which is not
		// the shape this pass matches.
		assert_eq!(slots("((1) + 2)", false), slots("((1) + 2)", true));
	}

	#[test]
	fn the_whole_pipeline_collapses_one_level_too() {
		// Deliberately separate from everything above: this is `format`'s
		// answer with every landed pass running, which is what `tk fmt` prints.
		// Phase 2c's warning is that a whole-pipeline assertion written while
		// the pipeline is half-built records the half-built answer, so it is
		// one test and it says which question it answers.
		let out = format("t.jsonnet", "((1))", &Options::default()).expect("parses");
		assert_eq!(out, "(1)\n");
	}
}
