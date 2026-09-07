use std::convert::Infallible;
use std::error::Error;
use std::fmt::{self, Formatter};
use std::marker::PhantomData;
use std::ops::Deref;

use ::serde::de::{self, Visitor};
use ::serde::{Deserializer, Serializer, forward_to_deserialize_any};

use crate::serde::{ParkGuard, ValueError};
use crate::{Evaluator, EvaluatorError};

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

/// Whether a field lookup reaches fields Jsonnet hides from output.
///
/// Hiding a field (`field:: value`) keeps it out of the manifested output, but
/// not out of reach: naming one in Jsonnet still evaluates it. Which of the two
/// a lookup wants depends on what it is for — code that mirrors what would be
/// written out wants [`Hidden::Skip`], code that reads configuration out of an
/// object the way Jsonnet would wants [`Hidden::Include`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Hidden {
	/// Only fields that would be manifested.
	#[default]
	Skip,
	/// Hidden fields as well, as naming one in Jsonnet does.
	Include,
}

impl Hidden {
	/// Whether this is [`Hidden::Include`].
	pub fn included(self) -> bool {
		self == Hidden::Include
	}
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

	/// Whether the object has a field by this name.
	///
	/// Does not force the field, so this is the way to ask about one whose value
	/// is not wanted (or might not be evaluable).
	fn has(&self, key: &str, hidden: Hidden)
	-> Result<bool, <Self::Evaluator as Evaluator>::Error>;

	/// The field's value, or [`None`] if the object has no such field.
	///
	/// Forces the field.
	fn get(
		&self,
		key: &str,
		hidden: Hidden,
	) -> Result<Option<Self::Value>, <Self::Evaluator as Evaluator>::Error>;

	/// [`Object::get`], failing when the field is absent.
	///
	/// Implementations should override this when they can say something more
	/// helpful than that the field is missing.
	fn get_or_bail(
		&self,
		key: &str,
		hidden: Hidden,
	) -> Result<Self::Value, <Self::Evaluator as Evaluator>::Error> {
		match self.get(key, hidden)? {
			Some(value) => Ok(value),
			None => Err(
				<<Self::Evaluator as Evaluator>::Error as EvaluatorError>::custom(format_args!(
					"no such field: {key}"
				)),
			),
		}
	}

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

	/// A string read out of a value.
	///
	/// Cheap to clone, and derefs to [`str`], so implementations can hand out
	/// whatever they already have rather than a fresh [`String`].
	type Str: Clone + fmt::Debug + Deref<Target = str>;

	fn into_array(self) -> Result<Self::Array, Self>;
	fn into_object(self) -> Result<Self::Object, Self>;

	/// Like [`Value::into_array`], but without giving up ownership.
	fn as_array(&self) -> Option<Self::Array>;

	/// Like [`Value::into_object`], but without giving up ownership.
	fn as_object(&self) -> Option<Self::Object>;

	/// This value as a string, if it is one.
	///
	/// Nothing is forced or evaluated: a value in hand has been evaluated
	/// already. Use this rather than deserializing when all that is wanted is to
	/// look at a value — reading a field to decide how to treat an object, say.
	fn as_str(&self) -> Option<Self::Str>;

	/// This value as a number, if it is one.
	fn as_number(&self) -> Option<f64>;

	/// This value as a boolean, if it is one.
	fn as_bool(&self) -> Option<bool>;

	/// Whether this value is `null`.
	fn is_null(&self) -> bool;

	/// Append this value's canonical JSON text to `buffer`.
	///
	/// This is the implementation's own manifestification, not a serde
	/// round-trip: number formatting, escaping and field order are whatever the
	/// implementation would write when asked to output JSON. Code that has to
	/// match another Jsonnet implementation byte for byte must go through here
	/// rather than through [`Serialize`](serde::Serialize), because the serde
	/// data model cannot represent everything a Jsonnet number can.
	///
	/// Forces the value and everything beneath it, exactly as serializing it
	/// would.
	fn manifest_into(&self, buffer: &mut String) -> Result<(), ValueError<Self>>;

	/// [`Value::manifest_into`] into a fresh [`String`].
	///
	/// Prefer [`Value::manifest_into`] when manifesting many values, so the
	/// buffer can be reused.
	fn manifest(&self) -> Result<String, ValueError<Self>> {
		let mut buffer = String::new();
		self.manifest_into(&mut buffer)?;
		Ok(buffer)
	}

	/// Park this value in the implementation's [`TransferSlot`](crate::TransferSlot), for a
	/// [`RawValue`](crate::RawValue) hand-off that is about to happen.
	///
	/// Implementations must park into a thread-local
	/// [`TransferSlot`](crate::TransferSlot) and return
	/// [`ParkGuard::new`], so that a value the hand-off did not pick up is
	/// dropped with the guard instead of living on in a global.
	fn park(self) -> ParkGuard<Self>;

	/// Take the value most recently parked by [`Value::park`] on this thread.
	fn take_parked() -> Option<Self>;
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
