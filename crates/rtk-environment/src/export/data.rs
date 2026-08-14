use rtk_jsonnet::{EvaluationValue, RawEvaluationValue};
use rtk_spec::DeepMerge;
use rtk_spec::v1alpha1::EnvironmentData;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The manifests of an environment, as evaluated but not yet walked.
///
/// Optional because an environment can legitimately evaluate to `null`, and
/// because a static environment's spec is read from `spec.json` before anything
/// is evaluated, so it starts out with none.
///
/// Deserializing this captures the manifests without walking them: nothing
/// beneath them is forced until they are exported. The captured value carries the
/// evaluation context it needs, so an environment can be handed around (and
/// exported) without keeping the evaluation itself alive separately.
#[derive(Clone, Debug, Default)]
pub struct OptionalData(Option<EvaluationValue>);

impl OptionalData {
	pub fn new(value: EvaluationValue) -> OptionalData {
		OptionalData(Some(value))
	}

	pub fn none() -> OptionalData {
		OptionalData(None)
	}

	/// The manifests, if the environment has any.
	pub fn get(&self) -> Option<&EvaluationValue> {
		self.0.as_ref()
	}

	pub fn into_inner(self) -> Option<EvaluationValue> {
		self.0
	}
}

impl From<EvaluationValue> for OptionalData {
	fn from(value: EvaluationValue) -> Self {
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
		// `Option`'s implementation asks for `deserialize_option`, which the
		// Jsonnet deserializer answers by visiting `some` for everything but
		// `null` — exactly the distinction wanted here.
		let Some(raw) = Option::<RawEvaluationValue>::deserialize(deserializer)? else {
			return Ok(OptionalData::none());
		};

		EvaluationValue::current(raw)
			.map(OptionalData::new)
			.ok_or_else(|| {
				D::Error::custom(
					"an environment's data can only be deserialized while its evaluation's \
					 context is in effect",
				)
			})
	}
}

impl Serialize for OptionalData {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		match self.0.as_ref() {
			// Serializing manifests happens through
			// [`EvaluationValue::manifest_into`] on the export path; this exists
			// for the sake of round-tripping an environment as a document.
			Some(value) => value.serialize(serializer),
			None => serializer.serialize_none(),
		}
	}
}
