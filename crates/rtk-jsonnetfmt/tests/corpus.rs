//! Breadth: real Jsonnet, formatted by go-jsonnet, compared byte for byte.
//!
//! This is what Phase 2 was measured against, and it replaces the round-trip
//! gate `docs/rtk-fmt-plan.md` originally asked for. That gate — `unparse(parse
//! (x)) == x` byte-for-byte — cannot hold, because go-jsonnet's unparser
//! renders text from the fodder model rather than copying the source: a tab
//! becomes eight spaces, a line-end comment is always preceded by exactly two
//! spaces, and `\r` is dropped from block strings. Asking rtk for a property
//! go-jsonnet itself lacks would have blocked the phase on a mistake.
//!
//! The answers are produced by `testdata/generate`, which calls
//! `formatter.Format` directly — the identical call `tanka.Format` makes. So
//! matching them *is* matching `tk fmt`, with no `tk` binary in the loop and no
//! temporary paths baked into the answers.
//!
//! Comparison is by bytes. Trailing newline, tabs against spaces, CRLF and BOM
//! are exactly where a formatter port drifts, and a line-based comparison hides
//! all four.
//!
//! # Two sets
//!
//! | set | files | inputs | answers |
//! | --- | --- | --- | --- |
//! | `in_repo` | 138 | already in this repository | `testdata/corpus/`, one golden each |
//! | `go_jsonnet_testdata` | 719 | committed here, in the same file | `testdata/go-jsonnet-corpus.json` |
//!
//! The second set's inputs are go-jsonnet's, not this repository's, so they are
//! committed **with** their answers. The counts below are asserted exactly, in
//! both directions, and a count that depended on whether a go-jsonnet checkout
//! happened to be present would be an assertion that cannot hold in both
//! environments — the `GO_JSONNET_FOR_TESTS` hazard `CLAUDE.md` records about
//! the `go_jsonnet/` fixture family, one level up. Self-contained artifacts are
//! how that is avoided; `testdata/corpus-baseline.toml` carries the argument in
//! full.
//!
//! # What these tests refuse to do
//!
//! **Skip.** Both artifacts are committed, so a missing one means a broken
//! checkout rather than a generator that has not been run, and a suite that
//! goes green having graded nothing is the failure this project has been bitten
//! by four times. For the same reason `files` is asserted as well as
//! `matching`: an artifact that silently lost entries would otherwise satisfy a
//! matching-count check while grading almost nothing.

use std::{
	collections::BTreeMap,
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
};

use rtk_jsonnetfmt::{Options, coalesce_error, format};
use serde::Deserialize;

/// The set whose manifest the node, pass and lexer oracles mirror file for
/// file. It does not grow casually; see `testdata/corpus-baseline.toml`.
const IN_REPO: &str = "in_repo";
/// go-jsonnet's own root `testdata/`.
const GO_JSONNET_TESTDATA: &str = "go_jsonnet_testdata";
/// Every set this file knows how to grade. A baseline naming anything else —
/// a typo, or a set added to the toml and to nothing else — fails, rather than
/// being ignored while its files go ungraded.
const SETS: [&str; 2] = [IN_REPO, GO_JSONNET_TESTDATA];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
	/// Which go-jsonnet produced these answers.
	go_jsonnet_version: String,
	entries: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
	/// Repository-relative, and also the diagnostic filename the generator
	/// passed to `Format` — so the Rust side has to pass the same one or the
	/// error goldens cannot match.
	source: String,
	golden: String,
	#[serde(default)]
	error: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExternalCorpus {
	go_jsonnet_version: String,
	/// Which set this file holds. Checked, because a file generated for a
	/// different set would otherwise be graded under the wrong baseline.
	set: String,
	/// The generator's own count, checked against the array it wrote.
	files: usize,
	entries: Vec<ExternalEntry>,
}

#[derive(Debug, Deserialize)]
struct ExternalEntry {
	/// The diagnostic filename `Format` was given: synthetic, and never a
	/// checkout path.
	name: String,
	input: String,
	output: String,
	#[serde(default)]
	error: bool,
}

#[derive(Debug, Deserialize)]
struct Baseline {
	sets: BTreeMap<String, SetBaseline>,
}

#[derive(Debug, Deserialize)]
struct SetBaseline {
	files: usize,
	matching: usize,
}

/// One gradeable file, however its set stores it.
struct Case {
	/// What `Format` was handed as its diagnostic filename.
	name: String,
	input: String,
	/// go-jsonnet's answer, or its error message where `error` is set.
	answer: String,
	error: bool,
}

/// A loaded set.
struct Set {
	go_jsonnet_version: String,
	cases: Vec<Case>,
}

fn crate_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
	crate_dir()
		.parent()
		.and_then(|crates| crates.parent())
		.expect("the crate is two levels below the repository root")
		.to_path_buf()
}

fn read(path: &Path) -> String {
	fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
}

fn load_baseline() -> Baseline {
	let path = crate_dir().join("testdata/corpus-baseline.toml");
	let baseline: Baseline = toml::from_str(&read(&path))
		.unwrap_or_else(|err| panic!("parsing {}: {err}", path.display()));

	let declared: Vec<&str> = baseline.sets.keys().map(String::as_str).collect();
	let mut known = SETS;
	known.sort_unstable();
	assert_eq!(
		declared,
		known.as_slice(),
		"{} declares sets this test does not grade, or omits one it does",
		path.display()
	);
	baseline
}

/// The in-repo set: answers in `testdata/corpus/`, inputs in the tree.
fn load_in_repo() -> Set {
	let corpus = crate_dir().join("testdata/corpus");
	let manifest: Manifest = serde_json::from_str(&read(&corpus.join("manifest.json")))
		.expect("the corpus manifest is valid JSON");
	assert!(!manifest.entries.is_empty(), "the in-repo corpus is empty");

	let root = repo_root();
	let mut cases = Vec::with_capacity(manifest.entries.len());
	let mut missing: Vec<&str> = Vec::new();

	for entry in &manifest.entries {
		let source = root.join(&entry.source);
		let Ok(input) = fs::read_to_string(&source) else {
			// A source named by the manifest but no longer in the tree: the
			// corpus needs regenerating, and skipping it would let the count
			// drift downwards unnoticed.
			missing.push(&entry.source);
			continue;
		};
		cases.push(Case {
			name: entry.source.clone(),
			input,
			answer: read(&corpus.join(&entry.golden)),
			error: entry.error,
		});
	}

	assert!(
		missing.is_empty(),
		"{} source file(s) in the manifest no longer exist. Run `make update-fmt-corpus`:\n  {}",
		missing.len(),
		missing.join("\n  ")
	);

	Set {
		go_jsonnet_version: manifest.go_jsonnet_version,
		cases,
	}
}

/// The go-jsonnet set: one self-contained file holding both halves.
fn load_go_jsonnet_testdata() -> Set {
	let path = crate_dir().join("testdata/go-jsonnet-corpus.json");
	let corpus: ExternalCorpus =
		serde_json::from_str(&read(&path)).expect("the go-jsonnet corpus is valid JSON");

	assert_eq!(
		corpus.set,
		GO_JSONNET_TESTDATA,
		"{} holds the set {:?}, which is not the one graded here",
		path.display(),
		corpus.set
	);
	assert_eq!(
		corpus.files,
		corpus.entries.len(),
		"{} says it holds {} files and holds {}",
		path.display(),
		corpus.files,
		corpus.entries.len()
	);
	assert!(!corpus.entries.is_empty(), "the go-jsonnet corpus is empty");

	Set {
		go_jsonnet_version: corpus.go_jsonnet_version,
		cases: corpus
			.entries
			.into_iter()
			.map(|entry| Case {
				name: entry.name,
				input: entry.input,
				answer: entry.output,
				error: entry.error,
			})
			.collect(),
	}
}

fn load(set: &str) -> Set {
	match set {
		IN_REPO => load_in_repo(),
		GO_JSONNET_TESTDATA => load_go_jsonnet_testdata(),
		other => panic!("no loader for the set {other:?}"),
	}
}

#[test]
fn corpus_matches_go_jsonnet_at_the_recorded_rate() {
	let baseline = load_baseline();

	let mut report = String::new();
	let mut summary: Vec<String> = Vec::new();

	for set in SETS {
		let expected = &baseline.sets[set];
		let loaded = load(set);

		let mut matching = 0;
		let mut differing: Vec<&str> = Vec::new();
		for case in &loaded.cases {
			let actual = coalesce_error(format(&case.name, &case.input, &Options::default()));
			if actual == case.answer {
				matching += 1;
			} else {
				differing.push(&case.name);
			}
		}

		let files = loaded.cases.len();
		summary.push(format!(
			"fmt corpus [{set}]: {matching} of {files} match go-jsonnet {}",
			loaded.go_jsonnet_version
		));

		if files != expected.files {
			write!(
				report,
				"\n[{set}] holds {files} files, but testdata/corpus-baseline.toml records \
				 {}. The denominator is ratcheted too: a set that lost files would otherwise \
				 pass a match-count check while grading almost nothing. Regenerate with \
				 `make update-fmt-corpus` and record the new number in the same commit.\n",
				expected.files
			)
			.expect("writing to a String cannot fail");
		}

		if matching != expected.matching {
			// A bounded sample: an unfinished formatter differs on nearly
			// everything, and a 700-line panic buries the number that matters.
			const SAMPLE: usize = 15;
			let mut sample = differing
				.iter()
				.take(SAMPLE)
				.copied()
				.collect::<Vec<_>>()
				.join("\n    ");
			if let Some(elided) = differing.len().checked_sub(SAMPLE).filter(|n| *n > 0) {
				write!(sample, "\n    ...and {elided} more")
					.expect("writing to a String cannot fail");
			}
			write!(
				report,
				"\n[{set}] match count is {matching} of {files}, but \
				 testdata/corpus-baseline.toml records {}.\n  \
				 If {matching} is higher, that is progress: set `matching = {matching}` under \
				 [sets.{set}] in the same commit.\n  \
				 If it is lower, something regressed — look at these:\n    {sample}\n  \
				 Answers generated by go-jsonnet {}.\n",
				expected.matching, loaded.go_jsonnet_version
			)
			.expect("writing to a String cannot fail");
		}
	}

	// Both sets are graded before anything is asserted, so one run reports
	// every number that moved rather than only the first.
	assert!(report.is_empty(), "{report}");

	for line in summary {
		eprintln!("{line}");
	}
}

/// Answers that format to something **different but still valid Jsonnet**.
///
/// Empty, and it may only grow with a `CLAUDE.md` entry naming the two steps of
/// `FormatNode`'s order that disagree. `tests/idempotence.rs` carries the
/// twelve snippets in this class.
const KNOWN_NON_CONVERGENT: &[&str] = &[];

/// Answers the formatter's own next run **refuses to parse**.
///
/// Categorically worse than a non-convergence, and `tests/idempotence.rs` says
/// so: over the snippets it treats this class as unlistable and asserts it
/// empty, because it means `tk fmt` writes a file that no longer parses — in
/// place, over the user's own tree. The breadth corpus reaches three. All three
/// are upstream's, all three were found by Phase 4's `go_jsonnet_testdata` set,
/// and **none is reachable from the in-repo 138**.
///
/// Each was confirmed against the real `tk` rather than traced by reading Go:
/// rtk and tk agree byte for byte on the first run — which the match test above
/// already proves, these three being inside its 719 — and on the second they
/// agree on the refusal *and* on its message and location. `CLAUDE.md` names
/// the mechanisms; there are two of them, not one.
///
/// Ratcheted both ways. A listed answer that starts reparsing fails as loudly
/// as an unlisted one that stops, because either means this list has become a
/// lie about what `tk fmt` does.
const OUTPUT_DOES_NOT_REPARSE: &[&str] = &[
	// `PrettyFieldNames` promotes `['x']:` to `x:` with no `ObjectComp` guard,
	// and a comprehension may only have `[e]` fields. Both then fail with
	// "Object comprehensions can only have [e] fields".
	"go-jsonnet/testdata/object_comp_err_elem.jsonnet",
	"go-jsonnet/testdata/object_literal_in_object_comp.jsonnet",
	// A `|||` block whose value is nothing but newlines. The unparser leaves a
	// blank value-line unindented, so no content line carries the block indent,
	// and the re-lex swallows the terminator: "Text block not terminated
	// with |||".
	"go-jsonnet/testdata/escaped_fields.jsonnet",
];

/// Render an answer compactly enough to read in a panic message.
///
/// By chars rather than bytes, as `tests/idempotence.rs` does: an answer may
/// hold a multi-byte character, and slicing a `str` inside one panics.
fn clip(text: &str) -> String {
	const LIMIT: usize = 160;
	if text.chars().count() <= LIMIT {
		return format!("{text:?}");
	}
	let head: String = text.chars().take(LIMIT).collect();
	format!("{head:?}… ({} bytes in full)", text.len())
}

/// Formatting go-jsonnet's own answer has to return that answer unchanged.
///
/// `docs/rtk-fmt-plan.md` asks for this per step, and Phase 2d is the step that
/// first asked it: `FixIndentation` and `FixNewlines` are the two passes that
/// could plausibly fail to settle, and the plan's original reasoning still
/// holds — a port that needs a second run to converge has a bug in one of them,
/// and adding a convergence loop would paper over it.
///
/// The inputs are the **answers**, not the sources, which is what makes this a
/// clean test of the property: an answer is by construction a fixed point of
/// go-jsonnet's own `Format`, so any movement here is rtk's.
///
/// # This is not an unconditional property
///
/// Phase 2c found that `jsonnetfmt` is not a fixed point in general —
/// `a['fo` `o` `']` formats to `a['foo']` and then to `a.foo`, because
/// `PrettyFieldNames` at step 10 sees the escape that `EnforceStringStyle`
/// removes at step 11. That is upstream's, and it is what `tk fmt` prints. So a
/// failure here is a bug **until** it is traced to a specific interaction in
/// `FormatNode`'s order, at which point it is recorded in `CLAUDE.md` and
/// excluded by name in one of the two lists above.
///
/// The two lists are kept apart because they are not the same finding. A file
/// that formats to a different file is unsettled; a file that formats to one
/// the parser refuses is destroyed. `tests/idempotence.rs` carries the twelve
/// snippet inputs in the first class and the seam behind each.
#[test]
fn formatting_an_answer_again_changes_nothing() {
	let mut report = String::new();
	let mut summary: Vec<String> = Vec::new();

	for set in SETS {
		let loaded = load(set);
		let mut checked = 0;
		let mut fixed = 0;
		let mut refused = 0;

		for case in &loaded.cases {
			// An answer holding a parse error is not Jsonnet; formatting it
			// would grade the error text, which the test above already does.
			if case.error {
				continue;
			}
			checked += 1;

			let listed_refused = OUTPUT_DOES_NOT_REPARSE.contains(&case.name.as_str());
			let listed_moved = KNOWN_NON_CONVERGENT.contains(&case.name.as_str());

			match format(&case.name, &case.answer, &Options::default()) {
				Err(error) => {
					refused += 1;
					if !listed_refused {
						write!(
							report,
							"\n[{set}] {} — the second run does not parse: {}\n      in  {}\n  \
							 This is not a non-convergence: it means `tk fmt` writes a file that \
							 no longer parses, and it has to be understood before anything else. \
							 Confirm it against the real tk on both runs, name the mechanism in \
							 CLAUDE.md, and only then add it to OUTPUT_DOES_NOT_REPARSE.\n",
							case.name,
							error.message(),
							clip(&case.answer)
						)
						.expect("writing to a String cannot fail");
					}
				}
				Ok(again) => {
					if listed_refused {
						write!(
							report,
							"\n[{set}] {} is in OUTPUT_DOES_NOT_REPARSE, but its second run now \
							 parses. Upstream has moved, or this port has: delete the entry, and \
							 the CLAUDE.md paragraph with it.\n",
							case.name
						)
						.expect("writing to a String cannot fail");
					} else if again == case.answer {
						fixed += 1;
						if listed_moved {
							write!(
								report,
								"\n[{set}] {} is in KNOWN_NON_CONVERGENT, but now settles. \
								 Delete the entry.\n",
								case.name
							)
							.expect("writing to a String cannot fail");
						}
					} else if !listed_moved {
						write!(
							report,
							"\n[{set}] {} changed when formatted a second time. It is a bug in a \
							 pass — most likely FixIndentation or FixNewlines — until it is \
							 traced to a specific cross-pass interaction in FormatNode's order. \
							 Do **not** add a convergence loop; see the correction in \
							 docs/rtk-fmt-plan.md, Phase 2.\n      1st {}\n      2nd {}\n",
							case.name,
							clip(&case.answer),
							clip(&again)
						)
						.expect("writing to a String cannot fail");
					}
				}
			}
		}

		// A set whose answers are all parse errors would check nothing at all.
		assert!(
			checked > 0,
			"[{set}] contributed no fixed-point checks, so this test graded it at zero"
		);
		// The refusal count is printed rather than only asserted, because it is
		// the number that says whether this set still reaches the worse class
		// at all — and a log nobody reads is exactly how this project has been
		// caught out before.
		summary.push(format!(
			"fmt idempotence [{set}]: {fixed} of {checked} answers are fixed points, \
			 {refused} refuse to reparse"
		));
	}

	// Every set is graded before anything is asserted, so one run reports all
	// of them rather than only the first.
	assert!(report.is_empty(), "{report}");

	for line in summary {
		eprintln!("{line}");
	}
}
