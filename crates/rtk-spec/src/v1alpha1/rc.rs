use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::v1alpha1::common::{JsonentImplementationOrConfig, Versions};

#[derive(Clone, CustomResource, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "tanka.dev",
    version = "v1alpha1",
    kind = "Rc",
    plural = "rc",
    derive = "Default",
    doc = "A `CustomResource` representing a Tanka project configuration.",
)]
#[serde(rename_all = "camelCase")]
pub struct RcSpec {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub api_server: Option<Url>,
	#[serde(skip_serializing_if = "crate::v1alpha1::common::bool_is_false")]
	pub disable_native_functions: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect_versions: Option<Versions>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub jsonnet_implementation: Option<JsonentImplementationOrConfig>,
}
