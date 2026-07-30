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
use jrsonnet_evaluator::tla::TlaArg;
use jrsonnet_evaluator::trace::PathResolver;
use jrsonnet_evaluator::{FileImportResolver, IStr, State, Thunk, Val};
use jrsonnet_gcmodule::Trace;
use jrsonnet_stdlib::ContextInitializer;
use rtk_jsonnet_core::{FlagsExt, Function};
use rtk_spec::canonical::Rc;
use rustc_hash::{FxBuildHasher, FxHashMap};

mod native;
mod serde;

pub use native::{Arguments, Value};
use thiserror::Error;
use tracing::Level;

pub use crate::serde::ValueSerializer;

#[derive(Clone, Debug, Default)]
struct Config {
	/// Output format configuration for the bundled Jrsonnet implementation.
	output_format: OutputFormatConfig,
}

impl Config {
	fn merge_from_flags(&mut self, flags: impl Iterator<Item = Flag>) {
		for flag in flags {
			match flag {
				Flag::OutputFormatFloats(output_format) => {
					self.output_format.floats = Some(output_format)
				}
				Flag::OutputFormatStdManifestYamlDoc(output_format) => {
					self.output_format.std_manifest_yaml_doc = Some(output_format)
				}
				Flag::OutputFormatStdManifestYamlStream(output_format) => {
					self.output_format.std_manifest_yaml_stream = Some(output_format)
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

#[derive(Clone, Debug, Error)]
pub enum Error {
	#[error("an evaluator error occurred")]
	Evaluator(#[from] EvaluatorError),
	#[error("a flag error occurred")]
	Flag(#[from] FlagError),
}

impl From<Infallible> for Error {
	fn from(_: Infallible) -> Self {
		unreachable!()
	}
}

pub struct Evaluator {
	config: Config,
	context_initializer: ContextInitializer,
	import_resolver: Option<FileImportResolver>,
	state: Option<State>,
	top_level_arguments: FxHashMap<IStr, TlaArg>,
}

impl Evaluator {
	thread_local! {
		static CURRENT: RefCell<Option<StdRc<Evaluator>>> = const { RefCell::new(None) };
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

impl<'a> rtk_jsonnet_core::Evaluator<'a> for Evaluator {
	type Implementation = Implementation;

	type Arguments<'b> = Arguments<'b>;
	type Error = EvaluatorError;
	type Evaluation = Evaluation;
	type Value = Value;

	fn new(implementation: &Self::Implementation) -> Self {
		Evaluator {
			config: implementation.config.clone(),
			context_initializer: ContextInitializer::new(PathResolver::Absolute),
			import_resolver: None,
			state: None,
			top_level_arguments: FxHashMap::with_hasher(FxBuildHasher::default()),
		}
	}

	fn name() -> &'static str {
		"jrsonnet"
	}

	fn with_rc(
		&mut self,
		rc: &'a Rc,
	) -> Result<&mut Self, <Self::Implementation as rtk_jsonnet_core::Implementation>::Error> {
		self.config.merge_from_flags(rc.flags()?);
		Ok(self)
	}

	fn with_import_paths(
		&mut self,
		import_paths: Vec<PathBuf>,
	) -> Result<&mut Self, <Self::Implementation as rtk_jsonnet_core::Implementation>::Error> {
		self.import_resolver = Some(FileImportResolver::new(import_paths));
		Ok(self)
	}

	fn with_plugin<P>(
		&mut self,
		plugin: P,
	) -> Result<&mut Self, <Self::Implementation as rtk_jsonnet_core::Implementation>::Error>
	where
		P: rtk_jsonnet_core::Plugin<'a, Self>,
	{
		plugin.install(self)?;
		Ok(self)
	}

	fn with_external_code(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, <Self::Implementation as rtk_jsonnet_core::Implementation>::Error> {
		self.context_initializer
			.add_ext_code(key, value)
			.map_err(EvaluatorError)?;
		Ok(self)
	}

	fn with_external_variable(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, <Self::Implementation as rtk_jsonnet_core::Implementation>::Error> {
		self.context_initializer
			.add_ext_str(key.into(), value.into());
		Ok(self)
	}

	fn with_native_function<F>(
		&mut self,
		key: &'a str,
		func: F,
	) -> Result<&mut Self, <Self::Implementation as rtk_jsonnet_core::Implementation>::Error>
	where
		F: 'static + rtk_jsonnet_core::Function<'a, Self>,
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
		impl<'a, F> Builtin for FunctionWrapper<F>
		where
			F: 'static + Function<'a, Evaluator>,
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
				let args = Arguments(args);
				Evaluator::CURRENT.with(move |evaluator| {
					if let Some(evaluator) = evaluator.borrow().as_ref() {
						match self.func.call(evaluator, args) {
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
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, <Self::Implementation as rtk_jsonnet_core::Implementation>::Error> {
		self.top_level_arguments
			.insert(key.into(), TlaArg::String(value.into()));
		Ok(self)
	}

	fn with_top_level_code(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, <Self::Implementation as rtk_jsonnet_core::Implementation>::Error> {
		self.top_level_arguments
			.insert(key.into(), TlaArg::InlineCode(value.to_owned()));
		Ok(self)
	}

	#[tracing::instrument(skip(self))]
	fn evaluate_file<P>(
		mut self,
		path: P,
	) -> Result<Self::Evaluation, <Self as rtk_jsonnet_core::Evaluator<'a>>::Error>
	where
		P: AsRef<Path> + fmt::Debug,
	{
		let state = {
			let mut state_builder = State::builder();

			state_builder.context_initializer(self.context_initializer.clone());

			if let Some(import_resolver) = self.import_resolver.take() {
				state_builder.import_resolver(import_resolver);
			}

			state_builder.build()
		};
		self.state = Some(state.clone());

		let current_evaluator = StdRc::new(self);
		Evaluator::CURRENT.with(move |evaluator| {
			let _guard = CurrentEvaluatorGuard::new(evaluator, StdRc::clone(&current_evaluator));

			let result = (|| {
				let val = state.import(path.as_ref())?;

				let val = if let Some(evaluator) = evaluator.borrow().as_ref() {
					if !evaluator.top_level_arguments.is_empty() {
						jrsonnet_evaluator::apply_tla(&evaluator.top_level_arguments, val)?
					} else {
						val
					}
				} else {
					val
				};

				Ok(val)
			})();

			result.map(|value| Evaluation(Some(value), current_evaluator))
		})
	}

	#[tracing::instrument(skip(self))]
	fn evaluate_snippet<S>(
		mut self,
		snippet: S,
	) -> Result<Self::Evaluation, <Self as rtk_jsonnet_core::Evaluator<'a>>::Error>
	where
		S: AsRef<str> + fmt::Debug,
	{
		let state = {
			let mut state_builder = State::builder();

			state_builder.context_initializer(self.context_initializer.clone());

			if let Some(import_resolver) = self.import_resolver.take() {
				state_builder.import_resolver(import_resolver);
			}

			state_builder.build()
		};
		self.state = Some(state.clone());

		let current_evaluator = StdRc::new(self);
		Evaluator::CURRENT.with(move |evaluator| {
			let _guard = CurrentEvaluatorGuard::new(evaluator, StdRc::clone(&current_evaluator));

			let result = (|| {
				let val = state.evaluate_snippet("<anonymous>", snippet.as_ref())?;

				let val = if let Some(evaluator) = evaluator.borrow().as_ref() {
					if !evaluator.top_level_arguments.is_empty() {
						jrsonnet_evaluator::apply_tla(&evaluator.top_level_arguments, val)?
					} else {
						val
					}
				} else {
					val
				};

				Ok(val)
			})();

			result.map(|value| Evaluation(Some(value), current_evaluator))
		})
	}
}

#[derive(Clone, Debug, Error)]
#[error("an evaluator error occurred")]
pub struct EvaluatorError(#[from] jrsonnet_evaluator::Error);

impl<'a> rtk_jsonnet_core::EvaluatorError<'a> for EvaluatorError {
	type Evaluator = Evaluator;

	#[inline]
	fn custom<T>(message: T) -> Self
	where
		T: Into<String>,
	{
		let error_kind = jrsonnet_evaluator::error::ErrorKind::RuntimeError(message.into().into());
		EvaluatorError(jrsonnet_evaluator::Error::new(error_kind))
	}
}

#[derive(Debug)]
pub struct Evaluation(
	pub(crate) Option<jrsonnet_evaluator::Val>,
	pub(crate) StdRc<Evaluator>,
);

impl<'a> rtk_jsonnet_core::Evaluation<'a> for Evaluation {}

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
	type Evaluator<'a> = Evaluator;
	type Flag = Flag;
	type Error = Error;
	type InitializationError = Infallible;

	fn new<'a>(flags: impl Iterator<Item = Self::Flag>) -> Result<Self, Self::InitializationError> {
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
	use rtk_jsonnet_core::{Evaluator as _, Implementation as _};
	use serde::{Deserialize, Serialize};

	#[derive(Debug)]
	struct Add;

	impl<'a> rtk_jsonnet_core::Function<'a, super::Evaluator> for Add {
		fn argv(&self) -> (usize, Option<usize>) {
			(2, None)
		}

		fn parameter_names(&self) -> Option<&'static [&'static str]> {
			Some(&["left", "right"])
		}

		fn call<'b>(
			&self,
			evaluator: &super::Evaluator,
			arguments: super::Arguments<'b>,
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
