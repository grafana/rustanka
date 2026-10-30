//! Grades the lexer port against the tokens and fodder go-jsonnet's own lexer
//! produces.
//!
//! This is the sharpest oracle in the crate, and the reason it exists at all is
//! that the corpus is too far downstream: a lexer mistake only reaches text
//! after travelling through the AST, every pass and the unparser, by which
//! point the diff says nothing about where it came from. Fodder is invisible
//! from outside go-jsonnet — `internal/parser` is an internal package and
//! `token`'s fields are unexported — so `make update-fmt-lexer-oracle` stages a
//! test into a checkout and dumps the real values from inside.
//!
//! The ratchet matches [`super`]'s corpus test: a count that may only improve,
//! because a per-file list would be 138 lines of "the lexer is not finished".
//! Unlike the corpus, this one reports the **first divergence in detail** —
//! file, token index, and the two fodder values — since that is what you act
//! on.

use std::{collections::BTreeMap, fmt::Write as _, fs, path::PathBuf};

use rtk_jsonnetfmt::lexer::lex;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileDump {
	source: String,
	#[serde(default)]
	tokens: Vec<TokenDump>,
	/// The lexer's message when it refused the file.
	#[serde(default)]
	error: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenDump {
	kind: String,
	#[serde(default)]
	data: String,
	#[serde(default)]
	fodder: Vec<FodderElementDump>,
	#[serde(default)]
	string_block_indent: String,
	#[serde(default)]
	string_block_term_indent: String,
}

#[derive(Debug, Deserialize)]
struct FodderElementDump {
	kind: String,
	blanks: usize,
	indent: usize,
	#[serde(default)]
	comment: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Baseline {
	matching: usize,
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

/// Render a dumped element in the same notation `FodderElement::describe` uses,
/// so a mismatch reads as a diff of two comparable things rather than of two
/// different notations.
///
/// Both halves are rendered on this side deliberately: the Go dumpers carry
/// fodder structurally, so no difference between Go's string escaping and
/// Rust's can be mistaken for a difference in fodder.
fn describe_dumped_fodder(fodder: &[FodderElementDump]) -> Vec<String> {
	fodder
		.iter()
		.map(|element| {
			format!(
				"{}(blanks={}, indent={}, comment={:?})",
				element.kind, element.blanks, element.indent, element.comment
			)
		})
		.collect()
}

/// The first way this file disagrees with go-jsonnet, or `None`.
fn first_divergence(dump: &FileDump, input: &str) -> Option<String> {
	let actual = lex(&dump.source, input);

	if !dump.error.is_empty() {
		return match actual {
			Ok(tokens) => Some(format!(
				"go-jsonnet refused the file with {:?}, this port lexed {} tokens",
				dump.error,
				tokens.len()
			)),
			Err(err) if err.message() != dump.error => Some(format!(
				"error text differs\n    go-jsonnet: {:?}\n    rtk:        {:?}",
				dump.error,
				err.message()
			)),
			Err(_) => None,
		};
	}

	let tokens = match actual {
		Ok(tokens) => tokens,
		Err(err) => {
			return Some(format!(
				"go-jsonnet lexed {} tokens, this port refused the file with {:?}",
				dump.tokens.len(),
				err.message()
			));
		}
	};

	if tokens.len() != dump.tokens.len() {
		return Some(format!(
			"token count differs: go-jsonnet {}, rtk {}",
			dump.tokens.len(),
			tokens.len()
		));
	}

	for (index, (expected, got)) in dump.tokens.iter().zip(&tokens).enumerate() {
		let at = format!("token {index}");
		if got.kind.name() != expected.kind {
			return Some(format!(
				"{at}: kind differs: go-jsonnet {:?}, rtk {:?}",
				expected.kind,
				got.kind.name()
			));
		}
		if got.data != expected.data {
			return Some(format!(
				"{at} ({}): data differs\n    go-jsonnet: {:?}\n    rtk:        {:?}",
				expected.kind, expected.data, got.data
			));
		}

		let expected_fodder = describe_dumped_fodder(&expected.fodder);
		let got_fodder = got.fodder.describe();
		if expected_fodder != got_fodder {
			return Some(format!(
				"{at} ({}): fodder differs\n    go-jsonnet: {expected_fodder:?}\n    rtk:        {got_fodder:?}",
				expected.kind
			));
		}

		if got.string_block_indent != expected.string_block_indent
			|| got.string_block_term_indent != expected.string_block_term_indent
		{
			return Some(format!(
				"{at}: block string indents differ\n    go-jsonnet: {:?} / {:?}\n    rtk:        {:?} / {:?}",
				expected.string_block_indent,
				expected.string_block_term_indent,
				got.string_block_indent,
				got.string_block_term_indent
			));
		}
	}

	None
}

#[test]
fn lexer_matches_go_jsonnet_at_the_recorded_rate() {
	let oracle_path = crate_dir().join("testdata/lexer-oracle.json");
	let Ok(raw) = fs::read_to_string(&oracle_path) else {
		eprintln!(
			"no lexer oracle at {}; generate it with `make update-fmt-lexer-oracle` (needs Go). \
			 Skipping.",
			oracle_path.display()
		);
		return;
	};

	let dumps: Vec<FileDump> = serde_json::from_str(&raw).expect("the oracle is valid JSON");
	assert!(!dumps.is_empty(), "the oracle is empty");

	let baseline_path = crate_dir().join("testdata/lexer-baseline.toml");
	let baseline: Baseline = toml::from_str(
		&fs::read_to_string(&baseline_path)
			.unwrap_or_else(|err| panic!("reading {}: {err}", baseline_path.display())),
	)
	.unwrap_or_else(|err| panic!("parsing {}: {err}", baseline_path.display()));

	let root = repo_root();
	let mut matching = 0;
	let mut divergences: BTreeMap<String, String> = BTreeMap::new();

	for dump in &dumps {
		let input = fs::read_to_string(root.join(&dump.source))
			.unwrap_or_else(|err| panic!("reading {}: {err}", dump.source));

		match first_divergence(dump, &input) {
			None => matching += 1,
			Some(detail) => {
				divergences.insert(dump.source.clone(), detail);
			}
		}
	}

	let total = dumps.len();
	if matching != baseline.matching {
		// Detail for a few, because the point of this oracle is to say *where*
		// the lexer went wrong, not how often.
		const SAMPLE: usize = 5;
		let mut report = String::new();
		for (source, detail) in divergences.iter().take(SAMPLE) {
			let _ = write!(report, "\n  {source}\n    {detail}\n");
		}
		if let Some(elided) = divergences.len().checked_sub(SAMPLE).filter(|n| *n > 0) {
			let _ = write!(report, "\n  ...and {elided} more files\n");
		}

		panic!(
			"lexer match count is {matching} of {total}, but \
			 testdata/lexer-baseline.toml records {}.\n\
			 \n\
			 If {matching} is higher, set `matching = {matching}` in the same commit.\n\
			 If it is lower, something regressed.\n\
			 {report}",
			baseline.matching
		);
	}

	eprintln!("lexer oracle: {matching} of {total} files match go-jsonnet token for token");
}

#[derive(Debug, Deserialize)]
struct Snippet {
	name: String,
	source: String,
}

/// The snippets, which cover what the corpus cannot reach.
///
/// The 138 corpus files hold no verbatim string and provoke no lexer error, so
/// without this every error message and both verbatim paths would be pinned by
/// expectations derived by hand from Go's source. These take their answers from
/// go-jsonnet instead, the same as everything else.
///
/// Unlike the corpus there is no baseline: the whole set is expected to match,
/// and a snippet exists precisely because someone wanted a behaviour pinned.
#[test]
fn snippets_match_go_jsonnet_exactly() {
	let oracle_path = crate_dir().join("testdata/lexer-snippet-oracle.json");
	let Ok(raw) = fs::read_to_string(&oracle_path) else {
		eprintln!(
			"no snippet oracle at {}; generate it with `make update-fmt-lexer-oracle` (needs Go). \
			 Skipping.",
			oracle_path.display()
		);
		return;
	};

	let dumps: Vec<FileDump> =
		serde_json::from_str(&raw).expect("the snippet oracle is valid JSON");

	let snippets_path = crate_dir().join("testdata/lexer-snippets.json");
	let snippets: Vec<Snippet> = serde_json::from_str(
		&fs::read_to_string(&snippets_path)
			.unwrap_or_else(|err| panic!("reading {}: {err}", snippets_path.display())),
	)
	.expect("the snippets file is valid JSON");

	// If these disagree the oracle is stale, and comparing them would silently
	// grade the wrong sources.
	let names: Vec<&str> = snippets.iter().map(|s| s.name.as_str()).collect();
	let dumped: Vec<&str> = dumps.iter().map(|d| d.source.as_str()).collect();
	assert_eq!(
		names, dumped,
		"the snippet oracle is stale; re-run `make update-fmt-lexer-oracle`"
	);

	let mut failures: Vec<String> = Vec::new();
	for (snippet, dump) in snippets.iter().zip(&dumps) {
		if let Some(detail) = first_divergence(dump, &snippet.source) {
			failures.push(format!("{}\n    {detail}", snippet.name));
		}
	}

	assert!(
		failures.is_empty(),
		"{} of {} snippets diverge from go-jsonnet:\n  {}",
		failures.len(),
		snippets.len(),
		failures.join("\n  ")
	);

	eprintln!(
		"lexer snippets: all {} match go-jsonnet token for token",
		snippets.len()
	);
}
