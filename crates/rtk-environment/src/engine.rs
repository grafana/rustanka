use std::path::PathBuf;

use crate::discover::{self, Discover, Discovered};

#[derive(Clone, Debug)]
pub struct Engine {
	pub(crate) jsonnet: rtk_jsonnet::Engine,
}

/// Exporting environments in parallel hands a clone of the engine to every
/// worker, so this must hold. Evaluators and evaluated values are `Rc`-based
/// and therefore stay on the thread that created them, but the engine itself is
/// just an `Arc` around configuration and lazily initialized implementations.
const _: () = {
	const fn assert_send_sync<T: Send + Sync>() {}
	assert_send_sync::<Engine>();
};

impl Engine {
	pub fn new(engine: rtk_jsonnet::Engine) -> Engine {
		Engine { jsonnet: engine }
	}

	/// Environments under `paths`, one at a time.
	///
	/// Reading what a directory declares means evaluating Jsonnet, and this does
	/// it as it goes, so that a caller which stops early has not paid for the
	/// rest. Exporting wants that: it evaluates one environment while discovering
	/// the next.
	#[tracing::instrument]
	pub fn discover(&self, paths: Vec<PathBuf>) -> Discover {
		Discover::new(self.jsonnet.clone(), paths)
	}

	/// Every environment under `paths`, reading several directories at once.
	///
	/// For callers that want all of them anyway, which listing and diffing do.
	/// The environments come back in the order [`Engine::discover`] would have
	/// handed them out, and so does the first failure among them.
	#[tracing::instrument]
	pub fn discover_all(&self, paths: Vec<PathBuf>) -> Result<Vec<Discovered>, crate::Error> {
		discover::resolve_all(&self.jsonnet, paths)
	}
}
