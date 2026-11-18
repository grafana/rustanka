//! Context-bound access to evaluation results. Keep the result, not a child;
//! call `with_root` again to traverse it later.

use crate::{Array, Evaluator, Hidden, Object, Value};
use serde::Deserialize;
use std::{fmt, path::Path};

type Arr<E> = <<E as Evaluator>::Value as Value>::Array;
type Obj<E> = <<E as Evaluator>::Value as Value>::Object;

/// Backend integration for context-bound traversal.
pub trait ScopedEvaluator: Evaluator {
	type Owner: From<(Self::Context, Self::Value)>;

	/// Install the owner's execution context and pass its matching root to the callback.
	fn visit_root<R>(
		owner: &Self::Owner,
		visit: impl FnOnce(&Self::Context, Self::Value) -> R,
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
		self.evaluate_file(path)
			.map(EvaluationResult::from_evaluation)
	}
	fn evaluate_snippet_result(
		self,
		snippet: impl AsRef<str> + fmt::Debug,
	) -> Result<EvaluationResult<Self>, Self::Error> {
		self.evaluate_snippet(snippet)
			.map(EvaluationResult::from_evaluation)
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
///     (ctx, value).into()
/// }
/// ```
pub struct EvaluationResult<E: ScopedEvaluator>(E::Owner);

impl<E: ScopedEvaluator> EvaluationResult<E> {
	fn from_evaluation(parts: (E::Context, E::Value)) -> Self {
		Self(parts.into())
	}
	pub fn with_root<R>(&self, visit: impl for<'s> FnOnce(ScopedValue<'s, E>) -> R) -> R {
		E::visit_root(&self.0, |context, value| {
			visit(ScopedValue { context, value })
		})
	}
	pub fn deserialize<'de, T: Deserialize<'de>>(&self) -> Result<T, E::Error> {
		self.with_root(|root| root.deserialize())
	}
	pub fn manifest(&self) -> Result<String, E::Error> {
		self.with_root(|root| root.manifest())
	}
}

/// A value supplied by a traversal, not constructible from independent parts.
/// ```compile_fail
/// use rtk_jsonnet_core::{ScopedEvaluator, ScopedValue};
/// fn assemble<E: ScopedEvaluator>(ctx: &E::Context, value: E::Value) {
///     ScopedValue::<E>::visit(ctx, value, |_| ());
/// }
/// ```
pub struct ScopedValue<'s, E: ScopedEvaluator> {
	context: &'s E::Context,
	value: E::Value,
}
impl<'s, E: ScopedEvaluator> ScopedValue<'s, E> {
	pub fn as_array(&self) -> Option<ScopedArray<'s, E>> {
		self.value.as_array().map(|array| ScopedArray {
			context: self.context,
			array,
		})
	}
	pub fn as_object(&self) -> Option<ScopedObject<'s, E>> {
		self.value.as_object().map(|object| ScopedObject {
			context: self.context,
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
		E::deserialize_data(self.context, self.value)
	}
}

pub struct ScopedArray<'s, E: ScopedEvaluator> {
	context: &'s E::Context,
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
			context: self.context,
			value,
		}))
	}
	pub fn iter(&self) -> impl Iterator<Item = Result<ScopedValue<'s, E>, E::Error>> + '_ {
		self.array.iter().map(|item| {
			item.map(|value| ScopedValue {
				context: self.context,
				value,
			})
		})
	}
}

pub struct ScopedObject<'s, E: ScopedEvaluator> {
	context: &'s E::Context,
	object: Obj<E>,
}
impl<'s, E: ScopedEvaluator> ScopedObject<'s, E> {
	pub fn has(&self, key: &str, hidden: Hidden) -> Result<bool, E::Error> {
		self.object.has(key, hidden)
	}
	pub fn get(&self, key: &str, hidden: Hidden) -> Result<Option<ScopedValue<'s, E>>, E::Error> {
		Ok(self.object.get(key, hidden)?.map(|value| ScopedValue {
			context: self.context,
			value,
		}))
	}
	pub fn get_or_bail(&self, key: &str, hidden: Hidden) -> Result<ScopedValue<'s, E>, E::Error> {
		Ok(ScopedValue {
			context: self.context,
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
				context: self.context,
				value,
			})
		})
	}
}
