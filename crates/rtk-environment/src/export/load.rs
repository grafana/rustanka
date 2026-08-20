//! Evaluating a discovered environment, manifests included.
//!
//! Discovery deliberately throws an environment's `data` away, so that listing
//! environments does not manifest them. Exporting needs it, so this evaluates the
//! environment again — this time materializing what it evaluates to, so that
//! everything downstream works on owned JSON that can cross threads.

use std::path::Path;
use std::sync::Arc;

use rtk_jsonnet::jpath::JPath;
use rtk_spec::canonical::Environment;
use serde::Deserialize as _;

use crate::export::{Error, LoadedEnvironment, OptionalData, process};
use crate::{Discovered, Engine, Search};

/// External variable Tanka exposes an environment's own spec through.
const ENVIRONMENT_EXT_CODE: &str = "tanka.dev/environment";

/// Selects a single inline environment by name, discarding the rest.
///
/// Mirrors Tanka's `SingleEnvEvalScript` (`pkg/tanka/evaluators.go`): the
/// environment stays where it was declared, and everything else collapses, so a
/// file declaring several environments can be exported one at a time.
const SINGLE_ENVIRONMENT_EVAL_SCRIPT: &str = r"
local singleEnv(object) =
  if std.isObject(object)
  then
    if std.objectHas(object, 'apiVersion')
       && std.objectHas(object, 'kind')
    then
      if object.kind == 'Environment'
         && std.member(object.metadata.name, '%s')
      then object
      else {}
    else
      std.mapWithKey(
        function(key, obj)
          singleEnv(obj),
        object
      )
  else if std.isArray(object)
  then
    std.map(
      function(obj)
        singleEnv(obj),
      object
    )
  else {};

local selected = singleEnv(main);
";

impl Engine {
	/// Resolve an environment again the way Tanka's export does, from its
	/// `metadata.namespace` rather than from where it was found.
	///
	/// `parallelLoadEnvironments` keeps only an environment's name and namespace,
	/// then reloads it from `filepath.Join(FindRoot(namespace), namespace)`.
	/// Because `metadata.namespace` is the entrypoint relative to *that
	/// environment's own* project root, and `FindRoot` resolves a relative path
	/// against the process working directory, the round trip is the identity only
	/// when the two roots agree. They do for every ordinary layout, so this
	/// almost always resolves back to the environment it started from.
	///
	/// Where they disagree, tk exports something else, and so does this: an
	/// environment that vendors for itself inside another project has a namespace
	/// relative to itself, which re-anchors somewhere else entirely. Reproduced
	/// rather than corrected, because matching tk's output is the point.
	///
	/// Returns `None` when the environment resolves back to itself, which is the
	/// common case and needs no further work.
	pub fn reresolve(
		&self,
		discovered: &Discovered,
		working_directory: &Path,
	) -> Option<Discovered> {
		let namespace = discovered.environment.metadata.namespace.as_deref()?;
		let root = JPath::project_root(working_directory.join(namespace)).ok()?;
		let entrypoint = root.join(namespace);

		// Only the directory is re-resolved: discovery yields environments by
		// directory, and an entrypoint is named relative to the root it was found
		// under, so a bare filename means the root itself.
		let directory = entrypoint.parent()?;
		if directory == discovered.path.as_path() {
			return None;
		}

		let mut candidates = self
			.resolve_candidate(Arc::new(directory.to_path_buf()))
			.ok()?;
		if candidates.len() > 1 {
			// An inline entrypoint declares several environments; tk carries the
			// name across and selects with it.
			let name = discovered.environment.metadata.name.as_deref();
			candidates.retain(|candidate| candidate.environment.metadata.name.as_deref() == name);
		}

		match candidates.as_slice() {
			[_] => candidates.pop(),
			_ => None,
		}
	}

	/// Evaluate a discovered environment, keeping its manifests.
	pub fn load(&self, discovered: &Discovered) -> Result<LoadedEnvironment, Error> {
		let entrypoint = discovered.path.join(JPath::DEFAULT_ENTRYPOINT);
		let jpath = JPath::resolve(&entrypoint)?;

		let evaluation = self.evaluate(discovered, &jpath)?;

		if discovered.is_static {
			// A static environment's spec comes from `spec.json`, and everything
			// the entrypoint evaluates to is its manifests.
			return Environment::new()
				.with_metadata(discovered.environment.metadata.clone())
				.with_spec(discovered.environment.spec.clone())
				.with_data(OptionalData::new(evaluation))
				.build()
				.map(LoadedEnvironment::configured)
				.map_err(|source| Error::Spec { source });
		}

		// An inline environment declares itself somewhere inside the evaluated
		// value, wherever the Jsonnet put it.
		let Some(mut environment) = LoadedEnvironment::find_in(evaluation)? else {
			return Environment::new()
				.with_metadata(discovered.environment.metadata.clone())
				.with_spec(discovered.environment.spec.clone())
				.with_data(OptionalData::none())
				.build()
				.map(LoadedEnvironment::configured)
				.map_err(|source| Error::Spec { source });
		};

		// Discovery worked out where the environment lives; the evaluated object
		// cannot know it.
		environment
			.environment
			.metadata
			.namespace
			.clone_from(&discovered.environment.metadata.namespace);

		Ok(environment)
	}

	/// Discover and evaluate exactly one environment or bare Jsonnet entrypoint.
	pub fn load_single(&self, path: &Path, name: Option<&str>) -> Result<LoadedEnvironment, Error> {
		let jpath = JPath::resolve(path)?;
		let directory = jpath
			.entrypoint
			.parent()
			.map(Path::to_path_buf)
			.unwrap_or_else(|| jpath.base_directory.clone());
		// Exactly the environment that was asked for, as tk's `Peek` loads it;
		// anything below it belongs to a different environment.
		let mut discovered = self
			.discover(vec![directory.clone()], Search::Environment)
			.collect::<Result<Vec<_>, _>>()?;
		discovered.retain(|environment| environment.path.as_path() == directory);
		let mut available = discovered
			.iter()
			.filter_map(|environment| environment.environment.metadata.name.as_deref())
			.map(str::to_owned)
			.collect::<Vec<_>>();
		available.sort();

		if let Some(name) = name {
			let mut exact = Vec::new();
			let mut partial = Vec::new();
			for environment in discovered {
				let environment_name = environment.environment.metadata.name.as_deref();
				if environment_name == Some(name) {
					exact.push(environment);
				} else if environment_name.is_some_and(|candidate| candidate.contains(name))
					|| environment.path.to_string_lossy().contains(name)
				{
					partial.push(environment);
				}
			}
			discovered = if exact.is_empty() { partial } else { exact };
		}

		match discovered.as_slice() {
			[environment] => return self.load(environment),
			[] if name.is_some() => {
				return Err(Error::NoEnvironmentNamed {
					name: name.expect("matched arm").to_owned(),
					available: available.join(", "),
				});
			}
			[] => {}
			[_, _, ..] => {
				let mut names = discovered
					.iter()
					.filter_map(|environment| environment.environment.metadata.name.as_deref())
					.collect::<Vec<_>>();
				names.sort_unstable();
				let names = names.join("\n - ");
				return Err(match name {
					Some(name) => Error::MultipleEnvironmentsNamed {
						path: path.display().to_string(),
						name: name.to_owned(),
						names,
					},
					None => Error::MultipleEnvironments {
						path: path.display().to_string(),
						names,
					},
				});
			}
		}

		self.load_bare(jpath)
	}

	fn load_bare(&self, jpath: JPath) -> Result<LoadedEnvironment, Error> {
		let options = self.jsonnet.options();
		let mut evaluator = self.jsonnet.create_evaluator();
		options.apply(&mut evaluator)?;
		evaluator.with_import_paths(jpath.import_paths.clone())?;

		let evaluation =
			evaluator.evaluate_snippet(self.entrypoint_snippet(&jpath.entrypoint, "main"))?;

		let data = process::materialize(&evaluation.into_value())?;
		LoadedEnvironment::bare(data)
	}

	fn evaluate(&self, discovered: &Discovered, jpath: &JPath) -> Result<serde_json::Value, Error> {
		let options = self.jsonnet.options();

		// Discovery evaluated this environment without knowing what it asked for,
		// since what it asked for is what discovery was reading. Now that its
		// spec is known, it is evaluated the way it wanted to be.
		let mut evaluator = self
			.jsonnet
			.create_evaluator_for(Some(&discovered.environment.spec));
		options.apply(&mut evaluator)?;
		evaluator.with_import_paths(jpath.import_paths.clone())?;

		// Tanka lets an environment read its own spec. Inline environments
		// declare theirs in the Jsonnet being evaluated, so only static ones have
		// something to expose here.
		if discovered.is_static {
			let spec = serde_json::to_string(&discovered.environment)?;
			evaluator.with_external_code(ENVIRONMENT_EXT_CODE, &spec)?;
		}

		let (selection, root) = match discovered.selected_by() {
			// Selecting one of several inline environments has to happen inside
			// Jsonnet, before anything is manifested: the environments that were
			// not asked for should not be evaluated at all.
			Some(name) => (
				SINGLE_ENVIRONMENT_EVAL_SCRIPT.replace("%s", name),
				"selected",
			),
			None => (String::new(), "main"),
		};
		let script = format!("{selection}\n{root}");
		let evaluation =
			evaluator.evaluate_snippet(self.entrypoint_snippet(&jpath.entrypoint, &script))?;

		process::materialize(&evaluation.into_value())
	}
}

impl LoadedEnvironment {
	/// Find the environment an evaluated document declares, wherever it is.
	///
	/// Takes the environment out of the document rather than copying it: an
	/// environment's `data` is the bulk of what was evaluated.
	fn find_in(value: serde_json::Value) -> Result<Option<LoadedEnvironment>, Error> {
		match value {
			serde_json::Value::Array(values) => {
				for value in values {
					if let Some(environment) = Self::find_in(value)? {
						return Ok(Some(environment));
					}
				}
			}
			serde_json::Value::Object(object) => {
				if object.contains_key("apiVersion") && object.contains_key("kind") {
					if object.get("kind").and_then(serde_json::Value::as_str) != Some("Environment")
					{
						return Ok(None);
					}
					let environment = Environment::deserialize(serde_json::Value::Object(object))
						.map_err(|source| Error::Environment { source })?;
					return Ok(Some(LoadedEnvironment::configured(environment)));
				}

				for (_, value) in object {
					if let Some(environment) = Self::find_in(value)? {
						return Ok(Some(environment));
					}
				}
			}
			_ => {}
		}

		Ok(None)
	}
}
