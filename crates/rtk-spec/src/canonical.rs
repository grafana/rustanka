use std::path::Path;
use std::fs::File;
use std::io;

use thiserror::Error;

use crate::{DeepMerge, DeepMergeFrom};

pub use crate::v1alpha1::*;

#[derive(Debug, Error)]
pub enum RcError {
	#[error("the provided tkrc contained multiple documents")]
	ContainedMultipleDocuments,
	#[error("failed to read the provided tkrc: {0}")]
	Io(#[from] io::Error),
	#[error("failed to deserialize the provided tkrc: {0}")]
	Deserialize(#[from] serde_saphyr::Error),
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

impl DeepMergeFrom<Environment> for Rc {
    fn merge_from(&mut self, other: Environment) {
        DeepMergeFrom::merge_from(&mut self.spec, other.spec);    
    }
}

impl DeepMergeFrom<EnvironmentSpec> for RcSpec {
    fn merge_from(&mut self, other: EnvironmentSpec) {
        if let Some(api_server) = other.api_server {
            self.api_server = Some(api_server);
        }
        self.expect_versions.merge_from(other.expect_versions);
        self.jsonnet_implementation.merge_from(other.export_jsonnet_implementation);
    }
}
