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

/// A `tkrc.yaml` as it is written by hand.
///
/// [`Rc`] is declared as a Kubernetes custom resource, so deserializing it
/// directly demands a `metadata` even though a project's settings have no use
/// for one — and demands it while leaving `apiVersion` and `kind` optional,
/// which is a strange thing to ask of a configuration file. Only `spec` is read,
/// so anything else the file carries is ignored and a file holding nothing at
/// all is the default configuration.
#[derive(Default, serde::Deserialize)]
struct RcFile {
	#[serde(default)]
	spec: RcSpec,
}

impl Rc {
	/// Read a project's `tkrc.yaml`.
	///
	/// A missing file is the caller's business; an empty one is the default
	/// configuration, as is a file with no `spec`.
	pub fn load<P>(path: P) -> Result<Rc, RcError>
	where
		P: AsRef<Path>,
	{
		let mut reader = File::open(path.as_ref())?;

		let mut iterator = serde_saphyr::read::<_, RcFile>(&mut reader);

		let Some(result) = iterator.next() else {
			return Ok(Rc::default());
		};
		let deserialized = result?;

		if iterator.next().is_some() {
			return Err(RcError::ContainedMultipleDocuments);
		}

		Ok(Rc {
			spec: deserialized.spec,
			..Rc::default()
		})
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

#[cfg(test)]
mod tests {
	use super::*;

	fn written(contents: &str) -> tempfile::NamedTempFile {
		use std::io::Write as _;
		let mut file = tempfile::NamedTempFile::new().expect("a temporary file");
		file.write_all(contents.as_bytes()).expect("the contents");
		file.flush().expect("flushed");
		file
	}

	/// A `tkrc.yaml` is written by hand, so only `spec` is asked for. It used to
	/// demand a `metadata` — while leaving `apiVersion` and `kind` optional —
	/// because [`Rc`] is declared as a custom resource. Nothing loaded one, so
	/// nobody had met that.
	#[test]
	fn a_project_configuration_needs_nothing_but_its_spec() {
		let file = written("spec:\n  maxStackDepth: 42\n");
		let rc = Rc::load(file.path()).expect("a spec is enough");
		assert_eq!(rc.spec.max_stack_depth, Some(42));
	}

	#[test]
	fn the_custom_resource_shape_is_still_accepted() {
		let file = written(
			"apiVersion: tanka.dev/v1alpha1\nkind: Rc\nmetadata: {}\nspec:\n  maxStackDepth: 7\n",
		);
		let rc = Rc::load(file.path()).expect("the fuller shape reads too");
		assert_eq!(rc.spec.max_stack_depth, Some(7));
	}

	#[test]
	fn a_file_that_says_nothing_is_the_default_configuration() {
		for contents in ["{}\n", "spec: {}\n", ""] {
			let file = written(contents);
			let rc = Rc::load(file.path())
				.unwrap_or_else(|error| panic!("{contents:?} should read: {error}"));
			assert_eq!(rc.spec.max_stack_depth, None, "for {contents:?}");
			assert!(!rc.spec.disable_native_functions, "for {contents:?}");
		}
	}

	/// A single field is enough; the rest are defaulted rather than demanded.
	#[test]
	fn one_setting_does_not_require_the_others() {
		let file = written("spec:\n  disableNativeFunctions: true\n");
		let rc = Rc::load(file.path()).expect("one field is enough");
		assert!(rc.spec.disable_native_functions);
		assert_eq!(rc.spec.max_stack_depth, None);
	}

	#[test]
	fn several_documents_are_refused() {
		let file = written("spec: {}\n---\nspec: {}\n");
		assert!(matches!(
			Rc::load(file.path()),
			Err(RcError::ContainedMultipleDocuments)
		));
	}
}
