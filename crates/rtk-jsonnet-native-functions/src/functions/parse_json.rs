use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::EvaluatorError as _;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Function;

impl<'a, E> jsonnet::Function<'a, E> for Function
where
	E: jsonnet::Evaluator<'a>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(1, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["json"])
	}

	fn call<'b>(
		&self,
		evaluator: &E,
		arguments: <E as jsonnet::Evaluator<'a>>::Arguments<'b>,
	) -> Result<<E as jsonnet::Evaluator<'a>>::Value, <E as jsonnet::Evaluator<'a>>::Error> {
		let (json,) = <(String,)>::deserialize(arguments)?;
		let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|error| {
			<E as jsonnet::Evaluator<'a>>::Error::custom(format!("failed to parse json: {error}"))
		})?;
		Ok(parsed.serialize(evaluator.create_serializer())?)
	}
}
