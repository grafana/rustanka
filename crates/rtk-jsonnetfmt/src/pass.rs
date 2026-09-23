//! A port of go-jsonnet's `internal/pass/pass.go`: the visitor every formatter
//! pass is built on.
//!
//! Nine of the twelve passes are an override of one or two methods on this
//! traversal, so its shape decides theirs. It is ported rather than
//! re-derived, for the reason the fmt port plan gives: in
//! `rtk-gobwas-glob`, the two files that mirrored Go's structure had zero
//! divergences against a generated oracle and the one that departed from it
//! shipped nine.
//!
//! # How Go's `p` parameter becomes `self`
//!
//! Every method in Go takes the pass again as its first argument —
//! `Array(ASTPass, *ast.Array, Context)` — and `Base` calls `p.Visit(p, …)`
//! rather than its own `Visit`. That is Go simulating virtual dispatch through
//! struct embedding: `Base` has to reach the *outer* pass, and embedding alone
//! would not. A Rust trait with provided methods dispatches virtually already,
//! so `self` does that job and the extra parameter is dropped.
//!
//! What does not come for free is Go's `c.Base.Array(p, node, ctx)` — a pass
//! calling the base traversal *after* its own work, which almost all of them
//! do. Rust has no `super`, so each default method delegates to a free function
//! in [`base`], and an overriding pass calls that same function where Go calls
//! `c.Base.…`. That is what those functions are for; they are not free
//! functions standing in for private methods.
//!
//! # Where this departs from Go, and why each is behaviour-identical
//!
//! 1. **`Visit` does not copy the open fodder out and back.** Go writes
//!    `f := *(*node).OpenFodder(); p.Fodder(p, &f, ctx); *(*node).OpenFodder() = f`,
//!    which is a no-op round trip — `Fodder` is a slice, `p.Fodder` mutates
//!    through the pointer it is given, and nothing replaces the node in
//!    between. This visits it in place.
//!
//! 2. **The leaf hooks take what their node actually carries.** Go's
//!    `LiteralNull(ASTPass, *ast.LiteralNull, Context)` passes a struct whose
//!    only contents are `NodeBase`, which `ast.rs` keeps on [`Node`] instead
//!    (see that module's third departure). So [`AstPass::literal_null`] takes
//!    no payload, [`AstPass::var`] takes the identifier and
//!    [`AstPass::literal_boolean`] the flag.
//!
//!    Nothing is lost. The formatter reads a location in exactly one place —
//!    `EnforceStringStyle` hands `lit.Loc()` to `StringUnescape` — and it
//!    **discards the error that location was for**, panicking with a fixed
//!    string instead. So no hook here needs a location, and none takes one.
//!
//! 3. **An optional child Go would crash on is skipped.** `Base.ObjectField`
//!    calls `p.Visit(p, &field.Expr1, ctx)` for the two kinds that have a field
//!    name expression, and would panic on `nil`; the parser cannot produce one.
//!    Here the `if let` simply does nothing, which is the same behaviour for
//!    every tree the parser can build.
//!
//! # Four places the traversal deliberately does not go
//!
//! Each is upstream's, each is reproduced, and each means a pass that rewrites
//! fodder — `EnforceCommentStyle`, `EnforceMaxBlankLines`, the strip passes —
//! never sees that slot:
//!
//! - `InSuper`'s `in_fodder` and `super_fodder`, so a comment in
//!   `x /* c */ in super` is untouched.
//! - `Index`'s `right_bracket_fodder` when the index is an identifier, so the
//!   comment in `a. /* c */ b` is untouched.
//! - `Apply`'s `tail_strict_fodder` unless `tailstrict` is actually present.
//! - `Parameter`'s `eq_fodder` unless the parameter has a default.

use crate::{
	ast::{
		Apply, ApplyBrace, Arguments, Array, ArrayComp, Assert, Binary, Conditional, Error,
		ForSpec, Function, Identifier, Import, InSuper, Index, LiteralNumber, LiteralString, Local,
		Node, NodeKind, Object, ObjectComp, ObjectField, ObjectFieldKind, Parameter, Parens, Slice,
		SuperIndex, Unary,
	},
	fodder::{Fodder, FodderElement},
};

/// go-jsonnet's `pass.ASTPass`.
///
/// Every method has the base traversal as its default, so a pass overrides
/// only what it changes and calls the matching [`base`] function to carry on
/// downwards.
pub trait AstPass {
	/// go-jsonnet's `pass.Context`, which is `interface{}` — each pass chooses
	/// its own, and all but one choose nothing.
	///
	/// Only [`AddPlusObject`](crate::passes::AddPlusObject) uses it, to decide
	/// whether replacing `e {}` with `e + {}` needs parentheses. That is why
	/// this is an associated type rather than `()`: the one pass that needs a
	/// context needs a particular one.
	///
	/// Go's is the parent *node*, and it distinguishes the slots of that parent
	/// by comparing the parent's child pointer against the current node — which
	/// Rust cannot do while the parent is mutably borrowed. So that pass
	/// carries [`passes::add_plus_object::Parent`], a descriptor of the parent
	/// refined per slot, and fills it in by overriding the five node hooks
	/// whose slots upstream's switch tells apart. This trait was written with
	/// that case scoped rather than discovered to be incompatible with it; the
	/// cost, paid there and not here, is that those five overrides restate the
	/// base traversal, because a [`base`] function takes one `ctx` and hands it
	/// to every slot.
	///
	/// [`passes::add_plus_object::Parent`]: crate::passes::add_plus_object::Parent
	type Ctx;

	/// `BaseContext`: the context the root is visited with.
	fn base_context(&mut self) -> Self::Ctx;

	fn fodder_element(&mut self, element: &mut FodderElement, ctx: &Self::Ctx) {
		base::fodder_element(self, element, ctx);
	}

	fn fodder(&mut self, fodder: &mut Fodder, ctx: &Self::Ctx) {
		base::fodder(self, fodder, ctx);
	}

	fn for_spec(&mut self, spec: &mut ForSpec, ctx: &Self::Ctx) {
		base::for_spec(self, spec, ctx);
	}

	fn parameters(
		&mut self,
		left: &mut Fodder,
		params: &mut Vec<Parameter>,
		right: &mut Fodder,
		ctx: &Self::Ctx,
	) {
		base::parameters(self, left, params, right, ctx);
	}

	fn arguments(
		&mut self,
		left: &mut Fodder,
		args: &mut Arguments,
		right: &mut Fodder,
		ctx: &Self::Ctx,
	) {
		base::arguments(self, left, args, right, ctx);
	}

	fn field_params(&mut self, field: &mut ObjectField, ctx: &Self::Ctx) {
		base::field_params(self, field, ctx);
	}

	fn object_field(&mut self, field: &mut ObjectField, ctx: &Self::Ctx) {
		base::object_field(self, field, ctx);
	}

	fn object_fields(&mut self, fields: &mut Vec<ObjectField>, ctx: &Self::Ctx) {
		base::object_fields(self, fields, ctx);
	}

	fn apply(&mut self, node: &mut Apply, ctx: &Self::Ctx) {
		base::apply(self, node, ctx);
	}

	fn apply_brace(&mut self, node: &mut ApplyBrace, ctx: &Self::Ctx) {
		base::apply_brace(self, node, ctx);
	}

	fn array(&mut self, node: &mut Array, ctx: &Self::Ctx) {
		base::array(self, node, ctx);
	}

	fn array_comp(&mut self, node: &mut ArrayComp, ctx: &Self::Ctx) {
		base::array_comp(self, node, ctx);
	}

	fn assert(&mut self, node: &mut Assert, ctx: &Self::Ctx) {
		base::assert(self, node, ctx);
	}

	fn binary(&mut self, node: &mut Binary, ctx: &Self::Ctx) {
		base::binary(self, node, ctx);
	}

	fn conditional(&mut self, node: &mut Conditional, ctx: &Self::Ctx) {
		base::conditional(self, node, ctx);
	}

	/// `$`, which carries nothing. Go cannot descend any further either.
	fn dollar(&mut self, ctx: &Self::Ctx) {
		base::dollar(self, ctx);
	}

	fn error(&mut self, node: &mut Error, ctx: &Self::Ctx) {
		base::error(self, node, ctx);
	}

	fn function(&mut self, node: &mut Function, ctx: &Self::Ctx) {
		base::function(self, node, ctx);
	}

	fn import(&mut self, node: &mut Import, ctx: &Self::Ctx) {
		base::import(self, node, ctx);
	}

	fn import_str(&mut self, node: &mut Import, ctx: &Self::Ctx) {
		base::import_str(self, node, ctx);
	}

	fn import_bin(&mut self, node: &mut Import, ctx: &Self::Ctx) {
		base::import_bin(self, node, ctx);
	}

	fn index(&mut self, node: &mut Index, ctx: &Self::Ctx) {
		base::index(self, node, ctx);
	}

	fn slice(&mut self, node: &mut Slice, ctx: &Self::Ctx) {
		base::slice(self, node, ctx);
	}

	fn local(&mut self, node: &mut Local, ctx: &Self::Ctx) {
		base::local(self, node, ctx);
	}

	fn literal_boolean(&mut self, value: &mut bool, ctx: &Self::Ctx) {
		base::literal_boolean(self, value, ctx);
	}

	fn literal_null(&mut self, ctx: &Self::Ctx) {
		base::literal_null(self, ctx);
	}

	fn literal_number(&mut self, node: &mut LiteralNumber, ctx: &Self::Ctx) {
		base::literal_number(self, node, ctx);
	}

	fn literal_string(&mut self, node: &mut LiteralString, ctx: &Self::Ctx) {
		base::literal_string(self, node, ctx);
	}

	fn object(&mut self, node: &mut Object, ctx: &Self::Ctx) {
		base::object(self, node, ctx);
	}

	fn object_comp(&mut self, node: &mut ObjectComp, ctx: &Self::Ctx) {
		base::object_comp(self, node, ctx);
	}

	fn parens(&mut self, node: &mut Parens, ctx: &Self::Ctx) {
		base::parens(self, node, ctx);
	}

	/// `self`, which carries nothing.
	fn self_expr(&mut self, ctx: &Self::Ctx) {
		base::self_expr(self, ctx);
	}

	fn super_index(&mut self, node: &mut SuperIndex, ctx: &Self::Ctx) {
		base::super_index(self, node, ctx);
	}

	fn in_super(&mut self, node: &mut InSuper, ctx: &Self::Ctx) {
		base::in_super(self, node, ctx);
	}

	fn unary(&mut self, node: &mut Unary, ctx: &Self::Ctx) {
		base::unary(self, node, ctx);
	}

	fn var(&mut self, id: &mut Identifier, ctx: &Self::Ctx) {
		base::var(self, id, ctx);
	}

	/// Visit a node of any kind.
	///
	/// This is the only hook that gets the whole [`Node`], so it is the only
	/// one that can **replace** it — which all three of Phase 2e's passes do.
	/// It is also why [`FixParens`](crate::passes::FixParens) overrides this
	/// rather than [`AstPass::parens`], although upstream overrides `Parens`:
	/// it needs the node's own fodder, and this module's second departure keeps
	/// that on [`Node`] instead of in the variant.
	fn visit(&mut self, node: &mut Node, ctx: &Self::Ctx) {
		base::visit(self, node, ctx);
	}

	/// `File`: the whole program, then the fodder after its last token.
	fn file(&mut self, node: &mut Node, final_fodder: &mut Fodder) {
		base::file(self, node, final_fodder);
	}
}

/// `formatter.visitFile`: run one pass over a whole file.
pub fn visit_file<P: AstPass + ?Sized>(pass: &mut P, node: &mut Node, final_fodder: &mut Fodder) {
	pass.file(node, final_fodder);
}

/// go-jsonnet's `pass.Base`: the plain traversal, one function per hook.
///
/// A pass reaches these where Go writes `c.Base.Array(p, node, ctx)` — see the
/// module documentation for why they are not methods.
pub mod base {
	use super::{
		Apply, ApplyBrace, Arguments, Array, ArrayComp, Assert, AstPass, Binary, Conditional,
		Error, Fodder, FodderElement, ForSpec, Function, Identifier, Import, InSuper, Index,
		LiteralNumber, LiteralString, Local, Node, NodeKind, Object, ObjectComp, ObjectField,
		ObjectFieldKind, Parameter, Parens, Slice, SuperIndex, Unary,
	};

	/// Fodder elements are leaves: there is nothing below one to descend into.
	pub fn fodder_element<P: AstPass + ?Sized>(
		_pass: &mut P,
		_element: &mut FodderElement,
		_ctx: &P::Ctx,
	) {
	}

	pub fn fodder<P: AstPass + ?Sized>(pass: &mut P, fodder: &mut Fodder, ctx: &P::Ctx) {
		for element in fodder.iter_mut() {
			pass.fodder_element(element, ctx);
		}
	}

	/// The outermost `for` first, which is how a comprehension was written:
	/// `ForSpec` nests the *later* specs outside the earlier ones.
	pub fn for_spec<P: AstPass + ?Sized>(pass: &mut P, spec: &mut ForSpec, ctx: &P::Ctx) {
		if let Some(outer) = &mut spec.outer {
			pass.for_spec(outer, ctx);
		}
		pass.fodder(&mut spec.for_fodder, ctx);
		pass.fodder(&mut spec.var_fodder, ctx);
		pass.fodder(&mut spec.in_fodder, ctx);
		pass.visit(&mut spec.expr, ctx);
		for condition in &mut spec.conditions {
			pass.fodder(&mut condition.if_fodder, ctx);
			pass.visit(&mut condition.expr, ctx);
		}
	}

	/// A parameter's `eq_fodder` is visited only where it has a default, which
	/// is also the only case where the unparser writes an `=`.
	pub fn parameters<P: AstPass + ?Sized>(
		pass: &mut P,
		left: &mut Fodder,
		params: &mut Vec<Parameter>,
		right: &mut Fodder,
		ctx: &P::Ctx,
	) {
		pass.fodder(left, ctx);
		for param in params.iter_mut() {
			pass.fodder(&mut param.name_fodder, ctx);
			if let Some(default_arg) = &mut param.default_arg {
				pass.fodder(&mut param.eq_fodder, ctx);
				pass.visit(default_arg, ctx);
			}
			pass.fodder(&mut param.comma_fodder, ctx);
		}
		pass.fodder(right, ctx);
	}

	/// Positional arguments first, then named — the order the unparser writes
	/// them, which is not necessarily the order they were written in.
	pub fn arguments<P: AstPass + ?Sized>(
		pass: &mut P,
		left: &mut Fodder,
		args: &mut Arguments,
		right: &mut Fodder,
		ctx: &P::Ctx,
	) {
		pass.fodder(left, ctx);
		for arg in &mut args.positional {
			pass.visit(&mut arg.expr, ctx);
			pass.fodder(&mut arg.comma_fodder, ctx);
		}
		for arg in &mut args.named {
			pass.fodder(&mut arg.name_fodder, ctx);
			pass.fodder(&mut arg.eq_fodder, ctx);
			pass.visit(&mut arg.arg, ctx);
			pass.fodder(&mut arg.comma_fodder, ctx);
		}
		pass.fodder(right, ctx);
	}

	/// The parameter list of the method sugar `f(x): e`, if this field has one.
	pub fn field_params<P: AstPass + ?Sized>(pass: &mut P, field: &mut ObjectField, ctx: &P::Ctx) {
		if let Some(method) = &mut field.method {
			pass.parameters(
				&mut method.paren_left_fodder,
				&mut method.parameters,
				&mut method.paren_right_fodder,
				ctx,
			);
		}
	}

	/// Which slots exist depends on the field's kind, so this switches on it
	/// exactly as `unparseFields` does.
	pub fn object_field<P: AstPass + ?Sized>(pass: &mut P, field: &mut ObjectField, ctx: &P::Ctx) {
		match field.kind {
			ObjectFieldKind::Local => {
				pass.fodder(&mut field.fodder1, ctx);
				pass.fodder(&mut field.fodder2, ctx);
				pass.field_params(field, ctx);
				pass.fodder(&mut field.op_fodder, ctx);
				if let Some(expr2) = &mut field.expr2 {
					pass.visit(expr2, ctx);
				}
			}

			ObjectFieldKind::FieldId => {
				pass.fodder(&mut field.fodder1, ctx);
				pass.field_params(field, ctx);
				pass.fodder(&mut field.op_fodder, ctx);
				if let Some(expr2) = &mut field.expr2 {
					pass.visit(expr2, ctx);
				}
			}

			// No `fodder1`: the field name is an expression and carries its own
			// opening fodder.
			ObjectFieldKind::FieldStr => {
				if let Some(expr1) = &mut field.expr1 {
					pass.visit(expr1, ctx);
				}
				pass.field_params(field, ctx);
				pass.fodder(&mut field.op_fodder, ctx);
				if let Some(expr2) = &mut field.expr2 {
					pass.visit(expr2, ctx);
				}
			}

			ObjectFieldKind::FieldExpr => {
				pass.fodder(&mut field.fodder1, ctx);
				if let Some(expr1) = &mut field.expr1 {
					pass.visit(expr1, ctx);
				}
				pass.fodder(&mut field.fodder2, ctx);
				pass.field_params(field, ctx);
				pass.fodder(&mut field.op_fodder, ctx);
				if let Some(expr2) = &mut field.expr2 {
					pass.visit(expr2, ctx);
				}
			}

			ObjectFieldKind::Assert => {
				pass.fodder(&mut field.fodder1, ctx);
				if let Some(expr2) = &mut field.expr2 {
					pass.visit(expr2, ctx);
				}
				if let Some(expr3) = &mut field.expr3 {
					pass.fodder(&mut field.op_fodder, ctx);
					pass.visit(expr3, ctx);
				}
			}
		}

		pass.fodder(&mut field.comma_fodder, ctx);
	}

	pub fn object_fields<P: AstPass + ?Sized>(
		pass: &mut P,
		fields: &mut Vec<ObjectField>,
		ctx: &P::Ctx,
	) {
		for field in fields.iter_mut() {
			pass.object_field(field, ctx);
		}
	}

	/// `tail_strict_fodder` is visited only where `tailstrict` was written.
	pub fn apply<P: AstPass + ?Sized>(pass: &mut P, node: &mut Apply, ctx: &P::Ctx) {
		pass.visit(&mut node.target, ctx);
		pass.arguments(
			&mut node.fodder_left,
			&mut node.arguments,
			&mut node.fodder_right,
			ctx,
		);
		if node.tail_strict {
			pass.fodder(&mut node.tail_strict_fodder, ctx);
		}
	}

	pub fn apply_brace<P: AstPass + ?Sized>(pass: &mut P, node: &mut ApplyBrace, ctx: &P::Ctx) {
		pass.visit(&mut node.left, ctx);
		pass.visit(&mut node.right, ctx);
	}

	pub fn array<P: AstPass + ?Sized>(pass: &mut P, node: &mut Array, ctx: &P::Ctx) {
		for element in &mut node.elements {
			pass.visit(&mut element.expr, ctx);
			pass.fodder(&mut element.comma_fodder, ctx);
		}
		pass.fodder(&mut node.close_fodder, ctx);
	}

	pub fn array_comp<P: AstPass + ?Sized>(pass: &mut P, node: &mut ArrayComp, ctx: &P::Ctx) {
		pass.visit(&mut node.body, ctx);
		pass.fodder(&mut node.trailing_comma_fodder, ctx);
		pass.for_spec(&mut node.spec, ctx);
		pass.fodder(&mut node.close_fodder, ctx);
	}

	pub fn assert<P: AstPass + ?Sized>(pass: &mut P, node: &mut Assert, ctx: &P::Ctx) {
		pass.visit(&mut node.cond, ctx);
		if let Some(message) = &mut node.message {
			pass.fodder(&mut node.colon_fodder, ctx);
			pass.visit(message, ctx);
		}
		pass.fodder(&mut node.semicolon_fodder, ctx);
		pass.visit(&mut node.rest, ctx);
	}

	pub fn binary<P: AstPass + ?Sized>(pass: &mut P, node: &mut Binary, ctx: &P::Ctx) {
		pass.visit(&mut node.left, ctx);
		pass.fodder(&mut node.op_fodder, ctx);
		pass.visit(&mut node.right, ctx);
	}

	pub fn conditional<P: AstPass + ?Sized>(pass: &mut P, node: &mut Conditional, ctx: &P::Ctx) {
		pass.visit(&mut node.cond, ctx);
		pass.fodder(&mut node.then_fodder, ctx);
		pass.visit(&mut node.branch_true, ctx);
		if let Some(branch_false) = &mut node.branch_false {
			pass.fodder(&mut node.else_fodder, ctx);
			pass.visit(branch_false, ctx);
		}
	}

	pub fn dollar<P: AstPass + ?Sized>(_pass: &mut P, _ctx: &P::Ctx) {}

	pub fn error<P: AstPass + ?Sized>(pass: &mut P, node: &mut Error, ctx: &P::Ctx) {
		pass.visit(&mut node.expr, ctx);
	}

	pub fn function<P: AstPass + ?Sized>(pass: &mut P, node: &mut Function, ctx: &P::Ctx) {
		pass.parameters(
			&mut node.paren_left_fodder,
			&mut node.parameters,
			&mut node.paren_right_fodder,
			ctx,
		);
		pass.visit(&mut node.body, ctx);
	}

	/// The three import kinds traverse identically: the filename's own fodder,
	/// then the filename as a string literal.
	///
	/// Note the literal is reached through [`AstPass::literal_string`] and
	/// **not** through [`AstPass::visit`], so a pass that overrides `visit`
	/// alone never sees an import's filename — which is why
	/// `EnforceStringStyle` overrides the hook.
	fn import_file<P: AstPass + ?Sized>(pass: &mut P, node: &mut Import, ctx: &P::Ctx) {
		let Node { fodder, kind, .. } = &mut *node.file;
		pass.fodder(fodder, ctx);
		// Go's field is typed `*ast.LiteralString`, so this is every tree the
		// parser can build: it refuses a computed import outright.
		if let NodeKind::LiteralString(literal) = kind {
			pass.literal_string(literal, ctx);
		}
	}

	pub fn import<P: AstPass + ?Sized>(pass: &mut P, node: &mut Import, ctx: &P::Ctx) {
		import_file(pass, node, ctx);
	}

	pub fn import_str<P: AstPass + ?Sized>(pass: &mut P, node: &mut Import, ctx: &P::Ctx) {
		import_file(pass, node, ctx);
	}

	pub fn import_bin<P: AstPass + ?Sized>(pass: &mut P, node: &mut Import, ctx: &P::Ctx) {
		import_file(pass, node, ctx);
	}

	/// `right_bracket_fodder` is visited only where the index is an
	/// expression. Where it is an identifier the same slot holds the fodder
	/// before that identifier, and upstream never descends into it.
	pub fn index<P: AstPass + ?Sized>(pass: &mut P, node: &mut Index, ctx: &P::Ctx) {
		pass.visit(&mut node.target, ctx);
		pass.fodder(&mut node.left_bracket_fodder, ctx);
		if node.id.is_none() {
			if let Some(index) = &mut node.index {
				pass.visit(index, ctx);
			}
			pass.fodder(&mut node.right_bracket_fodder, ctx);
		}
	}

	/// Both colon slots are visited whether or not the index beside them is
	/// present, which is what lets `NoRedundantSliceColon` find the fodder of
	/// a colon that is about to disappear.
	pub fn slice<P: AstPass + ?Sized>(pass: &mut P, node: &mut Slice, ctx: &P::Ctx) {
		pass.visit(&mut node.target, ctx);
		pass.fodder(&mut node.left_bracket_fodder, ctx);
		if let Some(begin_index) = &mut node.begin_index {
			pass.visit(begin_index, ctx);
		}
		pass.fodder(&mut node.end_colon_fodder, ctx);
		if let Some(end_index) = &mut node.end_index {
			pass.visit(end_index, ctx);
		}
		pass.fodder(&mut node.step_colon_fodder, ctx);
		if let Some(step) = &mut node.step {
			pass.visit(step, ctx);
		}
		pass.fodder(&mut node.right_bracket_fodder, ctx);
	}

	pub fn local<P: AstPass + ?Sized>(pass: &mut P, node: &mut Local, ctx: &P::Ctx) {
		for bind in &mut node.binds {
			pass.fodder(&mut bind.var_fodder, ctx);
			if let Some(fun) = &mut bind.fun {
				pass.parameters(
					&mut fun.paren_left_fodder,
					&mut fun.parameters,
					&mut fun.paren_right_fodder,
					ctx,
				);
			}
			pass.fodder(&mut bind.eq_fodder, ctx);
			pass.visit(&mut bind.body, ctx);
			pass.fodder(&mut bind.close_fodder, ctx);
		}
		pass.visit(&mut node.body, ctx);
	}

	pub fn literal_boolean<P: AstPass + ?Sized>(_pass: &mut P, _value: &mut bool, _ctx: &P::Ctx) {}

	pub fn literal_null<P: AstPass + ?Sized>(_pass: &mut P, _ctx: &P::Ctx) {}

	pub fn literal_number<P: AstPass + ?Sized>(
		_pass: &mut P,
		_node: &mut LiteralNumber,
		_ctx: &P::Ctx,
	) {
	}

	pub fn literal_string<P: AstPass + ?Sized>(
		_pass: &mut P,
		_node: &mut LiteralString,
		_ctx: &P::Ctx,
	) {
	}

	pub fn object<P: AstPass + ?Sized>(pass: &mut P, node: &mut Object, ctx: &P::Ctx) {
		pass.object_fields(&mut node.fields, ctx);
		pass.fodder(&mut node.close_fodder, ctx);
	}

	pub fn object_comp<P: AstPass + ?Sized>(pass: &mut P, node: &mut ObjectComp, ctx: &P::Ctx) {
		pass.object_fields(&mut node.fields, ctx);
		pass.for_spec(&mut node.spec, ctx);
		pass.fodder(&mut node.close_fodder, ctx);
	}

	pub fn parens<P: AstPass + ?Sized>(pass: &mut P, node: &mut Parens, ctx: &P::Ctx) {
		pass.visit(&mut node.inner, ctx);
		pass.fodder(&mut node.close_fodder, ctx);
	}

	pub fn self_expr<P: AstPass + ?Sized>(_pass: &mut P, _ctx: &P::Ctx) {}

	/// `id_fodder` is visited either way — unlike [`index`], which skips its
	/// twin of this slot when the index is an identifier.
	pub fn super_index<P: AstPass + ?Sized>(pass: &mut P, node: &mut SuperIndex, ctx: &P::Ctx) {
		pass.fodder(&mut node.dot_fodder, ctx);
		if node.id.is_none()
			&& let Some(index) = &mut node.index
		{
			pass.visit(index, ctx);
		}
		pass.fodder(&mut node.id_fodder, ctx);
	}

	/// Only the index. `in_fodder` and `super_fodder` are never visited, so no
	/// pass rewrites a comment written inside `x /* c */ in super`.
	pub fn in_super<P: AstPass + ?Sized>(pass: &mut P, node: &mut InSuper, ctx: &P::Ctx) {
		pass.visit(&mut node.index, ctx);
	}

	pub fn unary<P: AstPass + ?Sized>(pass: &mut P, node: &mut Unary, ctx: &P::Ctx) {
		pass.visit(&mut node.expr, ctx);
	}

	pub fn var<P: AstPass + ?Sized>(_pass: &mut P, _id: &mut Identifier, _ctx: &P::Ctx) {}

	/// The node's own opening fodder, then its kind's traversal.
	pub fn visit<P: AstPass + ?Sized>(pass: &mut P, node: &mut Node, ctx: &P::Ctx) {
		pass.fodder(&mut node.fodder, ctx);

		match &mut node.kind {
			NodeKind::Apply(inner) => pass.apply(inner, ctx),
			NodeKind::ApplyBrace(inner) => pass.apply_brace(inner, ctx),
			NodeKind::Array(inner) => pass.array(inner, ctx),
			NodeKind::ArrayComp(inner) => pass.array_comp(inner, ctx),
			NodeKind::Assert(inner) => pass.assert(inner, ctx),
			NodeKind::Binary(inner) => pass.binary(inner, ctx),
			NodeKind::Conditional(inner) => pass.conditional(inner, ctx),
			NodeKind::Dollar => pass.dollar(ctx),
			NodeKind::Error(inner) => pass.error(inner, ctx),
			NodeKind::Function(inner) => pass.function(inner, ctx),
			NodeKind::Import(inner) => pass.import(inner, ctx),
			NodeKind::ImportStr(inner) => pass.import_str(inner, ctx),
			NodeKind::ImportBin(inner) => pass.import_bin(inner, ctx),
			NodeKind::Index(inner) => pass.index(inner, ctx),
			NodeKind::InSuper(inner) => pass.in_super(inner, ctx),
			NodeKind::LiteralBoolean(inner) => pass.literal_boolean(inner, ctx),
			NodeKind::LiteralNull => pass.literal_null(ctx),
			NodeKind::LiteralNumber(inner) => pass.literal_number(inner, ctx),
			NodeKind::LiteralString(inner) => pass.literal_string(inner, ctx),
			NodeKind::Local(inner) => pass.local(inner, ctx),
			NodeKind::Object(inner) => pass.object(inner, ctx),
			NodeKind::ObjectComp(inner) => pass.object_comp(inner, ctx),
			NodeKind::Parens(inner) => pass.parens(inner, ctx),
			NodeKind::SelfExpr => pass.self_expr(ctx),
			NodeKind::Slice(inner) => pass.slice(inner, ctx),
			NodeKind::SuperIndex(inner) => pass.super_index(inner, ctx),
			NodeKind::Unary(inner) => pass.unary(inner, ctx),
			NodeKind::Var(inner) => pass.var(inner, ctx),
		}
	}

	pub fn file<P: AstPass + ?Sized>(pass: &mut P, node: &mut Node, final_fodder: &mut Fodder) {
		let ctx = pass.base_context();
		pass.visit(node, &ctx);
		pass.fodder(final_fodder, &ctx);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::parser::snippet_to_raw_ast;

	/// A pass that only counts, to check the traversal reaches what it should.
	#[derive(Default)]
	struct Counter {
		nodes: Vec<&'static str>,
		fodder_elements: usize,
	}

	impl AstPass for Counter {
		type Ctx = ();

		fn base_context(&mut self) {}

		fn fodder_element(&mut self, element: &mut FodderElement, ctx: &()) {
			self.fodder_elements += 1;
			base::fodder_element(self, element, ctx);
		}

		fn visit(&mut self, node: &mut Node, ctx: &()) {
			self.nodes.push(node.kind.name());
			base::visit(self, node, ctx);
		}
	}

	fn walk(source: &str) -> Counter {
		let (mut node, mut final_fodder) =
			snippet_to_raw_ast("t.jsonnet", source).expect("the snippet parses");
		let mut counter = Counter::default();
		visit_file(&mut counter, &mut node, &mut final_fodder);
		counter
	}

	#[test]
	fn the_traversal_reaches_every_node_of_a_nested_tree() {
		// `local` binds, an object, a field value and a call, so the walk has to
		// go through four different hooks to reach them all.
		let counter = walk("local f(x) = x; { a: f(1) }");
		assert_eq!(
			counter.nodes,
			vec!["Local", "Var", "Object", "Apply", "Var", "LiteralNumber"]
		);
	}

	#[test]
	fn an_import_filename_is_not_reached_through_visit() {
		// `Base.Import` calls the LiteralString hook directly, so the filename
		// is never visited as a node. `EnforceStringStyle` depends on this:
		// overriding `visit` alone would miss every import in the file.
		let counter = walk("import 'x.libsonnet'");
		assert_eq!(counter.nodes, vec!["Import"]);
	}

	#[test]
	fn in_super_fodder_is_never_visited() {
		// Upstream's `Base.InSuper` descends into the index and nothing else,
		// so the comment here is unreachable to every pass. Reproduced on
		// purpose.
		let counter = walk("{ x: 'a' /* c */ in super }");
		assert_eq!(counter.fodder_elements, 0);
	}

	#[test]
	fn a_dotted_index_does_not_visit_the_fodder_before_its_identifier() {
		// `Index` reuses `RightBracketFodder` for the fodder before the id, and
		// the base traversal only descends into it in the bracket case.
		assert_eq!(walk("a. /* c */ b").fodder_elements, 0);
		assert_eq!(walk("a[/* c */ 'b']").fodder_elements, 1);
	}

	#[test]
	fn a_comment_in_a_parameter_list_is_reached_either_way() {
		// Without a default the comment lands in `paren_right_fodder`; with one
		// it lands in `eq_fodder`, which the traversal only descends into
		// because the default is there.
		assert_eq!(walk("function(x /* c */) x").fodder_elements, 1);
		assert_eq!(walk("function(x /* c */ = 1) x").fodder_elements, 1);
	}
}
