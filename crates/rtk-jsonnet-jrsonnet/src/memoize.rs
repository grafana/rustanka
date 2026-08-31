//! Jrsonnet's native, thread-local implementation of `rtkMemoize`.

use std::cell::RefCell;

use jrsonnet_evaluator::error::ErrorKind::RuntimeError;
use jrsonnet_evaluator::{IStr, Thunk, Val};
use jrsonnet_macros::builtin;
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Default)]
struct Cache {
	values: FxHashMap<IStr, Val>,
	active: FxHashSet<IStr>,
}

thread_local! {
	/// Native values cannot leave the thread that created them. Keeping the
	/// cache here preserves them exactly, including lazy fields and functions,
	/// and gives every evaluator on this OS thread the same memoization scope.
	static CACHE: RefCell<Cache> = RefCell::default();
}

fn with_cache<R>(callback: impl FnOnce(&RefCell<Cache>) -> R) -> R {
	// Initialize the collector first so TLS teardown drops cached values before
	// destroying the linked list that owns their GC headers. Jrsonnet's own
	// Cc-backed TLS values rely on the same initialization order.
	jrsonnet_gcmodule::with_thread_object_space(|_| CACHE.with(callback))
}

/// Removes an in-progress marker on success, error or panic.
struct Computing(IStr);

impl Drop for Computing {
	fn drop(&mut self) {
		with_cache(|cache| cache.borrow_mut().active.remove(&self.0));
	}
}

/// Cache `value` under `key` in this OS thread's native jrsonnet cache.
///
/// Only the key is eager. A hit returns the exact cached [`Val`] graph without
/// touching `value`; a miss evaluates only its outer thunk, leaving anything
/// below the resulting value as lazy as jrsonnet made it.
#[builtin]
#[allow(non_snake_case, reason = "the native function's public name")]
pub(crate) fn rtkMemoize(key: IStr, value: Thunk<Val>) -> jrsonnet_evaluator::Result<Val> {
	if let Some(value) = with_cache(|cache| cache.borrow().values.get(&key).cloned()) {
		return Ok(value);
	}

	let computing = with_cache(|cache| cache.borrow_mut().active.insert(key.clone()));
	if !computing {
		return Err(RuntimeError(
			format!("rtkMemoize: re-entrant evaluation of key {key:?}").into(),
		)
		.into());
	}
	let _computing = Computing(key.clone());

	// No cache borrow may survive this call: the value may recursively memoize
	// another key (or attempt this key again).
	let value = value.evaluate()?;
	with_cache(|cache| {
		let mut cache = cache.borrow_mut();
		cache.values.insert(key, value.clone());
	});

	Ok(value)
}

#[cfg(test)]
pub(crate) fn clear() {
	with_cache(|cache| {
		let mut cache = cache.borrow_mut();
		assert!(cache.active.is_empty(), "cannot clear an active memo cache");
		cache.values.clear();
	});
}

#[cfg(test)]
mod tests {
	use jrsonnet_evaluator::{ObjValue, Val};
	use rtk_jsonnet_core::{Evaluator as _, Implementation as _};

	use super::*;
	use crate::{Evaluation, Evaluator, Implementation};

	fn evaluator() -> Evaluator {
		let implementation = Implementation::new(std::iter::empty()).unwrap();
		let mut evaluator = implementation.create_evaluator();
		evaluator.with_rtk_memoize().unwrap();
		evaluator
	}

	fn evaluate(snippet: &str) -> Evaluation {
		evaluator()
			.evaluate_snippet(snippet)
			.map(Evaluation::from)
			.unwrap()
	}

	fn field(object: &ObjValue, name: &str) -> Val {
		object
			.get(name.into())
			.unwrap()
			.unwrap_or_else(|| panic!("missing field {name}"))
	}

	#[test]
	fn a_hit_returns_the_same_native_lazy_value() {
		clear();
		let evaluation = evaluate(
			r"{
				first: std.native('rtkMemoize')('native-lazy', {
					visible: 1,
					hidden:: 2,
					broken: error 'must stay lazy',
				}),
				second: std.native('rtkMemoize')('native-lazy', error 'must not evaluate'),
			}",
		);
		let Val::Obj(root) = &evaluation.value().0 else {
			panic!("root is not an object");
		};
		let Val::Obj(first) = field(root, "first") else {
			panic!("first is not an object");
		};
		let Val::Obj(second) = field(root, "second") else {
			panic!("second is not an object");
		};

		assert!(ObjValue::ptr_eq(&first, &second));
		assert_eq!(field(&second, "visible").as_num(), Some(1.0));
		assert_eq!(field(&second, "hidden").as_num(), Some(2.0));
	}

	#[test]
	fn functions_keep_their_native_closure() {
		clear();
		let first = evaluate(
			"{ value: std.native('rtkMemoize')('native-function', local captured = 7; function(x) x + captured) }",
		);
		let Val::Obj(root) = &first.value().0 else {
			panic!("root is not an object");
		};
		assert!(matches!(field(root, "value"), Val::Func(_)));
		drop(first);

		let second =
			evaluate("std.native('rtkMemoize')('native-function', error 'must not evaluate')(5)");
		assert_eq!(second.value().0.as_num(), Some(12.0));
	}

	#[test]
	fn object_self_and_super_survive_their_original_evaluator() {
		clear();
		let first = evaluate(
			r"{
				value: std.native('rtkMemoize')('native-self-super',
					{ base: 3 } + {
						base+: 4,
						doubled: self.base * 2,
						original: super.base,
					}
				),
			}",
		);
		let Val::Obj(root) = &first.value().0 else {
			panic!("root is not an object");
		};
		assert!(matches!(field(root, "value"), Val::Obj(_)));
		drop(first);

		let second = evaluate(
			r"
				local value = std.native('rtkMemoize')(
					'native-self-super',
					error 'must not evaluate'
				);
				{
					base: value.base,
					doubled: value.doubled,
					original: value.original,
				}
			",
		);
		assert_eq!(
			serde_json::to_value(second).unwrap(),
			serde_json::json!({ "base": 7, "doubled": 14, "original": 3 })
		);
	}

	#[test]
	fn assertions_are_not_forced_by_caching() {
		clear();
		let evaluation = evaluate(
			r"{
				first: std.native('rtkMemoize')('native-assertion', {
					assert false : 'assertion runs on access',
					value: 1,
				}),
				second: std.native('rtkMemoize')('native-assertion', error 'must not evaluate'),
			}",
		);
		let Val::Obj(root) = &evaluation.value().0 else {
			panic!("root is not an object");
		};
		let Val::Obj(first) = field(root, "first") else {
			panic!("first is not an object");
		};
		let Val::Obj(second) = field(root, "second") else {
			panic!("second is not an object");
		};

		assert!(ObjValue::ptr_eq(&first, &second));
		let error = first
			.get("value".into())
			.expect_err("assertion should fail");
		assert!(error.to_string().contains("assertion runs on access"));

		// A failure below the cached value does not evict the value itself.
		let third = evaluate(
			"std.native('rtkMemoize')('native-assertion', error 'still must not evaluate')",
		);
		let Val::Obj(third) = &third.value().0 else {
			panic!("third is not an object");
		};
		assert!(ObjValue::ptr_eq(&first, third));
	}

	#[test]
	fn separate_evaluators_on_one_thread_share_the_cache() {
		clear();
		let first = evaluate("std.native('rtkMemoize')('same-thread', 'first')");
		assert_eq!(first.value().0.as_str().unwrap().to_string(), "first");

		let second = evaluate("std.native('rtkMemoize')('same-thread', error 'must not evaluate')");
		assert_eq!(second.value().0.as_str().unwrap().to_string(), "first");
	}

	#[test]
	fn separate_os_threads_have_separate_caches() {
		let threads = ["left", "right"].map(|value| {
			std::thread::spawn(move || {
				clear();
				let snippet =
					format!("std.native('rtkMemoize')('per-thread', {{ value: '{value}' }})");
				let evaluation = evaluate(&snippet);
				let Val::Obj(value) = &evaluation.value().0 else {
					panic!("cached value is not an object");
				};
				field(value, "value").as_str().unwrap().to_string()
			})
		});
		let [left, right] = threads.map(|thread| thread.join().unwrap());

		assert_eq!(left, "left");
		assert_eq!(right, "right");
	}

	#[test]
	fn a_panicking_computation_does_not_poison_the_key() {
		clear();
		let key = IStr::from("panic-retry");
		let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			let value = Thunk!(move || panic!("candidate panicked"));
			let _ = rtkMemoize(key.clone(), value);
		}));
		assert!(panic.is_err());

		let recovered = rtkMemoize(
			"panic-retry".into(),
			Thunk::evaluated(Val::Num(1_u8.into())),
		)
		.unwrap();
		assert_eq!(recovered.as_num(), Some(1.0));
	}

	#[test]
	fn failed_and_recursive_computations_do_not_poison_the_key() {
		clear();
		let failed = evaluator()
			.evaluate_snippet("std.native('rtkMemoize')('retry', error 'first attempt fails')");
		assert!(failed.is_err());

		let recursive = evaluator().evaluate_snippet(
			"std.native('rtkMemoize')('retry', std.native('rtkMemoize')('retry', 1))",
		);
		let error = recursive.expect_err("same-key recursion should fail");
		assert!(error.to_string().contains("re-entrant evaluation"));

		let recovered = evaluate("std.native('rtkMemoize')('retry', 'recovered')");
		assert_eq!(
			recovered.value().0.as_str().unwrap().to_string(),
			"recovered"
		);
	}

	#[test]
	fn distinct_keys_do_not_collide() {
		clear();
		let evaluation = evaluate(
			r"
				local memo = std.native('rtkMemoize');
				{
					first: memo('distinct-first', 'one'),
					second: memo('distinct-second', 'two'),
					again: memo('distinct-first', error 'must not evaluate'),
				}
			",
		);

		let Val::Obj(object) = &evaluation.value().0 else {
			panic!("result is not an object");
		};
		let text = |name| field(object, name).as_str().unwrap().to_string();
		assert_eq!(text("first"), "one");
		assert_eq!(text("second"), "two");
		assert_eq!(text("again"), "one");
	}

	#[test]
	fn different_keys_can_be_computed_recursively() {
		clear();
		let evaluation =
			evaluate("std.native('rtkMemoize')('outer', std.native('rtkMemoize')('inner', 7))");
		assert_eq!(evaluation.value().0.as_num(), Some(7.0));
	}

	#[test]
	fn runtime_demand_decides_which_value_wins() {
		clear();
		let evaluation = evaluate(
			r"
				local memo = std.native('rtkMemoize');
				local first = memo('demand-order', 'first');
				local second = memo('demand-order', 'second');
				[second, first]
			",
		);
		let Val::Arr(values) = &evaluation.value().0 else {
			panic!("result is not an array");
		};
		let values = values
			.iter()
			.map(|value| value.unwrap().as_str().unwrap().to_string())
			.collect::<Vec<_>>();
		assert_eq!(values, ["second", "second"]);
	}

	#[test]
	fn tailstrict_can_force_the_candidate_before_the_native_call() {
		clear();
		let evaluation = evaluate(
			r"
				local memo = std.native('rtkMemoize');
				[
					memo('tailstrict', 'cached'),
					memo('tailstrict', error 'tailstrict forced the candidate') tailstrict,
				]
			",
		);
		let error =
			serde_json::to_value(evaluation).expect_err("tailstrict should force the error");
		assert!(
			error
				.to_string()
				.contains("tailstrict forced the candidate")
		);
	}
}
