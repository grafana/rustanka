//! importers - Find the environments affected by a set of changed files
//!
//! Given a set of changed (or deleted) files, this module discovers every
//! environment entrypoint (`main.jsonnet`) that transitively imports them.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use rayon::prelude::*;
use thiserror::Error;

use crate::jpath::JPath;
use crate::scan::{PathExt, Source};

#[derive(Debug, Error)]
pub enum Error {
	#[error("failed to resolve {path}")]
	Canonicalize { path: PathBuf, source: io::Error },
	#[error("file {0:?} does not exist")]
	FileDoesNotExist(PathBuf),
	#[error("failed to read {path}")]
	Read { path: PathBuf, source: io::Error },
	#[error("failed to resolve root")]
	Root(#[source] io::Error),
}

/// A file to find importers for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetFile {
	/// A file that exists on disk.
	Existing(PathBuf),
	/// A file that has been deleted; environments that could have imported it
	/// are still reported so they can be re-evaluated.
	Deleted(PathBuf),
}

impl FromStr for TargetFile {
	type Err = Infallible;

	/// Parses the `deleted:` prefix convention used by `tk tool importers`.
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s.strip_prefix("deleted:") {
			Some(deleted) => TargetFile::Deleted(PathBuf::from(deleted)),
			None => TargetFile::Existing(PathBuf::from(s)),
		})
	}
}

/// An index of every jsonnet file under a project root, used to answer
/// "which environments import this file?" queries.
///
/// Building the index scans the whole tree once; queries share its caches, so
/// prefer one index for many lookups over rebuilding per file.
#[derive(Debug)]
pub struct ImporterIndex {
	root: PathBuf,
	vendor: PathBuf,
	lib: PathBuf,
	/// Every jsonnet/libsonnet file under the root, with its scanned imports.
	files: HashMap<PathBuf, JsonnetFile>,
	/// Path -> canonical path, so queries resolve symlinks without syscalls.
	canonical: HashMap<PathBuf, PathBuf>,
}

impl ImporterIndex {
	/// Scan all jsonnet files under `root` and build the index.
	pub fn build(root: impl AsRef<Path>) -> Result<Self, Error> {
		let root = fs::canonicalize(root).map_err(Error::Root)?;

		let scanned = Self::find_jsonnet_files(&root)
			.into_par_iter()
			.map(|path| {
				let content = fs::read_to_string(&path).map_err(|source| Error::Read {
					path: path.clone(),
					source,
				})?;
				let file = JsonnetFile::parse(&path, &content);
				let canonical = path.canonicalize_or_self();
				Ok((path, file, canonical))
			})
			.collect::<Result<Vec<_>, Error>>()?;

		let mut files = HashMap::with_capacity(scanned.len());
		let mut canonical_map = HashMap::with_capacity(scanned.len() * 2);
		for (path, file, canonical) in scanned {
			// Also map canonical -> canonical, for lookups on paths that are
			// already resolved.
			if path != canonical {
				canonical_map.insert(canonical.clone(), canonical.clone());
			}
			canonical_map.insert(path.clone(), canonical);
			files.insert(path, file);
		}

		Ok(ImporterIndex {
			vendor: root.join("vendor"),
			lib: root.join("lib"),
			root,
			files,
			canonical: canonical_map,
		})
	}

	/// Find every environment entrypoint (`main.jsonnet`) that transitively
	/// imports any of the given files, sorted.
	pub fn find_importers(&self, targets: &[TargetFile]) -> Result<Vec<PathBuf>, Error> {
		let mut to_check = Vec::new();
		let mut existing = Vec::new();

		for target in targets {
			match target {
				// Deleted files can't be canonicalized or matched against
				// symlinks; check both their CWD-relative and root-relative
				// interpretations as-is.
				TargetFile::Deleted(path) if path.is_absolute() => to_check.push(path.clone()),
				TargetFile::Deleted(path) => {
					if let Ok(absolute) = fs::canonicalize(path) {
						to_check.push(absolute);
					}
					to_check.push(self.root.join(path));
				}
				TargetFile::Existing(path) => {
					if !path.exists() {
						return Err(Error::FileDoesNotExist(path.clone()));
					}
					existing.push(path.as_path());
				}
			}
		}

		to_check.extend(self.expand_symlinks(&existing)?);

		let mut search = Search {
			index: self,
			memo: HashMap::new(),
		};
		let mut found = HashSet::new();
		for file in &to_check {
			found.insert(file.clone());
			for importer in search.importers_of(file, &mut HashSet::new()) {
				// Map through the canonical cache so symlinked importers
				// deduplicate to a single path.
				let importer = self.canonical.get(&importer).cloned().unwrap_or(importer);
				found.insert(importer);
			}
		}

		let mut entrypoints: Vec<PathBuf> = found
			.into_iter()
			.filter(|path| path.is_entrypoint())
			.collect();
		entrypoints.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
		Ok(entrypoints)
	}

	/// Expand each file to the set of paths it is reachable through: itself,
	/// plus every spelling of the path through a symlink under the root.
	fn expand_symlinks(&self, files: &[&Path]) -> Result<Vec<PathBuf>, Error> {
		if files.is_empty() {
			return Ok(Vec::new());
		}

		let symlink_map = self.symlink_map();
		let mut expanded = HashSet::new();
		for &file in files {
			let absolute = fs::canonicalize(file).map_err(|source| Error::Canonicalize {
				path: file.to_owned(),
				source,
			})?;

			for (target, links) in &symlink_map {
				let Ok(relative) = absolute.strip_prefix(target) else {
					continue;
				};
				for link in links {
					expanded.insert(if relative.as_os_str().is_empty() {
						link.clone()
					} else {
						link.join(relative)
					});
				}
			}
			expanded.insert(absolute);
		}

		Ok(expanded.into_iter().collect())
	}

	/// Whether one of `file`'s imports (or chart references) points at the
	/// target. `target_canonical` must be the canonical form of `target`.
	fn imports_target(
		&self,
		file: &Path,
		contents: &JsonnetFile,
		target: &Path,
		target_canonical: &Path,
		target_basename: &OsStr,
	) -> bool {
		if !contents.chart_dirs.is_empty()
			&& target_canonical.is_chart_relevant_file()
			&& contents
				.chart_dirs
				.iter()
				.any(|dir| target_canonical.starts_with(dir))
		{
			return true;
		}

		let file_dir = file.parent_or_root();

		for import in &contents.imports {
			let import_path = Path::new(import);

			// Cheap pre-filter: the import can only match if the file names
			// agree.
			if import_path.file_name() != Some(target_basename) {
				continue;
			}

			let import_clean: PathBuf = import_path.components().collect();

			if import.starts_with("..") {
				// Relative imports: resolve against the importing file's
				// directory, both in full and with one level of ../ stripped.
				let full = file_dir.clean_join(&import_clean);
				let shallow = import_clean
					.strip_prefix("..")
					.map(|shallow| file_dir.clean_join(shallow));
				if self.paths_match(target_canonical, &full)
					|| shallow.is_ok_and(|shallow| self.paths_match(target_canonical, &shallow))
				{
					return true;
				}
			} else if self.paths_match(target_canonical, &self.vendor.join(&import_clean))
				|| self.paths_match(target_canonical, &self.lib.join(&import_clean))
			{
				// Absolute-style imports resolve against vendor/ and lib/.
				return true;
			}

			// Imports resolved against the environment base directory the
			// importing file belongs to.
			let base = self.find_base(file);
			if target
				.strip_prefix(&base)
				.is_ok_and(|relative| relative == import_path)
			{
				return true;
			}

			// Imports relative to the importing file's directory, e.g. a
			// 'text-file.txt' imported from 'vendor/vendored/main.libsonnet'.
			if self.paths_match(target_canonical, &file_dir.join(import_path)) {
				return true;
			}
		}

		false
	}

	/// Whether the candidate path refers to the same file as the (already
	/// canonical) target path.
	fn paths_match(&self, target_canonical: &Path, candidate: &Path) -> bool {
		if target_canonical == candidate {
			return true;
		}
		if let Some(canonical) = self.canonical.get(candidate) {
			return target_canonical == canonical;
		}
		// Not in the cache (e.g. text files); fall back to a syscall. Most
		// candidates are pre-filtered by basename, so this stays rare.
		fs::canonicalize(candidate).is_ok_and(|canonical| target_canonical == canonical)
	}

	/// Find the environment base directory (nearest ancestor containing a
	/// main.jsonnet) for a file, falling back to the project root.
	fn find_base(&self, file: &Path) -> PathBuf {
		let mut current = if file.is_file() {
			file.parent_or_root()
		} else {
			file
		};

		while current.starts_with(&self.root) {
			if current.join(JPath::DEFAULT_ENTRYPOINT).exists() {
				return current.to_owned();
			}
			match current.parent() {
				Some(parent) => current = parent,
				None => break,
			}
		}

		self.root.clone()
	}

	/// Map every symlink under the root by its canonical target, so files can
	/// be re-expressed through the symlinks that reach them without walking
	/// the tree per file.
	fn symlink_map(&self) -> HashMap<PathBuf, Vec<PathBuf>> {
		let links: Vec<(PathBuf, PathBuf)> = Self::collect_directories(&self.root, 2)
			.par_iter()
			.flat_map_iter(|dir| {
				walkdir::WalkDir::new(dir)
					.follow_links(false)
					.into_iter()
					.filter_map(|entry| {
						let entry = entry.ok()?;
						let path = entry.path();
						if !path.is_symlink() {
							return None;
						}
						let target = fs::read_link(path).ok()?;
						let target = if target.is_absolute() {
							target
						} else {
							path.parent_or_root().join(target)
						};
						Some((fs::canonicalize(target).ok()?, entry.into_path()))
					})
					.collect::<Vec<_>>()
			})
			.collect();

		let mut map: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
		for (target, link) in links {
			map.entry(target).or_default().push(link);
		}
		map
	}

	/// Find all jsonnet/libsonnet files under a directory, in parallel.
	fn find_jsonnet_files(dir: &Path) -> Vec<PathBuf> {
		Self::collect_directories(dir, 2)
			.par_iter()
			.flat_map_iter(|dir| {
				walkdir::WalkDir::new(dir)
					.into_iter()
					.filter_map(Result::ok)
					.filter(|entry| entry.path().is_file() && entry.path().is_jsonnet_file())
					.map(walkdir::DirEntry::into_path)
			})
			.collect()
	}

	/// Recursively split a directory a few levels deep, so parallel walks
	/// stay balanced even when the tree is lopsided.
	fn collect_directories(path: &Path, max_depth: usize) -> Vec<PathBuf> {
		if max_depth == 0 {
			return vec![path.to_owned()];
		}
		let Ok(entries) = fs::read_dir(path) else {
			return vec![path.to_owned()];
		};

		let mut directories = Vec::new();
		for entry in entries.flatten() {
			let entry_path = entry.path();
			if entry_path.is_dir() {
				directories.extend(Self::collect_directories(&entry_path, max_depth - 1));
			}
		}

		// A directory with no subdirectories is itself a unit of work.
		if directories.is_empty() {
			directories.push(path.to_owned());
		}
		directories
	}

	/// Walk up from a directory (including not-yet-created ones) to the
	/// nearest existing main.jsonnet.
	fn find_entrypoint(dir: &Path) -> Option<PathBuf> {
		let mut current = dir;
		loop {
			if current.exists() {
				let entrypoint = current.join(JPath::DEFAULT_ENTRYPOINT);
				if entrypoint.exists() {
					return Some(entrypoint);
				}
			}
			current = current.parent()?;
		}
	}
}

/// Scanned contents of a single jsonnet file.
#[derive(Debug)]
struct JsonnetFile {
	/// Import path literals, verbatim from the source.
	imports: Vec<String>,
	/// Canonical chart/kustomize directories the file references.
	chart_dirs: Vec<PathBuf>,
}

impl JsonnetFile {
	fn parse(path: &Path, content: &str) -> Self {
		let source = Source::new(content);
		let dir = path.parent_or_root();

		JsonnetFile {
			imports: source
				.imports()
				.map(|import| import.path.to_owned())
				.collect(),
			chart_dirs: source
				.chart_paths()
				.flat_map(|chart_path| chart_path.resolve_dirs(dir))
				.collect(),
		}
	}
}

/// State for one `find_importers` query: the recursion memo is shared across
/// all of the query's target files, while the cycle-guard chain is fresh for
/// each one.
#[derive(Debug)]
struct Search<'a> {
	index: &'a ImporterIndex,
	memo: HashMap<PathBuf, Vec<PathBuf>>,
}

impl Search<'_> {
	/// Find every file that imports the target, directly or transitively.
	fn importers_of(&mut self, target: &Path, chain: &mut HashSet<PathBuf>) -> Vec<PathBuf> {
		if !chain.insert(target.to_owned()) {
			return Vec::new();
		}
		if let Some(cached) = self.memo.get(target) {
			return cached.clone();
		}

		let index = self.index;
		let target_canonical = index
			.canonical
			.get(target)
			.cloned()
			.unwrap_or_else(|| target.canonicalize_or_self());

		let mut importers = Vec::new();

		// Files outside vendor/ and lib/ live inside an environment: their
		// entrypoint imports them implicitly. If no enclosing entrypoint
		// exists, every child environment's entrypoint might.
		if !target.starts_with(&index.vendor) && !target.starts_with(&index.lib) {
			let target_dir = target.parent_or_root();
			if let Some(entrypoint) = ImporterIndex::find_entrypoint(target_dir) {
				importers.push(entrypoint);
			} else if target_dir.exists() {
				importers.extend(
					ImporterIndex::find_jsonnet_files(target_dir)
						.into_iter()
						.filter(|path| path.is_entrypoint()),
				);
			}
		}

		let target_basename = target.file_name().unwrap_or_default();
		let direct: Vec<&Path> = index
			.files
			.par_iter()
			.filter_map(|(path, contents)| {
				index
					.imports_target(path, contents, target, &target_canonical, target_basename)
					.then_some(path.as_path())
			})
			.collect();

		for path in direct {
			importers.push(path.to_owned());
			importers.extend(self.importers_of(path, chain));
		}

		// A vendored file is shadowed for any environment that carries its
		// own vendored copy; drop those importers.
		if let Ok(relative) = target.strip_prefix(&index.vendor) {
			importers.retain(|importer| {
				let local_override = importer.parent_or_root().join("vendor").join(relative);
				!index.files.contains_key(&local_override)
			});
		}

		self.memo.insert(target.to_owned(), importers.clone());
		importers
	}
}
