#[cfg(feature = "benchmarking")]
pub mod benchmarking;
mod discover;
mod engine;
pub mod export;
mod metadata;

pub use discover::{Discover, Discovered};
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
