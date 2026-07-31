use jrsonnet_evaluator::val::ArrValue;
use jrsonnet_evaluator::{IStr, ObjValue, Thunk, Val};

use crate::serde::{ValueDeserializer, ValueSerializer};
use crate::{Evaluator, EvaluatorError};

pub struct Arguments<'a>(pub &'a [Option<Thunk<Val>>]);

impl<'a, 'b> rtk_jsonnet_core::Arguments<'a, 'b> for Arguments<'b> {
	type Evaluator = Evaluator;
}

#[derive(Clone, Debug)]
pub struct Value(pub Val);

impl<'a> rtk_jsonnet_core::Value<'a> for Value {
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
			Val::Obj(object) => Ok(Object::new(object)),
			value => Err(Value(value)),
		}
	}
}

#[derive(Clone, Debug)]
pub struct Array(pub ArrValue);

impl<'a> rtk_jsonnet_core::Array<'a> for Array {
	type Evaluator = Evaluator;
	type Value = Value;

	fn len(&self) -> usize {
		self.0.len()
	}

	fn get(&self, index: usize) -> Result<Option<Value>, EvaluatorError> {
		Ok(self.0.get(index)?.map(Value))
	}

	fn iter(&self) -> impl Iterator<Item = Result<Value, EvaluatorError>> {
		self.0
			.iter()
			.map(|element| element.map(Value).map_err(EvaluatorError::from))
	}
}

/// The visible field names are computed once at construction so
/// [`rtk_jsonnet_core::Object::fields`] can lend `&str`s out of `&self`.
#[derive(Clone, Debug)]
pub struct Object {
	pub(crate) inner: ObjValue,
	fields: Vec<IStr>,
}

impl Object {
	pub(crate) fn new(object: ObjValue) -> Self {
		Object {
			fields: object.fields(),
			inner: object,
		}
	}
}

impl<'a> rtk_jsonnet_core::Object<'a> for Object {
	type Evaluator = Evaluator;
	type Value = Value;

	fn fields(&self) -> impl Iterator<Item = &str> {
		self.fields.iter().map(|field| &**field)
	}

	fn has(&self, key: &str) -> Result<bool, EvaluatorError> {
		Ok(self.inner.has_field(key.into()))
	}

	fn get(&self, key: &str) -> Result<Value, EvaluatorError> {
		Ok(Value(self.inner.get_or_bail(key.into())?))
	}
}
