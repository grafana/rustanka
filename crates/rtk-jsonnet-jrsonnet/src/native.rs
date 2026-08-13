use jrsonnet_evaluator::val::ArrValue;
use jrsonnet_evaluator::{ObjValue, Thunk, Val};
use rtk_jsonnet_core as jsonnet;

use crate::serde::{ValueDeserializer, ValueSerializer};
use crate::{Evaluator, EvaluatorError};

pub struct Arguments(pub Box<[Option<Thunk<Val>>]>);

impl jsonnet::Arguments for Arguments {
	type Evaluator = Evaluator;
}

#[derive(Clone, Debug)]
pub struct Value(pub Val);

impl jsonnet::Value for Value {
	type Evaluator = Evaluator;

	type Deserializer = ValueDeserializer;
	type Serializer = ValueSerializer;

	type Array = Array;
	type Object = Object;

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
}

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

	fn has(&self, key: &str) -> Result<bool, EvaluatorError> {
		Ok(self.0.has_field(key.into()))
	}

	fn get(&self, key: &str) -> Result<Value, EvaluatorError> {
		Ok(Value(self.0.get_or_bail(key.into())?))
	}

	fn values(&self) -> Self::ValuesIter<'_> {
		Box::new(
			self.0
				.iter()
				.map(|(_, value)| value.map(Value).map_err(EvaluatorError::from)),
		)
	}
}
