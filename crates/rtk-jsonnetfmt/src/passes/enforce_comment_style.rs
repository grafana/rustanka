//! A port of `internal/formatter/enforce_comment_style.go`.
//!
//! A single-line comment gets the configured marker — `//` under
//! `Options::default` — by replacing the marker and keeping everything after
//! it. The rewrite is textual, not semantic, so `#c`
//! becomes `//c` with no space invented, and `## c` becomes `//# c` because
//! only the first character goes.
//!
//! # Only a one-line comment is touched
//!
//! The rewrite is inside `len(element.Comment) == 1`. That excludes every
//! multi-line `/* … */`, whose fodder element carries one entry per line — so
//! a paragraph comment is never restyled, in either direction. Interstitials
//! are excluded a step earlier, by kind. Between them that leaves exactly the
//! `#` and `//` comments, which is why `CommentStyle::Slash` only ever has a
//! `#` to convert.
//!
//! # The hashbang carve-out, and the oddity in it
//!
//! A `#!` comment in the file's first fodder is left alone, so a script's
//! interpreter line survives `tk fmt`. The guard is
//! `!seenFirstFodder && len(*comment) > 1 && (*comment)[1] == '!'`, and it
//! **`return`s before setting `seenFirstFodder`**. So the flag never becomes
//! true on a spared hashbang, and a *second* `#!` is still "first" and is also
//! spared. Reproduced, not fixed; the snippet
//! `comment_style/hashbang_twice` pins it.
//!
//! What sets the flag is any non-interstitial element, whether or not
//! anything about it was rewritten — the assignment sits outside the
//! `len == 1` test. Three consequences, each with a snippet:
//!
//! - A leading **blank line** is a commentless `LineEnd`, so one newline at
//!   the top of a file disables the carve-out for the `#!` under it.
//! - An **interstitial** does not set it, so `/* c */ #!b` on one line still
//!   spares the hashbang. (With a newline between them the lexer emits its
//!   own `LineEnd`, and that does set it.)
//! - A comment **already in the target style** sets it without being
//!   rewritten, so `// a` above a `#!b` costs the hashbang its exemption.
//!
//! # Where a comment is out of reach
//!
//! This pass only ever sees the fodder `pass::base` walks, and four slots are
//! never walked (see [`crate::pass`]). Two of them can hold a `#` comment, so
//! `tk fmt` leaves it as written: `{ a: 'b' # c` … `in super }`, whose comment
//! is `InSuper`'s `in_fodder`, and `a. # c` … `b`, whose comment is `Index`'s
//! `right_bracket_fodder` doubling as the fodder before an identifier. The
//! same comment after `super.` *is* rewritten, because `base::super_index`
//! visits `id_fodder` unconditionally — which is what makes the `Index` case a
//! hole rather than a rule.
//!
//! # What grades this pass
//!
//! `testdata/pass-snippets.json`, and almost nothing else. The 138-file corpus
//! changes **zero** cells for this pass, because a `#` comment in a file that
//! is already `tk fmt`-clean has by definition already been rewritten. The one
//! authoritative `tk fmt` answer outside the snippets is go-jsonnet's own
//! `formatter/testdata/empty_comment.fmt.golden`, which is `#` above `{}`
//! formatting to `//` — graded as the `go_jsonnet/empty_comment` fixture when
//! `GO_JSONNET_FOR_TESTS` is set, and it confirms the bare-hash case where
//! `len(*comment) > 1` fails and the hashbang guard is never consulted.

use crate::{
	CommentStyle,
	fodder::{FodderElement, FodderKind},
	pass::AstPass,
};

/// `formatter.EnforceCommentStyle`.
///
/// Deliberately **not** `Copy`. The pass carries the `seenFirstFodder` flag
/// across the whole traversal, and a copy would silently restart the hashbang
/// carve-out part way through a file. Construct one per file, as
/// `FormatNode` does.
#[derive(Debug, Clone)]
pub struct EnforceCommentStyle {
	style: CommentStyle,
	/// Upstream's `seenFirstFodder`. Set by any non-interstitial element that
	/// is not a spared hashbang — see the module documentation.
	seen_first_fodder: bool,
}

impl EnforceCommentStyle {
	pub fn new(style: CommentStyle) -> Self {
		Self {
			style,
			seen_first_fodder: false,
		}
	}
}

impl AstPass for EnforceCommentStyle {
	type Ctx = ();

	fn base_context(&mut self) {}

	fn fodder_element(&mut self, element: &mut FodderElement, _ctx: &()) {
		if element.kind == FodderKind::Interstitial {
			// Note this leaves `seen_first_fodder` alone, which is upstream's
			// bracketing and is load-bearing: an interstitial does not count
			// as fodder seen.
			return;
		}

		// `if let [comment]` is Go's `len(element.Comment) == 1`.
		if let [comment] = element.comment.as_mut_slice() {
			// Go indexes the bytes — `(*comment)[0]` — and would panic on an
			// empty comment. The lexer cannot produce one (a comment is at
			// least its marker), so this is the same behaviour for every tree
			// there is, without the panic.
			if self.style == CommentStyle::Hash && comment.starts_with('/') {
				// `[2..]` is safe for the same reason `[2:]` is in Go: a
				// one-line comment starting with `/` starts with `//`, because
				// a single-line `/* … */` is an interstitial and a multi-line
				// one has more than one entry.
				let rewritten = format!("#{}", &comment[2..]);
				*comment = rewritten;
			}
			if self.style == CommentStyle::Slash && comment.starts_with('#') {
				if !self.seen_first_fodder && comment.as_bytes().get(1) == Some(&b'!') {
					// THE ODDITY: returning here skips the assignment below,
					// so a spared hashbang leaves the flag false and the next
					// one is spared too.
					return;
				}
				let rewritten = format!("//{}", &comment[1..]);
				*comment = rewritten;
			}
		}

		self.seen_first_fodder = true;
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Options, StringStyle, format, parser::snippet_to_raw_ast, pass::visit_file};

	/// Collects every comment the traversal reaches, in visiting order.
	///
	/// The assertions are about the comments rather than about the formatted
	/// text, because the layout also depends on `FixIndentation`,
	/// `FixNewlines` and `removeInitialNewlines`, none of which exists yet.
	/// This asks only what this pass did.
	#[derive(Debug, Default)]
	struct Comments(Vec<String>);

	impl AstPass for Comments {
		type Ctx = ();

		fn base_context(&mut self) {}

		fn fodder_element(&mut self, element: &mut FodderElement, _ctx: &()) {
			self.0.extend(element.comment.iter().cloned());
		}
	}

	/// Every comment in `input`, after this pass has run over it at `style`.
	fn comments_at(style: CommentStyle, input: &str) -> Vec<String> {
		let (mut node, mut final_fodder) =
			snippet_to_raw_ast("t.jsonnet", input).expect("the snippet parses");
		visit_file(
			&mut EnforceCommentStyle::new(style),
			&mut node,
			&mut final_fodder,
		);
		let mut collector = Comments::default();
		visit_file(&mut collector, &mut node, &mut final_fodder);
		collector.0
	}

	fn comments(input: &str) -> Vec<String> {
		comments_at(CommentStyle::Slash, input)
	}

	/// The whole pipeline, for the cases the traversal cannot reach — the
	/// collector above has the same four holes, so asking it would be
	/// circular.
	fn formatted(input: &str) -> String {
		let options = Options {
			string_style: StringStyle::Leave,
			..Options::default()
		};
		format("t.jsonnet", input, &options).expect("the snippet parses")
	}

	#[test]
	fn a_hash_comment_becomes_a_slash_comment() {
		assert_eq!(comments("# c\n1"), ["// c"]);
		// Textual, so no space is invented.
		assert_eq!(comments("#c\n1"), ["//c"]);
		// Only the first character is replaced.
		assert_eq!(comments("## c\n1"), ["//# c"]);
		// A bare hash: `len > 1` fails, so the hashbang guard is not even
		// consulted.
		assert_eq!(comments("#\n1"), ["//"]);
		// Two elements, each with one comment.
		assert_eq!(comments("# a\n# b\n1"), ["// a", "// b"]);
	}

	#[test]
	fn a_slash_comment_is_already_right() {
		assert_eq!(comments("// c\n1"), ["// c"]);
	}

	#[test]
	fn a_hashbang_in_the_first_fodder_is_left_alone() {
		assert_eq!(
			comments("#!/usr/bin/env jsonnet\n1"),
			["#!/usr/bin/env jsonnet"]
		);
		// The shortest comment the guard can fire on.
		assert_eq!(comments("#!\n1"), ["#!"]);
		// Not a hashbang: the guard reads byte 1, which is a space here.
		assert_eq!(comments("# !c\n1"), ["// !c"]);
	}

	#[test]
	fn a_spared_hashbang_does_not_count_as_fodder_seen() {
		// The oddity: the guard returns before setting the flag, so the
		// second `#!` is still "first".
		assert_eq!(comments("#!a\n#!b\n1"), ["#!a", "#!b"]);
	}

	#[test]
	fn anything_non_interstitial_costs_the_hashbang_its_exemption() {
		// An ordinary comment sets the flag.
		assert_eq!(comments("# a\n#!b\n1"), ["// a", "//!b"]);
		// So does one that was already in the target style and so was not
		// rewritten at all.
		assert_eq!(comments("// a\n#!b\n1"), ["// a", "//!b"]);
		// And so does a commentless line end, which is what a leading blank
		// line is: the flag is assigned outside the `len == 1` test, so one
		// newline at the top of a file costs the hashbang its exemption.
		assert_eq!(comments("\n#!a\n1"), ["//!a"]);
		// A multi-line C comment is not rewritten — `len(Comment)` is 2 — but
		// `addFodderSafe` puts a synthetic line end in front of a paragraph
		// appended to empty fodder, and *that* sets the flag.
		assert_eq!(
			comments("/* a\n   b */\n#!c\n1"),
			["/* a", "   b */", "//!c"]
		);
	}

	#[test]
	fn an_interstitial_does_not_count_as_fodder_seen() {
		// On the same line, so the lexer emits no line end between them and
		// the interstitial is the only thing the hashbang follows.
		assert_eq!(comments("/* c */ #!b\n1"), ["/* c */", "#!b"]);
	}

	#[test]
	fn a_c_comment_is_never_restyled() {
		// An interstitial is excluded by kind, before the marker is examined.
		assert_eq!(comments("local x = /* c */ 1; x"), ["/* c */"]);
		// And a paragraph by `len(Comment) == 1`.
		assert_eq!(comments("/* a\n   b */\n1"), ["/* a", "   b */"]);
	}

	#[test]
	fn a_comment_deep_in_the_file_is_reached() {
		assert_eq!(comments("{\n  a: 1,  # c\n  b: 2,\n}"), ["// c"]);
	}

	#[test]
	fn the_final_fodder_is_reached() {
		// `base::file` visits it after the root node.
		assert_eq!(comments("1\n# c\n"), ["// c"]);
	}

	#[test]
	fn a_parameters_eq_fodder_is_visited_where_there_is_a_default() {
		assert_eq!(comments("function(x  # c\n  = 1) x"), ["// c"]);
	}

	#[test]
	fn a_comment_in_an_unvisited_slot_survives() {
		// `InSuper`'s `in_fodder` is never walked, so this `#` is out of the
		// pass's reach — and `tk fmt` leaves it as written.
		assert!(formatted("{ a: 'b' # c\n in super }").contains("# c"));
		// `Index` skips `right_bracket_fodder` when the index is an
		// identifier, and that slot doubles as the fodder before it.
		assert!(formatted("a.  # c\n  b").contains("# c"));
		// But `super_index` visits `id_fodder` either way, which is what
		// makes the two above holes rather than a rule.
		assert!(formatted("{ a: super.  # c\n  b }").contains("// c"));
	}

	#[test]
	fn leave_skips_the_pass_entirely() {
		let options = Options {
			comment_style: CommentStyle::Leave,
			..Options::default()
		};
		assert!(
			format("t.jsonnet", "# c\n1", &options)
				.expect("it parses")
				.contains("# c")
		);
	}

	#[test]
	fn hash_is_the_mirror_of_slash() {
		// NOT GRADED BY THE PASS ORACLE: the dumper runs every pass under
		// `DefaultOptions()`, so only `Slash` is covered there, and `tk fmt`
		// reaches no other value. Read off upstream: the `Hash` branch
		// replaces two characters with one and has no hashbang carve-out at
		// all, since under it a `#!` is already in the target style.
		assert_eq!(comments_at(CommentStyle::Hash, "// c\n1"), ["# c"]);
		assert_eq!(comments_at(CommentStyle::Hash, "//c\n1"), ["#c"]);
		assert_eq!(comments_at(CommentStyle::Hash, "//\n1"), ["#"]);
		assert_eq!(comments_at(CommentStyle::Hash, "# c\n1"), ["# c"]);
		assert_eq!(comments_at(CommentStyle::Hash, "#!a\n1"), ["#!a"]);
	}
}
