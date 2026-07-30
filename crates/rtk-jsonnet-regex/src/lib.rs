use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use regex::Regex;
use rtk_jsonnet_core as jsonnet;

mod functions;

#[derive(Clone, Debug)]
pub struct Plugin {
	state: Arc<State>,
}

impl Plugin {
	pub fn new() -> Plugin {
		Plugin {
			state: Arc::new(State::default()),
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
			"escapeStringRegex",
			functions::escape_string_regex::Function,
		)?;
		evaluator.with_native_function(
			"regexMatch",
			functions::regex_match::Function::new(Arc::clone(&self.state)),
		)?;
		evaluator.with_native_function(
			"regexSubst",
			functions::regex_subst::Function::new(self.state),
		)?;
		Ok(())
	}
}

#[derive(Debug, Default)]
struct State {
	regex_cache: Mutex<HashMap<String, Arc<Regex>>>,
}

impl State {
	fn parse(&self, pattern: &str) -> Result<Arc<Regex>, regex::Error> {
		{
			let cache = self
				.regex_cache
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			if let Some(regex) = cache.get(pattern) {
				return Ok(Arc::clone(regex));
			}
		}

		let regex = Arc::new(Regex::new(pattern)?);
		let mut cache = self
			.regex_cache
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		Ok(Arc::clone(cache.entry(pattern.to_owned()).or_insert(regex)))
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use super::State;

	#[test]
	fn cache_reuses_compiled_regexes() {
		let state = State::default();
		let first = state.parse("a+").unwrap();
		let second = state.parse("a+").unwrap();

		assert!(Arc::ptr_eq(&first, &second));
	}

	#[test]
	fn cache_recovers_from_poisoned_lock() {
		let state = Arc::new(State::default());
		let thread_state = Arc::clone(&state);
		let _ = std::thread::spawn(move || {
			let _cache = thread_state.regex_cache.lock().unwrap();
			panic!("poison cache");
		})
		.join();

		assert!(state.parse("a+").unwrap().is_match("aaa"));
	}
}
