//! Runtime dispatch over the core traversal API. Every arm forwards its
//! backend's already-bound result; the engine never handles detached parts.
use std::fmt;

use rtk_jsonnet_core::{self as core, Hidden};
use rtk_jsonnet_jrsonnet::Evaluator as Jr;
use rtk_jsonnet_reference::Evaluator as Ref;
use serde::{Deserialize, Serialize};

use crate::Error;

enum Backend<J, R> {
	Jr(J),
	Ref(R),
}

macro_rules! dispatch {
	($value:expr, |$v:ident, $wrap:ident| $body:expr) => {
		match $value {
			Backend::Jr($v) => {
				use Backend::Jr as $wrap;
				$body
			}
			Backend::Ref($v) => {
				use Backend::Ref as $wrap;
				$body
			}
		}
	};
	($value:expr, |$v:ident| $body:expr) => {
		match $value {
			Backend::Jr($v) => $body,
			Backend::Ref($v) => $body,
		}
	};
}

impl<T, J: Iterator<Item = T>, R: Iterator<Item = T>> Iterator for Backend<J, R> {
	type Item = T;
	fn next(&mut self) -> Option<T> {
		dispatch!(self, |it| it.next())
	}
}

/// A root or explicitly retained child, inseparable from its original owner.
pub struct Evaluation(Backend<core::EvaluationResult<Jr>, core::EvaluationResult<Ref>>);
impl fmt::Debug for Evaluation {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Evaluation").finish_non_exhaustive()
	}
}
impl Evaluation {
	pub(crate) fn jr(root: core::EvaluationResult<Jr>) -> Self {
		Self(Backend::Jr(root))
	}
	pub(crate) fn reference(root: core::EvaluationResult<Ref>) -> Self {
		Self(Backend::Ref(root))
	}
	#[cfg(test)]
	pub(crate) fn is_reference(&self) -> bool {
		matches!(self.0, Backend::Ref(_))
	}

	/// Visit the root while its own context is active. Return owned data, or
	/// explicitly [`retain`](ScopedValue::retain) a subtree for a later scope.
	/// ```compile_fail
	/// use rtk_jsonnet::Evaluation;
	/// fn escape(e: &Evaluation) {
	///     let child = e.with_root(|root| root.as_array().unwrap().get(0).unwrap());
	/// }
	/// ```
	pub fn with_root<R>(&self, visit: impl for<'s> FnOnce(ScopedValue<'s>) -> R) -> R {
		dispatch!(&self.0, |e, wrap| e
			.with_root(|v| visit(ScopedValue(wrap(v)))))
	}
}
impl Serialize for Evaluation {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		dispatch!(&self.0, |e| e.serialize(serializer))
	}
}

pub struct ScopedValue<'s>(Backend<core::ScopedValue<'s, Jr>, core::ScopedValue<'s, Ref>>);
pub struct ScopedArray<'s>(Backend<core::ScopedArray<'s, Jr>, core::ScopedArray<'s, Ref>>);
pub struct ScopedObject<'s>(Backend<core::ScopedObject<'s, Jr>, core::ScopedObject<'s, Ref>>);

impl<'s> ScopedValue<'s> {
	/// Keep this subtree without forcing it. No target owner can be supplied.
	/// ```compile_fail
	/// use rtk_jsonnet::{Evaluation, ScopedValue};
	/// fn rebind(other: &Evaluation, child: ScopedValue<'_>) -> Evaluation {
	///     child.retain(other)
	/// }
	/// ```
	pub fn retain(self) -> Evaluation {
		dispatch!(self.0, |v, wrap| Evaluation(wrap(v.into_owned())))
	}
	pub fn as_array(&self) -> Option<ScopedArray<'s>> {
		dispatch!(&self.0, |v, wrap| v
			.as_array()
			.map(|a| ScopedArray(wrap(a))))
	}
	pub fn as_object(&self) -> Option<ScopedObject<'s>> {
		dispatch!(&self.0, |v, wrap| v
			.as_object()
			.map(|o| ScopedObject(wrap(o))))
	}
	pub fn as_str(&self) -> Option<String> {
		dispatch!(&self.0, |v| v.as_str())
	}
	pub fn as_number(&self) -> Option<f64> {
		dispatch!(&self.0, |v| v.as_number())
	}
	pub fn as_bool(&self) -> Option<bool> {
		dispatch!(&self.0, |v| v.as_bool())
	}
	pub fn is_null(&self) -> bool {
		dispatch!(&self.0, |v| v.is_null())
	}
	pub fn manifest_into(&self, buffer: &mut String) -> Result<(), Error> {
		dispatch!(&self.0, |v| Ok(v.manifest_into(buffer)?))
	}
	pub fn manifest(&self) -> Result<String, Error> {
		dispatch!(&self.0, |v| Ok(v.manifest()?))
	}
	pub fn deserialize<'de, T: Deserialize<'de>>(self) -> Result<T, Error> {
		dispatch!(self.0, |v| Ok(v.deserialize()?))
	}
}
impl<'s> ScopedArray<'s> {
	pub fn len(&self) -> usize {
		dispatch!(&self.0, |a| a.len())
	}
	pub fn is_empty(&self) -> bool {
		dispatch!(&self.0, |a| a.is_empty())
	}
	pub fn get(&self, i: usize) -> Result<Option<ScopedValue<'s>>, Error> {
		dispatch!(&self.0, |a, wrap| Ok(a
			.get(i)?
			.map(|v| ScopedValue(wrap(v)))))
	}
	pub fn iter(&self) -> impl Iterator<Item = Result<ScopedValue<'s>, Error>> + '_ {
		dispatch!(&self.0, |a, wrap| wrap(
			a.iter().map(|v| Ok(ScopedValue(wrap(v?))))
		))
	}
}
impl<'s> ScopedObject<'s> {
	pub fn has(&self, key: &str, hidden: Hidden) -> Result<bool, Error> {
		dispatch!(&self.0, |o| Ok(o.has(key, hidden)?))
	}
	pub fn get(&self, key: &str, hidden: Hidden) -> Result<Option<ScopedValue<'s>>, Error> {
		dispatch!(&self.0, |o, wrap| Ok(o
			.get(key, hidden)?
			.map(|v| ScopedValue(wrap(v)))))
	}
	pub fn get_or_bail(&self, key: &str, hidden: Hidden) -> Result<ScopedValue<'s>, Error> {
		dispatch!(&self.0, |o, wrap| Ok(ScopedValue(wrap(
			o.get_or_bail(key, hidden)?
		))))
	}
	pub fn field_names(&self, hidden: Hidden) -> Vec<String> {
		dispatch!(&self.0, |o| o.field_names(hidden))
	}
	pub fn run_assertions(&self) -> Result<(), Error> {
		dispatch!(&self.0, |o| Ok(o.run_assertions()?))
	}
	pub fn values(&self) -> impl Iterator<Item = Result<ScopedValue<'s>, Error>> + '_ {
		dispatch!(&self.0, |o, wrap| wrap(
			o.values().map(|v| Ok(ScopedValue(wrap(v?))))
		))
	}
}
