//! Exporting environments to a directory of Kubernetes manifests.
//!
//! Two entry points, both on [`Engine`]: [`Engine::export_single`] exports an
//! environment that has already been evaluated, and [`Engine::export_bulk`]
//! discovers environments under a set of paths and exports all of them.
//!
//! # How the work is spread out
//!
//! Evaluated Jsonnet values are `Rc`-based, so they never leave the thread that
//! produced them. Each environment is therefore evaluated, walked and serialized
//! on one worker thread, which sends the finished text of its manifests to the
//! thread driving the export. That thread interleaves three things: pulling the
//! next environment out of discovery (which evaluates Jsonnet too, so it also has
//! to stay put), handing environments to workers, and writing what comes back.
//!
//! Everything that crosses a thread boundary is a `String` or a `PathBuf`, and
//! everything shared is immutable and behind an [`Arc`], apart from the abort
//! flag.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kube_core::{Selector, SelectorExt};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rtk_jsonnet::jpath::JPath;
use rtk_spec::canonical::Environment;
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, trace};

use crate::export::manifest::Manifest;
use crate::export::template::{FilenameTemplate, SpecializedTemplate};
use crate::export::writer::{File, Writer, Written};
use crate::{Discover, Discovered, Engine};

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

/// How many serialized manifests a worker sends at a time.
///
/// Environments can hold thousands of manifests, and this is what keeps a whole
/// environment's output from sitting in memory before any of it is written.
const CHUNK_SIZE: usize = 256;

/// An environment with no labels at all still has to be matched against.
static NO_LABELS: std::sync::LazyLock<std::collections::BTreeMap<String, String>> =
	std::sync::LazyLock::new(std::collections::BTreeMap::new);

/// An environment with its evaluated manifests attached.
pub type LoadedEnvironment = Environment<'static, OptionalData>;

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
	/// How many files to write at once.
	pub write_concurrency: usize,
	/// Export only environments whose name or path contains this. Bulk exports
	/// only.
	pub name: Option<String>,
	/// Export only environments whose labels match this selector, in `kubectl`
	/// syntax. Bulk exports only.
	pub selector: Option<String>,
	/// Export every environment found, rather than refusing an ambiguous set.
	/// Bulk exports only.
	pub recursive: bool,
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
			write_concurrency: 16,
			name: None,
			selector: None,
			recursive: false,
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
	/// Writing files, on the driving thread.
	pub write: Duration,
	/// How many manifests the environment exported.
	pub manifests: usize,
}

/// What one environment exported.
#[derive(Clone, Debug)]
pub struct Report {
	/// Where the environment was found.
	pub source: Arc<PathBuf>,
	/// How `manifest.json` refers to the environment: its entrypoint, relative to
	/// the working directory where possible.
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
	fn new(source: Arc<PathBuf>, identifier: String) -> Report {
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
}

/// What a bulk export exported.
#[derive(Clone, Debug, Default)]
pub struct Exported {
	/// One report per environment, in discovery order.
	pub reports: Vec<Report>,
}

impl Exported {
	pub fn successful(&self) -> usize {
		self.reports.len() - self.failed()
	}

	pub fn failed(&self) -> usize {
		self.reports.iter().filter(|report| report.failed()).count()
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

	#[error(
		"found {count} environments. Use --name to select one or --recursive to export all:\n  \
		 - {first}\n  - {second}"
	)]
	Ambiguous {
		count: usize,
		first: PathBuf,
		second: PathBuf,
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

	#[error("found invalid Kubernetes object (at {path}): missing attribute \"apiVersion\"")]
	MissingApiVersion { path: String },

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

	#[error("a worker stopped before finishing")]
	WorkerLost,

	#[error("skipped after an earlier fatal error")]
	Skipped,
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
	targets: Vec<process::TargetMatcher>,
	/// Set when an environment fails fatally, so workers stop starting new ones.
	abort: AtomicBool,
}

impl Export {
	fn new(options: &Options) -> Result<Arc<Export>, Error> {
		Ok(Arc::new(Export {
			template: FilenameTemplate::new(&options.format)?,
			targets: process::TargetMatcher::compile(&options.targets)?,
			options: options.clone(),
			abort: AtomicBool::new(false),
		}))
	}

	fn aborted(&self) -> bool {
		self.abort.load(Ordering::Relaxed)
	}

	fn abort(&self) {
		self.abort.store(true, Ordering::Relaxed);
	}

	/// Work out what an environment exports, without serializing any of it yet.
	fn plan<'e>(&self, environment: &'e LoadedEnvironment) -> Result<Plan<'e>, Error> {
		let Some(data) = environment.data.get() else {
			return Ok(Plan::default());
		};

		let mut manifests = Vec::new();
		let mut buffer = String::new();
		process::collect_manifests(data, "", &mut buffer, &mut manifests)?;

		if !self.targets.is_empty() {
			manifests.retain(|manifest| process::keep_target(manifest, &self.targets));
		}

		Ok(Plan {
			parts: Some((
				self.template.specialize(environment)?,
				process::Processor::new(environment),
			)),
			manifests,
		})
	}

	/// Render and serialize one chunk of a [`Plan`].
	///
	/// The work is spread over the pool, so an environment with thousands of
	/// manifests is not serialized one at a time while the writes wait.
	fn serialize_chunk(&self, chunk: &[Value], plan: &Plan<'_>) -> Result<Vec<File>, Error> {
		let (template, processor) = plan
			.parts
			.as_ref()
			.expect("a plan with manifests has a template");

		chunk
			.par_iter()
			.map(|manifest| {
				let mut manifest = manifest.clone();
				processor.process(&mut manifest);

				let rendered = template.render(&manifest)?;
				let path =
					template::to_relative_path(&rendered, &self.options.extension, &manifest)?;

				Ok(File {
					path,
					contents: process::serialize(manifest)?,
				})
			})
			.collect()
	}
}

/// The manifests an environment exports, before they are serialized.
///
/// Holds nothing from the evaluation the manifests came out of, so the threads
/// serializing them do not have to reach back into it.
#[derive(Default)]
struct Plan<'e> {
	manifests: Vec<Value>,
	/// How to name and process each manifest. Absent only when there is nothing
	/// to export.
	parts: Option<(SpecializedTemplate, process::Processor<'e>)>,
}

impl Plan<'_> {
	fn manifests(&self) -> usize {
		self.manifests.len()
	}

	fn chunks(&self) -> impl Iterator<Item = &[Value]> {
		self.manifests.chunks(CHUNK_SIZE)
	}
}

/// What a worker sends back to the thread driving the export.
enum Message {
	/// Some of an environment's manifests, serialized and ready to write.
	Chunk { index: usize, files: Vec<File> },
	/// An environment is done, successfully or not.
	Finished {
		index: usize,
		outcome: Result<Option<TimingData>, Error>,
	},
}

impl Engine {
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
		prepare_output_dir(&export)?;

		let identifier = identify(source);
		let mut report = Report::new(Arc::new(source.to_path_buf()), identifier.clone());
		let mut timing = options.timing.then(TimingData::default);

		let planned = Instant::now();
		let plan = export.plan(environment)?;
		let mut serializing = planned.elapsed();
		let mut writing = Duration::ZERO;

		let written = writer::drive(|| async {
			let mut writer = Writer::new(options.output_dir.clone(), options.write_concurrency);
			let mut written = Vec::new();

			// Serializing a chunk blocks this thread, but the writes already
			// queued keep going: Tokio runs them on its blocking pool.
			for chunk in plan.chunks() {
				let serialized = Instant::now();
				let files = export.serialize_chunk(chunk, &plan)?;
				serializing += serialized.elapsed();

				let queued = Instant::now();
				writer.write(0, files, &mut written).await?;
				writing += queued.elapsed();
			}

			let drained = Instant::now();
			writer.drain(&mut written).await?;
			writing += drained.elapsed();

			Ok::<_, Error>(written)
		})?;

		collect_written(&mut report, written);
		if let Some(timing) = timing.as_mut() {
			timing.serialize = serializing;
			timing.write = writing;
			timing.manifests = plan.manifests();
		}
		report.timing = timing;

		if !options.skip_manifest {
			let mut index = Manifest::read(&options.output_dir)?;
			index.record(&identifier, report.files.iter().map(PathBuf::as_path));
			index.write()?;
		}

		Ok(report)
	}

	/// Discover environments under `paths` and export all of them.
	pub fn export_bulk(&self, paths: Vec<PathBuf>, options: &Options) -> Result<Exported, Error> {
		let export = Export::new(options)?;
		let selector = options
			.selector
			.as_deref()
			.map(selector::parse)
			.transpose()?;

		let pool = rayon::ThreadPoolBuilder::new()
			.num_threads(options.parallelism.max(1))
			// Jsonnet evaluation recurses deeply.
			.stack_size(8 * 1024 * 1024)
			.build()
			.expect("a rayon pool can be built");

		let engine = self.clone();
		let exported = writer::drive(|| async {
			// Discovery is built here rather than by the caller: it evaluates
			// Jsonnet, so it has to live on whichever thread ends up driving the
			// export.
			let mut driver = BulkExport {
				discover: Some(engine.discover(paths)),
				export: Arc::clone(&export),
				engine,
				pool: &pool,
				selector,
				pending: Vec::new(),
				dispatched: 0,
				outstanding: FxHashSet::default(),
				reports: Vec::new(),
				prepared: false,
			};
			driver.run().await
		})?;

		// An export that found nothing to do leaves no trace of having run, as tk
		// does not: no output directory, and no index claiming it is empty.
		if !exported.reports.is_empty() {
			reconcile(&export, &exported)?;
		}

		Ok(exported)
	}
}

/// The driver of a bulk export: discovers, dispatches and writes, in that order
/// of preference, on one thread.
struct BulkExport<'p> {
	export: Arc<Export>,
	engine: Engine,
	pool: &'p rayon::ThreadPool,
	/// Discovery evaluates Jsonnet, so it stays on this thread, in between
	/// writes.
	discover: Option<Discover>,
	selector: Option<Selector>,
	/// Environments pulled from discovery but not dispatched yet, because the
	/// ambiguity check had to look ahead.
	pending: Vec<Discovered>,
	dispatched: usize,
	/// Environments handed to a worker that have not reported back yet.
	outstanding: FxHashSet<usize>,
	reports: Vec<Report>,
	/// Whether the output directory has been made yet. Deferred until there is
	/// something to put in it.
	prepared: bool,
}

#[expect(
	clippy::future_not_send,
	reason = "the driver owns discovery, which holds evaluated Jsonnet values: \
	          the whole point is that it stays on one thread"
)]
impl BulkExport<'_> {
	async fn run(&mut self) -> Result<Exported, Error> {
		self.prioritize_exact_name()?;
		self.check_ambiguity()?;

		let (sender, mut receiver) = mpsc::channel(self.parallelism() * 2);
		let mut sender = Some(sender);
		let mut writer = Writer::new(
			self.export.options.output_dir.clone(),
			self.export.options.write_concurrency,
		);
		let mut written = Vec::new();

		loop {
			// Finished writes first, so the writer has room for more.
			while let Some(harvested) = writer.try_harvest() {
				written.push(harvested?);
			}

			// Then whatever the workers have already produced.
			while let Ok(message) = receiver.try_recv() {
				self.receive(message, &mut writer, &mut written).await?;
			}

			// Then, if there is room, one more environment. This is the blocking
			// step: discovery evaluates the Jsonnet that declares inline
			// environments.
			if sender.is_some() && self.has_room(&writer) {
				let dispatched = {
					let sender = sender.as_ref().expect("just checked");
					self.dispatch(sender)?
				};
				if dispatched {
					continue;
				}
				// Discovery is done. Dropping our sender lets the channel close
				// once the last worker drops its clone.
				sender = None;
				continue;
			}

			if sender.is_none() && self.outstanding.is_empty() && writer.is_idle() {
				break;
			}

			// Nothing left to do but wait for a worker or a write.
			tokio::select! {
				biased;
				message = receiver.recv(), if !self.outstanding.is_empty() => {
					if let Some(message) = message {
						self.receive(message, &mut writer, &mut written).await?;
					} else {
						// Every sender is gone with environments still
						// outstanding, which means a worker died mid-flight.
						// Whatever the others exported is still worth reporting.
						self.abandon();
					}
				}
				harvested = writer.harvest(), if !writer.is_idle() => {
					if let Some(harvested) = harvested? {
						written.push(harvested);
					}
				}
			}
		}

		writer.drain(&mut written).await?;
		self.finish(written)
	}

	/// Prefer exact environment names over substring and path matches, as tk
	/// does. Discovery has to finish before anything is dispatched: an exact
	/// match may appear after an otherwise valid partial match.
	fn prioritize_exact_name(&mut self) -> Result<(), Error> {
		let Some(name) = self.export.options.name.clone() else {
			return Ok(());
		};

		let mut exact = Vec::new();
		let mut partial = Vec::new();
		while let Some(discover) = self.discover.as_mut() {
			let Some(discovered) = writer::blocking(|| discover.next()) else {
				self.discover = None;
				break;
			};
			let discovered = discovered?;
			if !self.matches(&discovered) {
				continue;
			}

			if discovered.environment.metadata.name.as_deref() == Some(name.as_str()) {
				exact.push(discovered);
			} else {
				partial.push(discovered);
			}
		}

		let selected = if exact.is_empty() { partial } else { exact };
		self.pending.extend(selected.into_iter().rev());
		Ok(())
	}

	fn parallelism(&self) -> usize {
		self.export.options.parallelism.max(1)
	}

	/// Whether another environment can be started without running too far ahead
	/// of the writes.
	fn has_room(&self, writer: &Writer) -> bool {
		self.outstanding.len() < self.parallelism() * 2 && !writer.is_saturated()
	}

	/// Refuse an ambiguous export, as tk does, before dispatching any work.
	fn check_ambiguity(&mut self) -> Result<(), Error> {
		if self.export.options.recursive || self.export.options.name.is_some() {
			return Ok(());
		}

		let Some(first) = self.next_environment()? else {
			return Ok(());
		};
		let Some(second) = self.next_environment()? else {
			self.pending.push(first);
			return Ok(());
		};

		let mut count = 2;
		while self.next_environment()?.is_some() {
			count += 1;
		}

		Err(Error::Ambiguous {
			count,
			first: first.path.as_ref().clone(),
			second: second.path.as_ref().clone(),
		})
	}

	/// Hand the next environment to the pool. Returns whether there was one.
	fn dispatch(&mut self, sender: &mpsc::Sender<Message>) -> Result<bool, Error> {
		let Some(discovered) = self.next_environment()? else {
			return Ok(false);
		};

		// Now that there is an environment to export, somewhere to put it. Left
		// until here so that finding none leaves the filesystem alone, and so that
		// an ambiguous set is refused before anything is created.
		if !self.prepared {
			prepare_output_dir(&self.export)?;
			self.prepared = true;
		}

		let index = self.dispatched;
		self.dispatched += 1;
		self.outstanding.insert(index);
		self.reports.push(Report::new(
			Arc::clone(&discovered.path),
			identify(discovered.path.as_path()),
		));

		let export = Arc::clone(&self.export);
		let engine = self.engine.clone();
		let sender = sender.clone();
		self.pool.spawn(move || {
			let outcome = export_environment(&engine, &export, &discovered, index, &sender);
			if let Err(error) = outcome.as_ref()
				&& error.fatal()
			{
				export.abort();
			}
			// A receiver that is gone means the driver has already given up.
			drop(sender.blocking_send(Message::Finished { index, outcome }));
		});

		Ok(true)
	}

	/// The next environment matching `--name` and `--selector`.
	fn next_environment(&mut self) -> Result<Option<Discovered>, Error> {
		if let Some(discovered) = self.pending.pop() {
			return Ok(Some(discovered));
		}

		loop {
			let Some(discover) = self.discover.as_mut() else {
				return Ok(None);
			};

			// Discovery evaluates Jsonnet for inline environments, which can take
			// a while; file writes carry on regardless, since Tokio runs them on
			// its blocking pool.
			let Some(discovered) = writer::blocking(|| discover.next()) else {
				self.discover = None;
				return Ok(None);
			};

			let discovered = discovered?;
			if self.matches(&discovered) {
				return Ok(Some(discovered));
			}
		}
	}

	fn matches(&self, discovered: &Discovered) -> bool {
		if let Some(name) = self.export.options.name.as_deref() {
			let matches_name = discovered
				.environment
				.metadata
				.name
				.as_deref()
				.is_some_and(|environment| environment.contains(name))
				|| discovered.path.to_string_lossy().contains(name);
			if !matches_name {
				return false;
			}
		}

		if let Some(selector) = self.selector.as_ref() {
			let labels = discovered.environment.metadata.labels.as_ref();
			if !selector.matches(labels.unwrap_or(&NO_LABELS)) {
				return false;
			}
		}

		true
	}

	/// Take one message from a worker, writing whatever came with it.
	async fn receive(
		&mut self,
		message: Message,
		writer: &mut Writer,
		written: &mut Vec<Written>,
	) -> Result<(), Error> {
		match message {
			Message::Chunk { index, files } => writer.write(index, files, written).await,
			Message::Finished { index, outcome } => {
				self.outstanding.remove(&index);
				let report = &mut self.reports[index];
				match outcome {
					Ok(timing) => report.timing = timing,
					Err(error) => {
						let fatal = error.fatal();
						report.error = Some(Arc::new(error));
						if fatal {
							self.export.abort();
						}
					}
				}
				Ok(())
			}
		}
	}

	/// Give up on environments whose worker disappeared.
	fn abandon(&mut self) {
		for index in self.outstanding.drain() {
			self.reports[index].error = Some(Arc::new(Error::WorkerLost));
		}
	}

	/// Turn the driver's state into the export's result.
	fn finish(&mut self, written: Vec<Written>) -> Result<Exported, Error> {
		let mut reports: Vec<Report> = self.reports.drain(..).collect();

		for written in written {
			let report = &mut reports[written.index];
			if written.unchanged {
				report.unchanged += 1;
			}
			report.files.push(written.path);
		}

		// Writes finish in whatever order the filesystem gets to them, which
		// should not show up in the result.
		for report in &mut reports {
			report.files.sort();
		}

		if reports.is_empty()
			&& (self.export.options.name.is_some() || self.export.options.selector.is_some())
		{
			return Err(Error::NothingMatched {
				name: self.export.options.name.clone(),
				selector: self.export.options.selector.clone(),
			});
		}

		Ok(Exported { reports })
	}
}

/// Evaluate and export one environment, sending its manifests to the driver as
/// they are serialized.
fn export_environment(
	engine: &Engine,
	export: &Export,
	discovered: &Discovered,
	index: usize,
	sender: &mpsc::Sender<Message>,
) -> Result<Option<TimingData>, Error> {
	if export.aborted() {
		return Err(Error::Skipped);
	}

	let mut timing = export.options.timing.then(TimingData::default);
	debug!(environment = ?discovered.path, "exporting");

	let evaluate_started = Instant::now();
	let environment = engine.load(discovered)?;
	if let Some(timing) = timing.as_mut() {
		timing.evaluate = evaluate_started.elapsed();
	}

	let serialize_started = Instant::now();
	let plan = export.plan(&environment)?;
	for chunk in plan.chunks() {
		let files = export.serialize_chunk(chunk, &plan)?;
		sender
			.blocking_send(Message::Chunk { index, files })
			.map_err(|_| Error::WorkerLost)?;
	}
	let manifests = plan.manifests();
	if let Some(timing) = timing.as_mut() {
		timing.serialize = serialize_started.elapsed();
		timing.manifests = manifests;
	}

	trace!(environment = ?discovered.path, manifests, "exported");
	Ok(timing)
}

/// Refuse to export into a directory that already holds an export, unless told
/// otherwise, and make sure it exists.
fn prepare_output_dir(export: &Export) -> Result<(), Error> {
	let output_dir = &export.options.output_dir;

	if export.options.merge_strategy == MergeStrategy::None && !manifest::is_empty_dir(output_dir)?
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

fn collect_written(report: &mut Report, written: Vec<Written>) {
	for written in written {
		if written.unchanged {
			report.unchanged += 1;
		}
		report.files.push(written.path);
	}
	report.files.sort();
}

/// Check what the export wrote against what was there before, then update the
/// index.
///
/// Conflicts are only detectable once every environment has been exported, since
/// any two of them could produce the same file.
fn reconcile(export: &Export, exported: &Exported) -> Result<(), Error> {
	let options = &export.options;
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

	let mut owners: FxHashMap<String, &str> = FxHashMap::default();
	for report in exported.reports.iter().filter(|report| !report.failed()) {
		for file in &report.files {
			let file = manifest::relative_key(file);

			if let Some(first) = owners.get(&file) {
				return Err(Error::DuplicateFile {
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
				return Err(Error::ForeignFile {
					file,
					owner: owner.to_owned(),
				});
			}

			superseded.remove(&file);
			owners.insert(file, &report.identifier);
		}
	}

	manifest::prune(&options.output_dir, &superseded);

	if !options.skip_manifest {
		index.forget(&superseded);
		for report in exported.reports.iter().filter(|report| !report.failed()) {
			index.record(
				&report.identifier,
				report.files.iter().map(PathBuf::as_path),
			);
		}
		index.write()?;
	}

	Ok(())
}

/// How `manifest.json` refers to an environment: its entrypoint, relative to the
/// working directory when it is below it.
fn identify(source: &Path) -> String {
	let entrypoint = if source.is_file() {
		source.to_path_buf()
	} else {
		source.join(JPath::DEFAULT_ENTRYPOINT)
	};

	std::env::current_dir()
		.ok()
		.and_then(|current_dir| {
			entrypoint
				.strip_prefix(current_dir)
				.ok()
				.map(Path::to_path_buf)
		})
		.unwrap_or(entrypoint)
		.to_string_lossy()
		.into_owned()
}
