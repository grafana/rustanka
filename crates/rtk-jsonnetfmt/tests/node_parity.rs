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
