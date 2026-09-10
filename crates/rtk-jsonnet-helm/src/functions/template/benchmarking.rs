//! Feature-gated access to the native render boundary, without Jsonnet or subprocess overhead.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use super::{Function, Options};
use crate::State;

pub struct NativeRenderer {
	function: Function,
	chart: PathBuf,
	options: Options,
	memoization: bool,
}

impl NativeRenderer {
	pub fn new(memoization: bool) -> Self {
		let root =
			PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rtk-benchmarks/helm-template");
		Self {
			function: Function::new(Arc::new(State::new(None))),
			chart: root.join("charts/bench-chart"),
			options: serde_json::from_value(json!({
				"calledFrom": root.join("main.jsonnet"),
				"namespace": "bench",
				// Keep the real chart while making repeated uncached samples affordable.
				"values": {"hashRounds": 2000},
			}))
			.unwrap(),
			memoization,
		}
	}

	pub fn render(&self) -> serde_json::Value {
		self.function
			.native_render("bench", &self.chart, &self.options, !self.memoization)
			.expect("benchmark chart renders")
	}
}
