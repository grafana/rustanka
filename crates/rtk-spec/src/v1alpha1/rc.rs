use crate::v1alpha1::common::{JsonentImplementationOrConfig, Versions, deserialize_api_server};
use k8s_openapi::DeepMerge;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, CustomResource, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[kube(
	group = "tanka.dev",
	version = "v1alpha1",
	kind = "Rc",
	plural = "rc",
	derive = "Default",
	doc = "A `CustomResource` representing a Tanka project configuration."
)]
#[serde(rename_all = "camelCase")]
pub struct RcSpec {
	#[serde(
		default,
		deserialize_with = "deserialize_api_server",
		skip_serializing_if = "Option::is_none"
	)]
	pub api_server: Option<Box<str>>,
	#[serde(skip_serializing_if = "crate::v1alpha1::common::bool_is_false")]
	pub disable_native_functions: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub expect_versions: Option<Versions>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub max_stack_depth: Option<usize>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub jsonnet_implementation: Option<JsonentImplementationOrConfig>,
}

impl DeepMerge for RcSpec {
	fn merge_from(&mut self, other: Self) {
		if let Some(api_server) = other.api_server {
			self.api_server = Some(api_server);
		}

		self.disable_native_functions |= other.disable_native_functions;

		self.expect_versions.merge_from(other.expect_versions);

		self.max_stack_depth = self.max_stack_depth.or(other.max_stack_depth);

		self.jsonnet_implementation
			.merge_from(other.jsonnet_implementation);
	}
}
