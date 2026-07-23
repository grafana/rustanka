use std::any::Any;
use std::cell::RefCell;
use std::convert::Infallible;
use std::error::Error as StdError;
use std::fmt::{self, Formatter, Write};
use std::path::{Path, PathBuf};
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
use rtk_spec::canonical::{Environment, Rc};
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

	/// When true, disables Tanka-specific native functions (manifestYamlFromJson,
	/// parseYaml, parseJson, etc.). This is useful when tk uses jrsonnet binary
	/// via exportJsonnetImplementation, where these native functions are not available
	/// and the jsonnet code falls back to std.manifestYamlDoc.
	disable_native_functions: bool,
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
				Flag::DisableNativeFunctions(disable_native_functions) => {
					self.disable_native_functions = disable_native_functions
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
	#[error("a flag-parsing error occurred")]
	FlagParsing(#[from] FlagError),
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
		static CURRENT: RefCell<Option<Evaluator>> = RefCell::new(None);
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

	fn with_environment(&mut self, environment: &'a Environment) -> Result<&mut Self, Self::Error> {
		self.config.merge_from_flags(environment.flags()?);
		Ok(self)
	}

	fn with_rc(&mut self, rc: &'a Rc) -> Result<&mut Self, Self::Error> {
		self.config.merge_from_flags(rc.flags()?);
		Ok(self)
	}

	fn with_import_paths(&mut self, import_paths: Vec<PathBuf>) -> Result<&mut Self, Self::Error> {
		self.import_resolver = Some(FileImportResolver::new(import_paths));
		Ok(self)
	}

	fn with_external_code<K, V>(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, Self::Error> {
		self.context_initializer.add_ext_code(key, value)?;
		Ok(self)
	}

	fn with_external_variable<K, V>(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, Self::Error> {
		self.context_initializer
			.add_ext_str(key.into(), value.into());
		Ok(self)
	}

	fn with_native_function<F>(&mut self, key: &'a str, func: F) -> Result<&mut Self, Self::Error>
	where
		F: 'static + rtk_jsonnet_core::Function<'a, Evaluator = Self>,
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
			F: 'static + Function<'a, Evaluator = Evaluator>,
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
							Err(error) => match error {
								EvaluatorError::Jrsonnet(error) => Err(error),
								_ => Err(jrsonnet_evaluator::Error::new(
									jrsonnet_evaluator::RuntimeError(format!("{error}").into()),
								)),
							},
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

					let mut params = Vec::with_capacity(argv);

					for i in 0..=argv {
						params.push(ParamParse::new(
							ParamName::Unnamed,
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

	fn with_top_level_argument<K, V>(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, Self::Error> {
		self.top_level_arguments
			.insert(key.into(), TlaArg::String(value.into()));
		Ok(self)
	}

	fn with_top_level_code<K, V>(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, Self::Error> {
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

		Evaluator::CURRENT.with(move |evaluator| {
			let old_evaluator = evaluator.replace(Some(self));

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

				Ok(Evaluation(Some(val)))
			})();

			evaluator.replace(old_evaluator);

			result
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

		Evaluator::CURRENT.with(move |evaluator| {
			let old_evaluator = evaluator.replace(Some(self));

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

				Ok(Evaluation(Some(val)))
			})();

			evaluator.replace(old_evaluator);

			result
		})
	}
}

#[derive(Clone, Debug, Error)]
pub enum EvaluatorError {
	#[error("a jrsonnet evaluation error occurred: {0}")]
	Jrsonnet(#[from] jrsonnet_evaluator::Error),
	#[error("a flag-parsing error occurred")]
	FlagParsing(#[from] FlagError),
}

#[derive(Debug)]
pub struct Evaluation(pub(crate) Option<jrsonnet_evaluator::Val>);

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
	DisableNativeFunctions(bool),
}

impl rtk_jsonnet_core::Flag for Flag {
	type Implementation = Implementation;

	type Error = FlagError;
	type Key = FlagKey;
	type Value = FlagValue;

	fn new(key: Self::Key, value: Self::Value) -> Result<Self, Self::Error> {
		match value {
			FlagValue::Bool(boolean) => {
				if key == FlagKey::DisableNativeFunctions {
					Ok(Flag::DisableNativeFunctions(boolean))
				} else {
					Err(FlagError::MismatchedKeyValue(key, value))
				}
			}
			FlagValue::OutputFormat(output_format) => match key {
				FlagKey::OutputFormatFloats => Ok(Flag::OutputFormatFloats(output_format)),
				FlagKey::OutputFormatStdManifestYamlDoc => {
					Ok(Flag::OutputFormatStdManifestYamlDoc(output_format))
				}
				FlagKey::OutputFormatStdManifestYamlStream => {
					Ok(Flag::OutputFormatStdManifestYamlStream(output_format))
				}
				_ => Err(FlagError::MismatchedKeyValue(key, value)),
			},
		}
	}
}

#[derive(Clone, Copy, Debug, Error)]
pub enum FlagError {
	#[error("invalid flag key: {0}")]
	InvalidFlagKey(#[from] FlagKeyFromStrError),
	#[error("invalid flag value: {0}")]
	InvalidFlagValue(#[from] FlagValueFromStrError),
	#[error("invalid flag key/value pair; {0} and {1} are not meant to be paired together")]
	MismatchedKeyValue(FlagKey, FlagValue),
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
	DisableNativeFunctions,
}

impl FlagKey {
	const ALL: &'static [FlagKey] = &[
		FlagKey::OutputFormatFloats,
		FlagKey::OutputFormatStdManifestYamlDoc,
		FlagKey::OutputFormatStdManifestYamlStream,
		FlagKey::DisableNativeFunctions,
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
			FlagKey::DisableNativeFunctions => formatter.write_str("disableNativeFunctions"),
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
			"disableNativeFunctions" => Ok(FlagKey::DisableNativeFunctions),
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
	Bool(bool),
	OutputFormat(OutputFormat),
}

impl fmt::Display for FlagValue {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self {
			FlagValue::Bool(boolean) => <bool as fmt::Display>::fmt(boolean, formatter),
			FlagValue::OutputFormat(output_format) => {
				<OutputFormat as fmt::Display>::fmt(output_format, formatter)
			}
		}
	}
}

impl FromStr for FlagValue {
	type Err = FlagValueFromStrError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		if let Ok(boolean) = s.parse::<bool>() {
			return Ok(FlagValue::Bool(boolean));
		}
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

/// The error returned by `<OutputFormat as FromStr>::from_str`.
#[derive(Clone, Copy, Debug, Error)]
#[error("invalid output format specified; valid forms are \"go-jsonnet\" and \"jrsonnet\"")]
pub struct OutputFormatFromStrError;
