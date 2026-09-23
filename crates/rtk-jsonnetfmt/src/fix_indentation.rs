//! A port of `internal/formatter/fix_indentation.go`: step 13 of
//! `FormatNode`, and the largest single thing in the formatter.
//!
//! # It is not a pass
//!
//! Which is why it is here rather than in [`crate::passes`]. Every other step
//! of the pipeline is an `ASTPass` invoked through upstream's `visitFile`
//! helper; this one is constructed and called directly:
//!
//! ```go
//! if options.Indent > 0 {
//!     visitor := FixIndentation{Options: options}
//!     visitor.VisitFile(node, finalFodder)
//! }
//! ```
//!
//! So it embeds `pass.Base` and never uses it: it has its own `Visit` with its
//! own signature — `Visit(expr, currIndent, crowded)` — and walks the tree
//! itself. Two consequences worth having in mind:
//!
//! - **It reaches all four slots [`crate::pass::base`] skips.** `InSuper`'s
//!   `in_fodder` and `super_fodder`, `Index`'s `right_bracket_fodder` in the
//!   identifier case, `Apply`'s `tail_strict_fodder` and a `Parameter`'s
//!   `eq_fodder` are each filled here. That is why a `#` comment written in
//!   `x /* … */ in super` survives `EnforceCommentStyle` and is still
//!   re-indented.
//! - **[`crate::pass::AstPass`] does not constrain it.** It answers to the
//!   corpus and to the pass oracle, and to nothing in `src/pass.rs`.
//!
//! # What it actually does
//!
//! It walks the tree keeping a model of the output column, and writes an
//! `indent` into every non-interstitial fodder element it passes. Nothing
//! else — except for `|||` block strings, where it also rewrites
//! [`LiteralString::block_indent`] and [`LiteralString::block_term_indent`].
//! It is the only step in the pipeline that edits the AST outside fodder.
//!
//! Indentation is also the one thing the parse/unparse round trip does *not*
//! regenerate for free. Every space **within** a line comes back out of the
//! unparser from `crowded`, `separate_token` and `PadArrays`/`PadObjects`, so
//! `{a:1}` becomes `{ a: 1 }` with no pass involved; but the spaces at the
//! start of a line come from `fodder.indent`, which the lexer filled counting
//! a tab as 8. So a tab-indented file needs this pass and a crowded one does
//! not. Check which before reading an indentation diff.
//!
//! # `base` and `line_up`
//!
//! [`Indent`] carries two numbers and the difference between them is most of
//! the pass. `line_up` is what an ordinary new line is indented to; `base` is
//! what the *next* level down is derived from. Upstream's own example, with
//! spaces as underscores:
//!
//! ```text
//! ____foobar(1,
//! ___________2)
//! ```
//!
//! At the `2` the indent is `base = 4`, `line_up = 11`. So a further newline
//! inside that argument would be indented from 4, not from 11 — unless the
//! node asked for the "strong" variant, which promotes `line_up` to `base`
//! as well.
//!
//! The four combinators differ in exactly two axes, and reading them as a
//! table is quicker than reading the four functions:
//!
//! | | first sub-expression on this line | on a new line |
//! | --- | --- | --- |
//! | `new_indent` | `{old.base, line_up}` | reset to `old.base + Indent` |
//! | `new_indent_strong` | `{line_up, line_up}` | reset to `old.base + Indent` |
//! | `align` | `{old.base, line_up}` | `old`, unchanged |
//! | `align_strong` | `{line_up, line_up}` | `old`, unchanged |
//!
//! "On this line" means the fodder is empty or starts with an interstitial —
//! a comment within a line does not break it. The two `align` variants are
//! associated functions rather than methods here because their reset branch
//! returns `old` untouched, so neither ever consults `Options.Indent`; that
//! is the whole difference between aligning and indenting.
//!
//! # Where the near-misses are
//!
//! the fmt port plan called this the pass where near-misses would cluster, and
//! the shapes are specific. Five worth knowing before changing anything:
//!
//! 1. **`fill_last`'s last element takes a different indent from the rest**,
//!    and *which* differs per node. `Apply`, `Array`, `Object`, `Parens`,
//!    `Index` and a `local` bind's close fodder all end on
//!    `curr_indent.base`; [`FixIndentation::params`] ends on
//!    `curr_indent.line_up`. That pair is one character apart in the source
//!    and is pinned by two snippets written to tell them apart.
//! 2. **`Conditional` fills `then` and `else` at `curr_indent.base`**, not
//!    `line_up`, while each branch gets its own `new_indent` at `column + 1`.
//! 3. **An object assert visits its condition with `curr_indent`** although
//!    it computed a `new_indent2` from that condition's own fodder, and only
//!    the *message* gets `new_indent2`. The top-level `Assert` node visits
//!    its condition with the new indent, so the two disagree. Reproduced.
//! 4. **`Local` chains rather than resetting**: a bind's body indent is
//!    `new_indent(…, new_indent, column + 1)` — the bind list's indent as its
//!    `old`, not the node's `curr_indent`. It is the only place in the pass
//!    that does this.
//! 5. **`Slice` never fills `right_bracket_fodder`.** Every other bracketed
//!    node calls `fill_last` there; this one fills the left bracket and each
//!    colon and then just adds a column for the `]`. So a newline before a
//!    slice's `]` keeps whatever indent it was lexed with.
//!
//! # And one upstream bug, in `specs`
//!
//! The conditions loop computes its indent from `openFodder(spec.Expr)` and
//! then calls `Visit(spec.Expr)` — the `for` expression, **not**
//! `cond.Expr`. So in a comprehension with an `if`:
//!
//! - the `for` expression is indented **twice**, at two different columns,
//!   and the second answer wins
//! - the `if` condition is **never** indented at all; only its `if_fodder` is
//!   filled
//!
//! Both halves are pinned by snippets. It is what `tk fmt` prints, so it is
//! reproduced rather than fixed.
//!
//! # Bytes here, runes there
//!
//! The column model counts a string literal's width in **bytes** —
//! `2 + len(node.Value)`, and Go's `len` on a string is its byte length — but
//! loops over the **runes** of a verbatim string, adding 1 each and 2 for a
//! quote that will be written doubled. Both are in the same `match`, three
//! arms apart. An interstitial's comment is counted in bytes too.

use crate::{
	Options,
	ast::{
		ForSpec, LiteralStringKind, Node, NodeKind, ObjectField, ObjectFieldHide, ObjectFieldKind,
		Parameter,
	},
	fodder::{Fodder, FodderKind},
};

/// The indentation level, as `formatter.indent`.
///
/// See the module documentation for what the two numbers are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indent {
	/// What the next level down is derived from.
	pub base: usize,
	/// What an ordinary new line is indented to. Generally `>= base`.
	pub line_up: usize,
}

impl Indent {
	/// The indent a file starts at: Go's `indent{0, 0}`.
	const ROOT: Self = Self {
		base: 0,
		line_up: 0,
	};

	const fn both(value: usize) -> Self {
		Self {
			base: value,
			line_up: value,
		}
	}
}

/// `formatter.FixIndentation`.
///
/// Upstream carries the whole `Options` and embeds a `pass.Base` it never
/// uses. This carries the three fields the pass actually reads, which is a
/// real statement about the pass: indentation does not depend on string
/// style, comment style, `SortImports` or `UseImplicitPlus`, so a change to
/// any of those cannot move a line.
#[derive(Debug, Clone)]
pub struct FixIndentation {
	/// The model of the output column. Upstream's `c.column`.
	column: usize,
	/// `Options.Indent`: spaces per level.
	indent_width: usize,
	/// `Options.PadArrays`.
	pad_arrays: bool,
	/// `Options.PadObjects`.
	pad_objects: bool,
}

impl FixIndentation {
	/// Construct from the options, as `FormatNode` does.
	///
	/// Whether the pass runs at all is [`crate::format`]'s business, exactly
	/// as it is `FormatNode`'s — the gate there is `Indent > 0`.
	pub fn new(options: &Options) -> Self {
		Self {
			column: 0,
			indent_width: options.indent,
			pad_arrays: options.pad_arrays,
			pad_objects: options.pad_objects,
		}
	}

	// -----------------------------------------------------------------------
	// The indent combinators.
	// -----------------------------------------------------------------------

	/// Go's `len(firstFodder) == 0 || firstFodder[0].Kind == FodderInterstitial`:
	/// does the sub-expression start on the *current* line?
	///
	/// An interstitial is a comment within a line, so it does not break one —
	/// which is why leading fodder made only of interstitials still counts as
	/// "this line".
	fn starts_on_this_line(first_fodder: &Fodder) -> bool {
		first_fodder
			.first()
			.is_none_or(|element| element.kind == FodderKind::Interstitial)
	}

	/// `newIndent`: line up if the sub-expression is on this line, else reset
	/// one level in from `base`.
	fn new_indent(&self, first_fodder: &Fodder, old: Indent, line_up: usize) -> Indent {
		if Self::starts_on_this_line(first_fodder) {
			Indent {
				base: old.base,
				line_up,
			}
		} else {
			Indent::both(old.base + self.indent_width)
		}
	}

	/// `newIndentStrong`: as [`Self::new_indent`], but the line-up column
	/// becomes the new `base` too, so deeper expressions indent from it.
	fn new_indent_strong(&self, first_fodder: &Fodder, old: Indent, line_up: usize) -> Indent {
		if Self::starts_on_this_line(first_fodder) {
			Indent::both(line_up)
		} else {
			Indent::both(old.base + self.indent_width)
		}
	}

	/// `align`: line up if the sub-expression is on this line, else leave the
	/// indent exactly as it was — no extra level.
	///
	/// An associated function because that reset branch returns `old`
	/// untouched, so this never consults `Options.Indent`. The same is true of
	/// [`Self::align_strong`], and it is the whole difference between these
	/// two and the `new_indent` pair.
	fn align(first_fodder: &Fodder, old: Indent, line_up: usize) -> Indent {
		if Self::starts_on_this_line(first_fodder) {
			Indent {
				base: old.base,
				line_up,
			}
		} else {
			old
		}
	}

	/// `alignStrong`: as [`Self::align`], with the line-up column promoted to
	/// `base`.
	fn align_strong(first_fodder: &Fodder, old: Indent, line_up: usize) -> Indent {
		if Self::starts_on_this_line(first_fodder) {
			Indent::both(line_up)
		} else {
			old
		}
	}

	// -----------------------------------------------------------------------
	// Writing indents, and the column model.
	// -----------------------------------------------------------------------

	/// `setIndents`: the last non-interstitial element gets `last_indent`,
	/// every earlier one gets `all_but_last_indent`.
	///
	/// Go counts the non-interstitials first and then panics with "Shouldn't
	/// get here" if the running index is not `count - 1` when it reaches the
	/// last one. That branch is unreachable — the index runs `0..count`, so
	/// `i + 1 >= count` holds exactly when `i == count - 1` — so it is not
	/// reproduced.
	fn set_indents(fodder: &mut Fodder, all_but_last_indent: usize, last_indent: usize) {
		let count = fodder
			.iter()
			.filter(|element| element.kind != FodderKind::Interstitial)
			.count();

		let mut seen = 0;
		for element in fodder.iter_mut() {
			if element.kind != FodderKind::Interstitial {
				element.indent = if seen + 1 < count {
					all_but_last_indent
				} else {
					last_indent
				};
				seen += 1;
			}
		}
	}

	/// `fillLast`: write the indents, then advance the column as if the
	/// fodder had been printed.
	///
	/// The loop is a model of [`crate::unparse::Unparser`]'s `fodder_fill`
	/// that keeps only the column. `crowded` and `separate_token` mean what
	/// they mean there, and `crowded` is a local copy that the loop clears —
	/// a line end resets the column to the indent it was just given, which
	/// discards anything an interstitial before it had added.
	fn fill_last(
		&mut self,
		fodder: &mut Fodder,
		mut crowded: bool,
		separate_token: bool,
		all_but_last_indent: usize,
		last_indent: usize,
	) {
		Self::set_indents(fodder, all_but_last_indent, last_indent);

		for element in fodder.iter() {
			match element.kind {
				// Go has these as two arms with identical bodies.
				FodderKind::Paragraph | FodderKind::LineEnd => {
					self.column = element.indent;
					crowded = false;
				}
				FodderKind::Interstitial => {
					if crowded {
						self.column += 1;
					}
					// Bytes, as Go's `len` on a string is.
					self.column += element
						.comment
						.first()
						.expect("an interstitial has exactly one comment")
						.len();
					crowded = true;
				}
			}
		}

		if separate_token && crowded {
			self.column += 1;
		}
	}

	/// `fill`: [`Self::fill_last`] where the last element takes the same
	/// indent as the rest.
	fn fill(&mut self, fodder: &mut Fodder, crowded: bool, separate_token: bool, indent: usize) {
		self.fill_last(fodder, crowded, separate_token, indent, indent);
	}

	// -----------------------------------------------------------------------
	// Helpers that mirror upstream's.
	// -----------------------------------------------------------------------

	/// `specs`: the `for` and `if` clauses of a comprehension.
	///
	/// [`ForSpec::outer`] holds the *earlier* clause, so recursing first puts
	/// them back in source order — the same shape the unparser walks.
	///
	/// **The conditions loop carries an upstream bug**: it computes its
	/// indent from `spec.expr` and then visits `spec.expr` again, where
	/// `cond.expr` was plainly meant. See the module documentation.
	fn specs(&mut self, spec: &mut ForSpec, curr_indent: Indent) {
		if let Some(outer) = spec.outer.as_deref_mut() {
			self.specs(outer, curr_indent);
		}

		self.fill(&mut spec.for_fodder, true, true, curr_indent.line_up);
		self.column += 3; // for
		self.fill(&mut spec.var_fodder, true, true, curr_indent.line_up);
		self.column += spec.var_name.len();
		self.fill(&mut spec.in_fodder, true, true, curr_indent.line_up);
		self.column += 2; // in

		let new_indent = self.new_indent(spec.expr.opening_fodder(), curr_indent, self.column);
		self.visit(&mut spec.expr, new_indent, true);

		for index in 0..spec.conditions.len() {
			self.fill(
				&mut spec.conditions[index].if_fodder,
				true,
				true,
				curr_indent.line_up,
			);
			self.column += 2; // if
			// THE BUG, reproduced: `spec.expr`, not `spec.conditions[index].expr`.
			let new_indent = self.new_indent(spec.expr.opening_fodder(), curr_indent, self.column);
			self.visit(&mut spec.expr, new_indent, true);
		}
	}

	/// `params`: a parameter list, for `function`, a method or a `local`
	/// function bind.
	///
	/// Note the closing paren: `fill_last` ends this one on
	/// `curr_indent.line_up`, where `Apply`'s ends on `curr_indent.base`.
	fn params(
		&mut self,
		fodder_l: &mut Fodder,
		params: &mut [Parameter],
		trailing_comma: bool,
		fodder_r: &mut Fodder,
		curr_indent: Indent,
	) {
		self.fill(fodder_l, false, false, curr_indent.line_up);
		self.column += 1; // (

		let new_indent = {
			// Go's `for … { firstInside = param.NameFodder; break }` idiom,
			// falling back to the right fodder when there are no parameters.
			let first_inside = params
				.first()
				.map_or(&*fodder_r, |param| &param.name_fodder);
			self.new_indent(first_inside, curr_indent, self.column)
		};

		let mut first = true;
		for param in &mut *params {
			if !first {
				self.column += 1; // ,
			}
			self.fill(&mut param.name_fodder, !first, true, new_indent.line_up);
			self.column += param.name.len();
			if param.default_arg.is_some() {
				self.fill(&mut param.eq_fodder, false, false, new_indent.line_up);
				// A default argument is written with no spacing: `x=e`.
				self.column += 1;
				if let Some(default_arg) = param.default_arg.as_deref_mut() {
					self.visit(default_arg, new_indent, false);
				}
			}
			self.fill(&mut param.comma_fodder, false, false, new_indent.line_up);
			first = false;
		}

		if trailing_comma {
			self.column += 1;
		}
		self.fill_last(
			fodder_r,
			false,
			false,
			new_indent.line_up,
			curr_indent.line_up,
		);
		self.column += 1; // )
	}

	/// `fieldParams`: the parameter list of the method sugar `f(x): e`.
	fn field_params(&mut self, field: &mut ObjectField, curr_indent: Indent) {
		if let Some(method) = field.method.as_mut() {
			self.params(
				&mut method.paren_left_fodder,
				&mut method.parameters,
				method.trailing_comma,
				&mut method.paren_right_fodder,
				curr_indent,
			);
		}
	}

	/// Upstream's `unparseFieldRemainder` closure: the tail shared by the
	/// three basic field kinds.
	fn field_remainder(&mut self, field: &mut ObjectField, curr_indent: Indent, new_indent: usize) {
		self.field_params(field, curr_indent);
		self.fill(&mut field.op_fodder, false, false, new_indent);
		if field.super_sugar {
			self.column += 1; // +
		}
		self.column += match field.hide {
			ObjectFieldHide::Inherit => 1, // :
			ObjectFieldHide::Hidden => 2,  // ::
			ObjectFieldHide::Visible => 3, // :::
		};
		if let Some(expr2) = field.expr2.as_deref_mut() {
			let value_indent = self.new_indent(expr2.opening_fodder(), curr_indent, self.column);
			self.visit(expr2, value_indent, true);
		}
	}

	/// `fields`: the fields of an object or an object comprehension.
	///
	/// `curr_indent` is the indent of the first field and `crowded` says
	/// whether it is crowded, both decided by the caller. The local
	/// `new_indent` is an **`usize`**, not an [`Indent`] — it is
	/// `curr_indent.line_up`, and the `ObjectLocal` branch deliberately uses
	/// `curr_indent.line_up` directly where the others use it through this
	/// name. They are the same number; the two spellings are upstream's.
	fn fields(&mut self, fields: &mut [ObjectField], curr_indent: Indent, crowded: bool) {
		let new_indent = curr_indent.line_up;

		for (index, field) in fields.iter_mut().enumerate() {
			if index > 0 {
				self.column += 1; // ,
			}
			let first_crowded = index > 0 || crowded;

			match field.kind {
				ObjectFieldKind::Local => {
					self.fill(&mut field.fodder1, first_crowded, true, curr_indent.line_up);
					self.column += 5; // local
					self.fill(&mut field.fodder2, true, true, curr_indent.line_up);
					self.column += field.id.as_deref().map_or(0, str::len);
					self.field_params(field, curr_indent);
					self.fill(&mut field.op_fodder, true, true, curr_indent.line_up);
					self.column += 1; // =
					if let Some(expr2) = field.expr2.as_deref_mut() {
						let body_indent =
							self.new_indent(expr2.opening_fodder(), curr_indent, self.column);
						self.visit(expr2, body_indent, true);
					}
				}

				ObjectFieldKind::FieldId => {
					self.fill(&mut field.fodder1, first_crowded, true, new_indent);
					self.column += field.id.as_deref().map_or(0, str::len);
					self.field_remainder(field, curr_indent, new_indent);
				}

				ObjectFieldKind::FieldStr => {
					if let Some(expr1) = field.expr1.as_deref_mut() {
						self.visit(expr1, curr_indent, first_crowded);
					}
					self.field_remainder(field, curr_indent, new_indent);
				}

				ObjectFieldKind::FieldExpr => {
					self.fill(&mut field.fodder1, first_crowded, true, new_indent);
					self.column += 1; // [
					if let Some(expr1) = field.expr1.as_deref_mut() {
						self.visit(expr1, curr_indent, false);
					}
					self.fill(&mut field.fodder2, false, false, new_indent);
					self.column += 1; // ]
					self.field_remainder(field, curr_indent, new_indent);
				}

				ObjectFieldKind::Assert => {
					self.fill(&mut field.fodder1, first_crowded, true, new_indent);
					self.column += 6; // assert
					// Computed from the condition's fodder at column + 1, for
					// the space after `assert` — and then the condition is
					// visited with `curr_indent` all the same. Only the
					// message below gets this. Upstream's; see the module
					// documentation.
					let message_indent = field.expr2.as_deref().map_or(curr_indent, |expr2| {
						self.new_indent(expr2.opening_fodder(), curr_indent, self.column + 1)
					});
					if let Some(expr2) = field.expr2.as_deref_mut() {
						self.visit(expr2, curr_indent, true);
					}
					if field.expr3.is_some() {
						self.fill(&mut field.op_fodder, true, true, message_indent.line_up);
						self.column += 1; // :
						if let Some(expr3) = field.expr3.as_deref_mut() {
							self.visit(expr3, message_indent, true);
						}
					}
				}
			}

			self.fill(&mut field.comma_fodder, false, false, new_indent);
		}
	}

	// -----------------------------------------------------------------------
	// The walk.
	// -----------------------------------------------------------------------

	/// `Visit`: the logic common to every node, then the node's own.
	///
	/// This is *not* [`crate::pass::AstPass::visit`] and has a different
	/// signature: the indent and the crowded flag are threaded through the
	/// walk rather than carried in a context.
	// One arm per case of upstream's 28-case type switch, deliberately, because
	// that correspondence is what makes this function auditable against
	// `fix_indentation.go` at all. Both lints below are the cost of the
	// transcription rather than something to tidy:
	//
	// * `too_many_lines` — it is one big switch, as Go's is.
	// * `match_same_arms` — `LiteralNull` and `SelfExpr` both advance the
	//   column by 4, because `null` and `self` are both four characters.
	//   Coalescing them into an or-pattern would be behaviour-identical and
	//   would lose the one-to-one mapping for a coincidence of arithmetic.
	#[allow(clippy::too_many_lines, clippy::match_same_arms)]
	fn visit(&mut self, expr: &mut Node, curr_indent: Indent, crowded: bool) {
		let separate_token = expr.left_recursive().is_none();
		// `expr.OpenFodder()` — the node's *own* fodder, not `openFodder(expr)`.
		self.fill(
			&mut expr.fodder,
			crowded,
			separate_token,
			curr_indent.line_up,
		);

		match &mut expr.kind {
			NodeKind::Apply(node) => {
				let mut new_column = self.column;
				if crowded {
					new_column += 1;
				}
				let new_indent = Self::align(node.target.opening_fodder(), curr_indent, new_column);
				self.visit(&mut node.target, new_indent, crowded);
				self.fill(&mut node.fodder_left, false, false, new_indent.line_up);
				self.column += 1; // (

				let arg_indent = {
					// Go assigns FodderRight, then the first named argument,
					// then the first positional one — so a positional wins.
					let first_fodder = node
						.arguments
						.positional
						.first()
						.map(|arg| arg.expr.opening_fodder())
						.or_else(|| node.arguments.named.first().map(|arg| &arg.name_fodder))
						.unwrap_or(&node.fodder_right);

					// Strong indent if any argument *except the first* is
					// preceded by a newline. `first` spans both loops, so
					// which argument is protected depends on whether there
					// are any positional ones.
					let mut strong_indent = false;
					let mut first = true;
					for arg in &node.arguments.positional {
						if first {
							first = false;
							continue;
						}
						if arg.expr.opening_fodder().contains_newline() {
							strong_indent = true;
						}
					}
					for arg in &node.arguments.named {
						if first {
							first = false;
							continue;
						}
						if arg.name_fodder.contains_newline() {
							strong_indent = true;
						}
					}

					if strong_indent {
						self.new_indent_strong(first_fodder, curr_indent, self.column)
					} else {
						self.new_indent(first_fodder, curr_indent, self.column)
					}
				};

				let mut first = true;
				for arg in &mut node.arguments.positional {
					if !first {
						self.column += 1; // ,
					}
					let space = !first;
					self.visit(&mut arg.expr, arg_indent, space);
					self.fill(&mut arg.comma_fodder, false, false, arg_indent.line_up);
					first = false;
				}
				for arg in &mut node.arguments.named {
					if !first {
						self.column += 1; // ,
					}
					let space = !first;
					self.fill(&mut arg.name_fodder, space, false, arg_indent.line_up);
					self.column += arg.name.len();
					self.column += 1; // =
					self.visit(&mut arg.arg, arg_indent, false);
					self.fill(&mut arg.comma_fodder, false, false, arg_indent.line_up);
					first = false;
				}

				if node.trailing_comma {
					self.column += 1;
				}
				// `curr_indent.base` here, where `params` uses `line_up`.
				self.fill_last(
					&mut node.fodder_right,
					false,
					false,
					arg_indent.line_up,
					curr_indent.base,
				);
				self.column += 1; // )
				if node.tail_strict {
					self.fill(&mut node.tail_strict_fodder, true, true, curr_indent.base);
					self.column += 10; // tailstrict
				}
			}

			NodeKind::ApplyBrace(node) => {
				let mut new_column = self.column;
				if crowded {
					new_column += 1;
				}
				let new_indent = Self::align(node.left.opening_fodder(), curr_indent, new_column);
				self.visit(&mut node.left, new_indent, crowded);
				self.visit(&mut node.right, new_indent, true);
			}

			NodeKind::Array(node) => {
				self.column += 1; // [

				let mut new_column = self.column;
				if self.pad_arrays {
					new_column += 1;
				}

				let new_indent = {
					let first_fodder = node
						.elements
						.first()
						.map_or(&node.close_fodder, |element| element.expr.opening_fodder());

					let strong_indent = node
						.elements
						.iter()
						.skip(1)
						.any(|element| element.expr.opening_fodder().contains_newline());

					if strong_indent {
						self.new_indent_strong(first_fodder, curr_indent, new_column)
					} else {
						self.new_indent(first_fodder, curr_indent, new_column)
					}
				};

				let had_elements = !node.elements.is_empty();
				for (index, element) in node.elements.iter_mut().enumerate() {
					if index > 0 {
						self.column += 1; // ,
					}
					self.visit(&mut element.expr, new_indent, index > 0 || self.pad_arrays);
					self.fill(&mut element.comma_fodder, false, false, new_indent.line_up);
				}
				if node.trailing_comma {
					self.column += 1;
				}

				self.fill_last(
					&mut node.close_fodder,
					had_elements,
					self.pad_arrays,
					new_indent.line_up,
					curr_indent.base,
				);
				self.column += 1; // ]
			}

			NodeKind::ArrayComp(node) => {
				self.column += 1; // [
				let mut new_column = self.column;
				if self.pad_arrays {
					new_column += 1;
				}
				let new_indent =
					self.new_indent(node.body.opening_fodder(), curr_indent, new_column);
				self.visit(&mut node.body, new_indent, self.pad_arrays);
				self.fill(
					&mut node.trailing_comma_fodder,
					false,
					false,
					new_indent.line_up,
				);
				if node.trailing_comma {
					self.column += 1; // ,
				}
				self.specs(&mut node.spec, new_indent);
				self.fill_last(
					&mut node.close_fodder,
					true,
					self.pad_arrays,
					new_indent.line_up,
					curr_indent.base,
				);
				self.column += 1; // ]
			}

			NodeKind::Assert(node) => {
				self.column += 6; // assert
				// + 1 for the space after `assert`.
				let new_indent =
					self.new_indent(node.cond.opening_fodder(), curr_indent, self.column + 1);
				self.visit(&mut node.cond, new_indent, true);
				if node.message.is_some() {
					self.fill(&mut node.colon_fodder, true, true, new_indent.line_up);
					self.column += 1; // :
					if let Some(message) = node.message.as_deref_mut() {
						self.visit(message, new_indent, true);
					}
				}
				self.fill(&mut node.semicolon_fodder, false, false, new_indent.line_up);
				self.column += 1; // ;
				self.visit(&mut node.rest, curr_indent, true);
			}

			NodeKind::Binary(node) => {
				let mut inner_column = self.column;
				if crowded {
					inner_column += 1;
				}
				// Strong indent for `A` / `+ B` and for `A +` / `B`, so the
				// operand does not line up under something that has moved.
				let new_indent = {
					let first_fodder = node.left.opening_fodder();
					if node.op_fodder.contains_newline()
						|| node.right.opening_fodder().contains_newline()
					{
						Self::align_strong(first_fodder, curr_indent, inner_column)
					} else {
						Self::align(first_fodder, curr_indent, inner_column)
					}
				};
				self.visit(&mut node.left, new_indent, crowded);
				self.fill(&mut node.op_fodder, true, true, new_indent.line_up);
				self.column += node.op.as_str().len();
				// Deliberately no new indent for the right-hand side, so that
				// `true &&` / `true &&` / `true` does not stair-step.
				self.visit(&mut node.right, new_indent, true);
			}

			NodeKind::Conditional(node) => {
				self.column += 2; // if
				let cond_indent =
					self.new_indent(node.cond.opening_fodder(), curr_indent, self.column + 1);
				self.visit(&mut node.cond, cond_indent, true);
				// `base`, not `line_up`.
				self.fill(&mut node.then_fodder, true, true, curr_indent.base);
				self.column += 4; // then
				let true_indent = self.new_indent(
					node.branch_true.opening_fodder(),
					curr_indent,
					self.column + 1,
				);
				self.visit(&mut node.branch_true, true_indent, true);
				if node.branch_false.is_some() {
					self.fill(&mut node.else_fodder, true, true, curr_indent.base);
					self.column += 4; // else
					let false_indent =
						node.branch_false
							.as_deref()
							.map_or(curr_indent, |branch_false| {
								self.new_indent(
									branch_false.opening_fodder(),
									curr_indent,
									self.column + 1,
								)
							});
					if let Some(branch_false) = node.branch_false.as_deref_mut() {
						self.visit(branch_false, false_indent, true);
					}
				}
			}

			NodeKind::Dollar => self.column += 1, // $

			NodeKind::Error(node) => {
				self.column += 5; // error
				let new_indent =
					self.new_indent(node.expr.opening_fodder(), curr_indent, self.column + 1);
				self.visit(&mut node.expr, new_indent, true);
			}

			NodeKind::Function(node) => {
				self.column += 8; // function
				self.params(
					&mut node.paren_left_fodder,
					&mut node.parameters,
					node.trailing_comma,
					&mut node.paren_right_fodder,
					curr_indent,
				);
				let new_indent =
					self.new_indent(node.body.opening_fodder(), curr_indent, self.column + 1);
				self.visit(&mut node.body, new_indent, true);
			}

			// The three import kinds differ only in the width of the keyword.
			NodeKind::Import(node) => {
				self.column += 6; // import
				let new_indent =
					self.new_indent(node.file.opening_fodder(), curr_indent, self.column + 1);
				self.visit(&mut node.file, new_indent, true);
			}
			NodeKind::ImportStr(node) => {
				self.column += 9; // importstr
				let new_indent =
					self.new_indent(node.file.opening_fodder(), curr_indent, self.column + 1);
				self.visit(&mut node.file, new_indent, true);
			}
			NodeKind::ImportBin(node) => {
				self.column += 9; // importbin
				let new_indent =
					self.new_indent(node.file.opening_fodder(), curr_indent, self.column + 1);
				self.visit(&mut node.file, new_indent, true);
			}

			// Both fodder slots here are ones `pass::base` never visits.
			NodeKind::InSuper(node) => {
				self.visit(&mut node.index, curr_indent, crowded);
				self.fill(&mut node.in_fodder, true, true, curr_indent.line_up);
				self.column += 2; // in
				self.fill(&mut node.super_fodder, true, true, curr_indent.line_up);
				self.column += 5; // super
			}

			NodeKind::Index(node) => {
				self.visit(&mut node.target, curr_indent, crowded);
				// Can also be the fodder before a `.`.
				self.fill(
					&mut node.left_bracket_fodder,
					false,
					false,
					curr_indent.line_up,
				);
				if let Some(id) = node.id.as_deref() {
					let id_len = id.len();
					self.column += 1; // .
					// Taken from the slot that is about to be filled, so the
					// order of these two lines matters.
					let new_indent =
						self.new_indent(&node.right_bracket_fodder, curr_indent, self.column);
					self.fill(
						&mut node.right_bracket_fodder,
						false,
						false,
						new_indent.line_up,
					);
					self.column += id_len;
				} else {
					self.column += 1; // [
					let new_indent = node.index.as_deref().map_or(curr_indent, |index| {
						self.new_indent(index.opening_fodder(), curr_indent, self.column)
					});
					if let Some(index) = node.index.as_deref_mut() {
						self.visit(index, new_indent, false);
					}
					self.fill_last(
						&mut node.right_bracket_fodder,
						false,
						false,
						new_indent.line_up,
						curr_indent.base,
					);
					self.column += 1; // ]
				}
			}

			NodeKind::Slice(node) => {
				self.visit(&mut node.target, curr_indent, crowded);
				self.fill(
					&mut node.left_bracket_fodder,
					false,
					false,
					curr_indent.line_up,
				);
				self.column += 1; // [

				// Go's `var newIndent indent` zero value, reassigned by
				// whichever branches run.
				let mut new_indent = Indent::both(0);

				if node.begin_index.is_some() {
					new_indent = node.begin_index.as_deref().map_or(new_indent, |begin| {
						self.new_indent(begin.opening_fodder(), curr_indent, self.column)
					});
					if let Some(begin) = node.begin_index.as_deref_mut() {
						self.visit(begin, new_indent, false);
					}
				}
				if node.end_index.is_some() {
					new_indent = self.new_indent(&node.end_colon_fodder, curr_indent, self.column);
					self.fill(&mut node.end_colon_fodder, false, false, new_indent.line_up);
					self.column += 1; // :
					if let Some(end) = node.end_index.as_deref_mut() {
						self.visit(end, new_indent, false);
					}
				}
				if node.step.is_some() {
					if node.end_index.is_none() {
						new_indent =
							self.new_indent(&node.end_colon_fodder, curr_indent, self.column);
						self.fill(&mut node.end_colon_fodder, false, false, new_indent.line_up);
						self.column += 1; // :
					}
					self.fill(
						&mut node.step_colon_fodder,
						false,
						false,
						new_indent.line_up,
					);
					self.column += 1; // :
					if let Some(step) = node.step.as_deref_mut() {
						self.visit(step, new_indent, false);
					}
				}
				if node.begin_index.is_none() && node.end_index.is_none() && node.step.is_none() {
					new_indent = self.new_indent(&node.end_colon_fodder, curr_indent, self.column);
					self.fill(&mut node.end_colon_fodder, false, false, new_indent.line_up);
					self.column += 1; // :
				}
				// `right_bracket_fodder` is deliberately never filled — see
				// the module documentation.
				self.column += 1; // ]
			}

			NodeKind::Local(node) => {
				self.column += 5; // local
				let new_indent = {
					// Go panics with "Not enough binds in local"; the parser
					// cannot produce one, and the unparser panics too.
					let first_bind = node.binds.first().expect("Not enough binds in local");
					self.new_indent(&first_bind.var_fodder, curr_indent, self.column + 1)
				};

				let mut first = true;
				for bind in &mut node.binds {
					if !first {
						self.column += 1; // ,
					}
					first = false;
					self.fill(&mut bind.var_fodder, true, true, new_indent.line_up);
					self.column += bind.variable.len();
					if let Some(fun) = bind.fun.as_mut() {
						self.params(
							&mut fun.paren_left_fodder,
							&mut fun.parameters,
							fun.trailing_comma,
							&mut fun.paren_right_fodder,
							new_indent,
						);
					}
					self.fill(&mut bind.eq_fodder, true, true, new_indent.line_up);
					self.column += 1; // =
					// `new_indent` as the old, not `curr_indent`: the one
					// place in this pass that chains.
					let body_indent =
						self.new_indent(bind.body.opening_fodder(), new_indent, self.column + 1);
					self.visit(&mut bind.body, body_indent, true);
					self.fill_last(
						&mut bind.close_fodder,
						false,
						false,
						body_indent.line_up,
						curr_indent.base,
					);
				}
				self.column += 1; // ;
				self.visit(&mut node.body, curr_indent, true);
			}

			NodeKind::LiteralBoolean(value) => {
				self.column += if *value { 4 } else { 5 };
			}

			NodeKind::LiteralNumber(node) => self.column += node.original_string.len(),

			NodeKind::LiteralString(node) => match node.kind {
				// Two arms with identical bodies upstream; bytes, plus the
				// two quotes.
				LiteralStringKind::Double | LiteralStringKind::Single => {
					self.column += 2 + node.value.len();
				}
				LiteralStringKind::Block => {
					// The one place this pass edits the AST outside fodder.
					node.block_indent = " ".repeat(curr_indent.base + self.indent_width);
					node.block_term_indent = " ".repeat(curr_indent.base);
					// An assignment, not an addition: the closing `|||` is at
					// the terminator indent whatever the column had reached.
					self.column = curr_indent.base;
					// Always `|||`, never `|||-`: only the block's end is
					// being accounted for.
					self.column += 3;
				}
				// Runes, not bytes — and a quote costs two, because the
				// unparser will write it doubled.
				LiteralStringKind::VerbatimSingle => {
					self.column += 3; // @, and both quotes
					for character in node.value.chars() {
						self.column += usize::from(character == '\'') + 1;
					}
				}
				LiteralStringKind::VerbatimDouble => {
					self.column += 3;
					for character in node.value.chars() {
						self.column += usize::from(character == '"') + 1;
					}
				}
			},

			NodeKind::LiteralNull => self.column += 4, // null

			NodeKind::Object(node) => {
				self.column += 1; // {
				let mut new_column = self.column;
				if self.pad_objects {
					new_column += 1;
				}
				let new_indent = {
					let first_fodder = node
						.fields
						.first()
						.and_then(ObjectField::open_fodder)
						.unwrap_or(&node.close_fodder);
					self.new_indent(first_fodder, curr_indent, new_column)
				};

				let had_fields = !node.fields.is_empty();
				self.fields(&mut node.fields, new_indent, self.pad_objects);
				if node.trailing_comma {
					self.column += 1;
				}
				self.fill_last(
					&mut node.close_fodder,
					had_fields,
					self.pad_objects,
					new_indent.line_up,
					curr_indent.base,
				);
				self.column += 1; // }
			}

			NodeKind::ObjectComp(node) => {
				self.column += 1; // {
				let mut new_column = self.column;
				if self.pad_objects {
					new_column += 1;
				}
				let new_indent = {
					let first_fodder = node
						.fields
						.first()
						.and_then(ObjectField::open_fodder)
						.unwrap_or(&node.close_fodder);
					self.new_indent(first_fodder, curr_indent, new_column)
				};

				self.fields(&mut node.fields, new_indent, self.pad_objects);
				if node.trailing_comma {
					self.column += 1; // ,
				}
				self.specs(&mut node.spec, new_indent);
				// `true` unconditionally, where `Object` passes whether it had
				// any fields.
				self.fill_last(
					&mut node.close_fodder,
					true,
					self.pad_objects,
					new_indent.line_up,
					curr_indent.base,
				);
				self.column += 1; // }
			}

			NodeKind::Parens(node) => {
				self.column += 1; // (
				// The only node that calls the strong variant unconditionally.
				let new_indent =
					self.new_indent_strong(node.inner.opening_fodder(), curr_indent, self.column);
				self.visit(&mut node.inner, new_indent, false);
				self.fill_last(
					&mut node.close_fodder,
					false,
					false,
					new_indent.line_up,
					curr_indent.base,
				);
				self.column += 1; // )
			}

			NodeKind::SelfExpr => self.column += 4, // self

			NodeKind::SuperIndex(node) => {
				self.column += 5; // super
				self.fill(&mut node.dot_fodder, false, false, curr_indent.line_up);
				if let Some(id) = node.id.as_deref() {
					let id_len = id.len();
					self.column += 1; // .
					let new_indent = self.new_indent(&node.id_fodder, curr_indent, self.column);
					self.fill(&mut node.id_fodder, false, false, new_indent.line_up);
					self.column += id_len;
				} else {
					self.column += 1; // [
					let new_indent = node.index.as_deref().map_or(curr_indent, |index| {
						self.new_indent(index.opening_fodder(), curr_indent, self.column)
					});
					if let Some(index) = node.index.as_deref_mut() {
						self.visit(index, new_indent, false);
					}
					self.fill_last(
						&mut node.id_fodder,
						false,
						false,
						new_indent.line_up,
						curr_indent.base,
					);
					self.column += 1; // ]
				}
			}

			NodeKind::Unary(node) => {
				self.column += node.op.as_str().len();
				// `openFodder(expr)` is the leftmost leaf's fodder, and the
				// same leaf answers whether the operand starts with `$`.
				let (new_indent, left_is_dollar) = {
					let leftmost = node.expr.left_recursive_deep();
					(
						self.new_indent(leftmost.open_fodder(), curr_indent, self.column),
						matches!(leftmost.kind, NodeKind::Dollar),
					)
				};
				self.visit(&mut node.expr, new_indent, left_is_dollar);
			}

			NodeKind::Var(id) => self.column += id.len(),
		}
	}

	/// `VisitFile`: the whole file, including the fodder after its last token.
	///
	/// The final fodder is flushed left — `setIndents(finalFodder, 0, 0)` —
	/// so a comment after the last expression loses whatever indentation it
	/// was written with, however deep the expression above it was.
	pub fn visit_file(&mut self, body: &mut Node, final_fodder: &mut Fodder) {
		self.visit(body, Indent::ROOT, false);
		Self::set_indents(final_fodder, 0, 0);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{CommentStyle, StringStyle, format};

	/// The whole pipeline with the representation passes off.
	///
	/// Safe as an end-to-end assertion now, in a way it was not in Phase 2c:
	/// with this step written, `FormatNode`'s pipeline is complete except for
	/// `SortImports`, `FixParens` and the two `*PlusObject` passes, none of
	/// which any input here reaches. So these answers are `tk fmt`'s. The
	/// precise per-node answers are the pass oracle's business —
	/// `testdata/pass-snippets.json` carries 83 cases for this step — and
	/// what is asserted here is the handful of shapes worth having spelled
	/// out in prose next to the code.
	fn formatted(input: &str) -> String {
		let options = Options {
			string_style: StringStyle::Leave,
			comment_style: CommentStyle::Leave,
			..Options::default()
		};
		format("t.jsonnet", input, &options).expect("the snippet parses")
	}

	#[test]
	fn a_tab_indented_file_is_respaced() {
		// The lexer counted the tab as 8. Nothing else in the pipeline can
		// fix this, which is why a tab-indented file needs this pass and a
		// crowded one does not.
		assert_eq!(formatted("{\n\ta: 1,\n}"), "{\n  a: 1,\n}\n");
	}

	#[test]
	fn an_already_formatted_file_comes_back_untouched() {
		// The property that matters most for a formatter that writes in
		// place. `tests/corpus.rs` is the real test of this over 138 files.
		for input in [
			"{\n  a: 1,\n}\n",
			"[\n  1,\n  2,\n]\n",
			"{\n  a: {\n    b: 1,\n  },\n}\n",
			"local x = 1;\n\n{\n  a: x,\n}\n",
		] {
			assert_eq!(formatted(input), input);
		}
	}

	#[test]
	fn arguments_line_up_under_the_open_paren() {
		// The `indent` struct's own doc example, at base 0 rather than 4:
		// `foobar(` is 7 characters, so the second argument lines up at 7.
		assert_eq!(formatted("foobar(1,\n2)"), "foobar(1,\n       2)\n");
	}

	#[test]
	fn a_newline_after_the_paren_resets_instead_of_lining_up() {
		// `FixNewlines` sees the newline before the first argument, sets
		// shouldExpandNearParens and moves the `)` down; then this pass finds
		// firstFodder starting with a line end and takes the reset branch, so
		// the arguments land at base + Indent rather than at column 7.
		assert_eq!(formatted("foobar(\n1, 2)"), "foobar(\n  1, 2\n)\n");
	}

	#[test]
	fn the_final_fodder_is_flushed_left() {
		// `VisitFile` ends with `setIndents(finalFodder, 0, 0)`.
		assert_eq!(formatted("1\n      // c\n"), "1\n// c\n");
	}

	#[test]
	fn a_block_string_gets_its_indents_rewritten() {
		// The one case where this pass edits the AST outside fodder:
		// BlockIndent becomes base + Indent spaces and BlockTermIndent
		// becomes base spaces, whatever the author wrote.
		assert_eq!(
			formatted("{\n  a: |||\n      x\n  |||,\n}"),
			"{\n  a: |||\n    x\n  |||,\n}\n"
		);
	}

	#[test]
	fn a_comprehension_condition_is_never_indented() {
		// The upstream bug, from the side that is visible in the output: the
		// conditions loop visits `spec.expr` instead of `cond.expr`, so the
		// six spaces before `true` are never touched. Everything around them
		// is.
		assert!(formatted("[x for x in [1] if\n      true]").contains("\n      true"));
	}
}
