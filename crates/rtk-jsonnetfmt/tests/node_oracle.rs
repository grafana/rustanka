//! Grades the AST and its fodder slots against what go-jsonnet's own parser
//! produces.
//!
//! This is the oracle the parser port is written against, and it exists before
//! the parser on purpose. `docs/rtk-fmt-plan.md` argues the order from
//! evidence: the lexer went 138/138 on a first compile because a token-level
//! dump existed first, and of 16 lexer expectations derived by *reading* Go's
//! source, 2 were wrong — both about fodder the model composes rather than
//! reads. A parser composes fodder at nearly every node, so that is its
//! dominant failure mode, and the text corpus is far too coarse to locate one:
//! a misplaced `CommaFodder` surfaces as a whitespace diff hundreds of lines
//! away, if at all. Here it surfaces at `$.Fields[2].CommaFodder`.
//!
//! `make update-fmt-node-oracle` regenerates both halves. Unlike the lexer
//! oracle it needs no staged checkout: every fodder slot is an exported field
//! of go-jsonnet's `ast` package, and `formatter.SnippetToRawAST` is the same
//! public entry point `Format` calls.
//!
//! # What is graded here, and what is not yet
//!
//! Until the parser lands there is nothing to compare against, so these tests
//! grade the *oracle* — that it deserializes, that it covers the files the
//! corpus covers, that every snippet parsed, and that between them the two
//! halves reach every node kind and every fodder slot the port will have to
//! fill. A slot no oracle reaches is a slot the parser could get wrong for
//! free, which is exactly what this phase is meant to prevent.
//!
//! The comparison itself lands with the parser, alongside a
//! `node-baseline.toml` ratchet matching the corpus and lexer tests.

use std::{
	collections::{BTreeMap, BTreeSet},
	fs,
	path::PathBuf,
};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileDump {
	source: String,
	#[serde(default)]
	entries: Vec<Entry>,
	/// The parser's message when it refused the file.
	#[serde(default)]
	error: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
	/// Where this node sits, from the root: `$.Fields[2].Expr2`.
	path: String,
	kind: String,
	/// What the unparser reads that is neither fodder nor a child node — an
	/// operator, a field's visibility, a trailing comma. Carried so the parser
	/// can be graded on it; nothing reads it until the parser lands.
	#[allow(dead_code)]
	#[serde(default)]
	attrs: BTreeMap<String, String>,
	#[serde(default)]
	slots: Vec<Slot>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Slot {
	name: String,
	/// Absent in the JSON when empty, which is not the same as the slot being
	/// missing: the slot list is fixed by the node's kind, and `Slice` treats
	/// the emptiness of `StepColonFodder` as meaningful.
	#[serde(default)]
	fodder: Vec<FodderElementDump>,
}

#[derive(Debug, Deserialize)]
struct FodderElementDump {
	#[allow(dead_code)]
	kind: String,
	#[allow(dead_code)]
	blanks: usize,
	#[allow(dead_code)]
	indent: usize,
	#[allow(dead_code)]
	#[serde(default)]
	comment: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Snippet {
	name: String,
	/// Documentation for human readers; the dumper ignores it and so does this.
	#[allow(dead_code)]
	why: String,
	#[allow(dead_code)]
	source: String,
}

#[derive(Debug, Deserialize)]
struct Manifest {
	entries: Vec<ManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct ManifestEntry {
	source: String,
}

fn crate_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Read one half of the oracle, or `None` when it has not been generated.
///
/// Absence is a skip rather than a failure, the way the lexer oracle and the
/// corpus treat it: generating it needs Go.
fn read_oracle(name: &str) -> Option<Vec<FileDump>> {
	let path = crate_dir().join("testdata").join(name);
	let raw = fs::read_to_string(&path).ok().or_else(|| {
		eprintln!(
			"no node oracle at {}; generate it with `make update-fmt-node-oracle` (needs Go). \
			 Skipping.",
			path.display()
		);
		None
	})?;
	Some(
		serde_json::from_str(&raw)
			.unwrap_or_else(|err| panic!("parsing {}: {err}", path.display())),
	)
}

fn read_snippets() -> Vec<Snippet> {
	let path = crate_dir().join("testdata/node-snippets.json");
	serde_json::from_str(
		&fs::read_to_string(&path)
			.unwrap_or_else(|err| panic!("reading {}: {err}", path.display())),
	)
	.unwrap_or_else(|err| panic!("parsing {}: {err}", path.display()))
}

/// Every node kind the dumper can emit, which is every kind
/// `internal/parser/parser.go` constructs.
///
/// `ast` also declares `DesugaredObject` and its field type, and the parser
/// never builds either: they appear during desugaring, which the formatter does
/// not do — it parses with `SnippetToRawAST` and unparses. So they are
/// deliberately absent, and the port should not carry them.
const NODE_KINDS: &[&str] = &[
	"Apply",
	"ApplyBrace",
	"Array",
	"ArrayComp",
	"Assert",
	"Binary",
	"Conditional",
	"Dollar",
	"Error",
	"Function",
	"Import",
	"ImportBin",
	"ImportStr",
	"InSuper",
	"Index",
	"LiteralBoolean",
	"LiteralNull",
	"LiteralNumber",
	"LiteralString",
	"Local",
	"Object",
	"ObjectComp",
	"Parens",
	"Self",
	"Slice",
	"SuperIndex",
	"Unary",
	"Var",
];

/// The helper structs that carry fodder of their own, plus the two synthetic
/// entries the dumper emits.
const HELPER_KINDS: &[&str] = &[
	"CommaSeparatedExpr",
	"FinalFodder",
	"ForSpec",
	"IfSpec",
	"LocalBind",
	"Method",
	"NamedArgument",
	"ObjectField",
	"Parameter",
	"<nil>",
];

/// Every `(kind, slot)` pair the parser fills, and so every one the port has to
/// fill to match. `Fodder` is on every node and is checked separately.
///
/// One entry is not a slot the *unparser* reads: `NamedArgument.EqFodder`.
/// `unparseParams` fills `param.EqFodder` for a function parameter, but the
/// named-argument branch of `Apply` writes `"="` directly and never fills it —
/// so `f(b /* x */ = 2)` loses that comment in `tk fmt`. The parser still
/// stores it, so the port must too or the AST will not match here; it is the
/// unparser that must go on ignoring it.
const EXPECTED_SLOTS: &[(&str, &str)] = &[
	("Apply", "FodderLeft"),
	("Apply", "FodderRight"),
	("Apply", "TailStrictFodder"),
	("Array", "CloseFodder"),
	("ArrayComp", "CloseFodder"),
	("ArrayComp", "TrailingCommaFodder"),
	("Assert", "ColonFodder"),
	("Assert", "SemicolonFodder"),
	("Binary", "OpFodder"),
	("CommaSeparatedExpr", "CommaFodder"),
	("Conditional", "ElseFodder"),
	("Conditional", "ThenFodder"),
	("ForSpec", "ForFodder"),
	("ForSpec", "InFodder"),
	("ForSpec", "VarFodder"),
	("Function", "ParenLeftFodder"),
	("Function", "ParenRightFodder"),
	("IfSpec", "IfFodder"),
	("InSuper", "InFodder"),
	("InSuper", "SuperFodder"),
	("Index", "LeftBracketFodder"),
	("Index", "RightBracketFodder"),
	("LocalBind", "CloseFodder"),
	("LocalBind", "EqFodder"),
	("LocalBind", "VarFodder"),
	("Method", "ParenLeftFodder"),
	("Method", "ParenRightFodder"),
	("NamedArgument", "CommaFodder"),
	("NamedArgument", "EqFodder"),
	("NamedArgument", "NameFodder"),
	("Object", "CloseFodder"),
	("ObjectComp", "CloseFodder"),
	("ObjectComp", "TrailingCommaFodder"),
	("ObjectField", "CommaFodder"),
	("ObjectField", "Fodder1"),
	("ObjectField", "Fodder2"),
	("ObjectField", "OpFodder"),
	("Parameter", "CommaFodder"),
	("Parameter", "EqFodder"),
	("Parameter", "NameFodder"),
	("Parens", "CloseFodder"),
	("Slice", "EndColonFodder"),
	("Slice", "LeftBracketFodder"),
	("Slice", "RightBracketFodder"),
	("Slice", "StepColonFodder"),
	("SuperIndex", "DotFodder"),
	("SuperIndex", "IDFodder"),
];

/// Node kinds whose opening fodder is stored further inside the tree.
///
/// `leftRecursive` in `jsonnetfmt.go` names exactly these, and `unparse` fills
/// the open fodder only when it returns nil — so on one of these the `Fodder`
/// slot is empty and the fodder lives on the leftmost leaf instead. A port that
/// put it on the outer node would still round-trip most files.
const LEFT_RECURSIVE: &[&str] = &["Apply", "ApplyBrace", "Binary", "Index", "InSuper", "Slice"];

struct Coverage {
	kinds: BTreeSet<String>,
	slots: BTreeSet<(String, String)>,
	/// Slots seen carrying at least one fodder element. A slot only ever seen
	/// empty is barely graded: the port could leave it empty always and match.
	non_empty_slots: BTreeSet<(String, String)>,
	entries: usize,
}

impl Coverage {
	fn of<'a>(dumps: impl IntoIterator<Item = &'a FileDump>) -> Self {
		let mut coverage = Self {
			kinds: BTreeSet::new(),
			slots: BTreeSet::new(),
			non_empty_slots: BTreeSet::new(),
			entries: 0,
		};
		for dump in dumps {
			for entry in &dump.entries {
				coverage.entries += 1;
				coverage.kinds.insert(entry.kind.clone());
				for slot in &entry.slots {
					let key = (entry.kind.clone(), slot.name.clone());
					if !slot.fodder.is_empty() {
						coverage.non_empty_slots.insert(key.clone());
					}
					coverage.slots.insert(key);
				}
			}
		}
		coverage
	}
}

#[test]
fn the_oracle_covers_the_same_files_as_the_corpus() {
	let Some(dumps) = read_oracle("node-oracle.json") else {
		return;
	};

	let manifest_path = crate_dir().join("testdata/corpus/manifest.json");
	let manifest: Manifest = serde_json::from_str(
		&fs::read_to_string(&manifest_path)
			.unwrap_or_else(|err| panic!("reading {}: {err}", manifest_path.display())),
	)
	.expect("the corpus manifest is valid JSON");

	let expected: Vec<&str> = manifest
		.entries
		.iter()
		.map(|entry| entry.source.as_str())
		.collect();
	let got: Vec<&str> = dumps.iter().map(|dump| dump.source.as_str()).collect();

	// Same list, same order. If these disagree the oracle is stale and every
	// comparison built on it would be grading the wrong file.
	assert_eq!(
		expected, got,
		"the node oracle is stale; re-run `make update-fmt-node-oracle`"
	);
}

#[test]
fn every_snippet_parses_and_the_snippet_oracle_is_current() {
	let Some(dumps) = read_oracle("node-snippet-oracle.json") else {
		return;
	};
	let snippets = read_snippets();

	let names: Vec<&str> = snippets.iter().map(|s| s.name.as_str()).collect();
	let dumped: Vec<&str> = dumps.iter().map(|d| d.source.as_str()).collect();
	assert_eq!(
		names, dumped,
		"the snippet oracle is stale; re-run `make update-fmt-node-oracle`"
	);

	// A snippet exists to pin a slot, so one go-jsonnet refuses is a snippet
	// with a typo in it, pinning nothing. The corpus is allowed its three
	// deliberate parse errors; these are not.
	let refused: Vec<String> = dumps
		.iter()
		.filter(|dump| !dump.error.is_empty())
		.map(|dump| format!("{}: {}", dump.source, dump.error))
		.collect();
	assert!(
		refused.is_empty(),
		"go-jsonnet refused {} snippet(s), so they pin nothing:\n  {}",
		refused.len(),
		refused.join("\n  ")
	);
}

#[test]
fn the_oracle_reaches_every_node_kind_and_every_fodder_slot() {
	let Some(corpus) = read_oracle("node-oracle.json") else {
		return;
	};
	let Some(snippets) = read_oracle("node-snippet-oracle.json") else {
		return;
	};

	let corpus_coverage = Coverage::of(&corpus);
	let snippet_coverage = Coverage::of(&snippets);
	let all = Coverage::of(corpus.iter().chain(&snippets));

	let mut missing_kinds: Vec<&str> = NODE_KINDS
		.iter()
		.chain(HELPER_KINDS)
		.filter(|kind| !all.kinds.contains(**kind))
		.copied()
		.collect();
	missing_kinds.sort_unstable();

	let missing_slots: Vec<String> = EXPECTED_SLOTS
		.iter()
		.filter(|(kind, slot)| {
			!all.slots
				.contains(&((*kind).to_owned(), (*slot).to_owned()))
		})
		.map(|(kind, slot)| format!("{kind}.{slot}"))
		.collect();

	// A slot the oracle only ever sees empty is a slot the port could leave
	// empty always and still match, so it is worth knowing about — but the
	// cases are legitimate (an `Apply` with no tailstrict never fills
	// `TailStrictFodder`), so this reports rather than fails.
	let always_empty: Vec<String> = EXPECTED_SLOTS
		.iter()
		.filter(|(kind, slot)| {
			all.slots
				.contains(&((*kind).to_owned(), (*slot).to_owned()))
				&& !all
					.non_empty_slots
					.contains(&((*kind).to_owned(), (*slot).to_owned()))
		})
		.map(|(kind, slot)| format!("{kind}.{slot}"))
		.collect();

	// Left-recursive nodes must never carry their own opening fodder: it is
	// stored as far inside the tree as possible, and `unparse` only fills the
	// open fodder where `leftRecursive` returns nil. Checking it on the oracle
	// confirms the *oracle* records that, so the port can be graded on it.
	let mut left_recursive_with_fodder: Vec<String> = Vec::new();
	for dump in corpus.iter().chain(&snippets) {
		for entry in &dump.entries {
			if !LEFT_RECURSIVE.contains(&entry.kind.as_str()) {
				continue;
			}
			let carries = entry
				.slots
				.iter()
				.any(|slot| slot.name == "Fodder" && !slot.fodder.is_empty());
			if carries {
				left_recursive_with_fodder.push(format!(
					"{} at {} ({})",
					entry.kind, entry.path, dump.source
				));
			}
		}
	}

	assert!(
		missing_kinds.is_empty(),
		"no oracle file reaches these node kinds, so the parser could get them \
		 wrong for free; add a snippet to testdata/node-snippets.json:\n  {}",
		missing_kinds.join("\n  ")
	);
	assert!(
		missing_slots.is_empty(),
		"no oracle file reaches these fodder slots; add a snippet to \
		 testdata/node-snippets.json:\n  {}",
		missing_slots.join("\n  ")
	);
	assert!(
		left_recursive_with_fodder.is_empty(),
		"a left-recursive node carries its own opening fodder, which \
		 `leftRecursive` says is impossible — the dumper or this list is \
		 wrong:\n  {}",
		left_recursive_with_fodder.join("\n  ")
	);

	eprintln!(
		"node oracle: {} entries over {} corpus files, {} entries over {} snippets\n\
		 node oracle: {} node kinds, {} fodder slots, {} of them seen non-empty",
		corpus_coverage.entries,
		corpus.len(),
		snippet_coverage.entries,
		snippets.len(),
		all.kinds.len(),
		all.slots.len(),
		all.non_empty_slots.len(),
	);
	if !always_empty.is_empty() {
		eprintln!(
			"node oracle: {} slot(s) only ever seen empty: {}",
			always_empty.len(),
			always_empty.join(", ")
		);
	}
}
