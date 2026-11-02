//! Token kinds, ported from go-jsonnet's `internal/parser/lexer.go`.
//!
//! The names [`TokenKind::name`] returns are Go's `tokenKindStrings`
//! verbatim — **including the quote characters** on the symbols, since Go
//! writes them as `` `"{"` ``. They reach users through parser messages like
//! `Expected token "}" but got …`, and the lexer oracle records them, so a
//! renaming here is a visible behaviour change.

use crate::{fodder::Fodder, location::LocationRange};

/// `parser.tokenKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
	// Symbols.
	BraceL,
	BraceR,
	BracketL,
	BracketR,
	Comma,
	Dollar,
	Dot,
	ParenL,
	ParenR,
	Semicolon,

	// Arbitrary length lexemes.
	Identifier,
	Number,
	Operator,
	StringBlock,
	StringDouble,
	StringSingle,
	VerbatimStringDouble,
	VerbatimStringSingle,

	// Keywords.
	Assert,
	Else,
	Error,
	False,
	For,
	Function,
	If,
	Import,
	ImportStr,
	ImportBin,
	In,
	Local,
	NullLit,
	/// `self`. Named with a suffix because `Self` is reserved in Rust.
	SelfKw,
	Super,
	TailStrict,
	Then,
	True,

	/// Holds the line and column information about the end of the file, and
	/// any fodder that trailed the last real token.
	EndOfFile,
}

impl TokenKind {
	/// Go's `tokenKind.String()`.
	pub fn name(self) -> &'static str {
		match self {
			Self::BraceL => "\"{\"",
			Self::BraceR => "\"}\"",
			Self::BracketL => "\"[\"",
			Self::BracketR => "\"]\"",
			Self::Comma => "\",\"",
			Self::Dollar => "\"$\"",
			Self::Dot => "\".\"",
			Self::ParenL => "\"(\"",
			Self::ParenR => "\")\"",
			Self::Semicolon => "\";\"",

			Self::Identifier => "IDENTIFIER",
			Self::Number => "NUMBER",
			Self::Operator => "OPERATOR",
			Self::StringBlock => "STRING_BLOCK",
			Self::StringDouble => "STRING_DOUBLE",
			Self::StringSingle => "STRING_SINGLE",
			Self::VerbatimStringDouble => "VERBATIM_STRING_DOUBLE",
			Self::VerbatimStringSingle => "VERBATIM_STRING_SINGLE",

			Self::Assert => "assert",
			Self::Else => "else",
			Self::Error => "error",
			Self::False => "false",
			Self::For => "for",
			Self::Function => "function",
			Self::If => "if",
			Self::Import => "import",
			Self::ImportStr => "importstr",
			Self::ImportBin => "importbin",
			Self::In => "in",
			Self::Local => "local",
			Self::NullLit => "null",
			Self::SelfKw => "self",
			Self::Super => "super",
			Self::TailStrict => "tailstrict",
			Self::Then => "then",
			Self::True => "true",

			Self::EndOfFile => "end of file",
		}
	}

	/// Go's `tokenHasContent`: whether `token.String()` shows the data.
	pub fn has_content(self) -> bool {
		matches!(
			self,
			Self::Identifier
				| Self::Number
				| Self::Operator
				| Self::StringBlock
				| Self::StringDouble
				| Self::StringSingle
				| Self::VerbatimStringDouble
				| Self::VerbatimStringSingle
		)
	}
}

/// `parser.token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
	pub kind: TokenKind,
	/// Whatever fodder occurred *before* this token.
	pub fodder: Fodder,
	/// The token's content, where it has any. A string token's data excludes
	/// its quotes but keeps its escape sequences unprocessed, because the
	/// formatter has to write them back out as they were.
	pub data: String,
	/// The whitespace that indented a `|||` block. Only set on a
	/// [`TokenKind::StringBlock`].
	pub string_block_indent: String,
	/// The whitespace before the closing `|||`, always shorter than
	/// [`Token::string_block_indent`].
	pub string_block_term_indent: String,
	pub location: LocationRange,
}

impl Token {
	/// Go's `token.String()`, used in parser error messages.
	pub fn display(&self) -> String {
		if self.data.is_empty() {
			self.kind.name().to_owned()
		} else if self.kind.has_content() {
			format!("({}, \"{}\")", self.kind.name(), self.data)
		} else {
			format!("\"{}\"", self.data)
		}
	}
}

/// Go's `getTokenKindFromID`: a keyword, or an identifier.
pub fn token_kind_from_id(text: &str) -> TokenKind {
	match text {
		"assert" => TokenKind::Assert,
		"else" => TokenKind::Else,
		"error" => TokenKind::Error,
		"false" => TokenKind::False,
		"for" => TokenKind::For,
		"function" => TokenKind::Function,
		"if" => TokenKind::If,
		"import" => TokenKind::Import,
		"importstr" => TokenKind::ImportStr,
		"importbin" => TokenKind::ImportBin,
		"in" => TokenKind::In,
		"local" => TokenKind::Local,
		"null" => TokenKind::NullLit,
		"self" => TokenKind::SelfKw,
		"super" => TokenKind::Super,
		"tailstrict" => TokenKind::TailStrict,
		"then" => TokenKind::Then,
		"true" => TokenKind::True,
		_ => TokenKind::Identifier,
	}
}

/// Go's `parser.IsValidIdentifier`.
///
/// `PrettyFieldNames` asks this to decide whether a quoted field name can lose
/// its quotes, so it is load-bearing for that pass and not just a helper.
pub fn is_valid_identifier(text: &str) -> bool {
	if text.is_empty() {
		return false;
	}
	for (index, c) in text.char_indices() {
		if index == 0 {
			if !is_identifier_first(c) {
				return false;
			}
		} else if !is_identifier(c) {
			return false;
		}
	}
	token_kind_from_id(text) == TokenKind::Identifier
}

pub fn is_identifier_first(c: char) -> bool {
	c.is_ascii_uppercase() || c.is_ascii_lowercase() || c == '_'
}

pub fn is_identifier(c: char) -> bool {
	is_identifier_first(c) || c.is_ascii_digit()
}

/// Go's `isSymbol`. Note `$` is in here *and* in the operator wind-back list.
pub fn is_symbol(c: char) -> bool {
	matches!(
		c,
		'!' | '$' | ':' | '~' | '+' | '-' | '&' | '|' | '^' | '=' | '<' | '>' | '*' | '/' | '%'
	)
}

pub fn is_horizontal_whitespace(c: char) -> bool {
	c == ' ' || c == '\t' || c == '\r'
}

pub fn is_whitespace(c: char) -> bool {
	c == '\n' || is_horizontal_whitespace(c)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn symbol_names_keep_gos_quotes() {
		// These reach users through parser messages, e.g.
		// `Expected token "}" but got …`.
		assert_eq!(TokenKind::BraceR.name(), "\"}\"");
		assert_eq!(TokenKind::Comma.name(), "\",\"");
		assert_eq!(TokenKind::EndOfFile.name(), "end of file");
		assert_eq!(TokenKind::Identifier.name(), "IDENTIFIER");
	}

	#[test]
	fn keywords_are_not_identifiers() {
		assert_eq!(token_kind_from_id("local"), TokenKind::Local);
		assert_eq!(token_kind_from_id("self"), TokenKind::SelfKw);
		assert_eq!(token_kind_from_id("locally"), TokenKind::Identifier);
	}

	#[test]
	fn valid_identifiers_exclude_keywords_and_leading_digits() {
		assert!(is_valid_identifier("foo"));
		assert!(is_valid_identifier("_foo1"));
		assert!(!is_valid_identifier(""));
		assert!(!is_valid_identifier("1foo"));
		assert!(!is_valid_identifier("foo-bar"));
		// A keyword is a valid *word* but not a valid identifier, which is why
		// `PrettyFieldNames` cannot unquote `'local'` as a field name.
		assert!(!is_valid_identifier("local"));
	}

	#[test]
	fn identifier_characters_are_ascii_only() {
		assert!(is_identifier_first('A'));
		assert!(is_identifier_first('_'));
		assert!(!is_identifier_first('1'));
		assert!(is_identifier('1'));
		// Jsonnet identifiers are ASCII; a letter outside it is not one.
		assert!(!is_identifier_first('é'));
	}

	#[test]
	fn token_display_distinguishes_content_from_symbols() {
		let bare = Token {
			kind: TokenKind::BraceR,
			fodder: Fodder::new(),
			data: String::new(),
			string_block_indent: String::new(),
			string_block_term_indent: String::new(),
			location: LocationRange::point("f", crate::location::Location::default()),
		};
		assert_eq!(bare.display(), "\"}\"");

		let identifier = Token {
			kind: TokenKind::Identifier,
			data: "foo".to_owned(),
			..bare.clone()
		};
		assert_eq!(identifier.display(), "(IDENTIFIER, \"foo\")");

		// A token with data but no content kind prints just the data, which is
		// how an operator reaches `Not a binary operator: …`.
		let operator = Token {
			kind: TokenKind::Comma,
			data: ",".to_owned(),
			..bare
		};
		assert_eq!(operator.display(), "\",\"");
	}
}
