//! A port of `internal/formatter/fix_newlines.go`.
//!
//! The pass that decides whether a structure is *expanded* or not. The
//! principle, in upstream's words: a structure either contains newlines in all
//! of its designated places or in none of them. So one element pushed onto its
//! own line pulls every sibling — and the closing bracket — onto theirs.
//!
//! # It looks only one level down
//!
//! Each node asks `FodderCountNewlines` of its own designated slots and of
//! nothing deeper, which is why a multi-line object inside a one-line array
//! leaves that array alone:
//!
//! ```jsonnet
//! [{
//!   a: 'b',
//!   c: 'd',
//! }]
//! ```
//!
//! There is no newline between the `[` and the `{`, nor before the `]`, so the
//! array stays unexpanded however tall its single element is. That is
//! upstream's own doc-comment example and the snippet
//! `fix_newlines/array_shallow_over_a_nested_object` pins it.
//!
//! # Counting newlines is not counting elements
//!
//! `FodderCountNewlines` scores an interstitial **0**, a line end 1, and a
//! paragraph its comment lines plus its blanks. So `[/* a */ 1, /* b */ 2]` is
//! not expanded — a comment within a line is not a newline — while
//! `[1,  // c` … `2]` is, because a trailing `//` comment is a paragraph and a
//! paragraph always scores at least 1.
//!
//! # `ensure_clean_newline` appends rather than replacing
//!
//! `FodderEnsureCleanNewline` is a no-op on fodder that already ends in a line
//! end or a paragraph, and otherwise appends a bare `LineEnd(0, 0)`. On fodder
//! ending in an interstitial that means the comment keeps its place and
//! whatever follows moves below it — the indent is then `FixIndentation`'s
//! business, not this pass's.
//!
//! # The two argument-list flags are not one flag
//!
//! [`FixNewlines::parameters`] and [`FixNewlines::arguments`] each carry
//! `shouldExpandBetween` and `shouldExpandNearParens`, and they answer
//! different questions:
//!
//! | source | flag | result |
//! | --- | --- | --- |
//! | `f(` … `1, 2, 3)` | near parens | the `)` moves down; `2` and `3` stay |
//! | `f(1,` … `2, 3)` | between | `2` and `3` line up; `1` and `)` stay |
//! | `f(1, 2, 3` … `)` | near parens | the first argument moves down |
//!
//! A newline before the *first* item, or before the closing paren, is "near
//! parens"; a newline before any later item is "between". Both comments in
//! upstream spell the two cases out with examples, and the snippets carry all
//! three rows.
//!
//! One detail that is easy to lose: in [`FixNewlines::arguments`] the `first`
//! flag is **shared across the positional and named loops**, in both the
//! measuring pass and the rewriting one. So in `f(1,` … `a=2)` the positional
//! argument consumes `first` and the named `a` counts as a later item, setting
//! `between`; in `f(` … `1, a=2)` the positional is first and sets `near`
//! instead. Two separate flags would get both rows wrong.
//!
//! # `Local` measures every bind and rewrites all but the first
//!
//! `shouldExpand` is computed over every bind, the first included, and the
//! rewrite is guarded by `i > 0`. So `local` … `x = 1, y = 2;` expands `y`
//! from a newline that belongs to `x`, and leaves `x` exactly as written. A
//! `local` with one bind and a newline before it is therefore a real no-op
//! rather than a case with nothing to expand — the snippet
//! `fix_newlines/local_single_bind_is_a_no_op` is there to say which.
//!
//! It also measures only `VarFodder`. A newline before a bind's `=` or before
//! its body is not a reason to expand anything.
//!
//! # What grades it
//!
//! `testdata/pass-snippets.json`, almost alone. The pass oracle puts this pass
//! at **1 changed cell of 138** corpus files, and at 1 of the 113 snippets
//! that existed before Phase 2d — and that one was an accident of the
//! trailing-comma group rather than a case written for it.

use crate::{
	ast::{Arguments, Array, ArrayComp, ForSpec, Local, Object, ObjectComp, Parameter, Parens},
	fodder::Fodder,
	pass::{AstPass, base},
};

/// `formatter.shouldExpandSpec`: does any `for` or `if` of this comprehension
/// start on a new line?
///
/// A free function, as upstream has it, and not a method on [`FixNewlines`]
/// (which is stateless, so `self` would go unread) nor on [`ForSpec`]. Its
/// primary state is a `ForSpec`, but what it expresses is *this pass's*
/// policy: `FixIndentation::specs` walks the same nesting to a different rule
/// and lives on its own pass.
///
/// `ForSpec` nests the earlier `for` in [`ForSpec::outer`], so this recurses
/// outward before testing its own slots — which means one newline anywhere in
/// a chain of comprehension clauses expands the whole chain.
/// Upstream writes this as three sequential `if … { shouldExpand = true }`
/// blocks with no early exit. Nothing here has a side effect, so `||` is the
/// same function; short-circuiting only skips reads.
fn should_expand_spec(spec: &ForSpec) -> bool {
	spec.outer.as_deref().is_some_and(should_expand_spec)
		|| spec.for_fodder.count_newlines() > 0
		|| spec
			.conditions
			.iter()
			.any(|condition| condition.if_fodder.count_newlines() > 0)
}

/// `formatter.ensureSpecExpanded`: put every `for` and `if` of this
/// comprehension on a line of its own.
fn ensure_spec_expanded(spec: &mut ForSpec) {
	if let Some(outer) = spec.outer.as_deref_mut() {
		ensure_spec_expanded(outer);
	}
	spec.for_fodder.ensure_clean_newline();
	for condition in &mut spec.conditions {
		condition.if_fodder.ensure_clean_newline();
	}
}

/// `formatter.FixNewlines`.
#[derive(Debug, Default, Clone, Copy)]
pub struct FixNewlines;

impl AstPass for FixNewlines {
	type Ctx = ();

	fn base_context(&mut self) {}

	fn array(&mut self, node: &mut Array, ctx: &()) {
		let should_expand = node
			.elements
			.iter()
			.any(|element| element.expr.opening_fodder().count_newlines() > 0)
			|| node.close_fodder.count_newlines() > 0;

		if should_expand {
			for element in &mut node.elements {
				element.expr.opening_fodder_mut().ensure_clean_newline();
			}
			node.close_fodder.ensure_clean_newline();
		}

		base::array(self, node, ctx);
	}

	fn object(&mut self, node: &mut Object, ctx: &()) {
		let should_expand = node
			.fields
			.iter()
			.any(|field| field.open_fodder_newlines() > 0)
			|| node.close_fodder.count_newlines() > 0;

		if should_expand {
			for field in &mut node.fields {
				if let Some(fodder) = field.open_fodder_mut() {
					fodder.ensure_clean_newline();
				}
			}
			node.close_fodder.ensure_clean_newline();
		}

		base::object(self, node, ctx);
	}

	fn local(&mut self, node: &mut Local, ctx: &()) {
		// Measured over every bind, the first included.
		let should_expand = node
			.binds
			.iter()
			.any(|bind| bind.var_fodder.count_newlines() > 0);

		if should_expand {
			// Rewritten from the second onwards: Go's `if i > 0`.
			for bind in node.binds.iter_mut().skip(1) {
				bind.var_fodder.ensure_clean_newline();
			}
		}

		base::local(self, node, ctx);
	}

	fn array_comp(&mut self, node: &mut ArrayComp, ctx: &()) {
		let should_expand = node.body.opening_fodder().count_newlines() > 0
			|| should_expand_spec(&node.spec)
			|| node.close_fodder.count_newlines() > 0;

		if should_expand {
			node.body.opening_fodder_mut().ensure_clean_newline();
			ensure_spec_expanded(&mut node.spec);
			node.close_fodder.ensure_clean_newline();
		}

		base::array_comp(self, node, ctx);
	}

	fn object_comp(&mut self, node: &mut ObjectComp, ctx: &()) {
		let should_expand = node
			.fields
			.iter()
			.any(|field| field.open_fodder_newlines() > 0)
			|| should_expand_spec(&node.spec)
			|| node.close_fodder.count_newlines() > 0;

		if should_expand {
			for field in &mut node.fields {
				if let Some(fodder) = field.open_fodder_mut() {
					fodder.ensure_clean_newline();
				}
			}
			ensure_spec_expanded(&mut node.spec);
			node.close_fodder.ensure_clean_newline();
		}

		base::object_comp(self, node, ctx);
	}

	fn parens(&mut self, node: &mut Parens, ctx: &()) {
		let should_expand = node.inner.opening_fodder().count_newlines() > 0
			|| node.close_fodder.count_newlines() > 0;

		if should_expand {
			node.inner.opening_fodder_mut().ensure_clean_newline();
			node.close_fodder.ensure_clean_newline();
		}

		base::parens(self, node, ctx);
	}

	fn parameters(
		&mut self,
		left: &mut Fodder,
		params: &mut Vec<Parameter>,
		right: &mut Fodder,
		ctx: &(),
	) {
		let mut expand_between = false;
		let mut expand_near_parens = false;

		let mut first = true;
		for param in &*params {
			if param.name_fodder.count_newlines() > 0 {
				if first {
					expand_near_parens = true;
				} else {
					expand_between = true;
				}
			}
			first = false;
		}
		if right.count_newlines() > 0 {
			expand_near_parens = true;
		}

		let mut first = true;
		for param in &mut *params {
			if (first && expand_near_parens) || (!first && expand_between) {
				param.name_fodder.ensure_clean_newline();
			}
			first = false;
		}
		if expand_near_parens {
			right.ensure_clean_newline();
		}

		base::parameters(self, left, params, right, ctx);
	}

	fn arguments(&mut self, left: &mut Fodder, args: &mut Arguments, right: &mut Fodder, ctx: &()) {
		let mut expand_between = false;
		let mut expand_near_parens = false;

		// `first` spans both loops, in the measuring pass and in the rewrite.
		// A positional argument therefore consumes it and the first *named*
		// argument counts as a later item — see the module documentation.
		let mut first = true;
		for arg in &args.positional {
			if arg.expr.opening_fodder().count_newlines() > 0 {
				if first {
					expand_near_parens = true;
				} else {
					expand_between = true;
				}
			}
			first = false;
		}
		for arg in &args.named {
			if arg.name_fodder.count_newlines() > 0 {
				if first {
					expand_near_parens = true;
				} else {
					expand_between = true;
				}
			}
			first = false;
		}
		if right.count_newlines() > 0 {
			expand_near_parens = true;
		}

		let mut first = true;
		for arg in &mut args.positional {
			if (first && expand_near_parens) || (!first && expand_between) {
				arg.expr.opening_fodder_mut().ensure_clean_newline();
			}
			first = false;
		}
		for arg in &mut args.named {
			if (first && expand_near_parens) || (!first && expand_between) {
				arg.name_fodder.ensure_clean_newline();
			}
			first = false;
		}
		if expand_near_parens {
			right.ensure_clean_newline();
		}

		base::arguments(self, left, args, right, ctx);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		CommentStyle, Options, StringStyle,
		ast::{Node, NodeKind},
		fodder::FodderKind,
		format,
		parser::snippet_to_raw_ast,
		pass::visit_file,
	};

	/// Parse, and optionally run the pass.
	fn parse(input: &str, run: bool) -> Node {
		let (mut node, mut final_fodder) =
			snippet_to_raw_ast("t.jsonnet", input).expect("the snippet parses");
		if run {
			visit_file(&mut FixNewlines, &mut node, &mut final_fodder);
		}
		node
	}

	/// Assert the pass left the tree exactly as parsed.
	///
	/// The strongest no-op assertion available, and it needs no knowledge of
	/// what the fodder looks like — only that it did not change. Most of what
	/// this pass has to get right is *not* expanding.
	fn assert_no_op(input: &str) {
		assert_eq!(
			parse(input, true),
			parse(input, false),
			"FixNewlines changed {input:?} and should not have"
		);
	}

	/// Which of a node's designated places now start on a fresh line.
	///
	/// This is the pass's own postcondition — "newlines in all of the
	/// designated places or in none" — read straight back off the tree.
	/// Deliberately not an assertion about rendered text: the exact bytes
	/// depend on how the lexer *composed* the fodder, which is the one thing
	/// this project has measured itself unreliable at deriving by hand, and on
	/// `FixIndentation`, which is not running here. `testdata/pass-snippets.json`
	/// carries the answers that need an oracle.
	fn breaks(input: &str) -> Vec<bool> {
		let node = parse(input, true);
		let mut out = Vec::new();

		fn spec_breaks(spec: &ForSpec, out: &mut Vec<bool>) {
			if let Some(outer) = spec.outer.as_deref() {
				spec_breaks(outer, out);
			}
			out.push(spec.for_fodder.has_clean_endline());
			for condition in &spec.conditions {
				out.push(condition.if_fodder.has_clean_endline());
			}
		}

		match &node.kind {
			NodeKind::Array(array) => {
				for element in &array.elements {
					out.push(element.expr.opening_fodder().has_clean_endline());
				}
				out.push(array.close_fodder.has_clean_endline());
			}
			NodeKind::Object(object) => {
				for field in &object.fields {
					out.push(field.open_fodder().is_some_and(Fodder::has_clean_endline));
				}
				out.push(object.close_fodder.has_clean_endline());
			}
			NodeKind::ObjectComp(comp) => {
				for field in &comp.fields {
					out.push(field.open_fodder().is_some_and(Fodder::has_clean_endline));
				}
				spec_breaks(&comp.spec, &mut out);
				out.push(comp.close_fodder.has_clean_endline());
			}
			NodeKind::ArrayComp(comp) => {
				out.push(comp.body.opening_fodder().has_clean_endline());
				spec_breaks(&comp.spec, &mut out);
				out.push(comp.close_fodder.has_clean_endline());
			}
			NodeKind::Local(local) => {
				for bind in &local.binds {
					out.push(bind.var_fodder.has_clean_endline());
				}
			}
			NodeKind::Parens(parens) => {
				out.push(parens.inner.opening_fodder().has_clean_endline());
				out.push(parens.close_fodder.has_clean_endline());
			}
			NodeKind::Function(function) => {
				for param in &function.parameters {
					out.push(param.name_fodder.has_clean_endline());
				}
				out.push(function.paren_right_fodder.has_clean_endline());
			}
			NodeKind::Apply(apply) => {
				for arg in &apply.arguments.positional {
					out.push(arg.expr.opening_fodder().has_clean_endline());
				}
				for arg in &apply.arguments.named {
					out.push(arg.name_fodder.has_clean_endline());
				}
				out.push(apply.fodder_right.has_clean_endline());
			}
			other => panic!("no designated places recorded for {}", other.name()),
		}

		out
	}

	/// The pipeline, with the representation passes off, for the few
	/// assertions that are about the finished layout.
	fn formatted(input: &str) -> String {
		let options = Options {
			string_style: StringStyle::Leave,
			comment_style: CommentStyle::Leave,
			..Options::default()
		};
		format("t.jsonnet", input, &options).expect("the snippet parses")
	}

	#[test]
	fn one_newline_expands_a_whole_array() {
		// Three elements then the close bracket. The first element is expanded
		// too, even though the newline that triggered it belongs to the
		// second: the rewrite loop has no `i > 0` guard, unlike `Local`'s.
		assert_eq!(breaks("[1,\n2, 3]"), [true, true, true, true]);
		// The close bracket on its own is enough.
		assert_eq!(breaks("[1, 2, 3\n]"), [true, true, true, true]);
	}

	#[test]
	fn an_array_with_no_newlines_is_left_alone() {
		assert_eq!(breaks("[1, 2, 3]"), [false, false, false, false]);
		assert_no_op("[1, 2, 3]");
	}

	#[test]
	fn the_test_is_shallow() {
		// Upstream's own example: the newlines are inside the braces, so the
		// array around them is not expanded — and the object inside is
		// already expanded, so nothing anywhere changes.
		assert_eq!(breaks("[{\n  a: 'b',\n  c: 'd',\n}]"), [false, false]);
		assert_no_op("[{\n  a: 'b',\n  c: 'd',\n}]");
	}

	#[test]
	fn interstitials_are_not_newlines_and_paragraphs_are() {
		// `count_newlines` scores an interstitial 0, so a line full of them
		// leaves the array unexpanded.
		assert_no_op("[/* a */ 1, /* b */ 2]");
		// A trailing `//` comment is a paragraph, which scores its comment
		// lines plus its blanks and so at least 1.
		assert_eq!(breaks("[1,  // c\n2, 3]"), [true, true, true, true]);
	}

	#[test]
	fn a_newline_is_appended_after_an_interstitial_rather_than_replacing_it() {
		// The second element's fodder ends in an interstitial, so it has no
		// clean endline and gains a bare line end *after* the comment — the
		// comment keeps its place and the element moves below it.
		let node = parse("[1, /* c */ 2\n]", true);
		let NodeKind::Array(array) = &node.kind else {
			unreachable!("parsed as an array")
		};
		let second = array.elements[1].expr.opening_fodder();
		assert_eq!(second.len(), 2, "appended, not replaced");
		assert_eq!(second.first().expect("two").kind, FodderKind::Interstitial);
		assert_eq!(second.last().expect("two").kind, FodderKind::LineEnd);
	}

	#[test]
	fn an_object_expands_from_either_end() {
		assert_eq!(breaks("{ a: 1,\nb: 2 }"), [true, true, true]);
		assert_eq!(breaks("{ a: 1, b: 2\n}"), [true, true, true]);
		assert_no_op("{ a: 1, b: 2 }");
	}

	#[test]
	fn a_quoted_field_name_carries_its_own_opening_fodder() {
		// `ObjectFieldStr` has no `fodder1` at all, so `open_fodder` has to
		// reach the name expression. Reading `fodder1` there instead would
		// leave an object of quoted fields permanently unexpanded — and would
		// write a newline into a slot the unparser never reads.
		assert_eq!(breaks("{ 'a': 1,\n'b': 2 }"), [true, true, true]);
		// One field of each kind, so both branches run over one node.
		assert_eq!(breaks("{ a: 1, 'b': 2\n}"), [true, true, true]);
	}

	#[test]
	fn a_local_expands_every_bind_but_the_first() {
		// The first bind's newline expands the second; the first is skipped by
		// the `i > 0` guard and keeps the line it was written on — so here it
		// stays true only because it was already true.
		assert_eq!(breaks("local\nx = 1, y = 2;\nx"), [true, true]);
		// A later bind's newline expands the ones after it and still leaves
		// the first alone — which is what makes the guard observable.
		assert_eq!(
			breaks("local x = 1,\ny = 2, z = 3;\nx"),
			[false, true, true]
		);
	}

	#[test]
	fn a_single_bind_local_is_a_no_op_even_when_it_should_expand() {
		// `should_expand` is true and the rewrite loop never runs, because the
		// only bind is the first. Worth an assertion of its own: it is not the
		// same kind of no-op as having nothing to expand.
		assert_no_op("local\nx = 1;\nx");
		assert_no_op("local x = 1, y = 2; x");
	}

	#[test]
	fn only_a_binds_var_fodder_is_measured() {
		// A newline before the `=`, or before the body, is not a reason to
		// expand the other binds.
		assert_no_op("local x =\n1, y = 2;\nx");
	}

	#[test]
	fn parens_expand_from_either_end() {
		assert_eq!(breaks("(\n1)"), [true, true]);
		assert_eq!(breaks("(1\n)"), [true, true]);
		assert_no_op("(1)");
	}

	#[test]
	fn a_parameter_list_has_two_independent_flags() {
		// Two parameters then the close paren. Near parens: the paren moves,
		// the later parameter does not.
		assert_eq!(breaks("function(\na, b) 1"), [true, false, true]);
		// Between: the later parameters line up, and neither the first nor the
		// paren does.
		assert_eq!(breaks("function(a,\nb, c) 1"), [false, true, true, false]);
		// A newline before the `)` is the other way into near parens, and it
		// expands the first parameter but not the second.
		assert_eq!(breaks("function(a, b\n) 1"), [true, false, true]);
		// Both flags at once.
		assert_eq!(breaks("function(\na,\nb, c) 1"), [true, true, true, true]);
	}

	#[test]
	fn an_empty_parameter_list_measures_the_close_paren() {
		// With no parameters both loops do nothing, and `r` already has a
		// clean newline, so nothing changes.
		assert_no_op("function(\n) 1");
	}

	#[test]
	fn an_argument_lists_first_flag_spans_both_kinds() {
		// `breaks` reports positional arguments, then named, then the close
		// paren — the order `arguments` walks them in.
		//
		// The positional argument consumes `first`, so the named `a` counts as
		// a later item and sets `between`: the close paren stays put.
		assert_eq!(breaks("f(1,\na=2, b=3)"), [false, true, true, false]);
		// The mirror: the positional is first and sets near parens, so the
		// named argument is not expanded and the paren moves.
		assert_eq!(breaks("f(\n1, a=2)"), [true, false, true]);
		// With no positional arguments, the first *named* one is what `first`
		// protects.
		assert_eq!(breaks("f(\na=1, b=2)"), [true, false, true]);
		assert_no_op("f(\n)");
	}

	#[test]
	fn a_comprehension_expands_from_any_of_its_clauses() {
		// Body, then the spec's `for`, then any `if`, then the close bracket.
		assert_eq!(breaks("[x\nfor x in [1]]"), [true, true, true]);
		assert_eq!(breaks("[\nx for x in [1]]"), [true, true, true]);
		assert_eq!(breaks("[x for x in [1]\nif x]"), [true, true, true, true]);
		assert_no_op("[x for x in [1]]");
	}

	#[test]
	fn a_newline_on_one_for_expands_the_whole_chain() {
		// The leftmost `for` is the outermost, so the newline here belongs to
		// the inner spec and `should_expand_spec` has to recurse through
		// `outer` to find it — then `ensure_spec_expanded` expands both.
		assert_eq!(
			breaks("[x for x in [1]\nfor y in [2]]"),
			[true, true, true, true]
		);
	}

	#[test]
	fn an_object_comprehension_takes_the_same_route() {
		assert_eq!(breaks("{ [x]: x\nfor x in [1] }"), [true, true, true]);
		assert_no_op("{ [x]: x for x in [1] }");
	}

	#[test]
	fn the_pipeline_runs_this_pass_unconditionally() {
		// `FormatNode` has no option gating `FixNewlines`, so there is no
		// configuration under which its effect can be observed on its own —
		// which is half of why the pass oracle exists. The flat case is
		// asserted here because it is the one whose answer does not also
		// depend on `FixIndentation`; the layouts this pass produces are
		// graded by the snippets and by the corpus.
		assert_eq!(formatted("[1, 2]"), "[1, 2]\n");
	}
}
