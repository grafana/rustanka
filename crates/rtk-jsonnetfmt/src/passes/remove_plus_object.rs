//! A port of `internal/formatter/remove_plus_object.go`: `a + { b: 1 }` is
//! written back as `a { b: 1 }`.
//!
//! Step 7 of `FormatNode` under `Options::default`, where `use_implicit_plus`
//! is on. [`AddPlusObject`](super::AddPlusObject) is the other branch of the
//! same `if` and is what `tk fmt` never runs.
//!
//! It is the only pass in the pipeline that *creates* an
//! [`ApplyBrace`](crate::ast::ApplyBrace), and the unparser writes one with no
//! operator between its two halves — so the `+` disappears and the fodder that
//! sat in front of it has to go somewhere. It goes to the front of the
//! object's own fodder, which is why `a\n+ { b: 1 }` keeps its line break and
//! comes back as `a\n{ b: 1 }`.
//!
//! # The left-hand side has to be a `Var` or an `Index`, and that is all
//!
//! Upstream says so in a comment — "Could relax this to allow more ASTs on the
//! LHS but this seems OK for now" — so this is a deliberate restriction rather
//! than an approximation of one, and it is easy to over-generalise. In
//! particular `{ a: 1 } + { b: 2 }` is **not** rewritten, which is visible in
//! `tests/golden/string_object_extend.jsonnet`: the `s + { a: 1 }` on one line
//! loses its `+` and the `{ c: 3 } { d: 4 }` on the next keeps its shape.
//!
//! Three node kinds that look like they should qualify and do not, because Go
//! types each of them separately:
//!
//! - `a[1:2] + { b: 1 }` — `ast.Slice`, not `ast.Index`.
//! - `super.a + { b: 1 }` — `ast.SuperIndex`.
//! - `f() + { a: 1 }` — `ast.Apply`. But `f().a + { b: 1 }` **is** rewritten,
//!   because the test is on the outermost node of the left side.
//!
//! And one on the right: `a + { [k]: 1 for k in x }` is not rewritten either,
//! because an object comprehension is `ast.ObjectComp`. Each of these has a
//! snippet, and the negatives are the ones worth having.

use crate::{
	ast::{ApplyBrace, Binary, BinaryOp, Node, NodeKind},
	pass::{AstPass, base},
};

/// `formatter.RemovePlusObject`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RemovePlusObject;

impl AstPass for RemovePlusObject {
	type Ctx = ();

	fn base_context(&mut self) {}

	fn visit(&mut self, node: &mut Node, ctx: &()) {
		if Self::is_implicit_plus_candidate(node) {
			let NodeKind::Binary(mut binary) =
				std::mem::replace(&mut node.kind, NodeKind::LiteralNull)
			else {
				unreachable!("just matched a Binary")
			};

			// `FodderMoveFront(&rhs.Fodder, &binary.OpFodder)`: the fodder
			// before the `+` becomes the fodder before the `{`, since the `+`
			// is about to stop being written.
			let Binary {
				op_fodder, right, ..
			} = &mut binary;
			right.fodder.move_front(op_fodder);

			// The replacement keeps the Binary's own fodder and location, which
			// is `NodeBase: binary.NodeBase`. Here they live on `node` and are
			// simply not touched.
			node.kind = NodeKind::ApplyBrace(ApplyBrace {
				left: binary.left,
				right: binary.right,
			});
		}

		base::visit(self, node, ctx);
	}
}

impl RemovePlusObject {
	/// Whether this node is the `<var-or-index> + <object>` upstream rewrites.
	///
	/// Split out because the replacement needs to *own* the Binary's payload to
	/// move its two children into an [`ApplyBrace`], and it cannot be owned
	/// while `node.kind` still holds it — so the test has to be a separate,
	/// borrowing pass over the same node. Go asks all three questions inside
	/// nested `if`s on a pointer it never gives up.
	fn is_implicit_plus_candidate(node: &Node) -> bool {
		let NodeKind::Binary(binary) = &node.kind else {
			return false;
		};
		binary.op == BinaryOp::Plus
			&& matches!(binary.left.kind, NodeKind::Var(_) | NodeKind::Index(_))
			&& matches!(binary.right.kind, NodeKind::Object(_))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Options, format, parser::snippet_to_raw_ast, pass::visit_file};

	/// Parse, and optionally run the pass.
	fn parse(input: &str, run: bool) -> Node {
		let (mut node, mut final_fodder) =
			snippet_to_raw_ast("t.jsonnet", input).expect("the snippet parses");
		if run {
			visit_file(&mut RemovePlusObject, &mut node, &mut final_fodder);
		}
		node
	}

	/// The kind of the root after the pass, which is this pass's whole
	/// postcondition: `Binary` means it was refused and `ApplyBrace` means it
	/// fired.
	fn root(input: &str) -> &'static str {
		parse(input, true).kind.name()
	}

	#[test]
	fn a_var_or_an_index_on_the_left_is_rewritten() {
		assert_eq!(root("a + { b: 1 }"), "ApplyBrace");
		assert_eq!(root("a.b + { c: 1 }"), "ApplyBrace");
		assert_eq!(root("a['b'] + { c: 1 }"), "ApplyBrace");
		// The test is on the outermost node of the left side, so an Index over
		// an Apply qualifies although a bare Apply does not.
		assert_eq!(root("f().a + { b: 1 }"), "ApplyBrace");
	}

	#[test]
	fn anything_else_on_the_left_is_refused() {
		// Upstream's own restriction: "Could relax this to allow more ASTs on
		// the LHS but this seems OK for now." Over-generalising here would
		// change what the golden for string_object_extend.jsonnet says.
		assert_eq!(root("{ a: 1 } + { b: 2 }"), "Binary");
		assert_eq!(root("f() + { a: 1 }"), "Binary");
		assert_eq!(root("(a) + { b: 1 }"), "Binary");
		// ast.Slice and ast.SuperIndex are their own Go types, not ast.Index.
		assert_eq!(root("a[1:2] + { b: 1 }"), "Binary");
	}

	#[test]
	fn the_right_hand_side_has_to_be_a_plain_object() {
		assert_eq!(root("a + [1]"), "Binary");
		assert_eq!(root("a + ({ b: 1 })"), "Binary");
		// An object comprehension is ast.ObjectComp, a different type.
		assert_eq!(root("a + { [k]: 1 for k in x }"), "Binary");
	}

	#[test]
	fn the_operator_has_to_be_plus() {
		assert_eq!(root("a - { b: 1 }"), "Binary");
		assert_eq!(root("a * { b: 1 }"), "Binary");
	}

	#[test]
	fn the_op_fodder_moves_on_to_the_object() {
		// The `+` stops being written, so anything in front of it would be
		// lost. Asserted as "which slot holds it", which is the pass's claim —
		// not as rendered text, which is the unparser's.
		let before = parse("a /* c */ + { b: 1 }", false);
		let NodeKind::Binary(binary) = &before.kind else {
			panic!("not a Binary")
		};
		assert_eq!(binary.op_fodder.describe().len(), 1);
		assert!(binary.right.fodder.is_empty());

		let after = parse("a /* c */ + { b: 1 }", true);
		let NodeKind::ApplyBrace(brace) = &after.kind else {
			panic!("not an ApplyBrace")
		};
		assert_eq!(brace.right.fodder.describe(), binary.op_fodder.describe());
	}

	#[test]
	fn a_line_end_on_each_side_of_the_seam_merges() {
		// Both slots hold a LineEnd, so the move goes through `FodderAppend`'s
		// rule that a LineEnd may not follow a LineEnd — one element, not two.
		let after = parse("a\n+\n{ b: 1 }", true);
		let NodeKind::ApplyBrace(brace) = &after.kind else {
			panic!("not an ApplyBrace")
		};
		assert_eq!(brace.right.fodder.len(), 1);
	}

	#[test]
	fn a_chain_converts_only_its_innermost_link() {
		// `+` is left-associative, so the outer Binary's left is a Binary and
		// is refused, while the inner one fires. This is exactly the shape the
		// docsonnet corpus file has.
		let after = parse("a + { b: 1 } + { c: 2 }", true);
		let NodeKind::Binary(outer) = &after.kind else {
			panic!("the outer node should still be a Binary")
		};
		assert_eq!(outer.left.kind.name(), "ApplyBrace");
	}

	#[test]
	fn the_walk_continues_into_the_replacement() {
		// `c + { d: 1 }` is inside the object that just became an ApplyBrace's
		// right-hand side, and it has to be rewritten too.
		let after = parse("a + { b: c + { d: 1 } }", true);
		let NodeKind::ApplyBrace(brace) = &after.kind else {
			panic!("not an ApplyBrace")
		};
		let NodeKind::Object(object) = &brace.right.kind else {
			panic!("the right-hand side is not an Object")
		};
		let value = object.fields[0]
			.expr2
			.as_deref()
			.expect("the field has a value");
		assert_eq!(value.kind.name(), "ApplyBrace");
	}

	#[test]
	fn the_whole_pipeline_drops_the_operator() {
		// One pipeline answer, kept separate from the postcondition tests
		// above. This is what `tk fmt` prints, and the same rewrite is what
		// takes tests/golden/string_object_extend.jsonnet green.
		let options = Options::default();
		assert_eq!(
			format("t.jsonnet", "local s = 'x';\ns + { a: 1 }\n", &options).expect("parses"),
			"local s = 'x';\ns { a: 1 }\n"
		);
		assert_eq!(
			format("t.jsonnet", "{ c: 3 } + { d: 4 }\n", &options).expect("parses"),
			"{ c: 3 } + { d: 4 }\n"
		);
	}
}
