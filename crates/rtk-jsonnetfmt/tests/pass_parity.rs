//! Each implemented pass is compared with Go on every input. Missing, duplicate, unknown or panicking comparisons fail.

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
	/// A pass that crashed on this input. This is a missing comparison and fails.
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
///
/// The three passes that read an option get it from `Options::default`, because
/// `_staged/passdump.go` constructs them with `iformatter.DefaultOptions()`.
/// A pass is run unconditionally here, as it is there: `FormatNode`'s gates
/// are [`rtk_jsonnetfmt::format`]'s business, and the corpus grades those.
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

	assert_eq!(
		file.passes
			.iter()
			.map(|cell| cell.pass.as_str())
			.collect::<Vec<_>>(),
		IMPLEMENTED,
		"{}: incomplete, duplicate or reordered pass comparisons",
		file.source
	);
	let raw = dump(&raw_node, &raw_final);

	for cell in &file.passes {
		assert!(
			cell.panicked.is_empty(),
			"{}::{}: required Go comparison panicked: {}",
			file.source,
			cell.pass,
			cell.panicked
		);

		// Parse afresh for each pass, exactly as the oracle does.
		let (mut node, mut final_fodder) =
			snippet_to_raw_ast(&file.source, input).expect("it parsed a moment ago");

		assert!(
			apply(&cell.pass, &mut node, &mut final_fodder),
			"{}: pass {} has no comparison implementation",
			file.source,
			cell.pass
		);

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
}

#[test]
fn every_pass_matches_go_jsonnet_on_every_corpus_file() {
	let dumps = read_oracle::<PassFileDump>("pass-oracle.json", HOW);

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
	let dumps = read_oracle::<PassFileDump>("pass-snippet-oracle.json", HOW);

	let snippets: Vec<Snippet> = read_oracle("pass-snippets.json", HOW);

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
	let dumps = read_oracle::<PassFileDump>("pass-snippet-oracle.json", HOW);

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
	let snippets: Vec<Snippet> = read_oracle("pass-snippets.json", HOW);
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

#[test]
fn incomplete_duplicate_unknown_and_panicking_pass_comparisons_fail() {
	let saved: Vec<serde_json::Value> = read_oracle("pass-regression-oracle.json", HOW);
	let snippets: Vec<Snippet> = read_oracle("pass-regressions.json", HOW);
	let source = saved[0]["source"].as_str().unwrap();
	let input = &snippets
		.iter()
		.find(|snippet| snippet.name == source)
		.unwrap()
		.source;
	for defect in ["missing", "duplicate", "unknown", "panic"] {
		let mut answer = saved[0].clone();
		let cells = answer["passes"].as_array_mut().unwrap();
		match defect {
			"missing" => {
				cells.pop();
			}
			"duplicate" => {
				cells.push(cells[0].clone());
			}
			"unknown" => {
				cells[0]["pass"] = serde_json::json!("NotARealPass");
			}
			"panic" => {
				cells[0]["panic"] = serde_json::json!("missing Go comparison");
			}
			_ => unreachable!(),
		}
		let answer: PassFileDump = serde_json::from_value(answer).unwrap();
		assert!(
			std::panic::catch_unwind(|| {
				grade(&answer, input, &mut BTreeMap::new(), &mut Tally::default());
			})
			.is_err(),
			"{defect} comparison silently passed"
		);
	}
}
