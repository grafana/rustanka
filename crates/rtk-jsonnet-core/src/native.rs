use std::convert::Infallible;
use std::error::Error;
use std::fmt::{self, Formatter};
use std::marker::PhantomData;

use serde::de::{self, Visitor};
use serde::{Deserializer, Serializer, forward_to_deserialize_any};

use crate::Evaluator;

/// Values passed to a [`Function`].
pub trait Arguments: (for<'de> Deserializer<'de>) + Sized {
	type Evaluator: Evaluator;
}

/// A dummy implementation of [`Arguments`] for [`Evaluator`]s that
/// don't provide native function interop.
pub struct InfallibleArguments<E> {
	_inner: Infallible,
	_phantom: PhantomData<fn(E)>,
}

impl<'de, E> Deserializer<'de> for InfallibleArguments<E> {
	type Error = InfallibleError;

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

impl<E> Arguments for InfallibleArguments<E>
where
	E: Evaluator,
{
	type Evaluator = E;
}

pub trait Array
where
	Self: Clone + Into<Self::Value>,
{
	type Evaluator: Evaluator<Value = Self::Value>;
	type Value: Value<Array = Self>;

	type Iter<'a>: Iterator<Item = Result<Self::Value, <Self::Evaluator as Evaluator>::Error>> + 'a
	where
		Self: 'a;

	fn len(&self) -> usize;

	fn is_empty(&self) -> bool {
		self.len() == 0
	}

	fn get(
		&self,
		index: usize,
	) -> Result<Option<Self::Value>, <Self::Evaluator as Evaluator>::Error>;

	fn iter(&self) -> Self::Iter<'_>;
}

pub struct InfallibleError {
	_inner: Infallible,
}

impl fmt::Debug for InfallibleError {
	fn fmt(&self, _: &mut Formatter<'_>) -> fmt::Result {
		unreachable!()
	}
}

impl fmt::Display for InfallibleError {
	fn fmt(&self, _: &mut Formatter<'_>) -> fmt::Result {
		unreachable!()
	}
}

impl Error for InfallibleError {}

impl de::Error for InfallibleError {
	fn custom<T>(_: T) -> Self
	where
		T: fmt::Display,
	{
		unreachable!()
	}
}

pub trait Function<E: Evaluator> {
	fn argv(&self) -> (usize, Option<usize>);

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		None
	}

	fn call(
		&self,
		evaluator: &E,
		arguments: <E as Evaluator>::Arguments,
	) -> Result<<E as Evaluator>::Value, <E as Evaluator>::Error>;
}

pub trait Object
where
	Self: Clone + Into<Self::Value>,
{
	type Evaluator: Evaluator<Value = Self::Value>;
	type Value: Value<Object = Self>;

	type ValuesIter<'a>: Iterator<Item = Result<Self::Value, <Self::Evaluator as Evaluator>::Error>>
	where
		Self: 'a;

	fn has(&self, key: &str) -> Result<bool, <Self::Evaluator as Evaluator>::Error>;
	fn get(&self, key: &str) -> Result<Self::Value, <Self::Evaluator as Evaluator>::Error>;

	fn values(&self) -> Self::ValuesIter<'_>;
}

pub trait Value
where
	Self: Clone + fmt::Debug,
{
	type Evaluator: Evaluator<Value = Self>;

	type Deserializer: ValueDeserializer<Evaluator = Self::Evaluator, Value = Self>;
	type Serializer: ValueSerializer<Evaluator = Self::Evaluator, Value = Self>;

	type Array: Array<Evaluator = Self::Evaluator, Value = Self>;
	type Object: Object<Evaluator = Self::Evaluator, Value = Self>;

	fn into_array(self) -> Result<Self::Array, Self>;
	fn into_object(self) -> Result<Self::Object, Self>;
}

pub trait ValueDeserializer
where
	Self: for<'de> Deserializer<'de, Error = <Self::Evaluator as Evaluator>::Error>,
{
	type Evaluator: Evaluator<Value = Self::Value>;
	type Value: Value<Deserializer = Self>;

	fn new(context: &<Self::Evaluator as Evaluator>::Context, value: Self::Value) -> Self;
}

pub trait ValueSerializer
where
	Self: Serializer<Ok = Self::Value>,
{
	type Evaluator: Evaluator<Value = Self::Value>;
	type Value: Value<Serializer = Self>;

	fn new(evaluator: &<Self::Evaluator as Evaluator>::Context) -> Self;
}
