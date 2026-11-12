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

use std::{
	collections::BTreeMap,
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
};

use rtk_jsonnetfmt::{Text, lexer::lex_bytes};
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

impl FileDump {
	fn read(path: &Path) -> Result<Vec<Self>, String> {
		let raw = fs::read(path).map_err(|err| format!("reading {}: {err}", path.display()))?;
		let dumps: Vec<Self> = serde_json::from_slice(&raw)
			.map_err(|err| format!("parsing {}: {err}", path.display()))?;
		if dumps.is_empty() {
			return Err(format!("{}: required oracle is empty", path.display()));
		}
		for dump in &dumps {
			if dump.source.is_empty() || dump.tokens.is_empty() == dump.error.is_empty() {
				return Err(format!(
					"{}: each source must have tokens or an error",
					path.display()
				));
			}
		}
		Ok(dumps)
	}
}

#[test]
fn required_oracles_fail_closed() {
	let dir = tempfile::tempdir().expect("temporary directory");
	let missing = dir.path().join("missing.json");
	assert!(FileDump::read(&missing).unwrap_err().contains("reading"));
	// A directory is an unreadable data file even when running as root.
	assert!(FileDump::read(dir.path()).unwrap_err().contains("reading"));
	let file = dir.path().join("oracle.json");
	for contents in ["", "{", "[]", "[{}]", r#"[{"source":"empty.jsonnet"}]"#] {
		fs::write(&file, contents).expect("write fixture");
		assert!(FileDump::read(&file).is_err(), "accepted {contents:?}");
	}
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenDump {
	#[serde(default)]
	data_hex: Option<String>,
	begin: [usize; 2],
	end: [usize; 2],
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
#[serde(rename_all = "camelCase")]
struct FodderElementDump {
	#[serde(default)]
	comment_hex: Option<Vec<String>>,
	kind: String,
	blanks: usize,
	indent: usize,
	#[serde(default)]
	comment: Vec<String>,
}

fn decode_hex(hex: &str) -> Vec<u8> {
	assert!(
		hex.len().is_multiple_of(2),
		"hex fixture must have complete bytes"
	);
	hex.as_bytes()
		.as_chunks::<2>()
		.0
		.iter()
		.map(|pair| {
			u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII hex"), 16)
				.expect("hex fixture")
		})
		.collect()
}

impl TokenDump {
	fn data_bytes(&self) -> Vec<u8> {
		self.data_hex
			.as_ref()
			.map_or_else(|| self.data.as_bytes().to_vec(), |hex| decode_hex(hex))
	}
}

impl FodderElementDump {
	fn comment_bytes(&self) -> Vec<Text> {
		self.comment_hex.as_ref().map_or_else(
			|| self.comment.iter().cloned().map(Text::from).collect(),
			|comments| {
				comments
					.iter()
					.map(|hex| Text::from(decode_hex(hex)))
					.collect()
			},
		)
	}
}

impl Snippet {
	fn source_bytes(&self) -> Vec<u8> {
		self.source_hex
			.as_ref()
			.map_or_else(|| self.source.as_bytes().to_vec(), |hex| decode_hex(hex))
	}
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
				element.kind,
				element.blanks,
				element.indent,
				element.comment_bytes()
			)
		})
		.collect()
}

/// The first way this file disagrees with go-jsonnet, or `None`.
fn first_divergence(dump: &FileDump, input: &[u8]) -> Option<String> {
	let actual = lex_bytes(&dump.source, input);

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
		let begin = [got.location.begin.line, got.location.begin.column];
		let end = [got.location.end.line, got.location.end.column];
		if begin != expected.begin || end != expected.end {
			return Some(format!(
				"{at}: location differs: go-jsonnet {:?}..{:?}, rtk {begin:?}..{end:?}",
				expected.begin, expected.end
			));
		}

		if got.kind.name() != expected.kind {
			return Some(format!(
				"{at}: kind differs: go-jsonnet {:?}, rtk {:?}",
				expected.kind,
				got.kind.name()
			));
		}
		if got.data.as_bytes() != expected.data_bytes() {
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
	let dumps = FileDump::read(&oracle_path).unwrap_or_else(|err| panic!("{err}"));

	let baseline_path = crate_dir().join("testdata/lexer-baseline.toml");
	let baseline: Baseline = toml::from_str(
		&fs::read_to_string(&baseline_path)
			.unwrap_or_else(|err| panic!("reading {}: {err}", baseline_path.display())),
	)
	.unwrap_or_else(|err| panic!("parsing {}: {err}", baseline_path.display()));

	let sources_path = crate_dir().join("testdata/lexer-sources.json");
	let sources: Vec<String> = serde_json::from_slice(
		&fs::read(&sources_path).expect("required lexer source manifest must be readable"),
	)
	.expect("lexer source manifest must be valid JSON");
	assert_eq!(
		sources,
		dumps
			.iter()
			.map(|dump| dump.source.clone())
			.collect::<Vec<_>>(),
		"lexer oracle sources must match the required manifest"
	);

	let root = repo_root();
	let mut matching = 0;
	let mut divergences: BTreeMap<String, String> = BTreeMap::new();

	for dump in &dumps {
		let input = fs::read(root.join(&dump.source))
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
	#[serde(default)]
	source: String,
	#[serde(default, rename = "sourceHex")]
	source_hex: Option<String>,
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
	let dumps = FileDump::read(&oracle_path).unwrap_or_else(|err| panic!("{err}"));

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
		if let Some(detail) = first_divergence(dump, &snippet.source_bytes()) {
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnescapeDump {
	name: String,
	input_hex: String,
	output_hex: String,
	error_hex: String,
}

#[test]
fn string_unescaping_matches_go_as_raw_bytes() {
	use rtk_jsonnetfmt::{
		location::{Location, LocationRange},
		string_util::string_unescape_bytes,
	};
	let data = crate_dir().join("testdata");
	let dumps: Vec<UnescapeDump> = serde_json::from_slice(
		&fs::read(data.join("string-oracle.json")).expect("required string oracle"),
	)
	.expect("valid string oracle");
	assert!(!dumps.is_empty(), "required string oracle is empty");
	let snippets: Vec<Snippet> = serde_json::from_slice(
		&fs::read(data.join("string-snippets.json")).expect("required string snippets"),
	)
	.expect("valid string snippets");
	assert_eq!(dumps.len(), snippets.len(), "string oracle is stale");
	let location = LocationRange::point("", Location::default());
	for (dump, snippet) in dumps.iter().zip(&snippets) {
		assert_eq!(dump.name, snippet.name);
		let input = decode_hex(&dump.input_hex);
		assert_eq!(
			input,
			snippet.source_bytes(),
			"string oracle input is stale"
		);
		match string_unescape_bytes(&location, &input) {
			Ok(value) => {
				assert!(dump.error_hex.is_empty(), "Go rejected {}", dump.name);
				assert_eq!(
					value.as_bytes(),
					decode_hex(&dump.output_hex),
					"{}",
					dump.name
				);
			}
			Err(error) => {
				assert!(!dump.error_hex.is_empty(), "Go accepted {}", dump.name);
				let mut stderr = Vec::new();
				error.write_to(&mut stderr).expect("write raw diagnostic");
				assert_eq!(stderr, decode_hex(&dump.error_hex), "{}", dump.name);
			}
		}
	}
}
