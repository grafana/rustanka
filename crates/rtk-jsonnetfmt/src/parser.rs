//! A port of go-jsonnet's `internal/parser/parser.go`.
//!
//! This is the formatter's front end, so it is a *raw* parse: no desugaring, no
//! static analysis, and every comment kept as fodder on the slot it precedes.
//! Which slot that is decides where the comment comes out, so the fodder
//! assignments here are the load-bearing part of the file, not the grammar.
//!
//! Graded by `testdata/node-oracle.json` — every node and every fodder slot
//! go-jsonnet's own parser produces, over the 138 corpus files and 63 snippets.
//! `tests/node_oracle.rs` says why that exists rather than a reading of this
//! source being trusted.
//!
//! # What is deliberately not ported
//!
//! `Parse` ends with `addContext(expr, &topLevelContext, anonymous)`, which
//! fills `NodeBase.Ctx` for error messages at *evaluation* time. The formatter
//! never reads it, and [`crate::ast::Node`] does not carry it.
//!
//! # One upstream oddity reproduced
//!
//! `tokenStringToAst` validates a single- or double-quoted string by calling
//! `StringUnescape` and, on failure, wraps `err.Error()` in a *new* static
//! error at the same token. `Error()` has already rendered the location, so the
//! location appears **twice** in the message. Reproduced, because it is what
//! `tk fmt` prints.

use std::collections::HashSet;

use crate::{
	Error,
	ast::{
		APPLY_PRECEDENCE, Apply, ApplyBrace, Arguments, Array, ArrayComp, Assert, Binary, BinaryOp,
		CommaSeparatedExpr, Conditional, Error as ErrorExpr, ForSpec, Function, FunctionSugar,
		Identifier, IfSpec, Import, InSuper, Index, LiteralNumber, LiteralString,
		LiteralStringKind, Local, LocalBind, MAX_PRECEDENCE, NamedArgument, Node, NodeKind, Object,
		ObjectComp, ObjectField, ObjectFieldHide, ObjectFieldKind, Parameter, Parens, Precedence,
		Slice, SuperIndex, UNARY_PRECEDENCE, Unary, UnaryOp,
	},
	fodder::Fodder,
	lexer::lex,
	location::LocationRange,
	string_util::string_unescape,
	token::{Token, TokenKind},
};

/// `locFromTokens`.
fn loc_from_tokens(begin: &Token, end: &Token) -> LocationRange {
	begin.location.between(&end.location)
}

/// `locFromTokenAST`.
fn loc_from_token_ast(begin: &Token, end: &Node) -> LocationRange {
	begin.location.between(&end.loc)
}

/// `makeUnexpectedError`.
fn make_unexpected_error(token: &Token, while_doing: &str) -> Error {
	Error::with_context(
		&token.location,
		&format!("Unexpected: {}", token.display()),
		while_doing,
	)
}

/// `parser.unexpectedTokenError`.
///
/// A free function rather than a method: Go hangs it off the parser but it
/// reads no parser state.
///
/// # Panics
///
/// If the token is the kind that was expected, as Go's does — reaching that
/// means the caller has a bug, not that the input was bad.
fn unexpected_token_error(expected: TokenKind, token: &Token) -> Error {
	assert_ne!(expected, token.kind, "Unexpectedly expected token kind");
	Error::from_static(
		&token.location,
		&format!(
			"Expected token {} but got {}",
			expected.name(),
			token.display()
		),
	)
}

/// `tokenStringToAst`.
fn token_string_to_ast(token: &Token) -> Result<Node, Error> {
	let (kind, validate) = match token.kind {
		TokenKind::StringSingle => (LiteralStringKind::Single, true),
		TokenKind::StringDouble => (LiteralStringKind::Double, true),
		TokenKind::StringBlock => (LiteralStringKind::Block, false),
		TokenKind::VerbatimStringDouble => (LiteralStringKind::VerbatimDouble, false),
		TokenKind::VerbatimStringSingle => (LiteralStringKind::VerbatimSingle, false),
		other => panic!("Not a string token {other:?}"),
	};

	let literal = LiteralString {
		value: token.data.clone(),
		// Only ever set on a block string, and reproduced verbatim by the
		// unparser.
		block_indent: token.string_block_indent.clone(),
		block_term_indent: token.string_block_term_indent.clone(),
		kind,
	};
	let node = Node::new(
		token.location.clone(),
		token.fodder.clone(),
		NodeKind::LiteralString(literal),
	);

	if validate {
		// The unescaped text is thrown away: this is only asking whether the
		// escapes are well formed.
		if let Err(err) = string_unescape(&node.loc, &token.data) {
			// `err.message()` already begins with the location, and upstream
			// wraps it in a fresh error at the same token — so the location
			// lands in the message twice. That is what `tk fmt` prints.
			return Err(Error::from_static(&token.location, err.message()));
		}
	}

	Ok(node)
}

/// `parser.Parse`: a token stream to a parse tree, plus the fodder that
/// followed the last token.
pub fn parse(tokens: &[Token]) -> Result<(Node, Fodder), Error> {
	let mut parser = Parser { tokens, current: 0 };
	let expr = parser.parse(MAX_PRECEDENCE)?;

	let eof = parser.peek();
	if eof.kind != TokenKind::EndOfFile {
		return Err(Error::from_static(
			&eof.location,
			&format!("Did not expect: {}", eof.display()),
		));
	}

	Ok((expr, eof.fodder.clone()))
}

/// `parser.SnippetToRawAST`, which is what `formatter.Format` calls.
pub fn snippet_to_raw_ast(
	diagnostic_filename: &str,
	snippet: &str,
) -> Result<(Node, Fodder), Error> {
	let tokens = lex(diagnostic_filename, snippet)?;
	parse(&tokens)
}

/// `parser.parser`.
///
/// The token slice outlives the parser, so every accessor hands back a
/// `&'a Token` rather than something borrowed from `self`. That is what lets
/// `begin` be held across the recursive descent, the way Go holds a pointer.
struct Parser<'a> {
	tokens: &'a [Token],
	current: usize,
}

impl<'a> Parser<'a> {
	fn pop(&mut self) -> &'a Token {
		let token = &self.tokens[self.current];
		self.current += 1;
		token
	}

	fn peek(&self) -> &'a Token {
		&self.tokens[self.current]
	}

	/// `doublePeek`.
	///
	/// Indexes one past `peek`, as Go's does, and would panic at the end of the
	/// stream for the same reason. It cannot get there: the only caller reaches
	/// it having already seen an identifier, and the lexer always appends an
	/// end-of-file token after one.
	fn double_peek(&self) -> &'a Token {
		&self.tokens[self.current + 1]
	}

	/// `popExpect`.
	fn pop_expect(&mut self, expected: TokenKind) -> Result<&'a Token, Error> {
		let token = self.pop();
		if token.kind != expected {
			return Err(unexpected_token_error(expected, token));
		}
		Ok(token)
	}

	/// `popExpectOp`.
	fn pop_expect_op(&mut self, op: &str) -> Result<&'a Token, Error> {
		let token = self.pop();
		if token.kind != TokenKind::Operator || token.data != op {
			return Err(Error::from_static(
				&token.location,
				&format!("Expected operator {op} but got {}", token.display()),
			));
		}
		Ok(token)
	}

	/// `parseArgument`: either `<f1> id <f2> = expr`, or just `expr`.
	fn parse_argument(&mut self) -> Result<Argument, Error> {
		let (name_fodder, name, eq_fodder) = if self.peek().kind == TokenKind::Identifier
			&& self.double_peek().kind == TokenKind::Operator
			&& self.double_peek().data == "="
		{
			let ident = self.pop();
			let eq = self.pop();
			(
				ident.fodder.clone(),
				Some(ident.data.clone()),
				eq.fodder.clone(),
			)
		} else {
			(Fodder::new(), None, Fodder::new())
		};

		let expr = self.parse(MAX_PRECEDENCE)?;
		Ok(Argument {
			name_fodder,
			name,
			eq_fodder,
			expr,
		})
	}

	/// `parseArguments`. The `bool` is whether the list ended with a comma.
	fn parse_arguments(
		&mut self,
		element_kind: &str,
	) -> Result<(&'a Token, Arguments, bool), Error> {
		let mut args = Arguments::default();
		let mut got_comma = false;
		let mut named_argument_added = false;
		let mut first = true;

		loop {
			let next = self.peek();

			if next.kind == TokenKind::ParenR {
				// `got_comma` can be either here.
				return Ok((self.pop(), args, got_comma));
			}

			if !first && !got_comma {
				return Err(Error::from_static(
					&next.location,
					&format!(
						"Expected a comma before next {element_kind}, got {}",
						next.display()
					),
				));
			}

			let argument = self.parse_argument()?;

			let mut comma_fodder = Fodder::new();
			if self.peek().kind == TokenKind::Comma {
				comma_fodder = self.pop().fodder.clone();
				got_comma = true;
			} else {
				got_comma = false;
			}

			match argument.name {
				None => {
					if named_argument_added {
						return Err(Error::from_static(
							&next.location,
							"Positional argument after a named argument is not allowed",
						));
					}
					args.positional.push(CommaSeparatedExpr {
						expr: Box::new(argument.expr),
						// Upstream only attaches it where a comma was actually
						// seen; with none, `comma_fodder` is empty anyway.
						comma_fodder,
					});
				}
				Some(name) => {
					named_argument_added = true;
					args.named.push(NamedArgument {
						name_fodder: argument.name_fodder,
						name,
						eq_fodder: argument.eq_fodder,
						arg: Box::new(argument.expr),
						comma_fodder,
					});
				}
			}

			first = false;
		}
	}

	/// `parseParameter`: either `<f1> id <f2> = expr`, or just `<f1> id`.
	fn parse_parameter(&mut self) -> Result<Parameter, Error> {
		let ident = self
			.pop_expect(TokenKind::Identifier)
			.map_err(|err| err.in_context("parsing parameter"))?;

		let mut parameter = Parameter {
			name_fodder: ident.fodder.clone(),
			name: ident.data.clone(),
			eq_fodder: Fodder::new(),
			default_arg: None,
			comma_fodder: Fodder::new(),
		};

		if self.peek().kind == TokenKind::Operator && self.peek().data == "=" {
			parameter.eq_fodder = self.pop().fodder.clone();
			parameter.default_arg = Some(Box::new(self.parse(MAX_PRECEDENCE)?));
		}

		Ok(parameter)
	}

	/// `parseParameters`. The `bool` is whether the list ended with a comma.
	fn parse_parameters(
		&mut self,
		element_kind: &str,
	) -> Result<(&'a Token, Vec<Parameter>, bool), Error> {
		let mut params: Vec<Parameter> = Vec::new();
		let mut got_comma = false;
		let mut first = true;

		loop {
			let next = self.peek();

			if next.kind == TokenKind::ParenR {
				// `got_comma` can be either here.
				return Ok((self.pop(), params, got_comma));
			}

			if !first && !got_comma {
				return Err(Error::from_static(
					&next.location,
					&format!(
						"Expected a comma before next {element_kind}, got {}",
						next.display()
					),
				));
			}

			let mut param = self.parse_parameter()?;

			if self.peek().kind == TokenKind::Comma {
				param.comma_fodder = self.pop().fodder.clone();
				got_comma = true;
			} else {
				got_comma = false;
			}
			params.push(param);

			first = false;
		}
	}

	/// The `( params )` that makes a bind or a field into a method.
	///
	/// Go inlines this four times, building an `*ast.Function` whose body it
	/// fills in later. [`FunctionSugar`] has no body to fill, so the four sites
	/// share this.
	fn parse_function_sugar(&mut self, element_kind: &str) -> Result<FunctionSugar, Error> {
		let paren_l = self.pop();
		let (paren_r, parameters, got_comma) = self.parse_parameters(element_kind)?;
		Ok(FunctionSugar {
			paren_left_fodder: paren_l.fodder.clone(),
			parameters,
			trailing_comma: got_comma,
			paren_right_fodder: paren_r.fodder.clone(),
		})
	}

	/// `parseBind`, which appends to `binds` and returns the `,` or `;` that
	/// ended it.
	fn parse_bind(&mut self, binds: &mut Vec<LocalBind>) -> Result<&'a Token, Error> {
		let var_id = self.pop_expect(TokenKind::Identifier)?;

		if binds.iter().any(|bind| bind.variable == var_id.data) {
			return Err(Error::from_static(
				&var_id.location,
				&format!("Duplicate local var: {}", var_id.data),
			));
		}

		let fun = if self.peek().kind == TokenKind::ParenL {
			Some(self.parse_function_sugar("function parameter")?)
		} else {
			None
		};

		let eq_token = self.pop_expect_op("=")?;
		let body = self.parse(MAX_PRECEDENCE)?;

		let delim = self.pop();
		if delim.kind != TokenKind::Semicolon && delim.kind != TokenKind::Comma {
			return Err(Error::from_static(
				&delim.location,
				&format!("Expected , or ; but got {}", delim.display()),
			));
		}

		binds.push(LocalBind {
			var_fodder: var_id.fodder.clone(),
			variable: var_id.data.clone(),
			fun,
			eq_fodder: eq_token.fodder.clone(),
			body: Box::new(body),
			close_fodder: delim.fodder.clone(),
		});

		Ok(delim)
	}

	/// `parseObjectAssignmentOp`: one of `:`, `::`, `:::`, `+:`, `+::`, `+:::`.
	fn parse_object_assignment_op(&mut self) -> Result<(Fodder, bool, ObjectFieldHide), Error> {
		let op = self.pop_expect(TokenKind::Operator)?;
		let op_fodder = op.fodder.clone();

		let malformed = || {
			Error::from_static(
				&op.location,
				&format!(
					"Expected one of :, ::, :::, +:, +::, +:::, got: {}",
					op.data
				),
			)
		};

		let mut rest = op.data.as_str();
		let plus_sugar = rest.starts_with('+');
		if plus_sugar {
			rest = &rest[1..];
		}

		let mut num_colons = 0;
		while !rest.is_empty() {
			if !rest.starts_with(':') {
				return Err(malformed());
			}
			rest = &rest[1..];
			num_colons += 1;
		}

		let hide = match num_colons {
			1 => ObjectFieldHide::Inherit,
			2 => ObjectFieldHide::Hidden,
			3 => ObjectFieldHide::Visible,
			_ => return Err(malformed()),
		};

		Ok((op_fodder, plus_sugar, hide))
	}

	/// `parseObjectRemainderComp`.
	///
	/// Go also takes the opening brace's token and never reads it; that
	/// parameter is dropped here rather than carried unused.
	fn parse_object_remainder_comp(
		&mut self,
		fields: Vec<ObjectField>,
		got_comma: bool,
		brace_l: &'a Token,
		next: &'a Token,
	) -> Result<(Node, &'a Token), Error> {
		let mut num_asserts = 0;
		let mut num_fields = 0;
		let mut only_field: Option<&ObjectField> = None;
		for field in &fields {
			match field.kind {
				ObjectFieldKind::Local => continue,
				ObjectFieldKind::Assert => {
					num_asserts += 1;
					continue;
				}
				_ => {}
			}
			num_fields += 1;
			// Go keeps the last one it saw, which is the only one that can be
			// read below — the count is checked first.
			only_field = Some(field);
		}

		if num_asserts > 0 {
			return Err(Error::from_static(
				&next.location,
				"Object comprehension cannot have asserts",
			));
		}
		if num_fields != 1 {
			return Err(Error::from_static(
				&next.location,
				"Object comprehension can only have one field",
			));
		}
		let field = only_field.expect("exactly one field, just counted");
		if field.hide != ObjectFieldHide::Inherit {
			return Err(Error::from_static(
				&next.location,
				"Object comprehensions cannot have hidden fields",
			));
		}
		if field.kind != ObjectFieldKind::FieldExpr {
			return Err(Error::from_static(
				&next.location,
				"Object comprehensions can only have [e] fields",
			));
		}

		let (spec, last) = self.parse_comprehension_specs(next, TokenKind::BraceR)?;
		let node = Node::new(
			loc_from_tokens(brace_l, last),
			brace_l.fodder.clone(),
			NodeKind::ObjectComp(ObjectComp {
				fields,
				trailing_comma: got_comma,
				trailing_comma_fodder: Fodder::new(),
				spec,
				close_fodder: last.fodder.clone(),
			}),
		);
		Ok((node, last))
	}

	/// `parseObjectRemainderField`: one of the three basic field kinds.
	fn parse_object_remainder_field(
		&mut self,
		literal_fields: &mut HashSet<String>,
		next: &'a Token,
	) -> Result<ObjectField, Error> {
		let mut fodder1 = Fodder::new();
		let mut fodder2 = Fodder::new();
		let mut expr1 = None;
		let mut id = None;

		let kind = match next.kind {
			TokenKind::Identifier => {
				fodder1 = next.fodder.clone();
				id = Some(next.data.clone());
				ObjectFieldKind::FieldId
			}
			TokenKind::StringDouble
			| TokenKind::StringSingle
			| TokenKind::StringBlock
			| TokenKind::VerbatimStringDouble
			| TokenKind::VerbatimStringSingle => {
				// No `fodder1`: the literal carries its own opening fodder,
				// which is why `unparseFields` unparses `Expr1` for this kind
				// instead of filling a slot.
				expr1 = Some(Box::new(token_string_to_ast(next)?));
				ObjectFieldKind::FieldStr
			}
			_ => {
				// A computed name. `next` is the `[`.
				fodder1 = next.fodder.clone();
				expr1 = Some(Box::new(self.parse(MAX_PRECEDENCE)?));
				let bracket_r = self.pop_expect(TokenKind::BracketR)?;
				fodder2 = bracket_r.fodder.clone();
				ObjectFieldKind::FieldExpr
			}
		};

		let method = if self.peek().kind == TokenKind::ParenL {
			Some(self.parse_function_sugar("method parameter")?)
		} else {
			None
		};

		let (op_fodder, plus_sugar, hide) = self.parse_object_assignment_op()?;

		if plus_sugar && method.is_some() {
			return Err(Error::from_static(
				&next.location,
				&format!("Cannot use +: syntax sugar in a method: {}", next.data),
			));
		}

		// Keyed on the token's raw data, so a quoted `'a'` and a bare `a` are
		// the same field name for this purpose.
		if kind != ObjectFieldKind::FieldExpr && !literal_fields.insert(next.data.clone()) {
			return Err(Error::from_static(
				&next.location,
				&format!("Duplicate field: {}", next.data),
			));
		}

		let body = self.parse(MAX_PRECEDENCE)?;

		Ok(ObjectField {
			kind,
			hide,
			super_sugar: plus_sugar,
			method,
			id,
			fodder1,
			fodder2,
			op_fodder,
			comma_fodder: self.peeked_comma_fodder(),
			expr1,
			expr2: Some(Box::new(body)),
			expr3: None,
		})
	}

	/// `parseObjectRemainderLocal`.
	fn parse_object_remainder_local(
		&mut self,
		binds: &mut HashSet<String>,
		next: &'a Token,
	) -> Result<ObjectField, Error> {
		let var_id = self.pop_expect(TokenKind::Identifier)?;

		if binds.contains(&var_id.data) {
			return Err(Error::from_static(
				&var_id.location,
				&format!("Duplicate local var: {}", var_id.data),
			));
		}

		let method = if self.peek().kind == TokenKind::ParenL {
			Some(self.parse_function_sugar("function parameter")?)
		} else {
			None
		};

		let op_token = self.pop_expect_op("=")?;
		let body = self.parse(MAX_PRECEDENCE)?;

		// Added after the body is parsed, as upstream does.
		binds.insert(var_id.data.clone());

		Ok(ObjectField {
			kind: ObjectFieldKind::Local,
			hide: ObjectFieldHide::Visible,
			super_sugar: false,
			method,
			id: Some(var_id.data.clone()),
			fodder1: next.fodder.clone(),
			fodder2: var_id.fodder.clone(),
			op_fodder: op_token.fodder.clone(),
			comma_fodder: self.peeked_comma_fodder(),
			expr1: None,
			expr2: Some(Box::new(body)),
			expr3: None,
		})
	}

	/// `parseObjectRemainderAssert`.
	fn parse_object_remainder_assert(&mut self, next: &'a Token) -> Result<ObjectField, Error> {
		let cond = self.parse(MAX_PRECEDENCE)?;

		let (op_fodder, message) =
			if self.peek().kind == TokenKind::Operator && self.peek().data == ":" {
				let colon_fodder = self.pop().fodder.clone();
				(colon_fodder, Some(Box::new(self.parse(MAX_PRECEDENCE)?)))
			} else {
				(Fodder::new(), None)
			};

		Ok(ObjectField {
			kind: ObjectFieldKind::Assert,
			hide: ObjectFieldHide::Visible,
			super_sugar: false,
			method: None,
			id: None,
			fodder1: next.fodder.clone(),
			fodder2: Fodder::new(),
			op_fodder,
			comma_fodder: self.peeked_comma_fodder(),
			expr1: None,
			expr2: Some(Box::new(cond)),
			expr3: message,
		})
	}

	/// The fodder of a following comma, **copied without consuming it**.
	///
	/// Every field kind ends this way upstream: it peeks, copies the fodder,
	/// and leaves the comma for `parseObjectRemainder` to pop. So the fodder is
	/// read twice and written once.
	fn peeked_comma_fodder(&self) -> Fodder {
		if self.peek().kind == TokenKind::Comma {
			self.peek().fodder.clone()
		} else {
			Fodder::new()
		}
	}

	/// `parseObjectRemainder`: an object or object comprehension, the leading
	/// `{` already consumed and passed as `brace_l`.
	fn parse_object_remainder(&mut self, brace_l: &'a Token) -> Result<(Node, &'a Token), Error> {
		let mut fields: Vec<ObjectField> = Vec::new();
		let mut literal_fields: HashSet<String> = HashSet::new();
		let mut binds: HashSet<String> = HashSet::new();

		let mut got_comma = false;
		let mut first = true;
		let mut next = self.pop();

		loop {
			if next.kind == TokenKind::BraceR {
				let node = Node::new(
					loc_from_tokens(brace_l, next),
					brace_l.fodder.clone(),
					NodeKind::Object(Object {
						fields,
						trailing_comma: got_comma,
						close_fodder: next.fodder.clone(),
					}),
				);
				return Ok((node, next));
			}

			if next.kind == TokenKind::For {
				return self.parse_object_remainder_comp(fields, got_comma, brace_l, next);
			}

			if !got_comma && !first {
				return Err(Error::from_static(
					&next.location,
					"Expected a comma before next field",
				));
			}

			let field = match next.kind {
				TokenKind::BracketL
				| TokenKind::Identifier
				| TokenKind::StringDouble
				| TokenKind::StringSingle
				| TokenKind::StringBlock
				| TokenKind::VerbatimStringDouble
				| TokenKind::VerbatimStringSingle => {
					self.parse_object_remainder_field(&mut literal_fields, next)?
				}
				TokenKind::Local => self.parse_object_remainder_local(&mut binds, next)?,
				TokenKind::Assert => self.parse_object_remainder_assert(next)?,
				_ => return Err(make_unexpected_error(next, "parsing field definition")),
			};
			fields.push(field);

			next = self.pop();
			if next.kind == TokenKind::Comma {
				got_comma = true;
				next = self.pop();
			} else {
				got_comma = false;
			}

			first = false;
		}
	}

	/// `parseComprehensionSpecs`: `for x in e for y in e if e ...`.
	///
	/// Returns the **innermost** spec, which is the last `for` written, with
	/// the earlier ones hanging off its `outer` chain.
	fn parse_comprehension_specs(
		&mut self,
		for_token: &'a Token,
		end: TokenKind,
	) -> Result<(ForSpec, &'a Token), Error> {
		self.parse_comprehension_specs_helper(for_token, None, end)
	}

	fn parse_comprehension_specs_helper(
		&mut self,
		for_token: &'a Token,
		outer: Option<Box<ForSpec>>,
		end: TokenKind,
	) -> Result<(ForSpec, &'a Token), Error> {
		let var_id = self.pop_expect(TokenKind::Identifier)?;
		let in_token = self.pop_expect(TokenKind::In)?;
		let array = self.parse(MAX_PRECEDENCE)?;

		let mut spec = ForSpec {
			for_fodder: for_token.fodder.clone(),
			var_fodder: var_id.fodder.clone(),
			var_name: var_id.data.clone(),
			in_fodder: in_token.fodder.clone(),
			expr: Box::new(array),
			conditions: Vec::new(),
			outer,
		};

		let mut maybe_if = self.pop();
		while maybe_if.kind == TokenKind::If {
			let cond = self.parse(MAX_PRECEDENCE)?;
			spec.conditions.push(IfSpec {
				if_fodder: maybe_if.fodder.clone(),
				expr: Box::new(cond),
			});
			maybe_if = self.pop();
		}

		if maybe_if.kind == end {
			return Ok((spec, maybe_if));
		}
		if maybe_if.kind != TokenKind::For {
			return Err(Error::from_static(
				&maybe_if.location,
				&format!(
					"Expected for, if or {} after for clause, got: {}",
					end.name(),
					maybe_if.display()
				),
			));
		}

		self.parse_comprehension_specs_helper(maybe_if, Some(Box::new(spec)), end)
	}

	/// `parseArray`, the leading `[` already consumed and passed as
	/// `bracket_l`. Reads up to and consumes the trailing `]`.
	fn parse_array(&mut self, bracket_l: &'a Token) -> Result<Node, Error> {
		if self.peek().kind == TokenKind::BracketR {
			let bracket_r = self.pop();
			return Ok(Node::new(
				loc_from_tokens(bracket_l, bracket_r),
				bracket_l.fodder.clone(),
				NodeKind::Array(Array {
					elements: Vec::new(),
					trailing_comma: false,
					close_fodder: bracket_r.fodder.clone(),
				}),
			));
		}

		let first = self.parse(MAX_PRECEDENCE)?;

		let mut got_comma = false;
		let mut comma_fodder = Fodder::new();
		if self.peek().kind == TokenKind::Comma {
			comma_fodder = self.pop().fodder.clone();
			got_comma = true;
		}

		if self.peek().kind == TokenKind::For {
			let for_token = self.pop();
			let (spec, last) = self.parse_comprehension_specs(for_token, TokenKind::BracketR)?;
			return Ok(Node::new(
				loc_from_tokens(bracket_l, last),
				bracket_l.fodder.clone(),
				NodeKind::ArrayComp(ArrayComp {
					body: Box::new(first),
					trailing_comma: got_comma,
					trailing_comma_fodder: comma_fodder,
					spec,
					close_fodder: last.fodder.clone(),
				}),
			));
		}

		// Not a comprehension, so it may have more elements.
		let mut elements = vec![CommaSeparatedExpr {
			expr: Box::new(first),
			comma_fodder,
		}];

		let bracket_r = loop {
			let next = self.peek();
			if next.kind == TokenKind::BracketR {
				break self.pop();
			}
			if !got_comma {
				return Err(Error::from_static(
					&next.location,
					"Expected a comma before next array element",
				));
			}

			let expr = self.parse(MAX_PRECEDENCE)?;
			let mut element = CommaSeparatedExpr {
				expr: Box::new(expr),
				comma_fodder: Fodder::new(),
			};
			if self.peek().kind == TokenKind::Comma {
				element.comma_fodder = self.pop().fodder.clone();
				got_comma = true;
			} else {
				got_comma = false;
			}
			elements.push(element);
		};

		Ok(Node::new(
			loc_from_tokens(bracket_l, bracket_r),
			bracket_l.fodder.clone(),
			NodeKind::Array(Array {
				elements,
				trailing_comma: got_comma,
				close_fodder: bracket_r.fodder.clone(),
			}),
		))
	}

	/// `parseTerminal`.
	fn parse_terminal(&mut self) -> Result<Node, Error> {
		let tok = self.pop();

		match tok.kind {
			TokenKind::Assert
			| TokenKind::BraceR
			| TokenKind::BracketR
			| TokenKind::Comma
			| TokenKind::Dot
			| TokenKind::Else
			| TokenKind::Error
			| TokenKind::For
			| TokenKind::Function
			| TokenKind::If
			| TokenKind::In
			| TokenKind::Import
			| TokenKind::ImportStr
			| TokenKind::ImportBin
			| TokenKind::Local
			| TokenKind::Operator
			| TokenKind::ParenR
			| TokenKind::Semicolon
			| TokenKind::TailStrict
			| TokenKind::Then => Err(make_unexpected_error(tok, "parsing terminal")),

			TokenKind::EndOfFile => {
				Err(Error::from_static(&tok.location, "Unexpected end of file"))
			}

			TokenKind::BraceL => {
				let (obj, _) = self.parse_object_remainder(tok)?;
				Ok(obj)
			}

			TokenKind::BracketL => self.parse_array(tok),

			TokenKind::ParenL => {
				let inner = self.parse(MAX_PRECEDENCE)?;
				let paren_r = self.pop_expect(TokenKind::ParenR)?;
				Ok(Node::new(
					loc_from_tokens(tok, paren_r),
					tok.fodder.clone(),
					NodeKind::Parens(Parens {
						inner: Box::new(inner),
						close_fodder: paren_r.fodder.clone(),
					}),
				))
			}

			TokenKind::Number => Ok(Node::new(
				tok.location.clone(),
				tok.fodder.clone(),
				NodeKind::LiteralNumber(LiteralNumber {
					original_string: tok.data.clone(),
				}),
			)),

			TokenKind::StringDouble
			| TokenKind::StringSingle
			| TokenKind::StringBlock
			| TokenKind::VerbatimStringDouble
			| TokenKind::VerbatimStringSingle => token_string_to_ast(tok),

			TokenKind::False => Ok(Node::new(
				tok.location.clone(),
				tok.fodder.clone(),
				NodeKind::LiteralBoolean(false),
			)),
			TokenKind::True => Ok(Node::new(
				tok.location.clone(),
				tok.fodder.clone(),
				NodeKind::LiteralBoolean(true),
			)),
			TokenKind::NullLit => Ok(Node::new(
				tok.location.clone(),
				tok.fodder.clone(),
				NodeKind::LiteralNull,
			)),

			TokenKind::Dollar => Ok(Node::new(
				tok.location.clone(),
				tok.fodder.clone(),
				NodeKind::Dollar,
			)),
			TokenKind::Identifier => Ok(Node::new(
				tok.location.clone(),
				tok.fodder.clone(),
				NodeKind::Var(tok.data.clone()),
			)),
			TokenKind::SelfKw => Ok(Node::new(
				tok.location.clone(),
				tok.fodder.clone(),
				NodeKind::SelfExpr,
			)),

			TokenKind::Super => {
				let next = self.pop();
				let mut index = None;
				let mut id: Option<Identifier> = None;
				let id_fodder;

				match next.kind {
					TokenKind::Dot => {
						let field_id = self.pop_expect(TokenKind::Identifier)?;
						id_fodder = field_id.fodder.clone();
						id = Some(field_id.data.clone());
					}
					TokenKind::BracketL => {
						index = Some(Box::new(self.parse(MAX_PRECEDENCE)?));
						let bracket_r = self.pop_expect(TokenKind::BracketR)?;
						id_fodder = bracket_r.fodder.clone();
					}
					_ => {
						// Reported at the `super`, not at what followed it.
						return Err(Error::from_static(
							&tok.location,
							"Expected . or [ after super",
						));
					}
				}

				Ok(Node::new(
					// Just the `super` token, not a range to the end.
					tok.location.clone(),
					tok.fodder.clone(),
					NodeKind::SuperIndex(SuperIndex {
						dot_fodder: next.fodder.clone(),
						id_fodder,
						id,
						index,
					}),
				))
			}
		}
	}

	/// `parse`, the precedence-climbing core.
	fn parse(&mut self, prec: Precedence) -> Result<Node, Error> {
		let begin = self.peek();

		// These have effectively MaxPrecedence, since the first call to parse
		// is what parses them.
		match begin.kind {
			TokenKind::Assert => return self.parse_assert(begin),
			TokenKind::Error => {
				self.pop();
				let expr = self.parse(MAX_PRECEDENCE)?;
				return Ok(Node::new(
					loc_from_token_ast(begin, &expr),
					begin.fodder.clone(),
					NodeKind::Error(ErrorExpr {
						expr: Box::new(expr),
					}),
				));
			}
			TokenKind::If => return self.parse_conditional(begin),
			TokenKind::Function => return self.parse_function(begin),
			TokenKind::Import => return self.parse_import(begin, NodeKind::Import),
			TokenKind::ImportStr => return self.parse_import(begin, NodeKind::ImportStr),
			TokenKind::ImportBin => return self.parse_import(begin, NodeKind::ImportBin),
			TokenKind::Local => return self.parse_local(begin),
			_ => {}
		}

		// A unary operator.
		if begin.kind == TokenKind::Operator {
			let Some(uop) = UnaryOp::from_symbol(&begin.data) else {
				// Unconditional: an operator that is not a unary one fails
				// here whatever the precedence.
				return Err(Error::from_static(
					&begin.location,
					&format!("Not a unary operator: {}", begin.data),
				));
			};
			if prec == UNARY_PRECEDENCE {
				let op = self.pop();
				let expr = self.parse(prec)?;
				return Ok(Node::new(
					loc_from_token_ast(op, &expr),
					begin.fodder.clone(),
					NodeKind::Unary(Unary {
						op: uop,
						expr: Box::new(expr),
					}),
				));
			}
		}

		if prec == 0 {
			return self.parse_terminal();
		}

		let mut lhs = self.parse(prec - 1)?;

		loop {
			// The next token has to be a binary operator, or one of the
			// postfix forms, or we are done at this level.
			let bop = match self.peek().kind {
				TokenKind::In => {
					if BinaryOp::In.precedence() != prec {
						return Ok(lhs);
					}
					Some(BinaryOp::In)
				}
				TokenKind::Operator => {
					// A colon terminates the expression: it belongs to an
					// enclosing assert or field, and must not trip the
					// is-a-binary-operator test below.
					if self.peek().data == ":" {
						return Ok(lhs);
					}
					// Likewise `::`, for `[e::]`.
					if self.peek().data == "::" {
						return Ok(lhs);
					}
					let Some(op) = BinaryOp::from_symbol(&self.peek().data) else {
						return Err(Error::from_static(
							&self.peek().location,
							&format!("Not a binary operator: {}", self.peek().data),
						));
					};
					if op.precedence() != prec {
						return Ok(lhs);
					}
					Some(op)
				}
				TokenKind::Dot | TokenKind::BracketL | TokenKind::ParenL | TokenKind::BraceL => {
					if APPLY_PRECEDENCE != prec {
						return Ok(lhs);
					}
					None
				}
				_ => return Ok(lhs),
			};

			let op = self.pop();
			lhs = match op.kind {
				TokenKind::BracketL => self.parse_index_or_slice(begin, op, lhs)?,
				TokenKind::Dot => {
					let field_id = self.pop_expect(TokenKind::Identifier)?;
					Node::new(
						loc_from_tokens(begin, field_id),
						// Left-recursive: the opening fodder stays on the
						// leftmost leaf.
						Fodder::new(),
						NodeKind::Index(Index {
							target: Box::new(lhs),
							left_bracket_fodder: op.fodder.clone(),
							right_bracket_fodder: field_id.fodder.clone(),
							id: Some(field_id.data.clone()),
							index: None,
						}),
					)
				}
				TokenKind::ParenL => {
					let (end, arguments, got_comma) = self.parse_arguments("function argument")?;
					let mut tail_strict = false;
					let mut tail_strict_fodder = Fodder::new();
					if self.peek().kind == TokenKind::TailStrict {
						tail_strict_fodder = self.pop().fodder.clone();
						tail_strict = true;
					}
					Node::new(
						loc_from_tokens(begin, end),
						Fodder::new(),
						NodeKind::Apply(Apply {
							target: Box::new(lhs),
							fodder_left: op.fodder.clone(),
							arguments,
							fodder_right: end.fodder.clone(),
							tail_strict_fodder,
							trailing_comma: got_comma,
							tail_strict,
						}),
					)
				}
				TokenKind::BraceL => {
					let (obj, end) = self.parse_object_remainder(op)?;
					Node::new(
						loc_from_tokens(begin, end),
						Fodder::new(),
						NodeKind::ApplyBrace(ApplyBrace {
							left: Box::new(lhs),
							right: Box::new(obj),
						}),
					)
				}
				_ => {
					if op.kind == TokenKind::In && self.peek().kind == TokenKind::Super {
						let super_token = self.pop();
						Node::new(
							loc_from_tokens(begin, super_token),
							Fodder::new(),
							NodeKind::InSuper(InSuper {
								index: Box::new(lhs),
								in_fodder: op.fodder.clone(),
								super_fodder: super_token.fodder.clone(),
							}),
						)
					} else {
						let rhs = self.parse(prec - 1)?;
						Node::new(
							loc_from_token_ast(begin, &rhs),
							Fodder::new(),
							NodeKind::Binary(Binary {
								left: Box::new(lhs),
								op_fodder: op.fodder.clone(),
								// Only the `In` and `Operator` arms fall
								// through to here, and both set it.
								op: bop.expect("a binary operator was matched above"),
								right: Box::new(rhs),
							}),
						)
					}
				}
			};
		}
	}

	/// The `assert` expression arm of `parse`.
	fn parse_assert(&mut self, begin: &'a Token) -> Result<Node, Error> {
		self.pop();
		let cond = self.parse(MAX_PRECEDENCE)?;

		let (colon_fodder, message) =
			if self.peek().kind == TokenKind::Operator && self.peek().data == ":" {
				let fodder = self.pop().fodder.clone();
				(fodder, Some(Box::new(self.parse(MAX_PRECEDENCE)?)))
			} else {
				(Fodder::new(), None)
			};

		let semicolon = self.pop_expect(TokenKind::Semicolon)?;
		let rest = self.parse(MAX_PRECEDENCE)?;

		Ok(Node::new(
			loc_from_token_ast(begin, &rest),
			begin.fodder.clone(),
			NodeKind::Assert(Assert {
				cond: Box::new(cond),
				message,
				rest: Box::new(rest),
				colon_fodder,
				semicolon_fodder: semicolon.fodder.clone(),
			}),
		))
	}

	/// The `if` arm of `parse`.
	fn parse_conditional(&mut self, begin: &'a Token) -> Result<Node, Error> {
		self.pop();
		let cond = self.parse(MAX_PRECEDENCE)?;
		let then_token = self.pop_expect(TokenKind::Then)?;
		let branch_true = self.parse(MAX_PRECEDENCE)?;

		let mut else_fodder = Fodder::new();
		let mut branch_false = None;
		// The location stops at the true branch unless there is an else.
		let mut loc = loc_from_token_ast(begin, &branch_true);

		if self.peek().kind == TokenKind::Else {
			else_fodder = self.pop().fodder.clone();
			let node = self.parse(MAX_PRECEDENCE)?;
			loc = loc_from_token_ast(begin, &node);
			branch_false = Some(Box::new(node));
		}

		Ok(Node::new(
			loc,
			begin.fodder.clone(),
			NodeKind::Conditional(Conditional {
				cond: Box::new(cond),
				then_fodder: then_token.fodder.clone(),
				branch_true: Box::new(branch_true),
				else_fodder,
				branch_false,
			}),
		))
	}

	/// The `function` arm of `parse`.
	fn parse_function(&mut self, begin: &'a Token) -> Result<Node, Error> {
		self.pop();
		let next = self.pop();
		if next.kind != TokenKind::ParenL {
			return Err(Error::from_static(
				&next.location,
				&format!("Expected ( but got {}", next.display()),
			));
		}
		let (paren_r, parameters, got_comma) = self.parse_parameters("function parameter")?;
		let body = self.parse(MAX_PRECEDENCE)?;

		Ok(Node::new(
			loc_from_token_ast(begin, &body),
			begin.fodder.clone(),
			NodeKind::Function(Function {
				paren_left_fodder: next.fodder.clone(),
				parameters,
				trailing_comma: got_comma,
				paren_right_fodder: paren_r.fodder.clone(),
				body: Box::new(body),
			}),
		))
	}

	/// The `import`, `importstr` and `importbin` arms of `parse`, which upstream
	/// spells out three times.
	fn parse_import(
		&mut self,
		begin: &'a Token,
		wrap: fn(Import) -> NodeKind,
	) -> Result<Node, Error> {
		self.pop();
		let body = self.parse(MAX_PRECEDENCE)?;

		match &body.kind {
			NodeKind::LiteralString(literal) => {
				if literal.kind == LiteralStringKind::Block {
					return Err(Error::from_static(
						&body.loc,
						"Block string literals not allowed in imports",
					));
				}
			}
			_ => {
				return Err(Error::from_static(
					&body.loc,
					"Computed imports are not allowed",
				));
			}
		}

		Ok(Node::new(
			loc_from_token_ast(begin, &body),
			begin.fodder.clone(),
			wrap(Import {
				file: Box::new(body),
			}),
		))
	}

	/// The `local` arm of `parse`.
	fn parse_local(&mut self, begin: &'a Token) -> Result<Node, Error> {
		self.pop();
		let mut binds: Vec<LocalBind> = Vec::new();
		loop {
			let delim = self.parse_bind(&mut binds)?;
			if delim.kind == TokenKind::Semicolon {
				break;
			}
		}
		let body = self.parse(MAX_PRECEDENCE)?;

		Ok(Node::new(
			loc_from_token_ast(begin, &body),
			begin.fodder.clone(),
			NodeKind::Local(Local {
				binds,
				body: Box::new(body),
			}),
		))
	}

	/// The `[` arm of the postfix loop, which is an index or a slice and does
	/// not know which until it has read the colons.
	fn parse_index_or_slice(
		&mut self,
		begin: &'a Token,
		bracket_l: &'a Token,
		target: Node,
	) -> Result<Node, Error> {
		let mut indexes: [Option<Box<Node>>; 3] = [None, None, None];
		let mut fodders: [Fodder; 3] = [Fodder::new(), Fodder::new(), Fodder::new()];
		let mut colons_consumed: usize = 0;

		let mut end: Option<&'a Token> = None;
		let mut ready_for_next_index = true;
		let mut right_bracket_fodder = Fodder::new();

		while colons_consumed < 3 {
			if self.peek().kind == TokenKind::BracketR {
				let token = self.pop();
				end = Some(token);
				right_bracket_fodder = token.fodder.clone();
				break;
			} else if self.peek().data == ":" {
				let token = self.pop();
				end = Some(token);
				fodders[colons_consumed] = token.fodder.clone();
				colons_consumed += 1;
				ready_for_next_index = true;
			} else if self.peek().data == "::" {
				// One token, two colons — which is why `a[::]` never assigns
				// the step slot and so parses identically to `a[:]`.
				let token = self.pop();
				end = Some(token);
				fodders[colons_consumed] = token.fodder.clone();
				colons_consumed += 2;
				ready_for_next_index = true;
			} else if ready_for_next_index {
				indexes[colons_consumed] = Some(Box::new(self.parse(MAX_PRECEDENCE)?));
				ready_for_next_index = false;
			} else {
				return Err(unexpected_token_error(TokenKind::BracketR, self.peek()));
			}
		}

		// Every branch that can end the loop sets `end` first.
		let end = end.expect("the loop cannot exit before reading a token");

		if colons_consumed > 2 {
			// e.g. `target[42:42:42:42]`.
			return Err(Error::from_static(
				&end.location,
				"Invalid slice: too many colons",
			));
		}
		if colons_consumed == 0 && ready_for_next_index {
			// e.g. `target[]`.
			return Err(Error::from_static(
				&end.location,
				"ast.Index requires an expression",
			));
		}

		let [first, second, third] = indexes;
		let [end_colon_fodder, step_colon_fodder, _] = fodders;

		let kind = if colons_consumed > 0 {
			NodeKind::Slice(Slice {
				target: Box::new(target),
				left_bracket_fodder: bracket_l.fodder.clone(),
				begin_index: first,
				end_colon_fodder,
				end_index: second,
				step_colon_fodder,
				step: third,
				right_bracket_fodder,
			})
		} else {
			NodeKind::Index(Index {
				target: Box::new(target),
				left_bracket_fodder: bracket_l.fodder.clone(),
				right_bracket_fodder,
				id: None,
				index: first,
			})
		};

		Ok(Node::new(
			loc_from_tokens(begin, end),
			// Left-recursive, so the opening fodder is not here.
			Fodder::new(),
			kind,
		))
	}
}

/// What [`Parser::parse_argument`] found: an expression, and the name and
/// fodder of `id =` where there was one.
///
/// Go returns a four-tuple with a nil `*Identifier` standing for "positional".
struct Argument {
	name_fodder: Fodder,
	name: Option<Identifier>,
	eq_fodder: Fodder,
	expr: Node,
}

#[cfg(test)]
mod tests {
	use super::*;

	fn parse_str(input: &str) -> Result<(Node, Fodder), Error> {
		snippet_to_raw_ast("f.jsonnet", input)
	}

	fn parse_ok(input: &str) -> Node {
		parse_str(input)
			.unwrap_or_else(|err| panic!("{input:?} should parse: {}", err.message()))
			.0
	}

	fn parse_err(input: &str) -> String {
		parse_str(input)
			.expect_err(&format!("{input:?} should not parse"))
			.message()
			.to_owned()
	}

	/// Assert on the message body, leaving the location to the oracle.
	///
	/// Deliberately not exact-matching the rendered string here. Locations are
	/// ranges whose ends this port computes, and an expectation *derived by
	/// reading* is how two of sixteen hand-written lexer expectations came out
	/// wrong. `tests/node_parity.rs` compares the whole message, location
	/// included, against what go-jsonnet actually printed — for three real
	/// corpus files and for every snippet — so these tests are free to say
	/// only what they are sure of.
	fn assert_message(input: &str, expected: &str) {
		let message = parse_err(input);
		assert!(
			message.ends_with(expected),
			"parsing {input:?}\n  expected a message ending {expected:?}\n  got {message:?}"
		);
		assert!(
			message.starts_with("f.jsonnet:"),
			"the location should be rendered in front: {message:?}"
		);
	}

	#[test]
	fn the_object_and_slice_refusals_are_reproduced() {
		// The three parse errors in the 138-file corpus are
		// `Duplicate field`, `Computed imports are not allowed` and
		// `Expected . or [ after super`, so those are graded end to end by the
		// node oracle. These check the same code paths from the inside.
		assert_message("{ c: 1, c: 2 }", "Duplicate field: c");
		assert_message("import 'a' + 'b'", "Computed imports are not allowed");
		assert_message("{ a: super }", "Expected . or [ after super");
		assert_message(
			"local a = [1]; a[1:2:3:4]",
			"Invalid slice: too many colons",
		);
		assert_message("local a = [1]; a[]", "ast.Index requires an expression");
		assert_message(
			"import |||\n  x\n|||",
			"Block string literals not allowed in imports",
		);
		assert_message("{ a: 1\n", "Expected a comma before next field");
		assert_message("local a = 1, a = 2; a", "Duplicate local var: a");
		assert_message("{ f(x)+: 1 }", "Cannot use +: syntax sugar in a method: f");
		assert_message(
			"{ [x]: 1, [y]: 2 for x in [1] }",
			"Object comprehension can only have one field",
		);
		assert_message(
			"{ assert true, [x]: 1 for x in [1] }",
			"Object comprehension cannot have asserts",
		);
		assert_message(
			"{ [x]:: 1 for x in [1] }",
			"Object comprehensions cannot have hidden fields",
		);
		assert_message(
			"{ a: 1 for x in [1] }",
			"Object comprehensions can only have [e] fields",
		);
	}

	#[test]
	fn context_is_appended_after_the_message() {
		// `WithContext` wraps the message and leaves the location in front, so
		// the context lands at the end of the rendered string rather than
		// between the location and the message.
		assert_message(
			"function(1) x",
			"Expected token IDENTIFIER but got (NUMBER, \"1\") while parsing parameter",
		);
		assert_message("{ ; }", "Unexpected: \";\" while parsing field definition");
		assert_message("[1 2]", "Expected a comma before next array element");
	}

	#[test]
	fn a_bad_escape_reports_its_location_twice() {
		// Upstream validates by calling StringUnescape and wrapping
		// `err.Error()` — already location-prefixed — in a fresh error at the
		// same token, so the location lands in the message twice. Reproduced,
		// because it is what `tk fmt` prints. The shape is checked rather than
		// the columns: two identical `f.jsonnet:` prefixes.
		let message = parse_err(r"'\q'");
		assert!(
			message.ends_with("Unknown escape sequence in string literal: \\q"),
			"{message:?}"
		);
		assert_eq!(
			message.matches("f.jsonnet:").count(),
			2,
			"the location should appear twice, as upstream renders it: {message:?}"
		);
	}

	#[test]
	fn a_verbatim_string_is_not_validated_for_escapes() {
		// `validate` is false for the verbatim kinds, so a backslash that
		// would be a bad escape elsewhere is just a character here.
		parse_ok(r"@'\q'");
		parse_ok("|||\n  \\q\n|||");
	}

	#[test]
	fn left_recursive_nodes_keep_their_fodder_on_the_leftmost_leaf() {
		// The single most consequential fodder rule in the parser: the outer
		// node gets none, and `openFodder` has to walk down to find it.
		let node = parse_ok("/* lead */ 1 + 2 * 3");
		assert_eq!(node.kind.name(), "Binary");
		assert!(
			node.fodder.is_empty(),
			"a left-recursive node stores its opening fodder further in"
		);
		assert_eq!(
			node.left_recursive_deep().fodder.len(),
			1,
			"the leftmost leaf holds it"
		);
	}

	#[test]
	fn a_double_colon_slice_parses_as_a_single_colon_one() {
		// `::` is one operator token, so the parser's `::` branch bumps the
		// count by two and never assigns the step slot. The node oracle showed
		// this; it is not a reading of the source.
		let double = parse_ok("local a = [1, 2, 3];\na[::]");
		let single = parse_ok("local a = [1, 2, 3];\na[:]");

		let step_colon = |node: &Node| {
			let NodeKind::Local(local) = &node.kind else {
				unreachable!("a local")
			};
			let NodeKind::Slice(slice) = &local.body.kind else {
				unreachable!("a slice")
			};
			(slice.step_colon_fodder.len(), slice.step.is_some())
		};

		assert_eq!(step_colon(&double), (0, false));
		assert_eq!(step_colon(&single), (0, false));
	}

	#[test]
	fn a_colon_terminates_an_expression_rather_than_being_an_operator() {
		// Without the special case in the postfix loop, the `:` of an assert
		// message would trip the is-a-binary-operator test.
		let node = parse_ok("assert 1 == 1 : 'boom';\n42");
		assert_eq!(node.kind.name(), "Assert");
	}

	#[test]
	fn an_operator_that_is_not_unary_is_refused_at_any_precedence() {
		// The check runs before the precedence test, so it fires at the top
		// of `parse` rather than only at UnaryPrecedence.
		assert_message("* 1", "Not a unary operator: *");
		assert_message("1 + ?", "Could not lex the character '?'");
	}

	#[test]
	fn trailing_fodder_comes_back_beside_the_root() {
		let (_, final_fodder) = parse_str("1\n\n// trailing\n").expect("parses");
		assert!(
			!final_fodder.is_empty(),
			"the fodder after the last token is the unparser's finish_file input"
		);
	}

	#[test]
	fn a_trailing_token_after_the_expression_is_refused() {
		assert_message("1 2", "Did not expect: (NUMBER, \"2\")");
	}
}
