#[cfg(feature = "benchmarking")]
pub mod benchmarking;
mod discover;
mod engine;
pub mod export;
mod metadata;

pub use discover::{Discover, Discovered, Search};
pub use engine::Engine;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error(transparent)]
	Io(#[from] std::io::Error),
	#[error(transparent)]
	Json(#[from] serde_json::Error),
	#[error("a jpath error occurred")]
	JPath(#[from] rtk_jsonnet::jpath::Error),
	#[error(transparent)]
	Evaluation(#[from] rtk_jsonnet::Error),
	/// A failure that happened on another thread.
	///
	/// Jsonnet's stack traces are `Rc`-based, so an error cannot leave the thread
	/// that raised it; what crosses is what it said.
	#[error("{0}")]
	Rendered(String),
}

/// The Tanka whose behaviour rtk implements, spelled as tk spells its own.
///
/// tk builds this in with `git describe --tags`, so a release reports
/// `v0.38.0`, and its version messages quote the string verbatim. rtk reports
/// the same one because that is the question an environment's
/// `spec.expectVersions.tanka` asks: not what version of rtk is running, but
/// whether the tool provides the Tanka the environment needs.
///
/// Note the absence of a prerelease. Masterminds treats a prerelease version as
/// unsatisfying any constraint that did not ask for one, so a `-pre` here would
/// fail nearly every constraint an environment could write.
pub const TANKA_COMPATIBLE_VERSION: &str = "v0.38.0";
