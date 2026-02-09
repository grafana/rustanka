//! Common test utilities.

use std::{
	collections::HashMap,
	io::{self, ErrorKind, Write},
	path::PathBuf,
};

use jrsonnet_evaluator::{
	error::{ErrorKind::*, Result as JrsonnetResult},
	AsPathLike, ImportResolver, ResolvePath,
};
use jrsonnet_gcmodule::Acyclic;
use jrsonnet_parser::{SourceFile, SourcePath};

/// A writer that simulates a broken pipe (SIGPIPE scenario).
///
/// This writer immediately returns `ErrorKind::BrokenPipe` on any write attempt,
/// simulating what happens when stdout is connected to a process that has exited.
pub struct BrokenPipeWriter;

impl Write for BrokenPipeWriter {
	fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
		Err(io::Error::new(ErrorKind::BrokenPipe, "broken pipe"))
	}

	fn flush(&mut self) -> io::Result<()> {
		Err(io::Error::new(ErrorKind::BrokenPipe, "broken pipe"))
	}
}

/// An in-memory import resolver for testing.
///
/// Stores files in a HashMap and resolves imports from memory,
/// avoiding the need for filesystem access in tests.
#[derive(Default, Acyclic)]
pub struct MemoryImportResolver {
	files: HashMap<PathBuf, Vec<u8>>,
}

impl MemoryImportResolver {
	pub fn new() -> Self {
		Self::default()
	}

	/// Add a file to the in-memory filesystem.
	pub fn add_file(&mut self, path: impl Into<PathBuf>, content: impl Into<Vec<u8>>) {
		self.files.insert(path.into(), content.into());
	}

	/// Builder-style method to add a file.
	pub fn with_file(mut self, path: impl Into<PathBuf>, content: impl Into<Vec<u8>>) -> Self {
		self.add_file(path, content);
		self
	}
}

impl ImportResolver for MemoryImportResolver {
	fn resolve_from(&self, from: &SourcePath, path: &dyn AsPathLike) -> JrsonnetResult<SourcePath> {
		let base_dir = if let Some(f) = from.downcast_ref::<SourceFile>() {
			f.path().parent().map(|p| p.to_path_buf())
		} else if from.is_default() {
			Some(PathBuf::from("/"))
		} else {
			None
		};

		let base_dir = base_dir.unwrap_or_else(|| PathBuf::from("/"));

		let rp = path.as_path();
		let resolved = base_dir.join(rp.as_ref());
		if self.files.contains_key(&resolved) {
			return Ok(SourcePath::new(SourceFile::new(resolved)));
		}

		let absolute = match rp {
			ResolvePath::Str(s) => PathBuf::from(s),
			ResolvePath::Path(p) => p.to_path_buf(),
		};
		if self.files.contains_key(&absolute) {
			return Ok(SourcePath::new(SourceFile::new(absolute)));
		}

		Err(ImportFileNotFound(from.clone(), path.as_path().to_owned()).into())
	}

	fn load_file_contents(&self, resolved: &SourcePath) -> JrsonnetResult<Vec<u8>> {
		let path = if let Some(f) = resolved.downcast_ref::<SourceFile>() {
			f.path()
		} else {
			return Err(ImportIo(format!("unsupported source path type: {:?}", resolved)).into());
		};

		self.files
			.get(path)
			.cloned()
			.ok_or_else(|| ResolvedFileNotFound(resolved.clone()).into())
	}
}
