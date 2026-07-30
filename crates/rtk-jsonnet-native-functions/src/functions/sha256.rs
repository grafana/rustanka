use rtk_jsonnet_core as jsonnet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub struct Function;

fn hash(value: &str) -> String {
	hex::encode(Sha256::digest(value.as_bytes()))
}

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
		let (value,) = <(String,)>::deserialize(arguments)?;
		Ok(hash(&value).serialize(evaluator.create_serializer())?)
	}
}

#[cfg(test)]
mod tests {
	use super::hash;

	#[test]
	fn produces_lowercase_sha256() {
		assert_eq!(
			hash("foo"),
			"2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae"
		);
	}
}
