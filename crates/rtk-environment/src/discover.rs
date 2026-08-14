use std::env;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use either::{Either, Left, Right};
use rtk_jsonnet::jpath::JPath;
use rtk_jsonnet::{Engine, EvaluationArrayValues, EvaluationObjectValues, EvaluationValue, Hidden};
use rtk_spec::canonical::Environment;
use rustc_hash::{FxBuildHasher, FxHashSet};
use tracing::Level;
use walkdir::WalkDir;

use crate::Error;

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

/// A snippet that imports an entrypoint as `main`, for `script` to work on.
///
/// An entrypoint taking top level arguments imports as a function rather than as
/// what it builds, so it has to be called. The arguments reach it through a
/// wrapping function for the evaluator to apply them to, and every parameter is
/// given a default because the evaluator passes only the arguments it was
/// actually given.
pub(crate) fn entrypoint_snippet(
	options: &rtk_jsonnet::Options,
	entrypoint: &str,
	script: &str,
) -> String {
	if !options.has_top_level_args() {
		return format!(r#"local main = import "{entrypoint}"; {script}"#);
	}

	let count = options.top_level_arguments.len() + options.top_level_code.len();
	let mut arguments = String::with_capacity(count * 16);
	let mut parameters = String::with_capacity(count * 24);

	let names = options
		.top_level_arguments
		.keys()
		.chain(options.top_level_code.keys());

	for (index, name) in names.enumerate() {
		if index != 0 {
			arguments.push_str(", ");
			parameters.push_str(", ");
		}

		arguments.push_str(name);
		let _ = write!(&mut parameters, "{name} = null");
	}

	format!(
		r#"function({parameters})
			local main = (import "{entrypoint}")({arguments});
			{script}"#
	)
}

type DirectoryIter =
	walkdir::FilterEntry<walkdir::IntoIter, for<'a> fn(&'a walkdir::DirEntry) -> bool>;

/// Result of environment discovery.
#[derive(Clone, Debug)]
pub struct Discovered {
	/// Path to the environment directory.
	pub path: Arc<PathBuf>,
	/// Whether this environment has a `spec.json`.
	pub is_static: bool,
	/// The discovered environment.
	pub environment: Environment<'static>,
}

pub struct Discover {
	engine: Engine,
	paths: <Vec<PathBuf> as IntoIterator>::IntoIter,
	directory: Option<DirectoryIter>,
	inline_environments: Option<DiscoverInlineEnvs>,
	seen_dirs: FxHashSet<Arc<PathBuf>>,
	current_dir: Option<PathBuf>,
	span: tracing::Span,
}

impl Discover {
	pub fn new(engine: Engine, paths: Vec<PathBuf>) -> Self {
		let paths_len = paths.len();
		Self {
			engine,
			paths: paths.into_iter(),
			directory: None,
			inline_environments: None,
			seen_dirs: FxHashSet::with_capacity_and_hasher(paths_len, FxBuildHasher),
			current_dir: None,
			span: tracing::span!(Level::TRACE, "discover"),
		}
	}

	#[tracing::instrument(skip(engine))]
	fn inline_environments(
		engine: &Engine,
		path: Arc<PathBuf>,
	) -> Result<Option<Either<Discovered, DiscoverInlineEnvs>>, Error> {
		let main_path = path.join("main.jsonnet");
		let options = engine.options();

		let jpath = JPath::resolve(&main_path)?;
		let entrypoint = jpath
			.entrypoint
			.strip_prefix(&jpath.base_directory)
			.unwrap_or(&jpath.entrypoint)
			.to_string_lossy();

		let snippet = entrypoint_snippet(options, &entrypoint, METADATA_EVAL_SCRIPT);

		// Inline environments are named by the Jsonnet that declares them, but
		// their namespace still comes from where the entrypoint lives.
		let namespace = namespace_of(&jpath);

		// Deliberately not evaluated as any environment in particular: the specs
		// that would say how to evaluate them are what this is reading, and an
		// entrypoint may need Tanka's native functions just to declare them. tk
		// discovers the same way, and applies what it finds only when exporting.
		let mut evaluator = engine.create_evaluator();
		options.apply(&mut evaluator)?;
		evaluator.with_import_paths(jpath.import_paths)?;
		let evaluation = evaluator.evaluate_snippet(snippet)?;

		match DiscoverInlineEnvs::discover_inline_env(path, namespace, evaluation.into_value())? {
			Some(Left(discovered)) => Ok(Some(Left(discovered))),
			other => Ok(other),
		}
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

	fn read_spec_json(spec_path: &Path) -> Result<Environment<'static>, Error> {
		let content = fs::read_to_string(spec_path)?;
		let environment = serde_json::from_str::<Environment<'_>>(&content)?;
		let mut environment = environment.without_data();

		// A static environment is named after its directory and carries its
		// entrypoint as its namespace, whatever `spec.json` says. Best effort:
		// an environment whose entrypoint cannot be resolved fails later, when
		// it is evaluated, with a better error than discovery could give.
		if let Some(directory) = spec_path.parent()
			&& let Ok(jpath) = JPath::resolve(directory)
		{
			crate::metadata::apply_paths(&mut environment.metadata, &jpath, true);
		}

		Ok(environment)
	}

	fn discover_environment(&mut self, path: Arc<PathBuf>) -> Result<Option<Discovered>, Error> {
		if !self.seen_dirs.insert(path.clone()) {
			return Ok(None);
		}

		let mut path_buf = (*path).clone();

		path_buf.push("spec.json");
		if path_buf.exists() {
			let environment = Self::read_spec_json(&path_buf)?;
			return Ok(Some(Discovered {
				path,
				is_static: true,
				environment,
			}));
		}
		path_buf.pop();

		match Self::inline_environments(&self.engine, path)? {
			Some(Left(discovered)) => Ok(Some(discovered)),
			Some(Right(discovered)) => {
				self.inline_environments = Some(discovered);
				Ok(None)
			}
			None => Ok(None),
		}
	}
}

impl Iterator for Discover {
	type Item = Result<Discovered, Error>;

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			let span = self.span.clone();
			let _guard = span.enter();

			if let Some(inline) = &mut self.inline_environments {
				if let Some(discovered) = inline.next() {
					return Some(discovered);
				}
				self.inline_environments = None;
				continue;
			}

			if let Some(directory) = &mut self.directory {
				let entry = match directory.next() {
					Some(Ok(entry)) => entry,
					Some(Err(_)) => continue,
					None => {
						self.directory = None;
						continue;
					}
				};
				if entry.file_type().is_dir() && Self::is_environment(entry.path()) {
					let path = Arc::new(entry.path().to_path_buf());
					match self.discover_environment(path) {
						Ok(Some(discovered)) => return Some(Ok(discovered)),
						Ok(None) => continue,
						Err(error) => return Some(Err(error)),
					}
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
						Err(error) => return Some(Err(error.into())),
					},
				};
				current_dir.join(path)
			};

			if absolute.is_file() {
				absolute = absolute.parent().map(Path::to_path_buf).unwrap_or(absolute);
			}

			if Self::is_environment(&absolute) {
				match self.discover_environment(Arc::new(absolute)) {
					Ok(Some(discovered)) => return Some(Ok(discovered)),
					Ok(None) => continue,
					Err(error) => return Some(Err(error)),
				}
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

enum DiscoverInlineEnvs {
	Array {
		path: Arc<PathBuf>,
		namespace: Option<Arc<str>>,
		iter: EvaluationArrayValues,
		recursion: Option<Box<DiscoverInlineEnvs>>,
	},
	Object {
		path: Arc<PathBuf>,
		namespace: Option<Arc<str>>,
		iter: EvaluationObjectValues,
		recursion: Option<Box<DiscoverInlineEnvs>>,
	},
}

impl DiscoverInlineEnvs {
	fn discover_inline_env(
		path: Arc<PathBuf>,
		namespace: Option<Arc<str>>,
		value: EvaluationValue,
	) -> Result<Option<Either<Discovered, Self>>, Error> {
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
					return Ok(Some(Left(Discovered::from_environment(
						path,
						namespace.as_deref(),
						environment,
					))));
				}
				Ok(Some(Right(Self::Object {
					path,
					namespace,
					iter: object.into_values(),
					recursion: None,
				})))
			}
			Err(value) => match value.into_array() {
				Ok(array) => Ok(Some(Right(Self::Array {
					path,
					namespace,
					iter: array.into_values(),
					recursion: None,
				}))),
				Err(_) => Ok(None),
			},
		}
	}

	fn recursion(&mut self) -> &mut Option<Box<Self>> {
		match self {
			Self::Array { recursion, .. } | Self::Object { recursion, .. } => recursion,
		}
	}
}

impl Iterator for DiscoverInlineEnvs {
	type Item = Result<Discovered, Error>;

	fn next(&mut self) -> Option<Self::Item> {
		if let Some(result) = self.recursion().as_mut().and_then(Iterator::next) {
			return Some(result);
		}
		*self.recursion() = None;

		loop {
			let (path, namespace, value) = match self {
				Self::Array {
					path,
					namespace,
					iter,
					..
				} => (path.clone(), namespace.clone(), iter.next()?),
				Self::Object {
					path,
					namespace,
					iter,
					..
				} => (path.clone(), namespace.clone(), iter.next()?),
			};
			let value = match value {
				Ok(value) => value,
				Err(error) => return Some(Err(error.into())),
			};
			match Self::discover_inline_env(path, namespace, value) {
				Ok(Some(Left(discovered))) => return Some(Ok(discovered)),
				Ok(Some(Right(recursion))) => {
					*self.recursion() = Some(Box::new(recursion));
					if let Some(result) = self.recursion().as_mut().and_then(Iterator::next) {
						return Some(result);
					}
					*self.recursion() = None;
				}
				Ok(None) => {}
				Err(error) => return Some(Err(error)),
			}
		}
	}
}

impl Discovered {
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
		Engine::new(rtk_jsonnet::Options::default())
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
