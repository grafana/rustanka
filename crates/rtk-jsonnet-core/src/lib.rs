use std::error::Error;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

use ::serde::{Deserialize, Deserializer, Serializer};
use rtk_spec::canonical::{EnvironmentSpec, JsonentImplementationOrConfig, Rc};

mod native;
pub mod serde;

pub use crate::native::{
	Arguments, Array, Function, Hidden, InfallibleArguments, Object, Value, ValueDeserializer,
	ValueSerializer,
};
pub use crate::serde::{ParkGuard, RAW_VALUE_TOKEN, RawValue, TransferSlot, ValueError};

pub trait Context: Clone + Sized {
	type Evaluator: Evaluator<Context = Self>;

	#[inline]
	fn create_deserializer(
		&self,
		value: <Self::Evaluator as Evaluator>::Value,
	) -> <<Self::Evaluator as Evaluator>::Value as Value>::Deserializer {
		<<<Self::Evaluator as Evaluator>::Value as Value>::Deserializer as ValueDeserializer>::new(
			self, value,
		)
	}

	#[inline]
	fn create_serializer(&self) -> <<Self::Evaluator as Evaluator>::Value as Value>::Serializer {
		<<<Self::Evaluator as Evaluator>::Value as Value>::Serializer as ValueSerializer>::new(self)
	}
}

pub trait Evaluator: Sized {
	type Implementation: Implementation<Evaluator = Self>;

	type Arguments: Arguments;
	type Context: Context<Evaluator = Self>;
	type Error: EvaluatorError<Evaluator = Self>;
	type Value: Value<Evaluator = Self>;

	fn new(implementation: &Self::Implementation) -> Self;

	fn with_rc(&mut self, rc: Rc) -> Result<&mut Self, Self::Error>;

	/// Configure this evaluator for the environment it is about to evaluate.
	///
	/// An environment may ask to be evaluated by a different Jsonnet
	/// implementation. Rather than hand over to one, rtk evaluates the
	/// environment itself and imitates the output the requested implementation
	/// would have produced — which is this implementation's job to arrange, since
	/// only it knows how its own output differs.
	///
	/// Anything [`Evaluator::with_rc`] configures wins, so a project's settings
	/// override an environment's preference. The environment is taken as its
	/// spec, which is the whole of what it configures; its metadata reaches
	/// Jsonnet as external code instead.
	fn with_environment(&mut self, environment: &EnvironmentSpec)
	-> Result<&mut Self, Self::Error>;

	fn with_import_paths(&mut self, import_paths: Vec<PathBuf>) -> Result<&mut Self, Self::Error>;

	fn with_plugin<P>(&mut self, plugin: P) -> Result<&mut Self, Self::Error>
	where
		P: Plugin<Self>;

	fn with_external_code(&mut self, key: &str, value: &str) -> Result<&mut Self, Self::Error>;

	fn with_external_variable(&mut self, key: &str, value: &str) -> Result<&mut Self, Self::Error>;

	fn with_native_function<F>(&mut self, key: &str, func: F) -> Result<&mut Self, Self::Error>
	where
		F: 'static + Function<Self>;

	fn with_top_level_argument(&mut self, key: &str, value: &str)
	-> Result<&mut Self, Self::Error>;

	fn with_top_level_code(&mut self, key: &str, value: &str) -> Result<&mut Self, Self::Error>;

	fn evaluate_file<P>(
		self,
		path: P,
	) -> Result<(Self::Context, Self::Value), <Self as Evaluator>::Error>
	where
		P: AsRef<Path> + fmt::Debug;

	fn evaluate_snippet<S>(
		self,
		snippet: S,
	) -> Result<(Self::Context, Self::Value), <Self as Evaluator>::Error>
	where
		S: AsRef<str> + fmt::Debug;
}

pub trait EvaluatorError
where
    Self: Error
        + for<'de> From<<<Self::Evaluator as Evaluator>::Arguments as Deserializer<'de>>::Error>
        + for<'de> From<<<<Self::Evaluator as Evaluator>::Value as Value>::Deserializer as Deserializer<'de>>::Error>
		+ From<<<<Self::Evaluator as Evaluator>::Value as Value>::Serializer as Serializer>::Error>,
{
    type Evaluator: Evaluator<Error = Self>;

    fn custom<T>(message: T) -> Self
    where
        T: fmt::Display;
}

pub trait Flag: Sized {
	type Implementation: Implementation<Flag = Self>;

	type Key: FromStr;
	type Value: (for<'de> Deserialize<'de>) + FromStr;
	type Error: Error + From<<Self::Key as FromStr>::Err> + From<<Self::Value as FromStr>::Err>;

	fn new(key: Self::Key, value: Self::Value) -> Result<Self, Self::Error>;
}

mod __sealant {
	pub trait Sealed {}
}

pub trait FlagsExt: __sealant::Sealed {
	fn flags<F>(&self) -> Result<impl Iterator<Item = F>, <F as Flag>::Error>
	where
		F: Flag;
}

/// Parse the flags an implementation was configured with, if it was named as a
/// configuration rather than on its own.
fn parse_flags<F>(
	implementation: Option<&JsonentImplementationOrConfig>,
) -> Result<impl Iterator<Item = F>, <F as Flag>::Error>
where
	F: Flag,
{
	let Some(JsonentImplementationOrConfig::JsonnetImplementationConfig(config)) = implementation
	else {
		return Ok(Vec::new().into_iter());
	};

	let flags = &config.flags.0;
	let mut parsed = Vec::with_capacity(flags.len());

	for (key, value) in flags {
		let key = key.parse::<F::Key>()?;
		let value = value.parse::<F::Value>()?;
		parsed.push(F::new(key, value)?);
	}

	Ok(parsed.into_iter())
}

impl __sealant::Sealed for EnvironmentSpec {}

impl FlagsExt for EnvironmentSpec {
	fn flags<F>(&self) -> Result<impl Iterator<Item = F>, <F as Flag>::Error>
	where
		F: Flag,
	{
		parse_flags(self.export_jsonnet_implementation.as_ref())
	}
}

impl __sealant::Sealed for Rc {}

impl FlagsExt for Rc {
	fn flags<F>(&self) -> Result<impl Iterator<Item = F>, <F as Flag>::Error>
	where
		F: Flag,
	{
		parse_flags(self.spec.jsonnet_implementation.as_ref())
	}
}

pub trait Implementation: Sized {
	type Evaluator: Evaluator<Implementation = Self>;
	type Flag: Flag<Implementation = Self>;
	type Error: Error
		+ From<Self::InitializationError>
		+ From<<Self::Flag as Flag>::Error>
		+ From<<Self::Evaluator as Evaluator>::Error>;
	type InitializationError: Error;

	fn new(flags: impl Iterator<Item = Self::Flag>) -> Result<Self, Self::InitializationError>;

	fn create_evaluator(&self) -> Self::Evaluator {
		Self::Evaluator::new(self)
	}
}

pub trait Plugin<E: Evaluator> {
	fn install(self, evaluator: &mut E) -> Result<(), E::Error>;
}
