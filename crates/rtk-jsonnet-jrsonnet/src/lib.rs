use std::any::Any;
use std::cell::RefCell;
use std::convert::Infallible;
use std::error::Error as StdError;
use std::fmt::{self, Formatter, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc as StdRc;
use std::str::FromStr;

use ::serde::{Deserialize, Serialize};
use jrsonnet_evaluator::function::builtin::Builtin;
use jrsonnet_evaluator::function::{
	CallLocation, FunctionSignature, ParamDefault, ParamName, ParamParse,
};
use jrsonnet_evaluator::manifest::set_use_go_style_floats;
use jrsonnet_evaluator::stack::set_stack_depth_limit;
use jrsonnet_evaluator::tla::TlaArg;
use jrsonnet_evaluator::trace::PathResolver;
use jrsonnet_evaluator::{FileImportResolver, IStr, State, Thunk, Val};
use jrsonnet_gcmodule::Trace;
pub use jrsonnet_stdlib::ContextInitializer;
use jrsonnet_stdlib::{
	ManifestYamlDocFormatting, ManifestYamlStreamEmptyBehavior, ManifestYamlStreamFormatting,
	QuoteValuesBehavior,
};
use rtk_jsonnet_core::{EvaluatorError as _, FlagsExt, Function};
use rtk_spec::canonical::{EnvironmentSpec, Rc};
use rustc_hash::{FxBuildHasher, FxHashMap};

mod native;
mod serde;

pub use native::{Arguments, Array, ArrayValues, Object, ObjectFields, ObjectValues, Str, Value};
use thiserror::Error;
use tracing::Level;

pub use crate::serde::{ValueDeserializer, ValueSerializer};

#[derive(Clone, Debug, Default)]
struct Config {
	/// Output format configuration for the bundled Jrsonnet implementation.
	output_format: OutputFormatConfig,
}

/// The stack depth rtk allows by default.
///
/// jrsonnet's own default is lower; this is the limit tk uses, which is what an
/// environment written against tk expects to have.
const DEFAULT_MAX_STACK: usize = 500;

impl Config {
	fn merge_from_flags(&mut self, flags: impl Iterator<Item = Flag>) {
		for flag in flags {
			match flag {
				Flag::OutputFormatFloats(output_format) => {
					self.output_format.floats = Some(output_format);
				}
				Flag::OutputFormatStdManifestYamlDoc(output_format) => {
					self.output_format.std_manifest_yaml_doc = Some(output_format);
				}
				Flag::OutputFormatStdManifestYamlStream(output_format) => {
					self.output_format.std_manifest_yaml_stream = Some(output_format);
				}
			}
		}
	}
}

/// Output format configuration for the bundled Jrsonnet implementation.
///
/// Use "jrsonnet" values for environments that use tk with exportJsonnetImplementation
/// pointing to a jrsonnet binary, to match the output format.
#[derive(Clone, Debug, Default)]
struct OutputFormatConfig {
	/// Controls float formatting in std.toString and related functions.
	///
	/// - "go-jsonnet" (default): Use Go's %.17g format (e.g., 0.59999999999999998)
	/// - "jrsonnet": Use shortest representation (e.g., 0.6)
	floats: Option<OutputFormat>,

	/// Controls the output format for std.manifestYamlDoc.
	///
	/// - "go-jsonnet" (default): values are always quoted, regardless of quote_keys setting
	/// - "jrsonnet": quote_values follows quote_keys (when quote_keys=false, quote_values=false)
	std_manifest_yaml_doc: Option<OutputFormat>,

	/// Controls the output format for std.manifestYamlStream with empty arrays.
	///
	/// - "go-jsonnet" (default): Empty arrays produce "---\n\n" (document marker + empty line)
	/// - "jrsonnet": Empty arrays produce "\n" (just a newline)
	std_manifest_yaml_stream: Option<OutputFormat>,
}

impl OutputFormatConfig {
	/// Use `output_format` for anything not configured explicitly.
	///
	/// An environment asking for another implementation only says what it would
	/// like by default: a flag naming a format outright, wherever it came from,
	/// has already been recorded here and is left alone.
	fn default_to(&mut self, output_format: OutputFormat) {
		self.floats.get_or_insert(output_format);
		self.std_manifest_yaml_doc.get_or_insert(output_format);
		self.std_manifest_yaml_stream.get_or_insert(output_format);
	}
}

/// Whatever this implementation could not do.
///
/// Both variants render as the error underneath them: a reader wants the reason,
/// and being told an error occurred on the way to it is not one.
#[derive(Clone, Debug, Error)]
pub enum Error {
	#[error(transparent)]
	Evaluator(#[from] EvaluatorError),
	#[error(transparent)]
	Flag(#[from] FlagError),
}

impl From<Infallible> for Error {
	fn from(_: Infallible) -> Self {
		unreachable!()
	}
}

#[derive(Clone)]
pub struct Evaluator {
	config: Config,
	context_initializer: ContextInitializer,
	import_paths: Vec<PathBuf>,
	max_stack: usize,
	state: Option<State>,
	top_level_arguments: FxHashMap<IStr, TlaArg>,
}

impl Evaluator {
	/// The configured context, including installed native functions.
	pub fn context_initializer(&self) -> ContextInitializer {
		self.context_initializer.clone()
	}

	/// Run work that invokes installed native functions outside this evaluator's
	/// own evaluation methods.
	pub fn with_current<T>(&self, callback: impl FnOnce() -> T) -> T {
		let evaluator = StdRc::new(self.clone());
		Evaluator::CURRENT.with(|current| {
			let _guard = CurrentEvaluatorGuard::new(current, evaluator);
			callback()
		})
	}

	thread_local! {
		static CURRENT: RefCell<Option<StdRc<Evaluator>>> = const { RefCell::new(None) };
	}

	/// The evaluator whose context is in effect on this thread, if any.
	///
	/// Set for the duration of [`Evaluation::with_context`], which is when
	/// values are deserialized, so anything that captures a value out of an
	/// evaluation can capture what keeps it usable at the same time. The handle
	/// is shared rather than copied, so the captured context is the very same
	/// one, not a duplicate of it.
	pub fn current() -> Option<StdRc<Evaluator>> {
		Evaluator::CURRENT.with(|current| current.borrow().clone())
	}

	/// Build the state to evaluate in, applying everything configured.
	///
	/// Part of what an evaluation is configured with belongs to the thread that
	/// runs it rather than to its state, so this has to run on that thread,
	/// immediately before evaluating.
	fn prepare(&mut self) -> State {
		let output_format = &self.config.output_format;

		self.context_initializer
			.set_manifest_yaml_doc_formatting(ManifestYamlDocFormatting {
				quote_values_behavior: match output_format.std_manifest_yaml_doc {
					Some(OutputFormat::Jrsonnet) => QuoteValuesBehavior::Jrsonnet,
					Some(OutputFormat::GoJsonnet) | None => QuoteValuesBehavior::GoJsonnet,
				},
			});

		self.context_initializer
			.set_manifest_yaml_stream_formatting(ManifestYamlStreamFormatting {
				empty_behavior: match output_format.std_manifest_yaml_stream {
					Some(OutputFormat::Jrsonnet) => ManifestYamlStreamEmptyBehavior::Jrsonnet,
					Some(OutputFormat::GoJsonnet) | None => {
						ManifestYamlStreamEmptyBehavior::GoJsonnet
					}
				},
			});

		// These two are per-thread rather than per-state, and a thread goes on to
		// evaluate further environments. Both are therefore set on every
		// evaluation, to whatever it wants, rather than only when it wants
		// something unusual: leaving them alone would let one environment decide
		// how the next one to land on the same thread is formatted.
		set_use_go_style_floats(!matches!(
			output_format.floats,
			Some(OutputFormat::Jrsonnet)
		));
		set_stack_depth_limit(self.max_stack);

		let mut builder = State::builder();
		builder.context_initializer(self.context_initializer.clone());

		if !self.import_paths.is_empty() {
			builder.import_resolver(FileImportResolver::new(self.import_paths.clone()));
		}

		let state = builder.build();
		self.state = Some(state.clone());
		state
	}

	/// Apply top level arguments to what an evaluation produced.
	///
	/// Always attempted, never only when arguments were passed: whether to call
	/// the result is a question about the result — an entrypoint that is a
	/// function of nothing but defaults still has to be called — and
	/// [`apply_tla`](jrsonnet_evaluator::apply_tla) leaves anything that is not a
	/// function alone.
	fn apply_top_level_arguments(&self, value: Val) -> Result<Val, EvaluatorError> {
		jrsonnet_evaluator::apply_tla(&self.top_level_arguments, value).map_err(EvaluatorError)
	}
}

struct CurrentEvaluatorGuard<'a> {
	current: &'a RefCell<Option<StdRc<Evaluator>>>,
	previous: Option<StdRc<Evaluator>>,
}

impl<'a> CurrentEvaluatorGuard<'a> {
	fn new(current: &'a RefCell<Option<StdRc<Evaluator>>>, evaluator: StdRc<Evaluator>) -> Self {
		Self {
			current,
			previous: current.replace(Some(evaluator)),
		}
	}
}

impl Drop for CurrentEvaluatorGuard<'_> {
	fn drop(&mut self) {
		self.current.replace(self.previous.take());
	}
}

impl fmt::Debug for Evaluator {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Evaluator")
			.field("config", &self.config)
			.finish_non_exhaustive()
	}
}

impl rtk_jsonnet_core::Context for Evaluator {
	type Evaluator = Self;
}

impl rtk_jsonnet_core::Evaluator for Evaluator {
	type Implementation = Implementation;

	type Arguments = Arguments;
	type Context = Self;
	type Error = EvaluatorError;
	type Value = Value;

	fn new(implementation: &Self::Implementation) -> Self {
		Evaluator {
			config: implementation.config.clone(),
			context_initializer: ContextInitializer::new(PathResolver::Absolute),
			import_paths: Vec::new(),
			max_stack: DEFAULT_MAX_STACK,
			state: None,
			top_level_arguments: FxHashMap::with_hasher(FxBuildHasher),
		}
	}

	fn with_rc(&mut self, rc: Rc) -> Result<&mut Self, Self::Error> {
		if let Some(max_stack) = rc.spec.max_stack_depth {
			self.max_stack = max_stack;
		}

		self.config
			.merge_from_flags(rc.flags().map_err(EvaluatorError::custom)?);
		Ok(self)
	}

	fn with_environment(
		&mut self,
		environment: &EnvironmentSpec,
	) -> Result<&mut Self, Self::Error> {
		// Imitating the jrsonnet binary is the whole of what this evaluator can
		// do about an environment naming another implementation: it cannot hand
		// over to one, but it can format its output the same way.
		if environment.emulates_jrsonnet() {
			self.config.output_format.default_to(OutputFormat::Jrsonnet);
		}

		self.config
			.merge_from_flags(environment.flags().map_err(EvaluatorError::custom)?);
		Ok(self)
	}

	fn with_import_paths(&mut self, import_paths: Vec<PathBuf>) -> Result<&mut Self, Self::Error> {
		self.import_paths = import_paths;
		Ok(self)
	}

	fn with_plugin<P>(&mut self, plugin: P) -> Result<&mut Self, Self::Error>
	where
		P: rtk_jsonnet_core::Plugin<Self>,
	{
		plugin.install(self)?;
		Ok(self)
	}

	fn with_external_code(&mut self, key: &str, value: &str) -> Result<&mut Self, Self::Error> {
		self.context_initializer
			.add_ext_code(key, value)
			.map_err(EvaluatorError)?;
		Ok(self)
	}

	fn with_external_variable(&mut self, key: &str, value: &str) -> Result<&mut Self, Self::Error> {
		self.context_initializer
			.add_ext_str(key.into(), value.into());
		Ok(self)
	}

	fn with_native_function<F>(&mut self, key: &str, func: F) -> Result<&mut Self, Self::Error>
	where
		F: 'static + rtk_jsonnet_core::Function<Self>,
	{
		#[derive(Debug, Trace)]
		struct FunctionWrapper<F: 'static> {
			signature: FunctionSignature,
			#[trace(skip)]
			name: Box<str>,
			#[trace(skip)]
			func: F,
		}

		// TODO: This sucks. But, like, Is there really a better way to have the
		// neccesary wrappers.
		impl<F> Builtin for FunctionWrapper<F>
		where
			F: 'static + Function<Evaluator>,
		{
			fn name(&self) -> &str {
				&self.name
			}
			fn params(&self) -> FunctionSignature {
				self.signature.clone()
			}
			fn as_any(&self) -> &dyn Any {
				self
			}

			fn call(
				&self,
				_: CallLocation<'_>,
				args: &[Option<Thunk<Val>>],
			) -> Result<Val, jrsonnet_evaluator::Error> {
				let args = Arguments(args.to_vec().into_boxed_slice());
				Evaluator::CURRENT.with(move |current| {
					let evaluator = current.borrow().as_ref().cloned();
					if let Some(evaluator) = evaluator {
						match self.func.call(&evaluator, args) {
							Ok(value) => Ok(value.0),
							Err(error) => Err(error.0),
						}
					} else {
						panic!("no evaluator present");
					}
				})
			}
		}

		self.context_initializer.add_native(
			key,
			FunctionWrapper {
				signature: FunctionSignature::new({
					let (argv, argd) = func.argv();
					let argd = argv - argd.unwrap_or_default();
					let parameter_names = func.parameter_names();
					if let Some(parameter_names) = parameter_names {
						assert_eq!(parameter_names.len(), argv);
					}

					let mut params = Vec::with_capacity(argv);

					for i in 0..argv {
						params.push(ParamParse::new(
							parameter_names.map_or(ParamName::Unnamed, |names| {
								ParamName::Named(names[i].into())
							}),
							if i >= argd {
								ParamDefault::Exists
							} else {
								ParamDefault::None
							},
						));
					}

					params.into()
				}),
				name: key.into(),
				func,
			},
		);

		Ok(self)
	}

	fn with_top_level_argument(
		&mut self,
		key: &str,
		value: &str,
	) -> Result<&mut Self, Self::Error> {
		self.top_level_arguments
			.insert(key.into(), TlaArg::String(value.into()));
		Ok(self)
	}

	fn with_top_level_code(&mut self, key: &str, value: &str) -> Result<&mut Self, Self::Error> {
		self.top_level_arguments
			.insert(key.into(), TlaArg::InlineCode(value.to_owned()));
		Ok(self)
	}

	#[tracing::instrument(skip(self))]
	fn evaluate_file<P>(mut self, path: P) -> Result<(Self::Context, Self::Value), Self::Error>
	where
		P: AsRef<Path> + fmt::Debug,
	{
		let state = self.prepare();

		let current_evaluator = StdRc::new(self);
		Evaluator::CURRENT.with(move |evaluator| {
			let _guard = CurrentEvaluatorGuard::new(evaluator, StdRc::clone(&current_evaluator));
			let _state_guard = state.enter();

			let value = state
				.import(path.as_ref())
				.map_err(EvaluatorError)
				.and_then(|value| current_evaluator.apply_top_level_arguments(value))?;

			Ok(((*current_evaluator).clone(), Value(value)))
		})
	}

	#[tracing::instrument(skip(self))]
	fn evaluate_snippet<S>(
		mut self,
		snippet: S,
	) -> Result<(Self::Context, Self::Value), Self::Error>
	where
		S: AsRef<str> + fmt::Debug,
	{
		let state = self.prepare();

		let current_evaluator = StdRc::new(self);
		Evaluator::CURRENT.with(move |evaluator| {
			let _guard = CurrentEvaluatorGuard::new(evaluator, StdRc::clone(&current_evaluator));
			let _state_guard = state.enter();

			let value = state
				.evaluate_snippet("<anonymous>", snippet.as_ref())
				.map_err(EvaluatorError)
				.and_then(|value| current_evaluator.apply_top_level_arguments(value))?;

			Ok(((*current_evaluator).clone(), Value(value)))
		})
	}
}

/// An error jrsonnet itself raised, which already reads as one.
#[derive(Clone, Debug, Error)]
#[error(transparent)]
pub struct EvaluatorError(#[from] jrsonnet_evaluator::Error);

impl rtk_jsonnet_core::EvaluatorError for EvaluatorError {
	type Evaluator = Evaluator;

	#[inline]
	fn custom<T>(message: T) -> Self
	where
		T: fmt::Display,
	{
		let error_kind =
			jrsonnet_evaluator::error::ErrorKind::RuntimeError(message.to_string().into());
		EvaluatorError(jrsonnet_evaluator::Error::new(error_kind))
	}
}

#[derive(Debug)]
pub struct Evaluation(pub(crate) Option<Value>, pub(crate) StdRc<Evaluator>);

impl Evaluation {
	pub fn new(context: Evaluator, value: Value) -> Self {
		Self(Some(value), StdRc::new(context))
	}

	/// Pair a value with a context that is already shared, as
	/// [`Evaluator::current`] hands it out.
	pub fn with_shared_context(context: StdRc<Evaluator>, value: Value) -> Self {
		Self(Some(value), context)
	}

	pub fn context(&self) -> &Evaluator {
		&self.1
	}

	pub fn value(&self) -> &Value {
		self.0
			.as_ref()
			.expect("the evaluation is only empty while being dropped")
	}

	pub fn with_context<T>(&self, callback: impl FnOnce(&Evaluator) -> T) -> T {
		Evaluator::CURRENT.with(|current| {
			let _guard = CurrentEvaluatorGuard::new(current, StdRc::clone(&self.1));
			let _state_guard = self.1.state.as_ref().and_then(State::try_enter);
			callback(&self.1)
		})
	}
}

impl From<(Evaluator, Value)> for Evaluation {
	fn from((context, value): (Evaluator, Value)) -> Self {
		Self::new(context, value)
	}
}

impl Drop for Evaluation {
	fn drop(&mut self) {
		self.0 = None;
		{
			let span = tracing::span!(Level::TRACE, "jrsonnet_gcmodule::collect_thread_cycles");
			let _entered = span.enter();
			let _ = jrsonnet_gcmodule::collect_thread_cycles();
		}
	}
}

#[derive(Clone, Debug)]
pub enum Flag {
	OutputFormatFloats(OutputFormat),
	OutputFormatStdManifestYamlDoc(OutputFormat),
	OutputFormatStdManifestYamlStream(OutputFormat),
}

impl rtk_jsonnet_core::Flag for Flag {
	type Implementation = Implementation;

	type Error = FlagError;
	type Key = FlagKey;
	type Value = FlagValue;

	fn new(key: Self::Key, value: Self::Value) -> Result<Self, Self::Error> {
		let FlagValue::OutputFormat(output_format) = value;
		match key {
			FlagKey::OutputFormatFloats => Ok(Flag::OutputFormatFloats(output_format)),
			FlagKey::OutputFormatStdManifestYamlDoc => {
				Ok(Flag::OutputFormatStdManifestYamlDoc(output_format))
			}
			FlagKey::OutputFormatStdManifestYamlStream => {
				Ok(Flag::OutputFormatStdManifestYamlStream(output_format))
			}
		}
	}
}

#[derive(Clone, Copy, Debug, Error)]
pub enum FlagError {
	#[error("invalid flag key: {0}")]
	InvalidFlagKey(#[from] FlagKeyFromStrError),
	#[error("invalid flag value: {0}")]
	InvalidFlagValue(#[from] FlagValueFromStrError),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FlagKey {
	#[serde(rename = "outputFormat.floats")]
	OutputFormatFloats,
	#[serde(rename = "outputFormat.std.manifestYamlDoc")]
	OutputFormatStdManifestYamlDoc,
	#[serde(rename = "outputFormat.std.manifestYamlStream")]
	OutputFormatStdManifestYamlStream,
}

impl FlagKey {
	const ALL: &'static [FlagKey] = &[
		FlagKey::OutputFormatFloats,
		FlagKey::OutputFormatStdManifestYamlDoc,
		FlagKey::OutputFormatStdManifestYamlStream,
	];
}

impl fmt::Display for FlagKey {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self {
			FlagKey::OutputFormatFloats => formatter.write_str("outputFormat.floats"),
			FlagKey::OutputFormatStdManifestYamlDoc => {
				formatter.write_str("outputFormat.std.manifestYamlDoc")
			}
			FlagKey::OutputFormatStdManifestYamlStream => {
				formatter.write_str("outputFormat.std.manifestYamlStream")
			}
		}
	}
}

impl FromStr for FlagKey {
	type Err = FlagKeyFromStrError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"outputFormat.floats" => Ok(FlagKey::OutputFormatFloats),
			"outputFormat.std.manifestYamlDoc" => Ok(FlagKey::OutputFormatStdManifestYamlDoc),
			"outputFormat.std.manifestYamlStream" => Ok(FlagKey::OutputFormatStdManifestYamlStream),
			_ => Err(FlagKeyFromStrError),
		}
	}
}

/// The error returned by `<FlagKey as FromStr>::from_str`.
#[derive(Clone, Copy, Debug)]
pub struct FlagKeyFromStrError;

impl fmt::Display for FlagKeyFromStrError {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str("invalid flag key specified; valid forms are ")?;
		for (i, valid) in FlagKey::ALL.iter().copied().enumerate() {
			if i != 0 {
				formatter.write_char(',')?;
				formatter.write_char(' ')?;
				if i == FlagKey::ALL.len() - 1 {
					formatter.write_str("and ")?;
				}
			}
			write!(formatter, "\"{valid}\"")?;
		}
		Ok(())
	}
}

impl StdError for FlagKeyFromStrError {}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum FlagValue {
	OutputFormat(OutputFormat),
}

impl fmt::Display for FlagValue {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self {
			FlagValue::OutputFormat(output_format) => {
				<OutputFormat as fmt::Display>::fmt(output_format, formatter)
			}
		}
	}
}

impl FromStr for FlagValue {
	type Err = FlagValueFromStrError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		if let Ok(output_format) = s.parse::<OutputFormat>() {
			return Ok(FlagValue::OutputFormat(output_format));
		}
		Err(FlagValueFromStrError)
	}
}

/// The error returned by `<FlagValue as FromStr>::from_str`.
#[derive(Clone, Copy, Debug, Error)]
#[error("invalid flag value specified")]
pub struct FlagValueFromStrError;

#[derive(Clone, Debug)]
pub struct Implementation {
	config: Config,
}

impl rtk_jsonnet_core::Implementation for Implementation {
	type Evaluator = Evaluator;
	type Flag = Flag;
	type Error = Error;
	type InitializationError = Infallible;

	fn new(flags: impl Iterator<Item = Self::Flag>) -> Result<Self, Self::InitializationError> {
		let mut implementation = Implementation {
			config: Config::default(),
		};
		implementation.config.merge_from_flags(flags);
		Ok(implementation)
	}
}
/// Specifies which jsonnet implementation's behavior to match
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
	/// Match go-jsonnet behavior (default)
	#[default]
	GoJsonnet,
	/// Match jrsonnet binary behavior
	Jrsonnet,
}

impl fmt::Display for OutputFormat {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self {
			OutputFormat::GoJsonnet => formatter.write_str("go-jsonnet"),
			OutputFormat::Jrsonnet => formatter.write_str("jrsonnet"),
		}
	}
}

impl FromStr for OutputFormat {
	type Err = OutputFormatFromStrError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"go-jsonnet" => Ok(OutputFormat::GoJsonnet),
			"jrsonnet" => Ok(OutputFormat::Jrsonnet),
			_ => Err(OutputFormatFromStrError),
		}
	}
}

#[cfg(test)]
mod native_function_tests {
	use rtk_jsonnet_core::{Context as _, Evaluator as _, Implementation as _};
	use serde::{Deserialize, Serialize};

	#[derive(Debug)]
	struct Add;

	impl rtk_jsonnet_core::Function<super::Evaluator> for Add {
		fn argv(&self) -> (usize, Option<usize>) {
			(2, None)
		}

		fn parameter_names(&self) -> Option<&'static [&'static str]> {
			Some(&["left", "right"])
		}

		fn call(
			&self,
			evaluator: &super::Evaluator,
			arguments: super::Arguments,
		) -> Result<super::Value, super::EvaluatorError> {
			let (left, right) = <(i64, i64)>::deserialize(arguments)?;
			Ok((left + right).serialize(evaluator.create_serializer())?)
		}
	}

	fn evaluate(snippet: &str) -> Result<serde_json::Value, String> {
		let implementation = super::Implementation::new(std::iter::empty()).unwrap();
		let mut evaluator = implementation.create_evaluator();
		evaluator.with_native_function("add", Add).unwrap();
		let evaluation = evaluator
			.evaluate_snippet(snippet)
			.map(super::Evaluation::from)
			.map_err(|error| error.to_string())?;
		serde_json::to_value(evaluation).map_err(|error| error.to_string())
	}

	#[test]
	fn invokes_native_with_exact_positional_arity() {
		assert_eq!(evaluate("std.native('add')(2, 3)").unwrap(), 5);
		assert!(evaluate("std.native('add')(2, 3, 4)").is_err());
	}

	#[test]
	fn invokes_native_with_named_arguments() {
		assert_eq!(evaluate("std.native('add')(right=3, left=2)").unwrap(), 5);
	}
}

/// The error returned by `<OutputFormat as FromStr>::from_str`.
#[derive(Clone, Copy, Debug, Error)]
#[error("invalid output format specified; valid forms are \"go-jsonnet\" and \"jrsonnet\"")]
pub struct OutputFormatFromStrError;
