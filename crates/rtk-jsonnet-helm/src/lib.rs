use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{OnceLock, RwLock};

use rtk_jsonnet_core as jsonnet;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHasher};

mod functions;

#[derive(Clone, Debug)]
pub struct Plugin {
	state: &'static State,
}

impl Plugin {
	pub fn new() -> Plugin {
		Plugin {
			state: State::get(),
		}
	}
}

impl Default for Plugin {
	fn default() -> Self {
		Self::new()
	}
}

impl<'a, E> jsonnet::Plugin<'a, E> for Plugin
where
	E: jsonnet::Evaluator<'a>,
{
	fn install(
		self,
		evaluator: &mut E,
	) -> Result<(), <<E as jsonnet::Evaluator<'a>>::Implementation as jsonnet::Implementation>::Error>
	{
		evaluator.with_native_function(
			"helmTemplate",
			functions::template::Function::new(self.state),
		)?;
		Ok(())
	}
}

#[derive(Debug)]
struct State {
	template_cache: RwLock<FxHashMap<Box<str>, serde_json::Value>>,
}

impl State {
	fn get() -> &'static State {
		static STATE: OnceLock<State> = OnceLock::new();
		STATE.get_or_init(|| State {
			template_cache: RwLock::new(FxHashMap::with_hasher(FxBuildHasher)),
		})
	}

	fn cache_key(
		name: &str,
		chart_path: &Path,
		chart_meta: Option<&str>,
		options: &functions::template::Options,
	) -> Box<str> {
		let hashed = {
			let mut hasher = FxHasher::default();
			options.hash(&mut hasher);
			chart_meta.hash(&mut hasher);
			hasher.finish()
		};

		format!("{name}|{}|{hashed:x}", chart_path.display()).into()
	}
}
