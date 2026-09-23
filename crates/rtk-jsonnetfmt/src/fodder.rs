//! A port of go-jsonnet's `ast/fodder.go`.
//!
//! Fodder is what the lexer keeps instead of throwing whitespace and comments
//! away. It is the reason `jsonnetfmt` can preserve the author's line structure
//! while normalising everything else, and it is the model every formatter pass
//! manipulates.
//!
//! # It is not a trivia stream
//!
//! This is the thing to understand before porting any pass. Fodder does not
//! record the whitespace it came from; it records a *decision* about vertical
//! space, as a small normalised structure:
//!
//! - blank lines are a **count** ([`FodderElement::blanks`]), not newlines
//! - indentation is a **count of spaces** ([`FodderElement::indent`]), and the
//!   lexer counts a tab as **8**, so a tab-indented file is re-emitted with
//!   spaces
//! - a comment is a list of lines, already stripped of trailing whitespace
//!
//! Each kind carries invariants that [`FodderElement::new`] enforces, and
//! [`Fodder::append`] maintains a further one across elements: a
//! [`FodderKind::LineEnd`] may not follow a `LineEnd` or a
//! [`FodderKind::Paragraph`]. Appending one merges it into its predecessor
//! instead, or promotes it to a paragraph when it carries a comment. Passes
//! rely on that, so fodder must only ever be extended through these methods.
//!
//! # Consequence for the round trip
//!
//! Because the unparser renders text *from* this model rather than copying the
//! source, `unparse(parse(x)) == x` is false in go-jsonnet even with every pass
//! disabled — a tab, a CRLF, trailing whitespace or a single space before a
//! `//` comment all come back different. the fmt port plan asked for that
//! round trip as the Phase 2a gate; the generated corpus replaces it.

/// `ast.FodderKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FodderKind {
	/// A line ending: whatever comes next belongs on a new line.
	///
	/// At most one comment, which flows *before* the newline. This element is
	/// what specifies the indentation and vertical spacing before whatever
	/// follows it.
	LineEnd,
	/// A comment in the middle of a line, which must be `/* C-style */`.
	///
	/// Exactly one comment, no blanks and no indent. Following a token it stays
	/// on that token's line; following a newline or paragraph it is the first
	/// thing on the next line.
	Interstitial,
	/// A comment occupying at least one whole line.
	///
	/// `//` and `#` comments have exactly one line; a C-style comment may have
	/// more. Like `LineEnd`, it specifies the spacing before what follows.
	Paragraph,
}

impl FodderKind {
	/// The name the fodder oracles spell this kind with.
	///
	/// Both Go dumpers write these strings, so they are a contract rather than
	/// a debug convenience.
	pub fn name(self) -> &'static str {
		match self {
			Self::LineEnd => "LineEnd",
			Self::Interstitial => "Interstitial",
			Self::Paragraph => "Paragraph",
		}
	}
}

/// `ast.FodderElement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FodderElement {
	pub kind: FodderKind,
	/// Blank lines before whatever comes next. Always 0 for an interstitial.
	pub blanks: usize,
	/// Spaces of indentation before whatever comes next, with a tab counted as
	/// 8 by the lexer. Always 0 for an interstitial.
	pub indent: usize,
	/// The comment, one entry per line, each already right-trimmed.
	pub comment: Vec<String>,
}

impl FodderElement {
	/// `ast.MakeFodderElement`.
	///
	/// Go panics on each of these, and so does this: they are invariants of the
	/// model, so reaching one means the port has a bug rather than that the
	/// input was bad. The messages are Go's.
	///
	/// # Panics
	///
	/// If the combination of kind, blanks, indent and comment is one the model
	/// does not allow.
	pub fn new(kind: FodderKind, blanks: usize, indent: usize, comment: Vec<String>) -> Self {
		match kind {
			FodderKind::LineEnd => assert!(
				comment.len() <= 1,
				"FodderLineEnd but comment == {comment:?}."
			),
			FodderKind::Interstitial => {
				assert_eq!(blanks, 0, "FodderInterstitial but blanks == {blanks}");
				// Go prints `blanks` in the indent message too; that is a typo
				// upstream, and reproducing it would only mislead a reader of
				// rtk's own panic.
				assert_eq!(indent, 0, "FodderInterstitial but indent == {indent}");
				assert_eq!(
					comment.len(),
					1,
					"FodderInterstitial but comment == {comment:?}."
				);
			}
			FodderKind::Paragraph => {
				assert!(!comment.is_empty(), "FodderParagraph but comment was empty")
			}
		}
		Self {
			kind,
			blanks,
			indent,
			comment,
		}
	}

	/// A `LineEnd` carrying no comment — the common case.
	pub fn line_end(blanks: usize, indent: usize) -> Self {
		Self::new(FodderKind::LineEnd, blanks, indent, Vec::new())
	}

	/// One element in the notation the fodder oracles compare in.
	///
	/// The oracles carry fodder structurally and this notation is applied to
	/// both halves **on this side**, so that no difference between Go's string
	/// escaping and Rust's can be mistaken for a difference in fodder. Two
	/// oracles now share it — the token one and the node one — which is why it
	/// lives here rather than being spelled out in each test.
	pub fn describe(&self) -> String {
		format!(
			"{}(blanks={}, indent={}, comment={:?})",
			self.kind.name(),
			self.blanks,
			self.indent,
			self.comment
		)
	}

	/// `ast.FodderElementCountNewlines`.
	///
	/// A paragraph counts one newline per comment line plus its blanks, which
	/// is why a pass that reasons about vertical space cannot just count
	/// elements.
	pub fn count_newlines(&self) -> usize {
		match self.kind {
			FodderKind::Interstitial => 0,
			FodderKind::LineEnd => 1,
			FodderKind::Paragraph => self.comment.len() + self.blanks,
		}
	}
}

/// `ast.Fodder`: everything the lexer kept before one token.
///
/// A newtype rather than a bare `Vec`, because the cross-element invariant only
/// holds if every extension goes through [`Fodder::append`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fodder {
	elements: Vec<FodderElement>,
}

impl Fodder {
	pub fn new() -> Self {
		Self::default()
	}

	/// Build from elements that are already known to satisfy the invariant.
	///
	/// Used by the lexer port, which constructs fodder in the order Go's does.
	/// Prefer [`Fodder::append`] anywhere the order is not already known good.
	pub fn from_elements(elements: Vec<FodderElement>) -> Self {
		Self { elements }
	}

	/// Append without reconciling the cross-element invariant.
	///
	/// This is Go's `addFodder`, and the distinction from `addFodderSafe` is
	/// upstream's: the lexer uses the unchecked one nearly everywhere and the
	/// safe one only for a multi-line C-style comment. That is sound because of
	/// the order it builds fodder in — `lexWhitespace` consumes a whole run of
	/// whitespace at once, so it cannot produce two adjacent line ends, and a
	/// comment always sits between them.
	///
	/// Outside the lexer, use [`Fodder::append`]. A pass that pushes here is
	/// how a `LineEnd` ends up following a `LineEnd`, which the unparser will
	/// happily render as two newlines that no source had.
	pub fn push_raw(&mut self, element: FodderElement) {
		self.elements.push(element);
	}

	pub fn as_slice(&self) -> &[FodderElement] {
		&self.elements
	}

	/// The elements, owned, consuming the fodder.
	///
	/// The inverse of [`Fodder::from_elements`], and the counterpart to Go's
	/// `for _, elem := range fodder`, which copies each element because
	/// `ast.FodderElement` is a value type. Rust's elements own a
	/// `Vec<String>` of comment lines, so iterating by reference and cloning
	/// would be the same work with an extra allocation per comment; a pass
	/// that is going to redistribute every element should take them instead.
	///
	/// `SortImports`' own `split_after_first_line` — declared on this type in
	/// `src/sort_imports.rs`, because it is `sort_imports.go`'s function — is
	/// the only caller, and it needs exactly this: every element ends up in
	/// one of two new fodders.
	pub fn into_elements(self) -> Vec<FodderElement> {
		self.elements
	}

	pub fn is_empty(&self) -> bool {
		self.elements.is_empty()
	}

	pub fn len(&self) -> usize {
		self.elements.len()
	}

	pub fn first(&self) -> Option<&FodderElement> {
		self.elements.first()
	}

	pub fn last(&self) -> Option<&FodderElement> {
		self.elements.last()
	}

	pub fn last_mut(&mut self) -> Option<&mut FodderElement> {
		self.elements.last_mut()
	}

	pub fn iter(&self) -> std::slice::Iter<'_, FodderElement> {
		self.elements.iter()
	}

	pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, FodderElement> {
		self.elements.iter_mut()
	}

	/// Drop leading elements while `should_drop` says to, then stop.
	///
	/// [`Node::remove_initial_newlines`] is the only caller, and it drops the
	/// leading `LineEnd`s at the top of a file. The run stops at the first
	/// element the predicate refuses, so the *rest* of the fodder is untouched
	/// however many later elements would also match.
	///
	/// [`Node::remove_initial_newlines`]: crate::ast::Node::remove_initial_newlines
	pub fn drop_leading_while(&mut self, mut should_drop: impl FnMut(&FodderElement) -> bool) {
		let mut drop_count = 0;
		while drop_count < self.elements.len() && should_drop(&self.elements[drop_count]) {
			drop_count += 1;
		}
		self.elements.drain(..drop_count);
	}

	/// `formatter.removeExtraTrailingNewlines`: step 14 of `FormatNode`.
	///
	/// A step of the pipeline rather than a pass — Go has it as an unexported
	/// four-line function in `jsonnetfmt.go`, which the staged pass dumper
	/// cannot reach, so it lives on the type it mutates. It zeroes the blanks
	/// on the **last** element of the final fodder and nothing else.
	///
	/// # It can only ever fire on a file that ends in a comment
	///
	/// Which is not what the name suggests, and is worth knowing before
	/// reasoning about the end of a file. The lexer's main loop measures a run
	/// of whitespace and *then* tests for end of input, breaking before it
	/// adds the line end — so trailing newlines never become fodder at all
	/// and there is nothing here to zero. The only way blank lines survive to
	/// the end of a file is on a comment's own element, whose blanks
	/// `lex_until_newline` measures while the run still has a token after it
	/// in prospect. So `1` and four newlines has empty final fodder, while
	/// `1`, `// c` and four newlines has a `LineEnd` and then a `Paragraph`
	/// carrying `blanks = 3` — and that paragraph is what this zeroes.
	///
	/// An interstitial's blanks are already 0 by the model's own invariant, so
	/// a file ending in one is untouched either way.
	///
	/// Go takes the slice by value and assigns through it, so the mutation
	/// reaches the caller's backing array. That is why this is a mutation here
	/// and not a return value.
	pub fn remove_extra_trailing_newlines(&mut self) {
		if let Some(last) = self.last_mut() {
			last.blanks = 0;
		}
	}

	/// `ast.FodderHasCleanEndline`: non-empty and not ending in an
	/// interstitial.
	///
	/// "Clean" means whatever comes next starts on a fresh line, so nothing has
	/// to be separated from it by a space.
	pub fn has_clean_endline(&self) -> bool {
		self.last()
			.is_some_and(|last| last.kind != FodderKind::Interstitial)
	}

	/// `formatter.containsNewline`: whether this fodder breaks the line.
	///
	/// An interstitial is a comment *within* a line, so fodder made only of
	/// interstitials leaves whatever follows on the same line; anything else
	/// moves it to the next one. Upstream declares this beside
	/// `FixTrailingCommas`, the one pass that asks it — a list split over
	/// several lines is exactly a list whose closing bracket, or whose last
	/// comma, has fodder that contains a newline. It is a property of fodder,
	/// so it lives here.
	pub fn contains_newline(&self) -> bool {
		self.iter()
			.any(|element| element.kind != FodderKind::Interstitial)
	}

	/// `ast.FodderAppend`, which is where the cross-element invariant lives.
	///
	/// A `LineEnd` may not follow a `LineEnd` or a `Paragraph`. So appending one
	/// after a clean ending either promotes it to a single-line paragraph, when
	/// it carries a comment, or folds its blanks and indent into the element
	/// already there. And a `Paragraph` appended after an interstitial gets a
	/// `LineEnd` inserted in front of it, because a paragraph has to start on a
	/// line of its own.
	pub fn append(&mut self, element: FodderElement) {
		if self.has_clean_endline() && element.kind == FodderKind::LineEnd {
			if element.comment.is_empty() {
				let back = self
					.elements
					.last_mut()
					.expect("a clean endline implies a last element");
				back.indent = element.indent;
				back.blanks += element.blanks;
			} else {
				self.elements.push(FodderElement::new(
					FodderKind::Paragraph,
					element.blanks,
					element.indent,
					element.comment,
				));
			}
			return;
		}

		if !self.has_clean_endline() && element.kind == FodderKind::Paragraph {
			self.elements
				.push(FodderElement::line_end(0, element.indent));
		}
		self.elements.push(element);
	}

	/// `ast.FodderConcat`: append `other`, keeping the invariant at the seam.
	pub fn concat(mut self, other: Self) -> Self {
		if self.is_empty() {
			return other;
		}
		if other.is_empty() {
			return self;
		}
		let mut rest = other.elements.into_iter();
		// Only the first element of `other` can break the invariant, so only it
		// goes through `append`.
		self.append(rest.next().expect("other is not empty"));
		self.elements.extend(rest);
		self
	}

	/// `ast.FodderMoveFront`: move `other` to the front of `self`, emptying it.
	pub fn move_front(&mut self, other: &mut Self) {
		let moved = std::mem::take(other);
		let mine = std::mem::take(self);
		*self = moved.concat(mine);
	}

	/// `ast.FodderEnsureCleanNewline`.
	pub fn ensure_clean_newline(&mut self) {
		if !self.has_clean_endline() {
			self.append(FodderElement::line_end(0, 0));
		}
	}

	/// Every element in the oracle notation. See [`FodderElement::describe`].
	pub fn describe(&self) -> Vec<String> {
		self.iter().map(FodderElement::describe).collect()
	}

	/// `ast.FodderCountNewlines`.
	pub fn count_newlines(&self) -> usize {
		self.iter().map(FodderElement::count_newlines).sum()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn comment(text: &str) -> Vec<String> {
		vec![text.to_owned()]
	}

	fn interstitial(text: &str) -> FodderElement {
		FodderElement::new(FodderKind::Interstitial, 0, 0, comment(text))
	}

	fn paragraph(lines: &[&str], blanks: usize, indent: usize) -> FodderElement {
		FodderElement::new(
			FodderKind::Paragraph,
			blanks,
			indent,
			lines.iter().map(|line| (*line).to_owned()).collect(),
		)
	}

	#[test]
	fn a_line_end_after_a_clean_ending_merges_rather_than_appending() {
		let mut fodder = Fodder::new();
		fodder.append(FodderElement::line_end(1, 2));
		fodder.append(FodderElement::line_end(3, 4));

		assert_eq!(fodder.len(), 1, "the second was merged into the first");
		let only = fodder.last().expect("one element");
		assert_eq!(only.blanks, 4, "blanks add up");
		assert_eq!(only.indent, 4, "the later indent wins");
	}

	#[test]
	fn a_line_end_with_a_comment_becomes_a_paragraph_instead() {
		let mut fodder = Fodder::new();
		fodder.append(FodderElement::line_end(0, 0));
		fodder.append(FodderElement::new(
			FodderKind::LineEnd,
			1,
			2,
			comment("// hello"),
		));

		assert_eq!(fodder.len(), 2);
		let last = fodder.last().expect("two elements");
		assert_eq!(
			last.kind,
			FodderKind::Paragraph,
			"a comment cannot be merged into a line end, so it is promoted"
		);
		assert_eq!(last.blanks, 1);
		assert_eq!(last.indent, 2);
	}

	#[test]
	fn a_line_end_after_an_interstitial_is_appended_as_itself() {
		let mut fodder = Fodder::new();
		fodder.append(interstitial("/* x */"));
		fodder.append(FodderElement::line_end(0, 4));

		assert_eq!(fodder.len(), 2);
		assert_eq!(fodder.last().expect("two").kind, FodderKind::LineEnd);
	}

	#[test]
	fn a_paragraph_after_an_interstitial_gets_a_line_end_inserted() {
		let mut fodder = Fodder::new();
		fodder.append(interstitial("/* x */"));
		fodder.append(paragraph(&["// y"], 0, 2));

		let kinds: Vec<FodderKind> = fodder.iter().map(|element| element.kind).collect();
		assert_eq!(
			kinds,
			vec![
				FodderKind::Interstitial,
				FodderKind::LineEnd,
				FodderKind::Paragraph
			],
			"a paragraph has to start on a line of its own"
		);
	}

	#[test]
	fn has_clean_endline_is_false_when_empty() {
		assert!(!Fodder::new().has_clean_endline());
	}

	#[test]
	fn concat_only_reconciles_the_seam() {
		let mut left = Fodder::new();
		left.append(FodderElement::line_end(1, 0));
		let mut right = Fodder::new();
		right.append(FodderElement::line_end(1, 4));
		right.append(interstitial("/* x */"));

		let joined = left.concat(right);

		assert_eq!(
			joined.len(),
			2,
			"the two line ends merged; the interstitial came across untouched"
		);
		assert_eq!(joined.first().expect("two").blanks, 2);
		assert_eq!(joined.last().expect("two").kind, FodderKind::Interstitial);
	}

	#[test]
	fn concat_with_an_empty_side_is_the_identity() {
		let mut fodder = Fodder::new();
		fodder.append(interstitial("/* x */"));

		assert_eq!(Fodder::new().concat(fodder.clone()), fodder);
		assert_eq!(fodder.clone().concat(Fodder::new()), fodder);
	}

	#[test]
	fn move_front_empties_the_source() {
		let mut target = Fodder::new();
		target.append(interstitial("/* second */"));
		let mut source = Fodder::new();
		source.append(interstitial("/* first */"));

		target.move_front(&mut source);

		assert!(source.is_empty());
		assert_eq!(target.len(), 2);
		assert_eq!(target.first().expect("two").comment, comment("/* first */"));
	}

	#[test]
	fn ensure_clean_newline_only_acts_when_needed() {
		let mut already_clean = Fodder::new();
		already_clean.append(FodderElement::line_end(0, 0));
		already_clean.ensure_clean_newline();
		assert_eq!(already_clean.len(), 1);

		let mut crowded = Fodder::new();
		crowded.append(interstitial("/* x */"));
		crowded.ensure_clean_newline();
		assert_eq!(crowded.len(), 2);

		// Empty fodder has no clean endline, so it gains one.
		let mut empty = Fodder::new();
		empty.ensure_clean_newline();
		assert_eq!(empty.len(), 1);
	}

	#[test]
	fn newline_counts_follow_the_kind() {
		assert_eq!(interstitial("/* x */").count_newlines(), 0);
		assert_eq!(FodderElement::line_end(3, 0).count_newlines(), 1);
		// Two comment lines plus one blank.
		assert_eq!(paragraph(&["/* a", " b */"], 1, 0).count_newlines(), 3);

		let mut fodder = Fodder::new();
		fodder.append(interstitial("/* x */"));
		fodder.append(FodderElement::line_end(0, 0));
		assert_eq!(fodder.count_newlines(), 1);
	}

	#[test]
	fn drop_leading_while_drops_only_the_leading_run() {
		let mut fodder = Fodder::from_elements(vec![
			FodderElement::line_end(0, 0),
			interstitial("/* x */"),
			FodderElement::line_end(0, 0),
		]);

		fodder.drop_leading_while(|element| element.kind == FodderKind::LineEnd);

		assert_eq!(fodder.len(), 2, "the trailing line end is not touched");
		assert_eq!(fodder.first().expect("two").kind, FodderKind::Interstitial);
	}

	#[test]
	fn remove_extra_trailing_newlines_only_touches_the_last_element() {
		let mut fodder = Fodder::from_elements(vec![
			FodderElement::line_end(3, 0),
			paragraph(&["// c"], 4, 0),
		]);

		fodder.remove_extra_trailing_newlines();

		let blanks: Vec<usize> = fodder.iter().map(|element| element.blanks).collect();
		assert_eq!(
			blanks,
			vec![3, 0],
			"only the last element is zeroed, and a paragraph is zeroed like a line end"
		);
	}

	#[test]
	fn remove_extra_trailing_newlines_on_empty_fodder_does_nothing() {
		// Go's guard is `len(finalFodder) > 0`, and empty final fodder is the
		// common case rather than an edge one: the lexer discards a run of
		// newlines at end of file outright, so every file that does not end
		// in a comment reaches here with nothing at all.
		let mut fodder = Fodder::new();
		fodder.remove_extra_trailing_newlines();
		assert!(fodder.is_empty());
	}

	#[test]
	#[should_panic(expected = "FodderInterstitial but blanks == 1")]
	fn an_interstitial_cannot_carry_blanks() {
		FodderElement::new(FodderKind::Interstitial, 1, 0, comment("/* x */"));
	}

	#[test]
	#[should_panic(expected = "FodderParagraph but comment was empty")]
	fn a_paragraph_needs_a_comment() {
		FodderElement::new(FodderKind::Paragraph, 0, 0, Vec::new());
	}

	#[test]
	#[should_panic(expected = "FodderLineEnd but comment ==")]
	fn a_line_end_cannot_carry_two_comment_lines() {
		FodderElement::new(
			FodderKind::LineEnd,
			0,
			0,
			vec!["// a".to_owned(), "// b".to_owned()],
		);
	}
}
