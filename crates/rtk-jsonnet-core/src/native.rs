use std::convert::Infallible;
use std::error::Error;
use std::fmt::{self, Formatter};
use std::marker::PhantomData;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer, forward_to_deserialize_any};

use crate::Evaluator;

/// Values passed to a [`Function`].
pub trait Arguments<'a, 'b>: Deserializer<'b> + Sized {
	type Evaluator: Evaluator<'a>;
}

/// A dummy implementation of [`Arguments`] for [`Evaluator`]s that
/// don't provide native function interop.
pub struct InfallibleArguments<'a, 'b, E> {
	_inner: Infallible,
	_phantom: PhantomData<fn(&'a (), &'b (), E)>,
}

impl<'a, 'de, E> Deserializer<'de> for InfallibleArguments<'a, 'de, E> {
	type Error = InfallibleError<'de, E>;

	fn deserialize_any<V>(self, _: V) -> Result<V::Value, Self::Error>
	where
		V: Visitor<'de>,
	{
		unreachable!()
	}

	forward_to_deserialize_any! {
		bytes identifier bool byte_buf char enum f32 f64 i128 i16 i32 i64 i8
		ignored_any map newtype_struct option seq str string struct tuple
		tuple_struct u128 u16 u32 u64 u8 unit unit_struct
	}
}

impl<'a, 'b, E> Arguments<'a, 'b> for InfallibleArguments<'a, 'b, E>
where
	E: Evaluator<'a>,
{
	type Evaluator = E;
}

pub trait Array<'a>
where
	Self: Clone + Deserialize<'a> + Serialize,
{
	type Evaluator: Evaluator<'a, Value = Self::Value>;
	type Value: Value<'a, Array = Self>;

	fn len(&self) -> usize;

	fn is_empty(&self) -> bool {
		self.len() == 0
	}

	fn get(
		&self,
		index: usize,
	) -> Result<Option<Self::Value>, <Self::Evaluator as Evaluator<'a>>::Error>;
	fn iter(
		&self,
	) -> impl Iterator<Item = Result<Self::Value, <Self::Evaluator as Evaluator<'a>>::Error>>;
}

pub struct InfallibleError<'a, E> {
	_inner: Infallible,
	_phantom: PhantomData<fn(&'a (), E)>,
}

impl<'a, E> fmt::Debug for InfallibleError<'a, E> {
	fn fmt(&self, _: &mut Formatter<'_>) -> fmt::Result {
		unreachable!()
	}
}

impl<'a, E> fmt::Display for InfallibleError<'a, E> {
	fn fmt(&self, _: &mut Formatter<'_>) -> fmt::Result {
		unreachable!()
	}
}

impl<'a, E> Error for InfallibleError<'a, E> {}

impl<'a, E> de::Error for InfallibleError<'a, E> {
	fn custom<T>(_: T) -> Self
	where
		T: fmt::Display,
	{
		unreachable!()
	}
}

pub trait Function<'a, E: Evaluator<'a>> {
	fn argv(&self) -> (usize, Option<usize>);

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		None
	}

	fn call<'b>(
		&self,
		evaluator: &E,
		arguments: <E as Evaluator<'a>>::Arguments<'b>,
	) -> Result<<E as Evaluator<'a>>::Value, <E as Evaluator<'a>>::Error>;
}

pub trait Object<'a>
where
	Self: Clone + Deserialize<'a> + Serialize,
{
	type Evaluator: Evaluator<'a, Value = Self::Value>;
	type Value: Value<'a, Object = Self>;

	fn fields(&self) -> impl Iterator<Item = &str>;
	fn has(&self, key: &str) -> Result<bool, <Self::Evaluator as Evaluator<'a>>::Error>;
	fn get(&self, key: &str) -> Result<Self::Value, <Self::Evaluator as Evaluator<'a>>::Error>;
}

pub trait Value<'a>
where
	Self: Clone + Deserialize<'a> + Serialize,
{
	type Evaluator: Evaluator<'a, Value = Self>;

	type Deserializer: ValueDeserializer<'a, Evaluator = Self::Evaluator, Value = Self>;
	type Serializer: ValueSerializer<'a, Evaluator = Self::Evaluator, Value = Self>;

	type Array: Array<'a, Evaluator = Self::Evaluator, Value = Self>;
	type Object: Object<'a, Evaluator = Self::Evaluator, Value = Self>;

	fn into_array(self) -> Result<Self::Array, Self>;
	fn into_object(self) -> Result<Self::Object, Self>;
}

pub trait ValueDeserializer<'a>
where
	Self: Deserializer<'a, Error = <Self::Evaluator as Evaluator<'a>>::Error>,
{
	type Evaluator: Evaluator<'a, Value = Self::Value>;
	type Value: Value<'a, Deserializer = Self>;

	fn new(evaluator: &Self::Evaluator, value: Self::Value) -> Self;
}

pub trait ValueSerializer<'a>
where
	Self: Serializer<Ok = Self::Value>,
{
	type Evaluator: Evaluator<'a, Value = Self::Value>;
	type Value: Value<'a, Serializer = Self>;

	fn new(evaluator: &Self::Evaluator) -> Self;
}
