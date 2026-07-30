use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::EvaluatorError as _;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Function;

fn parse(yaml: &str) -> Result<Vec<serde_json::Value>, String> {
	let options = serde_saphyr::Options {
		legacy_octal_numbers: true,
		budget: None,
		..Default::default()
	};
	serde_saphyr::from_multiple_with_options(yaml, options)
		.map_err(|error| format!("failed to parse yaml: {error}"))
}

impl<'a, E> jsonnet::Function<'a, E> for Function
where
	E: jsonnet::Evaluator<'a>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(1, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["yaml"])
	}

	fn call<'b>(
		&self,
		evaluator: &E,
		arguments: <E as jsonnet::Evaluator<'a>>::Arguments<'b>,
	) -> Result<<E as jsonnet::Evaluator<'a>>::Value, <E as jsonnet::Evaluator<'a>>::Error> {
		let (yaml,) = <(String,)>::deserialize(arguments)?;
		let documents = parse(&yaml).map_err(<E as jsonnet::Evaluator<'a>>::Error::custom)?;
		Ok(documents.serialize(evaluator.create_serializer())?)
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::parse;

	#[test]
	fn parses_multiple_documents_and_yaml_1_1_octal_numbers() {
		assert_eq!(
			parse("mode: 0755\n---\nname: second\n").unwrap(),
			vec![json!({ "mode": 493 }), json!({ "name": "second" })]
		);
	}

	#[test]
	fn empty_input_has_no_documents() {
		assert!(parse("").unwrap().is_empty());
	}
}
