use jrsonnet_evaluator::error::ErrorKind;
use jrsonnet_evaluator::typed::ValType;
use jrsonnet_evaluator::{Error, Thunk, Val};

use crate::serde::ValueSerializer;
use crate::{Evaluator, EvaluatorError};

pub struct Arguments<'a>(pub &'a [Option<Thunk<Val>>]);

impl<'a> rtk_jsonnet_core::Arguments<'a> for Arguments<'a> {
	type Evaluator = Evaluator;

	fn get_indexed(
		&self,
		index: usize,
	) -> Result<
		Option<<Self::Evaluator as rtk_jsonnet_core::Evaluator<'a>>::Value>,
		<Self::Evaluator as rtk_jsonnet_core::Evaluator<'a>>::Error,
	> {
		self.0
			.get(index)
			.and_then(|thunk| thunk.as_ref().and_then(|thunk| Some(thunk.evaluate())))
			.transpose()
			.and_then(|value| Ok(value.map(Value)))
			.map_err(EvaluatorError)
	}
}

pub struct Value(pub Val);

impl<'a> rtk_jsonnet_core::Value<'a> for Value {
	type Evaluator = Evaluator;

	type Serializer = ValueSerializer;

	fn get_indexed(
		&self,
		index: usize,
	) -> Result<Option<Self>, <Self::Evaluator as rtk_jsonnet_core::Evaluator<'a>>::Error> {
		if let Some(arr) = self.0.as_arr() {
			let item = arr.get(index)?;
			Ok(item.map(Value))
		} else {
			Err(EvaluatorError(Error::new(
				ErrorKind::TypeMismatch("", vec![ValType::Arr], self.0.value_type()),
			)))
		}
	}

	fn get_keyed<K>(
		&self,
		key: &K,
	) -> Result<Option<Self>, <Self::Evaluator as rtk_jsonnet_core::Evaluator<'a>>::Error>
	where
		K: AsRef<str>,
	{
		if let Some(obj) = self.0.as_obj() {
			let item = obj.get(key.as_ref().into())?;
			Ok(item.map(Value))
		} else {
			Err(EvaluatorError(Error::new(
				ErrorKind::TypeMismatch("", vec![ValType::Obj], self.0.value_type()),
			)))
		}
	}
}
