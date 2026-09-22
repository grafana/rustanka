//! A port of go-jsonnet's `ast/ast.go`, as the *formatter* uses it.
//!
//! This is not a generic syntax tree and must not be tidied into one. The
//! unparser reads **named slots per node type**, and their identity is
//! load-bearing: [`Index`] uses one slot for the fodder before a `]` and for
//! the fodder before an identifier, and [`Slice`] treats the *emptiness* of
//! [`Slice::step_colon_fodder`] as meaningful. Every slot here is named after
//! the Go field it ports, and the ones that do two jobs say so.
//!
//! Every node kind `internal/parser/parser.go` constructs is here, and nothing
//! else. `ast.DesugaredObject`, `ast.DesugaredObjectField` and
//! `ast.CommaSeparatedID` are deliberately absent: they appear during
//! desugaring, which the formatter never does — it parses with
//! `SnippetToRawAST` and unparses.
//!
//! # Three deliberate departures from the Go struct layout
//!
//! Each is behaviour-identical for the formatter, and each is here because a
//! literal transcription would be either impossible or misleading in Rust.
//!
//! 1. **`Method` and `Fun` carry no body.** Go types both as `*ast.Function`
//!    and aliases the body: "If Method is set then Expr2 == Method.Body". Rust
//!    cannot alias, and it does not have to — nothing in `internal/formatter`
//!    or `internal/pass` ever reads `Method.Body` or `Fun.Body`. Every one of
//!    the nine use sites reads only the parens, the parameters and the trailing
//!    comma, and the field's value is written from `Expr2` (or the bind's
//!    `Body`) instead. Only the desugarer follows the alias, with
//!    `field.Expr2 = field.Method`, and the formatter does not desugar. So both
//!    are [`FunctionSugar`], which holds exactly the four things that are read.
//!
//! 2. **`NodeBase` keeps only fodder and a location.** Go also carries `Ctx`
//!    and `FreeVars`; the parser fills `FreeVars` with an empty slice and the
//!    formatter reads neither.
//!
//! 3. **`ObjectField`, `Parameter` and `LocalBind` keep no location.** Theirs
//!    are read by the desugarer and by `internal/parser/context.go`, neither of
//!    which runs here. [`Node::loc`] *is* kept, because one formatter path
//!    reads it: `EnforceStringStyle` hands it to `StringUnescape`, which puts
//!    it in an error message.

use crate::{fodder::Fodder, location::LocationRange};

/// `ast.Identifier`.
///
/// A plain alias rather than a newtype: Go's is `type Identifier string`, and
/// the formatter only ever writes it out or compares it.
pub type Identifier = String;

/// `ast.NodeBase` plus the kind, which together are Go's `ast.Node`.
///
/// Go embeds `NodeBase` in each node struct and dispatches on an interface.
/// One struct carrying the common fields is closer to that than putting the
/// fodder in every variant would be, and it is what makes
/// [`Node::open_fodder`] a field access rather than a match — which matters,
/// because the passes reach for it on every node they visit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
	/// The fodder before the node's first token.
	///
	/// **Empty on a left-recursive node.** Where the first token actually
	/// belongs to a sub-expression, the fodder is stored as far inside the tree
	/// as possible, so on an [`Apply`], [`ApplyBrace`], [`Binary`], [`Index`],
	/// [`InSuper`] or [`Slice`] this is empty and the fodder sits on the
	/// leftmost leaf. [`Node::left_recursive`] is the same rule, and
	/// `unparse` fills this only where that returns `None`.
	pub fodder: Fodder,
	pub loc: LocationRange,
	pub kind: NodeKind,
}

impl Node {
	pub fn new(loc: LocationRange, fodder: Fodder, kind: NodeKind) -> Self {
		Self { fodder, loc, kind }
	}

	/// `NodeBase.OpenFodder`.
	pub fn open_fodder(&self) -> &Fodder {
		&self.fodder
	}

	/// `NodeBase.OpenFodder`, for the passes, which mutate it in place.
	pub fn open_fodder_mut(&mut self) -> &mut Fodder {
		&mut self.fodder
	}

	/// `leftRecursive` from `internal/formatter/jsonnetfmt.go`: the child whose
	/// first token is also this node's first token, if there is one.
	pub fn left_recursive(&self) -> Option<&Node> {
		match &self.kind {
			NodeKind::Apply(node) => Some(&node.target),
			NodeKind::ApplyBrace(node) => Some(&node.left),
			NodeKind::Binary(node) => Some(&node.left),
			NodeKind::Index(node) => Some(&node.target),
			NodeKind::InSuper(node) => Some(&node.index),
			NodeKind::Slice(node) => Some(&node.target),
			_ => None,
		}
	}

	/// `leftRecursive`, mutably.
	fn left_recursive_mut(&mut self) -> Option<&mut Node> {
		match &mut self.kind {
			NodeKind::Apply(node) => Some(&mut node.target),
			NodeKind::ApplyBrace(node) => Some(&mut node.left),
			NodeKind::Binary(node) => Some(&mut node.left),
			NodeKind::Index(node) => Some(&mut node.target),
			NodeKind::InSuper(node) => Some(&mut node.index),
			NodeKind::Slice(node) => Some(&mut node.target),
			_ => None,
		}
	}

	/// `leftRecursiveDeep`: the transitive closure, which is `self` when there
	/// is no left-recursive child.
	pub fn left_recursive_deep(&self) -> &Node {
		let mut last = self;
		while let Some(left) = last.left_recursive() {
			last = left;
		}
		last
	}

	/// `openFodder`: the fodder that actually precedes this node's first token,
	/// wherever in the tree it is stored.
	///
	/// Recursive where Go's is a loop, because the loop form is the borrow
	/// checker's classic sticking point: reassigning the cursor from a reborrow
	/// of itself, or using it again after a failed `match`, both need borrows
	/// that outlive what NLL will give. The recursion takes one fresh borrow
	/// per level instead, and its depth is bounded by left-recursive nesting —
	/// the same bound the parser and every pass already recurse to.
	pub fn opening_fodder_mut(&mut self) -> &mut Fodder {
		if self.left_recursive().is_some() {
			return self
				.left_recursive_mut()
				.expect("just checked there is a left-recursive child")
				.opening_fodder_mut();
		}
		&mut self.fodder
	}
}

/// One AST node's payload. Go dispatches on the `ast.Node` interface instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
	Apply(Apply),
	ApplyBrace(ApplyBrace),
	Array(Array),
	ArrayComp(ArrayComp),
	Assert(Assert),
	Binary(Binary),
	Conditional(Conditional),
	/// `$`.
	Dollar,
	Error(Error),
	Function(Function),
	Import(Import),
	ImportStr(Import),
	ImportBin(Import),
	Index(Index),
	InSuper(InSuper),
	LiteralBoolean(bool),
	/// `null`.
	LiteralNull,
	LiteralNumber(LiteralNumber),
	LiteralString(LiteralString),
	Local(Local),
	Object(Object),
	ObjectComp(ObjectComp),
	Parens(Parens),
	/// `self`. Named with a suffix because `Self` is reserved in Rust.
	SelfExpr,
	Slice(Slice),
	SuperIndex(SuperIndex),
	Unary(Unary),
	Var(Identifier),
}

impl NodeKind {
	/// The name the node oracle records for this kind, which is Go's type name.
	///
	/// `make update-fmt-node-oracle` writes these strings, so they are a
	/// contract rather than a debug convenience.
	pub fn name(&self) -> &'static str {
		match self {
			Self::Apply(_) => "Apply",
			Self::ApplyBrace(_) => "ApplyBrace",
			Self::Array(_) => "Array",
			Self::ArrayComp(_) => "ArrayComp",
			Self::Assert(_) => "Assert",
			Self::Binary(_) => "Binary",
			Self::Conditional(_) => "Conditional",
			Self::Dollar => "Dollar",
			Self::Error(_) => "Error",
			Self::Function(_) => "Function",
			Self::Import(_) => "Import",
			Self::ImportStr(_) => "ImportStr",
			Self::ImportBin(_) => "ImportBin",
			Self::Index(_) => "Index",
			Self::InSuper(_) => "InSuper",
			Self::LiteralBoolean(_) => "LiteralBoolean",
			Self::LiteralNull => "LiteralNull",
			Self::LiteralNumber(_) => "LiteralNumber",
			Self::LiteralString(_) => "LiteralString",
			Self::Local(_) => "Local",
			Self::Object(_) => "Object",
			Self::ObjectComp(_) => "ObjectComp",
			Self::Parens(_) => "Parens",
			Self::SelfExpr => "Self",
			Self::Slice(_) => "Slice",
			Self::SuperIndex(_) => "SuperIndex",
			Self::Unary(_) => "Unary",
			Self::Var(_) => "Var",
		}
	}
}

/// `ast.Apply`: a function call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Apply {
	pub target: Box<Node>,
	/// Before the `(`.
	pub fodder_left: Fodder,
	pub arguments: Arguments,
	/// Before the `)`.
	pub fodder_right: Fodder,
	/// Before `tailstrict`. Read only when [`Apply::tail_strict`] is set.
	pub tail_strict_fodder: Fodder,
	/// Always false where there were no arguments.
	pub trailing_comma: bool,
	pub tail_strict: bool,
}

/// `ast.Arguments`: positional and named arguments to a call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Arguments {
	pub positional: Vec<CommaSeparatedExpr>,
	pub named: Vec<NamedArgument>,
}

/// `ast.CommaSeparatedExpr`: an element of a comma-separated list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommaSeparatedExpr {
	pub expr: Box<Node>,
	pub comma_fodder: Fodder,
}

/// `ast.NamedArgument`: `x=1` at a call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedArgument {
	pub name_fodder: Fodder,
	pub name: Identifier,
	/// Before the `=`.
	///
	/// **The unparser never reads this.** `unparseParams` fills
	/// [`Parameter::eq_fodder`] for a function *parameter*, but the
	/// named-argument branch of `Apply` writes `=` directly, so
	/// `f(b /* x */ = 2)` loses that comment in `tk fmt`. The parser fills it
	/// all the same, and so must this port, or the AST will not match
	/// go-jsonnet's.
	pub eq_fodder: Fodder,
	pub arg: Box<Node>,
	pub comma_fodder: Fodder,
}

/// `ast.ApplyBrace`: `e { }`, which desugars to `e + { }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyBrace {
	pub left: Box<Node>,
	pub right: Box<Node>,
}

/// `ast.Array`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Array {
	pub elements: Vec<CommaSeparatedExpr>,
	/// Before the `]`.
	pub close_fodder: Fodder,
	/// Always false where there were no elements.
	pub trailing_comma: bool,
}

/// `ast.ArrayComp`: `[e for x in e]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayComp {
	pub body: Box<Node>,
	/// Before the comma that may follow the body, which a comprehension allows
	/// where an array's trailing comma would sit.
	pub trailing_comma_fodder: Fodder,
	pub spec: ForSpec,
	/// Before the `]`.
	pub close_fodder: Fodder,
	pub trailing_comma: bool,
}

/// `ast.Assert`: an `assert` expression, not an object-level assert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assert {
	pub cond: Box<Node>,
	/// `None` where no message was given, in which case
	/// [`Assert::colon_fodder`] is never read.
	pub message: Option<Box<Node>>,
	pub rest: Box<Node>,
	pub colon_fodder: Fodder,
	pub semicolon_fodder: Fodder,
}

/// `ast.Binary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binary {
	pub left: Box<Node>,
	pub op_fodder: Fodder,
	pub op: BinaryOp,
	pub right: Box<Node>,
}

/// `ast.Conditional`: `if/then/else`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conditional {
	pub cond: Box<Node>,
	pub then_fodder: Fodder,
	pub branch_true: Box<Node>,
	pub else_fodder: Fodder,
	/// `None` where there was no `else`, in which case
	/// [`Conditional::else_fodder`] is never read.
	pub branch_false: Option<Box<Node>>,
}

/// `ast.Error`: `error e`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
	pub expr: Box<Node>,
}

/// `ast.Function`: `function(x) e`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
	pub paren_left_fodder: Fodder,
	pub parameters: Vec<Parameter>,
	/// Always false where there were no parameters.
	pub trailing_comma: bool,
	pub paren_right_fodder: Fodder,
	pub body: Box<Node>,
}

/// The parameter list of a method or of a `local` function bind.
///
/// Go types both as `*ast.Function` and aliases the body into the field's
/// `Expr2` or the bind's `Body`. Nothing in the formatter reads that body — see
/// the module documentation — so this holds only what is read. There is no
/// opening fodder either, because there was no `function` keyword to precede.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSugar {
	pub paren_left_fodder: Fodder,
	pub parameters: Vec<Parameter>,
	pub trailing_comma: bool,
	pub paren_right_fodder: Fodder,
}

/// `ast.Parameter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
	pub name_fodder: Fodder,
	pub name: Identifier,
	/// Before the `=`. Read only where [`Parameter::default_arg`] is set, which
	/// is also what makes the parameter optional rather than positional.
	pub eq_fodder: Fodder,
	pub default_arg: Option<Box<Node>>,
	pub comma_fodder: Fodder,
}

/// `ast.Import`, `ast.ImportStr` and `ast.ImportBin`, which differ only in
/// their keyword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
	/// Always a [`LiteralString`] node in a well-formed tree; the parser
	/// refuses anything else with "Computed imports are not allowed".
	pub file: Box<Node>,
}

/// `ast.Index`: both `e[e]` and the sugar `e.f`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
	pub target: Box<Node>,
	/// Before the `[` when [`Index::index`] is used, and before the `.` when
	/// [`Index::id`] is.
	pub left_bracket_fodder: Fodder,
	/// Before the `]` when [`Index::index`] is used, and before the identifier
	/// when [`Index::id`] is — one slot, two jobs, which is why this shape is
	/// ported rather than an approximation of it.
	pub right_bracket_fodder: Fodder,
	/// Exactly one of this and [`Index::index`] is set.
	pub id: Option<Identifier>,
	pub index: Option<Box<Node>>,
}

/// `ast.Slice`: `a[begin:end:step]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice {
	pub target: Box<Node>,
	pub left_bracket_fodder: Fodder,
	pub begin_index: Option<Box<Node>>,
	pub end_colon_fodder: Fodder,
	pub end_index: Option<Box<Node>>,
	/// Before the second `:`.
	///
	/// **Its emptiness is meaningful.** The unparser writes the step colon when
	/// [`Slice::step`] is set *or* this is non-empty, so an empty one is not
	/// the same as an absent one. Two consequences, both pinned by snippets:
	/// `a[::]` parses to the same tree as `a[:]`, because the lexer makes `::`
	/// one operator token and the parser's `::` branch never assigns this slot;
	/// and `a[1:2:]` writes back as `a[1:2]`, because a colon with no fodder
	/// before it leaves this empty and so invisible to the unparser.
	pub step_colon_fodder: Fodder,
	pub step: Option<Box<Node>>,
	pub right_bracket_fodder: Fodder,
}

/// `ast.SuperIndex`: `super.f` and `super[e]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperIndex {
	/// Before the `.` in `super.f`, and before the `[` in `super[e]`.
	pub dot_fodder: Fodder,
	/// Before the `f` in `super.f`, and before the `]` in `super[e]`. The same
	/// doubling as [`Index`], under different names.
	pub id_fodder: Fodder,
	/// Exactly one of this and [`SuperIndex::index`] is set.
	pub id: Option<Identifier>,
	pub index: Option<Box<Node>>,
}

/// `ast.InSuper`: `e in super`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InSuper {
	pub index: Box<Node>,
	pub in_fodder: Fodder,
	pub super_fodder: Fodder,
}

/// `ast.LocalBind`: one binding of a `local`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBind {
	pub var_fodder: Fodder,
	pub variable: Identifier,
	/// The sugar of `local f(x) = e`. See [`FunctionSugar`].
	pub fun: Option<FunctionSugar>,
	pub eq_fodder: Fodder,
	pub body: Box<Node>,
	/// Before the closing `,` or `;`, whichever this bind ends with.
	pub close_fodder: Fodder,
}

/// `ast.Local`: `local x = e; e`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
	/// Never empty: the unparser panics on a `local` with no binds, and the
	/// parser cannot produce one.
	pub binds: Vec<LocalBind>,
	pub body: Box<Node>,
}

/// `ast.LiteralNumber`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralNumber {
	/// The number exactly as it was written, which is what the unparser writes
	/// back — so `1.0` and `1.000` are different programs to the formatter even
	/// though they are the same number. No parsed value is kept, because
	/// nothing in the formatter needs one.
	pub original_string: String,
}

/// `ast.LiteralStringKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralStringKind {
	Single,
	Double,
	Block,
	VerbatimDouble,
	VerbatimSingle,
}

impl LiteralStringKind {
	/// `LiteralStringKind.FullyEscaped`: whether the kind may contain escape
	/// sequences that need unescaping.
	///
	/// This is what decides whether [`LiteralString::value`] still holds the
	/// source's escapes or has already been unescaped by the parser.
	pub fn fully_escaped(self) -> bool {
		matches!(self, Self::Single | Self::Double)
	}

	/// The name the node oracle records.
	pub fn name(self) -> &'static str {
		match self {
			Self::Single => "StringSingle",
			Self::Double => "StringDouble",
			Self::Block => "StringBlock",
			Self::VerbatimDouble => "VerbatimStringDouble",
			Self::VerbatimSingle => "VerbatimStringSingle",
		}
	}
}

/// `ast.LiteralString`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralString {
	/// For a fully escaped kind, the text **with its original escape sequences
	/// still in it**, because the unparser writes them back untouched. For a
	/// verbatim kind the parser has already collapsed the doubled quotes, and
	/// the unparser puts them back.
	pub value: String,
	/// The indentation of a `|||` block, reproduced verbatim.
	pub block_indent: String,
	/// The indentation before the closing `|||`.
	pub block_term_indent: String,
	pub kind: LiteralStringKind,
}

/// `ast.ObjectFieldKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFieldKind {
	/// `<f1> 'assert' <expr2> [<opF> ':' <expr3>] <commaF>`.
	Assert,
	/// `<f1> <id> <colon> <expr2> <commaF>`.
	FieldId,
	/// `<f1> '[' <expr1> <f2> ']' <colon> <expr2> <commaF>`.
	FieldExpr,
	/// `<expr1> <colon> <expr2> <commaF>` — note it has no `f1`, because the
	/// expression carries its own opening fodder.
	FieldStr,
	/// `<f1> 'local' <f2> <id> '=' <expr2> <commaF>`.
	Local,
}

impl ObjectFieldKind {
	/// The name the node oracle records.
	pub fn name(self) -> &'static str {
		match self {
			Self::Assert => "ObjectAssert",
			Self::FieldId => "ObjectFieldID",
			Self::FieldExpr => "ObjectFieldExpr",
			Self::FieldStr => "ObjectFieldStr",
			Self::Local => "ObjectLocal",
		}
	}
}

/// `ast.ObjectFieldHide`: a field's visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFieldHide {
	/// `f:: e`.
	Hidden,
	/// `f: e`.
	Inherit,
	/// `f::: e`.
	Visible,
}

impl ObjectFieldHide {
	/// The name the node oracle records.
	pub fn name(self) -> &'static str {
		match self {
			Self::Hidden => "Hidden",
			Self::Inherit => "Inherit",
			Self::Visible => "Visible",
		}
	}
}

/// `ast.ObjectField`.
///
/// One struct for all five kinds, as Go has it, rather than five types. Which
/// slots are read depends on [`ObjectField::kind`], and the unparser's
/// `unparseFields` is the authority on that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectField {
	pub kind: ObjectFieldKind,
	/// Ignored unless the kind is one of the three basic fields.
	pub hide: ObjectFieldHide,
	/// `+:`. Ignored unless the kind is one of the three basic fields.
	pub super_sugar: bool,
	/// The method sugar of `f(x): e`. See [`FunctionSugar`].
	pub method: Option<FunctionSugar>,
	pub id: Option<Identifier>,
	/// Before the field's first token. Not read for
	/// [`ObjectFieldKind::FieldStr`].
	pub fodder1: Fodder,
	/// Before the `]` of a computed field name, and before the identifier of an
	/// `ObjectLocal`.
	pub fodder2: Fodder,
	/// Before the `:`, `::` or `:::` — or before the `=` of an `ObjectLocal`,
	/// or before the `:` of an assert's message.
	pub op_fodder: Fodder,
	pub comma_fodder: Fodder,
	/// The field name, where it is an expression. Not in scope of the object.
	pub expr1: Option<Box<Node>>,
	/// The field's value, or the assert's condition. In scope of the object.
	pub expr2: Option<Box<Node>>,
	/// An assert's message.
	pub expr3: Option<Box<Node>>,
}

/// `ast.Object`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
	pub fields: Vec<ObjectField>,
	/// Before the `}`.
	pub close_fodder: Fodder,
	/// Only allowed where there is at least one field.
	pub trailing_comma: bool,
}

/// `ast.ObjectComp`: `{ [e]: e for x in e }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectComp {
	pub fields: Vec<ObjectField>,
	/// Always empty, and never read.
	///
	/// [`ArrayComp`] has the same slot and the unparser does fill that one, but
	/// `parseObjectRemainderComp` never assigns this and `unparseFields` writes
	/// the last field's [`ObjectField::comma_fodder`] instead. Carried because
	/// Go carries it, and because the node oracle records it.
	pub trailing_comma_fodder: Fodder,
	pub spec: ForSpec,
	/// Before the `}`.
	pub close_fodder: Fodder,
	pub trailing_comma: bool,
}

/// `ast.Parens`: `( e )`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parens {
	pub inner: Box<Node>,
	/// Before the `)`.
	pub close_fodder: Fodder,
}

/// `ast.IfSpec`: the `if` of a comprehension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfSpec {
	pub if_fodder: Fodder,
	pub expr: Box<Node>,
}

/// `ast.ForSpec`: the `for` of a comprehension.
///
/// Nested the way they are semantically nested rather than the way they are
/// written: `expr for x in a for y in b` is
/// `ForSpec(y, outer = ForSpec(x, outer = None))`, so the leftmost `for` is the
/// outermost. The unparser recurses into [`ForSpec::outer`] before writing
/// anything of its own, which is what puts them back in source order.
///
/// An `if` attaches to the `for` on its left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForSpec {
	pub for_fodder: Fodder,
	pub var_fodder: Fodder,
	pub var_name: Identifier,
	pub in_fodder: Fodder,
	pub expr: Box<Node>,
	pub conditions: Vec<IfSpec>,
	pub outer: Option<Box<ForSpec>>,
}

/// `ast.Unary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unary {
	pub op: UnaryOp,
	pub expr: Box<Node>,
}

/// `ast.UnaryOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
	Not,
	BitwiseNot,
	Plus,
	Minus,
}

impl UnaryOp {
	/// Go's `uopStrings`, which is also what the unparser writes.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Not => "!",
			Self::BitwiseNot => "~",
			Self::Plus => "+",
			Self::Minus => "-",
		}
	}

	/// Go's `UopMap`.
	pub fn from_symbol(text: &str) -> Option<Self> {
		match text {
			"!" => Some(Self::Not),
			"~" => Some(Self::BitwiseNot),
			"+" => Some(Self::Plus),
			"-" => Some(Self::Minus),
			_ => None,
		}
	}
}

/// `ast.BinaryOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
	Mult,
	Div,
	Percent,
	Plus,
	Minus,
	ShiftL,
	ShiftR,
	Greater,
	GreaterEq,
	Less,
	LessEq,
	In,
	ManifestEqual,
	ManifestUnequal,
	BitwiseAnd,
	BitwiseXor,
	BitwiseOr,
	And,
	Or,
}

impl BinaryOp {
	/// Go's `bopStrings`, which is also what the unparser writes.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Mult => "*",
			Self::Div => "/",
			Self::Percent => "%",
			Self::Plus => "+",
			Self::Minus => "-",
			Self::ShiftL => "<<",
			Self::ShiftR => ">>",
			Self::Greater => ">",
			Self::GreaterEq => ">=",
			Self::Less => "<",
			Self::LessEq => "<=",
			Self::In => "in",
			Self::ManifestEqual => "==",
			Self::ManifestUnequal => "!=",
			Self::BitwiseAnd => "&",
			Self::BitwiseXor => "^",
			Self::BitwiseOr => "|",
			Self::And => "&&",
			Self::Or => "||",
		}
	}

	/// Go's `BopMap`, which the parser consults to decide whether an operator
	/// token is a binary operator at all.
	pub fn from_symbol(text: &str) -> Option<Self> {
		match text {
			"*" => Some(Self::Mult),
			"/" => Some(Self::Div),
			"%" => Some(Self::Percent),
			"+" => Some(Self::Plus),
			"-" => Some(Self::Minus),
			"<<" => Some(Self::ShiftL),
			">>" => Some(Self::ShiftR),
			">" => Some(Self::Greater),
			">=" => Some(Self::GreaterEq),
			"<" => Some(Self::Less),
			"<=" => Some(Self::LessEq),
			"in" => Some(Self::In),
			"==" => Some(Self::ManifestEqual),
			"!=" => Some(Self::ManifestUnequal),
			"&" => Some(Self::BitwiseAnd),
			"^" => Some(Self::BitwiseXor),
			"|" => Some(Self::BitwiseOr),
			"&&" => Some(Self::And),
			"||" => Some(Self::Or),
			_ => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{fodder::FodderElement, location::Location};

	fn loc() -> LocationRange {
		LocationRange::point("f.jsonnet", Location { line: 1, column: 1 })
	}

	fn leaf(kind: NodeKind) -> Box<Node> {
		Box::new(Node::new(loc(), Fodder::new(), kind))
	}

	fn number(text: &str) -> Box<Node> {
		leaf(NodeKind::LiteralNumber(LiteralNumber {
			original_string: text.to_owned(),
		}))
	}

	#[test]
	fn operator_tables_round_trip() {
		// Every operator has to survive text -> enum -> text, because the
		// parser reads the first and the unparser writes the last.
		for text in [
			"*", "/", "%", "+", "-", "<<", ">>", ">", ">=", "<", "<=", "in", "==", "!=", "&", "^",
			"|", "&&", "||",
		] {
			let op = BinaryOp::from_symbol(text)
				.unwrap_or_else(|| panic!("{text} should be a binary operator"));
			assert_eq!(op.as_str(), text);
		}
		for text in ["!", "~", "+", "-"] {
			let op = UnaryOp::from_symbol(text)
				.unwrap_or_else(|| panic!("{text} should be a unary operator"));
			assert_eq!(op.as_str(), text);
		}
	}

	#[test]
	fn a_symbol_that_is_not_an_operator_is_rejected() {
		// The parser relies on this to stop reading a binary expression, so a
		// spurious `Some` here would swallow a `:` or a `;`.
		assert_eq!(BinaryOp::from_symbol(":"), None);
		assert_eq!(BinaryOp::from_symbol("!"), None);
		assert_eq!(UnaryOp::from_symbol("*"), None);
	}

	#[test]
	fn only_fully_escaped_kinds_keep_their_escapes() {
		assert!(LiteralStringKind::Single.fully_escaped());
		assert!(LiteralStringKind::Double.fully_escaped());
		assert!(!LiteralStringKind::Block.fully_escaped());
		assert!(!LiteralStringKind::VerbatimDouble.fully_escaped());
		assert!(!LiteralStringKind::VerbatimSingle.fully_escaped());
	}

	#[test]
	fn left_recursion_walks_to_the_leftmost_leaf() {
		// `1 * 2 + 3` parses as `(1 * 2) + 3`, so the leftmost leaf is the 1 —
		// and that is where the file's opening fodder has to live.
		let inner = Node::new(
			loc(),
			Fodder::new(),
			NodeKind::Binary(Binary {
				left: number("1"),
				op_fodder: Fodder::new(),
				op: BinaryOp::Mult,
				right: number("2"),
			}),
		);
		let outer = Node::new(
			loc(),
			Fodder::new(),
			NodeKind::Binary(Binary {
				left: Box::new(inner),
				op_fodder: Fodder::new(),
				op: BinaryOp::Plus,
				right: number("3"),
			}),
		);

		let leftmost = outer.left_recursive_deep();
		assert_eq!(
			leftmost.kind.name(),
			"LiteralNumber",
			"the closure should reach past both Binary nodes"
		);
	}

	#[test]
	fn a_node_that_is_not_left_recursive_is_its_own_leftmost() {
		let node = Node::new(loc(), Fodder::new(), NodeKind::LiteralNull);
		assert!(node.left_recursive().is_none());
		assert_eq!(node.left_recursive_deep().kind.name(), "LiteralNull");
	}

	#[test]
	fn opening_fodder_reaches_the_leaf_that_holds_it() {
		// This is what `removeInitialNewlines` mutates, and on a left-recursive
		// node the fodder it has to reach is several levels down.
		let mut outer = Node::new(
			loc(),
			Fodder::new(),
			NodeKind::Binary(Binary {
				left: number("1"),
				op_fodder: Fodder::new(),
				op: BinaryOp::Plus,
				right: number("2"),
			}),
		);

		outer
			.opening_fodder_mut()
			.append(FodderElement::line_end(0, 0));

		assert!(
			outer.fodder.is_empty(),
			"the outer Binary must not have gained fodder of its own"
		);
		let NodeKind::Binary(binary) = &outer.kind else {
			unreachable!("built as a Binary")
		};
		assert_eq!(binary.left.fodder.len(), 1, "the leaf holds it instead");
	}
}
