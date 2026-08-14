//! Writing exported manifests to disk.
//!
//! Serializing happens on worker threads, but writing happens on whichever
//! thread drives the export: file writes are handed to Tokio, which runs them on
//! its blocking pool, so several are in flight at once without the driver having
//! to coordinate anything but their completions.

use std::future::Future;
use std::io::ErrorKind;
use std::panic;
use std::path::{Path, PathBuf};

use rustc_hash::FxHashSet;
use tokio::runtime::{Builder, Handle, RuntimeFlavor};
use tokio::task::JoinSet;

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
	/// Which environment it belongs to, by discovery order.
	pub(crate) index: usize,
	/// Its path, relative to the output directory.
	pub(crate) path: PathBuf,
	/// Whether the file was already byte-identical and left alone.
	pub(crate) unchanged: bool,
}

/// Writes files, a bounded number at a time.
#[derive(Debug)]
pub(crate) struct Writer {
	output_dir: PathBuf,
	/// Directories created (or found) during this export. Only the driver
	/// touches this, so it needs no lock, and creating directories here rather
	/// than inside the write tasks keeps concurrent tasks from racing on the
	/// same one.
	ensured: FxHashSet<Box<Path>>,
	writes: JoinSet<Result<Written, Error>>,
	concurrency: usize,
}

impl Writer {
	pub(crate) fn new(output_dir: PathBuf, concurrency: usize) -> Writer {
		Writer {
			output_dir,
			ensured: FxHashSet::default(),
			writes: JoinSet::new(),
			concurrency: concurrency.max(1),
		}
	}

	pub(crate) fn is_idle(&self) -> bool {
		self.writes.is_empty()
	}

	pub(crate) fn is_saturated(&self) -> bool {
		self.writes.len() >= self.concurrency
	}

	/// Queue `files`, waiting only if too many writes are already in flight.
	pub(crate) async fn write(
		&mut self,
		index: usize,
		files: Vec<File>,
		written: &mut Vec<Written>,
	) -> Result<(), Error> {
		for file in files {
			while self.is_saturated() {
				written.push(self.harvest().await?.expect("writes are in flight"));
			}

			let path = self.output_dir.join(&file.path);
			self.ensure_parent(&path)?;
			self.writes.spawn(write(index, file, path));
		}

		Ok(())
	}

	/// Take one completed write, waiting for one if necessary.
	pub(crate) async fn harvest(&mut self) -> Result<Option<Written>, Error> {
		match self.writes.join_next().await {
			Some(Ok(written)) => written.map(Some),
			Some(Err(join)) => Err(join_error(join)),
			None => Ok(None),
		}
	}

	/// Take a completed write if one is ready, without waiting.
	pub(crate) fn try_harvest(&mut self) -> Option<Result<Written, Error>> {
		match self.writes.try_join_next() {
			Some(Ok(written)) => Some(written),
			Some(Err(join)) => Some(Err(join_error(join))),
			None => None,
		}
	}

	/// Wait for every queued write.
	pub(crate) async fn drain(&mut self, written: &mut Vec<Written>) -> Result<(), Error> {
		while let Some(next) = self.harvest().await? {
			written.push(next);
		}

		Ok(())
	}

	/// Create a file's parent directory, unless this export already has.
	fn ensure_parent(&mut self, path: &Path) -> Result<(), Error> {
		let Some(parent) = path.parent() else {
			return Ok(());
		};
		if self.ensured.contains(parent) {
			return Ok(());
		}

		create_dir_all(parent)?;
		self.ensured.insert(parent.into());
		Ok(())
	}
}

/// Create `directory` and its parents, tolerating one that is already there.
///
/// `create_dir_all` reports success for existing directories, but it can still
/// lose a race against another writer creating the same one.
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
async fn write(index: usize, file: File, path: PathBuf) -> Result<Written, Error> {
	if let Ok(existing) = tokio::fs::read(&path).await
		&& existing == file.contents.as_bytes()
	{
		return Ok(Written {
			index,
			path: file.path,
			unchanged: true,
		});
	}

	match tokio::fs::write(&path, &file.contents).await {
		Ok(()) => {}
		// The parent directory is created before the write is queued, so this
		// only happens if something removed it in the meantime.
		Err(error) if error.kind() == ErrorKind::NotFound => {
			if let Some(parent) = path.parent() {
				create_dir_all(parent)?;
			}
			tokio::fs::write(&path, &file.contents)
				.await
				.map_err(|source| Error::Write {
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
		index,
		path: file.path,
		unchanged: false,
	})
}

fn join_error(join: tokio::task::JoinError) -> Error {
	match join.try_into_panic() {
		Ok(panic) => panic::resume_unwind(panic),
		Err(join) => Error::Write {
			path: PathBuf::new(),
			source: std::io::Error::other(join),
		},
	}
}

/// Run a future that writes files to completion, from synchronous code.
///
/// Reuses the caller's runtime when that is possible, and builds a private one
/// when it is not:
///
/// - on a multi-threaded runtime, [`tokio::task::block_in_place`] hands this
///   thread's work to another worker so the rest of the runtime keeps running;
/// - on a current-thread runtime blocking is not allowed at all, so the future
///   is constructed and driven on a scoped thread of our own;
/// - outside any runtime, a private current-thread runtime drives it here.
///
/// `construct` builds the future rather than the caller passing one in, because
/// an export's future is not [`Send`]: it holds evaluated Jsonnet values, which
/// stay on the thread that produced them. Constructing it inside means it is
/// built on whichever thread ends up driving it.
pub(crate) fn drive<C, F, T>(construct: C) -> T
where
	C: FnOnce() -> F + Send,
	F: Future<Output = T>,
	T: Send,
{
	match Handle::try_current() {
		Ok(handle) if handle.runtime_flavor() != RuntimeFlavor::CurrentThread => {
			tokio::task::block_in_place(|| handle.block_on(construct()))
		}
		Ok(_) => std::thread::scope(|scope| {
			match scope.spawn(|| block_on_private_runtime(construct())).join() {
				Ok(output) => output,
				Err(panic) => panic::resume_unwind(panic),
			}
		}),
		Err(_) => block_on_private_runtime(construct()),
	}
}

fn block_on_private_runtime<F: Future>(future: F) -> F::Output {
	Builder::new_current_thread()
		.enable_all()
		.build()
		.expect("a current-thread runtime can always be built")
		.block_on(future)
}

/// Run blocking work from inside a future, telling Tokio about it when it can do
/// something with that knowledge.
///
/// On a multi-threaded worker this hands the thread's remaining work to another
/// worker for the duration. Everywhere else it just runs `work`: inside a
/// current-thread runtime [`tokio::task::block_in_place`] would panic, and
/// outside a runtime there is nothing to tell.
///
/// Blocking the driver is safe rather than merely announced, because file writes
/// run on Tokio's blocking pool: blocking here delays noticing that they
/// finished, not the writes themselves.
pub(crate) fn blocking<T>(work: impl FnOnce() -> T) -> T {
	match Handle::try_current() {
		Ok(handle) if handle.runtime_flavor() != RuntimeFlavor::CurrentThread => {
			tokio::task::block_in_place(work)
		}
		_ => work(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn blocking_runs_work_outside_a_runtime() {
		assert_eq!(blocking(|| 1 + 1), 2);
	}

	#[test]
	fn blocking_runs_work_on_a_current_thread_runtime() {
		// `block_in_place` panics here, so `blocking` must not reach for it.
		let output = Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
			.block_on(async { blocking(|| 1 + 1) });
		assert_eq!(output, 2);
	}

	#[test]
	fn blocking_hands_off_on_a_multi_thread_runtime() {
		let runtime = Builder::new_multi_thread()
			.worker_threads(2)
			.enable_all()
			.build()
			.unwrap();

		// From a worker thread, where a hand-off actually happens.
		let output = runtime.block_on(async { tokio::spawn(async { blocking(|| 1 + 1) }).await });
		assert_eq!(output.unwrap(), 2);

		// And from `block_on` itself, where it is allowed but does nothing.
		assert_eq!(runtime.block_on(async { blocking(|| 1 + 1) }), 2);
	}

	#[test]
	fn drive_works_outside_a_runtime() {
		assert_eq!(drive(|| async { 1 + 1 }), 2);
	}

	#[test]
	fn drive_works_inside_a_current_thread_runtime() {
		let output = Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
			.block_on(async { drive(|| async { 1 + 1 }) });
		assert_eq!(output, 2);
	}

	#[test]
	fn drive_works_inside_a_multi_thread_runtime() {
		let runtime = Builder::new_multi_thread()
			.worker_threads(2)
			.enable_all()
			.build()
			.unwrap();

		let output =
			runtime.block_on(async { tokio::spawn(async { drive(|| async { 1 + 1 }) }).await });
		assert_eq!(output.unwrap(), 2);
	}

	#[test]
	fn drive_can_write_files_in_every_context() {
		fn export(directory: &Path) -> Result<Vec<Written>, Error> {
			drive(|| async {
				let mut writer = Writer::new(directory.to_path_buf(), 4);
				let mut written = Vec::new();
				writer
					.write(
						0,
						(0..8)
							.map(|index| File {
								path: PathBuf::from(format!("nested/{index}.yaml")),
								contents: format!("index: {index}\n"),
							})
							.collect(),
						&mut written,
					)
					.await?;
				writer.drain(&mut written).await?;
				Ok(written)
			})
		}

		let directory = tempfile::tempdir().unwrap();

		let written = export(directory.path()).unwrap();
		assert_eq!(written.len(), 8);
		assert!(written.iter().all(|written| !written.unchanged));
		assert_eq!(
			std::fs::read_to_string(directory.path().join("nested/3.yaml")).unwrap(),
			"index: 3\n"
		);

		// A second export finds every file unchanged, and the directory already
		// there.
		let written = export(directory.path()).unwrap();
		assert_eq!(written.len(), 8);
		assert!(written.iter().all(|written| written.unchanged));

		// And it all works the same from inside a runtime.
		let runtime = Builder::new_multi_thread()
			.worker_threads(2)
			.enable_all()
			.build()
			.unwrap();
		let path = directory.path().to_path_buf();
		let written = runtime
			.block_on(async move { tokio::spawn(async move { export(&path) }).await.unwrap() })
			.unwrap();
		assert_eq!(written.len(), 8);
	}
}
