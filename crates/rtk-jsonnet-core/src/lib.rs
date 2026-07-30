use std::error::Error;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

use ::serde::{Deserialize, Deserializer, Serialize, Serializer};
use rtk_spec::canonical::{Environment, JsonentImplementationOrConfig, Rc};

mod native;

pub use crate::native::{Arguments, Function, InfallibleArguments, Value, ValueSerializer};

pub trait Evaluator<'a>: 'a + Sized {
	type Implementation: Implementation<Evaluator<'a> = Self>;

	type Arguments<'b>: Arguments<'a, 'b>;
	type Error: EvaluatorError<'a, Evaluator = Self>;
	type Evaluation: Evaluation<'a>;
	type Value: Value<'a, Evaluator = Self>;

	fn new(implementation: &Self::Implementation) -> Self;

	fn name() -> &'static str;

	#[inline]
	fn create_serializer(&self) -> <Self::Value as Value<'a>>::Serializer {
		<<Self::Value as Value<'a>>::Serializer as ValueSerializer<'a>>::new(self)
	}

	fn with_rc(
		&mut self,
		rc: &'a Rc,
	) -> Result<&mut Self, <Self::Implementation as Implementation>::Error>;
	fn with_import_paths(
		&mut self,
		import_paths: Vec<PathBuf>,
	) -> Result<&mut Self, <Self::Implementation as Implementation>::Error>;

	fn with_plugin<P>(
		&mut self,
		p: P,
	) -> Result<&mut Self, <Self::Implementation as Implementation>::Error>
	where
		P: Plugin<'a, Self>;

	fn with_external_code(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, <Self::Implementation as Implementation>::Error>;
	fn with_external_variable(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, <Self::Implementation as Implementation>::Error>;

	fn with_native_function<F>(
		&mut self,
		key: &'a str,
		func: F,
	) -> Result<&mut Self, <Self::Implementation as Implementation>::Error>
	where
		F: 'static + Function<'a, Self>;

	fn with_top_level_argument(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, <Self::Implementation as Implementation>::Error>;
	fn with_top_level_code(
		&mut self,
		key: &'a str,
		value: &'a str,
	) -> Result<&mut Self, <Self::Implementation as Implementation>::Error>;

	fn evaluate_file<P>(self, path: P) -> Result<Self::Evaluation, <Self as Evaluator<'a>>::Error>
	where
		P: 'a + AsRef<Path> + fmt::Debug;

	fn evaluate_snippet<S>(
		self,
		snippet: S,
	) -> Result<Self::Evaluation, <Self as Evaluator<'a>>::Error>
	where
		S: 'a + AsRef<str> + fmt::Debug;
}

pub trait EvaluatorError<'a>
where
    Self: Error
        + for<'b> From<<<Self::Evaluator as Evaluator<'a>>::Arguments<'b> as Deserializer<'b>>::Error>
        + From<<<Self::Evaluator as Evaluator<'a>>::Value as Deserializer<'a>>::Error>
		+ From<<<<Self::Evaluator as Evaluator<'a>>::Value as Value<'a>>::Serializer as Serializer>::Error>,
{
    type Evaluator: Evaluator<'a, Error = Self>;

    fn custom<T>(message: T) -> Self
    where
        T: Into<String>;
}

pub trait Evaluation<'a>: 'a + Sized + Serialize {}

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

impl __sealant::Sealed for Environment {}

impl FlagsExt for Environment {
	fn flags<F>(&self) -> Result<impl Iterator<Item = F>, <F as Flag>::Error>
	where
		F: Flag,
	{
		let jsonnet_implementation_config = match self.spec.export_jsonnet_implementation.as_ref() {
			Some(JsonentImplementationOrConfig::JsonnetImplementationConfig(
				jsonnet_implementation_config,
			)) => jsonnet_implementation_config,
			_ => return Ok(vec![].into_iter()),
		};

		let flags = &jsonnet_implementation_config.flags.0;

		let mut parsed = Vec::with_capacity(flags.len());

		for (key, value) in flags {
			let key = key.parse::<F::Key>()?;
			let value = value.parse::<F::Value>()?;
			let flag = F::new(key, value)?;
			parsed.push(flag);
		}

		Ok(parsed.into_iter())
	}
}

impl __sealant::Sealed for Rc {}

impl FlagsExt for Rc {
	fn flags<F>(&self) -> Result<impl Iterator<Item = F>, <F as Flag>::Error>
	where
		F: Flag,
	{
		let jsonnet_implementation_config = match self.spec.jsonnet_implementation.as_ref() {
			Some(JsonentImplementationOrConfig::JsonnetImplementationConfig(
				jsonnet_implementation_config,
			)) => jsonnet_implementation_config,
			_ => return Ok(vec![].into_iter()),
		};

		let flags = &jsonnet_implementation_config.flags.0;

		let mut parsed = Vec::with_capacity(flags.len());

		for (key, value) in flags {
			let key = key.parse::<F::Key>()?;
			let value = value.parse::<F::Value>()?;
			let flag = F::new(key, value)?;
			parsed.push(flag);
		}

		Ok(parsed.into_iter())
	}
}

pub trait Implementation: Sized {
	type Evaluator<'a>: Evaluator<'a, Implementation = Self>;
	type Flag: Flag<Implementation = Self>;
	type Error: Error
		+ From<Self::InitializationError>
		+ From<<Self::Flag as Flag>::Error>
		+ for<'a> From<<Self::Evaluator<'a> as Evaluator<'a>>::Error>;
	type InitializationError: Error;

	fn new<'a>(flags: impl Iterator<Item = Self::Flag>) -> Result<Self, Self::InitializationError>;

	fn create_evaluator(&self) -> Self::Evaluator<'_> {
		Self::Evaluator::new(self)
	}
}

pub trait Plugin<'a, E: Evaluator<'a>> {
	fn install(
		self,
		evaluator: &mut E,
	) -> Result<(), <<E as Evaluator<'a>>::Implementation as Implementation>::Error>;
}
