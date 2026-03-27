use std::rc::Rc;

use jrsonnet_interner::IStr;
use rustc_hash::FxHashSet;

use crate::expr::{
	ArgsDesc, AssertStmt, BindSpec, CompSpec, Destruct, Expr, FieldMember, FieldName, ForSpecData,
	IfSpecData, IndexPart, Member, ObjBody, ObjComp, Param, ParamsDesc, SliceDesc,
};

/// Immutable, Rc-shared set of variable names used (referenced) by an expression.
///
/// Designed for allocation-avoidance: single-child transparent nodes share their
/// child's Rc rather than allocating a new set. Compound nodes create one
/// `FxHashSet` and extend it from all children (no intermediate cloning).
#[derive(Clone, Debug)]
pub struct UsedVars(Rc<FxHashSet<IStr>>);

impl PartialEq for UsedVars {
	fn eq(&self, other: &Self) -> bool {
		Rc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
	}
}

impl UsedVars {
	/// The empty set — no variables referenced.
	pub fn empty() -> Self {
		Self(Rc::new(FxHashSet::default()))
	}

	/// Wrap a pre-built set.
	pub fn from_set(set: FxHashSet<IStr>) -> Self {
		Self(Rc::new(set))
	}

	/// Returns the underlying set.
	pub fn set(&self) -> &FxHashSet<IStr> {
		&self.0
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	pub fn contains(&self, name: &IStr) -> bool {
		self.0.contains(name)
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Insert all variable names from this set into `target`.
	pub fn extend_into(&self, target: &mut FxHashSet<IStr>) {
		target.extend(self.0.iter().cloned());
	}
}

/// Compute the set of variables used (referenced) by an expression.
///
/// This is a simple transitive union of all `Var(name)` references
/// in the expression tree — no scope tracking or bound-name subtraction.
///
/// Compound nodes create one `FxHashSet` and extend it from all children.
/// Single-child transparent nodes share their child's Rc (zero alloc).
pub(crate) fn compute_used_vars(expr: &Expr) -> UsedVars {
	match expr {
		// Leaf nodes with no variable references
		Expr::Literal(_) | Expr::Str(_) | Expr::Num(_) => UsedVars::empty(),

		// Variable reference — stored in Analysis.var, not in the set
		Expr::Var(_) => UsedVars::empty(),

		// Single-child transparent nodes — share child's Rc when possible.
		// If child is a Var, its name is in analysis.var (not used_vars), so
		// we must include it explicitly.
		Expr::Parened(e)
		| Expr::UnaryOp(_, e)
		| Expr::ErrorStmt(e)
		| Expr::Import(e)
		| Expr::ImportStr(e)
		| Expr::ImportBin(e) => {
			if let Some(var) = &e.analysis().var {
				let mut set = FxHashSet::default();
				set.insert(var.clone());
				e.used_vars().extend_into(&mut set);
				UsedVars::from_set(set)
			} else {
				e.used_vars().clone()
			}
		}

		// All compound nodes: create one set, extend from children
		_ => {
			let mut set = FxHashSet::default();
			collect_used_vars(expr, &mut set);
			// If a child already has this exact set, reuse its Rc
			try_reuse_child_vars(expr, &set).unwrap_or_else(|| UsedVars::from_set(set))
		}
	}
}

/// Check if any direct child's `used_vars` already covers the full set.
/// Since child.used_vars ⊆ set (we extended from it), equal length means identical.
/// The child must also have no `var` (otherwise variables are split across fields).
fn try_reuse_child_vars(expr: &Expr, set: &FxHashSet<IStr>) -> Option<UsedVars> {
	use crate::AnalyzedExpr;

	let target_len = set.len();

	let check = |e: &AnalyzedExpr| -> Option<UsedVars> {
		if e.analysis().var.is_none() && e.used_vars().len() == target_len {
			Some(e.used_vars().clone())
		} else {
			None
		}
	};

	match expr {
		Expr::BinaryOp(a, _, b) => check(a).or_else(|| check(b)),
		Expr::ObjExtend(a, _) => check(a),
		Expr::ArrComp(body, _) => check(body),
		Expr::LocalExpr(bindings, body) => {
			if let Some(v) = check(body) {
				return Some(v);
			}
			for b in bindings {
				let val = match b {
					BindSpec::Field { value, .. } | BindSpec::Function { value, .. } => value,
				};
				if let Some(v) = check(val) {
					return Some(v);
				}
			}
			None
		}
		Expr::Function(_, body) => check(body),
		Expr::Apply(func, args, _) => {
			if let Some(v) = check(func) {
				return Some(v);
			}
			for arg in &args.unnamed {
				if let Some(v) = check(arg) {
					return Some(v);
				}
			}
			for (_, arg) in &args.named {
				if let Some(v) = check(arg) {
					return Some(v);
				}
			}
			None
		}
		Expr::Index { indexable, parts } => {
			if let Some(v) = check(indexable) {
				return Some(v);
			}
			for part in parts {
				if let Some(v) = check(&part.value) {
					return Some(v);
				}
			}
			None
		}
		Expr::IfElse {
			cond,
			cond_then,
			cond_else,
		} => check(&cond.0)
			.or_else(|| check(cond_then))
			.or_else(|| cond_else.as_ref().and_then(check)),
		Expr::AssertExpr(AssertStmt(cond, msg), body) => check(cond)
			.or_else(|| check(body))
			.or_else(|| msg.as_ref().and_then(check)),
		Expr::Slice(value, desc) => check(value)
			.or_else(|| desc.start.as_ref().and_then(check))
			.or_else(|| desc.end.as_ref().and_then(check))
			.or_else(|| desc.step.as_ref().and_then(check)),
		Expr::Arr(items) => {
			for item in items {
				if let Some(v) = check(item) {
					return Some(v);
				}
			}
			None
		}
		// Obj/ObjComp children are not AnalyzedExpr at the top level
		_ => None,
	}
}

/// Recursively collect all variable references from an expression into `out`.
fn collect_used_vars(expr: &Expr, out: &mut FxHashSet<IStr>) {
	match expr {
		Expr::Literal(_) | Expr::Str(_) | Expr::Num(_) => {}

		Expr::Var(name) => {
			out.insert(name.clone());
		}

		Expr::Parened(e)
		| Expr::UnaryOp(_, e)
		| Expr::ErrorStmt(e)
		| Expr::Import(e)
		| Expr::ImportStr(e)
		| Expr::ImportBin(e) => {
			e.extend_used_into(out);
		}

		Expr::BinaryOp(a, _, b) => {
			a.extend_used_into(out);
			b.extend_used_into(out);
		}

		Expr::ObjExtend(a, body) => {
			a.extend_used_into(out);
			collect_obj_body(body, out);
		}

		Expr::Arr(items) => {
			for item in items {
				item.extend_used_into(out);
			}
		}

		Expr::ArrComp(body_expr, comp_specs) => {
			body_expr.extend_used_into(out);
			collect_comp_specs(comp_specs, out);
		}

		Expr::Obj(body) => collect_obj_body(body, out),

		Expr::LocalExpr(bindings, body) => {
			body.extend_used_into(out);
			for b in bindings {
				collect_bind_spec(b, out);
			}
		}

		Expr::Function(params, body) => {
			body.extend_used_into(out);
			collect_params(params, out);
		}

		Expr::Apply(func, args, _) => {
			func.extend_used_into(out);
			collect_args(args, out);
		}

		Expr::Index { indexable, parts } => {
			indexable.extend_used_into(out);
			for part in parts {
				collect_index_part(part, out);
			}
		}

		Expr::IfElse {
			cond,
			cond_then,
			cond_else,
		} => {
			cond.0.extend_used_into(out);
			cond_then.extend_used_into(out);
			if let Some(else_expr) = cond_else {
				else_expr.extend_used_into(out);
			}
		}

		Expr::AssertExpr(AssertStmt(cond, msg), body) => {
			cond.extend_used_into(out);
			body.extend_used_into(out);
			if let Some(msg_expr) = msg {
				msg_expr.extend_used_into(out);
			}
		}

		Expr::Slice(value, desc) => {
			value.extend_used_into(out);
			collect_slice_desc(desc, out);
		}
	}
}

fn collect_bind_spec(bind: &BindSpec, out: &mut FxHashSet<IStr>) {
	match bind {
		BindSpec::Field { into, value } => {
			value.extend_used_into(out);
			collect_destruct(into, out);
		}
		BindSpec::Function {
			name: _,
			params,
			value,
		} => {
			value.extend_used_into(out);
			collect_params(params, out);
		}
	}
}

#[allow(unused_variables)]
fn collect_destruct(d: &Destruct, out: &mut FxHashSet<IStr>) {
	match d {
		Destruct::Full(_) => {}
		#[cfg(feature = "exp-destruct")]
		Destruct::Skip => {}
		#[cfg(feature = "exp-destruct")]
		Destruct::Array {
			start,
			rest: _,
			end,
		} => {
			for d in start.iter().chain(end.iter()) {
				collect_destruct(d, out);
			}
		}
		#[cfg(feature = "exp-destruct")]
		Destruct::Object {
			fields, rest: _, ..
		} => {
			for (_, into, default) in fields {
				if let Some(d) = into {
					collect_destruct(d, out);
				}
				if let Some(expr) = default {
					expr.extend_used_into(out);
				}
			}
		}
	}
}

fn collect_params(params: &ParamsDesc, out: &mut FxHashSet<IStr>) {
	for Param(dest, default) in params.iter() {
		collect_destruct(dest, out);
		if let Some(default_expr) = default {
			default_expr.extend_used_into(out);
		}
	}
}

fn collect_args(args: &ArgsDesc, out: &mut FxHashSet<IStr>) {
	for arg in &args.unnamed {
		arg.extend_used_into(out);
	}
	for (_, arg) in &args.named {
		arg.extend_used_into(out);
	}
}

fn collect_index_part(part: &IndexPart, out: &mut FxHashSet<IStr>) {
	part.value.extend_used_into(out);
}

fn collect_slice_desc(desc: &SliceDesc, out: &mut FxHashSet<IStr>) {
	if let Some(s) = &desc.start {
		s.extend_used_into(out);
	}
	if let Some(e) = &desc.end {
		e.extend_used_into(out);
	}
	if let Some(s) = &desc.step {
		s.extend_used_into(out);
	}
}

fn collect_obj_body(body: &ObjBody, out: &mut FxHashSet<IStr>) {
	match body {
		ObjBody::MemberList(members) => {
			for member in members {
				match member {
					Member::Field(field) => collect_field_member(field, out),
					Member::BindStmt(bind) => collect_bind_spec(bind, out),
					Member::AssertStmt(AssertStmt(cond, msg)) => {
						cond.extend_used_into(out);
						if let Some(m) = msg {
							m.extend_used_into(out);
						}
					}
				}
			}
		}
		ObjBody::ObjComp(comp) => collect_obj_comp(comp, out),
	}
}

fn collect_field_member(field: &FieldMember, out: &mut FxHashSet<IStr>) {
	field.value.extend_used_into(out);
	if let FieldName::Dyn(expr) = &field.name {
		expr.extend_used_into(out);
	}
	if let Some(params) = &field.params {
		collect_params(params, out);
	}
}

fn collect_obj_comp(comp: &ObjComp, out: &mut FxHashSet<IStr>) {
	collect_field_member(&comp.field, out);
	for bind in comp.pre_locals.iter().chain(comp.post_locals.iter()) {
		collect_bind_spec(bind, out);
	}
	collect_comp_specs(&comp.compspecs, out);
}

fn collect_comp_specs(specs: &[CompSpec], out: &mut FxHashSet<IStr>) {
	for spec in specs {
		match spec {
			CompSpec::IfSpec(IfSpecData(cond)) => {
				cond.extend_used_into(out);
			}
			CompSpec::ForSpec(ForSpecData(dest, iter_expr)) => {
				collect_destruct(dest, out);
				iter_expr.extend_used_into(out);
			}
		}
	}
}
