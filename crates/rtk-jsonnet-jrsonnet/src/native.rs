use std::thread_local;

use jrsonnet_evaluator::error::ErrorKind;
use jrsonnet_evaluator::manifest::{JsonFormat, ManifestFormat};
use jrsonnet_evaluator::val::{ArrValue, StrValue};
use jrsonnet_evaluator::{Error, IStr, ObjValue, Thunk, Val};
use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::{Hidden, ParkGuard, TransferSlot};

use crate::serde::{ValueDeserializer, ValueSerializer};
use crate::{Evaluator, EvaluatorError};

pub struct Arguments(pub Box<[Option<Thunk<Val>>]>);

impl jsonnet::Arguments for Arguments {
	type Evaluator = Evaluator;
}

#[derive(Clone, Debug)]
pub struct Value(pub Val);

/// A string read out of a [`Value`]: jrsonnet's interned string, so handing one
/// out costs nothing but a handle.
pub type Str = IStr;

thread_local! {
	/// Hand-off slot for [`RawValue`](rtk_jsonnet_core::RawValue) transfers, see
	/// [`TransferSlot`].
	static TRANSFER: TransferSlot<Val> = const { TransferSlot::new() };
}

/// jrsonnet has no null-ish `Val`, but [`EnvironmentData`] and friends need a
/// `Default`; `null` is the value that manifests and deserializes like an
/// absent one.
///
/// [`EnvironmentData`]: rtk_spec::v1alpha1::EnvironmentData
impl Default for Value {
	fn default() -> Self {
		Value(Val::Null)
	}
}

impl jsonnet::Value for Value {
	type Evaluator = Evaluator;

	type Deserializer = ValueDeserializer;
	type Serializer = ValueSerializer;

	type Array = Array;
	type Object = Object;

	type Str = Str;

	fn into_array(self) -> Result<Array, Self> {
		match self.0 {
			Val::Arr(array) => Ok(Array(array)),
			value => Err(Value(value)),
		}
	}

	fn into_object(self) -> Result<Object, Self> {
		match self.0 {
			Val::Obj(object) => Ok(Object(object)),
			value => Err(Value(value)),
		}
	}

	fn as_array(&self) -> Option<Array> {
		self.0.as_arr().map(Array)
	}

	fn as_object(&self) -> Option<Object> {
		self.0.as_obj().map(Object)
	}

	/// Strings are interned, so this hands out a handle to the one that is
	/// already there. A string built by concatenation is a rope, and flattening
	/// it costs one pass and one interning.
	fn as_str(&self) -> Option<Str> {
		self.0.as_str().map(StrValue::into_flat)
	}

	fn as_number(&self) -> Option<f64> {
		self.0.as_num()
	}

	fn as_bool(&self) -> Option<bool> {
		self.0.as_bool()
	}

	fn is_null(&self) -> bool {
		self.0.as_null().is_some()
	}

	/// Manifests with [`JsonFormat::default`], which is what tk's Jsonnet output
	/// goes through, so that anything derived from this text (Kubernetes
	/// manifests in particular) matches tk byte for byte.
	fn manifest_into(&self, buffer: &mut String) -> Result<(), EvaluatorError> {
		JsonFormat::default()
			.manifest_buf(&self.0, buffer)
			.map_err(EvaluatorError::from)
	}

	fn park(self) -> ParkGuard<Self> {
		TRANSFER.with(|transfer| transfer.park(self.0));
		ParkGuard::new()
	}

	fn take_parked() -> Option<Self> {
		TRANSFER.with(TransferSlot::take).map(Value)
	}
}

#[derive(Clone, Debug)]
pub struct Array(pub ArrValue);

impl Array {
	pub fn into_values(self) -> ArrayValues {
		ArrayValues {
			array: self.0,
			index: 0,
		}
	}
}

pub struct ArrayValues {
	array: ArrValue,
	index: usize,
}

impl Iterator for ArrayValues {
	type Item = Result<Value, EvaluatorError>;

	fn next(&mut self) -> Option<Self::Item> {
		let value = self.array.get(self.index).transpose()?;
		self.index += 1;
		Some(value.map(Value).map_err(EvaluatorError::from))
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		let remaining = self.array.len().saturating_sub(self.index);
		(remaining, Some(remaining))
	}
}

impl ExactSizeIterator for ArrayValues {}

impl jsonnet::Array for Array {
	type Evaluator = Evaluator;
	type Value = Value;

	type Iter<'a> = Box<dyn Iterator<Item = Result<Self::Value, EvaluatorError>> + 'a>;

	fn len(&self) -> usize {
		self.0.len()
	}

	fn get(&self, index: usize) -> Result<Option<Value>, EvaluatorError> {
		Ok(self.0.get(index)?.map(Value))
	}

	fn iter(&self) -> Self::Iter<'_> {
		// FIXME: Use unboxed variant when
		// https://github.com/rust-lang/rust/issues/63063 finally lands.
		Box::new(
			self.0
				.iter()
				.map(|element| element.map(Value).map_err(EvaluatorError::from)),
		)
	}
}

impl From<Array> for Value {
	fn from(value: Array) -> Self {
		Value(Val::Arr(value.0))
	}
}

#[derive(Clone, Debug)]
pub struct Object(pub(crate) ObjValue);

impl Object {
	pub fn into_values(self) -> ObjectValues {
		ObjectValues {
			fields: self.0.fields().into_iter(),
			object: self.0,
		}
	}

	/// Like [`Object::into_values`], but keeping each value's field name.
	pub fn into_fields(self) -> ObjectFields {
		ObjectFields {
			fields: self.0.fields().into_iter(),
			object: self.0,
		}
	}
}

pub struct ObjectFields {
	object: ObjValue,
	fields: std::vec::IntoIter<jrsonnet_evaluator::IStr>,
}

impl Iterator for ObjectFields {
	type Item = (Box<str>, Result<Value, EvaluatorError>);

	fn next(&mut self) -> Option<Self::Item> {
		let field = self.fields.next()?;
		let value = self
			.object
			.get_or_bail(field.clone())
			.map(Value)
			.map_err(EvaluatorError::from);
		Some((field.as_str().into(), value))
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		self.fields.size_hint()
	}
}

impl ExactSizeIterator for ObjectFields {}

pub struct ObjectValues {
	object: ObjValue,
	fields: std::vec::IntoIter<jrsonnet_evaluator::IStr>,
}

impl Iterator for ObjectValues {
	type Item = Result<Value, EvaluatorError>;

	fn next(&mut self) -> Option<Self::Item> {
		let field = self.fields.next()?;
		Some(
			self.object
				.get_or_bail(field)
				.map(Value)
				.map_err(EvaluatorError::from),
		)
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		self.fields.size_hint()
	}
}

impl ExactSizeIterator for ObjectValues {}

impl From<Object> for Value {
	fn from(value: Object) -> Self {
		Value(Val::Obj(value.0))
	}
}

impl jsonnet::Object for Object {
	type Evaluator = Evaluator;
	type Value = Value;

	type ValuesIter<'a> = Box<dyn Iterator<Item = Result<Value, EvaluatorError>> + 'a>;

	fn has(&self, key: &str, hidden: Hidden) -> Result<bool, EvaluatorError> {
		Ok(self.0.has_field_ex(key.into(), hidden.included()))
	}

	fn get(&self, key: &str, hidden: Hidden) -> Result<Option<Value>, EvaluatorError> {
		// Interned once and used twice: jrsonnet's own `get` reaches hidden
		// fields, so skipping them means asking about visibility first.
		let key: IStr = key.into();
		if !hidden.included() && !self.0.has_field(key.clone()) {
			return Ok(None);
		}

		Ok(self.0.get(key)?.map(Value))
	}

	/// Overridden so that a missing field still comes with jrsonnet's suggestions
	/// of fields that were probably meant.
	fn get_or_bail(&self, key: &str, hidden: Hidden) -> Result<Value, EvaluatorError> {
		let key: IStr = key.into();

		// A field that is there but hidden is not the field this caller asked
		// for, and jrsonnet's suggestions would unhelpfully suggest that very
		// field. Checked before the lookup, so the hidden field is not forced
		// just to be thrown away.
		if !hidden.included()
			&& !self.0.has_field(key.clone())
			&& self.0.has_field_include_hidden(key.clone())
		{
			return Err(Error::new(ErrorKind::NoSuchField(key, Vec::new())).into());
		}

		Ok(Value(self.0.get_or_bail(key)?))
	}

	fn values(&self) -> Self::ValuesIter<'_> {
		Box::new(
			self.0
				.iter()
				.map(|(_, value)| value.map(Value).map_err(EvaluatorError::from)),
		)
	}
}
