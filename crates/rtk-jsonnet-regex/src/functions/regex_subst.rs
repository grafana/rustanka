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
		(3, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["regex", "src", "repl"])
	}

	fn call<'b>(&self, evaluator: &E, arguments: E::Arguments) -> Result<E::Value, E::Error> {
		let Arguments { regex, src, repl } = Arguments::deserialize(arguments)?;
		let regex = self
			.state
			.parse(&regex)
			.map_err(|error| E::Error::custom(format!("regex parse failed: {error}")))?;
		let replaced = regex.replace_all(&src, repl.as_str()).into_owned();

		replaced
			.serialize(evaluator.create_serializer())
			.map_err(Into::into)
	}
}

#[derive(Debug, Deserialize)]
struct Arguments {
	regex: String,
	src: String,
	repl: String,
}

#[cfg(test)]
mod tests {
	use regex::Regex;

	#[test]
	fn replaces_all_matches_and_expands_captures() {
		let regex = Regex::new(r"(\w+):(\w+)").unwrap();
		assert_eq!(
			regex.replace_all("one:two three:four", "$2/$1"),
			"two/one four/three"
		);
	}
}
