//! Phase 1's exit criterion: discovery agrees with `tk` on a fixture tree with
//! symlinks, dotfiles, a vendor directory, a non-Jsonnet named file and a
//! duplicated argument.
//!
//! Each test names the behaviour of Tanka's `FindFiles` it pins; the six are
//! enumerated in `src/files.rs`.

use std::{
	fs,
	path::{Path, PathBuf},
};

use rtk_gobwas_glob::{Glob, TANKA_DEFAULT_EXCLUDES, compile_all};
use rtk_jsonnetfmt::{find_files, find_files_all};
use tempfile::TempDir;

/// A tree covering every case the six behaviours turn on.
///
/// The prefix matters: `tempfile`'s default is `.tmpXXXX`, and a dotted
/// component anywhere in an absolute path makes the default `.*` exclude match
/// the whole thing — `*` crosses `/`. That is a real trap, noted in
/// `CLAUDE.md`, and the reason discovery finds nothing when pointed at a
/// default temporary directory.
fn fixture_tree() -> TempDir {
	let dir = tempfile::Builder::new()
		.prefix("rtk-fmt-discovery")
		.tempdir()
		.expect("temporary directory is created");
	let root = dir.path();

	for sub in [
		"environments/default",
		"environments/vendor",
		"lib",
		"vendor",
	] {
		fs::create_dir_all(root.join(sub)).expect("subdirectory is created");
	}

	let files = [
		// A file whose entire name is the extension. `filepath.Ext` calls this
		// ".jsonnet"; `Path::extension` calls it `None`.
		(".jsonnet", "{}\n"),
		("main.jsonnet", "{ a: 1 }\n"),
		("notes.JSONNET", "{}\n"),
		("README.md", "not jsonnet\n"),
		("lib/util.libsonnet", "{}\n"),
		("vendor/k.libsonnet", "{}\n"),
		("environments/default/main.jsonnet", "{}\n"),
		("environments/vendor/nested.libsonnet", "{}\n"),
	];
	for (path, contents) in files {
		fs::write(root.join(path), contents).expect("fixture file is written");
	}

	#[cfg(unix)]
	{
		std::os::unix::fs::symlink(root.join("main.jsonnet"), root.join("link.jsonnet"))
			.expect("file symlink is created");
		std::os::unix::fs::symlink(root.join("lib"), root.join("linkdir"))
			.expect("directory symlink is created");
	}

	dir
}

fn defaults() -> Vec<Glob> {
	compile_all(TANKA_DEFAULT_EXCLUDES).expect("tk's default excludes compile")
}

fn excludes(patterns: &[&str]) -> Vec<Glob> {
	compile_all(patterns.iter().copied()).expect("patterns compile")
}

fn as_str(path: &Path) -> &str {
	path.to_str().expect("fixture paths are UTF-8")
}

/// Strip the temporary root so expectations read as relative paths.
fn relative(root: &Path, found: &[PathBuf]) -> Vec<String> {
	let prefix = format!("{}/", as_str(root));
	found
		.iter()
		.map(|path| {
			path.strip_prefix(root)
				.unwrap_or_else(|_| panic!("{} is not under {prefix}", path.display()))
				.to_string_lossy()
				.replace(std::path::MAIN_SEPARATOR, "/")
		})
		.collect()
}

/// Behaviour 1: `os.Stat` plus `IsRegular` returns the target before any glob
/// or extension check runs.
#[test]
fn a_named_regular_file_bypasses_the_excludes() {
	let tree = fixture_tree();
	let target = tree.path().join("vendor/k.libsonnet");

	let found = find_files(as_str(&target), &defaults()).expect("discovery succeeds");

	assert_eq!(
		found,
		vec![target],
		"a vendored file named outright is formatted despite the default vendor exclude"
	);
}

/// Behaviour 1 again, and the sharper edge of it: the extension is not checked
/// either, so `tk fmt README.md` hands `README.md` to the Jsonnet formatter.
#[test]
fn a_named_regular_file_bypasses_the_extension_check() {
	let tree = fixture_tree();
	let target = tree.path().join("README.md");

	let found = find_files(as_str(&target), &defaults()).expect("discovery succeeds");

	assert_eq!(found, vec![target]);
}

/// Behaviour 1, and the path is returned with the spelling it was given.
#[test]
fn a_named_regular_file_is_returned_verbatim() {
	let tree = fixture_tree();
	let target = format!("{}/./main.jsonnet", as_str(tree.path()));

	let found = find_files(&target, &defaults()).expect("discovery succeeds");

	assert_eq!(
		found,
		vec![PathBuf::from(target)],
		"the file case returns the argument, uncleaned — only walked paths are cleaned"
	);
}

/// Behaviour 2: the walk callback returns on `d.IsDir()` *before* the exclude
/// loop, and returns `nil` rather than `fs.SkipDir`. So an exclude that matches
/// a directory's own path prunes nothing.
#[test]
fn a_directory_matching_an_exclude_is_still_walked() {
	let tree = fixture_tree();
	let root = as_str(tree.path());

	// This pattern matches the `lib` directory's path exactly, and nothing
	// under it.
	let found = find_files(root, &excludes(&["**/lib"])).expect("discovery succeeds");

	assert!(
		relative(tree.path(), &found).contains(&"lib/util.libsonnet".to_owned()),
		"excluding a directory does not exclude its contents: {found:?}"
	);
}

/// Behaviour 3: walked paths are cleaned, the way `filepath.Join` cleans them.
#[test]
fn walked_paths_are_cleaned() {
	let tree = fixture_tree();
	let target = format!("{}/./lib", as_str(tree.path()));

	let found = find_files(&target, &[]).expect("discovery succeeds");

	assert_eq!(
		found,
		vec![tree.path().join("lib/util.libsonnet")],
		"a `/./` in the argument does not survive into the walked paths"
	);
}

/// Behaviours 4 and 5, and the whole reason `rtk-gobwas-glob` exists: a single
/// `*` crosses `/`, so `*/vendor/*` reaches a nested vendor directory.
#[test]
fn a_single_star_crosses_a_slash() {
	let tree = fixture_tree();
	let root = as_str(tree.path());

	let found = find_files(root, &excludes(&["*/vendor/*"])).expect("discovery succeeds");
	let found = relative(tree.path(), &found);

	assert!(
		!found.contains(&"environments/vendor/nested.libsonnet".to_owned()),
		"`*` must cross `/`, as it does with no separators: {found:?}"
	);
	assert!(
		!found.contains(&"vendor/k.libsonnet".to_owned()),
		"and reach the top-level vendor directory too: {found:?}"
	);
}

/// Behaviour 5: exactly `.jsonnet` and `.libsonnet`, case-sensitive, computed
/// the way `filepath.Ext` computes them.
#[test]
fn extensions_are_exact_and_case_sensitive() {
	let tree = fixture_tree();
	let found = relative(
		tree.path(),
		&find_files(as_str(tree.path()), &[]).expect("discovery succeeds"),
	);

	assert!(
		found.contains(&".jsonnet".to_owned()),
		"a file named only `.jsonnet` counts, which `Path::extension` would deny: {found:?}"
	);
	assert!(!found.contains(&"notes.JSONNET".to_owned()), "{found:?}");
	assert!(!found.contains(&"README.md".to_owned()), "{found:?}");
}

/// Behaviour 6, plus the lexical order `filepath.WalkDir` walks in. The whole
/// list is pinned rather than probed, because the order is what a caller
/// reports as `Formatted N files` and what `--verbose` prints.
#[test]
fn the_whole_tree_is_found_in_walk_order() {
	let tree = fixture_tree();
	let found = relative(
		tree.path(),
		&find_files(as_str(tree.path()), &[]).expect("discovery succeeds"),
	);

	let mut expected = vec![
		".jsonnet",
		"environments/default/main.jsonnet",
		"environments/vendor/nested.libsonnet",
		"lib/util.libsonnet",
	];
	// A symlink to a file is a non-directory entry with a Jsonnet extension, so
	// it is picked up; a symlink to a directory is never descended into, so
	// `linkdir/util.libsonnet` never appears.
	if cfg!(unix) {
		expected.push("link.jsonnet");
	}
	expected.push("main.jsonnet");
	expected.push("vendor/k.libsonnet");

	assert_eq!(found, expected);
}

/// Behaviour 6: no dedup across arguments. A file named twice is formatted
/// twice and counted twice.
#[test]
fn arguments_are_concatenated_without_dedup_or_sorting() {
	let tree = fixture_tree();
	let lib = format!("{}/lib", as_str(tree.path()));
	let vendor_file = format!("{}/vendor/k.libsonnet", as_str(tree.path()));

	let found = find_files_all([&vendor_file, &lib, &vendor_file], &defaults())
		.expect("discovery succeeds");

	assert_eq!(
		found,
		vec![
			PathBuf::from(&vendor_file),
			tree.path().join("lib/util.libsonnet"),
			PathBuf::from(vendor_file),
		],
		"results are per-argument, in argument order, with duplicates kept"
	);
}

/// `tk`'s defaults, on the tree as a whole.
#[test]
fn tanka_defaults_exclude_vendor_and_dotfiles() {
	let tree = fixture_tree();
	let found = relative(
		tree.path(),
		&find_files(as_str(tree.path()), &defaults()).expect("discovery succeeds"),
	);

	assert!(
		!found.iter().any(|path| path.contains("vendor")),
		"{found:?}"
	);
	// Caught by `**/.*`, not by `.*`: the paths here are absolute, so nothing
	// matches a pattern anchored on a leading dot. That asymmetry is exactly
	// why `tk` ships both forms.
	assert!(
		!found.contains(&".jsonnet".to_owned()),
		"a dotfile is excluded: {found:?}"
	);
	assert!(
		found.contains(&"lib/util.libsonnet".to_owned()),
		"{found:?}"
	);
	assert!(found.contains(&"main.jsonnet".to_owned()), "{found:?}");
}

/// A missing argument is an error, as `os.Stat` makes it.
#[test]
fn a_missing_target_is_an_error() {
	let tree = fixture_tree();
	let missing = format!("{}/nope", as_str(tree.path()));

	let err = find_files(&missing, &[]).expect_err("a missing path is an error");

	assert!(err.to_string().contains("nope"), "{err}");
}

/// A symlinked file named as the argument is a regular file to `os.Stat`,
/// which follows symlinks.
#[cfg(unix)]
#[test]
fn a_named_symlink_to_a_file_is_returned() {
	let tree = fixture_tree();
	let target = tree.path().join("link.jsonnet");

	let found = find_files(as_str(&target), &defaults()).expect("discovery succeeds");

	assert_eq!(found, vec![target]);
}

#[test]
fn parent_components_do_not_bypass_excludes() {
	let tree = fixture_tree();
	let target = tree.path().join("environments/default/../..");
	let exclude = Glob::new(&rtk_gobwas_glob::quote_meta(as_str(
		&tree.path().join("main.jsonnet"),
	)))
	.unwrap();
	let found = find_files(&target, &[exclude]).unwrap();
	assert!(!found.contains(&tree.path().join("main.jsonnet")));
	assert!(found.contains(&tree.path().join("lib/util.libsonnet")));
	assert!(found.iter().all(|path| {
		!path
			.as_os_str()
			.as_encoded_bytes()
			.windows(2)
			.any(|w| w == b"//")
	}));
}

#[test]
fn a_named_file_preserves_native_spelling() {
	let tree = fixture_tree();
	let target = tree.path().join("lib/.././main.jsonnet");
	let found = find_files(&target, &defaults()).unwrap();
	assert_eq!(found[0].as_os_str(), target.as_os_str());
}

// macOS filesystems reject invalid UTF-8 names; Linux permits them.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_paths_survive_discovery_and_can_be_opened() {
	use std::{ffi::OsString, os::unix::ffi::OsStringExt};
	let tree = fixture_tree();
	let directory = tree
		.path()
		.join(OsString::from_vec(b"native-\xff".to_vec()));
	fs::create_dir(&directory).unwrap();
	let target = directory.join(OsString::from_vec(b"file-\xfe.jsonnet".to_vec()));
	fs::write(&target, "{}\n").unwrap();
	assert_eq!(
		find_files(&target, &defaults()).unwrap(),
		vec![target.clone()]
	);
	assert_eq!(find_files(&directory, &[]).unwrap(), vec![target.clone()]);
	assert_eq!(
		fs::read_to_string(&find_files(&directory, &[]).unwrap()[0]).unwrap(),
		"{}\n"
	);
	assert!(
		find_files(&directory, &excludes(&["**/file-*.jsonnet"]))
			.unwrap()
			.is_empty()
	);
}

#[cfg(unix)]
#[test]
fn a_named_directory_symlink_is_not_cleaned_or_followed() {
	let tree = fixture_tree();
	std::os::unix::fs::symlink(tree.path().join("lib"), tree.path().join("linkdir.jsonnet"))
		.unwrap();
	let target = tree.path().join("lib/.././linkdir.jsonnet");
	let found = find_files(&target, &[]).unwrap();
	assert_eq!(found.len(), 1);
	assert_eq!(found[0].as_os_str(), target.as_os_str());
}
