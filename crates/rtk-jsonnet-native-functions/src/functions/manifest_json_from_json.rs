use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::Context;
use rtk_jsonnet_core::EvaluatorError as _;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Function;

fn manifest(json: &str, indent: usize) -> Result<String, String> {
	let parsed: serde_json::Value =
		serde_json::from_str(json).map_err(|error| format!("failed to parse json: {error}"))?;
	let indentation = " ".repeat(indent);
	let formatter = serde_json::ser::PrettyFormatter::with_indent(indentation.as_bytes());
	let mut buffer = Vec::new();
	let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
	parsed
		.serialize(&mut serializer)
		.map_err(|error| format!("failed to serialize json: {error}"))?;
	buffer.push(b'\n');
	String::from_utf8(buffer).map_err(|error| format!("failed to convert to utf8: {error}"))
}

impl<E> jsonnet::Function<E> for Function
where
	E: jsonnet::Evaluator<Context = E> + Context<Evaluator = E>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(2, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["json", "indent"])
	}

	fn call<'b>(&self, evaluator: &E, arguments: E::Arguments) -> Result<E::Value, E::Error> {
		let (json, indent) = <(String, usize)>::deserialize(arguments)?;
		let output = manifest(&json, indent).map_err(E::Error::custom)?;
		Ok(output.serialize(evaluator.create_serializer())?)
	}
}

#[cfg(test)]
mod tests {
	use super::manifest;

	#[test]
	fn uses_requested_indentation_and_trailing_newline() {
		assert_eq!(
			manifest(r#"{"a":{"b":1}}"#, 4).unwrap(),
			"{\n    \"a\": {\n        \"b\": 1\n    }\n}\n"
		);
	}
}
