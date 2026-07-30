use rtk_jsonnet_core as jsonnet;
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
		Some(&["str"])
	}

	fn call<'b>(
		&self,
		evaluator: &E,
		arguments: <E as jsonnet::Evaluator<'a>>::Arguments<'b>,
	) -> Result<<E as jsonnet::Evaluator<'a>>::Value, <E as jsonnet::Evaluator<'a>>::Error> {
		let Arguments { pattern } = Arguments::deserialize(arguments)?;
		escape(&pattern)
			.serialize(evaluator.create_serializer())
			.map_err(Into::into)
	}
}

#[derive(Debug, Deserialize)]
struct Arguments {
	pattern: String,
}

fn escape(pattern: &str) -> String {
	const GO_META: &str = r"\.+*?()|[]{}^$";
	let mut escaped = String::with_capacity(pattern.len() * 2);
	for ch in pattern.chars() {
		if GO_META.contains(ch) {
			escaped.push('\\');
		}
		escaped.push(ch);
	}
	escaped
}

#[cfg(test)]
mod tests {
	use super::escape;

	#[test]
	fn escapes_exact_go_quote_meta_set() {
		assert_eq!(escape(r"\.+*?()|[]{}^$-"), r"\\\.\+\*\?\(\)\|\[\]\{\}\^\$-");
	}

	#[test]
	fn preserves_unicode_and_non_meta_punctuation() {
		assert_eq!(escape("hello-world_你好"), "hello-world_你好");
	}
}
