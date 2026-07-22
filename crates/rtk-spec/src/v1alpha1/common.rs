use std::borrow::Cow;
use std::fmt::{self, Formatter};
use std::path::PathBuf;
use std::str::FromStr;

use rustc_hash::FxHashMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct JsonentImplementationFlags(pub FxHashMap<Box<str>, Box<str>>);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum JsonnetImplementation {
	Reference,
	#[default]
	GoJsonnet,
	Jrsonnet,
	Binary(PathBuf),
}

impl<'de> Deserialize<'de> for JsonnetImplementation {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		struct JsonnetImplementationVisitor;

		impl<'de> Visitor<'de> for JsonnetImplementationVisitor {
			type Value = JsonnetImplementation;

			fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
				write!(formatter, "a valid jsonnet implementation")
			}

			fn visit_str<E>(self, string: &str) -> Result<Self::Value, E>
			where
				E: de::Error,
			{
				string.parse::<JsonnetImplementation>().map_err(E::custom)
			}
		}

		deserializer.deserialize_str(JsonnetImplementationVisitor)
	}
}

impl FromStr for JsonnetImplementation {
	type Err = JsonnetImplementationFromStrError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"c++" | "cxx" | "reference" => Ok(JsonnetImplementation::Reference),
			"go" | "go-jsonnet" => Ok(JsonnetImplementation::GoJsonnet),
			"jrsonnet" => Ok(JsonnetImplementation::Jrsonnet),
			_ if s.starts_with("binary:") => Ok(JsonnetImplementation::Binary(
				s.trim_start_matches("binary:").into(),
			)),
			_ => Err(JsonnetImplementationFromStrError(s.into())),
		}
	}
}

impl JsonSchema for JsonnetImplementation {
	fn schema_id() -> Cow<'static, str> {
		Cow::Borrowed("JsonnetImplementation")
	}

	fn schema_name() -> Cow<'static, str> {
		Cow::Borrowed(concat!(module_path!(), "::JsonnetImplementation").into())
	}

	fn json_schema(_: &mut SchemaGenerator) -> Schema {
		json_schema!({
			"oneOf": [
				{
					"enum": [
						"c++",
						"cxx",
						"reference",
						"go",
						"go-jsonnet",
						"jrsonnet",
					],
				},
				{
					"type": "string",
					"pattern": "^binary:.*",
				},
			]
		})
	}
}

impl Serialize for JsonnetImplementation {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		match self {
			JsonnetImplementation::Reference => serializer.serialize_str("reference"),
			JsonnetImplementation::GoJsonnet => serializer.serialize_str("go-jsonnet"),
			JsonnetImplementation::Jrsonnet => serializer.serialize_str("jrsonnet"),
			JsonnetImplementation::Binary(binary) => {
				let mut string = binary.to_string_lossy().into_owned();
				string.insert_str(0, "binary:");
				serializer.serialize_str(&string)
			}
		}
	}
}

#[derive(Clone, Debug, Error)]
#[error("{0:?} is not a valid jsonnet implementation")]
pub struct JsonnetImplementationFromStrError(Box<str>);

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonnetImplementationConfig {
	#[serde(rename = "type")]
	pub type_: JsonnetImplementation,
	pub flags: JsonentImplementationFlags,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(untagged)]
pub enum JsonentImplementationOrConfig {
	JsonnetImplementation(JsonnetImplementation),
	JsonnetImplementationConfig(JsonnetImplementationConfig),
}

#[inline(always)]
pub(crate) fn bool_is_false(bool: &bool) -> bool {
	!bool
}
