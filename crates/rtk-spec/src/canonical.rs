use std::fs::File;
use std::io;
use std::path::Path;

use thiserror::Error;

use crate::DeepMergeFrom;

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

impl<'a, D: EnvironmentData<'a>> DeepMergeFrom<Environment<'a, D>> for Rc {
	fn merge_from(&mut self, other: Environment<'a, D>) {
		self.spec.merge_from(other.spec);
	}
}

impl DeepMergeFrom<EnvironmentSpec> for RcSpec {
	fn merge_from(&mut self, other: EnvironmentSpec) {
		if let Some(api_server) = other.api_server {
			self.api_server = Some(api_server);
		}
		if let Some(expect_versions) = other.expect_versions {
			let versions = self.expect_versions.get_or_insert_with(|| Versions {
				binaries: None,
				kubectl: None,
				tanka: None,
				helm: None,
			});
			if let Some(tanka) = expect_versions.tanka {
				versions.tanka = Some(tanka);
			}
		}
		self.jsonnet_implementation
			.merge_from(other.export_jsonnet_implementation);
	}
}
