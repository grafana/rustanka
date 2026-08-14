//! Serde interop that carries implementation values through the data model.
//!
//! Serde's data model has no way to carry an opaque, implementation-defined
//! value: everything has to be expressible as primitives, sequences and maps.
//! `serde_json` works around this for its own `RawValue` by smuggling the raw
//! JSON text as a `&str`, which works because JSON text *is* in the data model.
//! Jsonnet values are not — they are lazy, `Rc`-backed graphs — so [`RawValue`]
//! instead hands the value to the deserializer (or serializer) out of band,
//! through a per-implementation, thread-local [`TransferSlot`], and uses a
//! sentinel-named newtype struct ([`RAW_VALUE_TOKEN`]) as the in-band signal
//! that the hand-off is happening.

use std::cell::RefCell;
use std::fmt::{self, Formatter};
use std::marker::PhantomData;

use rtk_spec::DeepMerge;
use rtk_spec::v1alpha1::EnvironmentData;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Evaluator, Value};

/// The newtype struct name a [`RawValue`] hand-off is announced with.
///
/// A [`ValueDeserializer`](crate::ValueDeserializer) sees this name in
/// [`Deserializer::deserialize_newtype_struct`] and, instead of recursing,
/// parks its value and visits unit. A [`ValueSerializer`](crate::ValueSerializer)
/// sees it in [`Serializer::serialize_newtype_struct`] and takes the parked
/// value instead of serializing the payload. Every other serializer and
/// deserializer ignores the name, which is what makes the fallbacks work:
/// serializing falls back to serializing the value itself, while deserializing
/// fails with a clear error.
pub const RAW_VALUE_TOKEN: &str = "$rtk_jsonnet_core::serde::RawValue";

/// A value captured straight out of an evaluation, without deserializing it.
///
/// Deserializing this only works with the deserializer of the implementation
/// that produced the value (see [`RAW_VALUE_TOKEN`]); everything else is an
/// error. Serializing works everywhere: implementations get the value back
/// as-is, and other serializers get the value serialized through the data
/// model.
///
/// The captured value keeps whatever laziness it had: nothing beneath it is
/// forced by being captured.
#[derive(Clone, Debug)]
pub struct RawValue<V>(V)
where
	V: Value;

impl<V> RawValue<V>
where
	V: Value,
{
	/// Wrap a value that was obtained without going through serde.
	pub fn new(value: V) -> RawValue<V> {
		RawValue(value)
	}

	/// The captured value.
	pub fn get(&self) -> &V {
		&self.0
	}

	/// Unwrap the captured value.
	pub fn into_inner(self) -> V {
		self.0
	}
}

impl<V> Default for RawValue<V>
where
	V: Default + Value,
{
	fn default() -> Self {
		RawValue(V::default())
	}
}

impl<V> DeepMerge for RawValue<V>
where
	V: Value,
{
	/// Values are opaque, so merging can only replace.
	fn merge_from(&mut self, other: Self) {
		*self = other;
	}
}

impl<'a, V> EnvironmentData<'a> for RawValue<V>
where
	V: 'a + Default + Serialize + Value,
{
	fn present() -> bool {
		true
	}
}

/// The error every deserializer that did not park a value gets.
fn not_parked<E>() -> E
where
	E: de::Error,
{
	de::Error::custom(
		"a raw value can only be deserialized by the jsonnet implementation that produced it",
	)
}

impl<'de, V> Deserialize<'de> for RawValue<V>
where
	V: Value,
{
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		// The value is taken here, inside the visitor, rather than after
		// `deserialize_newtype_struct` returns: the parking deserializer holds a
		// `ParkGuard` for the duration of that call, so by the time it returns
		// the slot has been cleared again.
		struct RawValueVisitor<V>(PhantomData<fn() -> V>)
		where
			V: Value;

		impl<'de, V> Visitor<'de> for RawValueVisitor<V>
		where
			V: Value,
		{
			type Value = RawValue<V>;

			fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
				formatter.write_str("a value parked by a jsonnet deserializer")
			}

			fn visit_unit<E>(self) -> Result<Self::Value, E>
			where
				E: de::Error,
			{
				V::take_parked().map(RawValue).ok_or_else(not_parked)
			}

			fn visit_newtype_struct<D>(self, _: D) -> Result<Self::Value, D::Error>
			where
				D: Deserializer<'de>,
			{
				Err(not_parked())
			}
		}

		deserializer.deserialize_newtype_struct(RAW_VALUE_TOKEN, RawValueVisitor(PhantomData))
	}
}

impl<V> Serialize for RawValue<V>
where
	V: Serialize + Value,
{
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		// Implementations take the parked value and ignore the payload; every
		// other serializer ignores the token and serializes the payload. The
		// guard makes sure the slot never outlives this call, whichever of the
		// two happened.
		let _guard = self.0.clone().park();
		serializer.serialize_newtype_struct(RAW_VALUE_TOKEN, &self.0)
	}
}

/// A thread-local hand-off slot for [`RawValue`] transfers.
///
/// Implementations declare exactly one of these per value type:
///
/// ```ignore
/// thread_local! {
///     static TRANSFER: TransferSlot<Val> = const { TransferSlot::new() };
/// }
/// ```
///
/// Values are parked and taken within a single serde call, so the slot is
/// always empty between hand-offs. It is nevertheless a stack rather than a
/// single slot, so that a nested hand-off cannot lose a value, and
/// [`ParkGuard`] clears anything a serializer or deserializer left behind.
/// Deliberately not a process-wide static: a leaked global would keep whole
/// evaluation graphs alive for the rest of the run.
#[derive(Debug)]
pub struct TransferSlot<T> {
	parked: RefCell<Vec<T>>,
}

impl<T> TransferSlot<T> {
	pub const fn new() -> TransferSlot<T> {
		TransferSlot {
			parked: RefCell::new(Vec::new()),
		}
	}

	/// Park a value for the hand-off that is about to happen.
	pub fn park(&self, value: T) {
		self.parked.borrow_mut().push(value);
	}

	/// Take the most recently parked value, if the hand-off happened.
	pub fn take(&self) -> Option<T> {
		self.parked.borrow_mut().pop()
	}
}

impl<T> Default for TransferSlot<T> {
	fn default() -> Self {
		TransferSlot::new()
	}
}

/// Clears a parked value that was never taken.
///
/// Returned by [`Value::park`] and held for the duration of the serde call the
/// hand-off is part of.
#[must_use = "dropping the guard immediately unparks the value again"]
pub struct ParkGuard<V>(PhantomData<fn() -> V>)
where
	V: Value;

impl<V> ParkGuard<V>
where
	V: Value,
{
	/// Called by implementations of [`Value::park`], after parking.
	pub fn new() -> ParkGuard<V> {
		ParkGuard(PhantomData)
	}
}

impl<V> Default for ParkGuard<V>
where
	V: Value,
{
	fn default() -> Self {
		ParkGuard::new()
	}
}

impl<V> fmt::Debug for ParkGuard<V>
where
	V: Value,
{
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str("ParkGuard")
	}
}

impl<V> Drop for ParkGuard<V>
where
	V: Value,
{
	fn drop(&mut self) {
		drop(V::take_parked());
	}
}

/// The error type of the evaluator a [`Value`] belongs to.
pub type ValueError<V> = <<V as Value>::Evaluator as Evaluator>::Error;
