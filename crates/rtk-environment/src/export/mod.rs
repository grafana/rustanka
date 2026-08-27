//! Exporting environments to a directory of Kubernetes manifests.
//!
//! Two entry points, both on [`Engine`]: [`Engine::export_single`] exports an
//! environment that has already been evaluated, and [`Engine::export_bulk`]
//! discovers environments under a set of paths and exports all of them.
//!
//! # How the work is spread out
//!
//! Evaluated Jsonnet values are `Rc`-based, so they never leave the thread that
//! produced them. Each environment is therefore evaluated, walked, serialized and
//! written all on the one worker thread that picked it up, and environments run
//! in parallel across a [`rayon`] pool fed straight from discovery.
//!
//! Nothing evaluated crosses a thread boundary: an environment arrives as owned
//! metadata, and leaves as a [`Report`] of what it wrote. Errors are rendered
//! where they happen, because Jsonnet's stack traces are `Rc`-based too.
//! Everything shared is immutable and behind an [`Arc`], apart from the abort
//! flag.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kube_core::{Selector, SelectorExt};
use rayon::iter::{ParallelBridge, ParallelIterator};
use rtk_jsonnet::jpath::JPath;
use rtk_spec::canonical::{Environment, EnvironmentSpec};
use rustc_hash::{FxHashMap, FxHashSet};
use tracing::{debug, trace};

use crate::export::manifest::Manifest;
use crate::export::template::{FilenameTemplate, SpecializedTemplate};
use crate::export::writer::{Directories, File, Written};
use crate::{Discover, Discovered, Engine, Search};

mod data;
mod load;
mod manifest;
mod process;
mod selector;
pub(crate) mod template;
mod writer;

pub use crate::export::data::OptionalData;
pub use crate::export::manifest::{InvalidMergeStrategy, MergeStrategy};
pub use crate::export::template::DEFAULT_FORMAT;

/// A Kubernetes label selector used to filter discovered environments.
pub struct LabelSelector(Selector);

impl LabelSelector {
	pub fn parse(input: &str) -> Result<LabelSelector, Error> {
		selector::parse(input).map(LabelSelector)
	}

	pub fn matches<'a, D>(&self, environment: &Environment<'a, D>) -> bool
	where
		D: rtk_spec::v1alpha1::EnvironmentData<'a>,
	{
		self.0
			.matches(environment.metadata.labels.as_ref().unwrap_or(&NO_LABELS))
	}
}

/// How many serialized manifests a worker sends at a time.
///
/// Environments can hold thousands of manifests, and this is what keeps a whole
/// environment's output from sitting in memory before any of it is written.
const CHUNK_SIZE: usize = 256;

/// An environment with no labels at all still has to be matched against.
static NO_LABELS: std::sync::LazyLock<std::collections::BTreeMap<String, String>> =
	std::sync::LazyLock::new(std::collections::BTreeMap::new);

/// An environment with its evaluated manifests attached.
///
/// Bare Jsonnet entrypoints have no Tanka environment configuration. They still
/// use the same evaluated-data representation, but [`LoadedEnvironment::environment`]
/// returns `None` so consumers do not apply settings that were never configured.
#[derive(Clone, Debug)]
pub struct LoadedEnvironment {
	environment: Environment<'static, OptionalData>,
	configured: bool,
}

impl LoadedEnvironment {
	fn configured(environment: Environment<'static, OptionalData>) -> LoadedEnvironment {
		LoadedEnvironment {
			environment,
			configured: true,
		}
	}

	fn bare(data: serde_json::Value) -> Result<LoadedEnvironment, Error> {
		let environment = Environment::new()
			.with_spec(EnvironmentSpec::default())
			.with_data(OptionalData::new(data))
			.build()
			.map_err(|source| Error::Spec { source })?;
		Ok(LoadedEnvironment {
			environment,
			configured: false,
		})
	}

	/// The configured environment, or `None` for a bare Jsonnet entrypoint.
	pub fn environment(&self) -> Option<&Environment<'static, OptionalData>> {
		self.configured.then_some(&self.environment)
	}

	/// The configured environment spec, or `None` for a bare entrypoint.
	pub fn spec(&self) -> Option<&EnvironmentSpec> {
		self.environment().map(|environment| &environment.spec)
	}

	/// The evaluated root value, if it was not `null`.
	pub fn data(&self) -> Option<&serde_json::Value> {
		self.environment.data.get()
	}

	/// Tanka's stable label for this configured environment.
	pub fn environment_label(&self) -> Option<String> {
		self.environment()
			.map(|environment| process::environment_label(&environment.metadata))
	}

	fn inner(&self) -> &Environment<'static, OptionalData> {
		&self.environment
	}

	/// Refuse to act on an environment that asked for a different Tanka.
	///
	/// tk compares the version it was built with against
	/// `spec.expectVersions.tanka` and refuses when it does not satisfy it. rtk
	/// answers for the Tanka it implements rather than for its own version:
	/// comparing `0.5.x` against a constraint like `>=0.20` would fail every
	/// real environment, and the question being asked is which Tanka's
	/// behaviour is on offer.
	fn check_expected_tanka_version(&self) -> Result<(), Error> {
		let Some(constraint) = self
			.environment()
			.and_then(|environment| environment.spec.expect_versions.as_ref())
			.and_then(|versions| versions.tanka.as_deref())
			.filter(|constraint| !constraint.is_empty())
		else {
			return Ok(());
		};

		let constraints = rtk_masterminds::Constraints::parse(constraint).map_err(|source| {
			Error::UnreadableTankaConstraint {
				reason: source.to_string(),
			}
		})?;

		if constraints.matches(&rtk_masterminds::tanka_version()) {
			return Ok(());
		}

		Err(Error::UnsatisfiedTankaVersion {
			constraint: constraint.to_owned(),
		})
	}

	fn processed_manifests(
		&self,
		targets: &process::Targets,
	) -> Result<Vec<serde_json::Value>, Error> {
		// tk checks this in `LoadManifests`, which is what export, show, diff,
		// apply and prune all go through and what `eval` and `env list` do not —
		// so this is the one place it belongs.
		self.check_expected_tanka_version()?;

		let Some(data) = self.data() else {
			return Ok(Vec::new());
		};

		let mut manifests = Vec::new();
		process::collect_manifests(data.clone(), "", &mut manifests)?;
		manifests.retain(|manifest| targets.keeps(manifest));

		// Namespaces, labels and resource defaults are what the spec says this
		// environment's resources are, so everything that reads manifests —
		// exporting, diffing, applying — wants them injected. A bare Jsonnet
		// entrypoint has no spec, and so gets no namespace.
		let processing = process::Processing::new(self.inner(), self.environment().is_some());
		for manifest in &mut manifests {
			processing.apply(manifest);
		}

		Ok(manifests)
	}
}

/// Options for both kinds of export.
#[derive(Clone, Debug)]
pub struct Options {
	/// Directory to export into.
	pub output_dir: PathBuf,
	/// Extension for exported files. The contents are always YAML, as in tk.
	pub extension: String,
	/// Filename template, in Go's `text/template` syntax. See [`DEFAULT_FORMAT`].
	pub format: String,
	/// Regular expressions matched against `<kind>/<name>`, selecting which
	/// resources to export. A leading `!` excludes instead.
	pub targets: Vec<String>,
	/// What to do when the output directory already holds an export.
	pub merge_strategy: MergeStrategy,
	/// Environments that have been deleted since the last export, whose files
	/// should be cleaned up.
	pub merge_deleted_environments: Vec<String>,
	/// Skip writing `manifest.json`.
	pub skip_manifest: bool,
	/// Collect timing for each environment.
	pub timing: bool,
	/// How many environments to evaluate at once. Bulk exports only.
	pub parallelism: usize,
	/// Export only environments whose name or path contains this. Bulk exports
	/// only.
	pub name: Option<String>,
	/// Export only environments whose labels match this selector, in `kubectl`
	/// syntax. Bulk exports only.
	pub selector: Option<String>,
	/// Export every environment found, rather than refusing an ambiguous set.
	/// Bulk exports only.
	pub recursive: bool,
	/// The directory an environment's `metadata.namespace` is resolved against
	/// when an export re-resolves it, defaulting to the process working
	/// directory. See [`Engine::reresolve`].
	pub working_directory: Option<PathBuf>,
}

impl Default for Options {
	fn default() -> Self {
		Options {
			output_dir: PathBuf::from("."),
			extension: "yaml".to_owned(),
			format: DEFAULT_FORMAT.to_owned(),
			targets: Vec::new(),
			merge_strategy: MergeStrategy::default(),
			merge_deleted_environments: Vec::new(),
			skip_manifest: false,
			timing: false,
			parallelism: 8,
			name: None,
			selector: None,
			recursive: false,
			working_directory: None,
		}
	}
}

/// Where an export's time went, when [`Options::timing`] is set.
#[derive(Clone, Copy, Debug, Default)]
pub struct TimingData {
	/// Evaluating Jsonnet, on a worker thread.
	pub evaluate: Duration,
	/// Walking, processing and serializing manifests, on a worker thread.
	pub serialize: Duration,
	/// Writing files, on the same worker thread.
	pub write: Duration,
	/// How many manifests the environment exported.
	pub manifests: usize,
}

/// What one environment exported.
#[derive(Clone, Debug)]
pub struct Report {
	/// Where the environment was found.
	pub source: Arc<PathBuf>,
	/// How `manifest.json` refers to the environment: its `metadata.namespace`,
	/// which is its entrypoint relative to its project root.
	pub identifier: String,
	/// Files written, relative to [`Options::output_dir`], sorted.
	pub files: Vec<PathBuf>,
	/// How many of those were already byte-identical, and so left alone.
	pub unchanged: usize,
	/// Why the environment failed, if it did.
	pub error: Option<Arc<Error>>,
	/// Timing, when [`Options::timing`] is set.
	pub timing: Option<TimingData>,
}

impl Report {
	/// A report for an environment found at `source`, read by `entrypoint`.
	///
	/// `namespace` is the environment's `metadata.namespace` — its entrypoint
	/// relative to its own project root — which is what tk records against every
	/// file it writes (`fileToEnv[relpath] = env.Metadata.Namespace`). It has to
	/// be that rather than the path the export was given: the index is read back
	/// by a later export to decide which files an environment owns, so a value
	/// that depended on the working directory would stop matching as soon as one
	/// ran from somewhere else.
	///
	/// A bare Jsonnet entrypoint is not a Tanka environment and has no namespace.
	/// tk cannot export one at all, so there is nothing to match: it falls back
	/// to the entrypoint, relative to the working directory where possible.
	fn from_entrypoint(
		source: Arc<PathBuf>,
		entrypoint: PathBuf,
		namespace: Option<&str>,
	) -> Report {
		let identifier = match namespace {
			Some(namespace) => namespace.to_owned(),
			None => std::env::current_dir()
				.ok()
				.and_then(|current_dir| {
					entrypoint
						.strip_prefix(current_dir)
						.ok()
						.map(Path::to_path_buf)
				})
				.unwrap_or(entrypoint)
				.to_string_lossy()
				.into_owned(),
		};

		Report {
			source,
			identifier,
			files: Vec::new(),
			unchanged: 0,
			error: None,
			timing: None,
		}
	}

	pub fn failed(&self) -> bool {
		self.error.is_some()
	}

	fn record_written(&mut self, written: impl IntoIterator<Item = Written>) {
		for written in written {
			if written.unchanged {
				self.unchanged += 1;
			}
			self.files.push(written.path);
		}
	}
}

/// What a bulk export exported.
#[derive(Clone, Debug, Default)]
pub struct Exported {
	/// One report per environment, in discovery order.
	pub reports: Vec<Report>,
}

impl Exported {
	/// Environments that were exported.
	pub fn successful(&self) -> usize {
		self.reports.len() - self.failed() - self.skipped()
	}

	/// Environments that were tried and failed.
	///
	/// Not the ones that never got their turn: an environment abandoned after
	/// something else went wrong has said nothing about itself, and counting it
	/// as a failure made the tally depend on how far the pool had got.
	pub fn failed(&self) -> usize {
		self.reports
			.iter()
			.filter(|report| report.error.as_ref().is_some_and(|error| !error.skipped()))
			.count()
	}

	/// Environments abandoned because an earlier failure stopped the export.
	///
	/// How many there are depends on how many workers had already started when
	/// the export was stopped, which is a fact about the run rather than about
	/// the environments. Only the total of this and [`Exported::successful`] is
	/// fixed for a given input.
	pub fn skipped(&self) -> usize {
		self.reports
			.iter()
			.filter(|report| report.error.as_ref().is_some_and(|error| error.skipped()))
			.count()
	}

	/// Every file the export wrote, relative to the output directory.
	pub fn files(&self) -> impl Iterator<Item = &Path> {
		self.reports
			.iter()
			.flat_map(|report| report.files.iter().map(PathBuf::as_path))
	}
}

/// Everything that can go wrong while exporting.
///
/// Failures that only concern one environment end up in its [`Report`]; the ones
/// [`Error::fatal`] returns `true` for stop the export.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("could not discover environments: {0}")]
	Discover(Box<String>),

	/// Rendered eagerly, rather than kept: Jsonnet errors carry `Rc`-based
	/// stack traces, which cannot leave the thread that produced them, and this
	/// has to travel from a worker back to the export's driver.
	#[error("could not evaluate the environment:\n{message}")]
	Evaluate { message: String },

	#[error("the environment is not valid")]
	Spec {
		#[source]
		source: rtk_spec::canonical::EnvironmentBuilderError,
	},

	#[error("could not resolve the environment's import paths")]
	JPath(#[from] rtk_jsonnet::jpath::Error),

	#[error("no environments matched (name: {name:?}, selector: {selector:?})")]
	NothingMatched {
		name: Option<String>,
		selector: Option<String>,
	},

	#[error("no environment found matching name '{name}'. Available environments: {available}")]
	NoEnvironmentNamed { name: String, available: String },

	#[error(
		"found multiple Environments in {path:?}. Use `--name` to select a single one: \n - {names}"
	)]
	MultipleEnvironments { path: String, names: String },

	#[error(
		"found multiple Environments in {path:?} matching {name:?}. Provide a more specific name that matches a single one: \n - {names}"
	)]
	MultipleEnvironmentsNamed {
		path: String,
		name: String,
		names: String,
	},

	#[error(
		"found {count} environments. Use --name to select one or --recursive to export all:\n  \
		 - {first}\n  - {second}"
	)]
	Ambiguous {
		count: usize,
		first: PathBuf,
		second: PathBuf,
	},

	#[error("could not start {parallelism} export workers")]
	Pool {
		parallelism: usize,
		source: rayon::ThreadPoolBuildError,
	},

	#[error(
		"output dir `{output_dir}` not empty. Pass a different --merge-strategy to ignore this"
	)]
	OutputDirNotEmpty { output_dir: PathBuf },

	#[error("file '{file}' written by multiple environments: '{first}' and '{second}'")]
	DuplicateFile {
		file: String,
		first: String,
		second: String,
	},

	#[error("file '{file}' already exists from environment '{owner}'. Aborting")]
	ForeignFile { file: String, owner: String },

	#[error(
		"found a tanka Environment resource. Check that you aren't using a spec.json and inline environments simultaneously"
	)]
	EnvironmentResource,

	#[error("found invalid Kubernetes object (at {path}): {reason}")]
	InvalidManifest { path: String, reason: String },

	#[error("invalid target pattern {target:?}")]
	InvalidTarget {
		target: String,
		#[source]
		source: regex::Error,
	},

	#[error("invalid selector {selector:?}: {reason}")]
	InvalidSelector { selector: String, reason: String },

	#[error("invalid filename format {format:?}")]
	InvalidFormat {
		format: String,
		#[source]
		source: Box<anyhow::Error>,
	},

	#[error("could not render a filename")]
	Render(#[source] anyhow::Error),

	#[error("the filename template rendered nothing for {manifest}")]
	EmptyFilename { manifest: String },

	#[error("could not serialize a manifest")]
	Serialize(#[source] anyhow::Error),

	#[error("could not write {path}")]
	Write {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},

	#[error("could not read the export index")]
	Index(#[from] serde_json::Error),

	#[error("could not read the environment the Jsonnet declares")]
	Environment {
		#[source]
		source: serde_json::Error,
	},

	#[error("skipped after an earlier fatal error")]
	Skipped,

	#[error("parsing version constraint: '{reason}'. Please check 'spec.expectVersions.tanka'")]
	UnreadableTankaConstraint { reason: String },

	#[error(
		"current version '{}' does not satisfy the version required by the environment: '{constraint}'. You likely need to use another version of Tanka",
		crate::TANKA_COMPATIBLE_VERSION
	)]
	UnsatisfiedTankaVersion { constraint: String },
}

impl Error {
	/// Whether this stops the whole export rather than one environment.
	pub fn fatal(&self) -> bool {
		matches!(
			self,
			Error::InvalidTarget { .. }
				| Error::InvalidSelector { .. }
				| Error::InvalidFormat { .. }
				| Error::Render(_)
				| Error::EmptyFilename { .. }
				| Error::Ambiguous { .. }
				| Error::MultipleEnvironmentsNamed { .. }
				| Error::OutputDirNotEmpty { .. }
				| Error::DuplicateFile { .. }
				| Error::ForeignFile { .. }
		)
	}

	/// Whether this stands in for an export that was never attempted.
	///
	/// One environment reported like this says nothing about itself: something
	/// else went wrong first, and this one never got its turn.
	pub fn skipped(&self) -> bool {
		matches!(self, Error::Skipped)
	}

	/// Render this error and its causes, the way a user wants to read them.
	///
	/// Most of what goes wrong here goes wrong underneath something: a failed
	/// write knows the path it was writing, but the reason it failed belongs to
	/// the error beneath it. Displaying one of these on its own leaves that out.
	pub fn report(&self) -> String {
		render(self)
	}
}

impl From<rtk_jsonnet::Error> for Error {
	fn from(error: rtk_jsonnet::Error) -> Self {
		Error::Evaluate {
			message: render(&error),
		}
	}
}

impl From<crate::Error> for Error {
	fn from(error: crate::Error) -> Self {
		match error {
			crate::Error::Evaluation(error) => error.into(),
			error => Error::Discover(Box::new(render(&error))),
		}
	}
}

/// Render an error and its causes, the way a user wants to read them.
fn render(error: &(dyn std::error::Error + 'static)) -> String {
	use std::fmt::Write as _;

	let mut rendered = error.to_string();
	let mut source = error.source();
	while let Some(cause) = source {
		let _ = write!(&mut rendered, "\n  caused by: {cause}");
		source = cause.source();
	}
	rendered
}

/// Everything a worker reports has to be able to leave the worker's thread, and
/// reports are shared with whoever asked for the export.
const _: () = {
	const fn assert_send_sync<T: Send + Sync>() {}
	assert_send_sync::<Error>();
	assert_send_sync::<Report>();
	assert_send_sync::<Exported>();
};

/// The immutable half of an export, shared with every worker.
#[derive(Debug)]
struct Export {
	options: Options,
	/// Compiled once, rather than per environment.
	template: FilenameTemplate,
	targets: process::Targets,
	/// Set when an environment fails fatally, so workers stop starting new ones.
	abort: AtomicBool,
}

impl Export {
	fn new(options: &Options) -> Result<Arc<Export>, Error> {
		Ok(Arc::new(Export {
			template: FilenameTemplate::new(&options.format)?,
			targets: process::Targets::compile(&options.targets)?,
			options: options.clone(),
			abort: AtomicBool::new(false),
		}))
	}

	/// Where an environment's namespace is resolved from when it is re-resolved.
	///
	/// The process working directory, unless the caller named one — which tests
	/// and the golden harness do, so that a fixture can be exported from its own
	/// root without the process having to `chdir` into it.
	fn working_directory(&self) -> Option<PathBuf> {
		self.options
			.working_directory
			.clone()
			.or_else(|| std::env::current_dir().ok())
	}

	fn aborted(&self) -> bool {
		self.abort.load(Ordering::Relaxed)
	}

	fn abort(&self) {
		self.abort.store(true, Ordering::Relaxed);
	}

	/// Work out what an environment exports, without serializing any of it yet.
	fn plan(&self, environment: &LoadedEnvironment) -> Result<Plan, Error> {
		let manifests = environment.processed_manifests(&self.targets)?;

		Ok(Plan {
			template: (!manifests.is_empty())
				.then(|| self.template.specialize(environment.inner()))
				.transpose()?,
			manifests,
		})
	}

	/// Render and serialize one chunk of a [`Plan`].
	fn serialize_chunk(
		&self,
		chunk: &[serde_json::Value],
		plan: &Plan,
	) -> Result<Vec<File>, Error> {
		let template = plan
			.template
			.as_ref()
			.expect("a plan with manifests has a template");

		chunk
			.iter()
			.map(|manifest| {
				Ok(File {
					path: template.render_path(manifest, &self.options.extension)?,
					contents: process::serialize(manifest)?,
				})
			})
			.collect()
	}
}

/// The manifests an environment exports, before they are serialized.
#[derive(Default)]
struct Plan {
	manifests: Vec<serde_json::Value>,
	/// How to name each manifest. Absent only when there is nothing to export.
	template: Option<SpecializedTemplate>,
}

/// Work out which environment owns each written file, and what that conflicts
/// with.
///
/// Every report that wrote something is considered, failed or not: the files are
/// on disk either way, so they need an owner and they can still collide.
///
/// Claimed files are removed from `superseded`, which is left partly reduced
/// when a conflict stops the walk — which is why the caller must not prune on
/// the strength of it.
fn claim_files(
	exported: &Exported,
	index: &Manifest,
	superseded: &mut FxHashSet<String>,
) -> Option<Error> {
	let mut owners: FxHashMap<String, &str> = FxHashMap::default();

	for report in &exported.reports {
		for file in &report.files {
			let file = manifest::relative_key(file);

			if let Some(first) = owners.get(&file) {
				return Some(Error::DuplicateFile {
					file,
					first: (*first).to_owned(),
					second: report.identifier.clone(),
				});
			}

			// A file in the index that no exported environment is replacing
			// belongs to somebody else.
			if !superseded.contains(&file)
				&& let Some(owner) = index.owner(&file)
				&& owner != report.identifier
			{
				return Some(Error::ForeignFile {
					file,
					owner: owner.to_owned(),
				});
			}

			superseded.remove(&file);
			owners.insert(file, &report.identifier);
		}
	}

	None
}

/// Whether a manifest is a Tanka `Environment` rather than a Kubernetes resource.
///
/// tk filters with `(?i)^Environment/.*$` against `<kind>/<name>`, so the kind is
/// compared without regard to case and the name is not compared at all.
fn is_environment_resource(manifest: &serde_json::Value) -> bool {
	manifest
		.get("kind")
		.and_then(serde_json::Value::as_str)
		.is_some_and(|kind| kind.eq_ignore_ascii_case("Environment"))
}

impl Plan {
	fn manifests(&self) -> usize {
		self.manifests.len()
	}

	fn chunks(&self) -> impl Iterator<Item = &[serde_json::Value]> {
		self.manifests.chunks(CHUNK_SIZE)
	}
}

impl Engine {
	/// Return the processed, validated manifests from an evaluated environment.
	///
	/// This does not compile filename templates, serialize YAML, write files, or
	/// update an export index.
	pub fn manifests(
		&self,
		environment: &LoadedEnvironment,
		targets: &[String],
	) -> Result<Vec<serde_json::Value>, Error> {
		let targets = process::Targets::compile(targets)?;
		let manifests = environment.processed_manifests(&targets)?;

		// tk's `Load` refuses these once the manifests are processed: an
		// Environment is not a Kubernetes resource and cannot be applied, so
		// every command that reaches a cluster rejects one — and so does `show`,
		// which previews what those commands would send. Exporting keeps them,
		// which is why this belongs here rather than in `plan`.
		if manifests.iter().any(is_environment_resource) {
			return Err(Error::EnvironmentResource);
		}

		Ok(manifests)
	}

	/// Export one environment that has already been evaluated.
	///
	/// `source` is where the environment came from — its directory, or its
	/// entrypoint. `manifest.json` records the exported files against it, so a
	/// later export can tell which environment they belong to.
	pub fn export_single(
		&self,
		environment: &LoadedEnvironment,
		source: &Path,
		options: &Options,
	) -> Result<Report, Error> {
		let export = Export::new(options)?;
		export.prepare_output_dir()?;

		let entrypoint = if source.is_file() {
			source.to_path_buf()
		} else {
			source.join(JPath::DEFAULT_ENTRYPOINT)
		};
		let mut report = Report::from_entrypoint(
			Arc::new(source.to_path_buf()),
			entrypoint,
			environment
				.environment()
				.and_then(|environment| environment.metadata.namespace.as_deref()),
		);
		let mut timing = options.timing.then(TimingData::default);

		let planned = Instant::now();
		let plan = export.plan(environment)?;
		let mut serializing = planned.elapsed();
		let mut writing = Duration::ZERO;
		let mut directories = Directories::default();
		let mut written = Vec::new();

		for chunk in plan.chunks() {
			let serialized = Instant::now();
			let files = export.serialize_chunk(chunk, &plan)?;
			serializing += serialized.elapsed();

			let queued = Instant::now();
			directories.write_files(&options.output_dir, files, &mut written)?;
			writing += queued.elapsed();
		}

		report.record_written(written);
		if let Some(timing) = timing.as_mut() {
			timing.serialize = serializing;
			timing.write = writing;
			timing.manifests = plan.manifests();
		}
		report.timing = timing;

		if !options.skip_manifest {
			let mut index = Manifest::read(&options.output_dir)?;
			index.record(
				&report.identifier,
				report.files.iter().map(PathBuf::as_path),
			);
			index.write()?;
		}

		Ok(report)
	}

	/// Discover environments under `paths` and export all of them.
	pub fn export_bulk(&self, paths: Vec<PathBuf>, options: &Options) -> Result<Exported, Error> {
		let export = Export::new(options)?;
		// `--recursive` is what tk uses to choose between walking a tree and
		// loading the path it was handed, so it chooses here too.
		let search = if options.recursive {
			Search::Tree
		} else {
			Search::Environment
		};
		let matching = Matching {
			path: paths
				.iter()
				.map(|path| path.display().to_string())
				.collect::<Vec<_>>()
				.join(", "),
			discover: self.discover(paths, search),
			name: options.name.clone(),
			selector: options
				.selector
				.as_deref()
				.map(selector::parse)
				.transpose()?,
			recursive: options.recursive,
		};

		let mut environments = matching.select()?.peekable();

		// Now that there is an environment to export, somewhere to put it. Left
		// until here so that finding none leaves the filesystem alone, and so that
		// an ambiguous set is refused before anything is created.
		match environments.peek() {
			// A discovery error is the export's error, not an environment's.
			Some(Err(_)) => {
				return Err(environments
					.next()
					.expect("just peeked")
					.expect_err("just peeked an error"));
			}
			Some(Ok(_)) => export.prepare_output_dir()?,
			None => {}
		}

		let parallelism = options.parallelism.max(1);
		let pool = rayon::ThreadPoolBuilder::new()
			.num_threads(parallelism)
			// Jsonnet evaluation recurses deeply.
			.stack_size(8 * 1024 * 1024)
			.build()
			.map_err(|source| Error::Pool {
				parallelism,
				source,
			})?;

		// Discovery is pulled from the pool rather than up front, so that
		// evaluating one environment overlaps with discovering the next.
		//
		// Every environment produces a slot, failures included, so that this
		// cannot end early and leave what has already been written unrecorded.
		// Which failure is reported is then a decision made below, on results
		// put back in order, rather than whichever worker got there first.
		let engine = self.clone();
		let mut results: Vec<(usize, Result<Report, Error>)> = pool.install(|| {
			environments
				.enumerate()
				.par_bridge()
				.map(|(index, discovered)| {
					let result = match discovered {
						Ok(discovered) => Ok(export.export_environment(&engine, &discovered)),
						Err(error) => {
							// A directory that cannot be read is not one
							// environment's failure, and there is no telling what
							// the rest of the walk holds: nothing further is
							// evaluated or written.
							export.abort();
							Err(error)
						}
					};
					(index, result)
				})
				.collect()
		});

		// Environments finish in whatever order the pool gets to them, which
		// should not show up in the result.
		results.sort_by_key(|(index, _)| *index);

		let mut reports = Vec::with_capacity(results.len());
		let mut discovery_failure = None;
		for (_, result) in results {
			match result {
				Ok(report) => reports.push(report),
				Err(error) => {
					discovery_failure.get_or_insert(error);
				}
			}
		}
		let exported = Exported { reports };

		// A recursive export filters what it walked over and exports whatever
		// survived, which tk is content to have be nothing: `--name` and
		// `--selector` are a filter there, not a lookup. Asking for one
		// environment and not finding it is a different matter.
		if exported.reports.is_empty()
			&& discovery_failure.is_none()
			&& !options.recursive
			&& (options.name.is_some() || options.selector.is_some())
		{
			return Err(Error::NothingMatched {
				name: options.name.clone(),
				selector: options.selector.clone(),
			});
		}

		// An export that found nothing to do leaves no trace of having run, as tk
		// does not: no output directory, and no index claiming it is empty.
		//
		// One that did write something records it even when it is about to fail,
		// so that the index describes the directory rather than the export that
		// was meant to happen. A failure here displaces the one that stopped the
		// export, being about what the filesystem is left holding.
		if !exported.reports.is_empty() {
			export.reconcile(&exported)?;
		}

		match discovery_failure {
			Some(error) => Err(error),
			None => Ok(exported),
		}
	}
}

/// Discovery, filtered by `--name` and `--selector`.
struct Matching {
	discover: Discover,
	/// What was asked for, as it was written, for an error to name it the way
	/// tk's `ErrMultipleEnvs` does.
	path: String,
	name: Option<String>,
	selector: Option<Selector>,
	recursive: bool,
}

impl Matching {
	/// Whether `--name` selects this environment.
	///
	/// Tanka has two rules and chooses between them by command rather than by
	/// what it finds. A recursive export compares the name exactly, in
	/// `cmd/tk/export.go`. Everything else loads a single environment through a
	/// loader: the inline one matches a substring, because `SingleEnvEvalScript`
	/// asks `std.member`, and the static one ignores the filter altogether,
	/// since a static environment is named after its directory rather than by
	/// the Jsonnet being selected from.
	///
	/// What Tanka never does is compare the name against a path. rtk used to,
	/// which meant a filter could be satisfied by the directory someone happened
	/// to check the repository out into.
	fn named(&self, discovered: &Discovered) -> bool {
		let Some(name) = self.name.as_deref() else {
			return true;
		};

		if !self.recursive && discovered.is_static {
			return true;
		}

		let Some(environment) = discovered.environment.metadata.name.as_deref() else {
			return false;
		};

		if self.recursive {
			environment == name
		} else {
			environment.contains(name)
		}
	}

	fn matches(&self, discovered: &Discovered) -> bool {
		if !self.named(discovered) {
			return false;
		}

		if let Some(selector) = self.selector.as_ref() {
			let labels = discovered.environment.metadata.labels.as_ref();
			if !selector.matches(labels.unwrap_or(&NO_LABELS)) {
				return false;
			}
		}

		true
	}

	/// Which environments an export will export.
	///
	/// A recursive export streams, so that evaluating one environment overlaps
	/// with discovering the next: its `--name` is an exact comparison, which
	/// cannot become ambiguous, and nothing downstream needs the whole set.
	///
	/// Everything else has to look at every environment before dispatching any
	/// of them. A substring `--name` may turn out to match one environment
	/// exactly and others only in part, and Tanka prefers the exact one however
	/// late it is found; what is still ambiguous after that is refused, as is an
	/// export given neither `--name` nor `--recursive`.
	fn select(self) -> Result<Box<dyn Iterator<Item = Result<Discovered, Error>> + Send>, Error> {
		if self.recursive {
			return Ok(Box::new(self));
		}

		let name = self.name.clone();
		let self_path = self.path.clone();
		let mut found = self.collect::<Result<Vec<_>, _>>()?;

		let exactly = |discovered: &Discovered, name: &str| {
			discovered.environment.metadata.name.as_deref() == Some(name)
		};
		if let Some(name) = name.as_deref()
			&& found.iter().any(|discovered| exactly(discovered, name))
		{
			found.retain(|discovered| exactly(discovered, name));
		}

		if found.len() < 2 {
			return Ok(Box::new(found.into_iter().map(Ok)));
		}

		Err(match name {
			Some(name) => {
				let mut names = found
					.iter()
					.filter_map(|discovered| discovered.environment.metadata.name.as_deref())
					.collect::<Vec<_>>();
				names.sort_unstable();
				Error::MultipleEnvironmentsNamed {
					path: self_path,
					name,
					names: names.join("\n - "),
				}
			}
			None => Error::Ambiguous {
				count: found.len(),
				first: found[0].path.as_ref().clone(),
				second: found[1].path.as_ref().clone(),
			},
		})
	}
}

impl Iterator for Matching {
	type Item = Result<Discovered, Error>;

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			match self.discover.next()? {
				Ok(discovered) if self.matches(&discovered) => return Some(Ok(discovered)),
				Ok(_) => {}
				Err(error) => return Some(Err(error.into())),
			}
		}
	}
}

/// Serialize one processed manifest using Tanka's YAML formatting.
pub fn serialize_manifest(manifest: &serde_json::Value) -> Result<String, Error> {
	process::serialize(manifest)
}

impl Export {
	/// Evaluate, serialize and write one environment, all on the thread that
	/// picked it up.
	///
	/// An environment that fails reports its failure rather than returning it:
	/// one broken environment does not stop the others, and what it did manage to
	/// write is still worth reporting.
	fn export_environment(&self, engine: &Engine, discovered: &Discovered) -> Report {
		// tk reloads an environment from its namespace rather than from where it
		// was found, which usually lands back on the same environment.
		let reresolved = self
			.working_directory()
			.and_then(|working_directory| engine.reresolve(discovered, &working_directory));
		let discovered = reresolved.as_ref().unwrap_or(discovered);

		let mut report = Report::from_entrypoint(
			Arc::clone(&discovered.path),
			discovered.entrypoint.as_ref().clone(),
			discovered.environment.metadata.namespace.as_deref(),
		);

		match self.run_environment(engine, discovered, &mut report) {
			Ok(timing) => report.timing = timing,
			Err(error) => {
				if error.fatal() {
					self.abort();
				}
				report.error = Some(Arc::new(error));
			}
		}

		// Files land in whatever order they were serialized, which should not show
		// up in the result.
		report.files.sort();
		report
	}

	fn run_environment(
		&self,
		engine: &Engine,
		discovered: &Discovered,
		report: &mut Report,
	) -> Result<Option<TimingData>, Error> {
		if self.aborted() {
			return Err(Error::Skipped);
		}

		let mut timing = self.options.timing.then(TimingData::default);
		debug!(environment = ?discovered.path, "exporting");

		let evaluate_started = Instant::now();
		let environment = engine.load(discovered)?;
		if let Some(timing) = timing.as_mut() {
			timing.evaluate = evaluate_started.elapsed();
		}

		let planned = Instant::now();
		let plan = self.plan(&environment)?;
		let mut serializing = planned.elapsed();
		let mut writing = Duration::ZERO;

		// Written as they are serialized, and recorded as they are written, so
		// that an environment that fails part way through still reports what it
		// put on disk.
		let mut directories = Directories::default();
		for chunk in plan.chunks() {
			let serialized = Instant::now();
			let files = self.serialize_chunk(chunk, &plan)?;
			serializing += serialized.elapsed();

			let queued = Instant::now();
			let mut written = Vec::new();
			let outcome = directories.write_files(&self.options.output_dir, files, &mut written);
			writing += queued.elapsed();
			// Recorded whether or not the chunk finished: up to `CHUNK_SIZE`
			// files can already be on disk when one of them fails.
			report.record_written(written);
			outcome?;
		}

		let manifests = plan.manifests();
		if let Some(timing) = timing.as_mut() {
			timing.serialize = serializing;
			timing.write = writing;
			timing.manifests = manifests;
		}

		trace!(environment = ?discovered.path, manifests, "exported");
		Ok(timing)
	}

	/// Refuse to export into a directory that already holds an export, unless
	/// told otherwise, and make sure it exists.
	fn prepare_output_dir(&self) -> Result<(), Error> {
		let output_dir = &self.options.output_dir;

		if self.options.merge_strategy == MergeStrategy::None
			&& !manifest::is_empty_dir(output_dir)?
		{
			return Err(Error::OutputDirNotEmpty {
				output_dir: output_dir.clone(),
			});
		}

		std::fs::create_dir_all(output_dir).map_err(|source| Error::Write {
			path: output_dir.clone(),
			source,
		})
	}

	/// Check what the export wrote against what was there before, then update the
	/// index.
	///
	/// Conflicts are only detectable once every environment has been exported,
	/// since any two of them could produce the same file.
	fn reconcile(&self, exported: &Exported) -> Result<(), Error> {
		let options = &self.options;
		let mut index = Manifest::read(&options.output_dir)?;

		// Files the exported environments wrote last time can be overwritten and,
		// if they are not written again, should be deleted.
		let mut superseded = if options.merge_strategy == MergeStrategy::ReplaceEnvironments {
			index.files_of(
				exported
					.reports
					.iter()
					.filter(|report| !report.failed())
					.map(|report| report.identifier.as_str()),
			)
		} else {
			FxHashSet::default()
		};
		superseded.extend(index.files_of(&options.merge_deleted_environments));

		let conflict = claim_files(exported, &index, &mut superseded);

		// A conflict is only found once everything has been written, so the files
		// are already on disk. Nothing is deleted on the strength of a run that
		// did not finish, but the index still has to describe the directory:
		// files it does not mention are the ones `fail-on-conflicts` cannot
		// protect and `replace-envs` will not prune.
		let removed = if conflict.is_none() {
			// Only what actually went may be forgotten. Pruning is best effort,
			// and a file it could not delete is still there and still owned.
			manifest::prune(&options.output_dir, &superseded)
		} else {
			FxHashSet::default()
		};

		if !options.skip_manifest {
			index.forget(&removed);
			for report in &exported.reports {
				index.record(
					&report.identifier,
					report.files.iter().map(PathBuf::as_path),
				);
			}
			index.write()?;
		}

		match conflict {
			Some(conflict) => Err(conflict),
			None => Ok(()),
		}
	}
}
