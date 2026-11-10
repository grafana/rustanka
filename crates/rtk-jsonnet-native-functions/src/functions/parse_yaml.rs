use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::Context;
use rtk_jsonnet_core::EvaluatorError as _;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Function;

fn parse(yaml: &str) -> Result<Vec<serde_json::Value>, String> {
	let options = serde_saphyr::options! {
		legacy_octal_numbers: true,
		budget: None,
	};
	rtk_yaml::parse_documents(yaml, options)
		.map_err(|error| format!("failed to parse yaml: {error}"))
}

impl<E> jsonnet::Function<E> for Function
where
	E: jsonnet::Evaluator<Context = E> + Context<Evaluator = E>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(1, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["yaml"])
	}

	fn call<'b>(&self, evaluator: &E, arguments: E::Arguments) -> Result<E::Value, E::Error> {
		let (yaml,) = <(String,)>::deserialize(arguments)?;
		let documents = parse(&yaml).map_err(E::Error::custom)?;
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

	#[test]
	fn quoted_continuations_can_align_with_the_mapping_key() {
		let yaml = "properties:\n  description: 'first line\n  second line'\n  title: \"third line\n  fourth line\"\n";
		assert_eq!(
			parse(yaml).unwrap(),
			vec![json!({ "properties": {
				"description": "first line second line",
				"title": "third line fourth line",
			} })]
		);
	}
}
