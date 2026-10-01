use std::thread_local;

use rtk_jsonnet_core as core;
use rtk_jsonnet_core::{Hidden, ParkGuard, TransferSlot};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::core::{Evaluator, EvaluatorError};
use crate::serde::{ValueDeserializer, ValueSerializer};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Value(pub Json);

thread_local! {
	static TRANSFER: TransferSlot<Value> = const { TransferSlot::new() };
}

impl core::Value for Value {
	type Evaluator = Evaluator;
	type Deserializer = ValueDeserializer;
	type Serializer = ValueSerializer;
	type Array = Array;
	type Object = Object;
	type Str = String;

	fn into_array(self) -> Result<Array, Self> {
		match self.0 {
			Json::Array(values) => Ok(Array(values)),
			value => Err(Value(value)),
		}
	}
	fn into_object(self) -> Result<Object, Self> {
		match self.0 {
			Json::Object(values) => Ok(Object(values)),
			value => Err(Value(value)),
		}
	}
	fn as_array(&self) -> Option<Array> {
		self.0.as_array().cloned().map(Array)
	}
	fn as_object(&self) -> Option<Object> {
		self.0.as_object().cloned().map(Object)
	}
	fn as_str(&self) -> Option<String> {
		self.0.as_str().map(str::to_owned)
	}
	fn as_number(&self) -> Option<f64> {
		self.0.as_f64()
	}
	fn as_bool(&self) -> Option<bool> {
		self.0.as_bool()
	}
	fn is_null(&self) -> bool {
		self.0.is_null()
	}
	fn manifest_into(&self, buffer: &mut String) -> Result<(), EvaluatorError> {
		buffer.push_str(&serde_json::to_string(&self.0).map_err(EvaluatorError::from)?);
		Ok(())
	}
	fn park(self) -> ParkGuard<Self> {
		TRANSFER.with(|slot| slot.park(self));
		ParkGuard::new()
	}
	fn take_parked() -> Option<Self> {
		TRANSFER.with(TransferSlot::take)
	}
}

#[derive(Clone, Debug)]
pub struct Array(pub Vec<Json>);
impl From<Array> for Value {
	fn from(array: Array) -> Self {
		Self(Json::Array(array.0))
	}
}
impl core::Array for Array {
	type Evaluator = Evaluator;
	type Value = Value;
	type Iter<'a> =
		std::iter::Map<std::slice::Iter<'a, Json>, fn(&Json) -> Result<Value, EvaluatorError>>;
	fn len(&self) -> usize {
		self.0.len()
	}
	fn get(&self, index: usize) -> Result<Option<Value>, EvaluatorError> {
		Ok(self.0.get(index).cloned().map(Value))
	}
	fn iter(&self) -> Self::Iter<'_> {
		self.0.iter().map(|v| Ok(Value(v.clone())))
	}
}

#[derive(Clone, Debug)]
pub struct Object(pub serde_json::Map<String, Json>);
impl From<Object> for Value {
	fn from(object: Object) -> Self {
		Self(Json::Object(object.0))
	}
}
impl core::Object for Object {
	type Evaluator = Evaluator;
	type Value = Value;
	type ValuesIter<'a> =
		std::iter::Map<serde_json::map::Values<'a>, fn(&Json) -> Result<Value, EvaluatorError>>;
	fn has(&self, key: &str, _: Hidden) -> Result<bool, EvaluatorError> {
		Ok(self.0.contains_key(key))
	}
	fn get(&self, key: &str, _: Hidden) -> Result<Option<Value>, EvaluatorError> {
		Ok(self.0.get(key).cloned().map(Value))
	}
	fn values(&self) -> Self::ValuesIter<'_> {
		self.0.values().map(|v| Ok(Value(v.clone())))
	}
}

pub struct Arguments(pub Vec<Value>);
impl core::Arguments for Arguments {
	type Evaluator = Evaluator;
}
