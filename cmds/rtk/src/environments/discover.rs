use std::env;
use std::fmt::Write;
use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Error, Result};
use either::{Either, Left, Right};
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use serde_json::Map;
use tracing::Level;
use walkdir::WalkDir;

use crate::jsonnet::evaluator::DefaultEvaluator;
use crate::jsonnet::evaluator::{Evaluator, EvaluatorImplementation, EvaluatorOptions};
use crate::jsonnet::jpath;

/// Files that indicate a Tanka environment
const ENV_MARKERS: &[&str] = &["spec.json", "main.jsonnet"];

const METADATA_EVAL_SCRIPT: &str = r#"
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
"#;

/// Directories to skip during discovery
const SKIP_DIRS: &[&str] = &["vendor", "node_modules", ".git", "lib"];

pub struct Discover<E = DefaultEvaluator> {
	evaluator: E,
	paths: <Vec<PathBuf> as IntoIterator>::IntoIter,
	directory:
		Option<walkdir::FilterEntry<walkdir::IntoIter, for<'a> fn(&'a walkdir::DirEntry) -> bool>>,
	inline_environments: Option<DiscoverInlineEnvs>,
	seen_dirs: FxHashSet<Arc<PathBuf>>,
	current_dir: Option<PathBuf>,
	span: tracing::Span,
}

impl<E> Discover<E>
where
	E: Evaluator,
{
	pub fn new(evaluator: E, paths: Vec<PathBuf>) -> Discover<E> {
		let span = tracing::span!(Level::TRACE, "discover");
		let paths_len = paths.len();

		Discover {
			evaluator,
			paths: paths.into_iter(),
			directory: None,
			inline_environments: None,
			seen_dirs: FxHashSet::with_capacity_and_hasher(paths_len, FxBuildHasher::default()),
			current_dir: None,
			span,
		}
	}
}

impl<E> Discover<E>
where
	E: Evaluator,
{
	#[tracing::instrument(skip(evaluator))]
	fn inline_environments(
		evaluator: &E,
		path: Arc<PathBuf>,
	) -> Result<Option<Either<Discovered, DiscoverInlineEnvs>>> {
		let main_path = path.join("main.jsonnet");

		let eval_opts = EvaluatorOptions::default();
		let global_opts = evaluator.global_options();

		let has_tlas = !global_opts.tla_str.is_empty() || !global_opts.tla_code.is_empty();

		let evaluation = if has_tlas {
			let mut tla_args = String::with_capacity(
				((global_opts.tla_str.len() + global_opts.tla_code.len()) * (4 + ", ".len()))
					.next_power_of_two(),
			);
			let mut tla_args_desc = String::with_capacity(
				((global_opts.tla_str.len() + global_opts.tla_code.len())
					* (4 + " = null, ".len()))
				.next_power_of_two(),
			);

			for (i, tla_str) in global_opts.tla_str.keys().enumerate() {
				tla_args.push_str(tla_str);
				if i != global_opts.tla_str.len() - 1 {
					tla_args.push_str(", ");
				}

				let _ = write!(&mut tla_args_desc, "{tla_str} = null");
				if i != global_opts.tla_str.len() - 1 {
					tla_args_desc.push_str(", ");
				}
			}
			// Add separator between tla_str and tla_code groups
			if !global_opts.tla_str.is_empty() && !global_opts.tla_code.is_empty() {
				tla_args.push_str(", ");
				tla_args_desc.push_str(", ");
			}
			for (i, tla_code) in global_opts.tla_code.keys().enumerate() {
				tla_args.push_str(tla_code);
				if i != global_opts.tla_code.len() - 1 {
					tla_args.push_str(", ");
				}

				let _ = write!(&mut tla_args_desc, "{tla_code} = null");
				if i != global_opts.tla_code.len() - 1 {
					tla_args_desc.push_str(", ");
				}
			}

			let jpath_result = jpath::resolve(&main_path)?;
			let entrypoint = jpath_result.entrypoint.to_string_lossy();
			let jpath = jpath_result.import_paths;
			let snippet = format!(
				r#"function({tla_args_desc})
                local main = (import "{entrypoint}")({tla_args});
                {METADATA_EVAL_SCRIPT}"#
			);

			evaluator.eval_snippet_with_jpath(snippet, jpath, &eval_opts)?
		} else {
			let jpath_result = jpath::resolve(&main_path)?;
			let entrypoint = jpath_result.entrypoint.to_string_lossy();
			let jpath = jpath_result.import_paths;
			let snippet = format!(r#"local main = import "{entrypoint}"; {METADATA_EVAL_SCRIPT}"#);

			evaluator.eval_snippet_with_jpath(snippet, jpath, &eval_opts)?
		};

		evaluator.clear_thread_local_state();
		evaluator.collect_cycles();

		match DiscoverInlineEnvs::discover_inline_env(path, evaluation.value).transpose()? {
			// Single environment found directly at top level — clear env_name
			// to match v1 behavior (env_name is only set for multi-environment cases)
			Some(Left(mut discovered)) => {
				discovered.env_name = None;
				Ok(Some(Left(discovered)))
			}
			other => Ok(other),
		}
	}

	#[tracing::instrument]
	fn is_environment(path: &Path) -> bool {
		if !path.is_dir() {
			tracing::trace!(
				path = ?path,
				"path is not a directory",
			);
			return false;
		}

		// Check for environment markers
		for marker in ENV_MARKERS {
			let marker_path = path.join(marker);
			if marker_path.exists() {
				tracing::trace!("has marker ({marker}) -> true");
				return true;
			}
		}

		tracing::trace!("has no markers (spec.json or main.jsonnet) -> false");

		false
	}

	#[tracing::instrument]
	fn read_spec_data(path: &Path) -> Result<SpecMetadata> {
		let spec_path = path.join("spec.json");
		if !spec_path.exists() {
			return Ok(SpecMetadata {
				export_jsonnet_implementation: None,
				labels: FxHashMap::default(),
			});
		}

		let content = match fs::read_to_string(&spec_path) {
			Ok(c) => c,
			Err(_) => {
				return Ok(SpecMetadata {
					export_jsonnet_implementation: None,
					labels: FxHashMap::default(),
				});
			}
		};

		let json = serde_json::from_str::<serde_json::Value>(&content)?;

		let (export_jsonnet_implementation, labels) = match json.as_object() {
			Some(object) => Self::extract_export_impl_and_labels(object)?,
			None => (None, None),
		};

		Ok(SpecMetadata {
			export_jsonnet_implementation,
			labels: labels.unwrap_or_default(),
		})
	}

	fn extract_export_impl_and_labels(
		object: &serde_json::Map<String, serde_json::Value>,
	) -> Result<(
		Option<EvaluatorImplementation>,
		Option<FxHashMap<Box<str>, Box<str>>>,
	)> {
		let export_impl = object
			.get("spec")
			.and_then(|s| s.get("exportJsonnetImplementation"))
			.and_then(|v| v.as_str())
			.map(|s| s.parse::<EvaluatorImplementation>())
			.transpose()?;

		let labels = object
			.get("metadata")
			.and_then(|m| m.get("labels"))
			.and_then(|l| l.as_object())
			.map(|object| {
				let mut collected =
					FxHashMap::with_capacity_and_hasher(object.len(), FxBuildHasher::default());
				collected.extend(object.iter().filter_map(|(k, v)| {
					v.as_str()
						.map(|s| (k.as_str().into(), s.to_string().into()))
				}));
				collected
			});

		Ok((export_impl, labels))
	}
}

impl<E> Iterator for Discover<E>
where
	E: Evaluator,
{
	type Item = Result<Discovered>;

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			let span_guard = self.span.enter();

			match self {
				// If we're iterating over a directory and we don't currently have
				// any inline environments to yield, keep iterating over the
				// directory.
				Discover {
					directory: Some(directory),
					inline_environments: None,
					..
				} => match directory.next() {
					Some(Ok(entry)) => {
						let entry_path = entry.path();

						let next = 'next: {
							if entry.file_type().is_dir() && Self::is_environment(entry_path) {
								let canonical = Arc::new(entry_path.to_path_buf());

								if self.seen_dirs.insert(canonical.clone()) {
									let is_static = canonical.join("spec.json").exists();
									if is_static {
										// Static environment - read spec data from spec.json
										let spec_data = match Self::read_spec_data(&canonical) {
											Ok(spec_metadata) => spec_metadata,
											Err(error) => return Some(Err(error)),
										};
										break 'next ControlFlow::Break(Ok(Discovered {
											is_static: true,
											path: canonical,
											env_name: None,
											export_jsonnet_implementation: spec_data
												.export_jsonnet_implementation,
											labels: spec_data.labels,
										}));
									} else {
										// Inline environment(s) - discover sub-environments
										break 'next match Discover::inline_environments(
											&self.evaluator,
											canonical,
										) {
											Ok(Some(Left(inline_environment))) => {
												ControlFlow::Break(Ok(inline_environment))
											}
											Ok(Some(Right(inline_environments))) => {
												self.inline_environments =
													Some(inline_environments);
												ControlFlow::Continue(())
											}
											Ok(None) => ControlFlow::Continue(()),
											Err(error) => ControlFlow::Break(Err(error)),
										};
									}
								}
							}

							ControlFlow::Continue(())
						};

						match next {
							ControlFlow::Continue(()) => {
								drop(span_guard);
								continue;
							}
							ControlFlow::Break(result) => return Some(result),
						}
					}
					Some(Err(_)) => {
						drop(span_guard);
						continue;
					}
					None => {
						self.directory = None;
						drop(span_guard);
						continue;
					}
				},
				// If we have inline environments to yield, iterate over those.
				Discover {
					inline_environments: Some(inline_environments),
					..
				} => match inline_environments.next() {
					Some(result) => return Some(result),
					None => {
						self.inline_environments = None;
						drop(span_guard);
						continue;
					}
				},
				// Finally, in all other cases, keep consuming the paths we've been
				// given to discover.
				Discover { paths, .. } => {
					let path = match paths.next() {
						Some(path) => path,
						None => return None,
					};

					let next = 'next: {
						tracing::trace!(path = ?path, "Processing path");

						let abs_path = if path.is_absolute() {
							tracing::trace!(path = ?path, "Path is absolute");
							path
						} else {
							let cwd = if self.current_dir.is_none() {
								match env::current_dir() {
									Ok(current_dir) => self.current_dir.get_or_insert(current_dir),
									Err(error) => {
										break 'next ControlFlow::Break(Err(Error::new(error)));
									}
								}
							} else {
								self.current_dir.as_mut().unwrap()
							};
							tracing::trace!(
								path = ?path,
								cwd = ?cwd,
								"Path is relative",
							);
							cwd.join(path)
						};

						// If the path is a file (e.g., main.jsonnet), use its parent directory
						let abs_path = if abs_path.is_file() {
							let parent_path = abs_path
								.parent()
								.map(|p| p.to_path_buf())
								.unwrap_or(abs_path);
							tracing::trace!(
								parent_path = ?parent_path,
								"Path is a file, using parent directory",
							);
							parent_path
						} else {
							abs_path
						};

						let exists = abs_path.exists();
						let is_dir = abs_path.is_dir();
						tracing::trace!(
							abs_path = ?abs_path,
							exists = exists,
							is_dir = is_dir,
							"Resolved path",
						);

						if Self::is_environment(&abs_path) {
							tracing::trace!(
								abs_path = ?abs_path,
								"Path is directly an environment",
							);
							let abs_path = Arc::new(abs_path);
							if self.seen_dirs.insert(abs_path.clone()) {
								let is_static = abs_path.join("spec.json").exists();
								if is_static {
									// Static environment - read spec data from spec.json
									let spec_data = match Self::read_spec_data(&abs_path) {
										Ok(spec_data) => spec_data,
										Err(error) => break 'next ControlFlow::Break(Err(error)),
									};
									ControlFlow::Break(Ok(Discovered {
										is_static: true,
										path: abs_path.clone(),
										env_name: None,
										export_jsonnet_implementation: spec_data
											.export_jsonnet_implementation,
										labels: spec_data.labels,
									}))
								} else {
									// Inline environment(s) - discover sub-environments
									match Discover::inline_environments(&self.evaluator, abs_path) {
										Ok(Some(Left(inline_environment))) => {
											ControlFlow::Break(Ok(inline_environment))
										}
										Ok(Some(Right(inline_environments))) => {
											self.inline_environments = Some(inline_environments);
											ControlFlow::Continue(())
										}
										Ok(None) => ControlFlow::Continue(()),
										Err(error) => ControlFlow::Break(Err(error)),
									}
								}
							} else {
								match Discover::inline_environments(&self.evaluator, abs_path) {
									Ok(Some(Left(inline_environment))) => {
										ControlFlow::Break(Ok(inline_environment))
									}
									Ok(Some(Right(inline_environments))) => {
										self.inline_environments = Some(inline_environments);
										ControlFlow::Continue(())
									}
									Ok(None) => ControlFlow::Continue(()),
									Err(error) => ControlFlow::Break(Err(error)),
								}
							}
						} else {
							tracing::trace!(
								abs_path = ?abs_path,
								"Path is not directly an environment, will walk directory tree",
							);

							let filter: for<'a> fn(&'a walkdir::DirEntry) -> bool = |e| {
								// Only filter directories
								if !e.file_type().is_dir() {
									return true;
								}
								// Skip certain directory names
								if let Some(name) = e.file_name().to_str() {
									if SKIP_DIRS.contains(&name) || name.starts_with('.') {
										return false;
									}
								}
								true
							};
							let walker = WalkDir::new(&abs_path)
								.follow_links(true)
								.into_iter()
								.filter_entry(filter);

							self.directory = Some(walker);

							ControlFlow::Continue(())
						}
					};

					match next {
						ControlFlow::Continue(()) => {
							drop(span_guard);
							continue;
						}
						ControlFlow::Break(result) => return Some(result),
					}
				}
			}
		}
	}
}

enum DiscoverInlineEnvs {
	Array {
		path: Arc<PathBuf>,
		iter: <Vec<serde_json::Value> as IntoIterator>::IntoIter,
		recursion: Option<Box<DiscoverInlineEnvs>>,
	},
	Object {
		path: Arc<PathBuf>,
		iter: <Map<String, serde_json::Value> as IntoIterator>::IntoIter,
		recursion: Option<Box<DiscoverInlineEnvs>>,
	},
}

impl DiscoverInlineEnvs {
	fn discover_inline_env(
		path: Arc<PathBuf>,
		value: serde_json::Value,
	) -> Option<Result<Either<Discovered, DiscoverInlineEnvs>>> {
		match value {
			serde_json::Value::Null
			| serde_json::Value::Bool(_)
			| serde_json::Value::Number(_)
			| serde_json::Value::String(_) => None,
			serde_json::Value::Array(array) => Some(Ok(Right(DiscoverInlineEnvs::Array {
				path,
				iter: array.into_iter(),
				recursion: None,
			}))),
			serde_json::Value::Object(object) => {
				if object.get("kind").and_then(|v| v.as_str()) == Some("Environment") {
					let object = &object;
					if let Some(meta) = object.get("metadata") {
						if let Some(name) = meta.get("name").and_then(|v| v.as_str()) {
							let (export_jsonnet_implementation, labels) =
								match Discover::<DefaultEvaluator>::extract_export_impl_and_labels(
									object,
								) {
									Ok((export_jsonnet_implementation, labels)) => {
										(export_jsonnet_implementation, labels)
									}
									Err(error) => return Some(Err(error)),
								};

							return Some(Ok(Left(Discovered {
								path: path.clone(),
								env_name: Some(name.to_owned()),
								is_static: false,
								export_jsonnet_implementation,
								labels: labels.unwrap_or_default(),
							})));
						}
					}
				}

				Some(Ok(Right(DiscoverInlineEnvs::Object {
					path,
					iter: object.into_iter(),
					recursion: None,
				})))
			}
		}
	}
}

impl DiscoverInlineEnvs {
	fn recurse(&mut self, recursion: DiscoverInlineEnvs) {
		match self {
			DiscoverInlineEnvs::Array {
				recursion: recursing,
				..
			} => {
				recursing.replace(Box::new(recursion));
			}
			DiscoverInlineEnvs::Object {
				recursion: recursing,
				..
			} => {
				recursing.replace(Box::new(recursion));
			}
		}
	}
}

impl Iterator for DiscoverInlineEnvs {
	type Item = Result<Discovered>;

	fn next(&mut self) -> Option<Self::Item> {
		match self {
			DiscoverInlineEnvs::Array {
				path,
				iter: array,
				recursion,
			} => {
				if let Some(Some(result)) = recursion.as_mut().map(Iterator::next) {
					return Some(result);
				}
				*recursion = None;

				loop {
					match array.next() {
						Some(value) => {
							if let Some(discovered) =
								DiscoverInlineEnvs::discover_inline_env(path.clone(), value)
							{
								break match discovered {
									Ok(Left(discovered)) => Some(Ok(discovered)),
									Ok(Right(recursion)) => {
										self.recurse(recursion);
										self.next()
									}
									Err(error) => Some(Err(error)),
								};
							}
						}
						None => break None,
					}
				}
			}
			DiscoverInlineEnvs::Object {
				path,
				iter: object,
				recursion,
			} => {
				if let Some(Some(result)) = recursion.as_mut().map(Iterator::next) {
					return Some(result);
				}
				*recursion = None;

				loop {
					match object.next() {
						Some((_, value)) => {
							if let Some(discovered) =
								DiscoverInlineEnvs::discover_inline_env(path.clone(), value)
							{
								break match discovered {
									Ok(Left(discovered)) => Some(Ok(discovered)),
									Ok(Right(recursion)) => {
										self.recurse(recursion);
										self.next()
									}
									Err(error) => Some(Err(error)),
								};
							}
						}
						None => break None,
					}
				}
			}
		}
	}
}

/// Result of environment discovery
#[derive(Debug, Clone)]
pub struct Discovered {
	/// Path to the environment directory
	pub path: Arc<PathBuf>,
	/// Whether this is a static environment (has spec.json)
	#[allow(dead_code)]
	pub is_static: bool,
	/// For inline environments with multiple sub-environments, this is the name of the specific environment
	/// For static environments or single inline environments, this is None
	pub env_name: Option<String>,
	/// The exportJsonnetImplementation from the inline environment spec, if present
	/// This is used to determine whether to use jrsonnet-compatible output formatting
	pub export_jsonnet_implementation: Option<EvaluatorImplementation>,
	/// Labels from the environment metadata (for selector filtering)
	pub labels: FxHashMap<Box<str>, Box<str>>,
}

/// Data read from spec.json for static environments
struct SpecMetadata {
	export_jsonnet_implementation: Option<EvaluatorImplementation>,
	labels: FxHashMap<Box<str>, Box<str>>,
}

#[cfg(test)]
mod tests {
	use std::fs;

	use tempfile::TempDir;

	use super::*;
	use crate::jsonnet::evaluator::GlobalEvaluatorOptions;

	fn test_evaluator() -> DefaultEvaluator {
		DefaultEvaluator::new(GlobalEvaluatorOptions::default())
	}

	#[test]
	fn test_find_single_environment() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		// Create a single static environment
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();
		fs::write(
			root.join("env/spec.json"),
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"env"},"spec":{"namespace":"default"}}"#,
		)
		.unwrap();

		let envs: Vec<_> = Discover::new(test_evaluator(), vec![root.join("env")])
			.collect::<Result<Vec<_>>>()
			.unwrap();
		assert_eq!(envs.len(), 1);
		assert!(envs[0].is_static);
		assert!(envs[0].env_name.is_none());
		assert!(envs[0].export_jsonnet_implementation.is_none());
	}

	#[test]
	fn test_find_static_environment() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();
		fs::write(
			root.join("env/spec.json"),
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment"}"#,
		)
		.unwrap();

		let envs: Vec<_> = Discover::new(test_evaluator(), vec![root.join("env")])
			.collect::<Result<Vec<_>>>()
			.unwrap();
		assert_eq!(envs.len(), 1);
		assert!(envs[0].is_static);
		assert!(envs[0].env_name.is_none());
	}

	#[test]
	fn test_find_multiple_environments() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create multiple static environments
		for name in ["dev", "staging", "prod"] {
			fs::create_dir_all(root.join(format!("environments/{}", name))).unwrap();
			fs::write(
				root.join(format!("environments/{}/main.jsonnet", name)),
				"{}",
			)
			.unwrap();
			fs::write(
				root.join(format!("environments/{}/spec.json", name)),
				format!(
					r#"{{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{{"name":"{}"}},"spec":{{"namespace":"default"}}}}"#,
					name
				),
			)
			.unwrap();
		}

		let envs: Vec<_> = Discover::new(test_evaluator(), vec![root.join("environments")])
			.collect::<Result<Vec<_>>>()
			.unwrap();
		assert_eq!(envs.len(), 3);
		// All should have no env_name since they're separate directories
		for env in &envs {
			assert!(env.env_name.is_none());
		}
	}

	#[test]
	fn test_skip_vendor_directory() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create env in vendor (should be skipped)
		fs::create_dir_all(root.join("vendor/somelib")).unwrap();
		fs::write(root.join("vendor/somelib/main.jsonnet"), "{}").unwrap();
		fs::write(
			root.join("vendor/somelib/spec.json"),
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment"}"#,
		)
		.unwrap();

		// Create actual env at root level
		fs::write(root.join("main.jsonnet"), "{}").unwrap();
		fs::write(
			root.join("spec.json"),
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"root"},"spec":{"namespace":"default"}}"#,
		)
		.unwrap();

		let envs: Vec<_> = Discover::new(test_evaluator(), vec![root.to_path_buf()])
			.collect::<Result<Vec<_>>>()
			.unwrap();
		assert_eq!(envs.len(), 1);
		assert_eq!(envs[0].path.as_path(), root);
	}

	#[test]
	fn test_no_duplicate_environments() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();
		fs::write(
			root.join("env/spec.json"),
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"env"},"spec":{"namespace":"default"}}"#,
		)
		.unwrap();

		// Pass the same path twice
		let envs: Vec<_> =
			Discover::new(test_evaluator(), vec![root.join("env"), root.join("env")])
				.collect::<Result<Vec<_>>>()
				.unwrap();
		assert_eq!(envs.len(), 1);
	}
}
