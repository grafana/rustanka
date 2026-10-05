use std::fmt;

use rtk_jsonnet_core::{RAW_VALUE_TOKEN, Value as _};
use serde::de::{self, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{
	self, Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
	SerializeTuple, SerializeTupleStruct, SerializeTupleVariant,
};
use serde::{Deserializer, Serializer, forward_to_deserialize_any};
use serde_json::Value as Json;

use crate::core::{Evaluator, EvaluatorError};
use crate::native::{Arguments, Value};

impl de::Error for EvaluatorError {
	fn custom<T: fmt::Display>(message: T) -> Self {
		Self(message.to_string())
	}
}

pub struct ValueDeserializer(pub Value);
impl rtk_jsonnet_core::ValueDeserializer for ValueDeserializer {
	type Evaluator = Evaluator;
	type Value = Value;
	fn new(_: &Evaluator, value: Value) -> Self {
		Self(value)
	}
}

struct Values(std::vec::IntoIter<Json>);
impl<'de> SeqAccess<'de> for Values {
	type Error = EvaluatorError;
	fn next_element_seed<T: DeserializeSeed<'de>>(
		&mut self,
		seed: T,
	) -> Result<Option<T::Value>, Self::Error> {
		self.0
			.next()
			.map(|value| seed.deserialize(ValueDeserializer(Value(value))))
			.transpose()
	}
	fn size_hint(&self) -> Option<usize> {
		Some(self.0.len())
	}
}

struct Fields(std::vec::IntoIter<(String, Json)>, Option<Json>);
impl<'de> MapAccess<'de> for Fields {
	type Error = EvaluatorError;
	fn next_key_seed<K: DeserializeSeed<'de>>(
		&mut self,
		seed: K,
	) -> Result<Option<K::Value>, Self::Error> {
		match self.0.next() {
			Some((key, value)) => {
				self.1 = Some(value);
				seed.deserialize(key.into_deserializer()).map(Some)
			}
			None => Ok(None),
		}
	}
	fn next_value_seed<V: DeserializeSeed<'de>>(
		&mut self,
		seed: V,
	) -> Result<V::Value, Self::Error> {
		seed.deserialize(ValueDeserializer(Value(
			self.1.take().expect("value follows key"),
		)))
	}
	fn size_hint(&self) -> Option<usize> {
		Some(self.0.len())
	}
}

impl<'de> Deserializer<'de> for ValueDeserializer {
	type Error = EvaluatorError;
	fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
		match self.0.0 {
			Json::Null => visitor.visit_unit(),
			Json::Bool(b) => visitor.visit_bool(b),
			Json::Number(n) => {
				if let Some(i) = n.as_i64() {
					visitor.visit_i64(i)
				} else if let Some(u) = n.as_u64() {
					visitor.visit_u64(u)
				} else {
					visitor.visit_f64(n.as_f64().expect("finite JSON number"))
				}
			}
			Json::String(s) => visitor.visit_string(s),
			Json::Array(v) => visitor.visit_seq(Values(v.into_iter())),
			Json::Object(o) => {
				visitor.visit_map(Fields(o.into_iter().collect::<Vec<_>>().into_iter(), None))
			}
		}
	}
	fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
		if self.0.0.is_null() {
			visitor.visit_none()
		} else {
			visitor.visit_some(self)
		}
	}
	fn deserialize_newtype_struct<V: Visitor<'de>>(
		self,
		name: &'static str,
		visitor: V,
	) -> Result<V::Value, Self::Error> {
		if name == RAW_VALUE_TOKEN {
			let _guard = self.0.park();
			visitor.visit_unit()
		} else {
			visitor.visit_newtype_struct(self)
		}
	}
	fn deserialize_enum<V: Visitor<'de>>(
		self,
		_: &'static str,
		_: &'static [&'static str],
		visitor: V,
	) -> Result<V::Value, Self::Error> {
		// serde_json's externally tagged enum representation is either a string
		// or a map with a single key.
		match self.0.0 {
			Json::String(s) => visitor.visit_enum(s.into_deserializer()),
			Json::Object(o) if o.len() == 1 => {
				let (key, value) = o.into_iter().next().expect("one key");
				visitor.visit_enum(serde::de::value::MapAccessDeserializer::new(Fields(
					vec![(key, value)].into_iter(),
					None,
				)))
			}
			_ => Err(de::Error::custom(
				"expected a string or single-key object for enum",
			)),
		}
	}
	forward_to_deserialize_any! { bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf unit unit_struct seq tuple tuple_struct map struct identifier ignored_any }
}

impl<'de> Deserializer<'de> for Arguments {
	type Error = EvaluatorError;
	fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
		visitor.visit_seq(Values(
			self.0
				.into_iter()
				.map(|value| value.0)
				.collect::<Vec<_>>()
				.into_iter(),
		))
	}
	forward_to_deserialize_any! { bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map struct enum identifier ignored_any }
}

pub struct ValueSerializer;
impl rtk_jsonnet_core::ValueSerializer for ValueSerializer {
	type Evaluator = Evaluator;
	type Value = Value;
	fn new(_: &Evaluator) -> Self {
		Self
	}
}

// Delegate ordinary serde data to serde_json's in-memory value serializer;
// intercept the raw-value token to preserve an existing implementation value.
impl Serializer for ValueSerializer {
	type Ok = Value;
	type Error = serde_json::Error;
	type SerializeSeq = Compound<<serde_json::value::Serializer as Serializer>::SerializeSeq>;
	type SerializeTuple = Compound<<serde_json::value::Serializer as Serializer>::SerializeTuple>;
	type SerializeTupleStruct =
		Compound<<serde_json::value::Serializer as Serializer>::SerializeTupleStruct>;
	type SerializeTupleVariant =
		Compound<<serde_json::value::Serializer as Serializer>::SerializeTupleVariant>;
	type SerializeMap = Compound<<serde_json::value::Serializer as Serializer>::SerializeMap>;
	type SerializeStruct = Compound<<serde_json::value::Serializer as Serializer>::SerializeStruct>;
	type SerializeStructVariant =
		Compound<<serde_json::value::Serializer as Serializer>::SerializeStructVariant>;

	fn serialize_bool(self, v: bool) -> Result<Value, Self::Error> {
		Ok(Value(Json::Bool(v)))
	}
	fn serialize_i8(self, v: i8) -> Result<Value, Self::Error> {
		Ok(Value(Json::from(v)))
	}
	fn serialize_i16(self, v: i16) -> Result<Value, Self::Error> {
		Ok(Value(Json::from(v)))
	}
	fn serialize_i32(self, v: i32) -> Result<Value, Self::Error> {
		Ok(Value(Json::from(v)))
	}
	fn serialize_i64(self, v: i64) -> Result<Value, Self::Error> {
		Ok(Value(Json::from(v)))
	}
	fn serialize_i128(self, v: i128) -> Result<Value, Self::Error> {
		serde_json::value::Serializer.serialize_i128(v).map(Value)
	}
	fn serialize_u8(self, v: u8) -> Result<Value, Self::Error> {
		Ok(Value(Json::from(v)))
	}
	fn serialize_u16(self, v: u16) -> Result<Value, Self::Error> {
		Ok(Value(Json::from(v)))
	}
	fn serialize_u32(self, v: u32) -> Result<Value, Self::Error> {
		Ok(Value(Json::from(v)))
	}
	fn serialize_u64(self, v: u64) -> Result<Value, Self::Error> {
		Ok(Value(Json::from(v)))
	}
	fn serialize_u128(self, v: u128) -> Result<Value, Self::Error> {
		serde_json::value::Serializer.serialize_u128(v).map(Value)
	}
	fn serialize_f32(self, v: f32) -> Result<Value, Self::Error> {
		serde_json::value::Serializer.serialize_f32(v).map(Value)
	}
	fn serialize_f64(self, v: f64) -> Result<Value, Self::Error> {
		serde_json::value::Serializer.serialize_f64(v).map(Value)
	}
	fn serialize_char(self, v: char) -> Result<Value, Self::Error> {
		Ok(Value(Json::String(v.to_string())))
	}
	fn serialize_str(self, v: &str) -> Result<Value, Self::Error> {
		Ok(Value(Json::String(v.to_owned())))
	}
	fn serialize_bytes(self, v: &[u8]) -> Result<Value, Self::Error> {
		serde_json::value::Serializer.serialize_bytes(v).map(Value)
	}
	fn serialize_none(self) -> Result<Value, Self::Error> {
		Ok(Value(Json::Null))
	}
	fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<Value, Self::Error> {
		v.serialize(self)
	}
	fn serialize_unit(self) -> Result<Value, Self::Error> {
		Ok(Value(Json::Null))
	}
	fn serialize_unit_struct(self, _: &'static str) -> Result<Value, Self::Error> {
		Ok(Value(Json::Null))
	}
	fn serialize_unit_variant(
		self,
		_: &'static str,
		_: u32,
		variant: &'static str,
	) -> Result<Value, Self::Error> {
		Ok(Value(Json::String(variant.into())))
	}
	fn serialize_newtype_struct<T: Serialize + ?Sized>(
		self,
		name: &'static str,
		v: &T,
	) -> Result<Value, Self::Error> {
		if name == RAW_VALUE_TOKEN {
			return Value::take_parked()
				.ok_or_else(|| ser::Error::custom("raw value was not parked"));
		}
		v.serialize(self)
	}
	fn serialize_newtype_variant<T: Serialize + ?Sized>(
		self,
		name: &'static str,
		index: u32,
		variant: &'static str,
		v: &T,
	) -> Result<Value, Self::Error> {
		serde_json::value::Serializer
			.serialize_newtype_variant(name, index, variant, v)
			.map(Value)
	}
	fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
		serde_json::value::Serializer
			.serialize_seq(len)
			.map(Compound)
	}
	fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
		serde_json::value::Serializer
			.serialize_tuple(len)
			.map(Compound)
	}
	fn serialize_tuple_struct(
		self,
		name: &'static str,
		len: usize,
	) -> Result<Self::SerializeTupleStruct, Self::Error> {
		serde_json::value::Serializer
			.serialize_tuple_struct(name, len)
			.map(Compound)
	}
	fn serialize_tuple_variant(
		self,
		name: &'static str,
		index: u32,
		variant: &'static str,
		len: usize,
	) -> Result<Self::SerializeTupleVariant, Self::Error> {
		serde_json::value::Serializer
			.serialize_tuple_variant(name, index, variant, len)
			.map(Compound)
	}
	fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
		serde_json::value::Serializer
			.serialize_map(len)
			.map(Compound)
	}
	fn serialize_struct(
		self,
		name: &'static str,
		len: usize,
	) -> Result<Self::SerializeStruct, Self::Error> {
		serde_json::value::Serializer
			.serialize_struct(name, len)
			.map(Compound)
	}
	fn serialize_struct_variant(
		self,
		name: &'static str,
		index: u32,
		variant: &'static str,
		len: usize,
	) -> Result<Self::SerializeStructVariant, Self::Error> {
		serde_json::value::Serializer
			.serialize_struct_variant(name, index, variant, len)
			.map(Compound)
	}
	fn collect_str<T: fmt::Display + ?Sized>(self, value: &T) -> Result<Value, Self::Error> {
		Ok(Value(Json::String(value.to_string())))
	}
}

pub struct Compound<T>(T);
macro_rules! compound {
    ($trait:ident, $method:ident($($arg:ident : $ty:ty),*)) => {
        impl<T: $trait<Ok = Json, Error = serde_json::Error>> $trait for Compound<T> {
            type Ok = Value;
            type Error = serde_json::Error;
            fn $method<V: Serialize + ?Sized>(&mut self, $($arg: $ty,)* value: &V) -> Result<(), Self::Error> { self.0.$method($($arg,)* value) }
            fn end(self) -> Result<Value, Self::Error> { self.0.end().map(Value) }
        }
    };
}
compound!(SerializeSeq, serialize_element());
compound!(SerializeTuple, serialize_element());
compound!(SerializeTupleStruct, serialize_field());
compound!(SerializeTupleVariant, serialize_field());
compound!(SerializeStruct, serialize_field(key: &'static str));
compound!(SerializeStructVariant, serialize_field(key: &'static str));
impl<T: SerializeMap<Ok = Json, Error = serde_json::Error>> SerializeMap for Compound<T> {
	type Ok = Value;
	type Error = serde_json::Error;
	fn serialize_key<K: Serialize + ?Sized>(&mut self, key: &K) -> Result<(), Self::Error> {
		self.0.serialize_key(key)
	}
	fn serialize_value<V: Serialize + ?Sized>(&mut self, value: &V) -> Result<(), Self::Error> {
		self.0.serialize_value(value)
	}
	fn serialize_entry<K: Serialize + ?Sized, V: Serialize + ?Sized>(
		&mut self,
		key: &K,
		value: &V,
	) -> Result<(), Self::Error> {
		self.0.serialize_entry(key, value)
	}
	fn end(self) -> Result<Value, Self::Error> {
		self.0.end().map(Value)
	}
}
