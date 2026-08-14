use std::path::PathBuf;

use crate::discover::Discover;

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

	#[tracing::instrument]
	pub fn discover(&self, paths: Vec<PathBuf>) -> Discover {
		Discover::new(self.jsonnet.clone(), paths)
	}
}
