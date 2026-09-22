//! `formatter.SortImports`: step 1 of `FormatNode`, and the twelfth and last
//! of the passes.
//!
//! A port of `internal/formatter/sort_imports.go`. It sorts the imports at the
//! top of a file into groups by path, where a *top-level import* is a
//! `local x = import '…';` that is either the root of the tree or the body of
//! another top-level import — so, as upstream's own comment puts it, top-level
//! imports are more top-level than top-level functions.
//!
//! # It is unlike every pass before it, in three ways
//!
//! **It is not a visitor.** `SortImports(file *ast.Node)` is a free function
//! that touches nothing in `internal/pass`, so [`crate::pass`] does not help
//! here and this does not live in `src/passes/`. [`crate::fix_indentation`] is
//! the precedent for a step that is not a pass, for a different reason.
//!
//! **It rebuilds the tree rather than rewriting nodes in place.**
//! `Group::build` constructs a fresh chain of [`Local`] nodes from the end
//! backwards, one bind each, so `local a = import 'x', b = import 'y';` comes
//! out as two nested single-bind locals — *whether or not anything was
//! reordered*. The new nodes carry only fodder: Go builds each with
//! `ast.NodeBase{Fodder: fodder}`, so **every rebuilt local loses its
//! location**. Nothing observable depends on that (the formatter reads a
//! location in exactly one place, and discards the error it was for), and it
//! is reproduced rather than improved on.
//!
//! **It runs first**, before
//! [`remove_initial_newlines`](crate::ast::Node::remove_initial_newlines) at
//! step 2, so the fodder it divides has not been truncated yet and a file's
//! leading blank run is still there to be carried into the first group's own
//! fodder.
//!
//! # Where the fodder goes, which is the whole difficulty
//!
//! Each import carries the fodder that *follows* it —
//! `ImportElem::adjacent_fodder` — and `Group::build` puts element
//! `i - 1`'s adjacent fodder in front of element `i`. So a trailing comment
//! travels with the line it was written on:
//!
//! ```text
//! local b = import 'b';  // about b        local a = import 'a';
//! local a = import 'a';              ->    local b = import 'b';  // about b
//! ```
//!
//! and the *last* element's adjacent fodder becomes the fodder before whatever
//! follows the group.
//!
//! # Three panics, all provably unreachable, all reproduced
//!
//! Upstream has three. Each was traced before being ported, the way Phase 2d
//! traced two dead branches and 2e traced `AddPlusObject`'s dead
//! `ast.ApplyBrace` panic, and each is kept so that an upstream change cannot
//! silently diverge here.
//!
//! - **"beforeNext should still be empty."** in
//!   `Fodder::split_after_first_line`. The second half is only ever appended
//!   to once `in_second_part` is already set at the top of the loop, and the
//!   flip happens *after* that append — so at the moment of the flip the
//!   second half is empty by construction.
//! - **"Expected beforeNextFodder to be empty"** in `Group::absorb`. The
//!   second half is non-empty only if the fodder has elements after its first
//!   non-interstitial, or that element carries blanks. Either of those makes
//!   `Node::ends_an_import_group` answer `true`, and this assertion is on
//!   the branch where it answered `false` — over *the same fodder*, since
//!   `groupEndsAfter` reads `openFodder(next)` and `next` is the body that was
//!   split.
//! - **"topLevelImport called with bad local."** All three call sites pass a
//!   local that `Local::is_import_group` has just accepted. In Rust it is
//!   not even expressible: `Group::absorb` takes an owned [`Local`] that the
//!   caller could only have obtained by testing first.
//!
//! The pass oracle confirms it: no cell of either oracle records a panic, over
//! 138 corpus files and 437 snippets.
//!
//! # Graded by
//!
//! 76 `sort_imports/` snippets, 52 of which it changes, against
//! `testdata/pass-snippet-oracle.json`; and one corpus file,
//! `tests/realworld/entry-graalvm.jsonnet`. The split is the usual one and the
//! usual argument for writing the snippets first — before this phase the pass
//! was graded by that single file and by **none** of the 361 snippets then in
//! the set.

use std::collections::BTreeSet;

use crate::{
	ast::{Local, LocalBind, Node, NodeKind},
	fodder::{Fodder, FodderElement, FodderKind},
	go_sort,
	location::LocationRange,
};

impl Fodder {
	/// `splitFodder`: divide this fodder at the end of its first line, leaving
	/// any blank lines *after* that line in the second half.
	///
	/// The heuristic that decides, given two consecutive tokens with fodder
	/// between them, how much of it logically belongs to the token before and
	/// how much to the token after. Upstream's example:
	///
	/// ```text
	/// prev_token // prev_token is awesome!
	///
	/// // blah blah
	/// next_token
	/// ```
	///
	/// `"// prev_token is awesome!"` belongs to `prev_token`; the blank line
	/// and `"// blah blah"` belong to `next_token`.
	///
	/// # It is asymmetric, in two ways that both matter
	///
	/// The first half is built with a plain push and the second through
	/// [`Fodder::append`], so the cross-element invariant is reconciled on the
	/// second half only. That is what puts a **synthetic `LineEnd`** in front
	/// of a `Paragraph` that lands first in an empty second half — the case
	/// `local b = import 'b';` / `// c` / `local a = import 'a';` reaches, and
	/// the same mechanism Phase 2c found behind `EnforceCommentStyle`'s
	/// hashbang flag.
	///
	/// And blanks are *moved* rather than divided: the blanks on the first
	/// half's last element are zeroed and a **fresh** `LineEnd` carrying them
	/// is constructed at the front of the second half. So the two halves
	/// concatenate back to something equivalent to the original, not equal to
	/// it.
	///
	/// This is the category the project has measured itself unreliable at
	/// deriving by hand — fodder the model composes rather than reads, where
	/// 14 of 16 hand-written lexer expectations were right and both misses
	/// were composed. Nothing here was written from a reading of Go; the
	/// answers are the oracle's.
	///
	/// Declared here rather than in [`crate::fodder`] because it is
	/// `sort_imports.go`'s function and this pass is its only caller, but it
	/// is a method because it transforms a `Fodder` and nothing else.
	///
	/// # Panics
	///
	/// Never. See the module documentation for why upstream's
	/// "beforeNext should still be empty." is unreachable.
	fn split_after_first_line(self) -> (Self, Self) {
		let mut after_prev = Self::new();
		let mut before_next = Self::new();
		let mut in_second_part = false;

		for element in self.into_elements() {
			let (kind, blanks, indent) = (element.kind, element.blanks, element.indent);

			if in_second_part {
				before_next.append(element);
			} else {
				// Go's plain `append`. The first half cannot break the
				// invariant, because it is a prefix of fodder that already
				// satisfied it.
				after_prev.push_raw(element);
			}

			if kind != FodderKind::Interstitial && !in_second_part {
				in_second_part = true;
				if blanks > 0 {
					after_prev
						.last_mut()
						.expect("the element was pushed a moment ago")
						.blanks = 0;
					assert!(before_next.is_empty(), "beforeNext should still be empty.");
					before_next.push_raw(FodderElement::line_end(blanks, indent));
				}
			}
		}

		(after_prev, before_next)
	}
}

impl LocalBind {
	/// The path of `import '…'`, where this bind is exactly that and nothing
	/// more.
	///
	/// `isGoodLocal`'s test and the sort key in one method, because they are
	/// the same question: Go asks `bind.Fun == nil` and asserts
	/// `bind.Body.(*ast.Import)` to decide, then reads
	/// `theImport.File.Value` to sort. Answering both at once is what makes
	/// the type assertion in `extractImportElems` unnecessary here.
	///
	/// Three things it deliberately refuses:
	///
	/// - a **function bind**, `local f(x) = import 'a';`, because `Fun` is set
	/// - an **`importstr` or `importbin`**, because Go asserts `*ast.Import`
	///   specifically and those are `*ast.ImportStr` and `*ast.ImportBin`
	/// - anything else, `local n = 1;` included
	///
	/// The innermost `else` is unreachable: `ast.Import.File` is typed
	/// `*ast.LiteralString` upstream, and rtk's parser refuses a computed
	/// import with "Computed imports are not allowed". Reading it as "not an
	/// import bind" gives the same answer for every tree that exists.
	fn import_path(&self) -> Option<&str> {
		if self.fun.is_some() {
			return None;
		}
		let NodeKind::Import(import) = &self.body.kind else {
			return None;
		};
		let NodeKind::LiteralString(literal) = &import.file.kind else {
			return None;
		};
		Some(&literal.value)
	}
}

impl Local {
	/// `isGoodLocal`: whether **every** bind of this local is a plain import.
	///
	/// All of them, which is what makes `local a = import 'x', b = 1;` not a
	/// good local — and so stops the scan dead, whatever follows it.
	fn is_import_group(&self) -> bool {
		self.binds.iter().all(|bind| bind.import_path().is_some())
	}
}

/// One import of a group: its sort key, the bind it came from, and the fodder
/// that follows it.
///
/// Go's `importElem`, and precisely CLAUDE.md's case for a small private state
/// type — three related values threaded together through a recursive traversal
/// and a sort.
struct ImportElem {
	/// The fodder between this import and whatever comes after it, already
	/// through [`Fodder::ensure_clean_newline`]. `Group::build` puts it in
	/// front of the *next* element, so it travels with this one.
	adjacent_fodder: Fodder,
	/// `theImport.File.Value` — the **stored** value of the string literal.
	///
	/// So a path written with an escape sorts by its escaped spelling:
	/// `SortImports` is step 1 and `EnforceStringStyle` is step 11, and a
	/// fully escaped literal keeps its escapes until then. `'a'` sorts as
	/// six characters beginning with a backslash (0x5C), which puts it ahead
	/// of `'_x'` (0x5F) where the unescaped `a` (0x61) would put it behind.
	/// The same seam Phase 2c's non-convergence lives in, and pinned from both
	/// sides by `sort_imports/key_is_the_escaped_value{,_swaps}`.
	key: String,
	bind: LocalBind,
}

impl ImportElem {
	/// `extractImportElems`: one local's binds become one element each.
	///
	/// **`var_fodder` rotates.** The first bind keeps its own; every later
	/// bind takes the *previous* step's split-off second half, while the first
	/// half becomes the previous element's adjacent fodder. So in
	///
	/// ```text
	/// local b = import 'b',
	///       // about a
	///       a = import 'a';
	/// ```
	///
	/// the `LineEnd` stays with `b` and the comment goes with `a` — and once
	/// `a` sorts first, that comment is written between `local` and `a`.
	///
	/// `after` is the fodder the caller split off the local's body, and it is
	/// the last bind's adjacent fodder. It is consumed exactly once, which is
	/// why it is held in an [`Option`]: Go reads the same slice header on the
	/// final iteration and the loop cannot reach that branch twice.
	fn extract(mut binds: Vec<LocalBind>, after: Fodder) -> Vec<Self> {
		// Split every bind's `var_fodder` up front. Go reads `binds[i+1]`
		// while holding `binds[i]`, which owned iteration cannot do — and the
		// split has to see the *original* fodder, not one a previous step
		// rewrote. `splits[i]` is `splitFodder(binds[i].VarFodder)`, except
		// for `splits[0]`, whose second half is the first bind's own fodder
		// kept whole because Go seeds `before` with it rather than splitting
		// it.
		let mut splits: Vec<(Fodder, Fodder)> = Vec::with_capacity(binds.len());
		splits.push((Fodder::new(), std::mem::take(&mut binds[0].var_fodder)));
		for bind in binds.iter_mut().skip(1) {
			splits.push(std::mem::take(&mut bind.var_fodder).split_after_first_line());
		}

		let count = binds.len();
		let mut after = Some(after);
		let mut elems = Vec::with_capacity(count);

		for (at, mut bind) in binds.into_iter().enumerate() {
			let mut adjacent = if at + 1 < count {
				std::mem::take(&mut splits[at + 1].0)
			} else {
				after.take().expect("the last bind is reached exactly once")
			};
			adjacent.ensure_clean_newline();

			bind.var_fodder = std::mem::take(&mut splits[at].1);
			let key = bind
				.import_path()
				.expect("is_import_group has already accepted every bind of this local")
				.to_owned();

			elems.push(Self {
				adjacent_fodder: adjacent,
				key,
				bind,
			});
		}

		elems
	}
}

impl Node {
	/// `groupEndsAfter`, asked of the body the group's last local wraps: does
	/// the import group end before this node?
	///
	/// # The loop is subtler than upstream's doc comment
	///
	/// The comment says groups are separated by blank lines or by lines
	/// containing comments. What the code does is narrower and is what the
	/// snippets pin:
	///
	/// - anything that is **not** a good import local ends the group outright
	/// - an element with `blanks > 0` ends it **immediately**
	/// - otherwise the first non-interstitial element only sets a flag, and it
	///   is the element *after* it that ends the group
	///
	/// So a bare `LineEnd` **continues** the group, which is why consecutive
	/// `local x = import …;` lines are one group; a `LineEnd` followed by a
	/// `Paragraph` ends it, which is how a comment on its own line separates
	/// two groups without a blank line; and a `//` comment trailing a `;` does
	/// *not* separate them, because a comment on a line that is not fresh is a
	/// `LineEnd` **carrying** a comment rather than a paragraph.
	///
	/// Empty fodder ends nothing either — `local b = …;local a = …;` with no
	/// whitespace at all is one group, and the newlines in its output are
	/// invented by [`Fodder::ensure_clean_newline`].
	fn ends_an_import_group(&self) -> bool {
		let NodeKind::Local(local) = &self.kind else {
			return true;
		};
		if !local.is_import_group() {
			return true;
		}

		let mut newline_reached = false;
		for element in self.opening_fodder().iter() {
			if newline_reached || element.blanks > 0 {
				return true;
			}
			if element.kind != FodderKind::Interstitial {
				newline_reached = true;
			}
		}
		false
	}

	/// Whether this node is a `local` every bind of which is a plain import —
	/// `goodLocalOrNull` as a predicate.
	///
	/// A predicate rather than an extractor because owning the [`Local`] means
	/// taking it out of the node, with no way to put it back if the answer is
	/// no. That is §18 of `docs/learning-rust.md`: test the shape before you
	/// own it.
	fn is_import_group_local(&self) -> bool {
		match &self.kind {
			NodeKind::Local(local) => local.is_import_group(),
			_ => false,
		}
	}

	/// Take this node's `Local` payload, leaving a placeholder behind.
	///
	/// # Panics
	///
	/// If the node is not a `Local`. Every caller has just asked
	/// `Node::is_import_group_local`.
	fn take_local(&mut self) -> Local {
		let kind = std::mem::replace(&mut self.kind, NodeKind::LiteralNull);
		match kind {
			NodeKind::Local(local) => local,
			_ => unreachable!("is_import_group_local has already accepted this node"),
		}
	}

	/// `formatter.SortImports`: step 1 of `FormatNode`.
	///
	/// A method on `Node`, as
	/// [`remove_initial_newlines`](Node::remove_initial_newlines) — step 2 —
	/// already is. Both are steps of the pipeline rather than passes, and both
	/// transform the node they are given.
	///
	/// Does nothing at all unless the root is a local whose every bind is a
	/// plain import, so imports anywhere else in a file are never touched.
	pub fn sort_imports(&mut self) {
		if !self.is_import_group_local() {
			return;
		}
		// `*openFodder(local)`, which for a `Local` — never left-recursive —
		// is its own fodder. Cloned because it becomes the rebuilt first
		// local's fodder while this node is being dismantled.
		let group_open_fodder = self.opening_fodder().clone();
		let local = self.take_local();
		*self = Group::new().absorb(local, group_open_fodder);
	}
}

/// One group of imports, as `Group::absorb` accumulates it.
///
/// A newtype over the elements rather than a bare `Vec`, so that the three
/// things a group is asked — sort yourself, are your variables duplicated,
/// rebuild yourself — are methods on the state they operate on. Upstream has
/// them as free functions over a slice, which CLAUDE.md does not allow as a
/// substitute for private methods.
struct Group(Vec<ImportElem>);

impl Group {
	fn new() -> Self {
		Self(Vec::new())
	}

	/// `duplicatedVariables`: whether two binds of this group share a name.
	///
	/// **It keys on the bind variable, not on the import path.** Two imports
	/// of the same file are sorted normally; two bindings of the same *name*
	/// disable sorting for the whole group, not just for the pair.
	///
	/// One route only: shadowing across locals, as in
	/// `local b = import 'b';` / `local b = import 'a';`. Two binds of a
	/// single local cannot share a name, because the parser refuses with
	/// "Duplicate local var" before the formatter ever sees the tree.
	fn has_duplicated_variables(&self) -> bool {
		let names: BTreeSet<&str> = self
			.0
			.iter()
			.map(|elem| elem.bind.variable.as_str())
			.collect();
		names.len() < self.0.len()
	}

	/// `sortGroup`: sort by path, unless a variable is bound twice.
	///
	/// Through [`go_sort::slice`] rather than `sort_by`, because two imports
	/// may share a path and Go's tie order is not Rust's. That module explains
	/// what was measured and why it is 250 lines rather than one.
	fn sort(&mut self) {
		if !self.has_duplicated_variables() {
			go_sort::slice(&mut self.0, |left, right| left.key < right.key);
		}
	}

	/// `buildGroupAST`: a fresh chain of single-bind locals, end first.
	///
	/// Element `i` becomes a `Local` whose own fodder is element `i - 1`'s
	/// adjacent fodder, and element 0's is the group's opening fodder. So the
	/// walk is backwards, and each step needs the element *before* the one it
	/// is consuming.
	///
	/// Popping from the back is what makes that ownership work:
	/// [`Vec::pop`] hands over element `i` outright, and `last_mut` is then
	/// element `i - 1`, whose fodder can be taken because it will be popped
	/// on the very next iteration. No clone, and no placeholder. Reaching for
	/// indices instead means borrowing `self.0` while also moving out of it.
	fn build(mut self, body: Node, group_open_fodder: Fodder) -> Node {
		let mut group_open_fodder = group_open_fodder;
		let mut body = body;

		while let Some(elem) = self.0.pop() {
			let fodder = match self.0.last_mut() {
				Some(previous) => std::mem::take(&mut previous.adjacent_fodder),
				// The last iteration, which is element 0.
				None => std::mem::take(&mut group_open_fodder),
			};
			body = Node::new(
				// Go builds these with `ast.NodeBase{Fodder: fodder}`, so the
				// location is the zero value. See the module documentation.
				LocationRange::default(),
				fodder,
				NodeKind::Local(Local {
					binds: vec![elem.bind],
					body: Box::new(body),
				}),
			);
		}

		body
	}

	/// `topLevelImport`: absorb `local` into this group, and return the tree
	/// that replaces it.
	///
	/// Either the group continues — in which case this recurses on the body
	/// with the *same* group and the same opening fodder — or it ends here, in
	/// which case the group is sorted, whatever follows is processed (a second
	/// group through a **fresh** `Group`, or anything else with its opening
	/// fodder overwritten), and `Group::build` rebuilds the chain.
	///
	/// # Panics
	///
	/// Never; upstream's "Expected beforeNextFodder to be empty" is
	/// unreachable, and the module documentation says why.
	fn absorb(mut self, local: Local, group_open_fodder: Fodder) -> Node {
		let Local { binds, body } = local;
		let mut body = body;

		// The fodder between this local's `;` and its body, divided between
		// the two. Go reads `*openFodder(local.Body)` and leaves it in place;
		// it is overwritten later, on the branch that needs to.
		let (mut adjacent, before_next) = body.opening_fodder().clone().split_after_first_line();
		adjacent.ensure_clean_newline();
		self.0.extend(ImportElem::extract(binds, adjacent));

		if !body.ends_an_import_group() {
			assert!(
				before_next.is_empty(),
				"Expected beforeNextFodder to be empty"
			);
			let next = body.take_local();
			return self.absorb(next, group_open_fodder);
		}

		self.sort();

		// The *sorted* last element's adjacent fodder, which is the fodder
		// before whatever follows the group. Taken rather than cloned: the
		// rebuild uses elements `0..len-1`'s adjacent fodder and never the
		// last one's.
		let after_group = std::mem::take(
			&mut self
				.0
				.last_mut()
				.expect("a group holds at least the bind that started it")
				.adjacent_fodder,
		);
		let mut before_next = before_next;
		before_next.ensure_clean_newline();
		let next_open_fodder = after_group.concat(before_next);

		let body_after_group = if body.is_import_group_local() {
			// Another group of imports: a fresh group, taking the fodder this
			// one ended with as its opening fodder.
			let next = body.take_local();
			Group::new().absorb(next, next_open_fodder)
		} else {
			// Something else. `openFodder`, so on a left-recursive body this
			// writes several levels down the leftmost spine.
			*body.opening_fodder_mut() = next_open_fodder;
			*body
		};

		self.build(body_after_group, group_open_fodder)
	}
}

#[cfg(test)]
mod tests {
	use crate::{Options, format, parser::snippet_to_raw_ast};

	/// Step 1 alone: parse, sort the imports, unparse. No other pass runs.
	///
	/// Not a substitute for the oracle — `tests/pass_parity.rs` grades this
	/// pass node for node and slot for slot on 76 snippets, and hand-written
	/// *answers* are what this project has measured itself unreliable at. What
	/// these are for is the handful of claims worth stating in prose beside
	/// the code, and every one of them is a whole-output claim rather than a
	/// claim about composed fodder.
	///
	/// `finish_file` restores the trailing newline the lexer stripped, so
	/// every answer below ends in one.
	fn sorted(input: &str) -> String {
		let (mut node, final_fodder) =
			snippet_to_raw_ast("t.jsonnet", input).expect("the input parses");
		node.sort_imports();
		let mut unparser = crate::unparse::Unparser::new(Options::default());
		unparser.unparse(&node, false);
		unparser.finish_file(&final_fodder);
		unparser.finish()
	}

	#[test]
	fn a_group_is_sorted_by_path() {
		assert_eq!(
			sorted("local b = import 'b';\nlocal a = import 'a';\n1"),
			"local a = import 'a';\nlocal b = import 'b';\n1\n"
		);
	}

	#[test]
	fn a_blank_line_separates_two_groups() {
		// Nothing crosses the blank line, which is the only reason this is not
		// simply "sort the imports at the top of the file".
		assert_eq!(
			sorted(
				"local d = import 'd';\nlocal c = import 'c';\n\n\
				 local b = import 'b';\nlocal a = import 'a';\n1"
			),
			"local c = import 'c';\nlocal d = import 'd';\n\n\
			 local a = import 'a';\nlocal b = import 'b';\n1\n"
		);
	}

	#[test]
	fn one_local_with_several_binds_is_split_even_when_already_sorted() {
		// The rebuild is unconditional: `buildGroupAST` makes one local per
		// bind whether or not the sort moved anything. So this pass rewrites
		// a file it had no reordering to do in.
		assert_eq!(
			sorted("local a = import 'a', b = import 'b';\n1"),
			"local a = import 'a';\nlocal b = import 'b';\n1\n"
		);
	}

	#[test]
	fn a_duplicated_variable_disables_sorting_for_the_whole_group() {
		// Shadowing is the only way to reach this; the parser refuses two
		// binds of one local sharing a name. Note the chain is still rebuilt
		// — it just comes out identical, which is why the oracle records this
		// snippet as unchanged.
		assert_eq!(
			sorted("local b = import 'b';\nlocal b = import 'a';\n1"),
			"local b = import 'b';\nlocal b = import 'a';\n1\n"
		);
	}

	#[test]
	fn an_importstr_is_not_an_import() {
		// `isGoodLocal` asserts `*ast.Import`, so this root disqualifies the
		// whole file and the import below it is never reached.
		assert_eq!(
			sorted("local a = importstr 'z';\nlocal b = import 'a';\n1"),
			"local a = importstr 'z';\nlocal b = import 'a';\n1\n"
		);
	}

	#[test]
	fn imports_below_a_non_import_local_are_untouched() {
		assert_eq!(
			sorted("local x = 1;\nlocal b = import 'b';\nlocal a = import 'a';\n1"),
			"local x = 1;\nlocal b = import 'b';\nlocal a = import 'a';\n1\n"
		);
	}

	#[test]
	fn the_sort_key_is_the_stored_value_and_not_the_unescaped_one() {
		// A backslash is 0x5C and `_` is 0x5F, so the escaped spelling sorts
		// first; the character it denotes, `a`, is 0x61 and would sort second.
		// `EnforceStringStyle` unescapes it at step 11, long after this
		// decision has been made. Built with `char` rather than written out,
		// so the escape cannot be interpreted on its way into this file.
		let escaped = format!("local a = import '{}u0061';", '\\');
		assert_eq!(
			sorted(&format!("local u = import '_x';\n{escaped}\n1")),
			format!("{escaped}\nlocal u = import '_x';\n1\n")
		);
	}

	#[test]
	fn the_indent_of_a_split_bind_is_merged_away_rather_than_indented_later() {
		// Worth pinning because the plausible guess is wrong. A continuation
		// bind's `var_fodder` is a LineEnd indented to line up under `local`,
		// and splitting moves it to the *previous* element's adjacent fodder —
		// where `FodderConcat` merges it with the body's own LineEnd and
		// `FodderAppend` takes the **later** indent. So the 6 is gone before
		// `FixIndentation` at step 13 is ever asked, and step 1's own output
		// is already at column 0.
		assert_eq!(
			sorted("local b = import 'b',\n      a = import 'a';\n1"),
			"local a = import 'a';\nlocal b = import 'b';\n1\n"
		);
		assert_eq!(
			format(
				"t.jsonnet",
				"local b = import 'b',\n      a = import 'a';\n1",
				&Options::default()
			)
			.expect("it parses"),
			"local a = import 'a';\nlocal b = import 'b';\n1\n",
			"and the whole pipeline agrees, so nothing downstream undoes it"
		);
	}
}
