//! Writing exported manifests to disk.
//!
//! An environment is evaluated, serialized and written all on the one worker
//! thread that picked it up, so writing needs no coordination with any other
//! thread: environments run in parallel, and each writes only its own files.

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

/// Write `files` beneath `output_dir`, reporting what became of each.
pub(crate) fn write_files(
	output_dir: &Path,
	files: Vec<File>,
	directories: &mut Directories,
) -> Result<Vec<Written>, Error> {
	let mut written = Vec::with_capacity(files.len());

	for file in files {
		let path = output_dir.join(&file.path);
		directories.ensure_parent(&path)?;
		written.push(write(file, path)?);
	}

	Ok(written)
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

/// Write one file, skipping the write if the contents are already on disk.
///
/// Reading first is worth it because most files do not change between exports,
/// and on network or otherwise slow storage a read beats a write.
fn write(file: File, path: PathBuf) -> Result<Written, Error> {
	if let Ok(existing) = std::fs::read(&path)
		&& existing == file.contents.as_bytes()
	{
		return Ok(Written {
			path: file.path,
			unchanged: true,
		});
	}

	match std::fs::write(&path, &file.contents) {
		Ok(()) => {}
		// The parent directory is created before the write, so this only happens
		// if something removed it in the meantime.
		Err(error) if error.kind() == ErrorKind::NotFound => {
			if let Some(parent) = path.parent() {
				create_dir_all(parent)?;
			}
			std::fs::write(&path, &file.contents).map_err(|source| Error::Write {
				path: path.clone(),
				source,
			})?;
		}
		Err(source) => {
			return Err(Error::Write {
				path: path.clone(),
				source,
			});
		}
	}

	Ok(Written {
		path: file.path,
		unchanged: false,
	})
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

	#[test]
	fn writes_files_into_directories_it_creates() {
		let directory = tempfile::tempdir().unwrap();

		let written = write_files(directory.path(), files(8), &mut Directories::default()).unwrap();

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
		write_files(directory.path(), files(8), &mut Directories::default()).unwrap();

		// A second export finds every file unchanged, and the directory already
		// there.
		let written = write_files(directory.path(), files(8), &mut Directories::default()).unwrap();

		assert_eq!(written.len(), 8);
		assert!(written.iter().all(|written| written.unchanged));
	}

	#[test]
	fn overwrites_a_file_whose_contents_have_changed() {
		let directory = tempfile::tempdir().unwrap();
		write_files(directory.path(), files(1), &mut Directories::default()).unwrap();

		let changed = vec![File {
			path: PathBuf::from("nested/0.yaml"),
			contents: "index: changed\n".to_owned(),
		}];
		let written = write_files(directory.path(), changed, &mut Directories::default()).unwrap();

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

		write_files(directory.path(), files(2), &mut directories).unwrap();
		// The second call reuses what the first created, and still writes.
		let written = write_files(
			directory.path(),
			vec![File {
				path: PathBuf::from("nested/deeper/8.yaml"),
				contents: "index: 8\n".to_owned(),
			}],
			&mut directories,
		)
		.unwrap();

		assert_eq!(written.len(), 1);
		assert!(directory.path().join("nested/deeper/8.yaml").exists());
	}
}
