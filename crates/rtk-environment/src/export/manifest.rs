//! The `manifest.json` that maps exported files back to their environment.
//!
//! Once manifests are spread across a directory tree it is no longer obvious
//! which environment produced which file. Tanka writes this index so that CI (and
//! anyone debugging) can tell, and so that re-exporting an environment can clean
//! up files it no longer produces.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rustc_hash::FxHashSet;
use serde::Serialize;

use crate::export::Error;

/// The name of the index, inside the output directory.
pub(crate) const MANIFEST_FILE: &str = "manifest.json";

/// What to do when the output directory already holds an export.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MergeStrategy {
	/// Fail unless the output directory is empty.
	#[default]
	None,
	/// Export into a non-empty directory, but fail rather than overwrite a file
	/// belonging to another environment.
	FailOnConflicts,
	/// Delete the files the exported environments produced last time, then export
	/// them again.
	ReplaceEnvironments,
}

impl std::str::FromStr for MergeStrategy {
	type Err = InvalidMergeStrategy;

	fn from_str(strategy: &str) -> Result<Self, Self::Err> {
		match strategy {
			"" | "none" => Ok(MergeStrategy::None),
			"fail-on-conflicts" => Ok(MergeStrategy::FailOnConflicts),
			"replace-envs" => Ok(MergeStrategy::ReplaceEnvironments),
			_ => Err(InvalidMergeStrategy {
				strategy: strategy.into(),
			}),
		}
	}
}

#[derive(Debug, thiserror::Error)]
#[error("invalid merge strategy: {strategy}")]
pub struct InvalidMergeStrategy {
	pub strategy: Box<str>,
}

/// The `manifest.json` of a previous export.
#[derive(Debug, Default)]
pub(crate) struct Manifest {
	path: PathBuf,
	/// Exported file (relative, `/`-separated) to the environment that wrote it.
	files: BTreeMap<String, String>,
}

impl Manifest {
	/// Read the index in `output_dir`, treating a missing one as empty.
	pub(crate) fn read(output_dir: &Path) -> Result<Manifest, Error> {
		let path = output_dir.join(MANIFEST_FILE);
		let files = match std::fs::read_to_string(&path) {
			Ok(contents) => serde_json::from_str(&contents)?,
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
			Err(source) => return Err(Error::Write { path, source }),
		};

		Ok(Manifest { path, files })
	}

	/// The environment that exported `file`, if any.
	pub(crate) fn owner(&self, file: &str) -> Option<&str> {
		self.files.get(file).map(String::as_str)
	}

	/// The files exported by any of `environments`.
	///
	/// Environment identifiers are paths, and the same environment can be named
	/// relatively or absolutely, as a directory or as its entrypoint, so this
	/// matches all the spellings tk accepts.
	pub(crate) fn files_of<I, S>(&self, environments: I) -> FxHashSet<String>
	where
		I: IntoIterator<Item = S>,
		S: AsRef<str>,
	{
		let mut names = FxHashSet::default();
		let current_dir = std::env::current_dir().ok();

		for environment in environments {
			let environment = environment.as_ref();
			names.insert(environment.to_owned());

			let path = Path::new(environment);
			if path.is_absolute() {
				if let Some(relative) = current_dir
					.as_ref()
					.and_then(|current_dir| path.strip_prefix(current_dir).ok())
				{
					names.insert(relative.to_string_lossy().into_owned());
				}
			} else if let Some(current_dir) = current_dir.as_ref() {
				names.insert(current_dir.join(path).to_string_lossy().into_owned());
			}
		}

		self.files
			.iter()
			.filter(|(_, owner)| {
				names.contains(owner.as_str())
					|| names.iter().any(|name| {
						// Inline sub-environments are named `<entrypoint>:<name>`.
						owner.starts_with(&format!("{name}:"))
							|| owner.starts_with(&format!(
								"{}:",
								name.trim_end_matches(".jsonnet")
							))
							// And an environment may be named by its directory.
							|| **owner == format!("{name}/main.jsonnet")
							|| **owner == format!("{}/main.jsonnet", name.trim_end_matches('/'))
					})
			})
			.map(|(file, _)| file.clone())
			.collect()
	}

	/// Record the files an environment exported, replacing what it had before.
	pub(crate) fn record<'f, I>(&mut self, environment: &str, files: I)
	where
		I: IntoIterator<Item = &'f Path>,
	{
		for file in files {
			self.files
				.insert(relative_key(file), environment.to_owned());
		}
	}

	/// Forget files that were deleted rather than re-exported.
	pub(crate) fn forget<I, S>(&mut self, files: I)
	where
		I: IntoIterator<Item = S>,
		S: AsRef<str>,
	{
		for file in files {
			self.files.remove(file.as_ref());
		}
	}

	/// Write the index back out, with keys sorted, as tk formats it.
	pub(crate) fn write(&self) -> Result<(), Error> {
		let mut serialized = Vec::new();
		let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
		let mut serializer = serde_json::Serializer::with_formatter(&mut serialized, formatter);
		self.files.serialize(&mut serializer)?;

		// Written through a temporary and renamed into place. A torn
		// `manifest.json` is worse than a stale one: every file in the directory
		// loses its owner at once, and nothing afterwards can tell which
		// environment wrote what.
		let directory = self.path.parent().unwrap_or_else(|| Path::new("."));
		let write = |source| Error::Write {
			path: self.path.clone(),
			source,
		};
		let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(write)?;
		temporary.write_all(&serialized).map_err(write)?;
		temporary.as_file().sync_all().map_err(write)?;
		temporary
			.persist(&self.path)
			.map_err(|error| write(error.error))?;

		Ok(())
	}
}

/// A relative path as `manifest.json` spells it: `/`-separated on every
/// platform.
pub(crate) fn relative_key(path: &Path) -> String {
	path.components()
		.map(|component| component.as_os_str().to_string_lossy())
		.collect::<Vec<_>>()
		.join("/")
}

/// Whether a directory holds nothing (or does not exist).
pub(crate) fn is_empty_dir(directory: &Path) -> Result<bool, Error> {
	match std::fs::read_dir(directory) {
		Ok(mut entries) => Ok(entries.next().is_none()),
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
		Err(source) => Err(Error::Write {
			path: directory.into(),
			source,
		}),
	}
}

/// Delete files a re-export no longer produces, and any directory that leaves
/// empty.
/// Returns the files that are actually gone, which is what the index may forget.
///
/// A file that could not be deleted is still there and still belongs to whoever
/// wrote it, so forgetting it would leave it on disk owned by nobody — the very
/// drift pruning exists to avoid.
pub(crate) fn prune<I, S>(output_dir: &Path, files: I) -> FxHashSet<String>
where
	I: IntoIterator<Item = S>,
	S: AsRef<str>,
{
	let mut removed = FxHashSet::default();

	for file in files {
		let path = output_dir.join(file.as_ref());
		// Best effort: a file that is already gone needs no deleting, and one
		// that cannot be deleted is not worth failing an otherwise good export.
		if let Err(error) = std::fs::remove_file(&path)
			&& error.kind() != std::io::ErrorKind::NotFound
		{
			tracing::warn!(path = ?path, "could not delete {}: {error}", path.display());
			continue;
		}
		removed.insert(file.as_ref().to_owned());

		if let Some(parent) = path.parent()
			&& parent != output_dir
		{
			// Only succeeds while the directory is empty, which is exactly when
			// it should go.
			drop(std::fs::remove_dir(parent));
		}
	}

	removed
}

#[cfg(test)]
mod tests {
	use std::str::FromStr as _;

	use super::*;

	fn index(directory: &Path, files: &[(&str, &str)]) -> Manifest {
		let contents: BTreeMap<&str, &str> = files.iter().copied().collect();
		std::fs::write(
			directory.join(MANIFEST_FILE),
			serde_json::to_string(&contents).expect("valid json"),
		)
		.expect("the index");
		Manifest::read(directory).expect("the index reads")
	}

	#[test]
	fn parses_merge_strategies() {
		assert_eq!(MergeStrategy::from_str("").unwrap(), MergeStrategy::None);
		assert_eq!(
			MergeStrategy::from_str("none").unwrap(),
			MergeStrategy::None
		);
		assert_eq!(
			MergeStrategy::from_str("fail-on-conflicts").unwrap(),
			MergeStrategy::FailOnConflicts
		);
		assert_eq!(
			MergeStrategy::from_str("replace-envs").unwrap(),
			MergeStrategy::ReplaceEnvironments
		);
		assert_eq!(
			MergeStrategy::from_str("nonsense")
				.expect_err("not a strategy")
				.to_string(),
			"invalid merge strategy: nonsense"
		);
	}

	#[test]
	fn a_missing_index_reads_as_empty() {
		let directory = tempfile::tempdir().expect("a temporary directory");
		let index = Manifest::read(directory.path()).expect("a missing index is fine");
		assert!(index.owner("anything").is_none());
	}

	#[test]
	fn finds_the_files_of_an_environment_however_it_is_named() {
		let directory = tempfile::tempdir().expect("a temporary directory");
		let index = index(
			directory.path(),
			&[
				("a.yaml", "environments/demo/main.jsonnet"),
				("b.yaml", "environments/other/main.jsonnet"),
				("c.yaml", "environments/inline/main.jsonnet:dev"),
			],
		);

		// By entrypoint, which is how the index spells them.
		let files = index.files_of(["environments/demo/main.jsonnet"]);
		assert_eq!(files.iter().collect::<Vec<_>>(), vec!["a.yaml"]);

		// By directory.
		let files = index.files_of(["environments/demo"]);
		assert_eq!(files.iter().collect::<Vec<_>>(), vec!["a.yaml"]);

		// An inline sub-environment belongs to its entrypoint.
		let files = index.files_of(["environments/inline/main.jsonnet"]);
		assert_eq!(files.iter().collect::<Vec<_>>(), vec!["c.yaml"]);

		assert!(index.files_of(["environments/absent"]).is_empty());
		assert!(index.files_of(Vec::<String>::new()).is_empty());
	}

	#[test]
	fn records_forgets_and_writes() {
		let directory = tempfile::tempdir().expect("a temporary directory");
		let mut index = index(
			directory.path(),
			&[("gone.yaml", "environments/old/main.jsonnet")],
		);

		index.forget(["gone.yaml"]);
		index.record(
			"environments/demo/main.jsonnet",
			[Path::new("nested/kept.yaml"), Path::new("also.yaml")],
		);
		index.write().expect("the index writes");

		let written = std::fs::read_to_string(directory.path().join(MANIFEST_FILE)).unwrap();
		// Sorted keys, four-space indentation: the format tk writes.
		assert_eq!(
			written,
			"{\n    \"also.yaml\": \"environments/demo/main.jsonnet\",\n    \
			 \"nested/kept.yaml\": \"environments/demo/main.jsonnet\"\n}"
		);
	}

	#[test]
	fn prunes_files_and_the_directories_they_leave_empty() {
		let directory = tempfile::tempdir().expect("a temporary directory");
		let nested = directory.path().join("nested");
		std::fs::create_dir_all(&nested).expect("the directory");
		std::fs::write(nested.join("gone.yaml"), "{}").expect("the file");
		std::fs::write(directory.path().join("kept.yaml"), "{}").expect("the file");

		prune(directory.path(), ["nested/gone.yaml", "never-existed.yaml"]);

		assert!(!nested.exists(), "an emptied directory should go too");
		assert!(directory.path().join("kept.yaml").exists());
	}

	#[test]
	fn reports_empty_directories() {
		let directory = tempfile::tempdir().expect("a temporary directory");
		assert!(is_empty_dir(directory.path()).unwrap());
		assert!(is_empty_dir(&directory.path().join("absent")).unwrap());

		std::fs::write(directory.path().join("file"), "").expect("the file");
		assert!(!is_empty_dir(directory.path()).unwrap());
	}
}
