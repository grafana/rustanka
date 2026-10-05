//! A port of `gobwas/glob` v0.2.3 `syntax/lexer`.
//!
//! The structure is deliberately the Go one, down to the `read`/`unread`
//! bookkeeping, because the tokenisation of an ill-formed pattern is what
//! decides which error message comes out — and those messages are part of what
//! `tk` prints for a bad `--exclude`.

const CHAR_ANY: char = '*';
const CHAR_COMMA: char = ',';
const CHAR_SINGLE: char = '?';
const CHAR_ESCAPE: char = '\\';
const CHAR_RANGE_OPEN: char = '[';
const CHAR_RANGE_CLOSE: char = ']';
const CHAR_TERMS_OPEN: char = '{';
const CHAR_TERMS_CLOSE: char = '}';
const CHAR_RANGE_NOT: char = '!';
const CHAR_RANGE_BETWEEN: char = '-';

/// The characters `QuoteMeta` escapes; `gobwas`' `syntax.Special`.
const SPECIALS: [char; 7] = [
	CHAR_ANY,
	CHAR_SINGLE,
	CHAR_ESCAPE,
	CHAR_RANGE_OPEN,
	CHAR_RANGE_CLOSE,
	CHAR_TERMS_OPEN,
	CHAR_TERMS_CLOSE,
];

/// Whether `c` is a glob metacharacter.
pub fn special(c: char) -> bool {
	SPECIALS.contains(&c)
}

/// Go's `inTextBreakers`: what ends a run of literal text outside `{ }`.
static IN_TEXT_BREAKERS: [char; 4] = [CHAR_SINGLE, CHAR_ANY, CHAR_RANGE_OPEN, CHAR_TERMS_OPEN];

/// Go's `inTermsBreakers`: the same, plus what ends a term inside `{ }`.
static IN_TERMS_BREAKERS: [char; 6] = [
	CHAR_SINGLE,
	CHAR_ANY,
	CHAR_RANGE_OPEN,
	CHAR_TERMS_OPEN,
	CHAR_TERMS_CLOSE,
	CHAR_COMMA,
];

/// `gobwas/glob`'s `lexer.TokenType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
	Eof,
	Error,
	Text,
	Any,
	Super,
	Single,
	Not,
	Separator,
	RangeOpen,
	RangeClose,
	RangeLo,
	RangeHi,
	RangeBetween,
	TermsOpen,
	TermsClose,
}

impl TokenType {
	/// The name Go's `TokenType.String()` gives this type. Used only to build
	/// the `unexpected token: %s` message verbatim.
	fn name(self) -> &'static str {
		match self {
			Self::Eof => "eof",
			Self::Error => "error",
			Self::Text => "text",
			Self::Any => "any",
			Self::Super => "super",
			Self::Single => "single",
			Self::Not => "not",
			Self::Separator => "separator",
			Self::RangeOpen => "range_open",
			Self::RangeClose => "range_close",
			Self::RangeLo => "range_lo",
			Self::RangeHi => "range_hi",
			Self::RangeBetween => "range_between",
			Self::TermsOpen => "terms_open",
			Self::TermsClose => "terms_close",
		}
	}
}

/// `gobwas/glob`'s `lexer.Token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
	pub ty: TokenType,
	pub raw: String,
}

impl Token {
	fn new(ty: TokenType, raw: impl Into<String>) -> Self {
		Self {
			ty,
			raw: raw.into(),
		}
	}

	/// Go's `Token.String()`: `fmt.Sprintf("%v<%q>", t.Type, t.Raw)`.
	///
	/// Only the ASCII-printable case is reproduced, which is every character
	/// Go's `%q` can be handed here without escaping; anything else is passed
	/// through. The one message that embeds this is
	/// `unexpected token: <...>`, and the parser only ever reaches it for a
	/// token type the main loop does not handle.
	pub fn display(&self) -> String {
		format!("{}<{:?}>", self.ty.name(), self.raw)
	}
}

/// `gobwas/glob`'s `lexer.lexer`.
pub struct Lexer<'a> {
	data: &'a str,
	pos: usize,
	err: Option<String>,

	tokens: Vec<Token>,
	terms_level: usize,

	last_rune: Option<char>,
	last_rune_size: usize,
	has_rune: bool,
}

impl<'a> Lexer<'a> {
	pub fn new(source: &'a str) -> Self {
		Self {
			data: source,
			pos: 0,
			err: None,
			tokens: Vec::with_capacity(4),
			terms_level: 0,
			last_rune: None,
			last_rune_size: 0,
			has_rune: false,
		}
	}

	/// Go's `Next()`. Once the lexer has errored it yields `Error` forever,
	/// exactly as the Go one does.
	pub fn next_token(&mut self) -> Token {
		loop {
			if let Some(err) = &self.err {
				return Token::new(TokenType::Error, err.clone());
			}
			if !self.tokens.is_empty() {
				return self.tokens.remove(0);
			}
			self.fetch_item();
		}
	}

	/// Go's `peek()`. The rune error branch is unreachable: a `&str` is always
	/// valid UTF-8, so `could not read rune` cannot be produced here.
	fn peek(&self) -> (Option<char>, usize) {
		if self.pos == self.data.len() {
			return (None, 0);
		}
		let c = self.data[self.pos..]
			.chars()
			.next()
			.expect("pos is a char boundary before the end of the string");
		(Some(c), c.len_utf8())
	}

	fn read(&mut self) -> Option<char> {
		if self.has_rune {
			self.has_rune = false;
			self.pos += self.last_rune_size;
			return self.last_rune;
		}

		let (r, s) = self.peek();
		self.pos += s;
		self.last_rune = r;
		self.last_rune_size = s;
		r
	}

	/// Go's `unread()`. It errors there when called twice without an
	/// intervening `read`, which no call site in the lexer does, so the check
	/// is a debug assertion rather than an error path.
	fn unread(&mut self) {
		debug_assert!(!self.has_rune, "could not unread rune");
		self.pos -= self.last_rune_size;
		self.has_rune = true;
	}

	fn errorf(&mut self, message: impl Into<String>) {
		if self.err.is_none() {
			self.err = Some(message.into());
		}
	}

	fn in_terms(&self) -> bool {
		self.terms_level > 0
	}

	fn fetch_item(&mut self) {
		let Some(r) = self.read() else {
			self.tokens.push(Token::new(TokenType::Eof, ""));
			return;
		};

		if r == CHAR_TERMS_OPEN {
			self.terms_level += 1;
			self.tokens.push(Token::new(TokenType::TermsOpen, r));
		} else if r == CHAR_COMMA && self.in_terms() {
			self.tokens.push(Token::new(TokenType::Separator, r));
		} else if r == CHAR_TERMS_CLOSE && self.in_terms() {
			self.tokens.push(Token::new(TokenType::TermsClose, r));
			self.terms_level -= 1;
		} else if r == CHAR_RANGE_OPEN {
			self.tokens.push(Token::new(TokenType::RangeOpen, r));
			self.fetch_range();
		} else if r == CHAR_SINGLE {
			self.tokens.push(Token::new(TokenType::Single, r));
		} else if r == CHAR_ANY {
			if self.read() == Some(CHAR_ANY) {
				self.tokens.push(Token::new(TokenType::Super, "**"));
			} else {
				self.unread();
				self.tokens.push(Token::new(TokenType::Any, r));
			}
		} else {
			self.unread();
			let breakers: &[char] = if self.in_terms() {
				&IN_TERMS_BREAKERS
			} else {
				&IN_TEXT_BREAKERS
			};
			self.fetch_text(breakers);
		}
	}

	fn fetch_range(&mut self) {
		let mut want_hi = false;
		let mut want_close = false;
		let mut seen_not = false;
		loop {
			let Some(r) = self.read() else {
				self.errorf("unexpected end of input");
				return;
			};

			if want_close {
				if r == CHAR_RANGE_CLOSE {
					self.tokens.push(Token::new(TokenType::RangeClose, r));
				} else {
					self.errorf("expected close range character");
				}
				return;
			}

			if want_hi {
				self.tokens.push(Token::new(TokenType::RangeHi, r));
				want_close = true;
				continue;
			}

			if !seen_not && r == CHAR_RANGE_NOT {
				self.tokens.push(Token::new(TokenType::Not, r));
				seen_not = true;
				continue;
			}

			let (n, w) = self.peek();
			if n == Some(CHAR_RANGE_BETWEEN) {
				self.pos += w;
				self.tokens.push(Token::new(TokenType::RangeLo, r));
				self.tokens
					.push(Token::new(TokenType::RangeBetween, CHAR_RANGE_BETWEEN));
				want_hi = true;
				continue;
			}

			// Unread the rune read at the top of the loop and take the rest as
			// text, as Go does.
			self.unread();
			self.fetch_text(&[CHAR_RANGE_CLOSE]);
			want_close = true;
		}
	}

	fn fetch_text(&mut self, breakers: &[char]) {
		let mut data = String::new();
		let mut escaped = false;

		while let Some(r) = self.read() {
			if !escaped {
				if r == CHAR_ESCAPE {
					escaped = true;
					continue;
				}
				if breakers.contains(&r) {
					self.unread();
					break;
				}
			}
			escaped = false;
			data.push(r);
		}

		if !data.is_empty() {
			self.tokens.push(Token::new(TokenType::Text, data));
		}
	}
}

/// Go's `glob.QuoteMeta`.
pub fn quote_meta(s: &str) -> String {
	let mut out = String::with_capacity(s.len() * 2);
	for c in s.chars() {
		if special(c) {
			out.push(CHAR_ESCAPE);
		}
		out.push(c);
	}
	out
}
