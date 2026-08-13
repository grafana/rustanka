use std::path::PathBuf;

use crate::discover::Discover;

#[derive(Clone, Debug)]
pub struct Engine {
	pub(crate) jsonnet: rtk_jsonnet::Engine,
}

impl Engine {
	pub fn new(engine: rtk_jsonnet::Engine) -> Engine {
		Engine { jsonnet: engine }
	}

	#[tracing::instrument]
	pub fn discover(&self, paths: Vec<PathBuf>) -> Discover {
		Discover::new(self.jsonnet.clone(), paths)
	}
}
