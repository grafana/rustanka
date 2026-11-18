//! Context-bound access to evaluation results. Re-enter a root with `with_root`;
//! use `ScopedValue::into_owned` to retain just a subtree for later.

use crate::{Array, Evaluator, Hidden, Object, Value};
use serde::Deserialize;
use std::{fmt, path::Path};

type Arr<E> = <<E as Evaluator>::Value as Value>::Array;
type Obj<E> = <<E as Evaluator>::Value as Value>::Object;

/// Backend integration for context-bound traversal.
pub trait ScopedEvaluator: Evaluator {
	/// Install the owner's execution context and pass its matching root to the callback.
	fn visit_root<R>(
		owner: &Self::Owner,
		visit: RootVisitor<impl for<'s> FnOnce(ScopedValue<'s, Self>) -> R>,
	) -> R;

	/// Deserialize through serde's data model, without transferring native values
	/// (including nested values) through the raw-value protocol.
	fn deserialize_data<'de, T: Deserialize<'de>>(
		context: &Self::Context,
		value: Self::Value,
	) -> Result<T, Self::Error>;

	fn evaluate_file_result(
		self,
		path: impl AsRef<Path> + fmt::Debug,
	) -> Result<EvaluationResult<Self>, Self::Error> {
		self.evaluate_file(path).map(EvaluationResult::from_owner)
	}
	fn evaluate_snippet_result(
		self,
		snippet: impl AsRef<str> + fmt::Debug,
	) -> Result<EvaluationResult<Self>, Self::Error> {
		self.evaluate_snippet(snippet)
			.map(EvaluationResult::from_owner)
	}
}

/// An owned, reusable root. Its backend value and context are not separable.
///
/// ```
/// use rtk_jsonnet_core::{EvaluationResult, Hidden, ScopedEvaluator};
/// use serde::Deserialize;
/// fn read_field<'de, E: ScopedEvaluator, T: Deserialize<'de>>(
///     result: &EvaluationResult<E>, key: &str,
/// ) -> Result<Option<T>, E::Error> {
///     result.with_root(|root| match root.as_object() {
///         Some(object) => object.get(key, Hidden::Skip)?.map(|v| v.deserialize()).transpose(),
///         None => Ok(None),
///     })
/// }
/// ```
/// ```compile_fail
/// use rtk_jsonnet_core::{EvaluationResult, ScopedEvaluator};
/// fn escape<E: ScopedEvaluator>(result: &EvaluationResult<E>) {
///     let _child = result.with_root(|root| root.as_array().unwrap().get(0).unwrap());
/// }
/// ```
/// ```compile_fail
/// use rtk_jsonnet_core::{EvaluationResult, ScopedEvaluator};
/// fn detach<E: ScopedEvaluator>(result: &EvaluationResult<E>) -> E::Value {
///     result.with_root(|root| root.value.clone())
/// }
/// ```
///
/// Only an evaluation can supply a matching context and root:
/// ```compile_fail
/// use rtk_jsonnet_core::{EvaluationResult, ScopedEvaluator};
/// fn assemble<E: ScopedEvaluator>(ctx: E::Context, value: E::Value) -> EvaluationResult<E> {
///     EvaluationResult::from_owner((ctx, value).into())
/// }
/// ```
pub struct EvaluationResult<E: ScopedEvaluator>(E::Owner);

impl<E: ScopedEvaluator> EvaluationResult<E> {
	pub fn from_owner(owner: E::Owner) -> Self {
		Self(owner)
	}
	pub fn with_root<R>(&self, visit: impl for<'s> FnOnce(ScopedValue<'s, E>) -> R) -> R {
		E::visit_root(&self.0, RootVisitor(visit))
	}
	pub fn deserialize<'de, T: Deserialize<'de>>(&self) -> Result<T, E::Error> {
		self.with_root(|root| root.deserialize())
	}
	pub fn manifest(&self) -> Result<String, E::Error> {
		self.with_root(|root| root.manifest())
	}
}

impl<E: ScopedEvaluator> serde::Serialize for EvaluationResult<E>
where
	E::Value: serde::Serialize,
{
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		self.with_root(|root| root.value.serialize(serializer))
	}
}

/// Backend end of a traversal. Only core can wrap a caller's callback; the
/// caller receives a scoped view, never the backend arguments to `visit`.
pub struct RootVisitor<F>(F);

impl<F> RootVisitor<F> {
	pub fn visit<E: ScopedEvaluator, R>(
		self,
		context: &E::Context,
		value: E::Value,
		retain: &dyn Fn(E::Value) -> E::Owner,
	) -> R
	where
		F: for<'s> FnOnce(ScopedValue<'s, E>) -> R,
	{
		(self.0)(ScopedValue {
			scope: &Scope { context, retain },
			value,
		})
	}
}

struct Scope<'s, E: ScopedEvaluator> {
	context: &'s E::Context,
	retain: &'s dyn Fn(E::Value) -> E::Owner,
}

/// A value supplied by a traversal, not constructible from independent parts.
/// ```compile_fail
/// use rtk_jsonnet_core::{ScopedEvaluator, ScopedValue};
/// fn assemble<E: ScopedEvaluator>(ctx: &E::Context, value: E::Value) {
///     ScopedValue::<E>::visit(ctx, value, |_| ());
/// }
/// ```
pub struct ScopedValue<'s, E: ScopedEvaluator> {
	scope: &'s Scope<'s, E>,
	value: E::Value,
}
impl<'s, E: ScopedEvaluator> ScopedValue<'s, E> {
	/// Keep this subtree without forcing it or detaching it from its owner.
	pub fn into_owned(self) -> EvaluationResult<E> {
		EvaluationResult::from_owner((self.scope.retain)(self.value))
	}
	pub fn as_array(&self) -> Option<ScopedArray<'s, E>> {
		self.value.as_array().map(|array| ScopedArray {
			scope: self.scope,
			array,
		})
	}
	pub fn as_object(&self) -> Option<ScopedObject<'s, E>> {
		self.value.as_object().map(|object| ScopedObject {
			scope: self.scope,
			object,
		})
	}
	/// Copy the string out of backend storage before leaving the scope.
	pub fn as_str(&self) -> Option<String> {
		self.value.as_str().as_deref().map(str::to_owned)
	}
	pub fn as_number(&self) -> Option<f64> {
		self.value.as_number()
	}
	pub fn as_bool(&self) -> Option<bool> {
		self.value.as_bool()
	}
	pub fn is_null(&self) -> bool {
		self.value.is_null()
	}
	pub fn manifest_into(&self, buffer: &mut String) -> Result<(), E::Error> {
		self.value.manifest_into(buffer)
	}
	pub fn manifest(&self) -> Result<String, E::Error> {
		self.value.manifest()
	}
	pub fn deserialize<'de, T: Deserialize<'de>>(self) -> Result<T, E::Error> {
		E::deserialize_data(self.scope.context, self.value)
	}
}

pub struct ScopedArray<'s, E: ScopedEvaluator> {
	scope: &'s Scope<'s, E>,
	array: Arr<E>,
}
impl<'s, E: ScopedEvaluator> ScopedArray<'s, E> {
	pub fn len(&self) -> usize {
		self.array.len()
	}
	pub fn is_empty(&self) -> bool {
		self.array.is_empty()
	}
	pub fn get(&self, index: usize) -> Result<Option<ScopedValue<'s, E>>, E::Error> {
		Ok(self.array.get(index)?.map(|value| ScopedValue {
			scope: self.scope,
			value,
		}))
	}
	pub fn iter(&self) -> impl Iterator<Item = Result<ScopedValue<'s, E>, E::Error>> + '_ {
		self.array.iter().map(|item| {
			item.map(|value| ScopedValue {
				scope: self.scope,
				value,
			})
		})
	}
}

pub struct ScopedObject<'s, E: ScopedEvaluator> {
	scope: &'s Scope<'s, E>,
	object: Obj<E>,
}
impl<'s, E: ScopedEvaluator> ScopedObject<'s, E> {
	pub fn has(&self, key: &str, hidden: Hidden) -> Result<bool, E::Error> {
		self.object.has(key, hidden)
	}
	pub fn get(&self, key: &str, hidden: Hidden) -> Result<Option<ScopedValue<'s, E>>, E::Error> {
		Ok(self.object.get(key, hidden)?.map(|value| ScopedValue {
			scope: self.scope,
			value,
		}))
	}
	pub fn get_or_bail(&self, key: &str, hidden: Hidden) -> Result<ScopedValue<'s, E>, E::Error> {
		Ok(ScopedValue {
			scope: self.scope,
			value: self.object.get_or_bail(key, hidden)?,
		})
	}
	/// Owned field names, independent of the backend's string storage.
	pub fn field_names(&self, hidden: Hidden) -> Vec<String> {
		self.object
			.field_names(hidden)
			.into_iter()
			.map(|name| (*name).to_owned())
			.collect()
	}
	pub fn run_assertions(&self) -> Result<(), E::Error> {
		self.object.run_assertions()
	}
	pub fn values(&self) -> impl Iterator<Item = Result<ScopedValue<'s, E>, E::Error>> + '_ {
		self.object.values().map(|item| {
			item.map(|value| ScopedValue {
				scope: self.scope,
				value,
			})
		})
	}
}
