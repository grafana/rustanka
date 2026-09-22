//! Matching.
//!
//! `gobwas/glob`'s compiler is a large pile of optimisations — `BTree`s
//! anchored on the longest fixed-length matcher, `Row`, `EveryOf`, common-prefix
//! minimisation of alternations — none of which change *what* a pattern
//! matches. What is left when they are stripped away is small:
//!
//! | node     | matches                                              |
//! | -------- | ---------------------------------------------------- |
//! | `Text`   | that literal                                         |
//! | `Any`    | any run of non-separator characters, including none   |
//! | `Super`  | any run of any characters, including none             |
//! | `Single`  | exactly one non-separator character                  |
//! | `List`   | one character in (or, negated, not in) the set        |
//! | `Range`  | one character within (or, negated, outside) the range |
//! | `AnyOf`  | any one of its alternatives                           |
//!
//! Tanka calls `glob.Compile(e)` with **no** separator arguments, so `Any` and
//! `Super` are the same thing there and `*` crosses `/`. That is the whole
//! reason this crate exists instead of `globset`.
//!
//! So the tree is lowered to a small backtracking program. The visited set
//! makes it polynomial rather than exponential, which matters because the same
//! handful of patterns is matched against every file in a repository.

use std::collections::HashSet;

use crate::ast::{Kind, Tree};

/// One instruction. `pc + 1` is the implicit successor of everything except
/// `Jmp`, `Split` and `Match`.
#[derive(Debug, Clone)]
enum Op {
	Text(Vec<char>),
	Any,
	Super,
	Single,
	List {
		chars: Vec<char>,
		not: bool,
	},
	Range {
		lo: char,
		hi: char,
		not: bool,
	},
	/// Try each target.
	Split(Vec<usize>),
	Jmp(usize),
	Match,
}

/// A compiled pattern.
#[derive(Debug, Clone)]
pub struct Program {
	ops: Vec<Op>,
	separators: Vec<char>,
	/// Answered separately; see [`matches_empty_subject`].
	matches_empty: bool,
}

impl Program {
	/// Lower a parsed pattern, holding `separators` for `Any` and `Single`.
	pub fn compile(tree: &Tree, separators: &[char]) -> Self {
		let mut ops = Vec::new();
		emit(tree, Tree::ROOT, &mut ops);
		ops.push(Op::Match);
		Self {
			ops,
			separators: separators.to_vec(),
			matches_empty: matches_empty_subject(tree, Tree::ROOT, separators),
		}
	}

	fn is_separator(&self, c: char) -> bool {
		self.separators.contains(&c)
	}

	/// How many characters from `pos` are not separators.
	fn non_separator_run(&self, subject: &[char], pos: usize) -> usize {
		subject[pos..]
			.iter()
			.take_while(|c| !self.is_separator(**c))
			.count()
	}

	/// Whether the whole of `subject` is matched.
	pub fn is_match(&self, subject: &str) -> bool {
		// Not a shortcut: on an empty subject `gobwas` answers from the shape
		// of the matcher it built rather than from what the pattern means.
		if subject.is_empty() {
			return self.matches_empty;
		}

		let subject: Vec<char> = subject.chars().collect();
		let mut stack = vec![(0_usize, 0_usize)];
		let mut seen: HashSet<(usize, usize)> = HashSet::new();

		while let Some((pc, pos)) = stack.pop() {
			if !seen.insert((pc, pos)) {
				continue;
			}
			match &self.ops[pc] {
				Op::Match => {
					if pos == subject.len() {
						return true;
					}
				}
				Op::Jmp(target) => stack.push((*target, pos)),
				Op::Split(targets) => {
					for target in targets {
						stack.push((*target, pos));
					}
				}
				Op::Text(text) => {
					if subject[pos..].starts_with(text) {
						stack.push((pc + 1, pos + text.len()));
					}
				}
				Op::Single => {
					if pos < subject.len() && !self.is_separator(subject[pos]) {
						stack.push((pc + 1, pos + 1));
					}
				}
				Op::List { chars, not } => {
					if pos < subject.len() && (chars.contains(&subject[pos]) != *not) {
						stack.push((pc + 1, pos + 1));
					}
				}
				Op::Range { lo, hi, not } => {
					if pos < subject.len() && ((*lo..=*hi).contains(&subject[pos]) != *not) {
						stack.push((pc + 1, pos + 1));
					}
				}
				Op::Any => {
					for taken in 0..=self.non_separator_run(&subject, pos) {
						stack.push((pc + 1, pos + taken));
					}
				}
				Op::Super => {
					for taken in 0..=(subject.len() - pos) {
						stack.push((pc + 1, pos + taken));
					}
				}
			}
		}

		false
	}
}

/// `gobwas`' separator set for a `Super`.
const NO_SEPARATORS: &[char] = &[];

/// Whether `gobwas` would match the empty string.
///
/// This cannot be answered by running the program, because on an empty subject
/// `gobwas` answers from the shape of the matcher its optimiser happened to
/// build rather than from what the pattern means. Two upstream quirks are at
/// work, and neither is observable on any other subject:
///
/// - `Single`, `List` and `Range` decode a rune with
///   `utf8.DecodeRuneInString`, which yields `(RuneError, 0)` for `""`. Their
///   guard is `if len(s) > w`, and `0 > 0` is false, so they fall through and
///   test **U+FFFD** for membership. That is why `?` matches the empty string,
///   and why `[!abc]` does while `[abc]` does not.
/// - `BTree.Match` loops `for offset < limit`, both zero on empty input, so the
///   body never runs and it returns false. A composite pattern can never match
///   `""` however zero-width its parts are.
///
/// So the empty string is matched only where the compiler reduced the pattern
/// to a single matcher — `compileMatchers` returns `matchers[0]` for a
/// one-element list — or where `glueMatchersAsEvery` collapsed a run of `Any`
/// and `Super` sharing one separator set into a single `Any` or `Super`.
///
/// `FindFiles` never matches an empty path and `tk` never passes separators, so
/// none of this is reachable through `rtk fmt` or `rtk lint`. It is reproduced
/// because the truth table says so, and it is the truth table that decides
/// whether this crate is a port or an approximation.
fn matches_empty_subject(tree: &Tree, node: usize, separators: &[char]) -> bool {
	let node = &tree.nodes[node];
	match &node.kind {
		Kind::Pattern => match node.children.as_slice() {
			// An empty pattern compiles to `match.Nothing`, which matches only
			// the empty string.
			[] => true,
			[only] => matches_empty_subject(tree, *only, separators),
			children => {
				let mut sets = children
					.iter()
					.map(|child| gluable_separators(tree, *child, separators));
				match sets.next().flatten() {
					None => false,
					Some(first) => sets.all(|set| set == Some(first)),
				}
			}
		},
		Kind::AnyOf => node
			.children
			.iter()
			.any(|child| matches_empty_subject(tree, *child, separators)),
		Kind::Text(text) => text.is_empty(),
		// `Any.Match` asks whether the subject holds a separator, and `""`
		// holds none; `Super.Match` is unconditionally true.
		Kind::Any | Kind::Super => true,
		Kind::Single => !separators.contains(&char::REPLACEMENT_CHARACTER),
		Kind::List { chars, not } => chars.contains(char::REPLACEMENT_CHARACTER) == !*not,
		Kind::Range { lo, hi, not } => (*lo..=*hi).contains(&char::REPLACEMENT_CHARACTER) == !*not,
	}
}

/// The separator set `glueMatchersAsEvery` would compare for this node, or
/// `None` where the node cannot take part in a collapse that still matches the
/// empty string.
///
/// `Single` and a negated `List` are in `gobwas`' switch too, but they set
/// `min > 0`, so the `EveryOf` it builds carries a `Min` that rejects `""`.
/// Treating them as un-gluable here gives the same answer.
fn gluable_separators<'a>(tree: &Tree, node: usize, separators: &'a [char]) -> Option<&'a [char]> {
	match tree.nodes[node].kind {
		Kind::Super => Some(NO_SEPARATORS),
		Kind::Any => Some(separators),
		_ => None,
	}
}

/// Emit the code for `node`, in order, into `ops`.
fn emit(tree: &Tree, node: usize, ops: &mut Vec<Op>) {
	let node = &tree.nodes[node];
	match &node.kind {
		// A pattern is the concatenation of its children. An empty one matches
		// only the empty string, which is what emitting nothing means, and is
		// also what `gobwas` compiles it to (`match.NewNothing`).
		Kind::Pattern => {
			for child in &node.children {
				emit(tree, *child, ops);
			}
		}
		Kind::Text(text) => ops.push(Op::Text(text.chars().collect())),
		Kind::Any => ops.push(Op::Any),
		Kind::Super => ops.push(Op::Super),
		Kind::Single => ops.push(Op::Single),
		Kind::List { chars, not } => ops.push(Op::List {
			chars: chars.chars().collect(),
			not: *not,
		}),
		Kind::Range { lo, hi, not } => ops.push(Op::Range {
			lo: *lo,
			hi: *hi,
			not: *not,
		}),
		Kind::AnyOf => emit_any_of(tree, &node.children, ops),
	}
}

/// `Split` to each alternative; each alternative jumps to the join point.
fn emit_any_of(tree: &Tree, alternatives: &[usize], ops: &mut Vec<Op>) {
	if alternatives.is_empty() {
		return;
	}

	let split = ops.len();
	ops.push(Op::Split(Vec::new()));

	let mut targets = Vec::with_capacity(alternatives.len());
	let mut jumps = Vec::with_capacity(alternatives.len());
	for alternative in alternatives {
		targets.push(ops.len());
		emit(tree, *alternative, ops);
		jumps.push(ops.len());
		ops.push(Op::Jmp(0));
	}

	let join = ops.len();
	for jump in jumps {
		ops[jump] = Op::Jmp(join);
	}
	ops[split] = Op::Split(targets);
}
