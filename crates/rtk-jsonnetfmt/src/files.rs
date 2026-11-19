//! A port of Tanka's `pkg/jsonnet/files.go`.
//!
//! `tk fmt` and `tk lint` share `FindFiles`, and six of its behaviours are each
//! surprising enough to be worth naming — every one of them changes which
//! files get rewritten:
//!
//! 1. **A named regular file bypasses everything.** `os.Stat` plus
//!    `Mode().IsRegular()` returns the target before any glob or extension
//!    check runs. So `tk fmt vendor/foo.libsonnet` formats it despite the
//!    default `vendor/**` exclude, and `tk fmt README.md` hands `README.md` to
//!    the Jsonnet formatter.
//! 2. **Directories are never pruned.** The walk callback returns on
//!    `d.IsDir()` *before* the exclude loop, and returns `nil` rather than
//!    `fs.SkipDir`. A `**/vendor/**` exclude still walks the whole vendor tree
//!    and discards it file by file.
//! 3. **Globs match the walk path as given**, slash-normalised — absolute if
//!    the argument was absolute, relative if relative. Child paths go through
//!    `filepath.Join`, which *cleans*, so a `./foo` argument yields `foo/bar`
//!    and not `./foo/bar`. That matters: with no separators `.*` matches
//!    anything beginning with a dot, so an uncleaned `./` prefix would exclude
//!    the entire tree.
//! 4. **`*` crosses `/`**, because `glob.Compile` is called with no separator
//!    arguments. See `rtk-gobwas-glob`.
//! 5. **Extensions are exactly `.jsonnet` and `.libsonnet`**, case-sensitive,
//!    and computed the way `filepath.Ext` computes them — so a file *named*
//!    `.jsonnet` counts, which `Path::extension` would deny.
//! 6. **No sorting and no dedup across arguments.** `filepath.WalkDir` walks
//!    lexically within one tree, and the caller concatenates per-argument
//!    results in argument order. A file named twice is formatted twice and
//!    counted twice in `Formatted N files`.
//!
//! `filepath.WalkDir` does not follow symlinks, including one named as the
//! argument, so neither does this.

use std::{
	fs, io,
	path::{Component, Path, PathBuf},
};

use rtk_gobwas_glob::Glob;
use walkdir::WalkDir;

/// Why discovery could not finish.
///
/// Go returns the bare `os.Stat` / `WalkDir` error and `tk` wraps it with
/// `finding Jsonnet files`; the wrapping belongs to the command, not here.
#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("{}: {source}", path.display())]
	Stat { path: PathBuf, source: io::Error },
	#[error("walking {}: {source}", path.display())]
	Walk {
		path: PathBuf,
		source: walkdir::Error,
	},
}

/// The extensions `FindFiles` keeps.
const JSONNET_EXTENSIONS: [&str; 2] = [".jsonnet", ".libsonnet"];

/// Find the Jsonnet files under `target`, which may itself be a file.
///
/// A named regular file retains its original spelling. Filesystem paths stay
/// native throughout discovery, including names that are not valid UTF-8.
pub fn find_files(target: impl AsRef<Path>, excludes: &[Glob]) -> Result<Vec<PathBuf>, Error> {
	let target = target.as_ref();
	let metadata = fs::metadata(target).map_err(|source| Error::Stat {
		path: target.to_owned(),
		source,
	})?;

	// Behaviour 1: a named regular file is returned whatever it is called and
	// whatever the excludes say. `fs::metadata` follows symlinks, as `os.Stat`
	// does, so a symlink to a regular file counts as one.
	if metadata.is_file() {
		return Ok(vec![target.to_owned()]);
	}

	let mut files = Vec::new();

	// `filepath.WalkDir` lstats its root. A symlink is not a directory to it,
	// so it hands the root to the callback once as a plain entry and descends
	// no further — which, having already ruled out a regular file above, means
	// a symlink to a directory is considered as a single non-directory path and
	// its contents are never seen.
	let target_is_symlink =
		fs::symlink_metadata(target).is_ok_and(|metadata| metadata.file_type().is_symlink());
	if target_is_symlink {
		consider(target.to_owned(), excludes, &mut files);
		return Ok(files);
	}

	for entry in WalkDir::new(target).follow_links(false).sort_by_file_name() {
		let entry = entry.map_err(|source| Error::Walk {
			path: target.to_owned(),
			source,
		})?;

		// Behaviour 2: directories are skipped before the excludes are
		// consulted, and the walk continues into them regardless.
		if entry.file_type().is_dir() {
			continue;
		}

		// Behaviour 3: the path the excludes see, cleaned and slash-normalised
		// the way `filepath.Join` plus `filepath.ToSlash` leave it.
		consider(clean(entry.path()), excludes, &mut files);
	}

	Ok(files)
}

/// The tail of the walk callback: the excludes, then the extension.
fn consider(path: PathBuf, excludes: &[Glob], files: &mut Vec<PathBuf>) {
	let match_path = to_slash(&path);
	if excludes.iter().any(|glob| glob.is_match_bytes(&match_path)) {
		return;
	}
	// Behaviours 4 and 5.
	let ext = extension(&path);
	if JSONNET_EXTENSIONS
		.iter()
		.any(|candidate| ext == candidate.as_bytes())
	{
		files.push(path);
	}
}

/// Discover across several arguments, the way `FormatFiles` and `Lint` do.
///
/// Behaviour 6: results are concatenated in argument order, neither sorted nor
/// deduplicated.
pub fn find_files_all<I, S>(targets: I, excludes: &[Glob]) -> Result<Vec<PathBuf>, Error>
where
	I: IntoIterator<Item = S>,
	S: AsRef<Path>,
{
	let mut all = Vec::new();
	for target in targets {
		all.extend(find_files(target.as_ref(), excludes)?);
	}
	Ok(all)
}

/// Slash-normalise only the matching view; never reconstruct a filesystem
/// path from a Unicode conversion. On Unix these are the original filename
/// bytes, which Go strings also preserve.
fn to_slash(path: &Path) -> Vec<u8> {
	let mut bytes = path.as_os_str().as_encoded_bytes().to_vec();
	if std::path::MAIN_SEPARATOR != '/' {
		for byte in &mut bytes {
			if *byte == b'\\' {
				*byte = b'/';
			}
		}
	}
	bytes
}

/// Go's `filepath.Ext` includes a leading dot, even in a name like `.jsonnet`.
fn extension(path: impl AsRef<Path>) -> Vec<u8> {
	let Some(name) = path.as_ref().file_name() else {
		return Vec::new();
	};
	let bytes = name.as_encoded_bytes();
	bytes
		.iter()
		.rposition(|byte| *byte == b'.')
		.map_or_else(Vec::new, |dot| bytes[dot..].to_vec())
}

/// Lexically clean the path as `filepath.Join` does, without resolving
/// symlinks or converting native components to text. `PathBuf::pop` removes
/// the component and its separator together, including repeated `..` steps.
fn clean(path: impl AsRef<Path>) -> PathBuf {
	let mut result = PathBuf::new();
	for component in path.as_ref().components() {
		match component {
			Component::CurDir => {}
			Component::ParentDir => {
				if matches!(result.components().next_back(), Some(Component::Normal(_))) {
					result.pop();
				} else if !result.has_root() {
					result.push(component.as_os_str());
				}
			}
			_ => result.push(component.as_os_str()),
		}
	}
	if result.as_os_str().is_empty() {
		result.push(".");
	}
	result
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn extension_matches_filepath_ext() {
		assert_eq!(extension("main.jsonnet"), b".jsonnet");
		assert_eq!(extension("a/b/main.libsonnet"), b".libsonnet");
		assert_eq!(extension("a.b.jsonnet"), b".jsonnet");
		assert_eq!(extension("README.md"), b".md");
		assert_eq!(extension("Makefile"), b"");
		// The case `Path::extension` gets wrong.
		assert_eq!(extension(".jsonnet"), b".jsonnet");
		assert_eq!(extension("a/.libsonnet"), b".libsonnet");
		// A dot in a directory name is not an extension of the file.
		assert_eq!(extension("a.b/Makefile"), b"");
		assert_eq!(extension(""), b"");
	}

	#[test]
	fn extension_is_case_sensitive() {
		assert!(
			!JSONNET_EXTENSIONS
				.iter()
				.any(|ext| extension("main.JSONNET") == ext.as_bytes())
		);
		assert!(
			JSONNET_EXTENSIONS
				.iter()
				.any(|ext| extension("main.jsonnet") == ext.as_bytes())
		);
	}

	#[test]
	fn clean_matches_path_clean() {
		assert_eq!(clean(""), Path::new("."));
		assert_eq!(clean("."), Path::new("."));
		assert_eq!(clean("./foo/bar"), Path::new("foo/bar"));
		assert_eq!(clean("foo/bar"), Path::new("foo/bar"));
		assert_eq!(clean("foo//bar"), Path::new("foo/bar"));
		assert_eq!(clean("foo/./bar"), Path::new("foo/bar"));
		assert_eq!(clean("foo/../bar"), Path::new("bar"));
		assert_eq!(clean("../foo/bar"), Path::new("../foo/bar"));
		assert_eq!(clean("../../foo"), Path::new("../../foo"));
		assert_eq!(clean("/foo/bar"), Path::new("/foo/bar"));
		assert_eq!(clean("/../foo"), Path::new("/foo"));
		assert_eq!(clean("/foo/.."), Path::new("/"));
		assert_eq!(clean("foo/.."), Path::new("."));
		assert_eq!(clean("foo/"), Path::new("foo"));
		assert_eq!(clean("/"), Path::new("/"));
	}

	#[test]
	fn clean_keeps_multibyte_elements_whole() {
		assert_eq!(
			clean("./ünïcødé/main.jsonnet"),
			Path::new("ünïcødé/main.jsonnet")
		);
	}
	#[test]
	fn clean_removes_separators_with_parent_components() {
		for (input, expected) in [
			("foo/bar/../baz", "foo/baz"),
			("foo/bar/../../baz", "baz"),
			("foo/bar/..", "foo"),
			("/foo/bar/../../..", "/"),
			("../foo/../../bar", "../../bar"),
		] {
			assert_eq!(clean(input), Path::new(expected));
		}
	}

	#[cfg(unix)]
	#[test]
	fn native_bytes_survive_cleaning_and_extension_checks() {
		use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
		let path = Path::new(OsStr::from_bytes(
			b"./native-\xff/child/../file-\xfe.jsonnet",
		));
		let cleaned = clean(path);
		assert_eq!(
			cleaned.as_os_str().as_bytes(),
			b"native-\xff/file-\xfe.jsonnet"
		);
		assert_eq!(extension(&cleaned), b".jsonnet");
		assert_eq!(to_slash(&cleaned), b"native-\xff/file-\xfe.jsonnet");
	}

	#[cfg(windows)]
	#[test]
	fn clean_preserves_windows_volumes() {
		assert_eq!(
			clean(r"C:\foo\bar\..\main.jsonnet"),
			Path::new(r"C:\foo\main.jsonnet")
		);
		assert_eq!(
			clean(r"\\server\share\foo\..\main.jsonnet"),
			Path::new(r"\\server\share\main.jsonnet")
		);
	}
}
