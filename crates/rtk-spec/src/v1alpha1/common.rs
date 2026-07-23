use std::borrow::Cow;
use std::error::Error;
use std::fmt::{self, Formatter};
use std::path::PathBuf;
use std::str::FromStr;

use k8s_openapi::DeepMerge;
use rustc_hash::FxHashMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::merge_strategies;

/// A specified major version of `helm`- ie `v3`/`v4`.
#[derive(Clone, Copy, Debug, Default)]
pub enum HelmVersion {
    #[default]
    V3,
    V4,
}

impl DeepMerge for HelmVersion {
    fn merge_from(&mut self, other: Self) { *self = other; }
}

impl<'de> Deserialize<'de> for HelmVersion {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		struct HelmVersionVisitor;

		impl<'de> Visitor<'de> for HelmVersionVisitor {
			type Value = HelmVersion;

			fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
				write!(formatter, "a valid helm version")
			}

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                match v {
                    3 => Ok(HelmVersion::V3),
                    4 => Ok(HelmVersion::V4),
                    1|2 => Err(E::custom(format!("helm v{v} is not supported"))),
                    _ => Err(E::custom(format!("{v} is not a valid helm version"))),
                }
            }

			fn visit_str<E>(self, string: &str) -> Result<Self::Value, E>
			where
				E: de::Error,
			{
				string.parse::<HelmVersion>().map_err(E::custom)
			}
		}

		deserializer.deserialize_str(HelmVersionVisitor)
	}
}

impl FromStr for HelmVersion {
	type Err = HelmVersionFromStrError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
            "3"|"v3"|"V3" => Ok(HelmVersion::V3),
            "4"|"v4"|"V4" => Ok(HelmVersion::V4),
            _ => Err(HelmVersionFromStrError(s.into())),
		}
	}
}

impl JsonSchema for HelmVersion {
	fn schema_id() -> Cow<'static, str> {
		Cow::Borrowed("HelmVersion")
	}

	fn schema_name() -> Cow<'static, str> {
		Cow::Borrowed(concat!(module_path!(), "::HelmVersion").into())
	}

	fn json_schema(_: &mut SchemaGenerator) -> Schema {
		json_schema!({"enum": ["v3", "v4"]})
	}
}

impl Serialize for HelmVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {   
		match self {
			HelmVersion::V3 => serializer.serialize_str("v3"),
            HelmVersion::V4 => serializer.serialize_str("v4"),
		}
    }
}

/// The error returned by `<HelmVersion as FromStr>::Err`.
#[derive(Clone, Debug)]
pub struct HelmVersionFromStrError(Box<str>);

impl fmt::Display for HelmVersionFromStrError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match &*self.0 {
            "1"|"v1"|"V1" => formatter.write_str("v1 helm is not supported"),
            "2"|"v2"|"V2" => formatter.write_str("v2 helm is not supported"),
            _ => write!(formatter, "{:?} is not a valid helm version", self.0),
        }
    }
}

impl Error for HelmVersionFromStrError { }

/// Flags passed to a specified [`JsonnetImplementation`].
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct JsonentImplementationFlags(pub FxHashMap<Box<str>, Box<str>>);

impl DeepMerge for JsonentImplementationFlags {
    fn merge_from(&mut self, other: Self) {
        merge_strategies::hashmap::granular(&mut self.0, other.0, |a, b| *a = b);
    }
}

/// A specified preferred Jsonnet implementation for this [`Enviornment`] or
/// project.
/// 
/// The Jsonnet engine has the option to ignore this preference if it cannot
/// provide the specified implementation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum JsonnetImplementation {
	Reference,
	GoJsonnet,
	#[default]
	Jrsonnet,
	Binary(PathBuf),
}

impl DeepMerge for JsonnetImplementation {
    fn merge_from(&mut self, other: Self) { *self = other; }
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
					"enum": ["reference", "go-jsonnet", "jrsonnet"],
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

/// The error returned by `<JsonnetImplementation as FromStr>::Err`.
#[derive(Clone, Debug, Error)]
#[error("{0:?} is not a valid jsonnet implementation")]
pub struct JsonnetImplementationFromStrError(Box<str>);

/// A [`JsonnetImplementation`] and configruation for it.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonnetImplementationConfig {
	#[serde(rename = "type")]
	pub type_: JsonnetImplementation,
	pub flags: JsonentImplementationFlags,
}

impl DeepMerge for JsonnetImplementationConfig {
    fn merge_from(&mut self, other: Self) {
        self.type_.merge_from(other.type_);
        self.flags.merge_from(other.flags);
    }
}

/// A helper type that can de/serialize as either a [`JsonnetImplementation`] or
/// a [`JsonnetImplementationConfig`].
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(untagged)]
pub enum JsonentImplementationOrConfig {
	JsonnetImplementation(JsonnetImplementation),
	JsonnetImplementationConfig(JsonnetImplementationConfig),
}

impl JsonentImplementationOrConfig {
    pub fn implementation(&self) -> &JsonnetImplementation {
        match self {
            JsonentImplementationOrConfig::JsonnetImplementation(implementation) => implementation,
            JsonentImplementationOrConfig::JsonnetImplementationConfig(config) => &config.type_,
        }
    }
}

impl DeepMerge for JsonentImplementationOrConfig {
    fn merge_from(&mut self, other: Self) {
        match (self, other) {
            (
                JsonentImplementationOrConfig::JsonnetImplementationConfig(a),
                JsonentImplementationOrConfig::JsonnetImplementationConfig(b)
            ) if a.type_ == b.type_ => a.merge_from(b),
            (a, b) => *a = b,
        }
    }
}

impl Default for JsonentImplementationOrConfig {
    fn default() -> Self {
        JsonentImplementationOrConfig::JsonnetImplementation(JsonnetImplementation::default())
    }
}

/// A strategy used for `kubectl apply` and `kubectl diff`- `client` or `server`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Strategy {
    Client,
    Server,
}

impl DeepMerge for Strategy {
    fn merge_from(&mut self, other: Self) { *self = other; }
}

/// A [`semver::Version`] with a [`JsonSchema`] implementation. 
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Version(pub semver::Version);

impl DeepMerge for Version {
    fn merge_from(&mut self, other: Self) { *self = other; }
}

impl JsonSchema for Version {
	fn schema_id() -> Cow<'static, str> {
		Cow::Borrowed("Version")
	}

	fn schema_name() -> Cow<'static, str> {
		Cow::Borrowed(concat!(module_path!(), "::Version").into())
	}

	fn json_schema(_: &mut SchemaGenerator) -> Schema {
		json_schema!({
            "type": "string",
            "pattern": r#"^(?P<major>0|[1-9]\d*)\.(?P<minor>0|[1-9]\d*)\.(?P<patch>0|[1-9]\d*)(?:-(?P<prerelease>(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+(?P<buildmetadata>[0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$"#,
        })
	}
}

/// A [`semver::VersionReq`] with a [`JsonSchema`] implementation.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VersionReq(pub semver::VersionReq);

impl DeepMerge for VersionReq {
    fn merge_from(&mut self, other: Self) { *self = other; }
}

impl JsonSchema for VersionReq {
	fn schema_id() -> Cow<'static, str> {
		Cow::Borrowed("Version")
	}

	fn schema_name() -> Cow<'static, str> {
		Cow::Borrowed(concat!(module_path!(), "::Version").into())
	}

	fn json_schema(_: &mut SchemaGenerator) -> Schema {
		json_schema!({
            "type": "string",
            "pattern": r#"^(>=|<=|>|<|=|~|\^)?\s*v?(\d+|[xX*])(\.(\d+|[xX*]))?(\.(\d+|[xX*]))?(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$"#,
        })
	}
}

/// A specified set of versions for the utilities used by `rtk`.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Versions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binaries: Option<FxHashMap<PathBuf, VersionReq>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubectl: Option<VersionReq>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub tanka: Option<VersionReq>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub helm: Option<HelmVersion>,
}

impl DeepMerge for Versions {
    fn merge_from(&mut self, other: Self) {
        merge_strategies::hashmap::granular(&mut self.binaries, other.binaries, |a, b| *a = b);
        self.kubectl.merge_from(other.kubectl);
        self.tanka.merge_from(other.tanka);
        self.helm.merge_from(other.helm);
    }
}

#[inline(always)]
pub(crate) fn bool_is_false(bool: &bool) -> bool {
	!bool
}
