//! Does `format` settle after one run? Measured over the adversarial inputs,
//! with an explicit list of the cases that do not.
//!
//! # Why this is not just `tests/corpus.rs`
//!
//! `corpus.rs`'s `formatting_an_answer_again_changes_nothing` asks the same
//! question over both corpus sets' **answers** — 138 in-repo and 719 from
//! go-jsonnet's own testdata — and its allow-list is empty. That is a weaker
//! statement than it looks: an answer is by construction already a
//! fixed point of go-jsonnet's own `Format`, and the inputs written to be
//! awkward live in `testdata/pass-snippets.json` and
//! `testdata/node-snippets.json` instead. Until this file, **nothing had ever
//! run `format` over any of those**, let alone twice.
//!
//! When it was finally run it moved twelve of them, against a prediction of
//! five. The six extra were all `FixParens`, and they were missed because the
//! prediction was made by grepping the snippets for `((` — which cannot match
//! `(\n  (1)`, the shape that actually matters. The lesson is the project's own
//! and it was relearned the hard way: **do not derive by reading what a run can
//! measure.**
//!
//! # Every case below is upstream's
//!
//! `jsonnetfmt` is not a fixed point, and `rtk fmt` must not become one — see
//! `CLAUDE.md` and `docs/rtk-fmt-plan.md`. Matching `tk fmt` on **one** run is
//! the contract, so a convergence loop would be a divergence, not a fix. Three
//! mechanisms account for all twelve, and each is a seam between a pass that
//! decides something and a later pass that changes what it decided on:
//!
//! | mechanism | steps | cases |
//! | --- | --- | --- |
//! | `PrettyFieldNames` reads a *stored* value `EnforceStringStyle` then unescapes | 10, 11 | 2 |
//! | `FixParens` collapses one level per run, and moves fodder outward into a slot `removeInitialNewlines` already cleaned | 2, 6 | 8 |
//! | `SortImports` keys on a *stored* value `EnforceStringStyle` then unescapes | 1, 11 | 2 |
//!
//! The `FixParens` group is the one to know about, because its first-run output
//! is not merely unsettled — it is visibly malformed, and it is what `tk fmt`
//! prints:
//!
//! ```text
//! (\n  (1)\n)     ->  "\n(1\n)\n"        a leading blank line, body unindented
//! (\n\n  (1)\n)   ->  "\n\n(1\n)\n"      two leading blank lines
//! ( // a\n(1))    ->  "  // a\n(1\n)\n"  the file starts with two spaces
//! ```
//!
//! and `fix_parens/three_opens_with_comments` reorders **comments** across
//! runs rather than whitespace. Expect these to be reported as rtk bugs; they
//! are not.
//!
//! # Two more mechanisms live in `corpus.rs`, in a worse class
//!
//! The three above are the seams the *snippets* reach. `unparseable` below
//! asserts that no snippet's first-run output fails to parse, and none does —
//! but Phase 4's corpus set found that upstream has two mechanisms that do
//! exactly that, so the assertion is a real one rather than a formality:
//!
//! - `PrettyFieldNames` has no `ObjectComp` guard, so `{ ['x']: 1 for x in y }`
//!   becomes `{ x: 1 for x in y }`, which the parser refuses.
//! - a `|||` block of nothing but newlines loses the indent that held it
//!   together, because a blank value-line is written with no indent at all.
//!
//! Neither belongs in the table above: a second run of those does not move the
//! output, it fails, so a convergence loop would not even help. They are pinned
//! by [`OUTPUT_DOES_NOT_REPARSE`] here, by the list of the same name in
//! `tests/corpus.rs`, and by the two `text_block_only_*` lexer snippets. Across
//! the project it is five mechanisms and seventeen inputs, and `CLAUDE.md` has
//! the whole table with the class of each.
//!
//! # The list is a two-way ratchet
//!
//! A name here that now settles fails just as loudly as an unlisted name that
//! moves. The first means upstream changed or the port drifted and the entry
//! should go; the second is a new non-convergence and needs an entry in
//! `CLAUDE.md` before it earns a line here.

use std::{collections::BTreeSet, fmt::Write as _, fs, path::PathBuf};

use rtk_jsonnetfmt::{Options, format};
use serde::Deserialize;

/// One entry of a snippets file. `why` is ignored; serde drops unknown fields.
#[derive(Debug, Deserialize)]
struct Snippet {
	name: String,
	source: String,
}

/// The snippets that need a second run to settle, with the seam that does it.
///
/// Grouped by mechanism rather than sorted, because the grouping is the
/// explanation. Adding a line here without a `CLAUDE.md` entry defeats the
/// point of the list.
const KNOWN_NON_CONVERGENT: &[&str] = &[
	// `PrettyFieldNames` (step 10) asks `IsValidIdentifier` of the literal's
	// *stored* value, so a fully escaped name keeps its brackets or quotes;
	// `EnforceStringStyle` (step 11) then unescapes it, and the second run
	// promotes what the first refused.
	"pretty_names/index_escape_sequence",
	"pretty_names/field_escape_sequence",
	// `FixParens` (step 6) is an `if`, not a loop, and it hands the walk the
	// node that replaced the inner `Parens` rather than re-examining the outer
	// one — so three levels become two and four halve to two.
	"fix_parens/triple",
	"fix_parens/quad",
	// The same pass, a second and separate mechanism: both its moves are
	// `FodderMoveFront`, and for redundant parens on the file's leftmost spine
	// `openFodder(node)` *is* the file's opening fodder — which
	// `removeInitialNewlines` already cleaned at step 2, four steps earlier.
	// Anything between the two `(` therefore lands at the top of the file.
	"fix_parens/double_multiline",
	"fix_parens/double_newline_between_opens",
	"fix_parens/double_line_comment_between_opens",
	"fix_parens/double_comment_both_sides_of_seam",
	"fix_parens/double_blank_lines_between_opens",
	"fix_parens/three_opens_with_comments",
	// `SortImports` (step 1) keys on the literal's *stored* value, and
	// `EnforceStringStyle` (step 11) unescapes it — so the second run sorts on
	// different keys and **reorders the imports**, carrying their comments with
	// them. `a` begins with 0x5C and sorts ahead of `_x` (0x5F); the `a` it
	// denotes is 0x61 and sorts behind.
	"sort_imports/key_is_the_escaped_value",
	"sort_imports/key_is_the_escaped_value_swaps",
];

/// The snippets whose **first-run output does not parse**.
///
/// This list did not exist until Phase 4, and its absence was the right default
/// for three phases: no snippet reached the class, so the assertion below was
/// unconditional and said so. What changed is not the port but what is known
/// about upstream — Phase 4's corpus set found two files `tk fmt` destroys, and
/// these two snippets were written to pin the pass responsible.
///
/// So the list exists, and it is deliberately separate from
/// [`KNOWN_NON_CONVERGENT`] rather than merged into it. A file that formats to
/// a different file is unsettled; a file that formats to one the parser refuses
/// is destroyed, and `tk fmt` writes in place. Keeping one list would let the
/// second hide among the first.
///
/// `PrettyFieldNames` (step 10) rewrites an object field whose name is a string
/// literal from the computed `['x']:` form to a plain field — `x:` for an
/// identifier, `'a b':` for anything else — with **no `ObjectComp` guard**. A
/// comprehension may only have `[e]` fields, so either output is refused with
/// `Object comprehensions can only have [e] fields`. The pass oracle attributes
/// both snippets to `PrettyFieldNames` and to no other pass.
///
/// **What survives is a genuinely computed name.** `{ [k]: 1 for k in … }` is
/// untouched, and that is measured rather than asserted: five other snippets in
/// `pass-snippets.json` are object comprehensions with a computed name and all
/// five pass the sweep below.
///
/// Ratcheted both ways, like the list above.
const OUTPUT_DOES_NOT_REPARSE: &[&str] = &[
	"pretty_names/field_computed_in_object_comp",
	"pretty_names/field_computed_in_object_comp_not_an_identifier",
];

/// The snippet families to sweep. Both hold inputs written to be awkward,
/// which is exactly what the corpus cannot supply.
const FAMILIES: &[&str] = &["pass-snippets.json", "node-snippets.json"];

fn crate_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_snippets(name: &str) -> Vec<Snippet> {
	let path = crate_dir().join("testdata").join(name);
	let raw =
		fs::read_to_string(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
	serde_json::from_str(&raw).unwrap_or_else(|err| panic!("parsing {}: {err}", path.display()))
}

/// Render a snippet compactly enough to read in a panic message.
///
/// By chars rather than bytes: a snippet may hold a multi-byte character, and
/// slicing a `str` at a byte offset inside one panics.
fn clip(text: &str) -> String {
	const LIMIT: usize = 160;
	if text.chars().count() <= LIMIT {
		return format!("{text:?}");
	}
	let head: String = text.chars().take(LIMIT).collect();
	format!("{head:?}… ({} bytes in full)", text.len())
}

#[test]
fn formatting_a_snippet_twice_moves_only_the_known_cases() {
	let options = Options::default();
	let known: BTreeSet<&str> = KNOWN_NON_CONVERGENT.iter().copied().collect();
	let known_refused: BTreeSet<&str> = OUTPUT_DOES_NOT_REPARSE.iter().copied().collect();

	let mut moved: BTreeSet<String> = BTreeSet::new();
	let mut refused: BTreeSet<String> = BTreeSet::new();
	let mut unexpected = String::new();
	let mut unparseable = String::new();
	let mut checked = 0;

	for family in FAMILIES {
		for snippet in read_snippets(family) {
			// A snippet the formatter refuses grades nothing here; that the
			// refusal matches go-jsonnet is `node_parity`'s business.
			let Ok(first) = format(&snippet.name, &snippet.source, &options) else {
				continue;
			};
			checked += 1;

			let second = match format(&snippet.name, &first, &options) {
				Ok(second) => second,
				Err(err) => {
					// Categorically worse than a non-convergence: `tk fmt`
					// writes a file that no longer parses, in place.
					refused.insert(snippet.name.clone());
					if !known_refused.contains(snippet.name.as_str()) {
						let _ = write!(
							unparseable,
							"\n  {} ({family})\n      in  {}\n      out {}\n      err {}",
							snippet.name,
							clip(&snippet.source),
							clip(&first),
							err.message()
						);
					}
					continue;
				}
			};

			if second == first {
				continue;
			}
			moved.insert(snippet.name.clone());
			if !known.contains(snippet.name.as_str()) {
				let _ = write!(
					unexpected,
					"\n  {} ({family})\n      in  {}\n      1st {}\n      2nd {}",
					snippet.name,
					clip(&snippet.source),
					clip(&first),
					clip(&second)
				);
			}
		}
	}

	assert!(
		unparseable.is_empty(),
		"the formatter's own output does not parse, and the snippet is not in \
		 OUTPUT_DOES_NOT_REPARSE. This is not a non-convergence — it means `tk fmt` writes a \
		 broken file, in place, and it has to be understood before anything else. Confirm it \
		 against the real tk on both runs, name the mechanism in CLAUDE.md, and only then add \
		 the name:{unparseable}"
	);

	assert!(
		unexpected.is_empty(),
		"a snippet needs a second run to settle and is not in KNOWN_NON_CONVERGENT. Do **not** \
		 answer this with a convergence loop — matching `tk fmt` on one run is the contract, and \
		 iterating would diverge from it. Trace the seam to two specific steps of \
		 `FormatNode`'s order, record it in CLAUDE.md, and then add the name:{unexpected}"
	);

	// The other direction. A listed name that now settles is either a fixed
	// upstream or a drifted port, and both need the entry removed rather than
	// left to rot.
	let settled_after_all: Vec<&str> = KNOWN_NON_CONVERGENT
		.iter()
		.copied()
		.filter(|name| !moved.contains(*name))
		.collect();
	assert!(
		settled_after_all.is_empty(),
		"{} entry/entries in KNOWN_NON_CONVERGENT now settle on the first run. Either the port \
		 drifted or upstream changed; check against go-jsonnet and remove the entry:\n  {}",
		settled_after_all.len(),
		settled_after_all.join("\n  ")
	);

	// And the same direction for the worse class. A listed name whose output
	// starts parsing means upstream fixed the pass, which is a change worth
	// noticing rather than a list worth leaving.
	let reparsed_after_all: Vec<&str> = OUTPUT_DOES_NOT_REPARSE
		.iter()
		.copied()
		.filter(|name| !refused.contains(*name))
		.collect();
	assert!(
		reparsed_after_all.is_empty(),
		"{} entry/entries in OUTPUT_DOES_NOT_REPARSE now produce output that parses. Check \
		 against go-jsonnet, and remove the entry and its CLAUDE.md paragraph together:\n  {}",
		reparsed_after_all.len(),
		reparsed_after_all.join("\n  ")
	);

	eprintln!(
		"fmt idempotence: {checked} snippets formatted twice, {} move, {} produce output that \
		 does not parse, all of them known",
		moved.len(),
		refused.len()
	);
}

#[test]
fn every_known_non_convergent_name_exists() {
	// A typo in the list above would otherwise read as a case that settled, and
	// send a reader looking for a drift that never happened.
	let mut all: BTreeSet<String> = BTreeSet::new();
	for family in FAMILIES {
		all.extend(read_snippets(family).into_iter().map(|s| s.name));
	}

	let missing: Vec<&str> = KNOWN_NON_CONVERGENT
		.iter()
		.chain(OUTPUT_DOES_NOT_REPARSE)
		.copied()
		.filter(|name| !all.contains(*name))
		.collect();
	assert!(
		missing.is_empty(),
		"the non-convergence lists name {} snippet(s) that do not exist:\n  {}",
		missing.len(),
		missing.join("\n  ")
	);
}
