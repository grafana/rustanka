//! Jrsonnet AST expression types.

use std::{
	fmt::{self, Debug, Display},
	ops::{Deref, RangeInclusive},
};

use jrsonnet_gcmodule::Acyclic;
use jrsonnet_interner::IStr;

use crate::{
	NumValue,
	function::{FunctionSignature, ParamDefault, ParamName, ParamParse},
	source::Source,
};

/// Field name in object/obj-comp definition.
#[derive(Debug, PartialEq, Acyclic)]
pub enum FieldName {
	/// `{fixed: 2}`
	Fixed(IStr),
	/// `{["dyn"+"amic"]: 3}` - may only reference locals defined outside of the object.
	Dyn(Expr),
}

/// Field visibility in object/obj-comp/exp-object-iteration definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Acyclic)]
#[repr(u8)]
pub enum Visibility {
	/// `:` - normal visibility, visible by default, but inherits field visibility from `super`.
	Normal,
	/// `::` - hidden visibility, field should not be visible by default manifest functions and iteration.
	Hidden,
	/// `:::` - unhide visibility, visible by default, ignores `super` visibility,
	/// can be overriden by [`Visibility::Hidden`]
	Unhide,
}

/// Trivial values are passed from the AST to the evaluator as-is and have trivial conversions from and to
/// jsonnet runtime values.
#[derive(Debug, Clone, PartialEq, Acyclic)]
pub enum TrivialVal {
	/// Jsonnet `null`.
	Null,
	/// Jsonnet `true` or `false`.
	Bool(bool),
	/// Jsonnet number, finite, non-nan, see [`NumValue`].
	Num(NumValue),
	/// Jsonnet interned flat string, see [`IStr`].
	Str(IStr),
}

impl Visibility {
	/// Is this visibility defines/matches (in exp-object-iteration) a visible field?
	///
	/// Note: [`Self::Unhide`] also matches hidden fields.
	pub fn is_visible(&self) -> bool {
		matches!(self, Self::Normal | Self::Unhide)
	}
}

/// Assert statement, used in `assert` expressions and for object assertions.
///
/// ```jsonnet
/// assert a == 1: "message"
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub struct AssertStmt {
	/// Assertion condition, `a == 1`.
	pub assertion: Spanned<Expr>,
	/// Message to be shown on assertion failure, `"message"`.
	pub message: Option<Expr>,
}

/// Object/obj-comp full field definition.
///
/// ```jsonnet
/// [name]+:: value,
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub struct FieldMember {
	/// Field name, possibly dynamic, see [`FieldName`].
	pub name: Spanned<FieldName>,
	/// Is this a `+:` field? (extends super field with the same name using `+`).
	pub plus: bool,
	/// Method definition syntax: `a(b): c` is equivalent to `a: function(b) c`.
	pub params: Option<ExprParams>,
	/// Field visibility, see [`Visibility`].
	pub visibility: Visibility,
	/// Field value.
	pub value: Expr,
}

/// Unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Acyclic)]
pub enum UnaryOpType {
	/// `+v`
	Plus,
	/// `-v`
	Minus,
	/// `~v`
	BitNot,
	/// `!v`
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

/// Jsonnet binary expression.
///
/// ```jsonnet
/// a + b
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Acyclic)]
pub enum BinaryOpType {
	/// `*`
	Mul,
	/// `/`
	Div,

	/// `%` - modulo for numbers, formatting for strings, equivalent to `std.mod(a, b)`.
	Mod,

	/// `+`
	Add,
	/// `-`
	Sub,

	/// `<<`
	Lhs,
	/// `>>`
	Rhs,

	/// `<`
	Lt,
	/// `>`
	Gt,
	/// `<=`
	Lte,
	/// `>=`
	Gte,

	/// `&`
	BitAnd,
	/// `|`
	BitOr,
	/// `^`
	BitXor,

	/// `==`
	Eq,
	/// `!=`
	Neq,

	/// `&&`
	And,
	/// `||`
	Or,
	/// `??`
	#[cfg(feature = "exp-null-coaelse")]
	NullCoaelse,

	/// `in` - equialent to `std.objectHasEx(a, b, true)`.
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

/// Parameter definition in function expression/method object member.
///
/// ```jsonnet
/// name = default
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub struct ExprParam {
	/// Destruct for the field value, if not [`Destruct::Full`] - can only be used with positional argument.
	pub destruct: Destruct,
	/// Default value.
	pub default: Option<Expr>,
}

/// Defined function parameters.
#[derive(Debug, PartialEq, Acyclic)]
pub struct ExprParams {
	/// Defined function parameters.
	pub exprs: Vec<ExprParam>,
	/// Cached function signature. TODO: Should be calculated by static analyzer.
	pub signature: FunctionSignature,
	pub(crate) binds_len: usize,
}
impl ExprParams {
	/// Number of all parameters, including those with set defaults.
	pub fn len(&self) -> usize {
		self.exprs.len()
	}
	/// Is this function has no parameters?
	pub fn is_empty(&self) -> bool {
		self.exprs.is_empty()
	}

	/// Amount of bound locals, equal to number of parameters without `exp-destruct`.
	pub fn binds_len(&self) -> usize {
		self.binds_len
	}
	/// Create an [`ExprParams`].
	pub fn new(exprs: Vec<ExprParam>) -> Self {
		Self {
			signature: FunctionSignature::new(
				exprs
					.iter()
					.map(|p| {
						ParamParse::new(
							p.destruct.name(),
							ParamDefault::exists(p.default.is_some()),
						)
					})
					.collect(),
			),
			binds_len: exprs.iter().map(|v| v.destruct.binds_len()).sum(),
			exprs,
		}
	}
}

/// Function call arguments.
///
/// ```jsonnet
/// call(unnamed1, unnamed2, named1 = value, named2 = value)
/// ```
///
/// TODO: should `tailstrict` be moved here?
#[derive(Debug, PartialEq, Acyclic)]
pub struct ArgsDesc {
	/// Positional argument values.
	pub unnamed: Vec<Expr>,
	/// Named argument names.
	pub names: Vec<IStr>,
	/// Named argument values.
	pub values: Vec<Expr>,
}
impl ArgsDesc {
	/// Construct an [`ArgsDesc`].
	pub fn new(unnamed: Vec<Expr>, names: Vec<IStr>, values: Vec<Expr>) -> Self {
		Self {
			unnamed,
			names,
			values,
		}
	}
}

/// In [`Destruct`], unmatched array elements/object fields should either not exist,
/// or be explicitly dropped [`DestructRest::Drop`] or collected into another local [`DestructRest::Keep`].
#[derive(Debug, PartialEq, Eq, Acyclic)]
pub enum DestructRest {
	/// `...rest`
	Keep(IStr),
	/// `...`
	Drop,
}

/// Local definition/destructuring (with `exp-destruct`).
///
/// `name` in `local name = value`.
#[derive(Debug, PartialEq, Acyclic)]
pub enum Destruct {
	/// `name` - normal local definition, value is assigned to the specified name.
	Full(Spanned<IStr>),
	/// `?` - value is ignored, mostly useful for nested destructuring of arrays/objects.
	#[cfg(feature = "exp-destruct")]
	Skip,
	/// `[a, b, ...rest, c]` - destructure the value as array.
	///
	/// If the value length is not matched by `start` and there is no `rest` - throws.
	#[cfg(feature = "exp-destruct")]
	Array {
		/// How to collect first elements.
		start: Vec<Destruct>,
		/// How the elements in between the `start` and `end` should be handled.
		rest: Option<DestructRest>,
		/// How to collect last elements. Might only be non-empty in presence of `rest`.
		end: Vec<Destruct>,
	},
	/// `{a, b = default, c: rename, ...rest}` - destructure the value as object.
	#[cfg(feature = "exp-destruct")]
	Object {
		/// Destructuring of the individual fields.
		///
		/// Source field name, target destructure (defaults to `Destruct::Full` with the source name),
		/// default value (defaults to throw on the match time due to match failure).
		#[allow(clippy::type_complexity)]
		fields: Vec<(IStr, Option<Destruct>, Option<Spanned<Expr>>)>,
		/// How the remaining fields should be handled, the result is similar to how `std.objectRemoveField` behaves.
		rest: Option<DestructRest>,
	},
}
impl Destruct {
	/// Name of destructure, used for function named parameter names.
	pub fn name(&self) -> ParamName {
		match self {
			Self::Full(name) => ParamName::Named(name.value.clone()),
			#[cfg(feature = "exp-destruct")]
			_ => ParamName::Unnamed,
		}
	}
	/// How many local names this [`Destruct`] will create, always `1` when `exp-destruct` feature is not enabled.
	pub fn binds_len(&self) -> usize {
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
				start.iter().map(Destruct::binds_len).sum::<usize>()
					+ end.iter().map(Destruct::binds_len).sum::<usize>()
					+ cap_rest(rest)
			}
			#[cfg(feature = "exp-destruct")]
			Self::Object { fields, rest } => {
				let mut out = 0;
				for (_, into, _) in fields {
					match into {
						Some(v) => out += v.binds_len(),
						// Field is destructured to default name
						None => out += 1,
					}
				}
				out + cap_rest(rest)
			}
		}
	}
}

/// Single element of `local` expression/value of the `local` object/obj-comp member.
///
/// ```jsonnet
/// local a = b;
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub enum BindSpec {
	/// Normal local definition/destructure: `local a = b;`.
	Field {
		/// Local name/destructure.
		into: Destruct,
		/// Value.
		value: Expr,
	},
	/// Method-like local: `local a(b) = c;`, equivalent to `local a = function(b) c;`.
	Function {
		/// Local name, can't be destructure since functions can't be destructured.
		name: Spanned<IStr>,
		/// Function parameters.
		params: ExprParams,
		/// Function body.
		value: Expr,
	},
}
impl BindSpec {
	/// How many locals will this local definition define.
	pub fn binds_len(&self) -> usize {
		match self {
			BindSpec::Field { into, .. } => into.binds_len(),
			BindSpec::Function { .. } => 1,
		}
	}
}

/// `if` expression/`if` compspec condition.
///
/// ```jsonnet
/// if a == b
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub struct IfSpecData {
	/// Span for the `if` token.
	pub span: Span,
	/// Condition expression.
	pub cond: Expr,
}

/// `for` compspec definition.
///
/// ```jsonnet
/// for a in b
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub struct ForSpecData {
	/// New local definition, will be set per `over` iteration.
	pub destruct: Destruct,
	/// Expression which should evaluate to array to be iterated over.
	pub over: Expr,
}

/// `for` compspec object iteration definition.
///
/// ```jsonnet
/// for [key]: value in obj
/// ```
///
/// TODO: `exp-preserve-order` integration.
#[derive(Debug, PartialEq, Acyclic)]
pub struct ForObjSpecData {
	/// Object field name, will be set per `over` field iteration.
	pub key: IStr,
	/// Filter for the iterated object fields,
	/// - `:` will iterate over visible fields.
	/// - `::` will iterate over only invisible fields.
	/// - `:::` will iterate over all defined fields.
	pub visibility: Visibility,
	/// Object value destructure, will be set per `over` field iteration.
	pub value: Destruct,
	/// Expression which should evaluate to object to be iterated over.
	pub over: Expr,
}

/// One element of array/obj-comp iteration definition.
///
/// `for a in b if c` in [1 for a in b if c].
#[derive(Debug, PartialEq, Acyclic)]
pub enum CompSpec {
	/// `if` filter, see [`IfSpecData`].
	IfSpec(IfSpecData),
	/// `for` array iterator, see [`ForSpecData`].
	ForSpec(ForSpecData),
	/// `for` object iterator, see [`ForObjSpecData`].
	#[cfg(feature = "exp-object-iteration")]
	ForObjSpec(ForObjSpecData),
}

/// Object constructed from dynamically constructed field list using iteration.
///
/// ```jsonnet
/// {
///   ["field_" + id]: value
///   for id in [1, 2, 3]
/// }
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub struct ObjComp {
	/// Locals to be defined to be used by object field values, may reference object identity.
	pub locals: Vec<BindSpec>,
	/// Field definition template, will be evaluated for every compspec iteration, normally uses [`FieldName::Dyn`].
	pub field: Box<FieldMember>,
	/// Compspec.
	pub compspecs: Vec<CompSpec>,
}

/// Object constructed from explicitly defined elements.
///
/// ```jsonnet
/// {
///   a: 1,
///   b: 2,
/// }
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub struct ObjMembers {
	/// Locals to be defined to be used by object field values, may reference object identity.
	pub locals: Vec<BindSpec>,
	/// Associated object assertions, checked on first time object field value is computed.
	pub asserts: Vec<AssertStmt>,
	/// Static list of field definitions.
	pub fields: Vec<FieldMember>,
}

/// Object constructed by any means.
#[derive(Debug, PartialEq, Acyclic)]
pub enum ObjBody {
	/// Static field list, see [`ObjMembers`].
	MemberList(ObjMembers),
	/// Field list is created by iteration, see [`ObjComp`].
	ObjComp(ObjComp),
}

/// Object identity reference: `self`, `super`, or `$`.#[derive(Debug, PartialEq, Eq, Clone, Copy, Acyclic)]
#[derive(Debug, PartialEq, Acyclic)]
pub enum IdentityKind {
	/// `self` - renamed to `this` for easier usage in the code.
	This,
	/// `super`.
	Super,
	/// `$`.
	Dollar,
}

/// Slice expression range/step.
///
/// `[start:end:step]` in `array[start:end:step]`.
#[derive(Debug, PartialEq, Acyclic)]
pub struct SliceDesc {
	/// Slice start (inclusive) - elements before are not included. Counts from the end when negative.
	///
	/// `0` when `None`.
	pub start: Option<Spanned<Expr>>,
	/// Slice end (exclusive) - elements after/at are not included. Counts from the end when negative.
	///
	/// `std.length(array)` when `None`.
	pub end: Option<Spanned<Expr>>,
	/// Slice step - every `step`'th element is included in the resulting array. `step >= 1`.
	///
	/// `1` when `None`.
	pub step: Option<Spanned<Expr>>,
}

/// Assert expression.
///
/// ```jsonnet
/// assert value; rest
/// ```
///
/// Asserts `value`, continues with the evaluation of `rest`.
#[derive(Debug, PartialEq, Acyclic)]
pub struct AssertExpr {
	/// Value to assert, see [`AssertStmt`].
	pub assert: AssertStmt,
	/// What value to return when the assertion is succeeded.
	pub rest: Expr,
}

/// Binary expression.
///
/// ```jsonnet
/// a + b
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub struct BinaryOp {
	/// Value to the left of binary operator.
	pub lhs: Expr,
	/// Operator to apply.
	pub op: BinaryOpType,
	/// Value to the right of binary operator.
	pub rhs: Expr,
}

/// Import expression kind.
///
/// `str` in `importstr "path"`.
#[derive(Debug, PartialEq, Acyclic, Clone, Copy)]
pub enum ImportKind {
	/// `import`
	Normal,
	/// `importstr`
	Str,
	/// `importbin`
	Bin,
}

/// If else expression.
///
/// ```jsonnet
/// if a then b else c
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub struct IfElse {
	/// Condition to evaluate, see [`IfSpecData`].
	pub cond: IfSpecData,
	/// Value to return if the condition is `true`.
	pub cond_then: Expr,
	/// Value to return if the condition is `false`, `null` when `None`.
	pub cond_else: Option<Expr>,
}

/// Slice expression.
///
/// ```jsonnet
/// value[start:end:slice]
/// ```
#[derive(Debug, PartialEq, Acyclic)]
pub struct Slice {
	/// Value to slice, should evaluate to iterable.
	pub value: Expr,
	/// Slice definition, see [`SliceDesc`].
	pub slice: SliceDesc,
}

/// Syntax base
#[derive(Debug, PartialEq, Acyclic)]
pub enum Expr {
	/// Object-identity reference: `self`, `super`, `$`.
	Identity(Span, IdentityKind),

	/// Trivial value literal, see [`TrivialVal`].
	Trivial(TrivialVal),

	/// Local reference.
	Var(Spanned<IStr>),

	/// Array of expressions.
	///
	/// ```jsonnet
	/// [1, 2, "Hello"]
	/// ```
	Arr(Vec<Expr>),
	/// Array comprehension.
	///
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
	ArrComp(Box<Expr>, Vec<CompSpec>),

	/// Object creation, see [`ObjBody`].
	///
	/// ```jsonnet
	/// {a: 2}
	/// ```
	Obj(ObjBody),
	/// Object extension.
	///
	/// ```jsonnet
	/// var1 {b: 2}
	/// ```
	///
	/// Equivalent to `var {b: 2}`.
	ObjExtend(Box<Expr>, ObjBody),

	/// Unary operator expression, see [`UnaryOpType`].
	///
	/// ```jsonnet
	/// -2
	/// ```
	UnaryOp(UnaryOpType, Box<Expr>),
	/// Binary operator expression, see [`BinaryOp`].
	///
	/// ```jsonnet
	/// 2 - 2
	/// ```
	BinaryOp(Box<BinaryOp>),
	/// Assertion expression, see [`AssertExpr`].
	///
	/// ```jsonnet
	/// assert 2 == 2 : "Math is broken"
	/// ```
	AssertExpr(Box<AssertExpr>),
	/// Local expression.
	///
	/// Defines locals using [`BindSpec`], then evaluates expression in the resulting context scope.
	///
	/// ```jsonnet
	/// local a = 2; { b: a }
	/// ```
	LocalExpr(Vec<BindSpec>, Box<Expr>),

	/// Import expression
	///
	/// ```jsonnet
	/// import "hello"
	/// ```
	Import(Spanned<ImportKind>, Box<Expr>),
	/// Error expression, immideately throws the error on evaluation.
	///
	/// ```jsonnet
	/// error "I'm broken"
	/// ```
	ErrorStmt(Span, Box<Expr>),
	/// Function call.
	///
	/// See [`ArgsDesc`].
	Apply(Box<Expr>, Spanned<ArgsDesc>, bool),
	/// Indexing chain.
	///
	/// `a[b]`, `a.b`, `a?.b`
	Index {
		/// Value to index into.
		indexable: Box<Expr>,
		/// Indexing operators to apply in sequence, see [`IndexPart`].
		parts: Vec<IndexPart>,
	},
	/// Function definition.
	///
	/// ```jsonnet
	/// function(x) x
	/// ```
	Function(Span, ExprParams, Box<Expr>),
	/// IfElse expression, see [`IfElse`]
	IfElse(Box<IfElse>),
	/// Slice expression, see [`Slice`]
	Slice(Box<Slice>),
}

/// Single index chain expression.
///
/// `.value` in `indexed.value`.
#[derive(Debug, PartialEq, Acyclic)]
pub struct IndexPart {
	/// Span encompassing the current index element definition.
	pub span: Span,
	/// Field/element to get from the indexable value.
	pub value: Expr,
	/// Null-coaelsing - interrupt the chain earlier returning `null` when the field is missing in the indexed value.
	#[cfg(feature = "exp-null-coaelse")]
	pub null_coaelse: bool,
}

/// A slice of source code, plus path and code itself. Contains begin byte offset, end byte offset.
#[derive(Clone, PartialEq, Eq, Acyclic)]
#[repr(C)]
pub struct Span(pub Source, pub u32, pub u32);
impl Span {
	/// Is this span a substring of the other span?
	pub fn belongs_to(&self, other: &Span) -> bool {
		other.0 == self.0 && other.1 <= self.1 && other.2 >= self.2
	}
	/// Span standard inclusive range value.
	pub fn range(&self) -> RangeInclusive<usize> {
		let start = self.1;
		let mut end = self.2;
		if end > start {
			// Because it is originally exclusive
			end -= 1;
		}
		start as usize..=end as usize
	}
}

#[cfg(target_pointer_width = "64")]
static_assertions::assert_eq_size!(Span, (usize, usize));

impl Debug for Span {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{:?}:{:?}-{:?}", self.0, self.1, self.2)
	}
}

/// Spanned expression.
#[derive(Clone, PartialEq, Acyclic)]
pub struct Spanned<T: Acyclic> {
	/// Spanned expression.
	pub value: T,
	/// Span.
	pub span: Span,
}
impl<T: Acyclic> Deref for Spanned<T> {
	type Target = T;
	fn deref(&self) -> &Self::Target {
		&self.value
	}
}
impl<T: Acyclic> Spanned<T> {
	/// Construct the spanned expression.
	#[inline]
	pub fn new(value: T, span: Span) -> Self {
		Self { value, span }
	}
	/// Map the spanned expression, leaving span as is.
	pub fn map<U: Acyclic>(self, v: impl FnOnce(T) -> U) -> Spanned<U> {
		Spanned {
			span: self.span,
			value: v(self.value),
		}
	}
	/// Create [`Spanned`] with the reference to the expression.
	pub fn as_ref<'a>(&'a self) -> Spanned<&'a T>
	where
		&'a T: Acyclic,
	{
		Spanned {
			span: self.span.clone(),
			value: &self.value,
		}
	}
}

impl<T: Debug + Acyclic> Debug for Spanned<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let expr = &**self;
		if f.alternate() {
			write!(f, "{:#?}", expr)?;
		} else {
			write!(f, "{:?}", expr)?;
		}
		write!(f, " from {:?}", self.span)?;
		Ok(())
	}
}
