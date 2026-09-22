//! A port of `gobwas/glob` v0.2.3 `syntax/ast`.
//!
//! Go threads a mutable tree with parent pointers through a state machine of
//! `parseFn`s. The tree lives in an arena here and the state machine is the
//! same one; `Separator` and `TermsClose` both navigate by parent, which is why
//! the parent link is kept rather than building the tree bottom-up.

use crate::lexer::{Lexer, Token, TokenType};

/// `gobwas/glob`'s `ast.Kind`, with `ast.List` / `ast.Range` / `ast.Text`
/// folded into the variants that carry them.
///
/// `KindNothing` is absent: only `gobwas`' alternation minimiser produces it,
/// and that is an optimisation this port does not need (see `program`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
	Pattern,
	List { chars: String, not: bool },
	Range { lo: char, hi: char, not: bool },
	Text(String),
	Any,
	Super,
	Single,
	AnyOf,
}

/// One arena node. `children` are indices into the arena.
#[derive(Debug, Clone)]
pub struct Node {
	pub kind: Kind,
	pub children: Vec<usize>,
	pub parent: Option<usize>,
}

/// The parsed pattern. Index 0 is always the root `Pattern`.
#[derive(Debug, Clone)]
pub struct Tree {
	pub nodes: Vec<Node>,
}

impl Tree {
	fn new() -> Self {
		Self {
			nodes: vec![Node {
				kind: Kind::Pattern,
				children: Vec::new(),
				parent: None,
			}],
		}
	}

	pub const ROOT: usize = 0;

	fn push(&mut self, kind: Kind) -> usize {
		self.nodes.push(Node {
			kind,
			children: Vec::new(),
			parent: None,
		});
		self.nodes.len() - 1
	}

	/// Go's `ast.Insert`.
	fn insert(&mut self, parent: usize, child: usize) {
		self.nodes[parent].children.push(child);
		self.nodes[child].parent = Some(parent);
	}

	/// Append a fresh node of `kind` under `parent` and return its index.
	fn insert_new(&mut self, parent: usize, kind: Kind) -> usize {
		let child = self.push(kind);
		self.insert(parent, child);
		child
	}

	fn parent_of(&self, node: usize) -> Result<usize, String> {
		// Go would panic on a nil parent here. Every pattern that reaches it
		// has a parent, because `Separator` and `TermsClose` are only lexed
		// inside a `{`, which is what created the parent.
		self.nodes[node]
			.parent
			.ok_or_else(|| "unexpected token: terms close outside of terms".to_string())
	}
}

/// Which `parseFn` runs next.
enum State {
	Main,
	Range,
	Done,
}

/// Go's `ast.Parse` driven by `syntax.Parse`.
pub fn parse(pattern: &str) -> Result<Tree, String> {
	let mut lexer = Lexer::new(pattern);
	let mut tree = Tree::new();
	let mut current = Tree::ROOT;
	let mut state = State::Main;

	loop {
		match state {
			State::Done => return Ok(tree),
			State::Main => {
				let (next_state, next_current) = parser_main(&mut tree, &mut lexer, current)?;
				state = next_state;
				current = next_current;
			}
			State::Range => {
				parser_range(&mut tree, &mut lexer, current)?;
				state = State::Main;
			}
		}
	}
}

/// Go's `parserMain`. Go returns after a single token so the driver can loop;
/// that shape is kept so the control flow lines up with the original.
fn parser_main(
	tree: &mut Tree,
	lexer: &mut Lexer<'_>,
	current: usize,
) -> Result<(State, usize), String> {
	let token = lexer.next_token();
	match token.ty {
		TokenType::Eof => Ok((State::Done, current)),
		TokenType::Error => Err(token.raw),
		TokenType::Text => {
			tree.insert_new(current, Kind::Text(token.raw));
			Ok((State::Main, current))
		}
		TokenType::Any => {
			tree.insert_new(current, Kind::Any);
			Ok((State::Main, current))
		}
		TokenType::Super => {
			tree.insert_new(current, Kind::Super);
			Ok((State::Main, current))
		}
		TokenType::Single => {
			tree.insert_new(current, Kind::Single);
			Ok((State::Main, current))
		}
		TokenType::RangeOpen => Ok((State::Range, current)),
		TokenType::TermsOpen => {
			let any_of = tree.insert_new(current, Kind::AnyOf);
			let pattern = tree.insert_new(any_of, Kind::Pattern);
			Ok((State::Main, pattern))
		}
		TokenType::Separator => {
			let any_of = tree.parent_of(current)?;
			let pattern = tree.insert_new(any_of, Kind::Pattern);
			Ok((State::Main, pattern))
		}
		TokenType::TermsClose => {
			let any_of = tree.parent_of(current)?;
			let outer = tree.parent_of(any_of)?;
			Ok((State::Main, outer))
		}
		_ => Err(format!("unexpected token: {}", token.display())),
	}
}

/// Go's `parserRange`.
fn parser_range(tree: &mut Tree, lexer: &mut Lexer<'_>, current: usize) -> Result<(), String> {
	let mut not = false;
	let mut lo: Option<char> = None;
	let mut hi: Option<char> = None;
	let mut chars = String::new();

	loop {
		let token = lexer.next_token();
		match token.ty {
			TokenType::Eof => return Err("unexpected end".to_string()),
			TokenType::Error => return Err(token.raw),
			TokenType::Not => not = true,
			TokenType::RangeLo => lo = Some(single_rune(&token, "lo")?),
			TokenType::RangeHi => {
				let h = single_rune(&token, "lo")?;
				hi = Some(h);
				// Go compares against a zero `lo` when the pattern gave none,
				// which makes every `hi` greater; `None` reproduces that by
				// skipping the check.
				if let Some(l) = lo.filter(|l| h < *l) {
					return Err(format!(
						"hi character '{h}' should be greater than lo '{l}'"
					));
				}
			}
			TokenType::Text => chars = token.raw,
			TokenType::RangeClose => {
				let is_range = lo.is_some() && hi.is_some();
				let is_chars = !chars.is_empty();

				if is_chars == is_range {
					return Err("could not parse range".to_string());
				}

				let kind = if is_range {
					Kind::Range {
						lo: lo.expect("is_range implies lo"),
						hi: hi.expect("is_range implies hi"),
						not,
					}
				} else {
					Kind::List { chars, not }
				};
				tree.insert_new(current, kind);
				return Ok(());
			}
			// `RangeBetween` lands here: Go has an empty case for it, since the
			// `-` carries no information the `RangeLo`/`RangeHi` pair does not.
			// Anything else the lexer could hand a range is likewise ignored
			// and the loop reads on, as Go's switch does.
			_ => {}
		}
	}
}

/// Go decodes one rune and rejects a longer `Raw`. The message it prints says
/// `lo` for both ends; that typo is upstream's and is reproduced.
fn single_rune(token: &Token, which: &str) -> Result<char, String> {
	let mut chars = token.raw.chars();
	let first = chars
		.next()
		.ok_or_else(|| format!("unexpected length of {which} character"))?;
	if chars.next().is_some() {
		return Err(format!("unexpected length of {which} character"));
	}
	Ok(first)
}
