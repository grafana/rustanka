use rtk_spec::DeepMerge;
use rtk_spec::v1alpha1::EnvironmentData;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The manifests of an environment, as materialized JSON.
///
/// Optional because an environment can legitimately evaluate to `null`, and
/// because a static environment's spec is read from `spec.json` before anything
/// is evaluated, so it starts out with none.
#[derive(Clone, Debug, Default)]
pub struct OptionalData(Option<serde_json::Value>);

impl OptionalData {
	pub fn new(value: serde_json::Value) -> OptionalData {
		OptionalData(Some(value))
	}

	pub fn none() -> OptionalData {
		OptionalData(None)
	}

	/// The manifests, if the environment has any.
	pub fn get(&self) -> Option<&serde_json::Value> {
		self.0.as_ref()
	}

	pub fn into_inner(self) -> Option<serde_json::Value> {
		self.0
	}
}

impl From<serde_json::Value> for OptionalData {
	fn from(value: serde_json::Value) -> Self {
		OptionalData::new(value)
	}
}

impl DeepMerge for OptionalData {
	/// Manifests are opaque, so merging can only replace them.
	fn merge_from(&mut self, other: Self) {
		if other.0.is_some() {
			*self = other;
		}
	}
}

impl EnvironmentData<'_> for OptionalData {
	fn present() -> bool {
		true
	}
}

impl<'de> Deserialize<'de> for OptionalData {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		// `Option`'s implementation asks for `deserialize_option`, which answers
		// by visiting `some` for everything but `null` — exactly the distinction
		// wanted here, since an environment may evaluate to `null`.
		Ok(
			match Option::<serde_json::Value>::deserialize(deserializer)? {
				Some(serde_json::Value::Null) | None => OptionalData::none(),
				Some(value) => OptionalData::new(value),
			},
		)
	}
}

impl Serialize for OptionalData {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		match self.0.as_ref() {
			Some(value) => value.serialize(serializer),
			None => serializer.serialize_none(),
		}
	}
}
