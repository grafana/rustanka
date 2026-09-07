use std::cmp::Ordering;

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
			entries.sort_by(|(a, _), (b, _)| yaml_v3_key_compare(a, b));
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

fn yaml_v3_key_compare(a: &str, b: &str) -> Ordering {
	let ar: Vec<char> = a.chars().collect();
	let br: Vec<char> = b.chars().collect();
	let mut digits = false;

	for i in 0..ar.len().min(br.len()) {
		if ar[i] == br[i] {
			digits = ar[i].is_ascii_digit();
			continue;
		}

		let al = ar[i].is_alphabetic();
		let bl = br[i].is_alphabetic();
		if al && bl {
			return ar[i].cmp(&br[i]);
		}
		if al || bl {
			return if digits {
				if al {
					Ordering::Less
				} else {
					Ordering::Greater
				}
			} else if bl {
				Ordering::Less
			} else {
				Ordering::Greater
			};
		}

		let mut an: i64 = 0;
		let mut bn: i64 = 0;
		if ar[i] == '0' || br[i] == '0' {
			let mut j = i;
			while j > 0 && ar[j - 1].is_ascii_digit() {
				j -= 1;
				if ar[j] != '0' {
					an = 1;
					bn = 1;
					break;
				}
			}
		}

		let mut ai = i;
		while ai < ar.len() && ar[ai].is_ascii_digit() {
			an = an * 10 + (ar[ai] as i64 - '0' as i64);
			ai += 1;
		}
		let mut bi = i;
		while bi < br.len() && br[bi].is_ascii_digit() {
			bn = bn * 10 + (br[bi] as i64 - '0' as i64);
			bi += 1;
		}

		if an != bn {
			return an.cmp(&bn);
		}
		if ai != bi {
			return ai.cmp(&bi);
		}
		return ar[i].cmp(&br[i]);
	}

	ar.len().cmp(&br.len())
}

fn manifest(json: &str) -> Result<String, String> {
	let parsed: serde_json::Value =
		serde_json::from_str(json).map_err(|error| format!("failed to parse json: {error}"))?;
	let sorted = sort_json_keys_numerically(parsed);
	let options = serde_saphyr::SerializerOptions {
		indent_step: 4,
		indent_array: None,
		prefer_block_scalars: true,
		empty_map_as_braces: true,
		empty_array_as_brackets: true,
		block_scalar_indent_in_seq: Some(2),
		line_width: None,
		scientific_notation_threshold: Some(1_000_000),
		scientific_notation_small_threshold: Some(0.0001),
		quote_numeric_strings: true,
		..Default::default()
	};
	let mut output = String::new();
	serde_saphyr::to_fmt_writer_with_options(&mut output, &sorted, options)
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
