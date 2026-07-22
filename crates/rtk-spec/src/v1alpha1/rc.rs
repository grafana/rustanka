use std::fs::File;
use std::io;
use std::path::Path;

use kube::{CustomResource, api::ObjectMeta};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::v1alpha1::common::JsonentImplementationOrConfig;

#[derive(Debug, Error)]
pub enum RcError {
	#[error("the provided tkrc contained multiple documents")]
	ContainedMultipleDocuments,
	#[error("failed to read the provided tkrc: {0}")]
	Io(#[from] io::Error),
	#[error("failed to deserialize the provided tkrc: {0}")]
	Deserialize(#[from] serde_saphyr::Error),
}

#[derive(Clone, CustomResource, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[kube(group = "tanka.dev", version = "v1alpha1", kind = "Rc")]
#[serde(rename_all = "camelCase")]
pub struct RcSpec {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub jsonnet_implementation: Option<JsonentImplementationOrConfig>,
	#[serde(skip_serializing_if = "crate::v1alpha1::common::bool_is_false")]
	pub disable_native_functions: bool,
}

impl Rc {
	pub fn load<P>(path: P) -> Result<Rc, RcError>
	where
		P: AsRef<Path>,
	{
		let mut reader = File::open(path.as_ref())?;
		let mut iterator = serde_saphyr::read::<_, Rc>(&mut reader);
		let Some(result) = iterator.next() else {
			return Ok(Rc::default());
		};
		let deserialized = result?;
		if iterator.next().is_some() {
			return Err(RcError::ContainedMultipleDocuments);
		}
		Ok(deserialized)
	}
}

impl Default for Rc {
	fn default() -> Self {
		Rc {
			metadata: ObjectMeta::default(),
			spec: RcSpec::default(),
		}
	}
}
