use std::{
	fmt::{self, Debug, Display},
	ops::Deref,
	rc::Rc,
};

use jrsonnet_gcmodule::Acyclic;
use jrsonnet_interner::IStr;
use rustc_hash::FxHashSet;

use crate::source::Source;
use crate::used_vars::{compute_used_vars, UsedVars};

#[derive(Debug, PartialEq, Acyclic)]
pub enum FieldName {
	/// {fixed: 2}
	Fixed(IStr),
	/// {["dyn"+"amic"]: 3}
	Dyn(AnalyzedExpr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Acyclic)]
#[repr(u8)]
pub enum Visibility {
	/// :
	Normal,
	/// ::
	Hidden,
	/// :::
	Unhide,
}

impl Visibility {
	pub fn is_visible(&self) -> bool {
		matches!(self, Self::Normal | Self::Unhide)
	}
}

#[derive(Clone, Debug, PartialEq, Acyclic)]
pub struct AssertStmt(pub AnalyzedExpr, pub Option<AnalyzedExpr>);

#[derive(Debug, PartialEq, Acyclic)]
pub struct FieldMember {
	pub name: FieldName,
	pub plus: bool,
	pub params: Option<ParamsDesc>,
	pub visibility: Visibility,
	pub value: AnalyzedExpr,
}

#[derive(Debug, PartialEq, Acyclic)]
pub enum Member {
	Field(FieldMember),
	BindStmt(BindSpec),
	AssertStmt(AssertStmt),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Acyclic)]
pub enum UnaryOpType {
	Plus,
	Minus,
	BitNot,
	Not,
}

impl Display for UnaryOpType {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		use UnaryOpType::*;
		write!(
			f,
			"{}",
			match self {
				Plus => "+",
				Minus => "-",
				BitNot => "~",
				Not => "!",
			}
		)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Acyclic)]
pub enum BinaryOpType {
	Mul,
	Div,

	/// Implemented as intrinsic, put here for completeness
	Mod,

	Add,
	Sub,

	Lhs,
	Rhs,

	Lt,
	Gt,
	Lte,
	Gte,

	BitAnd,
	BitOr,
	BitXor,

	Eq,
	Neq,

	And,
	Or,
	#[cfg(feature = "exp-null-coaelse")]
	NullCoaelse,

	// Equialent to std.objectHasEx(a, b, true)
	In,
}

impl Display for BinaryOpType {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		use BinaryOpType::*;
		write!(
			f,
			"{}",
			match self {
				Mul => "*",
				Div => "/",
				Mod => "%",
				Add => "+",
				Sub => "-",
				Lhs => "<<",
				Rhs => ">>",
				Lt => "<",
				Gt => ">",
				Lte => "<=",
				Gte => ">=",
				BitAnd => "&",
				BitOr => "|",
				BitXor => "^",
				Eq => "==",
				Neq => "!=",
				And => "&&",
				Or => "||",
				In => "in",
				#[cfg(feature = "exp-null-coaelse")]
				NullCoaelse => "??",
			}
		)
	}
}

/// name, default value
#[derive(Debug, PartialEq, Acyclic)]
pub struct Param(pub Destruct, pub Option<AnalyzedExpr>);

/// Defined function parameters
#[derive(Debug, Clone, PartialEq, Acyclic)]
pub struct ParamsDesc(pub Rc<Vec<Param>>);

impl Deref for ParamsDesc {
	type Target = Vec<Param>;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

#[derive(Debug, PartialEq, Acyclic)]
pub struct ArgsDesc {
	pub unnamed: Vec<AnalyzedExpr>,
	pub named: Vec<(IStr, AnalyzedExpr)>,
}
impl ArgsDesc {
	pub fn new(unnamed: Vec<AnalyzedExpr>, named: Vec<(IStr, AnalyzedExpr)>) -> Self {
		Self { unnamed, named }
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Acyclic)]
pub enum DestructRest {
	/// ...rest
	Keep(IStr),
	/// ...
	Drop,
}

#[derive(Debug, Clone, PartialEq, Acyclic)]
pub enum Destruct {
	Full(IStr),
	#[cfg(feature = "exp-destruct")]
	Skip,
	#[cfg(feature = "exp-destruct")]
	Array {
		start: Vec<Destruct>,
		rest: Option<DestructRest>,
		end: Vec<Destruct>,
	},
	#[cfg(feature = "exp-destruct")]
	Object {
		fields: Vec<(IStr, Option<Destruct>, Option<AnalyzedExpr>)>,
		rest: Option<DestructRest>,
	},
}
impl Destruct {
	/// Name of destructure, used for function parameter names
	pub fn name(&self) -> Option<IStr> {
		match self {
			Self::Full(name) => Some(name.clone()),
			#[cfg(feature = "exp-destruct")]
			_ => None,
		}
	}
	pub fn capacity_hint(&self) -> usize {
		#[cfg(feature = "exp-destruct")]
		fn cap_rest(rest: &Option<DestructRest>) -> usize {
			match rest {
				Some(DestructRest::Keep(_)) => 1,
				Some(DestructRest::Drop) => 0,
				None => 0,
			}
		}
		match self {
			Self::Full(_) => 1,
			#[cfg(feature = "exp-destruct")]
			Self::Skip => 0,
			#[cfg(feature = "exp-destruct")]
			Self::Array { start, rest, end } => {
				start.iter().map(Destruct::capacity_hint).sum::<usize>()
					+ end.iter().map(Destruct::capacity_hint).sum::<usize>()
					+ cap_rest(rest)
			}
			#[cfg(feature = "exp-destruct")]
			Self::Object { fields, rest } => {
				let mut out = 0;
				for (_, into, _) in fields {
					match into {
						Some(v) => out += v.capacity_hint(),
						// Field is destructured to default name
						None => out += 1,
					}
				}
				out + cap_rest(rest)
			}
		}
	}
}

#[derive(Debug, Clone, PartialEq, Acyclic)]
pub enum BindSpec {
	Field {
		into: Destruct,
		value: AnalyzedExpr,
	},
	Function {
		name: IStr,
		params: ParamsDesc,
		value: AnalyzedExpr,
	},
}
impl BindSpec {
	pub fn capacity_hint(&self) -> usize {
		match self {
			BindSpec::Field { into, .. } => into.capacity_hint(),
			BindSpec::Function { .. } => 1,
		}
	}
}

#[derive(Debug, PartialEq, Acyclic)]
pub struct IfSpecData(pub AnalyzedExpr);

#[derive(Debug, PartialEq, Acyclic)]
pub struct ForSpecData(pub Destruct, pub AnalyzedExpr);

#[derive(Debug, PartialEq, Acyclic)]
pub enum CompSpec {
	IfSpec(IfSpecData),
	ForSpec(ForSpecData),
}

#[derive(Debug, PartialEq, Acyclic)]
pub struct ObjComp {
	pub pre_locals: Vec<BindSpec>,
	pub field: FieldMember,
	pub post_locals: Vec<BindSpec>,
	pub compspecs: Vec<CompSpec>,
}

#[derive(Debug, PartialEq, Acyclic)]
pub enum ObjBody {
	MemberList(Vec<Member>),
	ObjComp(ObjComp),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Acyclic)]
pub enum LiteralType {
	This,
	Super,
	Dollar,
	Null,
	True,
	False,
}

#[derive(Debug, PartialEq, Acyclic)]
pub struct SliceDesc {
	pub start: Option<AnalyzedExpr>,
	pub end: Option<AnalyzedExpr>,
	pub step: Option<AnalyzedExpr>,
}

/// Syntax base
#[derive(Debug, PartialEq, Acyclic)]
pub enum Expr {
	Literal(LiteralType),

	/// String value: "hello"
	Str(IStr),
	/// Number: 1, 2.0, 2e+20
	Num(f64),
	/// Variable name: test
	Var(IStr),

	/// Array of expressions: [1, 2, "Hello"]
	Arr(Vec<AnalyzedExpr>),
	/// Array comprehension:
	/// ```jsonnet
	///  ingredients: [
	///    { kind: kind, qty: 4 / 3 }
	///    for kind in [
	///      'Honey Syrup',
	///      'Lemon Juice',
	///      'Farmers Gin',
	///    ]
	///  ],
	/// ```
	ArrComp(AnalyzedExpr, Vec<CompSpec>),

	/// Object: {a: 2}
	Obj(ObjBody),
	/// Object extension: var1 {b: 2}
	ObjExtend(AnalyzedExpr, ObjBody),

	/// (obj)
	Parened(AnalyzedExpr),

	/// -2
	UnaryOp(UnaryOpType, AnalyzedExpr),
	/// 2 - 2
	BinaryOp(AnalyzedExpr, BinaryOpType, AnalyzedExpr),
	/// assert 2 == 2 : "Math is broken"
	AssertExpr(AssertStmt, AnalyzedExpr),
	/// local a = 2; { b: a }
	LocalExpr(Vec<BindSpec>, AnalyzedExpr),

	/// import "hello"
	Import(AnalyzedExpr),
	/// importStr "file.txt"
	ImportStr(AnalyzedExpr),
	/// importBin "file.txt"
	ImportBin(AnalyzedExpr),
	/// error "I'm broken"
	ErrorStmt(AnalyzedExpr),
	/// a(b, c)
	Apply(AnalyzedExpr, ArgsDesc, bool),
	/// a[b], a.b, a?.b
	Index {
		indexable: AnalyzedExpr,
		parts: Vec<IndexPart>,
	},
	/// function(x) x
	Function(ParamsDesc, AnalyzedExpr),
	/// if true == false then 1 else 2
	IfElse {
		cond: IfSpecData,
		cond_then: AnalyzedExpr,
		cond_else: Option<AnalyzedExpr>,
	},
	Slice(AnalyzedExpr, SliceDesc),
}

#[derive(Debug, PartialEq, Acyclic)]
pub struct IndexPart {
	pub value: AnalyzedExpr,
	#[cfg(feature = "exp-null-coaelse")]
	pub null_coaelse: bool,
}

/// file, begin offset, end offset
#[derive(Clone, PartialEq, Eq, Acyclic)]
#[repr(C)]
pub struct Span(pub Source, pub u32, pub u32);
impl Span {
	pub fn belongs_to(&self, other: &Span) -> bool {
		other.0 == self.0 && other.1 <= self.1 && other.2 >= self.2
	}
}

static_assertions::assert_eq_size!(Span, (usize, usize));

impl Debug for Span {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{:?}:{:?}-{:?}", self.0, self.1, self.2)
	}
}

/// Information about an expression that can be used for optimization.
#[derive(Debug, Clone, Acyclic)]
pub struct Analysis {
	/// Single variable referenced by this expression (for `Var` nodes).
	/// Avoids allocating a hashset for the common single-variable case.
	pub var: Option<IStr>,
	/// Set of variable names referenced (used) by this expression and its sub-expressions.
	pub used_vars: UsedVars,
}

/// Internal representation of an analyzed expression node.
#[derive(Debug, Acyclic)]
struct AnalyzedExprInternals {
	expr: Expr,
	span: Span,
	analysis: Analysis,
}

/// Holds AST expression, its location in source file, and used-variable analysis.
#[derive(Clone, Acyclic)]
pub struct AnalyzedExpr(Rc<AnalyzedExprInternals>);

/// Backward-compatible alias.
pub type LocExpr = AnalyzedExpr;

impl AnalyzedExpr {
	pub fn new(expr: Expr, span: Span) -> Self {
		let var = match &expr {
			Expr::Var(name) => Some(name.clone()),
			_ => None,
		};
		let used_vars = compute_used_vars(&expr);
		Self(Rc::new(AnalyzedExprInternals {
			expr,
			span,
			analysis: Analysis { var, used_vars },
		}))
	}
	#[inline]
	pub fn span(&self) -> Span {
		self.0.span.clone()
	}
	#[inline]
	pub fn expr(&self) -> &Expr {
		&self.0.expr
	}
	#[inline]
	pub fn analysis(&self) -> &Analysis {
		&self.0.analysis
	}
	#[inline]
	pub fn used_vars(&self) -> &UsedVars {
		&self.0.analysis.used_vars
	}
	/// Insert all variable names used by this expression into `target`.
	/// Handles both the singleton `var` field and the `used_vars` set.
	#[inline]
	pub fn extend_used_into(&self, target: &mut FxHashSet<IStr>) {
		if let Some(var) = &self.0.analysis.var {
			target.insert(var.clone());
		}
		self.0.analysis.used_vars.extend_into(target);
	}
}

impl PartialEq for AnalyzedExpr {
	fn eq(&self, other: &Self) -> bool {
		self.0.expr == other.0.expr && self.0.span == other.0.span
	}
}

static_assertions::assert_eq_size!(AnalyzedExpr, usize);

impl Debug for AnalyzedExpr {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let expr = self.expr();
		if f.alternate() {
			write!(f, "{:#?}", expr)?;
		} else {
			write!(f, "{:?}", expr)?;
		}
		write!(f, " from {:?}", self.span())?;
		Ok(())
	}
}
