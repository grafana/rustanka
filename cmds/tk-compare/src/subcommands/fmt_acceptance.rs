//! Phase 5's acceptance gate for `rtk fmt`: real Grafana Jsonnet, measured.
//!
//! `docs/rtk-fmt-plan.md`'s Phase 5 asks for four things, and this is all four
//! in one pass over one corpus, because they are four readings of the same
//! formatting run and splitting them would mean formatting a few thousand files
//! four times:
//!
//! 1. **Parity.** For every file, `tk fmt -` and `rtk fmt -` must agree on
//!    stdout, stderr and the exit code, byte for byte.
//! 2. **The already-formatted gate**, which is the one that matters to a user:
//!    over a corpus tk has already formatted, `rtk fmt --test` must exit 0 and
//!    list nothing. Strictly stronger than parity, which can pass while both
//!    tools rewrite every file.
//! 3. **The destructive mechanisms, measured over outputs.** Every formatted
//!    result is handed back to the parser, and every file whose output the
//!    parser refuses is named. `CLAUDE.md` records twice that a grep over
//!    inputs is the wrong instrument — Phase 4 predicted five non-convergent
//!    snippets by grepping for `((` and missed six whose shape is `(\n  (1)`.
//!    An instrument that reads outputs finds mechanisms nobody has thought of;
//!    a pattern search can only find the ones already known.
//! 4. **Discovery over a real tree**, which nothing else grades: the per-file
//!    comparison above deliberately says nothing about which files were found
//!    or in what order, so each root is also put through
//!    `fmt --test --verbose` in both tools and the listings compared.
//!
//! # Why `-` and not in-place formatting
//!
//! The plan offered two routes: a `[[tests]]` entry running `fmt` in place with
//! `workspace = true` plus a directory comparison, or comparing the two tools'
//! `--stdout` streams. This takes the second, one step further — per file
//! through `-` rather than per tree through `--stdout` — for four reasons, in
//! descending order of how much they matter:
//!
//! - **A parse failure aborts the whole run.** `tk fmt` and `rtk fmt` both
//!   return on the first file the parser refuses, so one bad file in a corpus
//!   of thousands truncates a whole-tree comparison to however many files
//!   preceded it — and the two tools would *agree* about aborting, so the gate
//!   would pass having compared almost nothing. That is precisely the shape
//!   this phase was told to design against. Per file, a refusal is one file's
//!   verdict and the denominator stays whole.
//! - **Attribution.** A directory comparison after in-place formatting says a
//!   tree differs. This says which file, and holds both answers.
//! - **Nothing is written to the corpus**, so the parity measurement cannot
//!   contaminate the already-formatted one. In-place mode also **rewrites every
//!   discovered file whether it changed or not**, so a directory comparison
//!   cannot tell "both formatters agreed" from "neither changed anything".
//! - `-` has no wrapper text at all: no `// {name}` header, no per-file blank
//!   line on stderr, no summary. The bytes on stdout are the formatted file.
//!
//! What the per-file route gives up is discovery, and item 4 buys it back.
//!
//! # The already-formatted property is measured twice, on purpose
//!
//! Per file it is an identity rather than a separate run: a file is already
//! formatted exactly when formatting tk's own output again changes nothing, so
//! `already_formatted`, `non_convergent` and `refusing_to_reparse` are the three
//! outcomes of one second run and must sum to `formatted`. The harness checks
//! that sum, because a counter that stopped being incremented would otherwise
//! read as good news.
//!
//! Then it is measured again end to end, through the real binary: tk's output
//! for every formattable file is staged into a tree, and `rtk fmt --test
//! --verbose` is run over it. That run has to exit 0, print the clean summary on
//! stderr, and print **one `ok  ` line per staged file** on stdout — the last of
//! those being the assertion that it graded the tree rather than an empty
//! directory.
//!
//! # What this refuses to do
//!
//! **Skip, or pass unmeasured.** A missing corpus is an error naming the
//! Makefile target. A corpus smaller than [`MINIMUM_FILES`] is a broken
//! checkout rather than a small corpus. A baseline count left at `-1` is a
//! failure, not a default of zero. Both `[known]` lists ratchet in both
//! directions, as `tests/corpus.rs`'s two lists do. And every count is printed
//! on stdout on every run, pass or fail, because a number printed only on
//! failure is invisible in a green log.

use std::{
	collections::BTreeSet,
	fmt::Write as _,
	fs,
	io::Write as _,
	path::{Path, PathBuf},
	process::{Command, Stdio},
	sync::Mutex,
};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use rtk_jsonnetfmt::Options;
use serde::Deserialize;

use crate::{
	cli::{FmtAcceptanceCli, GlobalOptions},
	constants::{RTK_EXEC_NAME, TK_EXEC_NAME},
};

/// Below this, the corpus is a broken checkout rather than a small corpus.
///
/// The baseline ratchet catches a shrinking corpus once it has been filled in,
/// but it cannot catch the *first* run measuring almost nothing — and the most
/// likely way to get there is mundane: a clone that produced an empty
/// directory, or a corpus staged under a dotted path, which the `**/.*` exclude
/// would then drop entirely. This makes that state loud.
const MINIMUM_FILES: usize = 1_000;

/// What `rtk fmt` prints on stderr when it changed nothing.
///
/// Written out rather than derived from the command, because deriving an
/// expectation from the code under test is how a test comes to assert only that
/// the code does what it does.
const CLEAN_SUMMARY: &str = "All discovered files are already formatted. No changes were made\n";

/// How many findings a failure report prints before eliding.
const SAMPLE: usize = 40;

#[derive(Debug, Deserialize)]
struct AcceptanceConfig {
	repos: Vec<Repo>,
	discovery: Discovery,
	baseline: Baseline,
	#[serde(default)]
	known: KnownConfig,
}

#[derive(Debug, Deserialize)]
struct Repo {
	name: String,
	url: String,
	/// What to clone. `ref` is a Rust keyword.
	#[serde(rename = "ref")]
	reference: String,
	/// The commit the recorded numbers answer for, or empty until first
	/// measured. Empty is reported as `UNPINNED`; set and mismatched fails.
	#[serde(default)]
	rev: String,
}

#[derive(Debug, Deserialize)]
struct Discovery {
	exclude: Vec<String>,
}

/// The two classes of second-run outcome that are allowed to be non-empty, by
/// name, the way `tests/corpus.rs` keeps `KNOWN_NON_CONVERGENT` and
/// `OUTPUT_DOES_NOT_REPARSE`.
///
/// Kept apart from one another for the reason that file gives: a file that
/// formats to a different file is unsettled, while a file that formats to one
/// the parser refuses is destroyed, and `tk fmt` writes in place. One list would
/// let the worse class hide inside the ordinary one.
#[derive(Debug, Default, Deserialize)]
struct KnownConfig {
	#[serde(default)]
	output_does_not_reparse: Vec<String>,
	#[serde(default)]
	non_convergent: Vec<String>,
}

/// The recorded numbers, ratcheted exactly in both directions.
///
/// `i64` rather than `usize` so that `-1` can mean "never measured" and fail.
/// An absent key defaulting to `0` would let an unmeasured gate go green, and
/// zero is a legitimate value for five of these.
#[derive(Debug, Deserialize)]
struct Baseline {
	files: i64,
	vendor_files: i64,
	matching: i64,
	matching_formatted: i64,
	formatted: i64,
	parse_errors: i64,
	error_text_matching: i64,
	already_formatted: i64,
	non_convergent: i64,
	refusing_to_reparse: i64,
	non_utf8: i64,
	roots_with_matching_discovery: i64,
}

/// One root of the corpus: a pinned clone, or a locally supplied extra.
struct Root {
	name: String,
	path: PathBuf,
	/// `None` for an extra root, which carries no provenance and is reported
	/// separately for exactly that reason.
	origin: Option<Origin>,
}

struct Origin {
	url: String,
	reference: String,
	/// Declared in the config, or empty.
	declared_rev: String,
	/// What the clone actually resolved to, from `<name>.rev`.
	cloned_rev: String,
}

/// Everything one file contributed.
#[derive(Default)]
struct Tally {
	files: usize,
	vendor_files: usize,
	/// Every observable agrees: stdout, stderr and the exit code. The strict
	/// number, over every file that was run.
	matching: usize,
	/// Of `formatted`, how many agree. **This is the formatter's parity
	/// number**, and it is kept apart from `matching` because the two answer
	/// different questions: a file neither tool can parse contributes an error
	/// message to `matching`, and rtk's error text is anyhow's while tk's is
	/// go-clix's. Conflating them would report a CLI difference as a formatter
	/// difference, on files where the formatter was never reached.
	matching_formatted: usize,
	formatted: usize,
	parse_errors: usize,
	/// Of `parse_errors`, how many agree on stderr and the exit code. The one
	/// thing here that grades a `fmt -` refusal against tk's, which nothing in
	/// `cmds/rtk/tests/fmt_parity_test.rs` does — its parse-failure test
	/// asserts `contains`, and none of its fifteen tk scenarios feeds in a file
	/// the parser refuses.
	error_text_matching: usize,
	already_formatted: usize,
	non_convergent: usize,
	refusing_to_reparse: usize,
	non_utf8: usize,
	/// tk formatted it and rtk refused, or the reverse, or the file could not
	/// be read or run at all. Counted so that the terminal buckets provably
	/// cover every discovered file.
	split_verdict: usize,
}

impl Tally {
	fn merge(mut self, other: Self) -> Self {
		self.files += other.files;
		self.vendor_files += other.vendor_files;
		self.matching += other.matching;
		self.matching_formatted += other.matching_formatted;
		self.formatted += other.formatted;
		self.parse_errors += other.parse_errors;
		self.error_text_matching += other.error_text_matching;
		self.already_formatted += other.already_formatted;
		self.non_convergent += other.non_convergent;
		self.refusing_to_reparse += other.refusing_to_reparse;
		self.non_utf8 += other.non_utf8;
		self.split_verdict += other.split_verdict;
		self
	}
}

/// A subprocess result, captured as bytes because the comparison is one.
struct Run {
	stdout: Vec<u8>,
	stderr: Vec<u8>,
	code: Option<i32>,
	/// `Display` of the status, which names a signal where there is no code —
	/// a stack overflow reports one of those.
	status: String,
}

/// What every worker shares.
///
/// One struct rather than eight parameters, and the two `Mutex`es are what make
/// the per-file pass reportable: `findings` collects everything a human has to
/// read, and `observed_known` records which `[known]` entries were actually
/// reached, so a listed file that has since been deleted upstream fails as
/// loudly as an unlisted one that starts failing.
struct Harness<'a> {
	tk: &'a str,
	rtk: &'a str,
	staged: &'a Path,
	known: &'a KnownConfig,
	findings: Mutex<Vec<String>>,
	observed_known: Mutex<BTreeSet<String>>,
}

impl Harness<'_> {
	fn note(&self, text: String) {
		self.findings
			.lock()
			.expect("the mutex is not poisoned")
			.push(text);
	}

	fn observed(&self, key: &str) {
		self.observed_known
			.lock()
			.expect("the mutex is not poisoned")
			.insert(key.to_owned());
	}
}

pub fn execute(cli: FmtAcceptanceCli, global: &GlobalOptions) -> Result<()> {
	let config = load_config(&cli.config)?;
	let tk = global.tk.clone().unwrap_or_else(|| TK_EXEC_NAME.to_owned());
	let rtk = global
		.rtk
		.clone()
		.unwrap_or_else(|| RTK_EXEC_NAME.to_owned());
	runnable(&tk)?;
	runnable(&rtk)?;

	let roots = resolve_roots(&config, &cli)?;
	let excludes = rtk_gobwas_glob::compile_all(&config.discovery.exclude)
		.context("compiling the discovery excludes from the acceptance config")?;

	println!("fmt acceptance corpus");
	for root in &roots {
		match &root.origin {
			Some(origin) => println!(
				"  {:<14} {} @ {} ({})",
				root.name,
				origin.url,
				origin.reference,
				if origin.cloned_rev.is_empty() {
					"rev unknown"
				} else {
					origin.cloned_rev.as_str()
				}
			),
			None => println!(
				"  {:<14} {} (extra root, outside the ratchet)",
				root.name,
				root.path.display()
			),
		}
	}

	// Discovery per root, so the printed table says where the denominator came
	// from. `find_files_all` is Tanka's own `FindFiles`, so this is the walk
	// `tk fmt` would do with these excludes.
	let mut discovered: Vec<(usize, String)> = Vec::new();
	for (index, root) in roots.iter().enumerate() {
		let files = rtk_jsonnetfmt::find_files_all([root.path.to_string_lossy()], &excludes)
			.with_context(|| format!("discovering Jsonnet under {}", root.path.display()))?;
		println!("  {:<14} {} files", root.name, files.len());
		discovered.extend(files.into_iter().map(|file| (index, file)));
	}

	let pinned_files = discovered
		.iter()
		.filter(|(index, _)| roots[*index].origin.is_some())
		.count();
	println!();
	println!(
		"discovered {} files: {pinned_files} from pinned repositories, {} from extra roots",
		discovered.len(),
		discovered.len() - pinned_files
	);
	if pinned_files < MINIMUM_FILES {
		bail!(
			"the pinned corpus holds {pinned_files} files, below the floor of {MINIMUM_FILES}. \
			 That is a broken checkout rather than a small corpus — run \
			 `make fmt-acceptance-corpus`, and check that the corpus directory is not itself \
			 under a dotted path, which the `**/.*` exclude would drop entirely."
		);
	}

	let staged = prepare_staging(&cli.staged_dir)?;
	let harness = Harness {
		tk: &tk,
		rtk: &rtk,
		staged: &staged,
		known: &config.known,
		findings: Mutex::new(Vec::new()),
		observed_known: Mutex::new(BTreeSet::new()),
	};

	let tally = match cli.jobs {
		Some(jobs) => rayon::ThreadPoolBuilder::new()
			.num_threads(jobs)
			.build()
			.context("building the worker pool")?
			.install(|| measure_all(&discovered, &roots, &harness)),
		None => measure_all(&discovered, &roots, &harness),
	};

	let tree_gate = run_tree_gate(&staged, &rtk, &config.discovery.exclude, tally.formatted);
	let (discovery_agreement, pinned_roots) =
		compare_discovery(&roots, &tk, &rtk, &config.discovery.exclude, &harness);

	let mut findings = harness
		.findings
		.into_inner()
		.expect("the mutex is not poisoned");
	findings.sort_unstable();
	let observed_known = harness
		.observed_known
		.into_inner()
		.expect("the mutex is not poisoned");

	// Printed before anything is asserted, so a failing run still reports every
	// number rather than only the first one that moved.
	report(&tally, discovery_agreement, pinned_roots, &staged);

	let mut failures = String::new();
	check_internal_consistency(&tally, &mut failures);
	if let Err(error) = tree_gate {
		writeln!(failures, "\nalready-formatted gate: {error:#}")
			.expect("writing to a String cannot fail");
	}
	check_revisions(&roots, &mut failures);
	check_known_lists(&config.known, &observed_known, &mut failures);

	// The **count ratchet** is only enforced when the corpus is exactly the
	// pinned one, because every count above includes the extras and a
	// denominator that depends on whose laptop is running cannot be asserted in
	// both places. Everything else — parity, the tree gate, the refusals — is
	// enforced either way, those being properties of the formatter rather than
	// of the corpus's size. The advisory state is said out loud rather than
	// implied by a missing line.
	if cli.extra_root.is_empty() {
		check_baseline(&config.baseline, &tally, discovery_agreement, &mut failures);
	} else {
		println!(
			"\nADVISORY: {} extra root(s) were given, so the recorded counts are reported above \
			 and not enforced for this run. Parity, the already-formatted gate and the reparse \
			 check are enforced as usual.",
			cli.extra_root.len()
		);
	}

	if !findings.is_empty() {
		writeln!(failures, "\n{} file(s) with a finding:", findings.len())
			.expect("writing to a String cannot fail");
		for finding in findings.iter().take(SAMPLE) {
			writeln!(failures, "  {finding}").expect("writing to a String cannot fail");
		}
		if let Some(elided) = findings
			.len()
			.checked_sub(SAMPLE)
			.filter(|count| *count > 0)
		{
			writeln!(failures, "  ...and {elided} more").expect("writing to a String cannot fail");
		}
	}

	if failures.is_empty() {
		println!("\nfmt acceptance: green.");
		return Ok(());
	}
	bail!("fmt acceptance failed.{failures}");
}

fn measure_all(discovered: &[(usize, String)], roots: &[Root], harness: &Harness<'_>) -> Tally {
	discovered
		.par_iter()
		.map(|(index, file)| measure_file(&roots[*index], file, harness))
		.reduce(Tally::default, Tally::merge)
}

fn load_config(path: &str) -> Result<AcceptanceConfig> {
	let contents =
		fs::read_to_string(path).with_context(|| format!("reading the config: {path}"))?;
	toml::from_str(&contents).with_context(|| format!("parsing the config: {path}"))
}

/// Refuse early rather than reporting thousands of spawn failures as findings.
///
/// What is asserted is that the binary can be *spawned*, not what it says: `tk`
/// prints its version through Go's `log` package and to stderr, and asserting
/// on that belongs in `.github/actions/install-tk`, which does it.
fn runnable(executable: &str) -> Result<()> {
	Command::new(executable)
		.arg("--version")
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.status()
		.with_context(|| format!("`{executable} --version` — is it on PATH?"))?;
	Ok(())
}

fn resolve_roots(config: &AcceptanceConfig, cli: &FmtAcceptanceCli) -> Result<Vec<Root>> {
	let corpus = PathBuf::from(&cli.corpus_dir);
	let mut roots = Vec::new();

	for repo in &config.repos {
		let path = corpus.join(&repo.name);
		if !path.is_dir() {
			bail!(
				"{} is missing. Run `make fmt-acceptance-corpus` to clone the pinned \
				 repositories; there is no committed copy of this corpus and there cannot be \
				 one — see the provenance section of {}.",
				path.display(),
				cli.config
			);
		}
		// Written by the clone target. Absent means an older or hand-made
		// checkout, which is reported rather than guessed at.
		let cloned_rev = fs::read_to_string(corpus.join(format!("{}.rev", repo.name)))
			.map(|text| text.trim().to_owned())
			.unwrap_or_default();
		roots.push(Root {
			name: repo.name.clone(),
			path,
			origin: Some(Origin {
				url: repo.url.clone(),
				reference: repo.reference.clone(),
				declared_rev: repo.rev.clone(),
				cloned_rev,
			}),
		});
	}

	for (index, extra) in cli.extra_root.iter().enumerate() {
		let path = PathBuf::from(extra);
		if !path.is_dir() {
			bail!("--extra-root {extra} is not a directory");
		}
		roots.push(Root {
			name: format!("extra-{}", index + 1),
			path,
			origin: None,
		});
	}

	Ok(roots)
}

/// A predictable, inspectable staging tree rather than a temporary one.
///
/// It holds tk's answer for every formattable file, which is what the
/// already-formatted gate runs over — and what a failing gate has to be
/// inspected in. A temporary directory deleted on the way out would leave a
/// failure with nothing to look at.
fn prepare_staging(directory: &str) -> Result<PathBuf> {
	let path = PathBuf::from(directory);
	if path.exists() {
		fs::remove_dir_all(&path)
			.with_context(|| format!("clearing the staging tree at {}", path.display()))?;
	}
	fs::create_dir_all(&path)
		.with_context(|| format!("creating the staging tree at {}", path.display()))?;
	Ok(path)
}

/// Whether any component of the path is `vendor`.
///
/// Reported rather than assumed: the last comment in this repository to claim
/// vendored coverage was wrong about it, so this is measured.
fn is_vendored(file: &str) -> bool {
	file.split('/').any(|component| component == "vendor")
}

/// A file's stable name: `<root>/<path below that root>`.
///
/// Stable is the point. The discovered path depends on `--corpus-dir`, so using
/// it as the key in `[known]` would make those lists break when the corpus is
/// cloned somewhere else. This is also the layout the staging tree uses, so a
/// finding names a path that exists in both places.
fn key_for(root: &Root, file: &str) -> String {
	let relative = Path::new(file)
		.strip_prefix(&root.path)
		.map(Path::to_path_buf)
		.unwrap_or_else(|_| PathBuf::from(file.trim_start_matches('/')));
	format!("{}/{}", root.name, relative.to_string_lossy())
}

fn measure_file(root: &Root, file: &str, harness: &Harness<'_>) -> Tally {
	let key = key_for(root, file);
	let mut tally = Tally {
		files: 1,
		vendor_files: usize::from(is_vendored(file)),
		..Tally::default()
	};

	let bytes = match fs::read(file) {
		Ok(bytes) => bytes,
		Err(error) => {
			harness.note(format!("{key}: unreadable: {error}"));
			tally.split_verdict += 1;
			return tally;
		}
	};

	// Go reads bytes and its strings tolerate invalid UTF-8; `rtk fmt` reads
	// through `read_to_string` and refuses. That is a real divergence rather
	// than a curiosity, so it is counted and named rather than skipped.
	if std::str::from_utf8(&bytes).is_err() {
		harness.note(format!(
			"{key}: not UTF-8. `tk fmt` formats it and `rtk fmt` refuses it — a divergence, not \
			 a corpus problem."
		));
		tally.non_utf8 += 1;
		return tally;
	}

	let (theirs, ours) = match (
		run_with_stdin(harness.tk, &["fmt", "-"], &bytes),
		run_with_stdin(harness.rtk, &["fmt", "-"], &bytes),
	) {
		(Ok(theirs), Ok(ours)) => (theirs, ours),
		(theirs, ours) => {
			let error = theirs
				.err()
				.or_else(|| ours.err())
				.expect("one of the two failed");
			harness.note(format!("{key}: could not be run: {error:#}"));
			tally.split_verdict += 1;
			return tally;
		}
	};

	let agreed =
		theirs.stdout == ours.stdout && theirs.stderr == ours.stderr && theirs.code == ours.code;
	if agreed {
		tally.matching += 1;
	} else {
		harness.note(format!(
			"{key}: tk and rtk disagree.\n      stdout {}\n      stderr {}\n      exit   {}",
			difference(&theirs.stdout, &ours.stdout),
			difference(&theirs.stderr, &ours.stderr),
			if theirs.code == ours.code {
				"same".to_owned()
			} else {
				format!(
					"tk {:?} ({}) / rtk {:?} ({})",
					theirs.code, theirs.status, ours.code, ours.status
				)
			}
		));
	}

	match (theirs.code, ours.code) {
		(Some(0), Some(0)) => {}
		(Some(0), _) | (_, Some(0)) => {
			// One formatted and the other refused. Already noted above, since
			// the exit codes differ; counted here so the buckets stay total.
			tally.split_verdict += 1;
			return tally;
		}
		_ => {
			// Both refused: real Jsonnet go-jsonnet's parser will not take. Not
			// a failure on its own — and if the two messages differ, the
			// disagreement above has already said so.
			tally.parse_errors += 1;
			tally.error_text_matching += usize::from(agreed);
			return tally;
		}
	}

	let Ok(formatted) = String::from_utf8(theirs.stdout) else {
		harness.note(format!(
			"{key}: tk's output is not UTF-8 although its input was, which nothing explains"
		));
		// Unreachable in practice, and the decrement keeps
		// `matching == matching_formatted + error_text_matching` exact rather
		// than approximately true — an invariant with one excused exception is
		// an invariant nobody trusts.
		tally.matching -= usize::from(agreed);
		tally.split_verdict += 1;
		return tally;
	};
	tally.formatted += 1;
	tally.matching_formatted += usize::from(agreed);

	// `stage` keeps tk's answer for the tree-level gate.
	if let Err(error) = stage(harness.staged, &key, formatted.as_bytes()) {
		harness.note(format!("{key}: could not be staged: {error:#}"));
	}

	second_run(&key, file, &formatted, harness, &mut tally);
	tally
}

/// Item 3, over the output rather than over the input.
///
/// The diagnostic filename handed to `format` is the file's own path rather than
/// `<stdin>`, so a refusal's message names where it came from — and the
/// *location* in that message is part of what has to be confirmed against tk
/// before anything is added to `[known]`.
fn second_run(key: &str, file: &str, formatted: &str, harness: &Harness<'_>, tally: &mut Tally) {
	let listed_refusing = harness
		.known
		.output_does_not_reparse
		.iter()
		.any(|listed| listed == key);
	let listed_moving = harness
		.known
		.non_convergent
		.iter()
		.any(|listed| listed == key);
	if listed_refusing || listed_moving {
		harness.observed(key);
	}

	match rtk_jsonnetfmt::format(file, formatted, &Options::default()) {
		Err(error) => {
			tally.refusing_to_reparse += 1;
			if !listed_refusing {
				harness.note(format!(
					"{key}: **formatted output does not parse**: {}\n      This is the class \
					 that destroys a file rather than unsettling it, and `tk fmt` writes in \
					 place. Confirm it against the real tk on both runs — the refusal, its \
					 message and its location — name the mechanism in CLAUDE.md, and only then \
					 list it under [known].output_does_not_reparse.",
					error.message()
				));
			}
		}
		Ok(again) if again == formatted => {
			tally.already_formatted += 1;
			if listed_refusing {
				harness.note(format!(
					"{key} is listed under [known].output_does_not_reparse but its output now \
					 parses and settles. Upstream has moved, or this port has: delete the entry, \
					 and the CLAUDE.md paragraph with it."
				));
			} else if listed_moving {
				harness.note(format!(
					"{key} is listed under [known].non_convergent but now settles. Delete the \
					 entry."
				));
			}
		}
		Ok(_) => {
			tally.non_convergent += 1;
			if !listed_moving {
				harness.note(format!(
					"{key}: formatting the formatted output changes it again. Do not answer this \
					 with a convergence loop — name the two steps of FormatNode's order that \
					 disagree, and list it under [known].non_convergent."
				));
			}
		}
	}
}

/// Write tk's answer into the staging tree, under the file's stable key.
///
/// Keeping `vendor/` in the staged path matters: the tree-level gate runs a real
/// `rtk fmt` over this tree, and a staged layout that flattened vendor away
/// would grade a different discovery than the corpus has.
fn stage(staged: &Path, key: &str, contents: &[u8]) -> Result<()> {
	let destination = staged.join(key);
	if let Some(parent) = destination.parent() {
		fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
	}
	fs::write(&destination, contents).with_context(|| format!("writing {}", destination.display()))
}

/// The already-formatted gate, end to end through the real binary.
///
/// Three things are required and the third is what stops this passing while
/// measuring nothing: exit 0, the clean summary on stderr, and **one `ok  ` line
/// per staged file** on stdout. Without the third, an empty staging tree would
/// satisfy the first two.
///
/// The expected stderr carries a leading blank line because `--verbose` prints
/// one after the loop — the one part of `--verbose` that is not on stdout.
fn run_tree_gate(staged: &Path, rtk: &str, excludes: &[String], expected: usize) -> Result<()> {
	let arguments = fmt_test_arguments(excludes, ".");
	let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
	let run = run_in(rtk, staged, &borrowed).context("running the already-formatted gate")?;

	let stdout = String::from_utf8_lossy(&run.stdout);
	let stderr = String::from_utf8_lossy(&run.stderr);
	let lines: Vec<&str> = stdout.lines().collect();

	if run.code != Some(0) {
		bail!(
			"`rtk fmt --test` over tk's own output exited {:?} ({}), so rtk would rewrite files \
			 tk had already formatted. The staging tree is at {}. stderr:\n{stderr}",
			run.code,
			run.status,
			staged.display()
		);
	}
	if lines.len() != expected {
		bail!(
			"the gate listed {} files but {expected} were staged, so it graded a different tree \
			 than was measured. The staging tree is at {}.",
			lines.len(),
			staged.display()
		);
	}
	let unformatted: Vec<&str> = lines
		.iter()
		.filter(|line| !line.starts_with("ok  "))
		.copied()
		.collect();
	if !unformatted.is_empty() {
		bail!(
			"{} staged file(s) were reported as needing formatting despite exit 0: {:?}",
			unformatted.len(),
			unformatted
		);
	}
	if stderr != format!("\n{CLEAN_SUMMARY}") {
		bail!("expected a blank line and the clean summary on stderr, got:\n{stderr:?}");
	}
	Ok(())
}

/// `fmt --test --verbose --exclude … <target>`, the same for both tools.
///
/// `--test` is what makes this safe over the corpus itself: the mode discards,
/// so nothing is written. The excludes are passed explicitly because the
/// defaults drop `vendor/**`, which this corpus deliberately includes.
fn fmt_test_arguments(excludes: &[String], target: &str) -> Vec<String> {
	let mut arguments = vec![
		"fmt".to_owned(),
		"--test".to_owned(),
		"--verbose".to_owned(),
	];
	for exclude in excludes {
		arguments.push("--exclude".to_owned());
		arguments.push(exclude.clone());
	}
	arguments.push(target.to_owned());
	arguments
}

/// Item 4: the same `fmt --test --verbose` run in both tools, per root.
///
/// It is the only thing here that grades discovery order over a real tree, and
/// it grades the per-file changed/unchanged verdict with it. A root holding a
/// file the parser refuses aborts both tools at the same file, so the comparison
/// still holds, over fewer files — which is exactly why it is not the parity
/// measurement.
///
/// Returns how many *pinned* roots agreed and how many there were. Extras are
/// compared too, because a local private tree is worth checking, but they do not
/// move the recorded number.
fn compare_discovery(
	roots: &[Root],
	tk: &str,
	rtk: &str,
	excludes: &[String],
	harness: &Harness<'_>,
) -> (usize, usize) {
	let mut agreeing = 0;
	let mut pinned = 0;
	for root in roots {
		let counts = root.origin.is_some();
		pinned += usize::from(counts);
		let target = root.path.to_string_lossy().into_owned();
		let arguments = fmt_test_arguments(excludes, &target);
		let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();

		match (run_plain(tk, &borrowed), run_plain(rtk, &borrowed)) {
			(Ok(theirs), Ok(ours)) => {
				if theirs.stdout == ours.stdout
					&& theirs.stderr == ours.stderr
					&& theirs.code == ours.code
				{
					agreeing += usize::from(counts);
				} else {
					harness.note(format!(
						"[{}] the discovery listing differs.\n      stdout {}\n      stderr {}",
						root.name,
						difference(&theirs.stdout, &ours.stdout),
						difference(&theirs.stderr, &ours.stderr)
					));
				}
			}
			(theirs, ours) => {
				let error = theirs
					.err()
					.or_else(|| ours.err())
					.expect("one of the two failed");
				harness.note(format!(
					"[{}] the discovery run failed: {error:#}",
					root.name
				));
			}
		}
	}
	(agreeing, pinned)
}

/// Where two byte strings first differ, rendered short enough to read.
fn difference(theirs: &[u8], ours: &[u8]) -> String {
	if theirs == ours {
		return "same".to_owned();
	}
	let at = theirs
		.iter()
		.zip(ours.iter())
		.position(|(left, right)| left != right)
		.unwrap_or_else(|| theirs.len().min(ours.len()));
	let window = |bytes: &[u8]| {
		let start = at.saturating_sub(40);
		let end = (at + 40).min(bytes.len());
		String::from_utf8_lossy(&bytes[start..end]).into_owned()
	};
	format!(
		"differ at byte {at} (tk {} bytes, rtk {} bytes)\n        tk  {:?}\n        rtk {:?}",
		theirs.len(),
		ours.len(),
		window(theirs),
		window(ours)
	)
}

fn run_with_stdin(executable: &str, arguments: &[&str], input: &[u8]) -> Result<Run> {
	spawn(executable, None, arguments, Some(input))
}

fn run_plain(executable: &str, arguments: &[&str]) -> Result<Run> {
	spawn(executable, None, arguments, None)
}

fn run_in(executable: &str, directory: &Path, arguments: &[&str]) -> Result<Run> {
	spawn(executable, Some(directory), arguments, None)
}

/// Run a binary, with the environment scrubbed the way the CLI parity test
/// scrubs it, and with stdin written from its own thread.
///
/// The thread is not caution. A formatted file can be hundreds of kilobytes and
/// a pipe buffer is tens, so writing all of stdin before reading any of stdout
/// deadlocks as soon as the child's output fills its own pipe.
/// `cmds/rtk/tests/fmt_parity_test.rs` writes on the calling thread and gets
/// away with it only because its inputs are two lines long. It is detached
/// rather than joined for the same reason: joining before `wait_with_output`
/// would reinstate the deadlock it exists to avoid.
fn spawn(
	executable: &str,
	directory: Option<&Path>,
	arguments: &[&str],
	input: Option<&[u8]>,
) -> Result<Run> {
	let mut command = Command::new(executable);
	command
		.args(arguments)
		// The three things that would otherwise let a developer's shell write
		// to the stderr being compared.
		.env("RUST_LOG", "error")
		.env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
		.env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.stdin(if input.is_some() {
			Stdio::piped()
		} else {
			Stdio::null()
		});
	if let Some(directory) = directory {
		command.current_dir(directory);
	}

	let mut child = command
		.spawn()
		.with_context(|| format!("spawning {executable}"))?;

	if let Some(input) = input {
		let mut pipe = child.stdin.take().context("stdin was piped")?;
		let owned = input.to_vec();
		std::thread::spawn(move || {
			// Ignored on purpose: a command that never reads stdin closes the
			// pipe, and the broken pipe is the answer rather than an error.
			// Dropping `pipe` at the end of this closure is what sends EOF.
			let _ = pipe.write_all(&owned);
		});
	}

	let output = child
		.wait_with_output()
		.with_context(|| format!("waiting for {executable}"))?;
	Ok(Run {
		stdout: output.stdout,
		stderr: output.stderr,
		code: output.status.code(),
		status: output.status.to_string(),
	})
}

/// The buckets have to cover every file, the second-run outcomes have to cover
/// every formatted one, and the two agreement buckets have to cover `matching`.
///
/// A harness bug rather than a formatter one — but checked rather than assumed,
/// because a counter that stopped being incremented reads as good news in every
/// one of these numbers.
fn check_internal_consistency(tally: &Tally, failures: &mut String) {
	let accounted = tally.formatted + tally.parse_errors + tally.non_utf8 + tally.split_verdict;
	if accounted != tally.files {
		writeln!(
			failures,
			"\n{} files were discovered but {accounted} reached a verdict, so this run graded \
			 fewer files than it counted. That is a bug in the harness.",
			tally.files
		)
		.expect("writing to a String cannot fail");
	}
	let second_runs = tally.already_formatted + tally.non_convergent + tally.refusing_to_reparse;
	if second_runs != tally.formatted {
		writeln!(
			failures,
			"\n{} files were formatted but {second_runs} reached a second-run outcome. That is a \
			 bug in the harness.",
			tally.formatted
		)
		.expect("writing to a String cannot fail");
	}
	let agreements = tally.matching_formatted + tally.error_text_matching;
	if agreements != tally.matching {
		writeln!(
			failures,
			"\n{} files agree on every observable but the two agreement buckets hold \
			 {agreements}. That is a bug in the harness.",
			tally.matching
		)
		.expect("writing to a String cannot fail");
	}
}

/// Print every count, on every run.
///
/// Not conditional on failure: a CI log nobody reads is how this project has
/// been caught out five times. The numbers are this gate's output, so they are
/// printed and then asserted, in that order.
fn report(tally: &Tally, discovery_agreement: usize, pinned_roots: usize, staged: &Path) {
	println!("\nmeasured");
	// The formatter's number first, because it is the one Phase 5 predicted and
	// the one the README's ✅ rests on. `parity (all streams)` is stricter and
	// includes files neither tool could parse, where what is being compared is
	// anyhow's error text against go-clix's.
	println!(
		"  fmt parity             {} of {} formatted",
		tally.matching_formatted, tally.formatted
	);
	println!(
		"  already formatted      {} of {} formatted",
		tally.already_formatted, tally.formatted
	);
	println!("  non-convergent         {}", tally.non_convergent);
	println!("  refusing to reparse    {}", tally.refusing_to_reparse);
	println!(
		"  parity (all streams)   {} of {} files",
		tally.matching, tally.files
	);
	// `0 of 0` reads as agreement and is an empty denominator, which is this
	// project's own recurring failure. The first run measured `parse_errors = 0`
	// — real Grafana Jsonnet parses — so this is the state to expect, and it has
	// to say that it graded nothing. The property itself is guaranteed elsewhere,
	// by `tk_agrees_on_a_refusal` in `cmds/rtk/tests/fmt_parity_test.rs`, which
	// constructs a refusal rather than hoping the corpus contains one.
	if tally.parse_errors == 0 {
		println!(
			"  refusal text agrees    UNGRADED — nothing in the corpus fails to parse, so this \
			 measured nothing"
		);
	} else {
		println!(
			"  refusal text agrees    {} of {} parse errors",
			tally.error_text_matching, tally.parse_errors
		);
	}
	println!("  split verdicts         {}", tally.split_verdict);
	println!("  not UTF-8              {}", tally.non_utf8);
	println!("  under vendor/          {}", tally.vendor_files);
	println!("  discovery agreement    {discovery_agreement} of {pinned_roots} pinned roots");
	println!("  staging tree           {}", staged.display());

	// Ready to paste, because the discipline is that a human records the number
	// in the same commit and a number that has to be retyped gets retyped
	// wrong. Deliberately not written back automatically: an auto-ratchet is a
	// ratchet nobody reads.
	println!("\n[baseline]");
	println!("files = {}", tally.files);
	println!("vendor_files = {}", tally.vendor_files);
	println!("matching = {}", tally.matching);
	println!("matching_formatted = {}", tally.matching_formatted);
	println!("formatted = {}", tally.formatted);
	println!("parse_errors = {}", tally.parse_errors);
	println!("error_text_matching = {}", tally.error_text_matching);
	println!("already_formatted = {}", tally.already_formatted);
	println!("non_convergent = {}", tally.non_convergent);
	println!("refusing_to_reparse = {}", tally.refusing_to_reparse);
	println!("non_utf8 = {}", tally.non_utf8);
	println!("roots_with_matching_discovery = {discovery_agreement}");
}

/// A declared `rev` that does not match the clone fails; an empty one is loud
/// and does not.
///
/// The asymmetry is the one unmeasured state this gate tolerates, because a
/// commit hash cannot be written down before the first clone resolves it.
fn check_revisions(roots: &[Root], failures: &mut String) {
	println!("\nrevisions, for the `rev` of each [[repos]] entry:");
	for root in roots {
		let Some(origin) = &root.origin else { continue };
		println!(
			"  {:<14} rev = \"{}\"",
			root.name,
			if origin.cloned_rev.is_empty() {
				"UNKNOWN"
			} else {
				origin.cloned_rev.as_str()
			}
		);

		if origin.declared_rev.is_empty() {
			println!(
				"  {:<14} UNPINNED: no `rev` in the config, so the counts above answer for a \
				 commit nothing records. Fill it in.",
				root.name
			);
			continue;
		}
		if origin.cloned_rev.is_empty() {
			writeln!(
				failures,
				"\n[{}] declares rev {} but the clone recorded none. Re-run \
				 `make fmt-acceptance-corpus`.",
				root.name, origin.declared_rev
			)
			.expect("writing to a String cannot fail");
			continue;
		}
		if origin.cloned_rev != origin.declared_rev {
			writeln!(
				failures,
				"\n[{}] is checked out at {} but the config pins {}. The recorded numbers answer \
				 for the pin, so either re-clone or move the pin and the baseline together.",
				root.name, origin.cloned_rev, origin.declared_rev
			)
			.expect("writing to a String cannot fail");
		}
	}
}

/// A listed file that was never reached is as much of a lie as an unlisted one
/// that fails.
///
/// The corpus tracks moving branches, so an upstream deletion or rename would
/// otherwise leave an entry in `[known]` describing nothing — and the count
/// beside it would go down, which reads as an improvement.
fn check_known_lists(known: &KnownConfig, observed: &BTreeSet<String>, failures: &mut String) {
	let listed = known
		.output_does_not_reparse
		.iter()
		.map(|key| ("output_does_not_reparse", key))
		.chain(
			known
				.non_convergent
				.iter()
				.map(|key| ("non_convergent", key)),
		);
	for (list, key) in listed {
		if !observed.contains(key) {
			writeln!(
				failures,
				"\n[known].{list} lists {key}, which this run never reached. The corpus tracks \
				 moving branches, so the file has probably been renamed or deleted upstream — \
				 which means the entry, and the count beside it, no longer describe anything."
			)
			.expect("writing to a String cannot fail");
		}
	}
}

fn check_baseline(
	baseline: &Baseline,
	tally: &Tally,
	discovery_agreement: usize,
	failures: &mut String,
) {
	let comparisons: [(&str, i64, usize); 12] = [
		("files", baseline.files, tally.files),
		("vendor_files", baseline.vendor_files, tally.vendor_files),
		("matching", baseline.matching, tally.matching),
		(
			"matching_formatted",
			baseline.matching_formatted,
			tally.matching_formatted,
		),
		("formatted", baseline.formatted, tally.formatted),
		("parse_errors", baseline.parse_errors, tally.parse_errors),
		(
			"error_text_matching",
			baseline.error_text_matching,
			tally.error_text_matching,
		),
		(
			"already_formatted",
			baseline.already_formatted,
			tally.already_formatted,
		),
		(
			"non_convergent",
			baseline.non_convergent,
			tally.non_convergent,
		),
		(
			"refusing_to_reparse",
			baseline.refusing_to_reparse,
			tally.refusing_to_reparse,
		),
		("non_utf8", baseline.non_utf8, tally.non_utf8),
		(
			"roots_with_matching_discovery",
			baseline.roots_with_matching_discovery,
			discovery_agreement,
		),
	];

	for (key, recorded, measured) in comparisons {
		if recorded < 0 {
			writeln!(
				failures,
				"\n{key} has never been measured (`{key} = {recorded}` in the acceptance \
				 config). This run measured {measured}. Record it in the same commit; a count \
				 left unrecorded is a gate that passes without saying anything."
			)
			.expect("writing to a String cannot fail");
			continue;
		}
		if recorded != i64::try_from(measured).unwrap_or(i64::MAX) {
			writeln!(
				failures,
				"\n{key} is {measured}, but the acceptance config records {recorded}. Exact in \
				 both directions: fewer is a regression, more is progress and the number is \
				 updated in the same commit."
			)
			.expect("writing to a String cannot fail");
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn make_root(name: &str, path: &str) -> Root {
		Root {
			name: name.to_owned(),
			path: PathBuf::from(path),
			origin: None,
		}
	}

	/// The committed config has to parse, and to declare a corpus.
	///
	/// It is hand-written TOML that nothing else reads until someone runs the
	/// gate — by which point they have already waited for several hundred
	/// megabytes of clones. A typo in it should fail in `cargo test`, next to
	/// the code that consumes it, rather than after the download.
	#[test]
	fn the_committed_config_parses_and_declares_a_corpus() {
		let path = Path::new(env!("CARGO_MANIFEST_DIR"))
			.join("../..")
			.join("fmt-acceptance.toml");
		let config = load_config(&path.to_string_lossy())
			.unwrap_or_else(|error| panic!("{}: {error:#}", path.display()));

		assert!(
			!config.repos.is_empty(),
			"the acceptance corpus declares no repositories"
		);
		for repo in &config.repos {
			assert!(!repo.name.is_empty(), "a [[repos]] entry has no name");
			assert!(
				repo.url.starts_with("https://"),
				"{} is cloned from {:?}, which is not an https URL",
				repo.name,
				repo.url
			);
			assert!(
				!repo.reference.is_empty(),
				"{} declares no ref to clone",
				repo.name
			);
		}
		assert!(
			!config.discovery.exclude.is_empty(),
			"an empty exclude list would walk .git"
		);
		// The corpus includes vendor deliberately, so the one thing the excludes
		// must not do is what `tk fmt`'s defaults do.
		assert!(
			!config
				.discovery
				.exclude
				.iter()
				.any(|pattern| pattern.contains("vendor")),
			"the acceptance corpus includes vendored Jsonnet on purpose, so the excludes must \
			 not carry `tk fmt`'s own vendor patterns: {:?}",
			config.discovery.exclude
		);
		rtk_gobwas_glob::compile_all(&config.discovery.exclude)
			.expect("every exclude compiles as a gobwas glob");
	}

	#[test]
	fn vendor_is_detected_by_component_and_not_by_substring() {
		assert!(is_vendored(
			"mimir/operations/mimir/vendor/github.com/a.libsonnet"
		));
		assert!(is_vendored("vendor/a.libsonnet"));
		assert!(!is_vendored("loki/production/vendored-thing/a.libsonnet"));
		assert!(!is_vendored("loki/production/a-vendor.libsonnet"));
	}

	/// The key has to be independent of where the corpus was cloned, or every
	/// `[known]` entry breaks when someone passes `--corpus-dir`.
	#[test]
	fn the_key_is_relative_to_its_root() {
		let cloned = make_root("mimir", "target/fmt-acceptance-corpus/mimir");
		assert_eq!(
			key_for(
				&cloned,
				"target/fmt-acceptance-corpus/mimir/operations/mimir/a.libsonnet"
			),
			"mimir/operations/mimir/a.libsonnet"
		);

		let elsewhere = make_root("mimir", "/somewhere/else/mimir");
		assert_eq!(
			key_for(&elsewhere, "/somewhere/else/mimir/operations/a.libsonnet"),
			"mimir/operations/a.libsonnet"
		);
	}

	#[test]
	fn the_excludes_are_passed_through_rather_than_defaulted() {
		// The defaults drop `vendor/**`, and this corpus includes it — so a
		// gate that forgot to pass the excludes would silently grade a smaller
		// tree than was measured.
		let arguments = fmt_test_arguments(&["**/.*".to_owned(), ".*".to_owned()], ".");
		assert_eq!(
			arguments,
			vec![
				"fmt",
				"--test",
				"--verbose",
				"--exclude",
				"**/.*",
				"--exclude",
				".*",
				"."
			]
		);
	}

	#[test]
	fn a_difference_names_the_byte_it_starts_at() {
		assert_eq!(difference(b"abc", b"abc"), "same");
		let rendered = difference(b"abc", b"abd");
		assert!(rendered.starts_with("differ at byte 2"), "{rendered}");
		// A shared prefix and a shorter left side: the window must not slice
		// past the end of either.
		let truncated = difference(b"abc", b"ab");
		assert!(truncated.starts_with("differ at byte 2"), "{truncated}");
	}

	/// Both lists ratchet in both directions, so a listed file that was never
	/// reached fails as loudly as an unlisted one that starts failing.
	#[test]
	fn a_listed_file_that_was_never_reached_fails() {
		let known = KnownConfig {
			output_does_not_reparse: vec!["mimir/gone.libsonnet".to_owned()],
			non_convergent: Vec::new(),
		};
		let mut failures = String::new();
		check_known_lists(&known, &BTreeSet::new(), &mut failures);
		assert!(failures.contains("mimir/gone.libsonnet"), "{failures}");

		let mut seen = String::new();
		let observed = BTreeSet::from(["mimir/gone.libsonnet".to_owned()]);
		check_known_lists(&known, &observed, &mut seen);
		assert!(seen.is_empty(), "{seen}");
	}

	/// An unmeasured count is a failure rather than a default of zero.
	#[test]
	fn an_unmeasured_baseline_fails() {
		let baseline = Baseline {
			files: -1,
			vendor_files: -1,
			matching: -1,
			matching_formatted: -1,
			formatted: -1,
			parse_errors: -1,
			error_text_matching: -1,
			already_formatted: -1,
			non_convergent: -1,
			refusing_to_reparse: -1,
			non_utf8: -1,
			roots_with_matching_discovery: -1,
		};
		let mut failures = String::new();
		check_baseline(&baseline, &Tally::default(), 0, &mut failures);
		assert_eq!(
			failures.matches("has never been measured").count(),
			12,
			"every count has to be recorded, and zero is a legitimate value for several of \
			 them — so an absent one cannot default:\n{failures}"
		);
	}

	/// The buckets are checked rather than trusted.
	#[test]
	fn the_buckets_have_to_cover_every_file() {
		let mut failures = String::new();
		check_internal_consistency(
			&Tally {
				files: 3,
				formatted: 1,
				already_formatted: 1,
				..Tally::default()
			},
			&mut failures,
		);
		assert!(failures.contains("reached a verdict"), "{failures}");

		let mut consistent = String::new();
		check_internal_consistency(
			&Tally {
				files: 2,
				formatted: 1,
				parse_errors: 1,
				matching: 2,
				matching_formatted: 1,
				error_text_matching: 1,
				already_formatted: 1,
				..Tally::default()
			},
			&mut consistent,
		);
		assert!(consistent.is_empty(), "{consistent}");
	}
}
