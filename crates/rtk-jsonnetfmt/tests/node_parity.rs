//! Grades the parser port node by node, and slot by slot, against the AST
//! go-jsonnet's own parser produces.
//!
//! `tests/node_oracle.rs` grades the *oracle* — that it is current and that it
//! reaches everything. This grades the **port** against it, which is what the
//! oracle was built for: a misplaced `CommaFodder` shows up here as a
//! divergence at `$.Fields[2].CommaFodder`, rather than as a whitespace diff
//! hundreds of lines away in the text corpus, or not at all.
//!
//! The dumper and the comparison live in `tests/astdump/`, shared with
//! `tests/pass_parity.rs`, because both oracles come out of one Go dumper and
//! so must be read by one Rust one.
//!
//! Three families, from three answer files. The corpus and the snippets grade
//! trees; `parse-error-snippets.json` grades **refusals**, which is a different
//! question and the only place a parser error's *location* is compared against
//! go-jsonnet at all — see
//! [`the_parser_matches_go_jsonnets_refusals_exactly`].
//!
//! # No baseline file
//!
//! The corpus and lexer tests carry a `matching = N` ratchet because they
//! measure breadth over a formatter that is deliberately unfinished. This one
//! asserts **full parity**, because a parser that is right for 130 of 138 files
//! is not a partial formatter — it is a wrong parser, and every pass built on
//! it would inherit the error. Divergences are reported with their paths so the
//! failure says what to fix.

mod astdump;

use std::{collections::BTreeMap, fs};

use astdump::{DumpEntry, Snippet, compare, dump, read_oracle, repo_root, report};
use rtk_jsonnetfmt::parser::snippet_to_raw_ast;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileDump {
	source: String,
	#[serde(default)]
	entries: Vec<DumpEntry>,
	#[serde(default)]
	error: String,
}

const HOW: &str = "make update-fmt-node-oracle";

/// The first way this file disagrees with go-jsonnet, or `None`.
fn first_divergence(expected: &FileDump, input: &str) -> Option<String> {
	let parsed = snippet_to_raw_ast(&expected.source, input);

	if !expected.error.is_empty() {
		return match parsed {
			Ok((node, _)) => Some(format!(
				"go-jsonnet refused the file with {:?}, this port parsed it as {}",
				expected.error,
				node.kind.name()
			)),
			Err(err) if err.message() != expected.error => Some(format!(
				"error text differs\n    go-jsonnet: {:?}\n    rtk:        {:?}",
				expected.error,
				err.message()
			)),
			Err(_) => None,
		};
	}

	let (node, final_fodder) = match parsed {
		Ok(parsed) => parsed,
		Err(err) => {
			return Some(format!(
				"go-jsonnet parsed {} entries, this port refused the file with {:?}",
				expected.entries.len(),
				err.message()
			));
		}
	};

	compare(&expected.entries, &dump(&node, &final_fodder))
}

#[test]
fn the_parser_matches_go_jsonnet_on_every_corpus_file() {
	let Some(dumps) = read_oracle::<Vec<FileDump>>("node-oracle.json", HOW) else {
		return;
	};

	let root = repo_root();
	let mut divergences: BTreeMap<String, String> = BTreeMap::new();

	for dump in &dumps {
		let input = fs::read_to_string(root.join(&dump.source))
			.unwrap_or_else(|err| panic!("reading {}: {err}", dump.source));
		if let Some(detail) = first_divergence(dump, &input) {
			divergences.insert(dump.source.clone(), detail);
		}
	}

	assert!(
		divergences.is_empty(),
		"{}",
		report(&divergences, dumps.len(), "corpus files")
	);

	eprintln!(
		"node parity: all {} corpus files match go-jsonnet node for node",
		dumps.len()
	);
}

#[test]
fn the_parser_matches_go_jsonnet_on_every_snippet() {
	let Some(dumps) = read_oracle::<Vec<FileDump>>("node-snippet-oracle.json", HOW) else {
		return;
	};

	let snippets: Vec<Snippet> =
		read_oracle("node-snippets.json", HOW).expect("the snippets are committed");

	let names: Vec<&str> = snippets.iter().map(|s| s.name.as_str()).collect();
	let dumped: Vec<&str> = dumps.iter().map(|d| d.source.as_str()).collect();
	assert_eq!(names, dumped, "the snippet oracle is stale; re-run `{HOW}`");

	let mut divergences: BTreeMap<String, String> = BTreeMap::new();
	for (snippet, dump) in snippets.iter().zip(&dumps) {
		if let Some(detail) = first_divergence(dump, &snippet.source) {
			divergences.insert(snippet.name.clone(), detail);
		}
	}

	assert!(
		divergences.is_empty(),
		"{}",
		report(&divergences, snippets.len(), "snippets")
	);

	eprintln!(
		"node parity: all {} snippets match go-jsonnet node for node",
		snippets.len()
	);
}

/// Grades the parser's **refusals** — the message and its location together.
///
/// # Why this is its own family
///
/// [`the_parser_matches_go_jsonnet_on_every_snippet`] cannot carry these.
/// `tests/node_oracle.rs` asserts go-jsonnet refuses *none* of
/// `node-snippets.json`, and that assertion is right: a snippet it refuses pins
/// no fodder slot, so one that fails to parse is a snippet with a typo in it.
/// Here the refusal *is* the answer, so the requirement inverts — every snippet
/// must be refused, and that is asserted below.
///
/// # What it closes
///
/// `staticError.Error()` is `"{loc} {msg}"` and `nodedump`'s `dumpOne` records
/// exactly that, so this is the only place in the crate where a parser error's
/// **position** is compared against go-jsonnet's. Before it there were three,
/// and they were an accident: the three corpus files that happen not to parse.
///
/// The rest was resting on nothing. `src/parser.rs`'s `assert_message` helper
/// asserts the message *body* with `ends_with` and defers the location "to the
/// oracle" — but the node and pass snippet oracles hold **zero** error cells
/// between them, over 63 and 437 entries. So 26 of go-jsonnet's 29 parser-error
/// templates could have reported any line, any column, and any of
/// `LocationRange`'s three renderings (`l:c`, `l:c-c2`, `(l:c)-(l2:c2)`), with
/// every test in the suite staying green. The AST oracles cannot help: they
/// drop locations deliberately, because `internal/formatter` reads one in
/// exactly one place and discards it.
///
/// That matters because a parse failure aborts the **whole** `tk fmt` run
/// before the summary line, so this string is as much of the contract as the
/// formatted bytes are — and it is the output a user meets on the one day they
/// have a syntax error.
///
/// Each snippet's `why` names the upstream site it is meant to reach. If one
/// reaches a *different* site the snippet is still valid and still graded,
/// since the answer comes from go-jsonnet either way; only the `why` would be
/// wrong.
#[test]
fn the_parser_matches_go_jsonnets_refusals_exactly() {
	let Some(dumps) = read_oracle::<Vec<FileDump>>("parse-error-snippet-oracle.json", HOW) else {
		return;
	};

	let snippets: Vec<Snippet> =
		read_oracle("parse-error-snippets.json", HOW).expect("the snippets are committed");

	let names: Vec<&str> = snippets.iter().map(|s| s.name.as_str()).collect();
	let dumped: Vec<&str> = dumps.iter().map(|d| d.source.as_str()).collect();
	assert_eq!(names, dumped, "the snippet oracle is stale; re-run `{HOW}`");

	// The inverse of `node_oracle.rs`'s assertion, and not redundant with the
	// comparison below: `first_divergence` falls through to an AST comparison
	// when the recorded error is empty, so a snippet go-jsonnet accepted would
	// otherwise pass while grading no message at all.
	let accepted: Vec<&str> = dumps
		.iter()
		.filter(|dump| dump.error.is_empty())
		.map(|dump| dump.source.as_str())
		.collect();
	assert!(
		accepted.is_empty(),
		"go-jsonnet parsed {} snippet(s) that are here to be refused, so they grade no \
		 message:\n  {}",
		accepted.len(),
		accepted.join("\n  ")
	);

	let mut divergences: BTreeMap<String, String> = BTreeMap::new();
	for (snippet, dump) in snippets.iter().zip(&dumps) {
		if let Some(detail) = first_divergence(dump, &snippet.source) {
			divergences.insert(snippet.name.clone(), detail);
		}
	}

	assert!(
		divergences.is_empty(),
		"{}",
		report(&divergences, snippets.len(), "parse-error snippets")
	);

	eprintln!(
		"node parity: all {} refusals match go-jsonnet, message and location",
		snippets.len()
	);
}

/// Every parse-error snippet has to say which upstream site it reaches.
///
/// The inputs here are hand-written and the answers are not, so the `why` is
/// the only record of *intent* — without it a snippet that stops reaching its
/// site still passes, and the coverage quietly becomes whatever go-jsonnet
/// happens to say.
#[test]
fn every_parse_error_snippet_says_what_it_is_for() {
	let snippets: Vec<Snippet> =
		read_oracle("parse-error-snippets.json", HOW).expect("the snippets are committed");
	assert!(!snippets.is_empty(), "the snippets file is empty");

	for snippet in &snippets {
		assert!(
			!snippet.why.is_empty(),
			"{} has no `why`: a snippet has to name the upstream site it reaches",
			snippet.name
		);
	}
}
