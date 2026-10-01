//! A port of go-jsonnet's `internal/parser/lexer.go`.
//!
//! This is the lexer the **formatter** needs, which is why it is ported rather
//! than reused from jrsonnet: it keeps whitespace and comments as
//! [`Fodder`](crate::fodder::Fodder) instead of discarding them, and the way it
//! builds that fodder decides most of what the output looks like.
//!
//! Three of its decisions are worth knowing before reading any pass:
//!
//! - **A tab counts as 8 spaces** of indent. Nothing downstream can recover the
//!   tab, so a tab-indented file is re-emitted with spaces.
//! - **A comment is a `Paragraph` or a `LineEnd` depending on `fresh_line`** —
//!   whether anything but whitespace has been seen on this line yet. The same
//!   `// text` is one or the other according to what precedes it.
//! - **Trailing horizontal whitespace is dropped** from comments, and final
//!   whitespace is discarded entirely, which is why the unparser has to add the
//!   file's last newline back.
//!
//! Positions are **byte** offsets, because Go's columns are. Graded against
//! real fodder by `make update-fmt-lexer-oracle`.

use crate::{
	Error,
	fodder::{Fodder, FodderElement, FodderKind},
	location::{Location, LocationRange},
	token::{
		Token, TokenKind, is_horizontal_whitespace, is_identifier, is_identifier_first, is_symbol,
		is_whitespace, token_kind_from_id,
	},
};

/// Go's `position`.
#[derive(Debug, Clone, Copy)]
struct Position {
	/// Byte offset of the next rune to read.
	byte_no: usize,
	line_no: usize,
	/// Byte offset just past the last newline.
	line_start: usize,
}

/// Go's `lexer`.
struct Lexer<'a> {
	/// go-jsonnet's `DiagnosticFileName`. Its `importedFilename` is always
	/// `""` when the formatter lexes, and only ever reaches a `LocationRange`'s
	/// unused `FileName`, so it is not carried.
	diagnostic_filename: &'a str,
	input: &'a str,

	tokens: Vec<Token>,

	/// Fodder accumulated for the token not yet emitted.
	fodder: Fodder,
	token_start: usize,
	token_start_loc: Location,

	/// Whether the last rune read was the first on a line, ignoring leading
	/// whitespace. This is what makes a comment a paragraph rather than a line
	/// end.
	fresh_line: bool,

	pos: Position,
}

impl<'a> Lexer<'a> {
	fn new(diagnostic_filename: &'a str, input: &'a str) -> Self {
		Self {
			diagnostic_filename,
			input,
			tokens: Vec::new(),
			fodder: Fodder::new(),
			token_start: 0,
			token_start_loc: Location { line: 1, column: 1 },
			fresh_line: true,
			pos: Position {
				byte_no: 0,
				line_no: 1,
				line_start: 0,
			},
		}
	}

	/// Go's `next`. Returns `None` for Go's `lexEOF`.
	fn next(&mut self) -> Option<char> {
		let c = self.peek()?;
		self.pos.byte_no += c.len_utf8();
		if c == '\n' {
			self.pos.line_start = self.pos.byte_no;
			self.pos.line_no += 1;
			self.fresh_line = true;
		} else if self.fresh_line && !is_whitespace(c) {
			self.fresh_line = false;
		}
		Some(c)
	}

	fn accept_n(&mut self, count: usize) {
		for _ in 0..count {
			self.next();
		}
	}

	fn peek(&self) -> Option<char> {
		self.input[self.pos.byte_no..].chars().next()
	}

	/// Whether the remaining input starts with `prefix`; Go's
	/// `strings.HasPrefix(l.input[l.pos.byteNo:], …)`.
	fn rest_starts_with(&self, prefix: &str) -> bool {
		self.input[self.pos.byte_no..].starts_with(prefix)
	}

	fn location(&self) -> Location {
		Location {
			line: self.pos.line_no,
			// A byte offset, one-based, as Go's is.
			column: self.pos.byte_no - self.pos.line_start + 1,
		}
	}

	/// Go's `resetTokenStart`. Throws away characters but keeps fodder.
	fn reset_token_start(&mut self) {
		self.token_start = self.pos.byte_no;
		self.token_start_loc = self.location();
	}

	fn emit_full_token(
		&mut self,
		kind: TokenKind,
		data: String,
		string_block_indent: String,
		string_block_term_indent: String,
	) {
		let location = LocationRange::new(
			self.diagnostic_filename,
			self.token_start_loc,
			self.location(),
		);
		self.tokens.push(Token {
			kind,
			fodder: std::mem::take(&mut self.fodder),
			data,
			string_block_indent,
			string_block_term_indent,
			location,
		});
	}

	fn emit_token(&mut self, kind: TokenKind) {
		let data = self.input[self.token_start..self.pos.byte_no].to_owned();
		self.emit_full_token(kind, data, String::new(), String::new());
		self.reset_token_start();
	}

	/// Go's `addFodder`, which appends **without** reconciling the invariant.
	///
	/// Safe here because of the order the lexer builds fodder in: `lexWhitespace`
	/// consumes a whole run at once, so it never produces two adjacent line
	/// ends, and a comment always sits between them.
	fn add_fodder(&mut self, kind: FodderKind, blanks: usize, indent: usize, comment: Vec<String>) {
		self.fodder
			.push_raw(FodderElement::new(kind, blanks, indent, comment));
	}

	/// Go's `addFodderSafe`, which goes through `FodderAppend`.
	fn add_fodder_safe(
		&mut self,
		kind: FodderKind,
		blanks: usize,
		indent: usize,
		comment: Vec<String>,
	) {
		self.fodder
			.append(FodderElement::new(kind, blanks, indent, comment));
	}

	fn error_at(&self, message: String, location: Location) -> Error {
		Error::from_static(
			&LocationRange::point(self.diagnostic_filename, location),
			&message,
		)
	}

	fn error_here(&self, message: String) -> Error {
		self.error_at(message, self.location())
	}

	/// Go's `lexWhitespace`: returns the number of newlines and the indent
	/// after the last one. A tab counts as **8**.
	fn lex_whitespace(&mut self) -> (usize, usize) {
		let mut indent = 0;
		let mut new_lines = 0;
		while let Some(c) = self.peek().filter(|c| is_whitespace(*c)) {
			self.next();
			match c {
				// Ignored outright, which is half of why a CRLF file does not
				// round-trip.
				'\r' => {}
				'\n' => {
					indent = 0;
					new_lines += 1;
				}
				' ' => indent += 1,
				'\t' => indent += 8,
				_ => {}
			}
		}
		(new_lines, indent)
	}

	/// Go's `lexUntilNewline`: the rest of the line with trailing horizontal
	/// whitespace trimmed, then the blanks and indent that follow it.
	fn lex_until_newline(&mut self) -> (String, usize, usize) {
		let mut text = String::new();
		let mut last_non_space = 0;
		while let Some(c) = self.peek().filter(|c| *c != '\n') {
			self.next();
			text.push(c);
			if !is_horizontal_whitespace(c) {
				last_non_space = text.len();
			}
		}
		// A byte length, and always on a character boundary because it is only
		// ever set to the length after pushing a whole character.
		text.truncate(last_non_space);

		let (new_lines, indent) = self.lex_whitespace();
		let blanks = new_lines.saturating_sub(1);
		(text, blanks, indent)
	}

	/// Go's `lexIdentifier`, which may emit a keyword.
	fn lex_identifier(&mut self) {
		while self.peek().is_some_and(is_identifier) {
			self.next();
		}
		let kind = token_kind_from_id(&self.input[self.token_start..self.pos.byte_no]);
		self.emit_token(kind);
	}

	/// Go's `lexNumber`, as the same state machine.
	///
	/// Underscores are accepted as digit separators and **dropped** from the
	/// token's data, so `1_000` is emitted as `1000` and the separator does not
	/// survive formatting.
	fn lex_number(&mut self) -> Result<(), Error> {
		#[derive(Clone, Copy)]
		enum State {
			Begin,
			AfterZero,
			AfterOneToNine,
			AfterIntUnderscore,
			AfterDot,
			AfterDigit,
			AfterFracUnderscore,
			AfterE,
			AfterExpSign,
			AfterExpDigit,
			AfterExpUnderscore,
		}

		let mut data = String::new();
		let mut state = State::Begin;

		loop {
			let r = self.peek();
			let digit = r.is_some_and(|c| c.is_ascii_digit());

			match state {
				State::Begin => match r {
					Some('0') => state = State::AfterZero,
					Some(c) if c.is_ascii_digit() => state = State::AfterOneToNine,
					// The caller only enters here on a digit.
					_ => unreachable!("lexNumber called without a leading digit"),
				},
				State::AfterZero => match r {
					Some('.') => state = State::AfterDot,
					Some('e' | 'E') => state = State::AfterE,
					Some('_') => {
						return Err(self.error_here(
							"Couldn't lex number, _ not allowed after leading 0".to_owned(),
						));
					}
					_ => break,
				},
				State::AfterOneToNine => match r {
					Some('.') => state = State::AfterDot,
					Some('e' | 'E') => state = State::AfterE,
					Some('_') => state = State::AfterIntUnderscore,
					_ if digit => state = State::AfterOneToNine,
					_ => break,
				},
				// The only way out of a `_` is a digit, and each of the three
				// returns to a different state. Upstream repeats the message
				// three times too.
				State::AfterIntUnderscore => {
					if !digit {
						return Err(self.error_here(format!(
							"Couldn't lex number, junk after '_': {}",
							quote_rune_to_ascii(r)
						)));
					}
					state = State::AfterOneToNine;
				}
				State::AfterFracUnderscore => {
					if !digit {
						return Err(self.error_here(format!(
							"Couldn't lex number, junk after '_': {}",
							quote_rune_to_ascii(r)
						)));
					}
					state = State::AfterDigit;
				}
				State::AfterExpUnderscore => {
					if !digit {
						return Err(self.error_here(format!(
							"Couldn't lex number, junk after '_': {}",
							quote_rune_to_ascii(r)
						)));
					}
					state = State::AfterExpDigit;
				}
				State::AfterDot => {
					if !digit {
						return Err(self.error_here(format!(
							"Couldn't lex number, junk after decimal point: {}",
							quote_rune_to_ascii(r)
						)));
					}
					state = State::AfterDigit;
				}
				State::AfterDigit => match r {
					Some('e' | 'E') => state = State::AfterE,
					Some('_') => state = State::AfterFracUnderscore,
					_ if digit => state = State::AfterDigit,
					_ => break,
				},
				State::AfterE => match r {
					Some('+' | '-') => state = State::AfterExpSign,
					_ if digit => state = State::AfterExpDigit,
					_ => {
						return Err(self.error_here(format!(
							"Couldn't lex number, junk after 'E': {}",
							quote_rune_to_ascii(r)
						)));
					}
				},
				State::AfterExpSign => {
					if !digit {
						return Err(self.error_here(format!(
							"Couldn't lex number, junk after exponent sign: {}",
							quote_rune_to_ascii(r)
						)));
					}
					state = State::AfterExpDigit;
				}
				State::AfterExpDigit => match r {
					Some('_') => state = State::AfterExpUnderscore,
					_ if digit => state = State::AfterExpDigit,
					_ => break,
				},
			}

			if let Some(c) = r.filter(|c| *c != '_') {
				data.push(c);
			}
			self.next();
		}

		self.emit_full_token(TokenKind::Number, data, String::new(), String::new());
		self.reset_token_start();
		Ok(())
	}

	/// Go's `lexSymbol`: a comment, a `|||` block string, or an operator.
	fn lex_symbol(&mut self) -> Result<(), Error> {
		// `next` clears `fresh_line`, so cache it before consuming anything.
		// This is what decides whether a comment becomes a paragraph or a line
		// end, and so how much vertical space survives around it.
		let fresh_line = self.fresh_line;
		let first = self.next();

		// A `#` or `//` comment, to the end of the line.
		if first == Some('#') || (first == Some('/') && self.peek() == Some('/')) {
			let (comment, blanks, indent) = self.lex_until_newline();
			let kind = if fresh_line {
				FodderKind::Paragraph
			} else {
				FodderKind::LineEnd
			};
			let text = format!("{}{comment}", first.expect("matched above"));
			self.add_fodder(kind, blanks, indent, vec![text]);
			return Ok(());
		}

		if first == Some('/') && self.peek() == Some('*') {
			return self.lex_c_comment();
		}

		if first == Some('|') && self.rest_starts_with("||") {
			return self.lex_text_block();
		}

		// Any run of symbols is one operator.
		while let Some(c) = self.peek().filter(|c| is_symbol(*c)) {
			// `//` cannot be part of an operator. Note this test is written
			// upstream as `c == '/' && rest starts with "/"`, and `rest` starts
			// *at* `c` — so it fires on any `/` at all.
			if c == '/' {
				break;
			}
			// `|||` cannot either, which also covers `|||-`.
			if c == '|' && self.rest_starts_with("||") {
				break;
			}
			self.next();
		}

		// An operator may not end with `+ - ~ ! $` unless it is one rune long,
		// so wind back to the first rune.
		//
		// Upstream reads the last byte **once**, before the loop, and never
		// reassigns it — so the decision to keep winding is made on that one
		// character however far back it goes. `+++` therefore lexes as `+`.
		// This relies on every operator symbol being ASCII.
		let last = self.input.as_bytes()[self.pos.byte_no - 1];
		if matches!(last, b'+' | b'-' | b'~' | b'!' | b'$') {
			while self.pos.byte_no > self.token_start + 1 {
				self.pos.byte_no -= 1;
			}
		}

		if &self.input[self.token_start..self.pos.byte_no] == "$" {
			self.emit_token(TokenKind::Dollar);
		} else {
			self.emit_token(TokenKind::Operator);
		}
		Ok(())
	}

	/// The `/* … */` branch of `lexSymbol`.
	fn lex_c_comment(&mut self) -> Result<(), Error> {
		// The column the `/` sat in, used to decide how much indentation may be
		// stripped from continuation lines.
		let margin = self.pos.byte_no - self.pos.line_start - 1;
		let comment_start = self.token_start_loc;

		self.next(); // the opening `*`
		let mut r = self.next();
		while r != Some('*') || self.peek() != Some('/') {
			if r.is_none() {
				return Err(self.error_at(
					"Multi-line comment has no terminating */".to_owned(),
					comment_start,
				));
			}
			r = self.next();
		}
		self.next(); // the closing `/`

		// Includes the `/*` and `*/`.
		let comment = self.input[self.token_start..self.pos.byte_no].to_owned();
		let (new_lines_after, indent_after) = self.lex_whitespace();

		if comment.contains('\n') {
			let mut lines = line_split(&comment, margin);
			assert!(
				lines[0].starts_with('/'),
				"Invalid parsing of C style comment {lines:?}"
			);

			// Upstream's "little hack to support FodderParagraphs with * down
			// the LHS" adds a space to every line beginning with `*` — but only
			// when *every* line does, and the first line always begins with the
			// comment's own `/`. So it can never fire. Computed anyway, so the
			// port does not quietly diverge if upstream ever fixes it.
			let all_star = lines
				.iter()
				.all(|line| !line.is_empty() && line.starts_with('*'));
			if all_star {
				for line in &mut lines {
					if line.starts_with('*') {
						line.insert(0, ' ');
					}
				}
			}

			// A paragraph has to end a line, so one is assumed when the comment
			// was the last thing on it.
			let (new_lines_after, indent_after) = if new_lines_after == 0 {
				(1, 0)
			} else {
				(new_lines_after, indent_after)
			};
			self.add_fodder_safe(
				FodderKind::Paragraph,
				new_lines_after - 1,
				indent_after,
				lines,
			);
		} else {
			self.add_fodder(FodderKind::Interstitial, 0, 0, vec![comment]);
			if new_lines_after > 0 {
				self.add_fodder(
					FodderKind::LineEnd,
					new_lines_after - 1,
					indent_after,
					Vec::new(),
				);
			}
		}
		Ok(())
	}

	/// The `|||` branch of `lexSymbol`.
	fn lex_text_block(&mut self) -> Result<(), Error> {
		let comment_start = self.token_start_loc;
		self.accept_n(2); // the rest of `|||`

		let chomp_trailing_newline = self.peek() == Some('-');
		if chomp_trailing_newline {
			self.next();
		}

		let mut data = String::new();

		// Skip horizontal whitespace, then require a newline.
		let mut r = self.next();
		while matches!(r, Some(' ' | '\t' | '\r')) {
			r = self.next();
		}
		if r != Some('\n') {
			return Err(self.error_at(
				"Text block requires new line after |||.".to_owned(),
				comment_start,
			));
		}

		// Leading blank lines come before the indent is measured.
		while self.peek() == Some('\n') {
			self.next();
			data.push('\n');
		}

		let rest = &self.input[self.pos.byte_no..];
		let mut white_space = check_whitespace(rest, rest);
		if white_space == 0 {
			return Err(self.error_at(
				"Text block's first line must start with whitespace".to_owned(),
				comment_start,
			));
		}
		let block_indent = self.input[self.pos.byte_no..self.pos.byte_no + white_space].to_owned();

		loop {
			self.accept_n(white_space);
			let mut r = self.next();
			while r != Some('\n') {
				let Some(c) = r else {
					return Err(self.error_at("Unexpected EOF".to_owned(), comment_start));
				};
				data.push(c);
				r = self.next();
			}
			data.push('\n');

			while self.peek() == Some('\n') {
				self.next();
				data.push('\n');
			}

			white_space = check_whitespace(&block_indent, &self.input[self.pos.byte_no..]);
			if white_space == 0 {
				// End of the block: whatever indents the closing `|||`.
				let mut term_indent = String::new();
				while let Some(c) = self.peek().filter(|c| *c == ' ' || *c == '\t') {
					self.next();
					term_indent.push(c);
				}
				if !self.rest_starts_with("|||") {
					return Err(self.error_at(
						"Text block not terminated with |||".to_owned(),
						comment_start,
					));
				}
				self.accept_n(3);

				if chomp_trailing_newline {
					data.pop();
				}
				self.emit_full_token(TokenKind::StringBlock, data, block_indent, term_indent);
				self.reset_token_start();
				return Ok(());
			}
		}
	}

	/// The `'…'` and `"…"` branches of `Lex`.
	///
	/// The data excludes the quotes but keeps every escape sequence exactly as
	/// written, because the unparser writes it straight back out.
	fn lex_quoted_string(&mut self, quote: char, kind: TokenKind) -> Result<(), Error> {
		let string_start = self.location();
		self.next(); // the opening quote
		loop {
			let Some(c) = self.next() else {
				return Err(self.error_at("Unterminated String".to_owned(), string_start));
			};
			if c == quote {
				// The quotes are one byte each.
				let data = self.input[self.token_start + 1..self.pos.byte_no - 1].to_owned();
				self.emit_full_token(kind, data, String::new(), String::new());
				self.reset_token_start();
				return Ok(());
			}
			if c == '\\' && self.peek().is_some() {
				self.next();
			}
		}
	}

	/// The `@'…'` and `@"…"` branches of `Lex`.
	///
	/// Here the doubled quotes *are* resolved, because no information is lost:
	/// the unparser puts them back.
	fn lex_verbatim_string(&mut self) -> Result<(), Error> {
		let string_start = self.location();
		self.next(); // the `@`
		let quote = self.next();
		let kind = match quote {
			Some('"') => TokenKind::VerbatimStringDouble,
			Some('\'') => TokenKind::VerbatimStringSingle,
			// Upstream formats the offending rune with `%v`, which prints its
			// numeric value rather than the character. Reproduced, oddity and
			// all, because the message is part of the contract.
			_ => {
				let shown = quote.map_or(-1, |c| c as i64);
				return Err(self.error_at(
					format!("Couldn't lex verbatim string, junk after '@': {shown}"),
					string_start,
				));
			}
		};
		let quote = quote.expect("matched above");

		let mut data = String::new();
		loop {
			let Some(c) = self.next() else {
				return Err(self.error_at("Unterminated String".to_owned(), string_start));
			};
			if c == quote {
				if self.peek() == Some(quote) {
					self.next();
					data.push(c);
				} else {
					self.emit_full_token(kind, data, String::new(), String::new());
					self.reset_token_start();
					return Ok(());
				}
			} else {
				data.push(c);
			}
		}
	}
}

/// Go's `parser.Lex`.
///
/// `diagnostic_filename` is what error messages are prefixed with, and is the
/// name `Format` was called with.
pub fn lex(diagnostic_filename: &str, input: &str) -> Result<Vec<Token>, Error> {
	let mut l = Lexer::new(diagnostic_filename, input);

	loop {
		let (new_lines, indent) = l.lex_whitespace();

		// Final whitespace is discarded rather than kept as fodder, which is
		// why the unparser has to guarantee the file's last newline itself.
		if l.peek().is_none() {
			l.next();
			l.reset_token_start();
			break;
		}

		if new_lines > 0 {
			l.add_fodder(FodderKind::LineEnd, new_lines - 1, indent, Vec::new());
		}
		// Whitespace is not part of the token that follows it.
		l.reset_token_start();

		let Some(r) = l.peek() else {
			break;
		};

		match r {
			'{' => {
				l.next();
				l.emit_token(TokenKind::BraceL);
			}
			'}' => {
				l.next();
				l.emit_token(TokenKind::BraceR);
			}
			'[' => {
				l.next();
				l.emit_token(TokenKind::BracketL);
			}
			']' => {
				l.next();
				l.emit_token(TokenKind::BracketR);
			}
			',' => {
				l.next();
				l.emit_token(TokenKind::Comma);
			}
			'.' => {
				l.next();
				l.emit_token(TokenKind::Dot);
			}
			'(' => {
				l.next();
				l.emit_token(TokenKind::ParenL);
			}
			')' => {
				l.next();
				l.emit_token(TokenKind::ParenR);
			}
			';' => {
				l.next();
				l.emit_token(TokenKind::Semicolon);
			}

			'0'..='9' => l.lex_number()?,

			'"' => l.lex_quoted_string('"', TokenKind::StringDouble)?,
			'\'' => l.lex_quoted_string('\'', TokenKind::StringSingle)?,
			'@' => l.lex_verbatim_string()?,

			_ => {
				if is_identifier_first(r) {
					l.lex_identifier();
				} else if is_symbol(r) || r == '#' {
					l.lex_symbol()?;
				} else {
					return Err(l.error_here(format!(
						"Could not lex the character {}",
						quote_rune_to_ascii(Some(r))
					)));
				}
			}
		}
	}

	// A token of its own, so that fodder trailing the last real token has
	// somewhere to live.
	l.emit_token(TokenKind::EndOfFile);
	Ok(l.tokens)
}

/// Go's `stripWhitespace`: trim both ends, but only up to `margin` on the left.
///
/// Operates on characters, as Go's does on runes.
fn strip_whitespace(text: &str, margin: usize) -> String {
	if text.is_empty() {
		return String::new();
	}
	let chars: Vec<char> = text.chars().collect();
	let mut start = 0;
	while start < chars.len() && is_horizontal_whitespace(chars[start]) && start < margin {
		start += 1;
	}
	let mut end = chars.len();
	while end > start && is_horizontal_whitespace(chars[end - 1]) {
		end -= 1;
	}
	chars[start..end].iter().collect()
}

/// Go's `lineSplit`: split on newlines and strip each line.
fn line_split(text: &str, margin: usize) -> Vec<String> {
	let mut lines = Vec::new();
	let mut current = String::new();
	for c in text.chars() {
		if c == '\n' {
			lines.push(strip_whitespace(&current, margin));
			current.clear();
		} else {
			current.push(c);
		}
	}
	lines.push(strip_whitespace(&current, margin));
	lines
}

/// Go's `checkWhitespace`: how much of `a`'s leading whitespace `b` also has.
///
/// Returns 0 when `b` fails to match, and also when `a` has no leading
/// whitespace at all — the caller reads 0 as "this line ends the block", which
/// is how a `|||` block knows where it stops.
fn check_whitespace(a: &str, b: &str) -> usize {
	let (a, b) = (a.as_bytes(), b.as_bytes());
	let mut i = 0;
	while i < a.len() {
		if a[i] != b' ' && a[i] != b'\t' {
			// `a` ran out of whitespace with `b` matching all the way.
			return i;
		}
		if i >= b.len() || a[i] != b[i] {
			return 0;
		}
		i += 1;
	}
	i
}

/// Go's `strconv.QuoteRuneToASCII`, which the number errors embed.
fn quote_rune_to_ascii(c: Option<char>) -> String {
	let Some(c) = c else {
		// Go quotes its invalid `lexEOF` rune as the replacement character.
		return "'\\ufffd'".to_owned();
	};
	match c {
		'\'' => "'\\''".to_owned(),
		'\\' => "'\\\\'".to_owned(),
		'\u{07}' => "'\\a'".to_owned(),
		'\u{08}' => "'\\b'".to_owned(),
		'\u{0c}' => "'\\f'".to_owned(),
		'\n' => "'\\n'".to_owned(),
		'\r' => "'\\r'".to_owned(),
		'\t' => "'\\t'".to_owned(),
		'\u{0b}' => "'\\v'".to_owned(),
		c if ('\u{20}'..='\u{7e}').contains(&c) => format!("'{c}'"),
		c if (c as u32) < 0x20 || c as u32 == 0x7f => format!("'\\x{:02x}'", c as u32),
		c if (c as u32) <= 0xffff => format!("'\\u{:04x}'", c as u32),
		c => format!("'\\U{:08x}'", c as u32),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Everything here covers surface the corpus oracle does **not** reach: the
	/// 138 corpus files contain no verbatim strings and provoke no lexer error,
	/// so none of the messages below and neither verbatim path is graded by it.
	///
	/// # Where these expectations came from, and what that cost
	///
	/// Every expectation below was originally derived **by hand** from
	/// go-jsonnet's source rather than from a generator.
	/// `testdata/lexer-snippets.json` now holds the same inputs and
	/// `tests/lexer_oracle.rs` grades them against go-jsonnet's real answers,
	/// which settled the question of how reliable that derivation was:
	/// **14 of 16 were right, and 2 were wrong.**
	///
	/// Both misses were the same mistake — a `LineEnd` the *model* inserts and
	/// the source does not contain. One is `FodderAppend` putting a line end in
	/// front of a paragraph appended to empty fodder; the other is the main
	/// loop's line end for the newline before a trailing comment. Neither is
	/// visible by reading the lexer alone; both are obvious once something
	/// prints the real fodder.
	///
	/// The lesson carries directly: the parser is the same activity at eight
	/// times the volume, and errors of exactly this kind — fodder that is
	/// composed rather than read — are what it will be full of. It needs
	/// node-level fodder dumped from go-jsonnet before expectations are written
	/// by hand, not after.
	///
	/// These are kept as readable documentation of what the lexer does; the
	/// snippet oracle remains the authority.
	fn tokens(input: &str) -> Vec<Token> {
		lex("test", input).expect("lexes cleanly")
	}

	fn message(input: &str) -> String {
		lex("test", input)
			.expect_err("is refused")
			.message()
			.to_owned()
	}

	/// The fodder of the token at `index`, rendered compactly.
	fn fodder_of(input: &str, index: usize) -> Vec<(FodderKind, usize, usize, Vec<String>)> {
		tokens(input)[index]
			.fodder
			.iter()
			.map(|element| {
				(
					element.kind,
					element.blanks,
					element.indent,
					element.comment.clone(),
				)
			})
			.collect()
	}

	#[test]
	fn a_verbatim_string_collapses_its_doubled_quotes() {
		let double = tokens(r#"@"a""b""#);
		assert_eq!(double[0].kind, TokenKind::VerbatimStringDouble);
		assert_eq!(double[0].data, "a\"b");

		let single = tokens("@'a''b'");
		assert_eq!(single[0].kind, TokenKind::VerbatimStringSingle);
		assert_eq!(single[0].data, "a'b");
	}

	#[test]
	fn junk_after_an_at_sign_reports_the_character_as_a_number() {
		// Upstream formats the rune with `%v`, which prints its numeric value.
		// 'x' is 120. Reproduced because the message is part of the contract.
		assert_eq!(
			message("@x"),
			"test:1:1 Couldn't lex verbatim string, junk after '@': 120"
		);
	}

	#[test]
	fn an_unterminated_string_is_reported_at_its_opening_quote() {
		assert_eq!(message("'abc"), "test:1:1 Unterminated String");
		assert_eq!(message("{ a: 'oops }\n"), "test:1:6 Unterminated String");
		assert_eq!(message("@\"abc"), "test:1:1 Unterminated String");
	}

	#[test]
	fn a_character_that_starts_nothing_is_refused() {
		assert_eq!(message("`"), "test:1:1 Could not lex the character '`'");
	}

	#[test]
	fn underscores_separate_digits_and_do_not_survive() {
		// The token's data loses them, so the formatter writes `1000` where the
		// author wrote `1_000`.
		let tokens = tokens("1_000");
		assert_eq!(tokens[0].kind, TokenKind::Number);
		assert_eq!(tokens[0].data, "1000");
	}

	#[test]
	fn the_number_state_machine_reports_each_junk_position() {
		assert_eq!(
			message("0_1"),
			"test:1:2 Couldn't lex number, _ not allowed after leading 0"
		);
		assert_eq!(
			message("1.x"),
			"test:1:3 Couldn't lex number, junk after decimal point: 'x'"
		);
		assert_eq!(
			message("1ex"),
			"test:1:3 Couldn't lex number, junk after 'E': 'x'"
		);
		assert_eq!(
			message("1e+x"),
			"test:1:4 Couldn't lex number, junk after exponent sign: 'x'"
		);
		assert_eq!(
			message("1_x"),
			"test:1:3 Couldn't lex number, junk after '_': 'x'"
		);
	}

	#[test]
	fn an_operator_cannot_end_with_a_trailing_plus() {
		// The wind-back means a run of `+` cannot form one operator. Upstream
		// reads the deciding character once and never reassigns it, so it winds
		// all the way back to a single rune — and the remaining `+`s are lexed
		// as separate tokens on later passes round the main loop.
		let tokens = tokens("+++");
		let operators: Vec<&str> = tokens
			.iter()
			.filter(|token| token.kind == TokenKind::Operator)
			.map(|token| token.data.as_str())
			.collect();
		assert_eq!(operators, vec!["+", "+", "+"]);
	}

	#[test]
	fn an_operator_of_several_symbols_survives_intact() {
		let tokens = tokens("a >= b");
		let operators: Vec<&str> = tokens
			.iter()
			.filter(|token| token.kind == TokenKind::Operator)
			.map(|token| token.data.as_str())
			.collect();
		assert_eq!(operators, vec![">="]);
	}

	#[test]
	fn a_tab_counts_as_eight_spaces_of_indent() {
		// Nothing downstream can recover the tab, which is half of why a
		// tab-indented file does not round-trip.
		assert_eq!(
			fodder_of("a\n\tb", 1),
			vec![(FodderKind::LineEnd, 0, 8, Vec::new())]
		);
	}

	#[test]
	fn a_comment_is_a_paragraph_or_a_line_end_by_what_precedes_it() {
		// First on its line: a paragraph, which carries its own vertical space.
		assert_eq!(
			fodder_of("// c\nx", 0),
			vec![(FodderKind::Paragraph, 0, 0, vec!["// c".to_owned()])]
		);

		// After a token on the same line: a line end carrying the comment.
		assert_eq!(
			fodder_of("x  // c\ny", 1),
			vec![(FodderKind::LineEnd, 0, 0, vec!["// c".to_owned()])]
		);
	}

	#[test]
	fn a_c_comment_is_interstitial_on_one_line_and_a_paragraph_across_two() {
		assert_eq!(
			fodder_of("a /* c */ b", 1),
			vec![(FodderKind::Interstitial, 0, 0, vec!["/* c */".to_owned()])]
		);

		// Note the `LineEnd` the source does not contain. A multi-line C comment
		// goes through `addFodderSafe`, and `FodderAppend` inserts a line end
		// before a paragraph whenever the fodder does not already end cleanly —
		// which includes the fodder being *empty*, since
		// `FodderHasCleanEndline` is false for an empty fodder.
		//
		// So a file beginning with a multi-line C comment has a synthetic line
		// end in front of it, and the unparser would render that as a blank
		// first line. `removeInitialNewlines` is what removes it, which is a
		// large part of why that pass exists at all.
		assert_eq!(
			fodder_of("/* a\n b */\nx", 0),
			vec![
				(FodderKind::LineEnd, 0, 0, Vec::new()),
				(
					FodderKind::Paragraph,
					0,
					0,
					vec!["/* a".to_owned(), " b */".to_owned()]
				)
			]
		);
	}

	#[test]
	fn trailing_whitespace_is_stripped_from_a_comment() {
		assert_eq!(
			fodder_of("// c   \nx", 0),
			vec![(FodderKind::Paragraph, 0, 0, vec!["// c".to_owned()])]
		);
	}

	#[test]
	fn blank_lines_become_a_count() {
		assert_eq!(
			fodder_of("a\n\n\n  b", 1),
			vec![(FodderKind::LineEnd, 2, 2, Vec::new())]
		);
	}

	#[test]
	fn a_text_block_records_both_indents_and_honours_the_chomp() {
		let plain = tokens("|||\n  a\n|||");
		assert_eq!(plain[0].kind, TokenKind::StringBlock);
		assert_eq!(plain[0].data, "a\n");
		assert_eq!(plain[0].string_block_indent, "  ");
		assert_eq!(plain[0].string_block_term_indent, "");

		// `|||-` drops the final newline.
		let chomped = tokens("|||-\n  a\n|||");
		assert_eq!(chomped[0].data, "a");
	}

	#[test]
	fn a_text_block_must_start_with_whitespace_and_be_terminated() {
		assert_eq!(
			message("|||\na\n|||"),
			"test:1:1 Text block's first line must start with whitespace"
		);
		assert_eq!(
			message("||| a\n"),
			"test:1:1 Text block requires new line after |||."
		);
	}

	#[test]
	fn an_unterminated_c_comment_is_reported_at_its_opening() {
		assert_eq!(
			message("/* a"),
			"test:1:1 Multi-line comment has no terminating */"
		);
	}

	#[test]
	fn the_end_of_file_token_carries_the_trailing_fodder() {
		// Final whitespace is discarded, so a file ending in a comment leaves it
		// on the EOF token and nothing else.
		let tokens = tokens("x\n// last\n");
		let last = tokens.last().expect("an EOF token");
		assert_eq!(last.kind, TokenKind::EndOfFile);
		// Two elements, not one: the newline after `x` is its own line end,
		// added by the main loop before the comment is even reached. Only the
		// *final* newline is discarded.
		assert_eq!(
			last.fodder
				.iter()
				.map(|element| (element.kind, element.comment.clone()))
				.collect::<Vec<_>>(),
			vec![
				(FodderKind::LineEnd, Vec::new()),
				(FodderKind::Paragraph, vec!["// last".to_owned()])
			]
		);
	}

	#[test]
	fn a_dollar_is_its_own_kind_rather_than_an_operator() {
		// `$` is both a symbol and in the wind-back set, so it arrives here
		// through the operator path and is then re-labelled.
		let tokens = tokens("$");
		assert_eq!(tokens[0].kind, TokenKind::Dollar);
	}
}
