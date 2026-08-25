//! `rtk export --helm-cache`, end to end.
//!
//! The cache's mechanics have unit tests that stub the render, so this is the
//! only place the whole path is exercised: a real chart, a real helm, real
//! entries on disk, and a second export served from them.
//!
//! Needs a helm binary. So do the helm golden fixtures, which run under
//! `cargo test`, so anywhere those pass this does too.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A copy of the fixture, so the export can write `target/helm` beside it.
fn staged_environment() -> tempfile::TempDir {
	let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.and_then(Path::parent)
		.expect("the repository root")
		.join("test_fixtures/golden_envs/helm_template_env");

	let staged = tempfile::Builder::new()
		.prefix("rtk-helm-cache")
		.tempdir()
		.expect("a temporary directory");
	for entry in walkdir::WalkDir::new(&source) {
		let entry = entry.expect("the fixture is readable");
		let relative = entry
			.path()
			.strip_prefix(&source)
			.expect("below the fixture");
		// The golden output is not part of the environment.
		if relative.starts_with("golden") {
			continue;
		}
		let destination = staged.path().join(relative);
		if entry.file_type().is_dir() {
			fs::create_dir_all(&destination).expect("the directory");
		} else {
			fs::copy(entry.path(), &destination).expect("the file");
		}
	}
	staged
}

/// Every cache entry under the environment's project directory.
fn entries(environment: &Path) -> Vec<PathBuf> {
	let mut entries = walkdir::WalkDir::new(environment.join("target/helm"))
		.into_iter()
		.flatten()
		.map(|entry| entry.path().to_path_buf())
		.filter(|path| {
			path.extension()
				.is_some_and(|extension| extension == "cbor")
		})
		.collect::<Vec<_>>();
	entries.sort();
	entries
}

/// The manifests an export wrote, and whether the render came from the cache.
struct Exported {
	manifests: String,
	served_from_cache: bool,
}

fn export(environment: &Path, into: &str, environment_variables: &[(&str, &str)]) -> Exported {
	let output_dir = environment.join(into);
	let mut command = Command::new(env!("CARGO_BIN_EXE_rtk"));
	command.current_dir(environment).args([
		"export",
		&output_dir.to_string_lossy(),
		".",
		"--helm-cache",
	]);
	for (name, value) in environment_variables {
		command.env(name, value);
	}

	command.env("RUST_LOG", "debug");
	let output = command.output().expect("rtk runs");
	assert!(
		output.status.success(),
		"rtk export failed:\n{}",
		String::from_utf8_lossy(&output.stderr)
	);
	let served_from_cache =
		String::from_utf8_lossy(&output.stderr).contains("helm render served from the disk cache");

	let mut manifests = walkdir::WalkDir::new(&output_dir)
		.into_iter()
		.flatten()
		.filter(|entry| entry.file_type().is_file())
		.map(|entry| {
			let relative = entry
				.path()
				.strip_prefix(&output_dir)
				.expect("below the output directory")
				.to_string_lossy()
				.into_owned();
			let contents = fs::read_to_string(entry.path()).expect("the manifest");
			format!("=== {relative}\n{contents}")
		})
		.collect::<Vec<_>>();
	manifests.sort();
	Exported {
		manifests: manifests.join(""),
		served_from_cache,
	}
}

/// The second export is served from disk, and says the same thing.
#[test]
fn a_second_export_is_served_from_the_cache() {
	let staged = staged_environment();
	let environment = staged.path();

	let first = export(environment, "out-first", &[]);
	assert!(!first.served_from_cache, "nothing was cached yet");
	let after_first = entries(environment);
	assert!(
		!after_first.is_empty(),
		"the first export should have written cache entries"
	);

	let second = export(environment, "out-second", &[]);
	assert!(second.served_from_cache, "the second export rendered again");
	assert_eq!(
		second.manifests, first.manifests,
		"the cached export rendered something else"
	);
	assert_eq!(
		entries(environment),
		after_first,
		"a hit should not have written a new entry"
	);
}

/// And it survives an environment helm cannot read the render from.
///
/// The key used to hash every kubeconfig it could find, so pointing `KUBECONFIG`
/// somewhere else discarded the cache. `helm template` never reads one — rtk
/// does not pass `--validate` — and this environment names a namespace, which
/// helm takes as an override, so nothing here can reach the render.
#[test]
fn the_cache_survives_an_unrelated_kube_environment() {
	let staged = staged_environment();
	let environment = staged.path();

	let first = export(environment, "out-first", &[]);
	let after_first = entries(environment);
	assert!(!after_first.is_empty());

	let elsewhere = environment.join("elsewhere");
	fs::create_dir_all(&elsewhere).expect("a directory to point at");
	let second = export(
		environment,
		"out-second",
		&[
			("KUBECONFIG", &elsewhere.join("config").to_string_lossy()),
			("HOME", &elsewhere.to_string_lossy()),
		],
	);

	assert!(
		second.served_from_cache,
		"an unrelated kube environment discarded the cache"
	);
	assert_eq!(second.manifests, first.manifests);
	assert_eq!(
		entries(environment),
		after_first,
		"an unrelated kube environment should not have made a second entry"
	);
}
