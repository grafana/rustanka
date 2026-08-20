use std::collections::VecDeque;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::iter::{ParallelBridge, ParallelIterator};
use rtk_jsonnet::jpath::JPath;
use rtk_jsonnet::{EvaluationValue, Hidden};
use rtk_spec::canonical::Environment;
use rustc_hash::{FxBuildHasher, FxHashSet};
use tracing::Level;
use walkdir::WalkDir;

use crate::{Engine, Error};

/// Files that indicate a Tanka environment.
const ENV_MARKERS: &[&str] = &["spec.json", "main.jsonnet"];

const METADATA_EVAL_SCRIPT: &str = r"
local noDataEnv(object) =
  std.prune(
    if std.isObject(object)
    then
      if std.objectHas(object, 'apiVersion')
         && std.objectHas(object, 'kind')
      then
        if object.kind == 'Environment'
        then object { data+:: {} }
        else {}
      else
        std.mapWithKey(
          function(key, obj)
            noDataEnv(obj),
          object
        )
    else if std.isArray(object)
    then
      std.map(
        function(obj)
          noDataEnv(obj),
        object
      )
    else {}
  );

noDataEnv(main)
";

/// Directories to skip during discovery.
const SKIP_DIRS: &[&str] = &["vendor", "node_modules", ".git", "lib"];

type DirectoryIter =
	walkdir::FilterEntry<walkdir::IntoIter, for<'a> fn(&'a walkdir::DirEntry) -> bool>;

/// Result of environment discovery.
#[derive(Clone, Debug)]
pub struct Discovered {
	/// Path to the environment directory.
	pub path: Arc<PathBuf>,
	/// Whether this environment has a `spec.json`.
	pub is_static: bool,
	/// Whether the entrypoint evaluates to this environment and nothing else,
	/// rather than declaring it somewhere inside a larger structure.
	pub standalone: bool,
	/// The discovered environment.
	pub environment: Environment<'static>,
}

impl Discovered {
	/// The name this environment has to be selected by, if it has to be at all.
	///
	/// A file may declare several environments, in which case exporting or
	/// diffing one of them means picking it out from inside Jsonnet, by name.
	/// An environment that is the whole of what its entrypoint evaluates to — or
	/// that has a `spec.json` of its own — needs no picking out, and is better
	/// off without it: the entrypoint can then simply be evaluated, which is what
	/// it takes for one that is a function of top level arguments to be called.
	pub fn selected_by(&self) -> Option<&str> {
		if self.is_static || self.standalone {
			return None;
		}

		self.environment.metadata.name.as_deref()
	}
}

const fn assert_send<T: Send>() {}
const _: () = {
	// Directories are resolved on several threads at once, so that environments
	// can be discovered in parallel. Holding an evaluated value anywhere in here
	// would make that impossible: see `inline_environments`.
	assert_send::<Discovered>();
	assert_send::<Candidates>();
};

/// The directories that hold environments, in the order they are found.
///
/// Finding them is a filesystem walk and is cheap; working out what each one
/// declares is not, and is [`Engine::resolve_candidate`]'s job. They are separate
/// so that the expensive half can be done several at a time.
struct Candidates {
	paths: <Vec<PathBuf> as IntoIterator>::IntoIter,
	directory: Option<DirectoryIter>,
	/// Directories already handed out. A path can be reached more than once,
	/// through a link or by being named as well as walked into.
	seen: FxHashSet<Arc<PathBuf>>,
	current_dir: Option<PathBuf>,
	span: tracing::Span,
}

impl Candidates {
	fn new(paths: Vec<PathBuf>) -> Candidates {
		let paths_len = paths.len();
		Candidates {
			paths: paths.into_iter(),
			directory: None,
			seen: FxHashSet::with_capacity_and_hasher(paths_len, FxBuildHasher),
			current_dir: None,
			span: tracing::span!(Level::TRACE, "discover"),
		}
	}

	/// Hand out a directory, unless it has been handed out already.
	fn unseen(&mut self, path: PathBuf) -> Option<Arc<PathBuf>> {
		let path = Arc::new(path);
		self.seen.insert(Arc::clone(&path)).then_some(path)
	}

	#[tracing::instrument]
	fn is_environment(path: &Path) -> bool {
		if !path.is_dir() {
			tracing::trace!(path = ?path, "path is not a directory");
			return false;
		}

		for marker in ENV_MARKERS {
			if path.join(marker).exists() {
				tracing::trace!("has marker ({marker}) -> true");
				return true;
			}
		}

		tracing::trace!("has no markers (spec.json or main.jsonnet) -> false");
		false
	}
}

impl Iterator for Candidates {
	/// Walking cannot fail; only working out where a relative path starts can.
	type Item = Result<Arc<PathBuf>, std::io::Error>;

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			let span = self.span.clone();
			let _guard = span.enter();

			if let Some(directory) = &mut self.directory {
				let entry = match directory.next() {
					Some(Ok(entry)) => entry,
					Some(Err(_)) => continue,
					None => {
						self.directory = None;
						continue;
					}
				};
				if entry.file_type().is_dir()
					&& Self::is_environment(entry.path())
					&& let Some(path) = self.unseen(entry.path().to_path_buf())
				{
					return Some(Ok(path));
				}
				continue;
			}

			let path = self.paths.next()?;
			tracing::trace!(path = ?path, "processing path");
			let mut absolute = if path.is_absolute() {
				path
			} else {
				let current_dir = match &self.current_dir {
					Some(current_dir) => current_dir,
					None => match env::current_dir() {
						Ok(current_dir) => self.current_dir.insert(current_dir),
						Err(error) => return Some(Err(error)),
					},
				};
				current_dir.join(path)
			};

			if absolute.is_file() {
				absolute = absolute.parent().map(Path::to_path_buf).unwrap_or(absolute);
			}

			// A path that is itself an environment is that environment, rather
			// than somewhere to look for others.
			if Self::is_environment(&absolute) {
				if let Some(path) = self.unseen(absolute) {
					return Some(Ok(path));
				}
				continue;
			}

			let filter: for<'a> fn(&'a walkdir::DirEntry) -> bool = |entry| {
				if !entry.file_type().is_dir() {
					return true;
				}
				entry
					.file_name()
					.to_str()
					.is_none_or(|name| !SKIP_DIRS.contains(&name) && !name.starts_with('.'))
			};
			self.directory = Some(
				WalkDir::new(absolute)
					.follow_links(true)
					.into_iter()
					.filter_entry(filter),
			);
		}
	}
}

impl Engine {
	/// Environments under `paths`, one at a time.
	///
	/// Reading what a directory declares means evaluating Jsonnet, and this does
	/// it as it goes, so that a caller which stops early has not paid for the
	/// rest. Exporting wants that: it evaluates one environment while discovering
	/// the next.
	#[tracing::instrument]
	pub fn discover(&self, paths: Vec<PathBuf>) -> Discover {
		Discover::new(self.clone(), paths)
	}

	/// Every environment under `paths`, reading several directories at once.
	///
	/// For callers that want all of them anyway, which listing and diffing do.
	/// The environments come back in the order [`Engine::discover`] would have
	/// handed them out, and so does the first failure among them.
	#[tracing::instrument]
	pub fn discover_all(&self, paths: Vec<PathBuf>) -> Result<Vec<Discovered>, Error> {
		let pool = rayon::ThreadPoolBuilder::new()
			.num_threads(available_parallelism())
			// Jsonnet evaluation recurses deeply.
			.stack_size(8 * 1024 * 1024)
			.build()
			.expect("a rayon pool can be built");

		// Failures are carried as text: a Jsonnet error's stack trace is `Rc`-based,
		// so it cannot leave the thread that raised it.
		let mut resolved: Vec<(usize, Result<Vec<Discovered>, String>)> = pool.install(|| {
			Candidates::new(paths)
				.enumerate()
				.par_bridge()
				.map(|(index, path)| {
					let found = path
						.map_err(Error::from)
						.and_then(|path| self.resolve_candidate(path))
						.map_err(|error| error.to_string());
					(index, found)
				})
				.collect()
		});

		// Directories are resolved in whatever order the pool gets to them, which
		// should show up neither in the order environments come back nor in which of
		// several failures is reported.
		resolved.sort_by_key(|(index, _)| *index);

		let mut environments = Vec::new();
		for (_, found) in resolved {
			environments.extend(found.map_err(Error::Rendered)?);
		}

		Ok(environments)
	}

	/// Every environment a directory declares.
	fn resolve_candidate(&self, path: Arc<PathBuf>) -> Result<Vec<Discovered>, Error> {
		if path.join("spec.json").exists() {
			return Ok(vec![Discovered::from_static(path)?]);
		}

		self.resolve_inline(path)
	}

	/// Every environment declared anywhere in a directory's entrypoint.
	#[tracing::instrument(skip(self))]
	fn resolve_inline(&self, path: Arc<PathBuf>) -> Result<Vec<Discovered>, Error> {
		let main_path = path.join("main.jsonnet");
		let options = self.jsonnet.options();

		let jpath = JPath::resolve(&main_path)?;
		let entrypoint = jpath
			.entrypoint
			.strip_prefix(&jpath.base_directory)
			.unwrap_or(&jpath.entrypoint)
			.to_string_lossy();

		let snippet = self.entrypoint_snippet(&entrypoint, METADATA_EVAL_SCRIPT);

		// Inline environments are named by the Jsonnet that declares them, but
		// their namespace still comes from where the entrypoint lives.
		let namespace = namespace_of(&jpath);

		// Deliberately not evaluated as any environment in particular: the specs
		// that would say how to evaluate them are what this is reading, and an
		// entrypoint may need Tanka's native functions just to declare them. tk
		// discovers the same way, and applies what it finds only when exporting.
		let mut evaluator = self.jsonnet.create_evaluator();
		options.apply(&mut evaluator)?;
		evaluator.with_import_paths(jpath.import_paths)?;
		let evaluation = evaluator.evaluate_snippet(snippet)?;

		// Collected in one go, rather than yielded from an iterator that borrows the
		// evaluation: evaluated values are `Rc`-based, so holding one would pin
		// discovery — and everything reading it — to this thread. What comes out is
		// owned, which is what lets one thread discover an environment and another
		// export it. The metadata script has already pruned every environment's
		// data, so this is small.
		let mut collector = InlineCollector::new(&path, namespace.as_deref());
		let standalone = collector.collect(evaluation.into_value())?;
		Ok(collector.finish(standalone))
	}
}

/// Environments under a set of paths, one at a time.
pub struct Discover {
	engine: Engine,
	candidates: Candidates,
	/// Environments from a directory that declares several, waiting to be handed
	/// out one at a time.
	found: VecDeque<Discovered>,
}

impl Discover {
	pub(crate) fn new(engine: Engine, paths: Vec<PathBuf>) -> Discover {
		Discover {
			engine,
			candidates: Candidates::new(paths),
			found: VecDeque::new(),
		}
	}
}

impl Iterator for Discover {
	type Item = Result<Discovered, Error>;

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some(discovered) = self.found.pop_front() {
				return Some(Ok(discovered));
			}

			match self.candidates.next()? {
				Ok(path) => match self.engine.resolve_candidate(path) {
					Ok(found) => self.found.extend(found),
					Err(error) => return Some(Err(error)),
				},
				Err(error) => return Some(Err(error.into())),
			}
		}
	}
}

fn available_parallelism() -> usize {
	std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// The state shared while recursively collecting inline environments.
struct InlineCollector<'a> {
	path: &'a Arc<PathBuf>,
	namespace: Option<&'a str>,
	found: Vec<Discovered>,
}

impl<'a> InlineCollector<'a> {
	fn new(path: &'a Arc<PathBuf>, namespace: Option<&'a str>) -> InlineCollector<'a> {
		InlineCollector {
			path,
			namespace,
			found: Vec::new(),
		}
	}

	/// Collect every environment declared anywhere in `value`, depth first.
	///
	/// Returns whether `value` was itself an environment, rather than something
	/// with environments somewhere inside it.
	fn collect(&mut self, value: EvaluationValue) -> Result<bool, Error> {
		let deserializable = value.clone();
		match value.into_object() {
			Ok(object) => {
				// The metadata script leaves nothing but environments behind, but
				// checking the kind is cheap and keeps a stray object from being
				// deserialized as one.
				if object.has("apiVersion", Hidden::Skip)?
					&& object
						.get("kind", Hidden::Skip)?
						.and_then(|kind| kind.as_str())
						.as_deref() == Some("Environment")
				{
					let environment: Environment<'static> = deserializable.deserialize()?;
					self.found.push(Discovered::from_environment(
						Arc::clone(self.path),
						self.namespace,
						environment,
					));
					return Ok(true);
				}

				for value in object.into_values() {
					self.collect(value?)?;
				}
			}
			Err(value) => {
				if let Ok(array) = value.into_array() {
					for value in array.into_values() {
						self.collect(value?)?;
					}
				}
			}
		}

		Ok(false)
	}

	fn finish(mut self, standalone: bool) -> Vec<Discovered> {
		if standalone {
			for discovered in &mut self.found {
				discovered.standalone = true;
			}
		}
		self.found
	}
}

impl Discovered {
	fn from_static(path: Arc<PathBuf>) -> Result<Self, Error> {
		let spec_path = path.join("spec.json");
		let jpath = spec_path
			.parent()
			.and_then(|directory| JPath::resolve(directory).ok());
		let resolved_spec = jpath
			.as_ref()
			.map(|jpath| jpath.base_directory.join("spec.json"))
			.filter(|path| path.exists());
		let content = fs::read_to_string(resolved_spec.as_deref().unwrap_or(&spec_path))?;
		let environment = serde_json::from_str::<Environment<'_>>(&content)?;
		let mut environment = environment.without_data();

		// A static environment is named after its directory and carries its
		// entrypoint as its namespace, whatever `spec.json` says. Best effort: an
		// environment whose entrypoint cannot be resolved fails later, when it is
		// evaluated, with a better error than discovery could give.
		if let Some(jpath) = jpath {
			crate::metadata::apply_paths(&mut environment.metadata, &jpath, true);
		}

		Ok(Self {
			path,
			is_static: true,
			standalone: true,
			environment,
		})
	}

	fn from_environment(
		path: Arc<PathBuf>,
		namespace: Option<&str>,
		environment: Environment<'_>,
	) -> Self {
		let mut environment = environment.without_data();
		if let Some(namespace) = namespace {
			environment.metadata.namespace = Some(namespace.to_owned());
		}

		Self {
			path,
			is_static: false,
			standalone: false,
			environment,
		}
	}
}

/// An environment's `metadata.namespace`: its entrypoint, relative to the
/// project root. See [`crate::metadata::apply_paths`].
fn namespace_of(jpath: &JPath) -> Option<Arc<str>> {
	jpath
		.entrypoint
		.strip_prefix(&jpath.root_directory)
		.ok()
		.map(|relative| relative.to_string_lossy().as_ref().into())
}

#[cfg(test)]
mod tests {
	use std::fs;
	use std::sync::atomic::{AtomicUsize, Ordering};

	use super::*;

	static NEXT_TEMP_DIR: AtomicUsize = AtomicUsize::new(0);

	struct TempDir(PathBuf);

	impl TempDir {
		fn new() -> std::io::Result<Self> {
			let path = env::temp_dir().join(format!(
				"rtk-environments-{}-{}",
				std::process::id(),
				NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed)
			));
			fs::create_dir(&path)?;
			Ok(Self(path))
		}

		fn path(&self) -> &Path {
			&self.0
		}
	}

	impl Drop for TempDir {
		fn drop(&mut self) {
			let _ = fs::remove_dir_all(&self.0);
		}
	}

	fn test_engine() -> Engine {
		Engine::new(rtk_jsonnet::Engine::new(rtk_jsonnet::Options::default()))
	}

	/// An environment declared in Jsonnet, optionally alongside another.
	fn inline(root: &Path, alongside: bool) {
		fs::create_dir_all(root.join("env")).unwrap();
		// Resolving an entrypoint's import paths needs a project to resolve within.
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		let declare = |name: &str| {
			format!(
				r"{{
					apiVersion: 'tanka.dev/v1alpha1',
					kind: 'Environment',
					metadata: {{ name: '{name}' }},
					spec: {{ namespace: '{name}' }},
					data: {{}},
				}}"
			)
		};

		let main = if alongside {
			format!(
				"{{ first: {}, second: {} }}",
				declare("first"),
				declare("second")
			)
		} else {
			declare("only")
		};

		fs::write(root.join("env/main.jsonnet"), main).unwrap();
	}

	/// A project with every shape discovery has to walk: static environments, a
	/// file declaring several inline ones, an environment nested below another
	/// directory, and a directory that is not an environment at all.
	fn mixed_project(root: &Path) {
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		for name in ["alpha", "beta"] {
			fs::create_dir_all(root.join(name)).unwrap();
			fs::write(root.join(name).join("main.jsonnet"), "{}").unwrap();
			fs::write(
				root.join(name).join("spec.json"),
				format!(
					r#"{{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{{"name":"{name}"}},"spec":{{}}}}"#
				),
			)
			.unwrap();
		}

		fs::create_dir_all(root.join("several")).unwrap();
		let declare = |name: &str| {
			format!(
				r"{{
					apiVersion: 'tanka.dev/v1alpha1',
					kind: 'Environment',
					metadata: {{ name: '{name}' }},
					spec: {{ namespace: '{name}' }},
					data: {{}},
				}}"
			)
		};
		fs::write(
			root.join("several/main.jsonnet"),
			format!(
				"{{ a: {}, b: {}, c: {} }}",
				declare("one"),
				declare("two"),
				declare("three")
			),
		)
		.unwrap();

		fs::create_dir_all(root.join("nested/deeper/env")).unwrap();
		fs::write(root.join("nested/deeper/env/main.jsonnet"), "{}").unwrap();
		fs::write(
			root.join("nested/deeper/env/spec.json"),
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"nested"},"spec":{}}"#,
		)
		.unwrap();

		fs::create_dir_all(root.join("not-an-environment")).unwrap();
		fs::write(root.join("not-an-environment/readme.txt"), "nothing here").unwrap();
	}

	fn described(environments: &[Discovered]) -> Vec<(PathBuf, Option<String>, bool, bool)> {
		environments
			.iter()
			.map(|found| {
				(
					found.path.as_ref().clone(),
					found.environment.metadata.name.clone(),
					found.is_static,
					found.standalone,
				)
			})
			.collect()
	}

	#[test]
	fn discovering_all_at_once_finds_what_discovering_one_at_a_time_does() {
		let temp = TempDir::new().unwrap();
		mixed_project(temp.path());
		// Named twice, and once as a file rather than its directory, so that
		// skipping a directory already seen is exercised.
		let paths = vec![
			temp.path().to_path_buf(),
			temp.path().join("alpha"),
			temp.path().join("several/main.jsonnet"),
		];

		let one_at_a_time = Discover::new(test_engine(), paths.clone())
			.collect::<Result<Vec<_>, _>>()
			.unwrap();
		let all_at_once = test_engine().discover_all(paths).unwrap();

		assert!(one_at_a_time.len() >= 6, "{:?}", described(&one_at_a_time));
		assert_eq!(described(&all_at_once), described(&one_at_a_time));
	}

	#[test]
	fn discovering_all_at_once_reports_the_failure_discovering_one_at_a_time_would() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Several environments that cannot be read, so that which failure is
		// reported is a choice rather than a foregone conclusion.
		for name in ["a-broken", "b-broken", "c-broken"] {
			fs::create_dir_all(root.join(name)).unwrap();
			fs::write(
				root.join(name).join("main.jsonnet"),
				format!("error '{name} is broken'"),
			)
			.unwrap();
		}
		fs::create_dir_all(root.join("d-fine")).unwrap();
		fs::write(root.join("d-fine/main.jsonnet"), "{}").unwrap();

		let paths = vec![root.to_path_buf()];
		let expected = Discover::new(test_engine(), paths.clone())
			.collect::<Result<Vec<_>, _>>()
			.expect_err("the broken environments fail")
			.to_string();

		// Run it more than once: the answer should not depend on which thread
		// finished first.
		for _ in 0..8 {
			let error = test_engine()
				.discover_all(paths.clone())
				.expect_err("the broken environments fail");
			assert_eq!(error.to_string(), expected);
		}
	}

	#[test]
	fn only_an_environment_declared_alongside_others_has_to_be_selected() {
		// An entrypoint that evaluates to one environment can simply be
		// evaluated. Picking one out of several has to happen inside Jsonnet, by
		// name, which is worth avoiding when there is nothing to pick from: an
		// entrypoint taking top level arguments has to be called, and calling it
		// is what evaluating it plainly does.
		let temp = TempDir::new().unwrap();
		inline(temp.path(), false);
		let environments = Discover::new(test_engine(), vec![temp.path().join("env")])
			.collect::<Result<Vec<_>, _>>()
			.unwrap();

		assert_eq!(environments.len(), 1);
		assert!(environments[0].standalone);
		assert_eq!(environments[0].selected_by(), None);
		assert_eq!(
			environments[0].environment.metadata.name.as_deref(),
			Some("only"),
			"the environment still knows its own name"
		);

		let temp = TempDir::new().unwrap();
		inline(temp.path(), true);
		let mut environments = Discover::new(test_engine(), vec![temp.path().join("env")])
			.collect::<Result<Vec<_>, _>>()
			.unwrap();
		environments.sort_by(|a, b| {
			a.environment
				.metadata
				.name
				.cmp(&b.environment.metadata.name)
		});

		assert_eq!(environments.len(), 2);
		assert!(environments.iter().all(|found| !found.standalone));
		assert_eq!(environments[0].selected_by(), Some("first"));
		assert_eq!(environments[1].selected_by(), Some("second"));
	}

	#[test]
	fn a_static_environment_is_never_selected_by_name() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();
		fs::write(
			root.join("env/spec.json"),
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"env"},"spec":{}}"#,
		)
		.unwrap();

		let environments = Discover::new(test_engine(), vec![root.join("env")])
			.collect::<Result<Vec<_>, _>>()
			.unwrap();

		assert_eq!(environments.len(), 1);
		assert_eq!(environments[0].selected_by(), None);
	}

	#[test]
	fn finds_static_environment_metadata() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();
		fs::write(
			root.join("env/spec.json"),
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"env","labels":{"tier":"test"}},"spec":{"namespace":"default","exportJsonnetImplementation":"jrsonnet"}}"#,
		)
		.unwrap();

		let environments = Discover::new(test_engine(), vec![root.join("env")])
			.collect::<Result<Vec<_>, _>>()
			.unwrap();
		assert_eq!(environments.len(), 1);
		assert!(environments[0].is_static);
		assert_eq!(
			environments[0].environment.metadata.name.as_deref(),
			Some("env")
		);
		assert_eq!(
			environments[0]
				.environment
				.spec
				.export_jsonnet_implementation
				.as_ref()
				.map(rtk_spec::canonical::JsonentImplementationOrConfig::implementation),
			Some(&rtk_spec::canonical::JsonnetImplementation::Jrsonnet)
		);
		assert_eq!(
			environments[0]
				.environment
				.metadata
				.labels
				.as_ref()
				.and_then(|labels| labels.get("tier"))
				.map(AsRef::as_ref),
			Some("test")
		);
	}

	#[test]
	fn finds_inline_environments_without_manifesting() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(
			root.join("env/main.jsonnet"),
			r"{
				dev: { apiVersion: 'tanka.dev/v1alpha1', kind: 'Environment', metadata: { name: 'dev', labels: { tier: 'test' } }, spec: { namespace: 'default' } },
				prod: { apiVersion: 'tanka.dev/v1alpha1', kind: 'Environment', metadata: { name: 'prod' }, spec: { namespace: 'default', exportJsonnetImplementation: 'jrsonnet' } },
			}",
		)
		.unwrap();

		let environments = Discover::new(test_engine(), vec![root.join("env")])
			.collect::<Result<Vec<_>, _>>()
			.unwrap();
		assert_eq!(environments.len(), 2);
		assert!(
			environments
				.iter()
				.all(|environment| !environment.is_static)
		);
		assert!(
			environments
				.iter()
				.any(|environment| environment.environment.metadata.name.as_deref() == Some("dev"))
		);
		assert!(
			environments.iter().any(
				|environment| environment.environment.metadata.name.as_deref() == Some("prod")
			)
		);
	}

	#[test]
	fn skips_directories_and_suppresses_duplicates() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		for directory in ["env", "vendor/ignored"] {
			fs::create_dir_all(root.join(directory)).unwrap();
			fs::write(root.join(directory).join("main.jsonnet"), "{}").unwrap();
			fs::write(
				root.join(directory).join("spec.json"),
				r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{},"spec":{}}"#,
			)
			.unwrap();
		}

		let environments = Discover::new(test_engine(), vec![root.to_path_buf(), root.join("env")])
			.collect::<Result<Vec<_>, _>>()
			.unwrap();
		assert_eq!(environments.len(), 1);
		assert_eq!(environments[0].path.as_path(), root.join("env"));
	}
}
