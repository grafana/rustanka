//! Acceptance checks for `rtk fmt` over real Grafana Jsonnet.
//!
//! One corpus measures four properties:
//! 1. Per-file stdout, stderr, and exit-code parity with `tk fmt -`.
//! 2. Whether tk-formatted files remain unchanged under `rtk fmt --test`.
//! 3. Whether formatted output reparses and settles on a second run.
//! 4. Discovery order, compared through each tool's `fmt --test --verbose`.
//!
//! Files are compared individually through stdin so one parse failure cannot
//! truncate the corpus while both tools appear to agree. This also preserves
//! the source corpus and produces output without filename headers or summaries.
//! Discovery is checked separately over each root.
//!
//! The second-run outcomes (`already_formatted`, `non_convergent`, and
//! `refusing_to_reparse`) must sum to `formatted`. Tk's formattable outputs are
//! staged for end-to-end file checks. Stable results share a strict clean-tree
//! gate; declared exceptions run separately so a refusal cannot hide later
//! files. Both gates compare the supplied executables' raw output and status.
//!
//! Missing or undersized corpora fail, as do unmeasured baseline counts (`-1`).
//! Known-case lists are checked both for new failures and entries that no longer
//! reproduce. Counts are printed on success as well as failure.

use std::{
	collections::BTreeSet,
	ffi::{OsStr, OsString},
	fmt::Write as _,
	fs,
	io::Write as _,
	path::{Component, Path, PathBuf},
	process::{Command, Stdio},
	sync::Mutex,
	time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
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

impl AcceptanceConfig {
	fn validate(&self) -> Result<()> {
		if self.repos.is_empty() {
			bail!("acceptance config has no repositories");
		}
		let mut names = BTreeSet::new();
		for repo in &self.repos {
			repo.validate()?;
			if !names.insert(&repo.name) {
				bail!("duplicate repository name: {}", repo.name);
			}
		}
		self.known.validate()
	}
}

impl Repo {
	fn validate(&self) -> Result<()> {
		if self.name.is_empty()
			|| !self.name.as_bytes()[0].is_ascii_alphanumeric()
			|| !self
				.name
				.bytes()
				.all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
			|| self.name.starts_with("extra-")
		{
			bail!("unsafe repository name: {:?}", self.name);
		}
		if self.rev.len() != 40 || !self.rev.bytes().all(|b| b.is_ascii_hexdigit()) {
			bail!("{} requires an immutable 40-digit commit rev", self.name);
		}
		if self.url.is_empty() || self.reference.is_empty() {
			bail!("{} needs url and ref", self.name);
		}
		Ok(())
	}
}

impl KnownConfig {
	fn category(&self, key: &Path) -> Option<&'static str> {
		if self
			.non_convergent
			.iter()
			.any(|value| Path::new(value) == key)
		{
			Some("non_convergent")
		} else if self
			.output_does_not_reparse
			.iter()
			.any(|value| Path::new(value) == key)
		{
			Some("output_does_not_reparse")
		} else {
			None
		}
	}

	fn validate(&self) -> Result<()> {
		let mut seen = BTreeSet::new();
		for key in self
			.non_convergent
			.iter()
			.chain(&self.output_does_not_reparse)
		{
			if key.contains('\\')
				|| key.split('/').count() < 2
				|| key
					.split('/')
					.any(|part| part.is_empty() || part == "." || part == "..")
				|| Path::new(key)
					.components()
					.any(|part| !matches!(part, Component::Normal(_)))
			{
				bail!("unsafe known-case key: {key:?}");
			}
			if !seen.insert(key) {
				bail!("duplicate or overlapping known-case entry: {key}");
			}
		}
		for key in &seen {
			if seen
				.iter()
				.any(|other| key != other && Path::new(other).starts_with(key))
			{
				bail!("overlapping known-case paths: {key}");
			}
		}
		Ok(())
	}
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
#[derive(Clone, Default)]
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
	config.validate()?;
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
	let mut discovered: Vec<(usize, PathBuf)> = Vec::new();
	for (index, root) in roots.iter().enumerate() {
		let files = rtk_jsonnetfmt::find_files_all([&root.path], &excludes)
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

	let staged = prepare_staging(&cli.staged_dir, &roots)?;
	let harness = Harness {
		tk: &tk,
		rtk: &rtk,
		staged: &staged,
		known: &config.known,
		findings: Mutex::new(Vec::new()),
		observed_known: Mutex::new(BTreeSet::new()),
	};

	let tallies = match cli.jobs {
		Some(jobs) => rayon::ThreadPoolBuilder::new()
			.num_threads(jobs)
			.build()
			.context("building the worker pool")?
			.install(|| measure_all(&discovered, &roots, &harness)),
		None => measure_all(&discovered, &roots, &harness),
	};

	let tally = tallies.pinned.clone().merge(tallies.extra.clone());
	let tree_gate = harness.run_tree_gate(&config.discovery.exclude, tally.already_formatted);
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
	println!("\npinned repositories (recorded baseline)");
	report(&tallies.pinned, discovery_agreement, pinned_roots, &staged);
	println!(
		"\nextra roots: {} files, {} matching, {} vendor files (outside baseline)",
		tallies.extra.files, tallies.extra.matching, tallies.extra.vendor_files
	);

	let mut failures = String::new();
	check_internal_consistency(&tally, &mut failures);
	if let Err(error) = tree_gate {
		writeln!(failures, "\nalready-formatted gate: {error:#}")
			.expect("writing to a String cannot fail");
	}
	if cli.advisory_drift {
		println!("\nADVISORY DRIFT: moving revisions; this is not pinned acceptance.");
	} else {
		check_revisions(&roots, &mut failures);
	}
	check_known_lists(&config.known, &observed_known, &mut failures);

	if !cli.advisory_drift {
		check_baseline(
			&config.baseline,
			&tallies.pinned,
			discovery_agreement,
			&mut failures,
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
		println!(
			"\n{}: green.",
			if cli.advisory_drift {
				"fmt advisory drift"
			} else {
				"fmt acceptance"
			}
		);
		return Ok(());
	}
	bail!("fmt acceptance failed.{failures}");
}

#[derive(Default)]
struct Tallies {
	pinned: Tally,
	extra: Tally,
}

impl Tallies {
	fn merge(self, other: Self) -> Self {
		Self {
			pinned: self.pinned.merge(other.pinned),
			extra: self.extra.merge(other.extra),
		}
	}
}

fn measure_all(discovered: &[(usize, PathBuf)], roots: &[Root], harness: &Harness<'_>) -> Tallies {
	discovered
		.par_iter()
		.map(|(index, file)| {
			let root = &roots[*index];
			let tally = harness.measure_file(root, file);
			if root.origin.is_some() {
				Tallies {
					pinned: tally,
					..Tallies::default()
				}
			} else {
				Tallies {
					extra: tally,
					..Tallies::default()
				}
			}
		})
		.reduce(Tallies::default, Tallies::merge)
}

fn load_config(path: &Path) -> Result<AcceptanceConfig> {
	let contents = fs::read_to_string(path)
		.with_context(|| format!("reading the config: {}", path.display()))?;
	toml::from_str(&contents).with_context(|| format!("parsing the config: {}", path.display()))
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
	let corpus = &cli.corpus_dir;
	let mut roots = Vec::new();

	for repo in &config.repos {
		let path = corpus.join(&repo.name);
		if path.is_symlink() {
			bail!("corpus repository {} is a symlink", path.display());
		}
		if !path.is_dir() {
			bail!(
				"{} is missing. Run `make fmt-acceptance-corpus` to clone the pinned \
				 repositories; there is no committed copy of this corpus and there cannot be \
				 one — see the provenance section of {}.",
				path.display(),
				cli.config.display()
			);
		}
		// Written by the clone target. Absent means an older or hand-made
		// checkout, which is reported rather than guessed at.
		let cloned_rev = fs::read_to_string(corpus.join(format!("{}.rev", repo.name)))
			.with_context(|| format!("reading revision for {}", repo.name))?
			.trim()
			.to_owned();
		if path.join(".git").exists() {
			let actual = Command::new("git")
				.arg("-C")
				.arg(&path)
				.args(["rev-parse", "HEAD"])
				.output()
				.with_context(|| format!("reading actual revision for {}", repo.name))?;
			if !actual.status.success()
				|| actual.stdout.strip_suffix(b"\n").unwrap_or(&actual.stdout)
					!= cloned_rev.as_bytes()
			{
				bail!("{} revision record does not match its checkout", repo.name);
			}
		}
		roots.push(Root {
			name: repo.name.clone(),
			path: path.canonicalize()?,
			origin: Some(Origin {
				url: repo.url.clone(),
				reference: repo.reference.clone(),
				declared_rev: repo.rev.clone(),
				cloned_rev,
			}),
		});
	}

	for (index, extra) in cli.extra_root.iter().enumerate() {
		let path = extra.clone();
		if !path.is_dir() {
			bail!("--extra-root {} is not a directory", extra.display());
		}
		roots.push(Root {
			name: format!("extra-{}", index + 1),
			path: path.canonicalize()?,
			origin: None,
		});
	}

	for (index, root) in roots.iter().enumerate() {
		for other in &roots[..index] {
			if overlaps(&root.path, &other.path) {
				bail!(
					"source roots {} and {} overlap",
					root.path.display(),
					other.path.display()
				);
			}
		}
	}
	Ok(roots)
}

/// Canonicalize existing ancestors too, so an absent child through a symlink
/// cannot disguise overlap with a source tree.
fn resolved_path(path: &Path) -> Result<PathBuf> {
	if path.exists() {
		return path.canonicalize().context("resolving path");
	}
	if path.is_symlink() {
		bail!("dangling symlink: {}", path.display());
	}
	let absolute = if path.is_absolute() {
		path.to_owned()
	} else {
		std::env::current_dir()?.join(path)
	};
	let parent = absolute.parent().context("path has no parent")?;
	let name = absolute
		.file_name()
		.context("path has no final component")?;
	Ok(resolved_path(parent)?.join(name))
}

fn overlaps(left: &Path, right: &Path) -> bool {
	left.starts_with(right) || right.starts_with(left)
}

/// Each run owns a newly created child. Existing content is never removed.
fn prepare_staging(directory: &Path, roots: &[Root]) -> Result<PathBuf> {
	let path = resolved_path(directory)?;
	for root in roots {
		if overlaps(&path, &root.path.canonicalize()?) {
			bail!(
				"staging directory {} overlaps source {}",
				path.display(),
				root.path.display()
			);
		}
	}
	fs::create_dir_all(&path)?;
	let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
	for attempt in 0..100 {
		let owned = path.join(format!("rtk-fmt-{}-{nonce}-{attempt}", std::process::id()));
		match fs::create_dir(&owned) {
			Ok(()) => {
				fs::write(
					owned.join(".rtk-fmt-staging"),
					b"rustanka fmt acceptance staging v1\n",
				)?;
				fs::create_dir(owned.join("stable"))?;
				return Ok(owned);
			}
			Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
			Err(error) => return Err(error.into()),
		}
	}
	bail!(
		"could not create a unique staging child under {}",
		path.display()
	)
}

fn is_vendored(file: impl AsRef<Path>) -> bool {
	file.as_ref()
		.components()
		.any(|component| component.as_os_str() == "vendor")
}

impl Root {
	fn key_for(&self, file: &Path) -> Result<PathBuf> {
		let relative = file
			.strip_prefix(&self.path)
			.context("discovered file escaped its root")?;
		if relative
			.components()
			.any(|part| !matches!(part, Component::Normal(_)))
		{
			bail!("invalid discovered relative path: {}", relative.display());
		}
		Ok(Path::new(&self.name).join(relative))
	}
}

impl Harness<'_> {
	fn measure_file(&self, root: &Root, file: &Path) -> Tally {
		let harness = self;
		let native_key = match root.key_for(file) {
			Ok(key) => key,
			Err(error) => {
				harness.note(format!("{}: {error:#}", file.display()));
				return Tally {
					files: 1,
					split_verdict: 1,
					..Tally::default()
				};
			}
		};
		let key = native_key.display();
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

		tally.non_utf8 = usize::from(std::str::from_utf8(&bytes).is_err());

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

		let agreed = theirs.stdout == ours.stdout
			&& theirs.stderr == ours.stderr
			&& theirs.code == ours.code;
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

		tally.formatted += 1;
		tally.matching_formatted += usize::from(agreed);
		harness.second_run(&native_key, &theirs.stdout, &mut tally);
		tally
	}

	/// Compare both real executables on the first-pass output. This preserves the
	/// CLI's stack protection and never changes the single-pass formatting contract.
	fn second_run(&self, key: &Path, formatted: &[u8], tally: &mut Tally) {
		let display = key.display();
		let listed = self.known.category(key);
		if listed.is_some() {
			self.observed(&key.to_string_lossy());
		}
		let (theirs, ours) = match (
			run_with_stdin(self.tk, &["fmt", "-"], formatted),
			run_with_stdin(self.rtk, &["fmt", "-"], formatted),
		) {
			(Ok(theirs), Ok(ours)) => (theirs, ours),
			_ => {
				self.note(format!("{display}: could not run second-pass executables"));
				return;
			}
		};
		if !theirs.agrees(&ours) {
			self.note(format!(
				"{display}: second-pass parity differs: stdout {}; stderr {}; status {:?}/{:?}",
				difference(&theirs.stdout, &ours.stdout),
				difference(&theirs.stderr, &ours.stderr),
				theirs.code,
				ours.code
			));
		}
		let category = if theirs.code == Some(0) {
			if theirs.stdout == formatted {
				tally.already_formatted += 1;
				None
			} else {
				tally.non_convergent += 1;
				Some("non_convergent")
			}
		} else if theirs.code == Some(1) {
			tally.refusing_to_reparse += 1;
			Some("output_does_not_reparse")
		} else {
			self.note(format!(
				"{display}: second pass crashed or exited unexpectedly: {}",
				theirs.status
			));
			return;
		};
		if listed != category {
			self.note(format!("{display}: second-run category is {category:?}, but [known] declares {listed:?}; update only after verifying the upstream behavior"));
		}
		let relative = Path::new(category.unwrap_or("stable")).join(key);
		if let Err(error) = self.stage(&relative, formatted) {
			self.note(format!("{display}: could not be staged: {error:#}"));
			return;
		}
		// Exceptions are deliberately separate from the stable tree: a parse
		// refusal would stop discovery and hide the files after it. Each exception
		// still reaches both real file-based CLIs, with raw stream/status parity.
		if let Some(category) = category {
			let arguments = fmt_test_arguments(&[], &relative);
			match (
				run_in(self.tk, self.staged, &arguments),
				run_in(self.rtk, self.staged, &arguments),
			) {
				(Ok(theirs), Ok(ours)) => {
					let expected = if category == "non_convergent" { 16 } else { 1 };
					if theirs.code != Some(expected) || !theirs.agrees(&ours) {
						self.note(format!("{display}: staged {category} check differs or has wrong status: tk {:?}, rtk {:?}; stdout {}; stderr {}", theirs.code, ours.code, difference(&theirs.stdout, &ours.stdout), difference(&theirs.stderr, &ours.stderr)));
					}
				}
				_ => self.note(format!("{display}: staged exception check could not run")),
			}
		}
	}

	fn stage(&self, key: &Path, contents: &[u8]) -> Result<()> {
		if key.as_os_str().is_empty()
			|| key
				.components()
				.any(|part| !matches!(part, Component::Normal(_)))
		{
			bail!("unsafe staging key: {}", key.display());
		}
		let destination = self.staged.join(key);
		if let Some(parent) = destination.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::write(&destination, contents)
			.with_context(|| format!("writing {}", destination.display()))
	}

	fn run_tree_gate(&self, excludes: &[String], expected: usize) -> Result<()> {
		let staged = self.staged.join("stable");
		let arguments = fmt_test_arguments(excludes, Path::new("."));
		let theirs = run_in(self.tk, &staged, &arguments)?;
		let ours = run_in(self.rtk, &staged, &arguments)?;
		if theirs.code != Some(0) || !theirs.agrees(&ours) {
			bail!(
				"stable staged tree parity failed: tk {:?}, rtk {:?}; stdout {}; stderr {}",
				theirs.code,
				ours.code,
				difference(&theirs.stdout, &ours.stdout),
				difference(&theirs.stderr, &ours.stderr)
			);
		}
		let globs = rtk_gobwas_glob::compile_all(excludes)?;
		let files = rtk_jsonnetfmt::find_files(&staged, &globs)?;
		if files.len() != expected {
			bail!(
				"stable tree contains {} discovered files, expected {expected}",
				files.len()
			);
		}
		if ours.stderr != format!("\n{CLEAN_SUMMARY}").as_bytes() {
			bail!(
				"stable tree did not report a clean summary: {:?}",
				ours.stderr
			);
		}
		Ok(())
	}
}

impl Run {
	fn agrees(&self, other: &Self) -> bool {
		self.code.is_some()
			&& self.code == other.code
			&& self.stdout == other.stdout
			&& self.stderr == other.stderr
	}
}

/// `fmt --test --verbose --exclude … <target>`, the same for both tools.
///
/// `--test` is what makes this safe over the corpus itself: the mode discards,
/// so nothing is written. The excludes are passed explicitly because the
/// defaults drop `vendor/**`, which this corpus deliberately includes.
fn fmt_test_arguments(excludes: &[String], target: &Path) -> Vec<OsString> {
	let mut arguments = vec![
		OsString::from("fmt"),
		OsString::from("--test"),
		OsString::from("--verbose"),
	];
	for exclude in excludes {
		arguments.push(OsString::from("--exclude"));
		arguments.push(OsString::from(exclude));
	}
	arguments.push(target.as_os_str().to_owned());
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
		let arguments = fmt_test_arguments(excludes, &root.path);

		match (run_plain(tk, &arguments), run_plain(rtk, &arguments)) {
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

fn run_with_stdin(executable: &str, arguments: &[impl AsRef<OsStr>], input: &[u8]) -> Result<Run> {
	spawn(executable, None, arguments, Some(input))
}

fn run_plain(executable: &str, arguments: &[impl AsRef<OsStr>]) -> Result<Run> {
	spawn(executable, None, arguments, None)
}

fn run_in(executable: &str, directory: &Path, arguments: &[impl AsRef<OsStr>]) -> Result<Run> {
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
	arguments: &[impl AsRef<OsStr>],
	input: Option<&[u8]>,
) -> Result<Run> {
	// Resolve a caller-supplied relative executable before changing the child's
	// working directory for the staged-file gates.
	let executable = Path::new(executable);
	let executable = if executable.is_relative() && executable.components().count() > 1 {
		std::env::current_dir()?.join(executable)
	} else {
		executable.to_owned()
	};
	let mut command = Command::new(&executable);
	command
		.args(arguments)
		// The three things that would otherwise let a developer's shell write
		// to the stderr being compared.
		.env("RUST_LOG", "error")
		.env("RUST_BACKTRACE", "0")
		.env("RUST_LIB_BACKTRACE", "0")
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
		.with_context(|| format!("spawning {}", executable.display()))?;

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
		.with_context(|| format!("waiting for {}", executable.display()))?;
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
	let accounted = tally.formatted + tally.parse_errors + tally.split_verdict;
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

/// Acceptance requires the immutable revision declared in the config.
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
			writeln!(
				failures,
				"  {:<14} UNPINNED: no `rev` in the config, so the counts above answer for a \
				 commit nothing records. Fill it in.",
				root.name
			)
			.expect("writing to a String cannot fail");
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
/// A changed pin or renamed input must not leave a stale allowance behind.
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
				"\n[known].{list} lists {key}, which this run never reached. The allowance \
				 is stale or its file is missing."
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
		let config =
			load_config(&path).unwrap_or_else(|error| panic!("{}: {error:#}", path.display()));

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
			cloned
				.key_for(Path::new(
					"target/fmt-acceptance-corpus/mimir/operations/mimir/a.libsonnet"
				))
				.unwrap(),
			Path::new("mimir/operations/mimir/a.libsonnet")
		);

		let elsewhere = make_root("mimir", "/somewhere/else/mimir");
		assert_eq!(
			elsewhere
				.key_for(Path::new("/somewhere/else/mimir/operations/a.libsonnet"))
				.unwrap(),
			Path::new("mimir/operations/a.libsonnet")
		);
	}

	#[test]
	fn the_excludes_are_passed_through_rather_than_defaulted() {
		// The defaults drop `vendor/**`, and this corpus includes it — so a
		// gate that forgot to pass the excludes would silently grade a smaller
		// tree than was measured.
		let arguments = fmt_test_arguments(&["**/.*".to_owned(), ".*".to_owned()], Path::new("."));
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
	#[test]
	fn staging_preserves_unrelated_contents_and_uses_fresh_children() {
		let temporary = tempfile::tempdir().unwrap();
		let base = temporary.path().join("staging");
		fs::create_dir(&base).unwrap();
		let sentinel = base.join("sentinel");
		fs::write(&sentinel, b"unrelated").unwrap();
		let first = prepare_staging(&base, &[]).unwrap();
		fs::write(first.join("sentinel"), b"previous run").unwrap();
		let second = prepare_staging(&base, &[]).unwrap();
		assert_ne!(first, second);
		assert_eq!(fs::read(sentinel).unwrap(), b"unrelated");
		assert_eq!(fs::read(first.join("sentinel")).unwrap(), b"previous run");
	}

	#[test]
	fn staging_refuses_sources_their_ancestors_and_descendants() {
		let temporary = tempfile::tempdir().unwrap();
		let source = temporary.path().join("source");
		fs::create_dir(&source).unwrap();
		let roots = [Root {
			name: "source".into(),
			path: source.clone(),
			origin: None,
		}];
		for path in [
			source.clone(),
			temporary.path().to_owned(),
			source.join("child"),
		] {
			assert!(
				prepare_staging(&path, &roots).is_err(),
				"{}",
				path.display()
			);
		}
	}

	#[cfg(unix)]
	#[test]
	fn staging_resolves_symlink_aliases_before_overlap_checks() {
		let temporary = tempfile::tempdir().unwrap();
		let source = temporary.path().join("source");
		fs::create_dir(&source).unwrap();
		let alias = temporary.path().join("alias");
		std::os::unix::fs::symlink(&source, &alias).unwrap();
		let roots = [Root {
			name: "source".into(),
			path: source,
			origin: None,
		}];
		assert!(prepare_staging(&alias, &roots).is_err());
		assert!(prepare_staging(&alias.join("absent"), &roots).is_err());
	}

	#[test]
	fn known_lists_reject_duplicates_overlaps_and_escaping_names() {
		for known in [
			KnownConfig {
				non_convergent: vec!["repo/a".into(), "repo/a".into()],
				..KnownConfig::default()
			},
			KnownConfig {
				non_convergent: vec!["repo/a".into()],
				output_does_not_reparse: vec!["repo/a".into()],
			},
			KnownConfig {
				non_convergent: vec!["repo/a".into(), "repo/a/b".into()],
				..KnownConfig::default()
			},
			KnownConfig {
				non_convergent: vec!["repo/../outside".into()],
				..KnownConfig::default()
			},
		] {
			assert!(known.validate().is_err());
		}
		assert!(KnownConfig {
			non_convergent: vec!["repo/a".into()],
			output_does_not_reparse: vec!["repo/b".into()]
		}
		.validate()
		.is_ok());
	}

	#[test]
	fn repository_names_and_revisions_are_validated() {
		for name in ["..", "a/b", "a\\b", "extra-1", "/tmp", "", ".hidden"] {
			assert!(Repo {
				name: name.into(),
				url: "https://example.test".into(),
				reference: "main".into(),
				rev: "1".repeat(40)
			}
			.validate()
			.is_err());
		}
		assert!(Repo {
			name: "safe-name".into(),
			url: "https://example.test".into(),
			reference: "main".into(),
			rev: "main".into()
		}
		.validate()
		.is_err());
	}

	#[test]
	fn extras_cannot_fill_a_missing_pinned_baseline() {
		let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fmt-acceptance.toml");
		let config = load_config(&path).unwrap();
		let mut tallies = Tallies::default();
		for extra_files in [0, 10_000] {
			tallies.extra.files = extra_files;
			let mut failures = String::new();
			check_baseline(&config.baseline, &tallies.pinned, 0, &mut failures);
			assert!(failures.contains("files is 0"), "{failures}");
			assert!(failures.contains("vendor_files is 0"), "{failures}");
		}
	}

	#[cfg(unix)]
	fn fake_formatter(path: &Path, body: &str) -> String {
		use std::os::unix::fs::PermissionsExt;
		fs::write(path, format!("#!/bin/sh\nset -eu\n[ \"$RUST_BACKTRACE\" = 0 ]\n[ \"$RUST_LIB_BACKTRACE\" = 0 ]\n{body}\n")).unwrap();
		fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
		path.to_str().unwrap().to_owned()
	}

	#[cfg(unix)]
	#[test]
	fn invalid_bytes_and_native_paths_reach_both_executables() {
		use std::os::unix::ffi::OsStringExt;
		let temporary = tempfile::tempdir().unwrap();
		let source = temporary.path().join("source");
		fs::create_dir(&source).unwrap();
		let mut file = source.join(OsString::from_vec(b"bad-\xff.jsonnet".to_vec()));
		let content = b"// \xff\xe2\x82\n{}\n";
		if let Err(error) = fs::write(&file, content) {
			assert!(
				cfg!(target_os = "macos") && matches!(error.raw_os_error(), Some(1 | 92)),
				"native filename creation failed: {error}"
			);
			eprintln!("native non-UTF-8 filename coverage unavailable: macOS filesystem rejected creation with {error}; byte-input coverage continues");
			file = source.join("bytes.jsonnet");
			fs::write(&file, content).unwrap();
		}
		let script = fake_formatter(&temporary.path().join("formatter"), "cat");
		let root = Root {
			name: "source".into(),
			path: source,
			origin: None,
		};
		let staged = prepare_staging(&temporary.path().join("staging"), &[]).unwrap();
		let known = KnownConfig::default();
		let harness = Harness {
			tk: &script,
			rtk: &script,
			staged: &staged,
			known: &known,
			findings: Mutex::new(Vec::new()),
			observed_known: Mutex::new(BTreeSet::new()),
		};
		let tally = harness.measure_file(&root, &file);
		assert_eq!(tally.non_utf8, 1);
		assert_eq!(tally.formatted, 1);
		assert_eq!(tally.matching, 1);
		assert_eq!(tally.already_formatted, 1);
		assert_eq!(
			fs::read(staged.join("stable").join(root.key_for(&file).unwrap())).unwrap(),
			content
		);
		assert!(harness.findings.lock().unwrap().is_empty());
		let mut failures = String::new();
		check_internal_consistency(&tally, &mut failures);
		assert!(failures.is_empty(), "{failures}");
	}

	#[cfg(unix)]
	#[test]
	fn second_pass_uses_supplied_executables_and_checks_staged_exceptions() {
		let temporary = tempfile::tempdir().unwrap();
		let script = fake_formatter(
			&temporary.path().join("formatter"),
			r#"
if [ "$2" = '-' ]; then
    value=$(cat)
else
    for file in "$@"; do :; done
    value=$(cat "$file")
fi
case "$value" in
    moving)
        if [ "$2" = '-' ]; then printf 'stable\n'; else printf 'needs formatting\n'; exit 16; fi ;;
    refuses) printf 'Error: upstream refusal\n' >&2; exit 1 ;;
    *) printf '%s\n' "$value" ;;
esac
"#,
		);
		let known = KnownConfig {
			non_convergent: vec!["repo/moving.jsonnet".into()],
			output_does_not_reparse: vec!["repo/refuses.jsonnet".into()],
		};
		let staged = prepare_staging(&temporary.path().join("staging"), &[]).unwrap();
		let harness = Harness {
			tk: &script,
			rtk: &script,
			staged: &staged,
			known: &known,
			findings: Mutex::new(Vec::new()),
			observed_known: Mutex::new(BTreeSet::new()),
		};
		let mut tally = Tally::default();
		harness.second_run(Path::new("repo/moving.jsonnet"), b"moving\n", &mut tally);
		harness.second_run(Path::new("repo/refuses.jsonnet"), b"refuses\n", &mut tally);
		assert_eq!(tally.non_convergent, 1);
		assert_eq!(tally.refusing_to_reparse, 1);
		assert!(
			harness.findings.lock().unwrap().is_empty(),
			"{:?}",
			harness.findings.lock().unwrap()
		);
		assert_eq!(
			fs::read(staged.join("non_convergent/repo/moving.jsonnet")).unwrap(),
			b"moving\n"
		);
		harness.second_run(Path::new("repo/moving.jsonnet"), b"stable\n", &mut tally);
		harness.second_run(Path::new("repo/moving.jsonnet"), b"refuses\n", &mut tally);
		assert_eq!(
			harness.findings.lock().unwrap().len(),
			2,
			"stale/category transition must fail"
		);
	}

	#[cfg(unix)]
	#[test]
	fn listed_exceptions_do_not_hide_second_pass_parity_mismatches() {
		let temporary = tempfile::tempdir().unwrap();
		let tk = fake_formatter(
			&temporary.path().join("tk"),
			"cat >/dev/null; printf 'changed\\n'",
		);
		let rtk = fake_formatter(&temporary.path().join("rtk"), "cat");
		let known = KnownConfig {
			non_convergent: vec!["repo/input.jsonnet".into()],
			..KnownConfig::default()
		};
		let staged = prepare_staging(&temporary.path().join("staging"), &[]).unwrap();
		let harness = Harness {
			tk: &tk,
			rtk: &rtk,
			staged: &staged,
			known: &known,
			findings: Mutex::new(Vec::new()),
			observed_known: Mutex::new(BTreeSet::new()),
		};
		harness.second_run(
			Path::new("repo/input.jsonnet"),
			b"input\n",
			&mut Tally::default(),
		);
		assert!(harness
			.findings
			.lock()
			.unwrap()
			.iter()
			.any(|text| text.contains("second-pass parity differs")));
	}
	#[cfg(unix)]
	#[test]
	fn native_keys_preserve_every_filename_byte() {
		use std::os::unix::ffi::OsStringExt;
		let root = make_root("repo", "/corpus/repo");
		let filename = OsString::from_vec(b"a-\xff.jsonnet".to_vec());
		assert_eq!(
			root.key_for(&root.path.join(&filename)).unwrap(),
			Path::new("repo").join(filename)
		);
	}

	#[cfg(unix)]
	#[test]
	fn subprocess_backtraces_are_disabled_individually_and_together() {
		const PROBE: &str = "RTK_FMT_ACCEPTANCE_ENV_PROBE";
		if std::env::var_os(PROBE).is_some() {
			assert!(
				std::env::var("RUST_BACKTRACE").unwrap() == "1"
					|| std::env::var("RUST_LIB_BACKTRACE").unwrap() == "1"
			);
			let run = run_plain(
				"/bin/sh",
				&[
					"-c",
					"printf '%s/%s' \"$RUST_BACKTRACE\" \"$RUST_LIB_BACKTRACE\"",
				],
			)
			.unwrap();
			assert_eq!(run.code, Some(0));
			assert_eq!(run.stdout, b"0/0");
			assert!(run.stderr.is_empty());
			return;
		}
		for (backtrace, lib_backtrace) in [("1", "0"), ("0", "1"), ("1", "1")] {
			let output = Command::new(std::env::current_exe().unwrap())
				.args(["--exact", "subcommands::fmt_acceptance::tests::subprocess_backtraces_are_disabled_individually_and_together"])
				.env(PROBE, "1").env("RUST_BACKTRACE", backtrace).env("RUST_LIB_BACKTRACE", lib_backtrace)
				.output().unwrap();
			assert!(output.status.success(), "{:?}", output);
		}
	}
}
