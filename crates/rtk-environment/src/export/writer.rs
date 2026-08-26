//! Writing exported manifests to disk.
//!
//! An environment is evaluated, serialized and written all on the one worker
//! thread that picked it up, so writing needs no coordination with any other
//! thread: environments run in parallel, and each writes only its own files.
//!

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rustc_hash::FxHashSet;

use crate::export::Error;

/// A serialized manifest, waiting to be written.
#[derive(Clone, Debug)]
pub(crate) struct File {
	/// Where to write it, relative to the output directory.
	pub(crate) path: PathBuf,
	pub(crate) contents: String,
}

impl File {
	/// Write this file, skipping the write if the contents are already on disk.
	///
	/// Reading first is worth it because most files do not change between
	/// exports, and on network or otherwise slow storage a read beats a write.
	fn write_to(self, path: PathBuf) -> Result<Written, Error> {
		if let Ok(existing) = std::fs::read(&path)
			&& existing == self.contents.as_bytes()
		{
			return Ok(Written {
				path: self.path,
				unchanged: true,
			});
		}

		match std::fs::write(&path, &self.contents) {
			Ok(()) => {}
			// The parent directory is created before the write, so this only
			// happens if something removed it in the meantime.
			Err(error) if error.kind() == ErrorKind::NotFound => {
				if let Some(parent) = path.parent() {
					create_dir_all(parent)?;
				}
				std::fs::write(&path, &self.contents).map_err(|source| Error::Write {
					path: path.clone(),
					source,
				})?;
			}
			Err(source) => return Err(Error::Write { path, source }),
		}

		Ok(Written {
			path: self.path,
			unchanged: false,
		})
	}
}

/// A file that has been dealt with.
#[derive(Clone, Debug)]
pub(crate) struct Written {
	/// Its path, relative to the output directory.
	pub(crate) path: PathBuf,
	/// Whether the file was already byte-identical and left alone.
	pub(crate) unchanged: bool,
}

/// Directories one environment's export has already created, so that a
/// directory shared by many manifests is only created once.
#[derive(Debug, Default)]
pub(crate) struct Directories(FxHashSet<Box<Path>>);

impl Directories {
	/// Write `files` beneath `output_dir`, recording what became of each.
	///
	/// Recorded into the caller's `written` as each file lands, rather than
	/// returned once they all have, so that a failure part way through still
	/// accounts for the ones already on disk. A file left out of the report is
	/// left out of `manifest.json`, and a file the index does not mention is one
	/// `fail-on-conflicts` cannot protect and `replace-envs` will not prune.
	pub(crate) fn write_files(
		&mut self,
		output_dir: &Path,
		files: Vec<File>,
		written: &mut Vec<Written>,
	) -> Result<(), Error> {
		written.reserve(files.len());

		for file in files {
			let path = output_dir.join(&file.path);
			self.ensure_parent(&path)?;
			written.push(file.write_to(path)?);
		}

		Ok(())
	}

	/// Create a file's parent directory, unless this export already has.
	fn ensure_parent(&mut self, path: &Path) -> Result<(), Error> {
		let Some(parent) = path.parent() else {
			return Ok(());
		};
		if self.0.contains(parent) {
			return Ok(());
		}

		create_dir_all(parent)?;
		self.0.insert(parent.into());
		Ok(())
	}
}

/// Create `directory` and its parents, tolerating one that is already there.
///
/// `create_dir_all` reports success for existing directories, but it can still
/// lose a race against another environment creating the same one.
fn create_dir_all(directory: &Path) -> Result<(), Error> {
	match std::fs::create_dir_all(directory) {
		Ok(()) => Ok(()),
		Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
		Err(source) => Err(Error::Write {
			path: directory.into(),
			source,
		}),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn files(count: usize) -> Vec<File> {
		(0..count)
			.map(|index| File {
				path: PathBuf::from(format!("nested/{index}.yaml")),
				contents: format!("index: {index}\n"),
			})
			.collect()
	}

	/// Write and hand back what was written, as the tests want to read it.
	fn write(directories: &mut Directories, output_dir: &Path, files: Vec<File>) -> Vec<Written> {
		let mut written = Vec::new();
		directories
			.write_files(output_dir, files, &mut written)
			.unwrap();
		written
	}

	#[test]
	fn writes_files_into_directories_it_creates() {
		let directory = tempfile::tempdir().unwrap();

		let written = write(&mut Directories::default(), directory.path(), files(8));

		assert_eq!(written.len(), 8);
		assert!(written.iter().all(|written| !written.unchanged));
		assert_eq!(
			std::fs::read_to_string(directory.path().join("nested/3.yaml")).unwrap(),
			"index: 3\n"
		);
	}

	#[test]
	fn leaves_files_that_are_already_what_they_should_be() {
		let directory = tempfile::tempdir().unwrap();
		write(&mut Directories::default(), directory.path(), files(8));

		// A second export finds every file unchanged, and the directory already
		// there.
		let written = write(&mut Directories::default(), directory.path(), files(8));

		assert_eq!(written.len(), 8);
		assert!(written.iter().all(|written| written.unchanged));
	}

	#[test]
	fn overwrites_a_file_whose_contents_have_changed() {
		let directory = tempfile::tempdir().unwrap();
		write(&mut Directories::default(), directory.path(), files(1));

		let changed = vec![File {
			path: PathBuf::from("nested/0.yaml"),
			contents: "index: changed\n".to_owned(),
		}];
		let written = write(&mut Directories::default(), directory.path(), changed);

		assert_eq!(written.len(), 1);
		assert!(!written[0].unchanged);
		assert_eq!(
			std::fs::read_to_string(directory.path().join("nested/0.yaml")).unwrap(),
			"index: changed\n"
		);
	}

	#[test]
	fn creates_each_directory_once_across_calls() {
		let directory = tempfile::tempdir().unwrap();
		let mut directories = Directories::default();

		write(&mut directories, directory.path(), files(2));
		// The second call reuses what the first created, and still writes.
		let written = write(
			&mut directories,
			directory.path(),
			vec![File {
				path: PathBuf::from("nested/deeper/8.yaml"),
				contents: "index: 8\n".to_owned(),
			}],
		);

		assert_eq!(written.len(), 1);
		assert!(directory.path().join("nested/deeper/8.yaml").exists());
	}
}
