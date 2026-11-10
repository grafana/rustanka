//! A port of `internal/formatter/pretty_field_names.go`.
//!
//! Field names and lookups take the shortest syntax that still parses:
//! `{ ['foo']: 1 }` becomes `{ 'foo': 1 }` becomes `{ foo: 1 }`, and
//! `a['foo']` becomes `a.foo`. Both steps run in one visit, so a computed name
//! that is a plain identifier loses the brackets and the quotes together.
//!
//! # Only the second step asks whether it is an identifier
//!
//! Dropping the brackets is unconditional for any literal string, so
//! `{ ['a b']: 1 }` becomes `{ 'a b': 1 }`. Dropping the quotes asks
//! [`is_valid_identifier`], which also rejects the keywords — which is why
//! `{ 'false': 1 }` keeps its quotes while `{ 'False': 1 }` does not.
//!
//! A fully escaped string keeps its escape sequences in
//! [`LiteralString::value`](crate::ast::LiteralString::value), so `a['foo']`
//! is asked about the text `foo`, which is not an identifier, and the
//! lookup is left alone. That is upstream's behaviour and not a rounding of
//! it: the value the pass reads is the value the parser stored.
//!
//! # `Index` assigns over the fodder before the `]`, and so loses it
//!
//! `index.RightBracketFodder = lit.Fodder` is an assignment, not a move, and
//! `Index` reuses that one slot for the fodder before a `]` and the fodder
//! before an identifier. So a comment written between the string and the
//! bracket — `a['foo' /* c */]` — is **dropped** by `tk fmt`. Reproduced, like
//! the three oddities `src/lexer.rs` lists and the one `ast::NamedArgument`
//! does; the object-field path uses `FodderMoveFront` and keeps everything.

use crate::{
	ast::{Index, Node, NodeKind, ObjectField, ObjectFieldKind},
	pass::{AstPass, base},
	token::is_valid_identifier,
};

/// `formatter.PrettyFieldNames`.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrettyFieldNames;

impl AstPass for PrettyFieldNames {
	type Ctx = ();

	fn base_context(&mut self) {}

	fn index(&mut self, node: &mut Index, ctx: &()) {
		// Cloned out first because the fodder has to outlive the index
		// expression it is attached to. Go reads it from the literal after
		// unlinking it, which the garbage collector makes safe there.
		let promote = match node.index.as_deref() {
			Some(Node {
				kind: NodeKind::LiteralString(literal),
				fodder,
				..
			}) if is_valid_identifier(&literal.value) => Some((literal.value.clone(), fodder.clone())),
			_ => None,
		};
		if let Some((id, fodder)) = promote {
			node.index = None;
			node.id = Some(id);
			node.right_bracket_fodder = fodder;
		}

		// Note the traversal now skips the index expression and that fodder
		// slot, because `id` is set — upstream calls the base traversal after
		// the rewrite too.
		base::index(self, node, ctx);
	}

	fn object_field(&mut self, field: &mut ObjectField, ctx: &()) {
		{
			let ObjectField {
				kind,
				id,
				method,
				fodder1,
				fodder2,
				op_fodder,
				expr1,
				..
			} = &mut *field;

			// First `['foo']` -> `'foo'`. The brackets are gone, so the fodder
			// that was before the `[` moves on to the name, and the fodder
			// that was before the `]` moves on to whatever followed it: the
			// method's `(` where there is one, otherwise the `:`.
			if *kind == ObjectFieldKind::FieldExpr
				&& let Some(name) = expr1.as_deref_mut()
				&& matches!(name.kind, NodeKind::LiteralString(_))
			{
				*kind = ObjectFieldKind::FieldStr;
				name.fodder.move_front(fodder1);
				if let Some(method) = method {
					method.paren_left_fodder.move_front(fodder2);
				} else {
					op_fodder.move_front(fodder2);
				}
			}

			// Then `'foo'` -> `foo`, which is where the name has to be an
			// identifier. `fodder1` is empty for a `FieldStr` — the name
			// expression carries its own opening fodder — and the branch above
			// has just emptied it where it came from there, so assigning it is
			// not losing anything.
			if *kind == ObjectFieldKind::FieldStr {
				let promote = match expr1.as_deref() {
					Some(Node {
						kind: NodeKind::LiteralString(literal),
						fodder,
						..
					}) if is_valid_identifier(&literal.value) => Some((literal.value.clone(), fodder.clone())),
					_ => None,
				};
				if let Some((name, fodder)) = promote {
					*kind = ObjectFieldKind::FieldId;
					*id = Some(name);
					*fodder1 = fodder;
					*expr1 = None;
				}
			}
		}

		base::object_field(self, field, ctx);
	}
}

#[cfg(test)]
mod tests {
	use crate::{CommentStyle, Options, StringStyle, format, format_default};

	fn pretty(input: &str) -> String {
		format_default("t.jsonnet", input).expect("the snippet parses")
	}

	/// The pipeline with the representation passes off.
	///
	/// Needed for the one case where `EnforceStringStyle` changes the answer
	/// this pass gave — see
	/// [`an_escape_is_read_as_written_and_so_is_not_an_identifier`].
	fn pretty_only(input: &str) -> String {
		let options = Options {
			string_style: StringStyle::Leave,
			comment_style: CommentStyle::Leave,
			..Options::default()
		};
		format("t.jsonnet", input, &options).expect("the snippet parses")
	}

	#[test]
	fn a_quoted_lookup_becomes_a_dotted_one() {
		assert_eq!(pretty("a['foo']"), "a.foo\n");
		assert_eq!(pretty("a[\"foo\"]"), "a.foo\n");
	}

	#[test]
	fn a_lookup_that_is_not_an_identifier_keeps_its_brackets() {
		assert_eq!(pretty("a['not an id']"), "a['not an id']\n");
		assert_eq!(pretty("a['']"), "a['']\n");
		assert_eq!(pretty("a[1]"), "a[1]\n");
		// A keyword is not an identifier: `is_valid_identifier` ends by asking
		// the lexer's keyword table.
		assert_eq!(pretty("a['false']"), "a['false']\n");
		assert_eq!(pretty("a['super']"), "a['super']\n");
		// But only exactly the keywords.
		assert_eq!(pretty("a['False']"), "a.False\n");
	}

	#[test]
	fn an_escape_is_read_as_written_and_so_is_not_an_identifier() {
		// A fully escaped string keeps its escapes in `value`, so this pass is
		// asked about the text `foo`, which has a backslash in it and is
		// not an identifier. Upstream does the same.
		//
		// `string_style` is off because `EnforceStringStyle` runs *after* this
		// pass and resolves the escape — see the test below, which is what
		// `tk fmt` actually prints.
		assert_eq!(pretty_only("a['fo\\u006f']"), "a['fo\\u006f']\n");
	}

	#[test]
	fn the_escape_this_pass_refused_is_resolved_by_the_next_one() {
		// A pipeline-order consequence, and it caught a wrong expectation this
		// test file had carried since Phase 2b.
		//
		// `FormatNode` runs `PrettyFieldNames` at step 10 and
		// `EnforceStringStyle` at step 11. So this pass sees `foo`, finds
		// a backslash, and keeps the brackets; the next pass then unescapes to
		// `foo` and writes it back plainly. The brackets survive an escape
		// that is no longer there.
		assert_eq!(pretty("a['fo\\u006f']"), "a['foo']\n");

		// Which makes `tk fmt` non-idempotent on this input: formatting the
		// answer again promotes it, because by then the value really is an
		// identifier. `docs/rtk-fmt-plan.md` records this against Phase 2's
		// exit criterion, which had assumed a one-pass fixed point.
		assert_eq!(pretty("a['foo']"), "a.foo\n");
	}

	#[test]
	fn the_fodder_before_the_closing_bracket_is_lost() {
		// Upstream assigns over `right_bracket_fodder` rather than moving into
		// it, and that one slot doubles as the fodder before the identifier.
		// So `tk fmt` drops this comment. See the module documentation.
		assert_eq!(pretty("a['foo' /* c */]"), "a.foo\n");
	}

	#[test]
	fn a_computed_field_name_loses_its_brackets_whatever_it_says() {
		// No identifier check on this step, only on the next one.
		assert_eq!(pretty("{['foo']: 1}"), "{ foo: 1 }\n");
		assert_eq!(pretty("{['a b']: 1}"), "{ 'a b': 1 }\n");
		// Not a literal string, so neither step applies.
		assert_eq!(pretty("{[k]: 1}"), "{ [k]: 1 }\n");
	}

	#[test]
	fn a_quoted_field_name_loses_its_quotes() {
		assert_eq!(pretty("{'foo': 1}"), "{ foo: 1 }\n");
		assert_eq!(pretty("{\"foo\": 1}"), "{ foo: 1 }\n");
		assert_eq!(pretty("{'false': 1}"), "{ 'false': 1 }\n");
	}

	#[test]
	fn a_method_keeps_its_parameters_through_both_steps() {
		assert_eq!(pretty("{['foo'](x): x}"), "{ foo(x): x }\n");
	}

	#[test]
	fn the_pass_is_the_only_thing_that_does_this() {
		// `PrettyFieldNames` is one of the three passes upstream puts behind a
		// flag, so turning it off has to leave the names exactly as written —
		// which is also what makes it the one 2b pass the text corpus could
		// have isolated on its own.
		let options = Options {
			pretty_field_names: false,
			..Options::default()
		};
		assert_eq!(
			format("t.jsonnet", "{['foo']: a['bar']}", &options).expect("it parses"),
			"{ ['foo']: a['bar'] }\n"
		);
	}
}
