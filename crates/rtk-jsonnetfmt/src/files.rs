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

use std::{fs, io, path::Path};

use rtk_gobwas_glob::Glob;
use walkdir::WalkDir;

/// Why discovery could not finish.
///
/// Go returns the bare `os.Stat` / `WalkDir` error and `tk` wraps it with
/// `finding Jsonnet files`; the wrapping belongs to the command, not here.
#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("{path}: {source}")]
	Stat { path: String, source: io::Error },
	#[error("walking {path}: {source}")]
	Walk {
		path: String,
		source: walkdir::Error,
	},
}

/// The extensions `FindFiles` keeps.
const JSONNET_EXTENSIONS: [&str; 2] = [".jsonnet", ".libsonnet"];

/// Find the Jsonnet files under `target`, which may itself be a file.
///
/// `target` is taken as a string rather than a `Path` on purpose: the exact
/// spelling is what a named regular file is returned as, and what the excludes
/// are matched against.
pub fn find_files(target: &str, excludes: &[Glob]) -> Result<Vec<String>, Error> {
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
		consider(clean(&to_slash(Path::new(target))), excludes, &mut files);
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
		consider(clean(&to_slash(entry.path())), excludes, &mut files);
	}

	Ok(files)
}

/// The tail of the walk callback: the excludes, then the extension.
fn consider(path: String, excludes: &[Glob], files: &mut Vec<String>) {
	if excludes.iter().any(|glob| glob.is_match(&path)) {
		return;
	}
	// Behaviours 4 and 5.
	if JSONNET_EXTENSIONS.contains(&extension(&path)) {
		files.push(path);
	}
}

/// Discover across several arguments, the way `FormatFiles` and `Lint` do.
///
/// Behaviour 6: results are concatenated in argument order, neither sorted nor
/// deduplicated.
pub fn find_files_all<I, S>(targets: I, excludes: &[Glob]) -> Result<Vec<String>, Error>
where
	I: IntoIterator<Item = S>,
	S: AsRef<str>,
{
	let mut all = Vec::new();
	for target in targets {
		all.extend(find_files(target.as_ref(), excludes)?);
	}
	Ok(all)
}

/// Go's `filepath.ToSlash`.
fn to_slash(path: &Path) -> String {
	let path = path.to_string_lossy();
	if std::path::MAIN_SEPARATOR == '/' {
		path.into_owned()
	} else {
		path.replace(std::path::MAIN_SEPARATOR, "/")
	}
}

/// Go's `filepath.Ext`, which scans back from the end and stops at a separator.
///
/// The difference from `Path::extension` is the leading dot and, more to the
/// point, a name that is *only* an extension: `filepath.Ext(".jsonnet")` is
/// `".jsonnet"`, while `Path::new(".jsonnet").extension()` is `None`.
fn extension(path: &str) -> &str {
	let bytes = path.as_bytes();
	let mut i = bytes.len();
	while i > 0 {
		i -= 1;
		match bytes[i] {
			b'/' => break,
			b'.' => return &path[i..],
			_ => {}
		}
	}
	""
}

/// Go's `path.Clean`, on a slash-separated path.
///
/// Ported because `filepath.Join` cleans and `Path::join` does not, and the
/// difference is visible to the excludes — see behaviour 3.
fn clean(path: &str) -> String {
	if path.is_empty() {
		return ".".to_string();
	}

	let bytes = path.as_bytes();
	let rooted = bytes[0] == b'/';
	let n = bytes.len();
	let mut out: Vec<u8> = Vec::with_capacity(n);

	// Go seeds its `lazybuf` with the leading separator and refuses to back up
	// over it: `dotdot` is the floor a `..` cannot pop past.
	let (mut r, mut dotdot) = if rooted {
		out.push(b'/');
		(1, 1)
	} else {
		(0, 0)
	};

	while r < n {
		if bytes[r] == b'/' {
			// Empty path element.
			r += 1;
		} else if bytes[r] == b'.' && (r + 1 == n || bytes[r + 1] == b'/') {
			// `.` element.
			r += 1;
		} else if bytes[r] == b'.' && bytes[r + 1] == b'.' && (r + 2 == n || bytes[r + 2] == b'/') {
			// `..` element: remove the last one written.
			r += 2;
			if out.len() > dotdot {
				out.truncate(out.len() - 1);
				while out.len() > dotdot && *out.last().expect("len > dotdot >= 0") != b'/' {
					out.truncate(out.len() - 1);
				}
			} else if !rooted {
				// Cannot back up past the start of a relative path, so keep
				// the `..` and refuse to back up past it later.
				if !out.is_empty() {
					out.push(b'/');
				}
				out.push(b'.');
				out.push(b'.');
				dotdot = out.len();
			}
		} else {
			// A real path element; add a separator if needed, then copy it.
			if (rooted && out.len() != 1) || (!rooted && !out.is_empty()) {
				out.push(b'/');
			}
			while r < n && bytes[r] != b'/' {
				out.push(bytes[r]);
				r += 1;
			}
		}
	}

	if out.is_empty() {
		return ".".to_string();
	}
	String::from_utf8(out).expect("only whole bytes of a UTF-8 string were copied")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn extension_matches_filepath_ext() {
		assert_eq!(extension("main.jsonnet"), ".jsonnet");
		assert_eq!(extension("a/b/main.libsonnet"), ".libsonnet");
		assert_eq!(extension("a.b.jsonnet"), ".jsonnet");
		assert_eq!(extension("README.md"), ".md");
		assert_eq!(extension("Makefile"), "");
		// The case `Path::extension` gets wrong.
		assert_eq!(extension(".jsonnet"), ".jsonnet");
		assert_eq!(extension("a/.libsonnet"), ".libsonnet");
		// A dot in a directory name is not an extension of the file.
		assert_eq!(extension("a.b/Makefile"), "");
		assert_eq!(extension(""), "");
	}

	#[test]
	fn extension_is_case_sensitive() {
		assert!(!JSONNET_EXTENSIONS.contains(&extension("main.JSONNET")));
		assert!(JSONNET_EXTENSIONS.contains(&extension("main.jsonnet")));
	}

	#[test]
	fn clean_matches_path_clean() {
		assert_eq!(clean(""), ".");
		assert_eq!(clean("."), ".");
		assert_eq!(clean("./foo/bar"), "foo/bar");
		assert_eq!(clean("foo/bar"), "foo/bar");
		assert_eq!(clean("foo//bar"), "foo/bar");
		assert_eq!(clean("foo/./bar"), "foo/bar");
		assert_eq!(clean("foo/../bar"), "bar");
		assert_eq!(clean("../foo/bar"), "../foo/bar");
		assert_eq!(clean("../../foo"), "../../foo");
		assert_eq!(clean("/foo/bar"), "/foo/bar");
		assert_eq!(clean("/../foo"), "/foo");
		assert_eq!(clean("/foo/.."), "/");
		assert_eq!(clean("foo/.."), ".");
		assert_eq!(clean("foo/"), "foo");
		assert_eq!(clean("/"), "/");
	}

	#[test]
	fn clean_keeps_multibyte_elements_whole() {
		assert_eq!(clean("./ünïcødé/main.jsonnet"), "ünïcødé/main.jsonnet");
	}
}
