use kube::CustomResource;

use rustc_hash::FxHashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::DeepMerge;
use crate::merge_strategies;
use crate::v1alpha1::common::{JsonentImplementationOrConfig, Strategy, Versions};

/// The `spec` of an [`Environment`].
#[derive(CustomResource, Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[kube(
    group = "tanka.dev",
    version = "v1alpha1",
    kind = "Environment",
    derive = "Default",
    doc = "A `CustomResource` representing a Tanka environment.",
    attr = "non_exhaustive",
)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct EnvironmentSpec {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub api_server: Option<Url>,
	#[serde(default)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub context_names: Vec<Box<str>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub namespace: Option<Box<str>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub diff_strategy: Option<Strategy>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub apply_strategy: Option<Strategy>,
	#[serde(default)]
	#[serde(skip_serializing_if = "crate::v1alpha1::common::bool_is_false")]
	pub inject_labels: bool,
	#[serde(default)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub tanka_env_label_from_fields: Vec<Box<str>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub resource_defaults: Option<ResourceDefaults>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub expect_versions: Option<Versions>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub export_jsonnet_implementation: Option<JsonentImplementationOrConfig>,
}

impl DeepMerge for EnvironmentSpec {
    fn merge_from(&mut self, other: Self) {
        if let Some(api_server) = other.api_server {
            self.api_server = Some(api_server);
        }

        merge_strategies::list::set(&mut self.context_names, other.context_names);
        
        if let Some(namespace) = other.namespace {
            self.namespace = Some(namespace);
        }

        self.diff_strategy.merge_from(other.diff_strategy);
        self.apply_strategy.merge_from(other.apply_strategy);

        self.inject_labels = self.inject_labels || other.inject_labels;

        merge_strategies::list::set(&mut self.tanka_env_label_from_fields, other.tanka_env_label_from_fields);

        self.resource_defaults.merge_from(other.resource_defaults);
        self.expect_versions.merge_from(other.expect_versions);
        self.export_jsonnet_implementation.merge_from(other.export_jsonnet_implementation);
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDefaults {
	#[serde(default)]
	#[serde(skip_serializing_if = "FxHashMap::is_empty")]
	pub annotations: FxHashMap<Box<str>, Box<str>>,
	#[serde(default)]
	#[serde(skip_serializing_if = "FxHashMap::is_empty")]
	pub labels: FxHashMap<Box<str>, Box<str>>,
}

impl DeepMerge for ResourceDefaults {
    fn merge_from(&mut self, other: Self) {
        merge_strategies::hashmap::granular(&mut self.annotations, other.annotations, |a, b| *a = b);
        merge_strategies::hashmap::granular(&mut self.labels, other.labels, |a, b| *a = b);
    }
}
