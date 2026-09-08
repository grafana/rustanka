use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::Context;
use rtk_jsonnet_core::EvaluatorError as _;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Function;

fn sort_json_keys_numerically(value: serde_json::Value) -> serde_json::Value {
	match value {
		serde_json::Value::Object(map) => {
			let mut entries: Vec<_> = map.into_iter().collect();
			entries.sort_by(|(a, _), (b, _)| rtk_yaml::compare_string_keys(a, b));
			serde_json::Value::Object(
				entries
					.into_iter()
					.map(|(key, value)| (key, sort_json_keys_numerically(value)))
					.collect(),
			)
		}
		serde_json::Value::Array(values) => {
			serde_json::Value::Array(values.into_iter().map(sort_json_keys_numerically).collect())
		}
		other => other,
	}
}

fn manifest(json: &str) -> Result<String, String> {
	let parsed: serde_json::Value =
		serde_json::from_str(json).map_err(|error| format!("failed to parse json: {error}"))?;
	let sorted = sort_json_keys_numerically(parsed);
	let options = rtk_yaml::SerializerOptions::tanka_v3();
	let mut output = String::new();
	rtk_yaml::to_fmt_writer_with_options(&mut output, &sorted, options)
		.map_err(|error| format!("failed to serialize yaml: {error}"))?;
	if !output.ends_with('\n') {
		output.push('\n');
	}
	Ok(output)
}

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
		let output = manifest(&json).map_err(E::Error::custom)?;
		Ok(output.serialize(evaluator.create_serializer())?)
	}
}

#[cfg(test)]
mod tests {
	use super::manifest;

	#[test]
	fn naturally_sorts_keys_recursively_and_keeps_trailing_newline() {
		let output = manifest(r#"{"item10":10,"nested":{"z":0,"a":1},"item2":2}"#).unwrap();
		assert_eq!(
			output,
			"item2: 2\nitem10: 10\nnested:\n    a: 1\n    z: 0\n"
		);
	}

	#[test]
	fn preserves_yaml_v3_empty_collection_and_block_scalar_options() {
		let output = manifest(r#"{"array":[],"map":{},"text":"first\nsecond\n"}"#).unwrap();
		assert_eq!(
			output,
			"array: []\nmap: {}\ntext: |\n    first\n    second\n"
		);
	}
}
