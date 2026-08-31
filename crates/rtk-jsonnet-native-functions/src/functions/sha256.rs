use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub struct Function;

fn hash(value: &str) -> String {
	hex::encode(Sha256::digest(value.as_bytes()))
}

impl<E> jsonnet::Function<E> for Function
where
	E: jsonnet::Evaluator<Context = E> + Context<Evaluator = E>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(1, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["str"])
	}

	fn call<'b>(&self, evaluator: &E, arguments: E::Arguments) -> Result<E::Value, E::Error> {
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
