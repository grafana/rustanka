//! The identity of the build that fills the Helm cache.
//!
//! Cached entries hold the *post-processed* result of a render, so a change to
//! how this crate builds helm's arguments or reads its output has to invalidate
//! them. A release is distinguished by its version, which the release workflow
//! writes into the workspace manifest — but every build between releases
//! reports the same one, so the commit and whether the tree is dirty come along
//! too.
//!
//! A dirty tree shares a single identity, so this does not distinguish one edit
//! from the next. `RTK_HELM_DISABLE_MEMOIZATION` is the tool while iterating on
//! this crate.

use std::process::Command;

fn main() {
	#[cfg(feature = "go-helm")]
	build_go_bridge();
	// A moved HEAD or ref changes the commit. A changed source file can change
	// whether the tree is dirty, and without watching that this script would
	// not rerun and would keep reporting a clean tree.
	println!("cargo:rerun-if-changed=../../.git/HEAD");
	println!("cargo:rerun-if-changed=../../.git/refs/");
	println!("cargo:rerun-if-changed=src");

	println!("cargo:rustc-env=RTK_HELM_BUILD={}", build_identity());
}

#[cfg(feature = "go-helm")]
fn build_go_bridge() {
	let output_directory = std::env::var("OUT_DIR").expect("Cargo sets OUT_DIR");
	let output = Command::new("go")
		.args([
			"build",
			"-tags",
			"netgo,osusergo",
			"-buildmode=c-archive",
			"-o",
		])
		.arg(format!("{output_directory}/libgo_helm.a"))
		.arg("./go-bridge")
		.output()
		.expect("Go is required for the go-helm feature");
	assert!(
		output.status.success(),
		"Go Helm build failed: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	println!("cargo:rerun-if-changed=go-bridge");
	println!("cargo:rerun-if-changed=go.mod");
	println!("cargo:rerun-if-changed=go.sum");
	println!("cargo:rustc-link-search=native={output_directory}");
	println!("cargo:rustc-link-lib=static=go_helm");
}

/// The version, plus the commit and dirtiness where git can say.
///
/// Falls back to the version alone wherever git cannot answer — a vendored
/// crate, a source tarball, a container without git — which is the same
/// identity a release build has.
fn build_identity() -> String {
	let version = env!("CARGO_PKG_VERSION");
	match git(&["rev-parse", "HEAD"]) {
		Some(commit) => {
			let dirty = if is_dirty() { "-dirty" } else { "" };
			format!("{version}+{commit}{dirty}")
		}
		None => version.to_owned(),
	}
}

/// Whether the working tree differs from `HEAD`, anywhere in the repository.
///
/// Deliberately not limited to this crate: a cached value is the product of
/// everything the render walks through, and narrowing this to one directory
/// would claim more precision than it has.
fn is_dirty() -> bool {
	git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty())
}

fn git(arguments: &[&str]) -> Option<String> {
	let output = Command::new("git").args(arguments).output().ok()?;
	if !output.status.success() {
		return None;
	}

	let text = String::from_utf8(output.stdout).ok()?;
	Some(text.trim().to_owned())
}
