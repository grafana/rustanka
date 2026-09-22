//! Grades each formatter pass against the tree go-jsonnet's own copy of that
//! pass leaves behind.
//!
//! # Why this exists, and why the corpus is not enough
//!
//! `tests/corpus.rs` is the real target — byte-for-byte `tk fmt` output over
//! every Jsonnet file in the repository — but it cannot grade a pass. Of its
//! 138 files, `PrettyFieldNames` changes two and `FixTrailingCommas` and
//! `NoRedundantSliceColon` change **none**, because Jsonnet that is already
//! `tk fmt`-clean gives a pass nothing to do. A corpus count that moved by two
//! says nothing at all about two of the three Phase 2b passes.
//!
//! `Options` cannot isolate them either: `PrettyFieldNames` has a flag, but
//! `FixTrailingCommas` and `NoRedundantSliceColon` are unconditional, so no
//! option setting makes their effect observable on its own.
//!
//! So `make update-fmt-pass-oracle` stages a dumper into a go-jsonnet checkout
//! — `internal/formatter` can only be imported from inside that module — and
//! records what each pass does to each source. This test replays that here.
//! The reason to go to the trouble is the project's own record: of 16 lexer
//! expectations derived by reading Go's source, 2 were wrong, and both were
//! about fodder the model *composes* rather than reads. These passes are
//! almost nothing but fodder composition.
//!
//! # One pass at a time, on a fresh parse
//!
//! Each cell is one pass applied to its own parse of the source, not to the
//! pipeline's accumulated state. That is what lets a pass be graded before the
//! ones ahead of it in `FormatNode` exist, which is what landing them one at a
//! time requires. `FormatNode`'s order is a fourteen-line list read off
//! upstream, and the corpus grades it end to end.
//!
//! # Most cells say `unchanged`, and that is a real assertion
//!
//! Where go-jsonnet's pass changed nothing, the oracle records `unchanged`
//! instead of repeating the tree, and this test then requires rtk's pass to
//! change nothing either. A pass that mangles an already-formatted file fails
//! here — which is the failure mode that matters most, since `tk fmt` is run
//! in place on working trees.
//!
//! # Full parity, no ratchet
//!
//! Like `node_parity`, and for the same reason: a pass that is right on most
//! inputs is not a partial formatter, it is a wrong pass. Passes rtk has not
//! written yet are **skipped by name**, and the test prints which — so the
//! coverage it is actually providing is visible rather than assumed.

mod astdump;

use std::{collections::BTreeMap, fs};

use astdump::{DumpEntry, Entry, Snippet, compare, dump, read_oracle, repo_root, report};
use rtk_jsonnetfmt::{
	ast::Node,
	fodder::Fodder,
	parser::snippet_to_raw_ast,
	pass::visit_file,
	passes::{FixTrailingCommas, NoRedundantSliceColon, PrettyFieldNames},
};
use serde::Deserialize;

const HOW: &str = "make update-fmt-pass-oracle";

/// What one pass did to one source, as the oracle records it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PassDump {
	pass: String,
	#[serde(default)]
	unchanged: bool,
	#[serde(default)]
	entries: Vec<DumpEntry>,
	/// A pass that crashed on this input. Not expected under
	/// `DefaultOptions`, and skipped rather than graded, because there is no
	/// tree to compare against.
	#[serde(default, rename = "panic")]
	panicked: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PassFileDump {
	source: String,
	#[serde(default)]
	error: String,
	#[serde(default)]
	passes: Vec<PassDump>,
}

/// The passes rtk has written, and which `FormatNode` step each is.
///
/// Spelled as the Go type names, which is what the oracle records. Asserted
/// against the oracle's own list, so a pass that lands without being wired in
/// here fails rather than being silently ungraded.
const IMPLEMENTED: &[&str] = &[
	"FixTrailingCommas",
	"NoRedundantSliceColon",
	"PrettyFieldNames",
];

/// Run one named pass over a whole file, or report that rtk has not got it.
fn apply(name: &str, node: &mut Node, final_fodder: &mut Fodder) -> bool {
	match name {
		"FixTrailingCommas" => visit_file(&mut FixTrailingCommas, node, final_fodder),
		"NoRedundantSliceColon" => visit_file(&mut NoRedundantSliceColon, node, final_fodder),
		"PrettyFieldNames" => visit_file(&mut PrettyFieldNames, node, final_fodder),
		_ => return false,
	}
	true
}

/// The first entry at which two of rtk's own dumps differ.
///
/// Used for the `unchanged` cells, where the claim is about rtk against
/// itself: the oracle says this pass is a no-op on this source, so the tree
/// after it must equal the tree before.
fn first_own_difference(before: &[Entry], after: &[Entry]) -> Option<String> {
	for (at, (was, now)) in before.iter().zip(after).enumerate() {
		if was != now {
			return Some(format!(
				"go-jsonnet's pass changed nothing here, rtk's changed entry {at}\n    \
				 before: {was:?}\n    after:  {now:?}"
			));
		}
	}
	if before.len() != after.len() {
		return Some(format!(
			"go-jsonnet's pass changed nothing here, rtk's took the tree from {} entries to {}",
			before.len(),
			after.len()
		));
	}
	None
}

/// How many cells were graded, and how many were skipped and why.
#[derive(Default)]
struct Tally {
	graded_changed: usize,
	graded_unchanged: usize,
	skipped_unimplemented: BTreeMap<String, usize>,
	skipped_panicked: usize,
}

/// Grade every pass cell of one source, collecting divergences by
/// `source::pass`.
fn grade(
	file: &PassFileDump,
	input: &str,
	divergences: &mut BTreeMap<String, String>,
	tally: &mut Tally,
) {
	let parsed = snippet_to_raw_ast(&file.source, input);

	if !file.error.is_empty() {
		// A source go-jsonnet refused has no passes to grade; that the
		// refusal matches is `node_parity`'s business and the corpus's.
		match parsed {
			Err(err) if err.message() == file.error => {}
			Err(err) => {
				divergences.insert(
					file.source.clone(),
					format!(
						"error text differs\n    go-jsonnet: {:?}\n    rtk:        {:?}",
						file.error,
						err.message()
					),
				);
			}
			Ok(_) => {
				divergences.insert(
					file.source.clone(),
					format!(
						"go-jsonnet refused this with {:?}, rtk parsed it",
						file.error
					),
				);
			}
		}
		return;
	}

	let Ok((raw_node, raw_final)) = parsed else {
		divergences.insert(
			file.source.clone(),
			"go-jsonnet parsed this, rtk refused it".to_owned(),
		);
		return;
	};

	let raw = dump(&raw_node, &raw_final);

	for cell in &file.passes {
		if !cell.panicked.is_empty() {
			tally.skipped_panicked += 1;
			continue;
		}

		// Parse afresh for each pass, exactly as the oracle does.
		let (mut node, mut final_fodder) =
			snippet_to_raw_ast(&file.source, input).expect("it parsed a moment ago");

		if !apply(&cell.pass, &mut node, &mut final_fodder) {
			*tally
				.skipped_unimplemented
				.entry(cell.pass.clone())
				.or_default() += 1;
			continue;
		}

		let got = dump(&node, &final_fodder);
		let detail = if cell.unchanged {
			tally.graded_unchanged += 1;
			first_own_difference(&raw, &got)
		} else {
			tally.graded_changed += 1;
			compare(&cell.entries, &got)
		};

		if let Some(detail) = detail {
			divergences.insert(format!("{}::{}", file.source, cell.pass), detail);
		}
	}
}

fn announce(what: &str, tally: &Tally) {
	eprintln!(
		"pass parity ({what}): {} cells where go-jsonnet's pass changed the tree, \
		 {} where it did not, all matching",
		tally.graded_changed, tally.graded_unchanged
	);
	if tally.skipped_panicked > 0 {
		eprintln!(
			"  {} cells skipped: the pass panicked",
			tally.skipped_panicked
		);
	}
	let mut names: Vec<&String> = tally.skipped_unimplemented.keys().collect();
	names.sort_unstable();
	if !names.is_empty() {
		eprintln!(
			"  not written yet, so not graded: {}",
			names
				.iter()
				.map(|name| name.as_str())
				.collect::<Vec<_>>()
				.join(", ")
		);
	}
}

#[test]
fn every_pass_matches_go_jsonnet_on_every_corpus_file() {
	let Some(dumps) = read_oracle::<Vec<PassFileDump>>("pass-oracle.json", HOW) else {
		return;
	};

	let root = repo_root();
	let mut divergences: BTreeMap<String, String> = BTreeMap::new();
	let mut tally = Tally::default();

	for file in &dumps {
		let input = fs::read_to_string(root.join(&file.source))
			.unwrap_or_else(|err| panic!("reading {}: {err}", file.source));
		grade(file, &input, &mut divergences, &mut tally);
	}

	assert!(
		divergences.is_empty(),
		"{}",
		report(&divergences, dumps.len(), "corpus files")
	);
	announce("corpus", &tally);
}

#[test]
fn every_pass_matches_go_jsonnet_on_every_snippet() {
	let Some(dumps) = read_oracle::<Vec<PassFileDump>>("pass-snippet-oracle.json", HOW) else {
		return;
	};

	let snippets: Vec<Snippet> =
		read_oracle("pass-snippets.json", HOW).expect("the snippets are committed");

	let names: Vec<&str> = snippets.iter().map(|s| s.name.as_str()).collect();
	let dumped: Vec<&str> = dumps.iter().map(|d| d.source.as_str()).collect();
	assert_eq!(names, dumped, "the snippet oracle is stale; re-run `{HOW}`");

	let mut divergences: BTreeMap<String, String> = BTreeMap::new();
	let mut tally = Tally::default();

	for (snippet, file) in snippets.iter().zip(&dumps) {
		grade(file, &snippet.source, &mut divergences, &mut tally);
	}

	assert!(
		divergences.is_empty(),
		"{}",
		report(&divergences, snippets.len(), "snippets")
	);
	announce("snippets", &tally);
}

#[test]
fn the_snippets_reach_every_pass_that_is_written() {
	// The corpus's problem, asserted away: a snippet set that leaves a pass
	// with nothing to do would grade it no better than the 138 real files do.
	// Every implemented pass has to *change* at least one snippet.
	let Some(dumps) = read_oracle::<Vec<PassFileDump>>("pass-snippet-oracle.json", HOW) else {
		return;
	};

	let mut exercised: BTreeMap<&str, usize> = BTreeMap::new();
	let mut known: Vec<&str> = Vec::new();
	for file in &dumps {
		for cell in &file.passes {
			if !known.contains(&cell.pass.as_str()) {
				known.push(&cell.pass);
			}
			if !cell.unchanged && cell.panicked.is_empty() {
				*exercised.entry(&cell.pass).or_default() += 1;
			}
		}
	}

	for pass in IMPLEMENTED {
		assert!(
			known.contains(pass),
			"{pass} is implemented but the oracle does not record it; add it to \
			 `passNames` in testdata/generate/_staged/passdump.go"
		);
		let count = exercised.get(pass).copied().unwrap_or_default();
		assert!(
			count > 0,
			"no snippet in testdata/pass-snippets.json makes {pass} change anything, \
			 so it is graded only by its own no-ops"
		);
	}

	eprintln!(
		"pass coverage: {}",
		exercised
			.iter()
			.map(|(pass, count)| format!("{pass} changes {count}"))
			.collect::<Vec<_>>()
			.join(", ")
	);
}

#[test]
fn every_snippet_parses() {
	// A snippet the parser refuses grades nothing, and the oracle would record
	// the refusal rather than a tree. Written inputs are allowed to be
	// awkward, not invalid.
	let snippets: Vec<Snippet> =
		read_oracle("pass-snippets.json", HOW).expect("the snippets are committed");
	assert!(!snippets.is_empty(), "the snippets file is empty");

	for snippet in &snippets {
		assert!(
			!snippet.why.is_empty(),
			"{} has no `why`: a snippet has to say what it is for",
			snippet.name
		);
		if let Err(err) = snippet_to_raw_ast(&snippet.name, &snippet.source) {
			panic!(
				"{} does not parse: {}\n    source: {:?}",
				snippet.name,
				err.message(),
				snippet.source
			);
		}
	}
}
