use std::convert::Infallible;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc as StdRc;
use std::sync::{Arc, RwLock, Weak};

use rtk_jsonnet_core::{
	Context as _, Evaluator as _, FlagsExt, Function, Implementation, Value as _,
};
use rtk_jsonnet_jrsonnet::{
	Error as JrsonnetError, Evaluation as JrsonnetEvaluation, Evaluator as JrsonnetEvaluator,
	EvaluatorError as JrsonnetEvaluatorError, Flag as JrsonnetFlag,
	Implementation as JrsonnetImplementation,
};
use rtk_spec::DeepMerge;
use rtk_spec::canonical::{JsonnetImplementation, Rc};
use rustc_hash::FxHashMap;
use thiserror::Error;

/// An error returned by one of the various Jsonnet implementations.
#[derive(Debug, Error)]
pub enum Error {
	#[error("a jrsonnet error occurred")]
	Jrsonnet(#[from] JrsonnetError),
}

impl From<Infallible> for Error {
	fn from(_: Infallible) -> Self {
		unreachable!()
	}
}

impl From<JrsonnetEvaluatorError> for Error {
	fn from(error: JrsonnetEvaluatorError) -> Self {
		JrsonnetError::Evaluator(error).into()
	}
}

#[derive(Clone, Debug)]
pub struct Engine(Arc<EngineInternals>);

impl Engine {
	pub fn new(options: Options) -> Engine {
		Engine(Arc::new_cyclic(|engine| EngineInternals {
			options,
			implementations: RwLock::new(Implementations {
				engine: engine.clone(),
				..Default::default()
			}),
		}))
	}

	pub fn create_evaluator(&self) -> Evaluator {
		Evaluator {
			engine: self.0.clone(),
			options: self.0.options.clone(),
			evaluator: None,
		}
	}

	pub fn options(&self) -> &Options {
		&self.0.options
	}
}

#[derive(Debug)]
struct EngineInternals {
	options: Options,
	implementations: RwLock<Implementations>,
}

#[derive(Debug)]
pub struct Evaluator {
	engine: Arc<EngineInternals>,
	options: Options,
	evaluator: Option<ImplementationEvaluator>,
}

macro_rules! call_implementation_evaluator_method {
    ($self:ident, $method:ident, $($argument:expr),* $(,)?) => {
        match &mut $self.evaluator {
            Some(ImplementationEvaluator::Jrsonnet(jrsonnet)) => jrsonnet.$method($($argument),*)?,
            None => {
                let implementation = $self.options.rc.spec.jsonnet_implementation
                    .as_ref()
                    .map(|i| i.implementation())
                    .cloned()
                    .unwrap_or_default();
                match $self.populate_evaluator(implementation)? {
                    ImplementationEvaluator::Jrsonnet(jrsonnet) => jrsonnet.$method($($argument),*)?,
                }
            },
        }
    };
    (@no_insert: $self:ident, $method:ident, $($argument:expr),* $(,)?) => {
        match &mut $self.evaluator {
            Some(ImplementationEvaluator::Jrsonnet(jrsonnet)) => jrsonnet.$method($($argument),*)?,
            None => panic!("attempt to use evaluator before population"),
        }
    };
    (@with: $evaluator:ident, $method:ident, $($argument:expr),* $(,)?) => {
        match $evaluator {
            ImplementationEvaluator::Jrsonnet(jrsonnet) => jrsonnet.$method($($argument),*)?,
        }
    };
}

impl Evaluator {
	pub fn with_rc(&mut self, rc: Rc) -> Result<&mut Self, Error> {
		self.options.rc.spec.merge_from(rc.spec);
		let implementation = self
			.options
			.rc
			.spec
			.jsonnet_implementation
			.as_ref()
			.map(rtk_spec::canonical::JsonentImplementationOrConfig::implementation)
			.cloned()
			.unwrap_or_default();
		self.populate_evaluator(implementation)?;
		call_implementation_evaluator_method!(@no_insert: self, with_rc, self.options.rc.clone());
		Ok(self)
	}

	pub fn with_import_paths(&mut self, import_paths: Vec<PathBuf>) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_import_paths, import_paths);
		Ok(self)
	}

	pub fn with_external_code(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_external_code, key, value);
		Ok(self)
	}

	pub fn with_native_function<F>(&mut self, key: &str, function: F) -> Result<&mut Self, Error>
	where
		F: 'static + Function<JrsonnetEvaluator>,
	{
		call_implementation_evaluator_method!(self, with_native_function, key, function);
		Ok(self)
	}

	pub fn with_external_variable(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_external_variable, key, value);
		Ok(self)
	}

	pub fn with_top_level_argument(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_top_level_argument, key, value);
		Ok(self)
	}

	pub fn with_top_level_code(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_top_level_code, key, value);
		Ok(self)
	}

	pub fn evaluate_file<P>(mut self, path: P) -> Result<Evaluation, Error>
	where
		P: AsRef<Path> + fmt::Debug,
	{
		if self.evaluator.is_none() {
			let implementation = self.selected_implementation();
			self.populate_evaluator(implementation)?;
		}
		match self.evaluator.take().expect("evaluator was populated") {
			ImplementationEvaluator::Jrsonnet(evaluator) => evaluator
				.evaluate_file(path)
				.map(JrsonnetEvaluation::from)
				.map(Evaluation::Jrsonnet)
				.map_err(|error| JrsonnetError::Evaluator(error).into()),
		}
	}

	pub fn evaluate_snippet<S>(mut self, snippet: S) -> Result<Evaluation, Error>
	where
		S: AsRef<str> + fmt::Debug,
	{
		if self.evaluator.is_none() {
			let implementation = self.selected_implementation();
			self.populate_evaluator(implementation)?;
		}
		match self.evaluator.take().expect("evaluator was populated") {
			ImplementationEvaluator::Jrsonnet(evaluator) => evaluator
				.evaluate_snippet(snippet)
				.map(JrsonnetEvaluation::from)
				.map(Evaluation::Jrsonnet)
				.map_err(|error| JrsonnetError::Evaluator(error).into()),
		}
	}

	fn selected_implementation(&self) -> JsonnetImplementation {
		self.options
			.rc
			.spec
			.jsonnet_implementation
			.as_ref()
			.map(rtk_spec::canonical::JsonentImplementationOrConfig::implementation)
			.cloned()
			.unwrap_or_default()
	}
}

impl Evaluator {
	fn populate_evaluator(
		&mut self,
		implementation: JsonnetImplementation,
	) -> Result<&mut ImplementationEvaluator, Error> {
		let mut implementations = self
			.engine
			.implementations
			.write()
			.expect("implementations should not be poisoned");

		implementations.maybe_init_implementation(implementation.clone())?;

		// TODO: Fix for multiple implementations.
		let evaluator = if let (
			JsonnetImplementation::Jrsonnet,
			Implementations {
				jrsonnet: Some(jrsonnet),
				..
			},
		) = (implementation, &*implementations)
		{
			self.evaluator.insert(ImplementationEvaluator::Jrsonnet(
				jrsonnet.create_evaluator(),
			))
		} else {
			drop(implementations);
			return self.populate_evaluator(JsonnetImplementation::Jrsonnet);
		};

		if !self.options.rc.spec.disable_native_functions {
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_native_functions::Plugin::new());
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_regex::Plugin::new());
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_helm::Plugin::new());
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_kustomize::Plugin::new());
		}

		Ok(evaluator)
	}
}

#[derive(Debug)]
enum ImplementationEvaluator {
	Jrsonnet(JrsonnetEvaluator),
}

#[derive(Debug)]
pub enum Evaluation {
	Jrsonnet(JrsonnetEvaluation),
}

impl Evaluation {
	pub fn into_value(self) -> EvaluationValue {
		match self {
			Evaluation::Jrsonnet(evaluation) => {
				let value = evaluation.value().clone();
				EvaluationValue::Jrsonnet {
					evaluation: StdRc::new(evaluation),
					value,
				}
			}
		}
	}
}

impl serde::Serialize for Evaluation {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		match self {
			Evaluation::Jrsonnet(evaluation) => evaluation.serialize(serializer),
		}
	}
}

#[derive(Clone, Debug)]
pub enum EvaluationValue {
	Jrsonnet {
		evaluation: StdRc<JrsonnetEvaluation>,
		value: rtk_jsonnet_jrsonnet::Value,
	},
}

impl EvaluationValue {
	pub fn into_array(self) -> Result<EvaluationArray, Self> {
		match self {
			EvaluationValue::Jrsonnet { evaluation, value } => match value.into_array() {
				Ok(array) => Ok(EvaluationArray::Jrsonnet { evaluation, array }),
				Err(value) => Err(EvaluationValue::Jrsonnet { evaluation, value }),
			},
		}
	}

	pub fn into_object(self) -> Result<EvaluationObject, Self> {
		match self {
			EvaluationValue::Jrsonnet { evaluation, value } => match value.into_object() {
				Ok(object) => Ok(EvaluationObject::Jrsonnet { evaluation, object }),
				Err(value) => Err(EvaluationValue::Jrsonnet { evaluation, value }),
			},
		}
	}

	pub fn deserialize<'de, T>(self) -> Result<T, Error>
	where
		T: serde::Deserialize<'de>,
	{
		match self {
			EvaluationValue::Jrsonnet { evaluation, value } => evaluation
				.with_context(|context| T::deserialize(context.create_deserializer(value)))
				.map_err(Error::from),
		}
	}
}

#[derive(Clone, Debug)]
pub enum EvaluationArray {
	Jrsonnet {
		evaluation: StdRc<JrsonnetEvaluation>,
		array: rtk_jsonnet_jrsonnet::Array,
	},
}

impl EvaluationArray {
	pub fn into_values(self) -> EvaluationArrayValues {
		match self {
			EvaluationArray::Jrsonnet { evaluation, array } => EvaluationArrayValues::Jrsonnet {
				evaluation,
				values: array.into_values(),
			},
		}
	}
}

pub enum EvaluationArrayValues {
	Jrsonnet {
		evaluation: StdRc<JrsonnetEvaluation>,
		values: rtk_jsonnet_jrsonnet::ArrayValues,
	},
}

impl Iterator for EvaluationArrayValues {
	type Item = Result<EvaluationValue, Error>;

	fn next(&mut self) -> Option<Self::Item> {
		match self {
			EvaluationArrayValues::Jrsonnet { evaluation, values } => {
				evaluation.with_context(|_| {
					values.next().map(|value| {
						value
							.map(|value| EvaluationValue::Jrsonnet {
								evaluation: StdRc::clone(evaluation),
								value,
							})
							.map_err(Error::from)
					})
				})
			}
		}
	}
}

#[derive(Clone, Debug)]
pub enum EvaluationObject {
	Jrsonnet {
		evaluation: StdRc<JrsonnetEvaluation>,
		object: rtk_jsonnet_jrsonnet::Object,
	},
}

impl EvaluationObject {
	pub fn has(&self, key: &str) -> Result<bool, Error> {
		use rtk_jsonnet_core::Object as _;

		match self {
			EvaluationObject::Jrsonnet { evaluation, object } => evaluation
				.with_context(|_| object.has(key))
				.map_err(Error::from),
		}
	}

	pub fn get(&self, key: &str) -> Result<EvaluationValue, Error> {
		use rtk_jsonnet_core::Object as _;

		match self {
			EvaluationObject::Jrsonnet { evaluation, object } => evaluation
				.with_context(|_| object.get(key))
				.map(|value| EvaluationValue::Jrsonnet {
					evaluation: StdRc::clone(evaluation),
					value,
				})
				.map_err(Error::from),
		}
	}

	pub fn into_values(self) -> EvaluationObjectValues {
		match self {
			EvaluationObject::Jrsonnet { evaluation, object } => EvaluationObjectValues::Jrsonnet {
				evaluation,
				values: object.into_values(),
			},
		}
	}
}

pub enum EvaluationObjectValues {
	Jrsonnet {
		evaluation: StdRc<JrsonnetEvaluation>,
		values: rtk_jsonnet_jrsonnet::ObjectValues,
	},
}

impl Iterator for EvaluationObjectValues {
	type Item = Result<EvaluationValue, Error>;

	fn next(&mut self) -> Option<Self::Item> {
		match self {
			EvaluationObjectValues::Jrsonnet { evaluation, values } => {
				evaluation.with_context(|_| {
					values.next().map(|value| {
						value
							.map(|value| EvaluationValue::Jrsonnet {
								evaluation: StdRc::clone(evaluation),
								value,
							})
							.map_err(Error::from)
					})
				})
			}
		}
	}
}

#[derive(Debug, Default)]
struct Implementations {
	engine: Weak<EngineInternals>,
	jrsonnet: Option<JrsonnetImplementation>,
}

impl Implementations {
	pub fn maybe_init_implementation(
		&mut self,
		implementation: JsonnetImplementation,
	) -> Result<(), Error> {
		let Some(engine) = Weak::upgrade(&self.engine) else {
			panic!("attempt to use implementations after engine is dropped");
		};

		match implementation {
			JsonnetImplementation::Reference => {
				tracing::warn!("the `reference` implementation is not implemented");
				Ok(())
			}
			JsonnetImplementation::GoJsonnet => {
				tracing::warn!("the `go-jsonnet` implementation is not implemented");
				Ok(())
			}
			JsonnetImplementation::Jrsonnet if self.jrsonnet.is_none() => {
				let flags = engine
					.options
					.rc
					.flags::<JrsonnetFlag>()
					.map_err(JrsonnetError::Flag)?;
				self.jrsonnet = Some(JrsonnetImplementation::new(flags)?);
				Ok(())
			}
			JsonnetImplementation::Jrsonnet => Ok(()),
			JsonnetImplementation::Binary(binary) => {
				tracing::warn!(binary = ?binary, "the `binary:*` implementation is not implemented");
				Ok(())
			}
		}
	}
}

#[derive(Clone, Debug, Default)]
pub struct Options {
	pub rc: Rc,

	pub ext_code: FxHashMap<Box<str>, Box<str>>,
	pub ext_variables: FxHashMap<Box<str>, Box<str>>,
	pub top_level_arguments: FxHashMap<Box<str>, Box<str>>,
	pub top_level_code: FxHashMap<Box<str>, Box<str>>,
}

impl Options {
	pub fn apply(&self, evaluator: &mut Evaluator) -> Result<(), Error> {
		evaluator.with_rc(self.rc.clone())?;

		for (top_level_code, value) in &self.ext_code {
			evaluator.with_external_code(top_level_code, value)?;
		}
		for (top_level_args, value) in &self.ext_variables {
			evaluator.with_external_variable(top_level_args, value)?;
		}

		for (top_level_args, value) in &self.top_level_arguments {
			evaluator.with_top_level_argument(top_level_args, value)?;
		}
		for (top_level_code, value) in &self.top_level_code {
			evaluator.with_top_level_code(top_level_code, value)?;
		}

		Ok(())
	}

	pub fn has_top_level_args(&self) -> bool {
		!self.top_level_arguments.is_empty() || !self.top_level_code.is_empty()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn evaluates_composed_native_plugins() {
		let evaluation = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(
				r#"{
					escaped: std.native("escapeStringRegex")("a.b"),
					json: std.native("manifestJsonFromJson")('{"a":1}', 2),
					matched: std.native("regexMatch")("^a", "abc"),
					parsedJson: std.native("parseJson")('{"a":1}'),
					parsedYaml: std.native("parseYaml")("mode: 0755"),
					sha256: std.native("sha256")("foo"),
					substituted: std.native("regexSubst")("a", "banana", "o"),
					yaml: std.native("manifestYamlFromJson")('{"a":1}'),
				}"#,
			)
			.unwrap();
		assert_eq!(
			serde_json::to_value(evaluation).unwrap(),
			serde_json::json!({
				"escaped": "a\\.b",
				"json": "{\n  \"a\": 1\n}\n",
				"matched": true,
				"parsedJson": {"a": 1},
				"parsedYaml": [{"mode": 493}],
				"sha256": "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
				"substituted": "bonono",
				"yaml": "a: 1\n",
			})
		);
	}

	#[test]
	fn honors_top_level_native_function_disable() {
		let mut options = Options::default();
		options.rc.spec.disable_native_functions = true;
		let result = Engine::new(options)
			.create_evaluator()
			.evaluate_snippet(r#"std.native("sha256")("foo")"#);
		assert!(result.is_err());
	}

	#[test]
	fn lazily_traverses_native_function_values_with_their_context() {
		let value = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(
				r#"{
					array: [std.native("sha256")("array")],
					object: { value: std.native("sha256")("object") },
				}"#,
			)
			.unwrap()
			.into_value();
		let object = value.into_object().expect("an object");

		let array = object.get("array").unwrap().into_array().expect("an array");
		let array_value: String = array
			.into_values()
			.next()
			.expect("an element")
			.unwrap()
			.deserialize()
			.unwrap();
		assert_eq!(
			array_value,
			"dbe42cc09c16704aa3d60127c60b4e1646fc6da1d4764aa517de053e65a663d7"
		);

		let nested = object
			.get("object")
			.unwrap()
			.into_object()
			.expect("an object");
		let object_value: String = nested.get("value").unwrap().deserialize().unwrap();
		assert_eq!(
			object_value,
			"2958d416d08aa5a472d7b509036cb7eafd542add84527e66a145ea64cb4cdc75"
		);
	}
}
