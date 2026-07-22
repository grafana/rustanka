use std::convert::Infallible;
use std::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Evaluator;

/// Values passed to a [`Function`].
pub trait Arguments<'a>: Sized {
	type Evaluator: Evaluator<'a>;

	/// Gets the argument at `index` as a [`Value`] modeled by the [`Evaluator`].
	fn get_indexed(
		&self,
		index: usize,
	) -> Result<
		Option<<Self::Evaluator as Evaluator<'a>>::Value>,
		<Self::Evaluator as Evaluator<'a>>::Error,
	>;
}

/// A dummy implementation of [`Arguments`] for [`Evaluator`]s that
/// don't provide native function interop.
pub struct InfallibleArguments<'a, E: 'a> {
	_inner: Infallible,
	_phantom: PhantomData<&'a E>,
}

impl<'a, E> Arguments<'a> for InfallibleArguments<'a, E>
where
	E: Evaluator<'a>,
{
	type Evaluator = E;

	#[inline]
	fn get_indexed(
		&self,
		_: usize,
	) -> Result<
		Option<<Self::Evaluator as Evaluator<'a>>::Value>,
		<Self::Evaluator as Evaluator<'a>>::Error,
	> {
		unreachable!()
	}
}

pub trait Function<'a> {
	type Evaluator: Evaluator<'a>;

	fn argv(&self) -> (usize, Option<usize>);

	fn call(
		&self,
		evaluator: &Self::Evaluator,
		arguments: <Self::Evaluator as Evaluator<'a>>::Arguments<'_>,
	) -> Result<<Self::Evaluator as Evaluator<'a>>::Value, <Self::Evaluator as Evaluator<'a>>::Error>;
}

pub trait Value<'a>
where
	Self: Deserializer<'a> + Deserialize<'a> + Serialize,
{
	type Evaluator: Evaluator<'a, Value = Self>;

	type Serializer: ValueSerializer<'a, Evaluator = Self::Evaluator, Value = Self>;

	/// Gets the value at `index`, provided this value is an array.
	fn get_indexed(
		&self,
		index: usize,
	) -> Result<Option<Self>, <Self::Evaluator as Evaluator<'a>>::Error>;

	/// Gets the value at `key`, provided this value is an object.
	fn get_keyed<K>(
		&self,
		key: &K,
	) -> Result<Option<Self>, <Self::Evaluator as Evaluator<'a>>::Error>
	where
		K: AsRef<str>;
}

pub trait ValueSerializer<'a>
where
	Self: Serializer,
{
	type Evaluator: Evaluator<'a, Value = Self::Value>;
	type Value: Value<'a, Serializer = Self>;

	fn new(evaluator: &Self::Evaluator) -> Self;
}
