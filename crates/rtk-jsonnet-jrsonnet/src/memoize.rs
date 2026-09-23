//! Process-wide, manifested-JSON implementation of `rtkMemoize`.

use std::{
	cell::RefCell,
	collections::{HashMap, HashSet},
	sync::{Arc, Condvar, Mutex, OnceLock},
};

use jrsonnet_evaluator::{
	IStr, Thunk, Val,
	error::{ErrorKind::RuntimeError, Result},
	manifest::JsonFormat,
};
use jrsonnet_macros::builtin;

enum MemoState {
	Computing,
	Done(String),
	Failed,
}

struct MemoSlot {
	state: Mutex<MemoState>,
	cond: Condvar,
}

static CACHE: OnceLock<Mutex<HashMap<String, Arc<MemoSlot>>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, Arc<MemoSlot>>> {
	CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

thread_local! {
	static ACTIVE: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Removes an in-progress marker and wakes waiters after an error or panic.
struct Computing<'a> {
	key: &'a str,
	slot: Arc<MemoSlot>,
	completed: bool,
}

impl Drop for Computing<'_> {
	fn drop(&mut self) {
		ACTIVE.with(|active| {
			active.borrow_mut().remove(self.key);
		});

		if self.completed {
			return;
		}

		cache()
			.lock()
			.unwrap_or_else(|error| error.into_inner())
			.remove(self.key);
		let mut state = self
			.slot
			.state
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		*state = MemoState::Failed;
		self.slot.cond.notify_all();
	}
}

fn contains_hidden_field(value: &Val) -> Result<bool> {
	match value {
		Val::Obj(object) => {
			let visible: HashSet<IStr> = object.fields_ex(false).into_iter().collect();
			let all = object.fields_ex(true);
			if all.len() != visible.len() {
				return Ok(true);
			}
			for name in all {
				let field = object
					.get(name)?
					.expect("a field returned by fields_ex exists");
				if contains_hidden_field(&field)? {
					return Ok(true);
				}
			}
			Ok(false)
		}
		Val::Arr(array) => {
			for item in array.iter() {
				if contains_hidden_field(&item?)? {
					return Ok(true);
				}
			}
			Ok(false)
		}
		_ => Ok(false),
	}
}

fn parse(key: &str, json: &str) -> Result<Val> {
	serde_json::from_str(json).map_err(|error| {
		RuntimeError(format!("rtkMemoize: failed to parse value for key {key:?}: {error}").into())
			.into()
	})
}

/// Cache a manifested JSON value process-wide under `key`.
///
/// The candidate stays lazy on a hit. On a miss, one worker computes it while
/// other workers wait, then every worker receives the same JSON projection.
#[builtin]
#[allow(non_snake_case, reason = "the native function's public name")]
pub(crate) fn rtkMemoize(key: String, value: Thunk<Val>) -> Result<Val> {
	if ACTIVE.with(|active| active.borrow().contains(&key)) {
		return Err(RuntimeError(
			format!("rtkMemoize: re-entrant evaluation of key {key:?}").into(),
		)
		.into());
	}

	loop {
		let (slot, computes) = {
			let mut values = cache().lock().unwrap_or_else(|error| error.into_inner());
			if let Some(slot) = values.get(&key) {
				(Arc::clone(slot), false)
			} else {
				let slot = Arc::new(MemoSlot {
					state: Mutex::new(MemoState::Computing),
					cond: Condvar::new(),
				});
				values.insert(key.clone(), Arc::clone(&slot));
				(slot, true)
			}
		};

		if computes {
			ACTIVE.with(|active| {
				active.borrow_mut().insert(key.clone());
			});
			let mut computing = Computing {
				key: &key,
				slot: Arc::clone(&slot),
				completed: false,
			};

			let evaluated = value.evaluate()?;
			if contains_hidden_field(&evaluated)? {
				return Err(RuntimeError(
					format!(
						"rtkMemoize: value for key {key:?} contains hidden field(s), \
						 which cannot be memoized (they would be dropped by JSON serialization)"
					)
					.into(),
				)
				.into());
			}
			let json = evaluated.manifest(JsonFormat::default())?;
			let result = parse(&key, &json)?;

			let mut state = slot.state.lock().unwrap_or_else(|error| error.into_inner());
			*state = MemoState::Done(json);
			slot.cond.notify_all();
			drop(state);
			computing.completed = true;
			return Ok(result);
		}

		let mut state = slot.state.lock().unwrap_or_else(|error| error.into_inner());
		loop {
			match &*state {
				MemoState::Done(json) => return parse(&key, json),
				MemoState::Failed => break,
				MemoState::Computing => {
					state = slot
						.cond
						.wait(state)
						.unwrap_or_else(|error| error.into_inner());
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use rtk_jsonnet_core::{Evaluator as _, Implementation as _};

	use crate::{Evaluation, Evaluator, Implementation};

	fn evaluator() -> Evaluator {
		let implementation = Implementation::new(std::iter::empty()).unwrap();
		let mut evaluator = implementation.create_evaluator();
		evaluator.with_rtk_memoize().unwrap();
		evaluator
	}

	fn evaluate(snippet: &str) -> serde_json::Value {
		let evaluation = evaluator()
			.evaluate_snippet(snippet)
			.map(Evaluation::from)
			.unwrap();
		serde_json::to_value(evaluation).unwrap()
	}

	#[test]
	fn a_hit_does_not_force_its_candidate() {
		assert_eq!(
			evaluate(
				r"{
					first: std.native('rtkMemoize')('hit-does-not-force', 'first'),
					second: std.native('rtkMemoize')('hit-does-not-force', error 'must not evaluate'),
				}"
			),
			serde_json::json!({ "first": "first", "second": "first" })
		);
	}

	#[test]
	fn separate_worker_threads_share_one_value() {
		let first = std::thread::spawn(|| {
			evaluate("std.native('rtkMemoize')('shared-across-workers', { value: 'first' })")
		})
		.join()
		.unwrap();
		let second = std::thread::spawn(|| {
			evaluate("std.native('rtkMemoize')('shared-across-workers', error 'must not evaluate')")
		})
		.join()
		.unwrap();

		assert_eq!(first, serde_json::json!({ "value": "first" }));
		assert_eq!(second, first);
	}

	#[test]
	fn hidden_fields_are_rejected_instead_of_silently_dropped() {
		let error = evaluator()
			.evaluate_snippet(
				"std.native('rtkMemoize')('hidden-field-rejected', { visible: 1, hidden:: 2 })",
			)
			.expect_err("hidden fields must not be cached as lossy JSON");
		assert!(error.to_string().contains("contains hidden field"));
	}

	#[test]
	fn a_failed_or_recursive_computation_does_not_poison_the_key() {
		let failed = evaluator().evaluate_snippet(
			"std.native('rtkMemoize')('retry-after-error', error 'first attempt fails')",
		);
		assert!(failed.is_err());

		let recursive = evaluator().evaluate_snippet(
			"std.native('rtkMemoize')('retry-after-error', std.native('rtkMemoize')('retry-after-error', 1))",
		);
		assert!(
			recursive
				.expect_err("same-key recursion must fail")
				.to_string()
				.contains("re-entrant evaluation")
		);

		assert_eq!(
			evaluate("std.native('rtkMemoize')('retry-after-error', 'recovered')"),
			serde_json::json!("recovered")
		);
	}
}
