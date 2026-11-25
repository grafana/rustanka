use std::any::Any;
use std::cell::{Cell, RefCell};
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
use jrsonnet_evaluator::{FileImportResolver, IStr, PreparedImportCache, State, Thunk, Val};
use jrsonnet_gcmodule::Trace;
pub use jrsonnet_stdlib::ContextInitializer;
use jrsonnet_stdlib::{
	ManifestYamlDocFormatting, ManifestYamlStreamEmptyBehavior, ManifestYamlStreamFormatting,
	QuoteValuesBehavior,
};
use rtk_jsonnet_core::{EvaluatorError as _, FlagsExt, Function};
use rtk_spec::canonical::{EnvironmentSpec, Rc};
use rustc_hash::{FxBuildHasher, FxHashMap};

mod memoize;
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
	// Last field: release state, native functions and arguments before requesting GC.
	teardown: Option<StdRc<Teardown>>,
}

impl Evaluator {
	/// The configured context, including installed native functions.
	pub fn context_initializer(&self) -> ContextInitializer {
		self.context_initializer.clone()
	}

	/// Run work that invokes installed native functions outside this evaluator's
	/// own evaluation methods.
	pub fn with_current<T>(&self, callback: impl FnOnce() -> T) -> T {
		let _scope = CollectionScope::enter();
		let evaluator = StdRc::new(self.clone());
		Evaluator::CURRENT.with(|current| {
			let _guard = CurrentEvaluatorGuard::new(current, evaluator);
			callback()
		})
	}

	thread_local! {
		static CURRENT: RefCell<Option<StdRc<Evaluator>>> = const { RefCell::new(None) };
		static PREPARED_IMPORTS: PreparedImportCache = PreparedImportCache::default();
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
	fn prepare(&mut self) {
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
		builder.prepared_import_cache(Self::PREPARED_IMPORTS.with(Clone::clone));
		builder.context_initializer(self.context_initializer.clone());

		if !self.import_paths.is_empty() {
			builder.import_resolver(FileImportResolver::new(self.import_paths.clone()));
		}

		self.state = Some(builder.build());
	}

	fn evaluate(
		mut self,
		run: impl FnOnce(&State) -> jrsonnet_evaluator::Result<Val>,
	) -> Result<Evaluation, EvaluatorError> {
		let _scope = CollectionScope::enter();
		self.teardown = Some(StdRc::new(Teardown));
		self.prepare();
		let context = StdRc::new(self);
		Evaluator::CURRENT.with(|current| {
			let _guard = CurrentEvaluatorGuard::new(current, context.clone());
			let state = context.state.as_ref().unwrap();
			let _state_guard = state.enter();
			let value = context.apply_top_level_arguments(run(state)?)?;
			Ok(Evaluation::with_shared_context(context, Value(value)))
		})
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
	type Owner = Evaluation;
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
			teardown: None,
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

	fn with_rtk_memoize(&mut self) -> Result<&mut Self, Self::Error> {
		self.context_initializer
			.add_native("rtkMemoize", memoize::rtkMemoize::default());
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
	fn evaluate_file<P>(self, path: P) -> Result<Evaluation, Self::Error>
	where
		P: AsRef<Path> + fmt::Debug,
	{
		self.evaluate(|state| state.import(path.as_ref()))
	}

	#[tracing::instrument(skip(self))]
	fn evaluate_snippet<S>(self, snippet: S) -> Result<Evaluation, Self::Error>
	where
		S: AsRef<str> + fmt::Debug,
	{
		self.evaluate(|state| state.evaluate_snippet("<anonymous>", snippet.as_ref()))
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

pub struct Evaluation {
	value: Value,
	// External evaluators can install their state separately from `with_current`.
	external_state: Option<State>,
	// Drop after both roots above. Retained children share this context.
	context: StdRc<Evaluator>,
}

impl fmt::Debug for Evaluation {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Evaluation")
			.field("value", &self.value)
			.field("context", &self.context)
			.finish_non_exhaustive()
	}
}

impl Evaluation {
	fn with_shared_context(context: StdRc<Evaluator>, value: Value) -> Self {
		Self {
			value,
			external_state: context
				.state
				.is_none()
				.then(|| jrsonnet_evaluator::with_state(|s| s)),
			context,
		}
	}

	#[cfg(test)]
	fn context(&self) -> &Evaluator {
		&self.context
	}

	fn value(&self) -> &Value {
		&self.value
	}

	/// Run with the state that owns this evaluation. Panics before invoking the
	/// callback if a different jrsonnet state is already active.
	fn with_context<T>(&self, callback: impl FnOnce(&Evaluator) -> T) -> T {
		let _scope = CollectionScope::enter();
		let evaluator = &self.context;
		Evaluator::CURRENT.with(|current| {
			let state = evaluator
				.state
				.as_ref()
				.or(self.external_state.as_ref())
				.expect("a live evaluation has an owner state");
			let _state_guard = state.enter_or_reuse();
			let _guard = CurrentEvaluatorGuard::new(current, StdRc::clone(evaluator));
			callback(evaluator)
		})
	}
}

impl rtk_jsonnet_core::ScopedEvaluator for Evaluator {
	fn visit_root<R>(
		owner: &Self::Owner,
		visit: rtk_jsonnet_core::RootVisitor<
			impl for<'s> FnOnce(rtk_jsonnet_core::ScopedValue<'s, Self>) -> R,
		>,
	) -> R {
		owner.with_context(|context| {
			visit.visit::<Self, _>(context, owner.value().clone(), &|value| {
				Evaluation::with_shared_context(StdRc::clone(&owner.context), value)
			})
		})
	}
	fn deserialize_data<'de, T: ::serde::Deserialize<'de>>(
		_: &Evaluator,
		value: Value,
	) -> Result<T, EvaluatorError> {
		T::deserialize(ValueDeserializer::<false>(value))
	}
}

// The collector rewrites list headers while tracing and then invokes arbitrary
// destructors. Never recursively enter it, or trace halfway through a native
// callback. Coalesce requests, not owners: one pass scans the whole thread.
thread_local! {
	static BUSY: Cell<bool> = const { Cell::new(false) };
	static PENDING: Cell<bool> = const { Cell::new(false) };
}

struct CollectionScope(bool);

impl CollectionScope {
	fn enter() -> Self {
		Self(BUSY.replace(true))
	}
}

impl Drop for CollectionScope {
	fn drop(&mut self) {
		if !self.0 {
			while PENDING.replace(false) {
				let _span =
					tracing::span!(Level::TRACE, "jrsonnet_gcmodule::collect_thread_cycles")
						.entered();
				let _ = jrsonnet_gcmodule::collect_thread_cycles();
			}
			BUSY.set(false);
		}
	}
}

struct Teardown;

impl Drop for Teardown {
	fn drop(&mut self) {
		let _scope = CollectionScope::enter();
		PENDING.set(true);
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
mod reentry_tests {
	use std::panic::{AssertUnwindSafe, catch_unwind};

	use super::{Evaluation, Evaluator, Implementation, State, StdRc, Val, Value};
	use rtk_jsonnet_core::{Evaluator as _, Implementation as _};

	fn evaluator() -> Evaluator {
		Implementation::new(std::iter::empty())
			.unwrap()
			.create_evaluator()
	}

	#[test]
	fn scoped_children_are_lazy_and_use_the_owner() {
		use rtk_jsonnet_core::{Hidden, RawValue, ScopedEvaluator as _};
		let mut ev = evaluator();
		ev.with_external_variable("answer", "yes").unwrap();
		let result = ev
			.evaluate_snippet_result("{a: [std.extVar('answer'), error 'later'], h:: null}")
			.unwrap();
		let answer = result.with_root(|root| {
			assert!(Evaluator::current().is_some());
			let obj = root.as_object().unwrap();
			assert_eq!(obj.field_names(Hidden::Skip).len(), 1);
			assert!(obj.has("h", Hidden::Include).unwrap());
			let arr = obj
				.get_or_bail("a", Hidden::Skip)
				.unwrap()
				.as_array()
				.unwrap();
			assert!(arr.get(2).unwrap().is_none());
			let mut items = arr.iter();
			let answer = items
				.next()
				.unwrap()
				.unwrap()
				.deserialize::<String>()
				.unwrap();
			assert!(items.next().unwrap().is_err());
			answer
		});
		assert_eq!(answer, "yes");
		assert!(Evaluator::current().is_none());
		result.with_root(|root| {
			let arr = root
				.as_object()
				.unwrap()
				.get_or_bail("a", Hidden::Skip)
				.unwrap();
			assert!(
				arr.deserialize::<Vec<RawValue<Value>>>()
					.unwrap_err()
					.to_string()
					.contains("cannot detach")
			);
		});
	}

	#[test]
	fn last_child_releases_its_arena_but_not_a_different_owners() {
		use rtk_jsonnet_core::{Hidden, ScopedEvaluator as _};
		fn owners() -> (
			rtk_jsonnet_core::EvaluationResult<Evaluator>,
			jrsonnet_evaluator::ArenaProbe,
			jrsonnet_evaluator::WeakObjValue,
		) {
			let root = evaluator()
				.evaluate_snippet_result("{ again: self }")
				.unwrap();
			root.with_root(|v| {
				let probe = Evaluator::current()
					.unwrap()
					.state
					.as_ref()
					.unwrap()
					.arena_probe()
					.unwrap();
				let child = v
					.as_object()
					.unwrap()
					.get_or_bail("again", Hidden::Skip)
					.unwrap()
					.into_owned();
				let weak = jrsonnet_evaluator::with_state(|state| {
					let Val::Obj(obj) = state.evaluate_snippet("cycle", "{ again: self }").unwrap()
					else {
						unreachable!()
					};
					obj.get("again".into()).unwrap();
					obj.downgrade()
				});
				(child, probe, weak)
			})
		}
		let (first, probe, weak) = owners();
		// Dropping the parent did not even collect an unrelated orphan cycle.
		assert!(weak.is_alive());
		let (other, other_probe, _) = owners();
		assert_eq!(probe.heap_initialized(), Some(true));
		drop(first);
		assert_eq!(probe.heap_initialized(), None);
		assert!(!weak.is_alive());
		assert_eq!(other_probe.heap_initialized(), Some(true));
		drop(other);
		assert_eq!(other_probe.heap_initialized(), None);
	}

	#[test]
	fn teardown_during_a_callback_or_collector_drop_waits_for_a_safe_point() {
		use jrsonnet_gcmodule::{Cc, Trace};
		use std::cell::RefCell;

		#[derive(Trace)]
		#[trace(tracking(force))]
		struct Cycle {
			edge: RefCell<Option<Cc<Cycle>>>,
			#[trace(skip)] // Opaque native ownership, released by gc_drop_t.
			_owner: Evaluation,
		}
		let outer = evaluator().evaluate_snippet("null").unwrap();
		let make_cycle = || {
			evaluator()
				.evaluate_snippet("local o = {again: self}; o.again")
				.unwrap()
		};
		let first = make_cycle();
		let second = make_cycle();
		let probe = |owner: &Evaluation| {
			owner
				.context()
				.state
				.as_ref()
				.unwrap()
				.arena_probe()
				.unwrap()
		};
		let first_probe = probe(&first);
		let second_probe = probe(&second);
		let cycle = Cc::new(Cycle {
			edge: RefCell::default(),
			_owner: second,
		});
		*cycle.edge.borrow_mut() = Some(cycle.clone());
		drop(cycle);
		outer.with_context(|_| {
			drop(first);
			assert!(first_probe.heap_initialized().is_some());
			assert!(second_probe.heap_initialized().is_some());
		});
		assert_eq!(first_probe.heap_initialized(), None);
		assert_eq!(second_probe.heap_initialized(), None);
	}

	#[test]
	fn traversal_strings_outlive_the_evaluation() {
		use rtk_jsonnet_core::{Hidden, ScopedEvaluator as _};
		let (names, text): (Vec<String>, String) = {
			let result = evaluator()
				.evaluate_snippet_result("{ greeting: 'hello' }")
				.unwrap();
			result.with_root(|root| {
				let obj = root.as_object().unwrap();
				let names = obj.field_names(Hidden::Skip);
				let text = obj
					.get_or_bail(&names[0], Hidden::Skip)
					.unwrap()
					.as_str()
					.unwrap();
				(names, text)
			})
		};
		assert_eq!(names, ["greeting"]);
		assert_eq!(text, "hello");
	}

	#[test]
	fn foreign_evaluation_is_rejected_before_callback() {
		let outer = evaluator().evaluate_snippet("{}").unwrap();
		let inner = evaluator().evaluate_snippet("{}").unwrap();
		outer.with_context(|ctx| {
			let mut ran = false;
			assert!(catch_unwind(AssertUnwindSafe(|| inner.with_context(|_| ran = true))).is_err());
			assert!(!ran);
			assert!(std::ptr::eq(ctx, Evaluator::current().unwrap().as_ref()));
			assert!(ctx.state.as_ref().unwrap().enter_or_reuse().is_none());
		});
		inner.with_context(|_| ());
	}

	#[test]
	fn same_state_nesting_restores_both_scopes_on_unwind() {
		let outer = evaluator().evaluate_snippet("{}").unwrap();
		let inner = Evaluation::with_shared_context(
			StdRc::new(outer.context().clone()),
			outer.value().clone(),
		);
		let mut reached_inner = false;
		outer.with_context(|ctx| {
			assert!(
				catch_unwind(AssertUnwindSafe(|| inner.with_context(|nested| {
					assert!(std::ptr::eq(nested, Evaluator::current().unwrap().as_ref()));
					reached_inner = true;
					panic!("inner callback");
				})))
				.is_err()
			);
			assert!(std::ptr::eq(ctx, Evaluator::current().unwrap().as_ref()));
			assert!(ctx.state.as_ref().unwrap().enter_or_reuse().is_none());
		});
		assert!(reached_inner);
		assert!(
			catch_unwind(AssertUnwindSafe(
				|| outer.with_context(|_| panic!("outer callback"))
			))
			.is_err()
		);
		assert!(Evaluator::current().is_none());
		assert!(
			outer
				.context()
				.state
				.as_ref()
				.unwrap()
				.try_enter()
				.is_some()
		);
	}

	#[test]
	fn captured_external_state_is_reentered() {
		let state = State::builder().build();
		let captured = {
			let _guard = state.enter();
			Evaluation::with_shared_context(StdRc::new(evaluator()), Value(Val::Null))
		};
		captured.with_context(|_| assert!(state.enter_or_reuse().is_none()));
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
			.map_err(|error| error.to_string())?;
		serde_json::to_value(evaluation).map_err(|error| error.to_string())
	}

	#[test]
	fn retained_child_reenters_its_owner_for_a_lazy_native_call() {
		use rtk_jsonnet_core::{Hidden, ScopedEvaluator as _};
		let implementation = super::Implementation::new(std::iter::empty()).unwrap();
		let child = {
			let mut evaluator = implementation.create_evaluator();
			evaluator.with_native_function("add", Add).unwrap();
			let result = evaluator
				.evaluate_snippet_result(
					"{ child: [std.native('add')(2, 3)], unused: error 'not forced' }",
				)
				.unwrap();
			result.with_root(|v| {
				v.as_object()
					.unwrap()
					.get_or_bail("child", Hidden::Skip)
					.unwrap()
					.into_owned()
			})
		};
		assert!(super::Evaluator::current().is_none());
		assert_eq!(child.deserialize::<Vec<i64>>().unwrap(), [5]);
		assert_eq!(serde_json::to_value(child).unwrap(), serde_json::json!([5]));
		assert!(super::Evaluator::current().is_none());
	}

	#[test]
	fn native_error_and_unwind_respect_retained_contexts_and_raw_values() {
		use super::{Evaluator, RefCell, StdRc, Val};
		use rtk_jsonnet_core::EvaluatorError as _;
		use std::panic::{AssertUnwindSafe, catch_unwind};

		#[derive(Debug, Default)]
		struct Capture {
			probe: RefCell<Option<jrsonnet_evaluator::ArenaProbe>>,
			context: RefCell<Option<Evaluator>>,
			value: RefCell<Option<Val>>,
		}
		#[derive(Clone, Copy)]
		enum Retain {
			Nothing,
			Context,
			Value,
		}
		struct Fail(StdRc<Capture>, Retain, bool);
		impl rtk_jsonnet_core::Function<Evaluator> for Fail {
			fn argv(&self) -> (usize, Option<usize>) {
				(1, None)
			}
			fn call(
				&self,
				evaluator: &Evaluator,
				args: super::Arguments,
			) -> Result<super::Value, super::EvaluatorError> {
				let Val::Obj(obj) = args.0[0].as_ref().unwrap().evaluate()? else {
					unreachable!()
				};
				obj.get("again".into())?;
				let probe = evaluator.state.as_ref().unwrap().arena_probe().unwrap();
				assert_eq!(probe.heap_initialized(), Some(true));
				self.0.probe.replace(Some(probe));
				match self.1 {
					Retain::Nothing => (),
					Retain::Context => {
						self.0.context.replace(Some(evaluator.clone()));
					}
					Retain::Value => {
						self.0.value.replace(Some(Val::Obj(obj)));
					}
				}
				if self.2 {
					panic!("native unwind");
				}
				Err(super::EvaluatorError::custom("native error"))
			}
		}
		for retain in [Retain::Nothing, Retain::Context, Retain::Value] {
			for unwind in [false, true] {
				let capture = StdRc::new(Capture::default());
				let mut evaluator = super::Implementation::new(std::iter::empty())
					.unwrap()
					.create_evaluator();
				evaluator
					.with_native_function("fail", Fail(capture.clone(), retain, unwind))
					.unwrap();
				let result = catch_unwind(AssertUnwindSafe(|| {
					evaluator.evaluate_snippet("std.native('fail')({ again: self })")
				}));
				if unwind {
					assert!(result.is_err());
				} else {
					assert!(result.unwrap().is_err());
				}
				assert!(Evaluator::current().is_none());
				let probe = capture.probe.take().unwrap();
				assert_eq!(
					probe.heap_initialized().is_some(),
					!matches!(retain, Retain::Nothing)
				);
				drop(capture.context.take());
				if let Some(Val::Obj(obj)) = capture.value.take() {
					assert!(obj.get("again".into()).unwrap().is_some());
					drop(obj);
					// A raw value has no teardown token. Its now-orphaned self-cycle
					// is eligible for normal collection, not forcibly disconnected.
					let _ = jrsonnet_gcmodule::collect_thread_cycles();
				}
				assert_eq!(probe.heap_initialized(), None);
			}
		}
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
