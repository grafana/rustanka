use std::sync::Arc;

use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::Context;
use rtk_jsonnet_core::EvaluatorError as _;
use serde::{Deserialize, Serialize};

use crate::State;

#[derive(Debug)]
pub struct Function {
	state: Arc<State>,
}

impl Function {
	pub(crate) fn new(state: Arc<State>) -> Function {
		Function { state }
	}
}

impl<E> jsonnet::Function<E> for Function
where
	E: jsonnet::Evaluator<Context = E> + Context<Evaluator = E>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(2, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["regex", "string"])
	}

	fn call<'b>(&self, evaluator: &E, arguments: E::Arguments) -> Result<E::Value, E::Error> {
		let Arguments { regex, string } = Arguments::deserialize(arguments)?;
		let regex = self
			.state
			.parse(&regex)
			.map_err(|error| E::Error::custom(format!("regex parse failed: {error}")))?;

		regex
			.is_match(&string)
			.serialize(evaluator.create_serializer())
			.map_err(Into::into)
	}
}

#[derive(Debug, Deserialize)]
struct Arguments {
	regex: String,
	string: String,
}
