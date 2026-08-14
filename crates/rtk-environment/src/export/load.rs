//! Evaluating a discovered environment, manifests included.
//!
//! Discovery deliberately throws an environment's `data` away, so that listing
//! environments does not manifest them. Exporting needs it, so this evaluates the
//! environment again — this time keeping the manifests, captured rather than
//! walked.

use rtk_jsonnet::jpath::JPath;
use rtk_jsonnet::{EvaluationValue, Hidden};
use rtk_spec::canonical::Environment;

use crate::export::{Error, LoadedEnvironment, OptionalData};
use crate::{Discovered, Engine};

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

singleEnv(main)
";

impl Engine {
	/// Evaluate a discovered environment, keeping its manifests.
	///
	/// The manifests are captured, not walked: nothing beneath them is forced
	/// until they are exported.
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
				.map_err(|source| Error::Spec { source });
		}

		// An inline environment declares itself somewhere inside the evaluated
		// value, wherever the Jsonnet put it.
		let Some(mut environment) = find_environment(&evaluation)? else {
			return Environment::new()
				.with_metadata(discovered.environment.metadata.clone())
				.with_spec(discovered.environment.spec.clone())
				.with_data(OptionalData::none())
				.build()
				.map_err(|source| Error::Spec { source });
		};

		// Discovery worked out where the environment lives; the evaluated object
		// cannot know it.
		environment
			.metadata
			.namespace
			.clone_from(&discovered.environment.metadata.namespace);

		Ok(environment)
	}

	fn evaluate(&self, discovered: &Discovered, jpath: &JPath) -> Result<EvaluationValue, Error> {
		let options = self.jsonnet.options();
		let mut evaluator = self.jsonnet.create_evaluator();
		options.apply(&mut evaluator)?;
		evaluator.with_import_paths(jpath.import_paths.clone())?;

		// Tanka lets an environment read its own spec. Inline environments
		// declare theirs in the Jsonnet being evaluated, so only static ones have
		// something to expose here.
		if discovered.is_static {
			let spec = serde_json::to_string(&discovered.environment)?;
			evaluator.with_external_code(ENVIRONMENT_EXT_CODE, &spec)?;
		}

		let evaluation = match environment_name(discovered) {
			// Selecting one of several inline environments has to happen inside
			// Jsonnet, before anything is manifested.
			Some(name) => {
				let entrypoint = jpath
					.entrypoint
					.strip_prefix(&jpath.base_directory)
					.unwrap_or(&jpath.entrypoint)
					.to_string_lossy();
				let script = SINGLE_ENVIRONMENT_EVAL_SCRIPT.replace("%s", name);
				evaluator
					.evaluate_snippet(format!("local main = (import '{entrypoint}');\n{script}"))?
			}
			None => evaluator.evaluate_file(&jpath.entrypoint)?,
		};

		Ok(evaluation.into_value())
	}
}

/// The name to select an inline environment by, if it needs selecting.
fn environment_name(discovered: &Discovered) -> Option<&str> {
	if discovered.is_static {
		return None;
	}

	discovered.environment.metadata.name.as_deref()
}

/// Find the environment an evaluated value declares, wherever it is.
fn find_environment(value: &EvaluationValue) -> Result<Option<LoadedEnvironment>, Error> {
	let Some(object) = value.as_object() else {
		let Some(array) = value.as_array() else {
			return Ok(None);
		};
		for element in array.into_values() {
			if let Some(environment) = find_environment(&element?)? {
				return Ok(Some(environment));
			}
		}
		return Ok(None);
	};

	if object.has("apiVersion", Hidden::Skip)? && object.has("kind", Hidden::Skip)? {
		let kind = object
			.get("kind", Hidden::Skip)?
			.and_then(|kind| kind.as_str());
		if kind.as_deref() != Some("Environment") {
			return Ok(None);
		}
		return Ok(Some(value.clone().deserialize()?));
	}

	for value in object.into_values() {
		if let Some(environment) = find_environment(&value?)? {
			return Ok(Some(environment));
		}
	}

	Ok(None)
}
