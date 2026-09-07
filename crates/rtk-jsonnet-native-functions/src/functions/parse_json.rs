use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::Context;
use rtk_jsonnet_core::EvaluatorError as _;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Function;

impl<E> jsonnet::Function<E> for Function
where
	E: jsonnet::Evaluator<Context = E> + Context<Evaluator = E>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(1, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["json"])
	}

	fn call<'b>(&self, evaluator: &E, arguments: E::Arguments) -> Result<E::Value, E::Error> {
		let (json,) = <(String,)>::deserialize(arguments)?;
		let parsed: serde_json::Value = serde_json::from_str(&json)
			.map_err(|error| E::Error::custom(format!("failed to parse json: {error}")))?;
		Ok(parsed.serialize(evaluator.create_serializer())?)
	}
}
