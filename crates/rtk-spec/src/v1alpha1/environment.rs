use kube::CustomResource;
use rustc_hash::FxHashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::v1alpha1::common::JsonentImplementationOrConfig;

#[derive(CustomResource, Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[kube(group = "tanka.dev", version = "v1alpha1", kind = "Environment")]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSpec {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub api_server: Option<Url>,
	#[serde(default)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub context_names: Vec<Box<str>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub namespace: Option<Box<str>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub diff_strategy: Option<Box<str>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub apply_strategy: Option<Box<str>>,
	#[serde(default)]
	#[serde(skip_serializing_if = "crate::v1alpha1::common::bool_is_false")]
	pub inject_labels: bool,
	#[serde(default)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub tanka_env_label_from_fields: Vec<Box<str>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub resource_defaults: Option<ResourceDefaults>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub expect_versions: Option<ExpectVersions>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub export_jsonnet_implementation: Option<JsonentImplementationOrConfig>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct ExpectVersions {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub tanka: Option<Box<str>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct ResourceDefaults {
	#[serde(default)]
	#[serde(skip_serializing_if = "FxHashMap::is_empty")]
	pub annotations: FxHashMap<Box<str>, Box<str>>,
	#[serde(default)]
	#[serde(skip_serializing_if = "FxHashMap::is_empty")]
	pub labels: FxHashMap<Box<str>, Box<str>>,
}
