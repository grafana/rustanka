//! A port of `internal/formatter/add_plus_object.go`: `e { }` is written back
//! as `e + { }`, with parentheses wherever the plus would otherwise reparse
//! differently.
//!
//! The `use_implicit_plus: false` branch of step 7, which means **`tk fmt`
//! never runs it** — `Options::default` takes
//! [`RemovePlusObject`](super::RemovePlusObject) instead. It is here because
//! upstream keeps it and because upstream's own `TestFormatNoImplicitPlus`
//! grades it; those nine cases are `tests/fixtures.rs`'s `no_implicit_plus/`
//! family, and they are the only fixtures in this crate whose failure would
//! change what a file *evaluates to* rather than how it looks.
//!
//! # Why parentheses, and why the side matters
//!
//! Upstream's own worked examples, which are the specification:
//!
//! ```text
//! {a:1} {b:2}          -> no parens; the context is the top level
//! {a:1} {b:2}(42)      -> parens; Apply binds tighter than `+`
//! {a:1} {b:2}.a        -> parens; Index binds tighter than `+`
//! {a:1} + {b:2} {c:3}  -> parens; `+` is left-associative, so the right needs them
//! {a:1} {b:2} + {c:3}  -> no parens; the left does not
//! + 42 {b:2}           -> parens; unary `+` binds tighter than binary `+`
//! ```
//!
//! The last one does not evaluate — a number plus an object is a runtime
//! error — and still has to format, because this is formatting and not
//! evaluation. Trees that parse but mean nothing are in scope.
//!
//! # The context, and why Go's trick does not port
//!
//! This is the one pass that uses `pass.Context`, and Go's is the parent node.
//! It then asks, of an `Apply` or an `Index` parent, `parent.Target == *node`
//! — a **pointer** comparison. Because `node` is `&parent.Target` whenever the
//! walk arrived through that slot, the comparison is an identity check that is
//! trivially true there and false everywhere else. So what it really asks is
//! *which slot of the parent did the walk come through*, and it answers by
//! comparing a place against itself.
//!
//! Rust cannot do that: the parent is mutably borrowed for the whole of its
//! own traversal, so there is no second reference to compare against, and
//! `Node` has no identity apart from its address. `src/pass.rs` scoped the
//! replacement when the trait was written — carry a **descriptor** of the
//! parent, refined per slot — and [`Parent`] is it. The refinement happens by
//! overriding the five node hooks whose slots upstream's switch distinguishes;
//! every other hook keeps the base traversal, and its children get
//! [`Parent::Other`], which is upstream's default branch.
//!
//! Each of those five overrides restates the base traversal because it has to
//! hand a *different* context to different slots, which `pass::base` cannot do
//! — it takes one `ctx` and gives it to everything. That is the cost of not
//! having pointer identity, and it is a real maintenance hazard: an override
//! that forgets a slot silently stops converting the ApplyBraces in it. The
//! guard is the snippets, which put an `e { }` in every slot of all five.
//!
//! # The replacement node is what goes down as the parent
//!
//! `c.Base.Visit(p, node, passCtx{parent: *node})` sits **outside** the `if`,
//! and `*node` is read after the rewrite. So where parens were added the
//! children see the new `Parens` rather than the `Binary`, and where they were
//! not they see the `Binary`. That is not a detail: it is why
//! `{a:1} {b:2} {c:3}.x` comes out as `({a:1} + {b:2} + {c:3}).x` with one
//! pair of parens rather than two. The outer ApplyBrace is an Index target and
//! is wrapped; the inner one is then the new Binary's *left*, and `+` on the
//! left of `+` needs nothing.
//!
//! # Three things about it that are inert or unreachable
//!
//! Each is verified rather than assumed, and each is reproduced anyway so that
//! an upstream change cannot silently diverge here.
//!
//! - **The fodder move is a no-op for every tree that can reach it.** Upstream
//!   builds the `Parens` with `Fodder: binary.NodeBase.Fodder` and then sets
//!   `binary.NodeBase.Fodder = nil`, which looks like the difference between a
//!   comment landing inside or outside the parens. It cannot be: the parser
//!   constructs every `ApplyBrace` with `ast.Fodder{}`, because an ApplyBrace
//!   is left-recursive and its opening fodder is stored on the leftmost leaf
//!   instead — and no earlier pass writes a node's *own* fodder, they all go
//!   through `openFodder`, which walks down that same spine. So both slots are
//!   always empty.
//! - **The `ast.ApplyBrace` parent panics, and is unreachable.** Every node is
//!   reached through `Visit`, and `Visit` replaces an ApplyBrace before
//!   descending, so no child can ever see one as its parent. The panic is
//!   reproduced on [`AddPlusObject::apply_brace`] — one step earlier in the
//!   walk than Go's, but on the same impossible condition and with upstream's
//!   own message.
//! - **The `InSuper` branch is a constant.** It compares
//!   `precedence(in) <= precedence(+)` — 8 against 6 — which is false, so an
//!   `e { } in super` never gets parens. That is the right answer: `+` binds
//!   tighter than `in`, so the parse is unchanged. Written as the comparison
//!   rather than as `false`, because upstream writes it that way.
//!
//! # One upstream gap, reproduced
//!
//! `ast.Slice` is not in the switch, so it takes the default branch and
//! `{a:1} {b:2}[1:2]` is written back as `{a:1} + {b:2}[1:2]` — which parses
//! as `{a:1} + ({b:2}[1:2])`, a different tree. A slice binds exactly as
//! tightly as an index, so this is the same bug the `ast.Index` case exists to
//! avoid, in the one node kind the case does not name. It changes what the
//! file evaluates to, which makes it the most serious of the upstream oddities
//! this port carries — and it is still upstream's answer, so it is what is
//! reproduced. `add_plus_object/slice_target` pins it.

use crate::{
	ast::{
		Apply, ApplyBrace, Binary, BinaryOp, InSuper, Index, Node, NodeKind, Parens, Precedence,
		UNARY_PRECEDENCE, Unary,
	},
	fodder::Fodder,
	pass::{AstPass, base},
};

/// Go's package-level `var plusPrec = iast.BinaryOpPrecedence(ast.BopPlus)`.
const PLUS_PRECEDENCE: Precedence = BinaryOp::Plus.precedence();

/// What the enclosing context says about whether an inserted `+` needs
/// parentheses: go-jsonnet's `passCtx`, refined per slot.
///
/// Go carries the parent node itself and distinguishes the slots by pointer
/// identity. Rust cannot, so each variant names the slot the walk arrived
/// through, and the pass fills it in when it visits that slot. See the module
/// documentation for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parent {
	/// Go's nil parent, from `BaseContext`. The root needs no parentheses.
	None,
	/// Every branch of upstream's switch that answers "no parentheses" without
	/// looking at anything: each node kind it does not name, and each slot of
	/// the ones it does name that the pointer comparison rules out.
	///
	/// Almost all of those slots genuinely delimit the expression, with a
	/// bracket, a comma or a keyword, so the flattened parse is the same one.
	/// `ast.Slice` is the exception and is the upstream gap the module
	/// documentation records.
	Other,
	/// The target of an `ast.Apply`, which binds tighter than `+`.
	ApplyTarget,
	/// The target of an `ast.Index`, which also binds tighter than `+`.
	IndexTarget,
	/// The index of an `ast.InSuper`, which is its only child — the right-hand
	/// side of an `in super` is always exactly `super`.
	InSuperIndex,
	/// The left operand of an `ast.Binary`, carrying that binary's operator.
	BinaryLeft(BinaryOp),
	/// The right operand of an `ast.Binary`, carrying that binary's operator.
	BinaryRight(BinaryOp),
	/// The operand of an `ast.Unary`.
	UnaryOperand,
}

impl Parent {
	/// Upstream's `needsParens`, which is the body of its `switch`.
	///
	/// The two precedence comparisons that are constants are written as
	/// comparisons anyway, because that is how upstream writes them and
	/// because the answer changes if the table ever does.
	fn needs_parens(self) -> bool {
		match self {
			Self::None | Self::Other => false,
			// Apply and Index bind tighter than a Binary, so the target
			// position always needs them. Go reaches this by finding that the
			// parent's target *is* this node.
			Self::ApplyTarget | Self::IndexTarget => true,
			Self::InSuperIndex => BinaryOp::In.precedence() <= PLUS_PRECEDENCE,
			// `+` is left-associative, so an operator of equal precedence needs
			// parentheses on the right and not on the left. That asymmetry is
			// the whole reason the two sides are separate variants.
			Self::BinaryLeft(op) => op.precedence() < PLUS_PRECEDENCE,
			Self::BinaryRight(op) => op.precedence() <= PLUS_PRECEDENCE,
			Self::UnaryOperand => UNARY_PRECEDENCE < PLUS_PRECEDENCE,
		}
	}
}

/// `formatter.AddPlusObject`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AddPlusObject;

impl AstPass for AddPlusObject {
	type Ctx = Parent;

	/// `BaseContext`, which returns an empty `passCtx` — a nil parent.
	fn base_context(&mut self) -> Parent {
		Parent::None
	}

	fn visit(&mut self, node: &mut Node, ctx: &Parent) {
		if matches!(node.kind, NodeKind::ApplyBrace(_)) {
			let NodeKind::ApplyBrace(brace) =
				std::mem::replace(&mut node.kind, NodeKind::LiteralNull)
			else {
				unreachable!("just matched an ApplyBrace")
			};

			// `NodeBase: applyBrace.NodeBase` — the fodder and the location are
			// on `node` here and are simply kept. `OpFodder` is not part of
			// `NodeBase`, so upstream's struct literal leaves it nil.
			node.kind = NodeKind::Binary(Binary {
				left: brace.left,
				op_fodder: Fodder::new(),
				op: BinaryOp::Plus,
				right: brace.right,
			});

			if ctx.needs_parens() {
				// The Parens takes the Binary's fodder and location and the
				// Binary's fodder is cleared: `Fodder: binary.NodeBase.Fodder`
				// then `binary.NodeBase.Fodder = nil`. `node` is the Parens
				// now, so its fodder is already the one being kept, and the
				// inner node starts with an empty one. Both are always empty in
				// practice — see the module documentation.
				let inner = Node::new(
					node.loc.clone(),
					Fodder::new(),
					std::mem::replace(&mut node.kind, NodeKind::LiteralNull),
				);
				node.kind = NodeKind::Parens(Parens {
					inner: Box::new(inner),
					close_fodder: Fodder::new(),
				});
			}
		}

		// `c.Base.Visit(p, node, passCtx{parent: *node})`, outside the `if`, so
		// the parent handed down is the replacement where there was one.
		//
		// What is passed here is what a node kind with no hook of its own gives
		// its children, which is upstream's default branch. The five kinds whose
		// slots upstream distinguishes override their hooks below and build their
		// own contexts from the node they are given.
		base::visit(self, node, &Parent::Other);
	}

	/// Restates `base::apply` so the target gets its own context.
	fn apply(&mut self, node: &mut Apply, _ctx: &Parent) {
		self.visit(&mut node.target, &Parent::ApplyTarget);
		// An argument is delimited by the call's own parentheses, so upstream's
		// pointer comparison fails for it and no parens are added.
		self.arguments(
			&mut node.fodder_left,
			&mut node.arguments,
			&mut node.fodder_right,
			&Parent::Other,
		);
		if node.tail_strict {
			self.fodder(&mut node.tail_strict_fodder, &Parent::Other);
		}
	}

	/// Restates `base::index`, including the slot it does not visit.
	fn index(&mut self, node: &mut Index, _ctx: &Parent) {
		self.visit(&mut node.target, &Parent::IndexTarget);
		self.fodder(&mut node.left_bracket_fodder, &Parent::Other);
		// `right_bracket_fodder` doubles as the fodder before an identifier, and
		// upstream never descends into it in that case. Reproduced here because
		// a restated traversal that quietly visited it would diverge.
		if node.id.is_none() {
			if let Some(index) = &mut node.index {
				// Delimited by the brackets.
				self.visit(index, &Parent::Other);
			}
			self.fodder(&mut node.right_bracket_fodder, &Parent::Other);
		}
	}

	/// Restates `base::binary`, which is where the associativity asymmetry
	/// lands.
	fn binary(&mut self, node: &mut Binary, _ctx: &Parent) {
		let op = node.op;
		self.visit(&mut node.left, &Parent::BinaryLeft(op));
		self.fodder(&mut node.op_fodder, &Parent::Other);
		self.visit(&mut node.right, &Parent::BinaryRight(op));
	}

	/// Restates `base::unary`.
	fn unary(&mut self, node: &mut Unary, _ctx: &Parent) {
		self.visit(&mut node.expr, &Parent::UnaryOperand);
	}

	/// Restates `base::in_super`, which descends into the index and nothing
	/// else — `in_fodder` and `super_fodder` are two of the four slots the
	/// traversal never reaches.
	fn in_super(&mut self, node: &mut InSuper, _ctx: &Parent) {
		self.visit(&mut node.index, &Parent::InSuperIndex);
	}

	/// Unreachable, and upstream panics on the condition that would reach it.
	///
	/// Go's panic is in the *child's* `Visit`, when it finds an `ast.ApplyBrace`
	/// as its parent; this is one step earlier in the same walk, and the
	/// condition is the same one — a node still being an ApplyBrace when the
	/// traversal descends into it. [`AddPlusObject::visit`] replaces every
	/// ApplyBrace it sees before calling the base traversal, and every node is
	/// reached through `visit`, so neither can happen.
	///
	/// # Panics
	///
	/// Always, with upstream's message.
	fn apply_brace(&mut self, _node: &mut ApplyBrace, _ctx: &Parent) {
		panic!("parent implicit-plus should already have been replaced with explicit plus");
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Options, format, parser::snippet_to_raw_ast, pass::visit_file};

	/// Format with `use_implicit_plus` off, which is the only configuration
	/// that runs this pass — and the configuration upstream's own
	/// `TestFormatNoImplicitPlus` uses.
	fn no_implicit_plus(input: &str) -> String {
		let options = Options {
			use_implicit_plus: false,
			..Options::default()
		};
		format("t.jsonnet", input, &options).expect("the snippet parses")
	}

	/// Parse and run only this pass.
	///
	/// The structural assertions go through this rather than through `format`,
	/// because what this pass decides is *where a Parens went* and that is a
	/// fact about the tree. The nine text answers below are upstream's own and
	/// are the only hand-written ones here.
	fn only_this_pass(input: &str) -> Node {
		let (mut node, mut final_fodder) =
			snippet_to_raw_ast("t.jsonnet", input).expect("the snippet parses");
		visit_file(&mut AddPlusObject, &mut node, &mut final_fodder);
		node
	}

	/// The root's kind after this pass alone.
	fn root(input: &str) -> &'static str {
		only_this_pass(input).kind.name()
	}

	#[test]
	fn the_nine_upstream_cases() {
		// `TestFormatNoImplicitPlus`, verbatim. `tests/fixtures.rs` grades the
		// same nine through the quarantine ratchet; they are here as well
		// because they are this pass's specification and a failure should name
		// the pass.
		assert_eq!(
			no_implicit_plus("{ f(x):: x * x } { a: 1 }.f(99)\n"),
			"({ f(x):: x * x } + { a: 1 }).f(99)\n"
		);
		assert_eq!(
			no_implicit_plus("{ a: 1 } { b: 2 }.a\n"),
			"({ a: 1 } + { b: 2 }).a\n"
		);
		assert_eq!(
			no_implicit_plus("{ a: 1 } { b: 2 }\n"),
			"{ a: 1 } + { b: 2 }\n"
		);
		assert_eq!(
			no_implicit_plus("{ a: 1 } { b: 2 } { c: 3 }\n"),
			"{ a: 1 } + { b: 2 } + { c: 3 }\n"
		);
		assert_eq!(
			no_implicit_plus("{ a: 1 } + { b: 2 } { c: 3 }\n"),
			"{ a: 1 } + ({ b: 2 } + { c: 3 })\n"
		);
		assert_eq!(
			no_implicit_plus("{ a: 1 } { b: 2 } + { c: 3 }\n"),
			"{ a: 1 } + { b: 2 } + { c: 3 }\n"
		);
		assert_eq!(no_implicit_plus("+ 42 { b: 2 }\n"), "+(42 + { b: 2 })\n");
		assert_eq!(
			no_implicit_plus("{ a: 1 } * { b: 2 } { c: 3 }\n"),
			"{ a: 1 } * ({ b: 2 } + { c: 3 })\n"
		);
		assert_eq!(
			no_implicit_plus("{ a: 1 } { b: 2 } * { c: 3 }\n"),
			"({ a: 1 } + { b: 2 }) * { c: 3 }\n"
		);
	}

	#[test]
	fn the_replacement_is_what_goes_down_as_the_parent() {
		// The outer ApplyBrace is an Index target and is wrapped; the inner one
		// is then the *new Binary's* left, where `+` needs nothing. So one pair
		// of parens and not two, which is what `passCtx{parent: *node}` being
		// read after the rewrite buys.
		let node = only_this_pass("{ a: 1 } { b: 2 } { c: 3 }.x");
		let NodeKind::Index(index) = &node.kind else {
			panic!("the root is not an Index")
		};
		let NodeKind::Parens(parens) = &index.target.kind else {
			panic!("the Index target was not wrapped")
		};
		let NodeKind::Binary(outer) = &parens.inner.kind else {
			panic!("the Parens does not hold a Binary")
		};
		// Not a Parens: the inner ApplyBrace saw the Binary as its parent.
		assert_eq!(outer.left.kind.name(), "Binary");
	}

	#[test]
	fn every_apply_brace_is_replaced_whatever_slot_it_is_in() {
		// The guard on the five restated traversals: an override that forgot a
		// slot would leave an ApplyBrace behind unvisited, and this pass must
		// leave none anywhere. Asserted structurally rather than on the text,
		// because the right text differs per slot and there are eighteen of
		// them.
		for input in [
			"f({ a: 1 } { b: 2 })",
			"f(x={ a: 1 } { b: 2 })",
			"f({ a: 1 } { b: 2 }) tailstrict",
			"a[{ b: 1 } { c: 2 }]",
			"{ a: 1 } { b: 2 }.a",
			"{ a: 1 } { b: 2 } + { c: 3 }",
			"{ a: 1 } + { b: 2 } { c: 3 }",
			"!a { b: 2 }",
			"{ x: { a: 1 } { b: 2 } in super }",
			"{ a: 1 } { b: 2 }[1:2]",
			"[{ a: 1 } { b: 2 }]",
			"{ x: { a: 1 } { b: 2 } }",
			"local x = { a: 1 } { b: 2 }; x",
			"if c then { a: 1 } { b: 2 } else 1",
			"error { a: 1 } { b: 2 }",
			"function(x={ a: 1 } { b: 2 }) x",
			"[x for x in { a: 1 } { b: 2 }]",
			"{ [k]: { a: 1 } { b: 2 } for k in y }",
		] {
			assert!(
				!contains_apply_brace(&only_this_pass(input)),
				"an ApplyBrace survived in {input:?}"
			);
		}
	}

	/// Whether any ApplyBrace is left in the tree.
	///
	/// Deliberately a *separate* pass over `pass::base`, and that is the point:
	/// if one of the five restated traversals dropped a slot, this pass would
	/// never descend into it, so it would never reach the ApplyBrace there and
	/// `apply_brace`'s panic would never fire either. A walk that does not
	/// share the override is the only thing that can see that.
	fn contains_apply_brace(node: &Node) -> bool {
		#[derive(Default)]
		struct Counter {
			found: bool,
		}
		impl AstPass for Counter {
			type Ctx = ();
			fn base_context(&mut self) {}
			fn visit(&mut self, node: &mut Node, ctx: &()) {
				if matches!(node.kind, NodeKind::ApplyBrace(_)) {
					self.found = true;
				}
				base::visit(self, node, ctx);
			}
		}

		let mut copy = node.clone();
		let mut final_fodder = Fodder::new();
		let mut counter = Counter::default();
		visit_file(&mut counter, &mut copy, &mut final_fodder);
		counter.found
	}

	#[test]
	fn an_unnamed_parent_kind_gets_no_parens() {
		// Every default-branch parent. The root is the Binary the ApplyBrace
		// became, which is what says no Parens was inserted around it.
		assert_eq!(root("{ a: 1 } { b: 2 }"), "Binary");
		assert_eq!(root("({ a: 1 } { b: 2 })"), "Parens");
	}

	#[test]
	fn a_slice_target_is_upstreams_gap() {
		// ast.Slice is not in the switch, so no parens are added although a
		// slice binds as tightly as an index — and the formatted expression
		// therefore parses differently from the one that went in. Reproduced,
		// not fixed; see the module documentation.
		let node = only_this_pass("{ a: 1 } { b: 2 }[1:2]");
		let NodeKind::Slice(slice) = &node.kind else {
			panic!("the root is not a Slice")
		};
		assert_eq!(slice.target.kind.name(), "Binary");
	}

	#[test]
	fn in_super_never_needs_parens() {
		// 8 <= 6 is false, and correctly so: `+` binds tighter than `in`.
		assert!(!Parent::InSuperIndex.needs_parens());

		let node = only_this_pass("{ x: { a: 1 } { b: 2 } in super }");
		let NodeKind::Object(object) = &node.kind else {
			panic!("the root is not an Object")
		};
		let value = object.fields[0]
			.expr2
			.as_deref()
			.expect("the field has a value");
		let NodeKind::InSuper(in_super) = &value.kind else {
			panic!("the field value is not an InSuper")
		};
		assert_eq!(in_super.index.kind.name(), "Binary");
	}

	#[test]
	fn the_two_binary_sides_disagree_at_equal_precedence() {
		// The whole of the associativity story, as a table over the operators
		// that straddle `+`.
		for op in [BinaryOp::Plus, BinaryOp::Minus] {
			assert!(!Parent::BinaryLeft(op).needs_parens(), "{}", op.as_str());
			assert!(Parent::BinaryRight(op).needs_parens(), "{}", op.as_str());
		}
		for op in [BinaryOp::Mult, BinaryOp::Div, BinaryOp::Percent] {
			assert!(Parent::BinaryLeft(op).needs_parens(), "{}", op.as_str());
			assert!(Parent::BinaryRight(op).needs_parens(), "{}", op.as_str());
		}
		for op in [BinaryOp::ShiftL, BinaryOp::In, BinaryOp::And, BinaryOp::Or] {
			assert!(!Parent::BinaryLeft(op).needs_parens(), "{}", op.as_str());
			assert!(!Parent::BinaryRight(op).needs_parens(), "{}", op.as_str());
		}
	}

	#[test]
	fn a_unary_parent_always_needs_parens() {
		assert!(Parent::UnaryOperand.needs_parens());
		for input in [
			"+42 { b: 2 }\n",
			"-a { b: 2 }\n",
			"!a { b: 2 }\n",
			"~a { b: 2 }\n",
		] {
			let out = no_implicit_plus(input);
			assert!(out.contains('('), "{input:?} formatted to {out:?}");
		}
	}

	#[test]
	fn the_default_options_never_run_this_pass() {
		// `tk fmt` takes RemovePlusObject, so `{ a: 1 } { b: 2 }` keeps its
		// implicit plus. If this ever starts asserting the other answer, the
		// corpus has moved and something is wired wrong.
		assert_eq!(
			format("t.jsonnet", "{ a: 1 } { b: 2 }\n", &Options::default()).expect("parses"),
			"{ a: 1 } { b: 2 }\n"
		);
	}
}
