use std::convert::Infallible;
use std::marker::PhantomData;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Evaluator;

/// Values passed to a [`Function`], with numeric indices and named indicies.
pub trait Arguments: Sized {
    type Evaluator: Evaluator;

    /// Gets the argument at `index` as a [`Value`] modeled by the [`Evaluator`].
    fn get_indexed(&self, index: usize)
        -> Result<Option<<Self::Evaluator as Evaluator>::Value>, <Self::Evaluator as Evaluator>::Error>;
    
    /// Gets the named argument `name` as a [`Value`] modeled by the [`Evaluator`].
    fn get_named<N>(&self, name: &N)
        -> Result<Option<<Self::Evaluator as Evaluator>::Value>, <Self::Evaluator as Evaluator>::Error>
    where
        N: AsRef<str>;
}

/// A dummy implementation of [`Arguments`] for [`Evaluator`]s that
/// don't provide native function interop.
pub struct InfallibleArguments<E: 'static> {
    _inner: Infallible,
    _phantom: PhantomData<&'static E>,
}

impl<E> Arguments for InfallibleArguments<E>
where
    E: 'static + Evaluator,
{
    type Evaluator = E;

    #[inline]
    fn get_indexed(&self, _: usize)
        -> Result<Option<<Self::Evaluator as Evaluator>::Value>, <Self::Evaluator as Evaluator>::Error>
    { unreachable!() }

    #[inline]
    fn get_named<N>(&self, _: &N)
        -> Result<Option<<Self::Evaluator as Evaluator>::Value>, <Self::Evaluator as Evaluator>::Error>
    where
        N: AsRef<str>
    { unreachable!() }
}

pub trait Function {
    type Evaluator: Evaluator;

    fn call(
        &self,
        evaluator: &Self::Evaluator,
        arguments: <Self::Evaluator as Evaluator>::Arguments,
    ) -> Result<<Self::Evaluator as Evaluator>::Value, <Self::Evaluator as Evaluator>::Error>;
}

pub trait Value: DeserializeOwned + Serialize {
    type Evaluator: Evaluator<Value = Self>;

    /// Creates a new [`Value`] by serializing `value`.
    fn new<V>(evaluator: &Self::Evaluator, value: V)
        -> Result<Self, <Self::Evaluator as Evaluator>::Error>
    where
        V: Serialize,
    {
        Ok(V::serialize(&value, evaluator.create_serializer())?)
    }
    
    /// Gets the value at `index`, provided this value is an array.
    fn get_indexed(&self, index: usize) -> Result<Self, <Self::Evaluator as Evaluator>::Error>;

    /// Gets the value at `key`, provided this value is an object.
    fn get_keyed<K>(&self, key: &K) -> Result<Self, <Self::Evaluator as Evaluator>::Error>
    where
        K: AsRef<str>;
}
