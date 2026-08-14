use std::fmt;

use jrsonnet_evaluator::error::ErrorKind;
use jrsonnet_evaluator::typed::ValType;
use jrsonnet_evaluator::val::ArrValue;
use jrsonnet_evaluator::{Error, IBytes, IStr, ObjValue, ObjValueBuilder, Thunk, Val};
use serde::de::value::StringDeserializer;
use serde::de::{
	self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, Unexpected, VariantAccess, Visitor,
};
use serde::{Deserialize, Deserializer, Serialize, forward_to_deserialize_any, ser};

use rtk_jsonnet_core::{RAW_VALUE_TOKEN, Value as _};

use crate::native::{Array, Object};
use crate::{Arguments, Evaluation, Evaluator, EvaluatorError, Value};

/// Required because [`ValueDeserializer`] (and [`Arguments`]) use
/// [`EvaluatorError`] as their serde error type, as demanded by
/// [`rtk_jsonnet_core::ValueDeserializer`].
impl de::Error for EvaluatorError {
	fn custom<T>(message: T) -> Self
	where
		T: fmt::Display,
	{
		EvaluatorError(<Error as de::Error>::custom(message))
	}
}

impl Arguments {
	fn seq_access<'de>(self) -> impl SeqAccess<'de, Error = EvaluatorError> {
		struct ArgumentsSeqAccess(<Box<[Option<Thunk<Val>>]> as IntoIterator>::IntoIter);

		impl<'de> SeqAccess<'de> for ArgumentsSeqAccess {
			type Error = EvaluatorError;

			fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
			where
				T: DeserializeSeed<'de>,
			{
				match self.0.next() {
					Some(Some(thunk)) => match thunk.evaluate() {
						Ok(value) => {
							Ok(Some(T::deserialize(seed, ValueDeserializer(Value(value)))?))
						}
						Err(error) => Err(error.into()),
					},
					Some(None) => Ok(Some(T::deserialize(
						seed,
						ValueDeserializer(Value(Val::Null)),
					)?)),
					None => Ok(None),
				}
			}

			fn size_hint(&self) -> Option<usize> {
				self.0.size_hint().1
			}
		}

		ArgumentsSeqAccess(self.0.into_iter())
	}
}

impl<'de> Deserializer<'de> for Arguments {
	type Error = EvaluatorError;

	fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
	where
		V: Visitor<'de>,
	{
		visitor.visit_seq(self.seq_access())
	}

	fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
	where
		V: Visitor<'de>,
	{
		visitor.visit_seq(self.seq_access())
	}

	forward_to_deserialize_any! {
		bytes bool byte_buf char enum f32 f64 i128 i16 i32 i64 i8 identifier
		ignored_any map newtype_struct option str string struct tuple
		tuple_struct u128 u16 u32 u64 u8 unit unit_struct
	}
}

impl<'de> Deserialize<'de> for Value {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		Val::deserialize(deserializer).map(Value)
	}
}

/// Deserializes Rust values out of a [`Value`] through the serde data model.
///
/// This is the eager path: every thunk a requested field transitively
/// contains is forced, and functions error. Use [`Value`]'s
/// `into_array`/`into_object` navigation to walk a tree lazily instead.
pub struct ValueDeserializer(pub(crate) Value);

impl rtk_jsonnet_core::ValueDeserializer for ValueDeserializer {
	type Evaluator = Evaluator;
	type Value = Value;

	fn new(_: &Evaluator, value: Value) -> Self {
		ValueDeserializer(value)
	}
}

impl<'de> Deserializer<'de> for ValueDeserializer {
	type Error = EvaluatorError;

	fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
	where
		V: Visitor<'de>,
	{
		match self.0.0 {
			Val::Bool(boolean) => visitor.visit_bool(boolean),
			Val::Null => visitor.visit_unit(),
			Val::Str(string) => visitor.visit_string(string.into_flat().to_string()),
			Val::Num(number) => {
				let number = number.get();
				// Mirrors `Val`'s `Serialize` implementation: integral numbers
				// deserialize as integers so integer-typed fields accept them.
				#[expect(clippy::cast_possible_truncation)]
				if number.fract() == 0.0 {
					visitor.visit_i64(number as i64)
				} else {
					visitor.visit_f64(number)
				}
			}
			Val::Arr(array) => visitor.visit_seq(SeqDeserializer::new(array)),
			Val::Obj(object) => visitor.visit_map(MapDeserializer::new(object)),
			Val::Func(_) => Err(de::Error::custom("tried to manifest function")),
		}
	}

	fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
	where
		V: Visitor<'de>,
	{
		match self.0.0 {
			Val::Null => visitor.visit_none(),
			_ => visitor.visit_some(self),
		}
	}

	/// Hands the value over out of band when asked for a
	/// [`RawValue`](rtk_jsonnet_core::RawValue), so it can be captured without
	/// being deserialized (and without forcing anything beneath it).
	fn deserialize_newtype_struct<V>(
		self,
		name: &'static str,
		visitor: V,
	) -> Result<V::Value, Self::Error>
	where
		V: Visitor<'de>,
	{
		if name == RAW_VALUE_TOKEN {
			let _guard = self.0.park();
			return visitor.visit_unit();
		}
		visitor.visit_newtype_struct(self)
	}

	fn deserialize_enum<V>(
		self,
		_: &'static str,
		_: &'static [&'static str],
		visitor: V,
	) -> Result<V::Value, Self::Error>
	where
		V: Visitor<'de>,
	{
		match self.0.0 {
			Val::Str(string) => visitor.visit_enum(EnumDeserializer {
				variant: string.into_flat(),
				value: None,
			}),
			Val::Obj(object) => {
				let Ok([variant]) = <[IStr; 1]>::try_from(object.fields()) else {
					return Err(de::Error::custom("expected an object with a single field"));
				};
				let value = object
					.get(variant.clone())?
					.expect("iterating over fields; the field exists");
				visitor.visit_enum(EnumDeserializer {
					variant,
					value: Some(Value(value)),
				})
			}
			_ => Err(Error::new(ErrorKind::TypeMismatch(
				"enum",
				vec![ValType::Str, ValType::Obj],
				self.0.0.value_type(),
			))
			.into()),
		}
	}

	/// Skips the value without evaluating anything beneath it, unlike
	/// forwarding to [`Deserializer::deserialize_any`], which would force
	/// every thunk the value transitively contains.
	fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
	where
		V: Visitor<'de>,
	{
		visitor.visit_unit()
	}

	forward_to_deserialize_any! {
		bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
		bytes byte_buf unit unit_struct seq tuple tuple_struct map struct
		identifier
	}
}

struct SeqDeserializer {
	array: ArrValue,
	index: usize,
}

impl SeqDeserializer {
	fn new(array: ArrValue) -> Self {
		SeqDeserializer { array, index: 0 }
	}
}

impl<'de> SeqAccess<'de> for SeqDeserializer {
	type Error = EvaluatorError;

	fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
	where
		T: DeserializeSeed<'de>,
	{
		let Some(element) = self.array.get(self.index)? else {
			return Ok(None);
		};
		self.index += 1;
		seed.deserialize(ValueDeserializer(Value(element)))
			.map(Some)
	}

	fn size_hint(&self) -> Option<usize> {
		Some(self.array.len().saturating_sub(self.index))
	}
}

struct MapDeserializer {
	object: ObjValue,
	fields: std::vec::IntoIter<IStr>,
	field: Option<IStr>,
}

impl MapDeserializer {
	fn new(object: ObjValue) -> Self {
		let fields = object.fields().into_iter();
		MapDeserializer {
			object,
			fields,
			field: None,
		}
	}
}

impl<'de> MapAccess<'de> for MapDeserializer {
	type Error = EvaluatorError;

	fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
	where
		K: DeserializeSeed<'de>,
	{
		let Some(field) = self.fields.next() else {
			return Ok(None);
		};
		let key = seed.deserialize(StringDeserializer::<EvaluatorError>::new(field.to_string()))?;
		self.field = Some(field);
		Ok(Some(key))
	}

	fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
	where
		V: DeserializeSeed<'de>,
	{
		let field = self
			.field
			.take()
			.expect("next_value_seed is only called after next_key_seed");
		let value = self
			.object
			.get(field)?
			.expect("iterating over fields; the field exists");
		seed.deserialize(ValueDeserializer(Value(value)))
	}

	fn size_hint(&self) -> Option<usize> {
		Some(self.fields.len())
	}
}

struct EnumDeserializer {
	variant: IStr,
	value: Option<Value>,
}

impl<'de> EnumAccess<'de> for EnumDeserializer {
	type Error = EvaluatorError;
	type Variant = VariantDeserializer;

	fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
	where
		V: DeserializeSeed<'de>,
	{
		let variant = seed.deserialize(StringDeserializer::<EvaluatorError>::new(
			self.variant.to_string(),
		))?;
		Ok((variant, VariantDeserializer { value: self.value }))
	}
}

struct VariantDeserializer {
	value: Option<Value>,
}

impl<'de> VariantAccess<'de> for VariantDeserializer {
	type Error = EvaluatorError;

	fn unit_variant(self) -> Result<(), Self::Error> {
		match self.value {
			None => Ok(()),
			Some(_) => Err(de::Error::invalid_type(
				Unexpected::NewtypeVariant,
				&"unit variant",
			)),
		}
	}

	fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
	where
		T: DeserializeSeed<'de>,
	{
		match self.value {
			Some(value) => seed.deserialize(ValueDeserializer(value)),
			None => Err(de::Error::invalid_type(
				Unexpected::UnitVariant,
				&"newtype variant",
			)),
		}
	}

	fn tuple_variant<V>(self, _: usize, visitor: V) -> Result<V::Value, Self::Error>
	where
		V: Visitor<'de>,
	{
		match self.value {
			Some(value) => ValueDeserializer(value).deserialize_seq(visitor),
			None => Err(de::Error::invalid_type(
				Unexpected::UnitVariant,
				&"tuple variant",
			)),
		}
	}

	fn struct_variant<V>(
		self,
		_: &'static [&'static str],
		visitor: V,
	) -> Result<V::Value, Self::Error>
	where
		V: Visitor<'de>,
	{
		match self.value {
			Some(value) => ValueDeserializer(value).deserialize_map(visitor),
			None => Err(de::Error::invalid_type(
				Unexpected::UnitVariant,
				&"struct variant",
			)),
		}
	}
}

impl Serialize for Value {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: ser::Serializer,
	{
		self.0.serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for Array {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		match Val::deserialize(deserializer)? {
			Val::Arr(array) => Ok(Array(array)),
			value => Err(de::Error::custom(format!(
				"invalid type: {}, expected array",
				value.value_type()
			))),
		}
	}
}

impl Serialize for Array {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: ser::Serializer,
	{
		Val::Arr(self.0.clone()).serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for Object {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		match Val::deserialize(deserializer)? {
			Val::Obj(object) => Ok(Object(object)),
			value => Err(de::Error::custom(format!(
				"invalid type: {}, expected object",
				value.value_type()
			))),
		}
	}
}

impl Serialize for Object {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: ser::Serializer,
	{
		Val::Obj(self.0.clone()).serialize(serializer)
	}
}

impl Serialize for Evaluation {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: ser::Serializer,
	{
		self.with_context(|_| self.value().0.serialize(serializer))
	}
}

pub struct ValueSerializer;

impl ValueSerializer {
	fn wrap_variant(variant: Option<IStr>, value: Val) -> Value {
		let Some(variant) = variant else {
			return Value(value);
		};
		let mut fields = ObjValue::builder_with_capacity(1);
		fields.field(variant).value(value);
		Value(Val::Obj(fields.build()))
	}
}

impl rtk_jsonnet_core::ValueSerializer for ValueSerializer {
	type Evaluator = Evaluator;
	type Value = Value;

	fn new(_: &Self::Evaluator) -> Self {
		ValueSerializer
	}
}

impl ser::Serializer for ValueSerializer {
	type Ok = Value;
	type Error = Error;

	type SerializeSeq = SeqSerializer;
	type SerializeTuple = SeqSerializer;
	type SerializeTupleStruct = SeqSerializer;
	type SerializeTupleVariant = SeqSerializer;
	type SerializeMap = MapSerializer;
	type SerializeStruct = MapSerializer;
	type SerializeStructVariant = MapSerializer;

	fn serialize_bool(self, boolean: bool) -> Result<Value, Error> {
		Ok(Value(Val::Bool(boolean)))
	}

	fn serialize_i8(self, number: i8) -> Result<Value, Error> {
		Ok(Value(Val::num(number)))
	}

	fn serialize_i16(self, number: i16) -> Result<Value, Error> {
		Ok(Value(Val::num(number)))
	}

	fn serialize_i32(self, number: i32) -> Result<Value, Error> {
		Ok(Value(Val::num(number)))
	}

	/// Unlike jrsonnet's own `Val::from_serde`, which stringifies 64-bit
	/// integers, these stay numbers: erroring outside the safe integer range
	/// keeps numbers round-trippable through the [`Deserializer`] above.
	fn serialize_i64(self, number: i64) -> Result<Value, Error> {
		Ok(Value(Val::try_num(number)?))
	}

	fn serialize_u8(self, number: u8) -> Result<Value, Error> {
		Ok(Value(Val::num(number)))
	}

	fn serialize_u16(self, number: u16) -> Result<Value, Error> {
		Ok(Value(Val::num(number)))
	}

	fn serialize_u32(self, number: u32) -> Result<Value, Error> {
		Ok(Value(Val::num(number)))
	}

	fn serialize_u64(self, number: u64) -> Result<Value, Error> {
		Ok(Value(Val::try_num(number)?))
	}

	fn serialize_f32(self, number: f32) -> Result<Value, Error> {
		Ok(Value(Val::try_num(number)?))
	}

	fn serialize_f64(self, number: f64) -> Result<Value, Error> {
		Ok(Value(Val::try_num(number)?))
	}

	fn serialize_char(self, character: char) -> Result<Value, Error> {
		Ok(Value(Val::string(character.to_string())))
	}

	fn serialize_str(self, string: &str) -> Result<Value, Error> {
		Ok(Value(Val::string(string)))
	}

	fn serialize_bytes(self, bytes: &[u8]) -> Result<Value, Error> {
		Ok(Value(Val::arr(IBytes::from(bytes))))
	}

	fn serialize_none(self) -> Result<Value, Error> {
		Ok(Value(Val::Null))
	}

	fn serialize_some<T>(self, value: &T) -> Result<Value, Error>
	where
		T: ?Sized + Serialize,
	{
		value.serialize(self)
	}

	fn serialize_unit(self) -> Result<Value, Error> {
		Ok(Value(Val::Null))
	}

	fn serialize_unit_struct(self, _: &'static str) -> Result<Value, Error> {
		Ok(Value(Val::Null))
	}

	fn serialize_unit_variant(
		self,
		_: &'static str,
		_: u32,
		variant: &'static str,
	) -> Result<Value, Error> {
		Ok(Value(Val::string(variant)))
	}

	/// Takes the value back out of band when serializing a
	/// [`RawValue`](rtk_jsonnet_core::RawValue), so a value that was captured
	/// from an evaluation round-trips unchanged instead of going through the
	/// serde data model.
	fn serialize_newtype_struct<T>(self, name: &'static str, value: &T) -> Result<Value, Error>
	where
		T: ?Sized + Serialize,
	{
		if name == RAW_VALUE_TOKEN
			&& let Some(parked) = Value::take_parked()
		{
			return Ok(parked);
		}
		value.serialize(self)
	}

	fn serialize_newtype_variant<T>(
		self,
		_: &'static str,
		_: u32,
		variant: &'static str,
		value: &T,
	) -> Result<Value, Error>
	where
		T: ?Sized + Serialize,
	{
		let Value(value) = value.serialize(ValueSerializer)?;
		Ok(ValueSerializer::wrap_variant(Some(variant.into()), value))
	}

	fn serialize_seq(self, length: Option<usize>) -> Result<Self::SerializeSeq, Error> {
		Ok(SeqSerializer {
			variant: None,
			elements: Vec::with_capacity(length.unwrap_or_default()),
		})
	}

	fn serialize_tuple(self, length: usize) -> Result<Self::SerializeTuple, Error> {
		self.serialize_seq(Some(length))
	}

	fn serialize_tuple_struct(
		self,
		_: &'static str,
		length: usize,
	) -> Result<Self::SerializeTupleStruct, Error> {
		self.serialize_seq(Some(length))
	}

	fn serialize_tuple_variant(
		self,
		_: &'static str,
		_: u32,
		variant: &'static str,
		length: usize,
	) -> Result<Self::SerializeTupleVariant, Error> {
		Ok(SeqSerializer {
			variant: Some(variant.into()),
			elements: Vec::with_capacity(length),
		})
	}

	fn serialize_map(self, length: Option<usize>) -> Result<Self::SerializeMap, Error> {
		Ok(MapSerializer {
			variant: None,
			fields: ObjValue::builder_with_capacity(length.unwrap_or_default()),
			field: None,
		})
	}

	fn serialize_struct(
		self,
		_: &'static str,
		length: usize,
	) -> Result<Self::SerializeStruct, Error> {
		self.serialize_map(Some(length))
	}

	fn serialize_struct_variant(
		self,
		_: &'static str,
		_: u32,
		variant: &'static str,
		length: usize,
	) -> Result<Self::SerializeStructVariant, Error> {
		Ok(MapSerializer {
			variant: Some(variant.into()),
			fields: ObjValue::builder_with_capacity(length),
			field: None,
		})
	}
}

pub struct SeqSerializer {
	variant: Option<IStr>,
	elements: Vec<Val>,
}

impl ser::SerializeSeq for SeqSerializer {
	type Ok = Value;
	type Error = Error;

	fn serialize_element<T>(&mut self, element: &T) -> Result<(), Error>
	where
		T: ?Sized + Serialize,
	{
		let Value(element) = element.serialize(ValueSerializer)?;
		self.elements.push(element);
		Ok(())
	}

	fn end(self) -> Result<Value, Error> {
		Ok(ValueSerializer::wrap_variant(
			self.variant,
			Val::arr(self.elements),
		))
	}
}

impl ser::SerializeTuple for SeqSerializer {
	type Ok = Value;
	type Error = Error;

	fn serialize_element<T>(&mut self, element: &T) -> Result<(), Error>
	where
		T: ?Sized + Serialize,
	{
		ser::SerializeSeq::serialize_element(self, element)
	}

	fn end(self) -> Result<Value, Error> {
		ser::SerializeSeq::end(self)
	}
}

impl ser::SerializeTupleStruct for SeqSerializer {
	type Ok = Value;
	type Error = Error;

	fn serialize_field<T>(&mut self, field: &T) -> Result<(), Error>
	where
		T: ?Sized + Serialize,
	{
		ser::SerializeSeq::serialize_element(self, field)
	}

	fn end(self) -> Result<Value, Error> {
		ser::SerializeSeq::end(self)
	}
}

impl ser::SerializeTupleVariant for SeqSerializer {
	type Ok = Value;
	type Error = Error;

	fn serialize_field<T>(&mut self, field: &T) -> Result<(), Error>
	where
		T: ?Sized + Serialize,
	{
		ser::SerializeSeq::serialize_element(self, field)
	}

	fn end(self) -> Result<Value, Error> {
		ser::SerializeSeq::end(self)
	}
}

pub struct MapSerializer {
	variant: Option<IStr>,
	fields: ObjValueBuilder,
	field: Option<IStr>,
}

impl ser::SerializeMap for MapSerializer {
	type Ok = Value;
	type Error = Error;

	fn serialize_key<T>(&mut self, key: &T) -> Result<(), Error>
	where
		T: ?Sized + Serialize,
	{
		let Value(key) = key.serialize(ValueSerializer)?;
		self.field = Some(key.to_string()?);
		Ok(())
	}

	fn serialize_value<T>(&mut self, value: &T) -> Result<(), Error>
	where
		T: ?Sized + Serialize,
	{
		let field = self
			.field
			.take()
			.expect("serialize_value is only called after serialize_key");
		let Value(value) = value.serialize(ValueSerializer)?;
		self.fields.field(field).try_value(value)?;
		Ok(())
	}

	fn end(self) -> Result<Value, Error> {
		Ok(ValueSerializer::wrap_variant(
			self.variant,
			Val::Obj(self.fields.build()),
		))
	}
}

impl ser::SerializeStruct for MapSerializer {
	type Ok = Value;
	type Error = Error;

	fn serialize_field<T>(&mut self, field: &'static str, value: &T) -> Result<(), Error>
	where
		T: ?Sized + Serialize,
	{
		ser::SerializeMap::serialize_entry(self, field, value)
	}

	fn end(self) -> Result<Value, Error> {
		ser::SerializeMap::end(self)
	}
}

impl ser::SerializeStructVariant for MapSerializer {
	type Ok = Value;
	type Error = Error;

	fn serialize_field<T>(&mut self, field: &'static str, value: &T) -> Result<(), Error>
	where
		T: ?Sized + Serialize,
	{
		ser::SerializeMap::serialize_entry(self, field, value)
	}

	fn end(self) -> Result<Value, Error> {
		ser::SerializeMap::end(self)
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use jrsonnet_evaluator::val::StrValue;
	use jrsonnet_evaluator::{State, Val};
	use serde::{Deserialize, Serialize};

	use crate::serde::ValueDeserializer;
	use crate::{EvaluatorError, Value, ValueSerializer};

	fn eval(snippet: &str) -> Val {
		State::builder()
			.build()
			.evaluate_snippet("test", snippet)
			.expect("snippet evaluates")
	}

	fn from_val<'de, T: Deserialize<'de>>(snippet: &str) -> Result<T, EvaluatorError> {
		T::deserialize(ValueDeserializer(Value(eval(snippet))))
	}

	fn to_val<T: Serialize>(value: &T) -> Result<Val, jrsonnet_evaluator::Error> {
		value.serialize(ValueSerializer).map(|value| value.0)
	}

	#[derive(Debug, Deserialize, PartialEq, Serialize)]
	#[serde(rename_all = "camelCase")]
	struct Fixture {
		name: String,
		count: u32,
		ratio: f64,
		enabled: bool,
		#[serde(default)]
		missing: Option<String>,
		present: Option<String>,
		tags: Vec<String>,
		nested: Nested,
		mapping: BTreeMap<String, i64>,
	}

	#[derive(Debug, Deserialize, PartialEq, Serialize)]
	struct Nested {
		value: f64,
	}

	#[derive(Debug, Deserialize, PartialEq, Serialize)]
	#[serde(rename_all = "kebab-case")]
	enum UnitEnum {
		GoJsonnet,
		Jrsonnet,
	}

	#[derive(Debug, Deserialize, PartialEq, Serialize)]
	#[serde(rename_all = "lowercase")]
	enum TaggedEnum {
		Str(String),
		Pair(u8, u8),
		Obj { a: bool },
	}

	fn fixture() -> Fixture {
		Fixture {
			name: "rtk!".into(),
			count: 3,
			ratio: 1.5,
			enabled: true,
			missing: None,
			present: Some("yes".into()),
			tags: vec!["a".into(), "b".into()],
			nested: Nested { value: 4.0 },
			mapping: BTreeMap::from([("one".into(), 1), ("two".into(), 2)]),
		}
	}

	#[test]
	fn deserializes_struct() {
		let fixture: Fixture = from_val(
			r#"{
            name: "rtk" + "!",
            count: 2 + 1,
            ratio: 1.5,
            enabled: true,
            present: "yes",
            missing: null,
            tags: ["a", "b"],
            nested: { value: 4 },
            mapping: { one: 1, two: 2 },
            ignored: { err: error "never evaluated" },
        }"#,
		)
		.unwrap();
		assert_eq!(fixture, self::fixture());
	}

	#[test]
	fn skipped_fields_stay_lazy() {
		#[derive(Debug, Deserialize, PartialEq)]
		struct Sparse {
			kept: bool,
		}
		let sparse: Sparse = from_val(
			r#"{
            kept: true,
            expensive: [error "boom"],
        }"#,
		)
		.unwrap();
		assert_eq!(sparse, Sparse { kept: true });
	}

	#[test]
	fn computed_fields_are_evaluated() {
		let mapping: BTreeMap<String, i64> = from_val(r#"{ ["k" + "ey"]: 40 + 2 }"#).unwrap();
		assert_eq!(mapping, BTreeMap::from([("key".into(), 42)]));
	}

	#[test]
	fn hidden_fields_are_not_deserialized() {
		let mapping: BTreeMap<String, i64> = from_val(r"{ visible: 1, hidden:: 2 }").unwrap();
		assert_eq!(mapping, BTreeMap::from([("visible".into(), 1)]));
	}

	#[test]
	fn deserializes_unit_enum_from_string() {
		let value: UnitEnum = from_val(r#""go-jsonnet""#).unwrap();
		assert_eq!(value, UnitEnum::GoJsonnet);
		let value: UnitEnum = from_val(r#""jrsonnet""#).unwrap();
		assert_eq!(value, UnitEnum::Jrsonnet);
	}

	#[test]
	fn deserializes_tagged_enum_variants() {
		let value: TaggedEnum = from_val(r#"{ str: "hi" }"#).unwrap();
		assert_eq!(value, TaggedEnum::Str("hi".into()));
		let value: TaggedEnum = from_val(r"{ pair: [1, 2] }").unwrap();
		assert_eq!(value, TaggedEnum::Pair(1, 2));
		let value: TaggedEnum = from_val(r"{ obj: { a: true } }").unwrap();
		assert_eq!(value, TaggedEnum::Obj { a: true });
	}

	#[test]
	#[expect(clippy::float_cmp)]
	fn integral_numbers_fit_integer_and_float_fields() {
		let value: i64 = from_val("3").unwrap();
		assert_eq!(value, 3);
		let value: f64 = from_val("3").unwrap();
		assert_eq!(value, 3.0);
		let value: u8 = from_val("255").unwrap();
		assert_eq!(value, 255);
	}

	#[test]
	fn type_mismatches_error() {
		assert!(from_val::<i64>("1.5").is_err());
		assert!(from_val::<u8>("256").is_err());
		assert!(from_val::<u32>(r#""nope""#).is_err());
		assert!(from_val::<String>("3").is_err());
		assert!(from_val::<String>("function(x) x").is_err());
	}

	#[test]
	fn null_and_options() {
		let value: Option<String> = from_val("null").unwrap();
		assert_eq!(value, None);
		let value: Option<String> = from_val(r#""set""#).unwrap();
		assert_eq!(value, Some("set".into()));
		let value: () = from_val("null").unwrap();
		assert_eq!(value, ());
	}

	#[test]
	fn evaluation_errors_propagate() {
		#[derive(Debug, Deserialize)]
		struct Failing {
			#[allow(dead_code)]
			bad: String,
		}
		let result: Result<Failing, EvaluatorError> = (|| {
			let val = State::builder()
				.build()
				.evaluate_snippet("test", r#"{ bad: error "boom" }"#)?;
			Failing::deserialize(ValueDeserializer(Value(val)))
		})();
		let error = result.expect_err("field evaluation fails");
		assert!(
			error.to_string().contains("boom"),
			"unexpected error: {error}"
		);
	}

	#[test]
	fn into_object_navigates_lazily() {
		use rtk_jsonnet_core::{Hidden, Object as _, Value as _};

		let value = Value(eval(
			r#"{ ok: 1, boom: error "nope", f: function(x) x, hidden:: 2 }"#,
		));
		let object = value.into_object().expect("an object");
		assert!(object.has("boom", Hidden::Skip).unwrap());
		assert!(!object.has("absent", Hidden::Skip).unwrap());
		// Only the requested field is forced: the error field never fires and
		// the function survives untouched.
		assert_eq!(
			object.get_or_bail("ok", Hidden::Skip).unwrap().0.as_num(),
			Some(1.0)
		);
		assert!(matches!(
			object.get_or_bail("f", Hidden::Skip).unwrap().0,
			Val::Func(_)
		));
		assert!(object.get("boom", Hidden::Skip).is_err());
		// An absent field is `None` rather than an error, unless it is demanded.
		assert!(object.get("absent", Hidden::Skip).unwrap().is_none());
		assert!(object.get_or_bail("absent", Hidden::Skip).is_err());
	}

	#[test]
	fn hidden_fields_are_reached_only_when_asked_for() {
		use rtk_jsonnet_core::{Hidden, Object as _, Value as _};

		let object = Value(eval(r"{ visible: 1, hidden:: 2 }"))
			.into_object()
			.expect("an object");

		// Hiding a field keeps it out of the manifested output, not out of
		// reach, so which of the two a lookup wants has to be said.
		assert!(!object.has("hidden", Hidden::Skip).unwrap());
		assert!(object.has("hidden", Hidden::Include).unwrap());

		assert!(object.get("hidden", Hidden::Skip).unwrap().is_none());
		assert_eq!(
			object
				.get("hidden", Hidden::Include)
				.unwrap()
				.and_then(|hidden| hidden.as_number()),
			Some(2.0)
		);

		// Visible fields are found either way.
		for hidden in [Hidden::Skip, Hidden::Include] {
			assert!(object.get("visible", hidden).unwrap().is_some());
			assert!(object.has("visible", hidden).unwrap());
		}

		// Demanding a hidden field that was asked to be skipped says it is not
		// there, rather than suggesting the very field it just refused.
		let error = object
			.get_or_bail("hidden", Hidden::Skip)
			.expect_err("the field was skipped");
		assert!(
			error.to_string().contains("hidden"),
			"unexpected error: {error}"
		);
		assert!(
			object.get_or_bail("hidden", Hidden::Include).is_ok(),
			"including hidden fields should find it"
		);
	}

	#[test]
	fn into_array_accesses_elements_lazily() {
		use rtk_jsonnet_core::{Array as _, Value as _};

		let value = Value(eval(r#"[1, error "boom", 3]"#));
		let array = value.into_array().expect("an array");
		assert_eq!(array.len(), 3);
		assert!(!array.is_empty());
		assert_eq!(
			array.get(0).unwrap().and_then(|element| element.0.as_num()),
			Some(1.0),
		);
		assert!(array.get(1).is_err());
		assert!(array.get(3).unwrap().is_none());
		let elements: Vec<_> = array.iter().collect();
		assert_eq!(elements.len(), 3);
		assert!(elements[0].is_ok());
		assert!(elements[1].is_err());
		assert!(elements[2].is_ok());
	}

	#[test]
	fn into_conversions_reject_other_types() {
		use rtk_jsonnet_core::Value as _;

		let value = Value(eval("3"));
		let value = value.into_array().expect_err("not an array");
		let value = value.into_object().expect_err("not an object");
		assert_eq!(value.0.as_num(), Some(3.0));
	}

	#[test]
	fn array_and_object_round_trip_serde() {
		use rtk_jsonnet_core::{Array as _, Object as _};

		use crate::{Array, Object};

		let array = Array::deserialize(ValueDeserializer(Value(eval("[1, 2]")))).unwrap();
		assert_eq!(array.len(), 2);
		assert_eq!(serde_json::to_string(&array).unwrap(), "[1,2]");
		assert!(Array::deserialize(ValueDeserializer(Value(eval("{}")))).is_err());

		let object = Object::deserialize(ValueDeserializer(Value(eval("{ a: 1 }")))).unwrap();
		assert!(object.has("a", rtk_jsonnet_core::Hidden::Skip).unwrap());
		assert_eq!(serde_json::to_string(&object).unwrap(), r#"{"a":1}"#);
		assert!(Object::deserialize(ValueDeserializer(Value(eval("[]")))).is_err());
	}

	#[test]
	fn deserializes_val_from_serde_data() {
		let value =
			Value::deserialize(ValueDeserializer(Value(eval(r#"{ a: [1, "two", null] }"#))))
				.unwrap();
		let obj = value.0.as_obj().expect("an object");
		assert_eq!(obj.fields().len(), 1);
	}

	#[test]
	fn deserializes_inline_environment() {
		let environment: rtk_spec::v1alpha1::Environment = from_val(
			r#"{
            apiVersion: "tanka.dev/v1alpha1",
            kind: "Environment",
            metadata: { name: "environments/demo", labels: { team: "platform" } },
            spec: {
                apiServer: "https://127.0.0.1:6443",
                namespace: "demo",
                injectLabels: true,
            },
            data: { never: error "data must stay lazy" },
        }"#,
		)
		.unwrap();
		assert_eq!(
			environment.metadata.name.as_deref(),
			Some("environments/demo")
		);
		assert_eq!(environment.spec.namespace.as_deref(), Some("demo"));
		assert!(environment.spec.inject_labels);
		let api_server = environment
			.spec
			.api_server
			.as_ref()
			.expect("apiServer is set");
		assert_eq!(api_server.as_str(), "https://127.0.0.1:6443/");
	}

	fn str_of(value: Option<Val>) -> Option<jrsonnet_evaluator::IStr> {
		value
			.as_ref()
			.and_then(Val::as_str)
			.map(StrValue::into_flat)
	}

	#[test]
	fn serializes_struct_to_object() {
		let value = to_val(&fixture()).unwrap();
		let object = value.as_obj().expect("an object");
		assert_eq!(
			str_of(object.get("name".into()).unwrap()),
			Some("rtk!".into())
		);
		assert_eq!(
			object.get("count".into()).unwrap().and_then(|v| v.as_num()),
			Some(3.0),
		);
		// `missing: None` serializes as an explicit null field
		assert!(matches!(
			object.get("missing".into()).unwrap(),
			Some(Val::Null),
		));
	}

	#[test]
	fn round_trips_through_serde() {
		let fixture = fixture();
		let value = to_val(&fixture).unwrap();
		assert_eq!(
			Fixture::deserialize(ValueDeserializer(Value(value))).unwrap(),
			fixture
		);

		for variant in [
			TaggedEnum::Str("hi".into()),
			TaggedEnum::Pair(1, 2),
			TaggedEnum::Obj { a: true },
		] {
			let value = to_val(&variant).unwrap();
			assert_eq!(
				TaggedEnum::deserialize(ValueDeserializer(Value(value))).unwrap(),
				variant
			);
		}
	}

	#[test]
	fn sixty_four_bit_integers_stay_numbers() {
		let value = to_val(&42i64).unwrap();
		assert_eq!(value.as_num(), Some(42.0));
		let value = to_val(&42u64).unwrap();
		assert_eq!(value.as_num(), Some(42.0));
		// beyond the safe integer range is an error instead of silent precision loss
		assert!(to_val(&u64::MAX).is_err());
		assert!(to_val(&i64::MIN).is_err());
	}

	#[test]
	fn non_finite_floats_error() {
		assert!(to_val(&f64::NAN).is_err());
		assert!(to_val(&f64::INFINITY).is_err());
	}

	#[test]
	fn serializes_unit_enum_to_string() {
		let value = to_val(&UnitEnum::GoJsonnet).unwrap();
		assert_eq!(str_of(Some(value)), Some("go-jsonnet".into()));
	}

	#[test]
	fn serializes_integer_map_keys_as_strings() {
		let value = to_val(&BTreeMap::from([(1, "one"), (2, "two")])).unwrap();
		let object = value.as_obj().expect("an object");
		assert_eq!(str_of(object.get("1".into()).unwrap()), Some("one".into()));
	}

	#[test]
	fn serializes_through_evaluator_wiring() {
		use rtk_jsonnet_core::{Context as _, Implementation as _};

		let implementation = crate::Implementation::new(std::iter::empty()).unwrap();
		let evaluator = implementation.create_evaluator();
		let value = fixture().serialize(evaluator.create_serializer()).unwrap();
		assert_eq!(
			Fixture::deserialize(evaluator.create_deserializer(value)).unwrap(),
			fixture()
		);
	}

	#[test]
	fn serializes_environment_to_val() {
		let environment: rtk_spec::v1alpha1::Environment = from_val(
			r#"{
				apiVersion: "tanka.dev/v1alpha1",
				kind: "Environment",
				metadata: { name: "environments/demo" },
				spec: { apiServer: "https://127.0.0.1:6443", namespace: "demo" },
			}"#,
		)
		.unwrap();
		let value = to_val(&environment).unwrap();
		let object = value.as_obj().expect("an object");
		let spec = object
			.get("spec".into())
			.unwrap()
			.and_then(|v| v.as_obj())
			.expect("a spec object");
		assert_eq!(
			str_of(spec.get("apiServer".into()).unwrap()),
			Some("https://127.0.0.1:6443/".into()),
		);
		assert_eq!(
			str_of(spec.get("namespace".into()).unwrap()),
			Some("demo".into()),
		);
	}

	#[test]
	fn raw_values_are_captured_without_forcing_them() {
		use rtk_jsonnet_core::RawValue;

		let raw: RawValue<Value> = from_val(r#"{ boom: error "must stay lazy", ok: 1 }"#).unwrap();
		let object = raw.get().0.as_obj().expect("an object");
		assert_eq!(object.fields().len(), 2);
		// Only what we ask for is forced.
		assert_eq!(
			object.get("ok".into()).unwrap().and_then(|v| v.as_num()),
			Some(1.0)
		);
		assert!(object.get("boom".into()).is_err());
	}

	#[test]
	fn raw_values_round_trip_through_the_value_serializer() {
		use rtk_jsonnet_core::RawValue;

		let raw: RawValue<Value> = from_val(r#"{ a: [1, "two"] }"#).unwrap();
		let value = to_val(&raw).unwrap();
		// The very same value came back, not a copy rebuilt through serde.
		assert_eq!(
			serde_json::to_value(Value(value)).unwrap(),
			serde_json::json!({ "a": [1, "two"] })
		);
	}

	#[test]
	fn raw_values_serialize_to_foreign_serializers_as_their_contents() {
		use rtk_jsonnet_core::RawValue;

		let raw: RawValue<Value> = from_val(r"{ a: 1 }").unwrap();
		assert_eq!(serde_json::to_string(&raw).unwrap(), r#"{"a":1}"#);
		// Nothing is left parked behind by that fallback.
		assert!(<Value as rtk_jsonnet_core::Value>::take_parked().is_none());
	}

	#[test]
	fn raw_values_reject_foreign_deserializers() {
		use rtk_jsonnet_core::RawValue;

		let error = serde_json::from_str::<RawValue<Value>>(r#"{"a":1}"#)
			.expect_err("a json deserializer cannot produce a jsonnet value");
		assert!(
			error.to_string().contains("jsonnet implementation"),
			"unexpected error: {error}"
		);
	}

	#[test]
	fn accessors_read_values_without_deserializing_them() {
		use rtk_jsonnet_core::{Hidden, Object as _, Value as _};

		// The concatenation is past jrsonnet's rope threshold, so `rope` is not a
		// flat string and has to be put together to be read.
		let snippet = format!(
			r#"{{
				string: "flat",
				rope: "{}" + "{}",
				number: 40 + 2,
				fraction: 0.5,
				enabled: true,
				nothing: null,
				array: [1],
				object: {{}},
				boom: error "must stay lazy",
			}}"#,
			"a".repeat(60),
			"b".repeat(60),
		);
		let object = Value(eval(&snippet)).into_object().expect("an object");

		let string = object.get_or_bail("string", Hidden::Skip).unwrap();
		assert_eq!(&*string.as_str().expect("a string"), "flat");
		let rope = object.get_or_bail("rope", Hidden::Skip).unwrap();
		let rope = rope.as_str().expect("a string");
		assert_eq!(rope.len(), 120);
		assert!(rope.starts_with("aaa") && rope.ends_with("bbb"));

		let field = |key: &str| object.get_or_bail(key, Hidden::Skip).unwrap();

		assert_eq!(field("number").as_number(), Some(42.0));
		assert_eq!(field("fraction").as_number(), Some(0.5));
		assert_eq!(field("enabled").as_bool(), Some(true));
		assert!(field("nothing").is_null());
		assert_eq!(
			field("array").as_array().map(|array| {
				use rtk_jsonnet_core::Array as _;
				array.len()
			}),
			Some(1)
		);
		assert!(field("object").as_object().is_some());

		// Reading one kind of value as another says no rather than failing.
		assert!(field("number").as_str().is_none());
		assert!(field("string").as_number().is_none());
		assert!(!field("string").is_null());

		// And none of that forced the field that would have failed.
		assert!(object.get("boom", Hidden::Skip).is_err());
	}

	#[test]
	fn getting_an_absent_field_is_not_an_error() {
		use rtk_jsonnet_core::{Hidden, Object as _, Value as _};

		let object = Value(eval(r"{ present: 1 }"))
			.into_object()
			.expect("an object");

		assert!(object.get("present", Hidden::Skip).unwrap().is_some());
		assert!(object.get("absent", Hidden::Skip).unwrap().is_none());

		// Demanding it says what was probably meant instead.
		let error = object
			.get_or_bail("presnet", Hidden::Skip)
			.expect_err("no such field");
		assert!(
			error.to_string().contains("present"),
			"unexpected error: {error}"
		);
	}

	#[test]
	fn manifests_like_tk_does() {
		use jrsonnet_evaluator::manifest::{JsonFormat, ManifestFormat};
		use rtk_jsonnet_core::Value as _;

		// Integral floats beyond i64 are the case the serde data model cannot
		// represent, and the reason manifests go through `manifest_into`.
		let snippet = r"{ big: 1e100, negativeZero: -0, ratio: 0.1, whole: 3.0 }";
		let value = Value(eval(snippet));

		let mut buffer = String::from("keeps existing contents: ");
		value.manifest_into(&mut buffer).unwrap();
		let (prefix, manifested) = buffer.split_at("keeps existing contents: ".len());
		assert_eq!(prefix, "keeps existing contents: ");
		assert_eq!(
			manifested,
			JsonFormat::default().manifest(&eval(snippet)).unwrap()
		);

		let parsed: serde_json::Value = serde_json::from_str(manifested).unwrap();
		assert_eq!(parsed["big"].as_f64(), Some(1e100));
		assert_eq!(parsed["whole"].as_i64(), Some(3));
		assert_eq!(parsed["ratio"].as_f64(), Some(0.1));
	}
}
