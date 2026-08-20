use std::convert::Infallible;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc as StdRc;
use std::sync::{Arc, RwLock, Weak};

use rtk_jsonnet_core::{
	Context as _, Evaluator as _, FlagsExt, Function, Hidden, Implementation, RawValue, Value as _,
};
use rtk_jsonnet_jrsonnet::{
	Error as JrsonnetError, Evaluation as JrsonnetEvaluation, Evaluator as JrsonnetEvaluator,
	EvaluatorError as JrsonnetEvaluatorError, Flag as JrsonnetFlag,
	Implementation as JrsonnetImplementation,
};
use rtk_spec::DeepMerge;
use rtk_spec::canonical::{EnvironmentSpec, JsonnetImplementation, Rc};
use rustc_hash::FxHashMap;
use thiserror::Error;

/// An error returned by one of the various Jsonnet implementations.
#[derive(Debug, Error)]
pub enum Error {
	/// Renders as the error underneath it: which implementation raised it is
	/// already clear from what it says, and is not the reason for it.
	#[error(transparent)]
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
		self.create_evaluator_for(None)
	}

	/// Create an evaluator for a particular environment.
	///
	/// An environment may ask to be evaluated by another Jsonnet implementation,
	/// which decides both how the result is formatted and whether Tanka's native
	/// functions exist at all — so it has to be known before the evaluator is
	/// configured, not applied to one afterwards. Taking it here is what makes
	/// that impossible to get wrong.
	pub fn create_evaluator_for(&self, environment: Option<&EnvironmentSpec>) -> Evaluator {
		Evaluator {
			engine: self.0.clone(),
			options: self.0.options.clone(),
			environment: environment.cloned(),
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
	environment: Option<EnvironmentSpec>,
	evaluator: Option<ImplementationEvaluator>,
}

/// A configured jrsonnet context for integrations with a custom `State`.
#[derive(Clone)]
pub struct JrsonnetContext(rtk_jsonnet_jrsonnet::Evaluator);

impl JrsonnetContext {
	pub fn context_initializer(&self) -> rtk_jsonnet_jrsonnet::ContextInitializer {
		self.0.context_initializer()
	}

	pub fn with_current<T>(&self, callback: impl FnOnce() -> T) -> T {
		self.0.with_current(callback)
	}
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
	/// Build a jrsonnet context initializer with this evaluator's native plugins.
	///
	/// This is for integrations that need a custom jrsonnet import resolver while
	/// retaining the same native functions as the main engine.
	pub fn jrsonnet_context(mut self) -> Result<JrsonnetContext, Error> {
		let options = self.options.clone();
		options.apply(&mut self)?;
		if self.evaluator.is_none() {
			let implementation = self.selected_implementation();
			self.populate_evaluator(implementation)?;
		}
		match self.evaluator.expect("evaluator was populated") {
			ImplementationEvaluator::Jrsonnet(evaluator) => Ok(JrsonnetContext(evaluator)),
		}
	}

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
		// Creating an evaluator only reads the shared implementation; only the
		// first one has to initialize it. Taking the write lock unconditionally
		// would serialize every environment's evaluator construction — the whole
		// standard library included — across the pool exporting them.
		let mut created = {
			let implementations = self
				.engine
				.implementations
				.read()
				.expect("implementations should not be poisoned");
			implementations.create_evaluator(&implementation)
		};

		if created.is_none() {
			let mut implementations = self
				.engine
				.implementations
				.write()
				.expect("implementations should not be poisoned");
			implementations.maybe_init_implementation(implementation.clone())?;
			created = implementations.create_evaluator(&implementation);
		}

		// TODO: Fix for multiple implementations.
		let Some(created) = created else {
			// Not an implementation rtk has of its own, as the warning said.
			return self.populate_evaluator(JsonnetImplementation::Jrsonnet);
		};
		let evaluator = self.evaluator.insert(created);

		// tk drops Tanka's native functions for an environment evaluated by a
		// jrsonnet binary, which knows nothing about them, so an environment that
		// asks for one has to lose them here as well — it may well be written to
		// notice their absence and fall back.
		let native_functions = !self.options.rc.spec.disable_native_functions
			&& !self
				.environment
				.as_ref()
				.is_some_and(EnvironmentSpec::emulates_jrsonnet);

		if native_functions {
			// This native stores implementation values directly and has to use
			// each evaluator's own lazy callback API rather than the generic,
			// serde-based plugin interface.
			call_implementation_evaluator_method!(@with: evaluator, with_rtk_memoize,);
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_native_functions::Plugin::new());
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_regex::Plugin::new());
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_helm::Plugin::new());
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_kustomize::Plugin::new());
		}

		// Applied on every population, not once: `with_rc` populates again, and
		// each population builds a fresh implementation evaluator that knows
		// nothing of what the last one was told.
		if let Some(environment) = &self.environment {
			call_implementation_evaluator_method!(@with: evaluator, with_environment, environment);
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

impl serde::Serialize for EvaluationValue {
	/// Serializes through the serde data model, which is lossy for numbers no
	/// data model can represent: use [`EvaluationValue::manifest_into`] for
	/// output that has to match another Jsonnet implementation exactly.
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		match self {
			EvaluationValue::Jrsonnet { evaluation, value } => {
				evaluation.with_context(|_| value.serialize(serializer))
			}
		}
	}
}

/// A value captured out of an evaluation without deserializing it.
///
/// Produced by deserializing anything that holds a
/// [`RawValue`](rtk_jsonnet_core::RawValue) — an [`Environment`]'s `data`, in
/// practice — and turned back into an [`EvaluationValue`] with
/// [`EvaluationValue::attach`].
///
/// [`Environment`]: rtk_spec::canonical::Environment
#[derive(Clone, Debug)]
pub enum RawEvaluationValue {
	Jrsonnet(RawValue<rtk_jsonnet_jrsonnet::Value>),
}

impl<'de> serde::Deserialize<'de> for RawEvaluationValue {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		RawValue::deserialize(deserializer).map(RawEvaluationValue::Jrsonnet)
	}
}

impl serde::Serialize for RawEvaluationValue {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		match self {
			RawEvaluationValue::Jrsonnet(value) => value.serialize(serializer),
		}
	}
}

impl DeepMerge for RawEvaluationValue {
	/// Values are opaque, so merging can only replace.
	fn merge_from(&mut self, other: Self) {
		*self = other;
	}
}

/// `null`, from the default implementation.
impl Default for RawEvaluationValue {
	fn default() -> Self {
		RawEvaluationValue::Jrsonnet(RawValue::default())
	}
}

impl rtk_spec::v1alpha1::EnvironmentData<'_> for RawEvaluationValue {
	fn present() -> bool {
		true
	}
}

impl EvaluationValue {
	/// Rebuild a value captured during a deserialization that is still going on.
	///
	/// Deserializing happens inside the evaluation's context, so a value
	/// captured there can be paired with that context immediately, which is what
	/// makes captured data usable without having to hold on to the evaluation it
	/// came from. Returns [`None`] when called outside a deserialization, where
	/// there is no context to pair it with.
	pub fn current(raw: RawEvaluationValue) -> Option<EvaluationValue> {
		match raw {
			RawEvaluationValue::Jrsonnet(raw) => {
				let context = rtk_jsonnet_jrsonnet::Evaluator::current()?;
				let value = raw.into_inner();
				Some(EvaluationValue::Jrsonnet {
					evaluation: StdRc::new(JrsonnetEvaluation::with_shared_context(
						context,
						value.clone(),
					)),
					value,
				})
			}
		}
	}

	/// Pair a value captured out of *this* evaluation with its context again, so
	/// that the rest of it can be forced.
	///
	/// Infallible while jrsonnet is the only implementation; this will start
	/// rejecting values that came from a different one once it is not.
	#[must_use]
	pub fn attach(&self, raw: RawEvaluationValue) -> EvaluationValue {
		match (self, raw) {
			(EvaluationValue::Jrsonnet { evaluation, .. }, RawEvaluationValue::Jrsonnet(raw)) => {
				EvaluationValue::Jrsonnet {
					evaluation: StdRc::clone(evaluation),
					value: raw.into_inner(),
				}
			}
		}
	}

	/// Append this value's canonical JSON text to `buffer`.
	///
	/// This is the implementation's own manifestification rather than a serde
	/// round-trip, so output derived from it matches tk byte for byte. See
	/// [`rtk_jsonnet_core::Value::manifest_into`].
	pub fn manifest_into(&self, buffer: &mut String) -> Result<(), Error> {
		match self {
			EvaluationValue::Jrsonnet { evaluation, value } => evaluation
				.with_context(|_| value.manifest_into(buffer))
				.map_err(Error::from),
		}
	}

	/// [`EvaluationValue::manifest_into`] into a fresh [`String`].
	pub fn manifest(&self) -> Result<String, Error> {
		let mut buffer = String::new();
		self.manifest_into(&mut buffer)?;
		Ok(buffer)
	}

	/// Like [`EvaluationValue::into_array`], but without giving up ownership.
	pub fn as_array(&self) -> Option<EvaluationArray> {
		match self {
			EvaluationValue::Jrsonnet { evaluation, value } => {
				value.as_array().map(|array| EvaluationArray::Jrsonnet {
					evaluation: StdRc::clone(evaluation),
					array,
				})
			}
		}
	}

	/// Like [`EvaluationValue::into_object`], but without giving up ownership.
	pub fn as_object(&self) -> Option<EvaluationObject> {
		match self {
			EvaluationValue::Jrsonnet { evaluation, value } => {
				value.as_object().map(|object| EvaluationObject::Jrsonnet {
					evaluation: StdRc::clone(evaluation),
					object,
				})
			}
		}
	}

	/// This value as a string, if it is one.
	///
	/// None of the accessors need the evaluation's context: a value in hand has
	/// been evaluated already. Reach for these rather than
	/// [`EvaluationValue::deserialize`] when all that is wanted is to look at a
	/// value.
	pub fn as_str(&self) -> Option<EvaluationStr> {
		match self {
			EvaluationValue::Jrsonnet { value, .. } => value.as_str().map(EvaluationStr::Jrsonnet),
		}
	}

	/// This value as a number, if it is one.
	pub fn as_number(&self) -> Option<f64> {
		match self {
			EvaluationValue::Jrsonnet { value, .. } => value.as_number(),
		}
	}

	/// This value as a boolean, if it is one.
	pub fn as_bool(&self) -> Option<bool> {
		match self {
			EvaluationValue::Jrsonnet { value, .. } => value.as_bool(),
		}
	}

	/// Whether this value is `null`.
	pub fn is_null(&self) -> bool {
		match self {
			EvaluationValue::Jrsonnet { value, .. } => value.is_null(),
		}
	}

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

/// A string read out of an [`EvaluationValue`].
///
/// Derefs to [`str`], and cloning it is cheap, so it can be held on to as
/// happily as it can be compared.
#[derive(Clone, Debug)]
pub enum EvaluationStr {
	Jrsonnet(rtk_jsonnet_jrsonnet::Str),
}

impl std::ops::Deref for EvaluationStr {
	type Target = str;

	fn deref(&self) -> &str {
		match self {
			EvaluationStr::Jrsonnet(string) => string,
		}
	}
}

impl fmt::Display for EvaluationStr {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(self)
	}
}

impl PartialEq<str> for EvaluationStr {
	fn eq(&self, other: &str) -> bool {
		**self == *other
	}
}

impl PartialEq<&str> for EvaluationStr {
	fn eq(&self, other: &&str) -> bool {
		**self == **other
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
	/// This object's field names, without forcing their values.
	pub fn field_names(&self, hidden: Hidden) -> Vec<EvaluationStr> {
		match self {
			EvaluationObject::Jrsonnet { object, .. } => object
				.field_names(hidden)
				.into_iter()
				.map(EvaluationStr::Jrsonnet)
				.collect(),
		}
	}

	/// Evaluate this object's assertions without forcing its fields.
	pub fn run_assertions(&self) -> Result<(), Error> {
		match self {
			EvaluationObject::Jrsonnet { evaluation, object } => evaluation
				.with_context(|_| object.run_assertions())
				.map_err(Error::from),
		}
	}

	/// Whether the object has a field by this name.
	///
	/// Does not force the field.
	pub fn has(&self, key: &str, hidden: Hidden) -> Result<bool, Error> {
		use rtk_jsonnet_core::Object as _;

		match self {
			EvaluationObject::Jrsonnet { evaluation, object } => evaluation
				.with_context(|_| object.has(key, hidden))
				.map_err(Error::from),
		}
	}

	/// The field's value, or [`None`] if the object has no such field.
	///
	/// Forces the field. Use [`EvaluationObject::has`] to ask about a field
	/// whose value is not wanted.
	pub fn get(&self, key: &str, hidden: Hidden) -> Result<Option<EvaluationValue>, Error> {
		use rtk_jsonnet_core::Object as _;

		match self {
			EvaluationObject::Jrsonnet { evaluation, object } => evaluation
				.with_context(|_| object.get(key, hidden))
				.map(|value| {
					value.map(|value| EvaluationValue::Jrsonnet {
						evaluation: StdRc::clone(evaluation),
						value,
					})
				})
				.map_err(Error::from),
		}
	}

	/// [`EvaluationObject::get`], failing when the field is absent.
	pub fn get_or_bail(&self, key: &str, hidden: Hidden) -> Result<EvaluationValue, Error> {
		use rtk_jsonnet_core::Object as _;

		match self {
			EvaluationObject::Jrsonnet { evaluation, object } => evaluation
				.with_context(|_| object.get_or_bail(key, hidden))
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

	/// Like [`EvaluationObject::into_values`], but keeping each value's field
	/// name.
	pub fn into_fields(self) -> EvaluationObjectFields {
		match self {
			EvaluationObject::Jrsonnet { evaluation, object } => EvaluationObjectFields::Jrsonnet {
				evaluation,
				fields: object.into_fields(),
			},
		}
	}
}

pub enum EvaluationObjectFields {
	Jrsonnet {
		evaluation: StdRc<JrsonnetEvaluation>,
		fields: rtk_jsonnet_jrsonnet::ObjectFields,
	},
}

impl Iterator for EvaluationObjectFields {
	type Item = (Box<str>, Result<EvaluationValue, Error>);

	fn next(&mut self) -> Option<Self::Item> {
		match self {
			EvaluationObjectFields::Jrsonnet { evaluation, fields } => {
				evaluation.with_context(|_| {
					let (field, value) = fields.next()?;
					let value = value
						.map(|value| EvaluationValue::Jrsonnet {
							evaluation: StdRc::clone(evaluation),
							value,
						})
						.map_err(Error::from);
					Some((field, value))
				})
			}
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
	/// Create an evaluator for `implementation`, if that is one rtk implements
	/// and has already initialized.
	fn create_evaluator(
		&self,
		implementation: &JsonnetImplementation,
	) -> Option<ImplementationEvaluator> {
		match (implementation, &self.jrsonnet) {
			(JsonnetImplementation::Jrsonnet, Some(jrsonnet)) => Some(
				ImplementationEvaluator::Jrsonnet(jrsonnet.create_evaluator()),
			),
			_ => None,
		}
	}

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

	/// How deep evaluation may recurse before it is called a runaway.
	///
	/// Overrides whatever the project's configuration asks for, being the more
	/// direct request of the two. Left unset, the configured depth applies, and
	/// failing that a default matching tk's.
	pub max_stack: Option<usize>,
}

impl Options {
	pub fn apply(&self, evaluator: &mut Evaluator) -> Result<(), Error> {
		let rc = match self.max_stack {
			Some(max_stack) => {
				let mut rc = self.rc.clone();
				rc.spec.max_stack_depth = Some(max_stack);
				rc
			}
			None => self.rc.clone(),
		};

		evaluator.with_rc(rc)?;

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
	fn evaluates_rtk_memoize_without_forcing_a_hit() {
		let evaluation = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(
				r#"{
					first: std.native("rtkMemoize")("engine-test", "first"),
					second: std.native("rtkMemoize")(
						value=error "must not evaluate",
						key="engine-test",
					),
				}"#,
			)
			.unwrap();

		assert_eq!(
			serde_json::to_value(evaluation).unwrap(),
			serde_json::json!({ "first": "first", "second": "first" })
		);
	}

	/// An environment spec, built the way one is really read.
	fn environment_spec(implementation: Option<&str>) -> EnvironmentSpec {
		let spec = match implementation {
			Some(implementation) => {
				format!(r#"{{ "exportJsonnetImplementation": "{implementation}" }}"#)
			}
			None => "{}".to_owned(),
		};

		serde_json::from_str(&spec).expect("a valid environment spec")
	}

	/// Everything about an evaluation that the requested implementation decides.
	const FORMATTING: &str = r#"{
		floats: std.toString(0.6),
		memoizeMissing: std.native("rtkMemoize") == null,
		nativeFunctionsMissing: std.native("sha256") == null,
		yamlDoc: std.manifestYamlDoc({ a: "b" }, false, false),
		yamlStream: std.manifestYamlStream([]),
	}"#;

	fn formatting_of(engine: &Engine, environment: Option<&EnvironmentSpec>) -> serde_json::Value {
		let mut evaluator = engine.create_evaluator_for(environment);
		engine.options().apply(&mut evaluator).unwrap();
		serde_json::to_value(evaluator.evaluate_snippet(FORMATTING).unwrap()).unwrap()
	}

	#[test]
	fn an_environment_can_ask_to_be_formatted_like_the_jrsonnet_binary() {
		let engine = Engine::new(Options::default());

		// rtk cannot hand an environment over to the implementation it asks for,
		// so it imitates it instead: the same output, from this evaluator. That
		// includes dropping Tanka's native functions, which the implementation
		// being imitated does not have — environments are written to notice.
		assert_eq!(
			formatting_of(
				&engine,
				Some(&environment_spec(Some("binary:/usr/local/bin/jrsonnet")))
			),
			serde_json::json!({
				"floats": "0.6",
				"memoizeMissing": true,
				"nativeFunctionsMissing": true,
				"yamlDoc": "a: b",
				"yamlStream": "...\n",
			})
		);

		// An environment that asks for nothing is formatted the way tk formats it,
		// which is go-jsonnet's way.
		assert_eq!(
			formatting_of(&engine, Some(&environment_spec(None))),
			serde_json::json!({
				"floats": "0.59999999999999998",
				"memoizeMissing": false,
				"nativeFunctionsMissing": false,
				"yamlDoc": "a: \"b\"",
				"yamlStream": "---\n\n...\n",
			})
		);
	}

	#[test]
	fn formatting_does_not_outlast_the_environment_that_asked_for_it() {
		let engine = Engine::new(Options::default());

		// Some of how a value is formatted belongs to the thread doing the
		// formatting, and a thread exporting several environments does them one
		// after another. What one environment asked for must not decide how the
		// next one is written out.
		let jrsonnet = environment_spec(Some("binary:/usr/local/bin/jrsonnet"));
		assert_eq!(formatting_of(&engine, Some(&jrsonnet))["floats"], "0.6");

		let plain = environment_spec(None);
		assert_eq!(
			formatting_of(&engine, Some(&plain))["floats"],
			"0.59999999999999998",
			"the previous environment's formatting leaked into this one"
		);
	}

	#[test]
	fn a_projects_configuration_overrides_an_environments_preference() {
		let mut options = Options::default();
		options.rc.spec.jsonnet_implementation = Some(
			serde_json::from_str(
				r#"{ "type": "jrsonnet", "flags": { "outputFormat.floats": "go-jsonnet" } }"#,
			)
			.expect("a valid implementation config"),
		);

		let formatting = formatting_of(
			&Engine::new(options),
			Some(&environment_spec(Some("binary:/usr/local/bin/jrsonnet"))),
		);

		// The project named a format outright, so that is the one used, however
		// the environment would have been formatted otherwise.
		assert_eq!(formatting["floats"], "0.59999999999999998");

		// Only what it named, though. The rest still follows the environment.
		assert_eq!(formatting["yamlDoc"], "a: b");
	}

	#[test]
	fn calls_a_top_level_function_even_with_no_arguments_to_pass_it() {
		// Whether an entrypoint has to be called is a question about the
		// entrypoint, not about whether anything was passed to it: one that takes
		// nothing but defaults still has to be called to get an environment out
		// of it.
		let evaluation = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(r#"function(who = "world") { hello: who }"#)
			.unwrap();

		assert_eq!(
			serde_json::to_value(evaluation).unwrap(),
			serde_json::json!({ "hello": "world" })
		);
	}

	#[test]
	fn recurses_as_deeply_as_tk_allows() {
		fn recurses(max_stack: Option<usize>) -> bool {
			let engine = Engine::new(Options {
				max_stack,
				..Options::default()
			});
			let mut evaluator = engine.create_evaluator();
			engine.options().apply(&mut evaluator).unwrap();

			evaluator
				.evaluate_snippet("local f(n) = if n == 0 then 0 else f(n - 1) + 1; f(300)")
				.is_ok()
		}

		// The evaluator's own default is lower than the one tk gives an
		// environment, and environments are written against tk's.
		assert!(recurses(None), "the default depth is shallower than tk's");

		// Asked for a depth outright, that is the depth, in either direction.
		assert!(!recurses(Some(200)));
		assert!(recurses(Some(500)));
	}

	#[test]
	fn honors_top_level_native_function_disable() {
		let mut options = Options::default();
		options.rc.spec.disable_native_functions = true;
		let engine = Engine::new(options);
		for snippet in [
			r#"std.native("sha256")("foo")"#,
			r#"std.native("rtkMemoize")("disabled", 1)"#,
		] {
			assert!(engine.create_evaluator().evaluate_snippet(snippet).is_err());
		}
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

		let array = object
			.get_or_bail("array", Hidden::Skip)
			.unwrap()
			.into_array()
			.expect("an array");
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
			.get_or_bail("object", Hidden::Skip)
			.unwrap()
			.into_object()
			.expect("an object");
		let object_value: String = nested
			.get_or_bail("value", Hidden::Skip)
			.unwrap()
			.deserialize()
			.unwrap();
		assert_eq!(
			object_value,
			"2958d416d08aa5a472d7b509036cb7eafd542add84527e66a145ea64cb4cdc75"
		);
	}

	#[test]
	fn lists_object_field_names_without_forcing_values() {
		let value = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(r#"{ broken: error "forced", item10: 10, item2: 2, tucked:: 3 }"#)
			.unwrap()
			.into_value();
		let object = value.as_object().expect("an object");

		let names = |hidden| {
			object
				.field_names(hidden)
				.iter()
				.map(ToString::to_string)
				.collect::<Vec<String>>()
		};

		assert_eq!(names(Hidden::Skip), ["broken", "item10", "item2"]);
		assert_eq!(
			names(Hidden::Include),
			["broken", "item10", "item2", "tucked"]
		);
		assert_eq!(
			object
				.get_or_bail("item2", Hidden::Skip)
				.unwrap()
				.as_number(),
			Some(2.0)
		);
	}

	#[test]
	fn captures_environment_data_lazily_and_manifests_it_after_attaching() {
		use rtk_spec::canonical::Environment;

		let value = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(
				r#"{
					apiVersion: "tanka.dev/v1alpha1",
					kind: "Environment",
					metadata: { name: "environments/demo" },
					spec: { namespace: "demo" },
					data: {
						big: 1e100,
						lazy: { nested: 2 + 3 },
					},
				}"#,
			)
			.unwrap()
			.into_value();

		// Deserializing the environment captures `data` without walking it.
		let environment: Environment<'_, RawEvaluationValue> = value.clone().deserialize().unwrap();
		assert_eq!(
			environment.metadata.name.as_deref(),
			Some("environments/demo")
		);

		// Re-attached to its evaluation, the captured value is walkable and
		// manifests the way tk would print it.
		let data = value.attach(environment.data);
		let object = data.into_object().expect("an object");
		assert_eq!(
			object
				.get_or_bail("lazy", Hidden::Skip)
				.unwrap()
				.manifest()
				.unwrap(),
			"{\n    \"nested\": 5\n}"
		);
		assert_eq!(
			serde_json::from_str::<serde_json::Value>(
				&object
					.get_or_bail("big", Hidden::Skip)
					.unwrap()
					.manifest()
					.unwrap()
			)
			.unwrap()
			.as_f64(),
			Some(1e100)
		);
	}
}
