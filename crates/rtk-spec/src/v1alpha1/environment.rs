use std::borrow::Cow;
use std::fmt::{self, Formatter};
use std::marker::PhantomData;

use k8s_openapi::ClusterResourceScope;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
	CustomResourceDefinition, CustomResourceDefinitionNames, CustomResourceDefinitionSpec,
	CustomResourceDefinitionVersion, CustomResourceValidation,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::ApiResource;
use kube::core::object::HasSpec;
use kube::core::schema::{OptionalEnum, OptionalIntOrString, StructuralSchemaRewriter};
use kube::{CustomResourceExt, Resource};
use rustc_hash::FxHashMap;
use schemars::generate::SchemaSettings;
use schemars::transform::AddNullable;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, MapAccess, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

use crate::DeepMerge;
use crate::merge_strategies;
use crate::v1alpha1::common::{JsonentImplementationOrConfig, Strategy, Versions};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Empty;

impl DeepMerge for Empty {
	#[inline]
	fn merge_from(&mut self, _: Self) {}
}

impl<'a> EnvironmentData<'a> for Empty {
	fn present() -> bool {
		false
	}
}

/// A [`CustomResource`] that defines a Tanka environment.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct Environment<'a, D: EnvironmentData<'a> = Empty> {
	pub metadata: ObjectMeta,
	pub spec: EnvironmentSpec,
	pub data: D,
	_phatom: PhantomData<&'a ()>,
}

impl Environment<'_> {
	/// Start building an [`Environment`].
	///
	/// ```
	/// # use rtk_spec::canonical::{Environment, EnvironmentSpec};
	/// let environment = Environment::new()
	///     .with_spec(EnvironmentSpec::default())
	///     .build()
	///     .unwrap();
	/// assert_eq!(environment.spec.namespace(), "default");
	/// ```
	pub fn new() -> EnvironmentBuilder<'static> {
		EnvironmentBuilder {
			metadata: ObjectMeta::default(),
			spec: None,
			data: Empty,
			_phantom: PhantomData,
		}
	}
}

/// Builds an [`Environment`], which cannot be constructed literally because it
/// is `#[non_exhaustive]`. See [`Environment::new`].
#[derive(Clone, Debug, Default)]
pub struct EnvironmentBuilder<'a, D: EnvironmentData<'a> = Empty> {
	metadata: ObjectMeta,
	spec: Option<EnvironmentSpec>,
	data: D,
	_phantom: PhantomData<&'a ()>,
}

impl<'a, D: EnvironmentData<'a>> EnvironmentBuilder<'a, D> {
	#[must_use]
	pub fn with_metadata(mut self, metadata: ObjectMeta) -> Self {
		self.metadata = metadata;
		self
	}

	#[must_use]
	pub fn with_spec(mut self, spec: EnvironmentSpec) -> Self {
		self.spec = Some(spec);
		self
	}

	/// Attach the environment's evaluated manifests, changing what kind of
	/// [`Environment`] is being built.
	#[must_use]
	pub fn with_data<D2: EnvironmentData<'a>>(self, data: D2) -> EnvironmentBuilder<'a, D2> {
		EnvironmentBuilder {
			metadata: self.metadata,
			spec: self.spec,
			data,
			_phantom: PhantomData,
		}
	}

	pub fn build(self) -> Result<Environment<'a, D>, EnvironmentBuilderError> {
		let Some(spec) = self.spec else {
			return Err(EnvironmentBuilderError::MissingSpec);
		};

		Ok(Environment {
			metadata: self.metadata,
			spec,
			data: self.data,
			_phatom: PhantomData,
		})
	}
}

/// A required part of an [`Environment`] was missing. See [`Environment::new`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EnvironmentBuilderError {
	#[error("an environment requires a spec")]
	MissingSpec,
}

impl<'a, D: EnvironmentData<'a>> Environment<'a, D> {
	pub fn without_data(self) -> Environment<'static> {
		Environment {
			metadata: self.metadata,
			spec: self.spec,
			..Default::default()
		}
	}
}

impl<'a, D: EnvironmentData<'a>> CustomResourceExt for Environment<'a, D> {
	fn crd() -> CustomResourceDefinition {
		let generate = SchemaSettings::openapi3()
			.with_transform(AddNullable::default())
			.with_transform(StructuralSchemaRewriter)
			.with_transform(OptionalEnum)
			.with_transform(OptionalIntOrString)
			.into_generator();
		let schema = serde_json::to_value(generate.into_root_schema_for::<Self>())
			.and_then(serde_json::from_value)
			.map(Some)
			.expect("valid JSONSchemaProps from schemars schema");
		CustomResourceDefinition {
			metadata: ObjectMeta {
				annotations: None,
				labels: None,
				name: Some("environments.tanka.dev".to_owned()),
				..Default::default()
			},
			spec: CustomResourceDefinitionSpec {
				group: "tanka.dev".to_owned(),
				names: CustomResourceDefinitionNames {
					categories: None,
					kind: "Environment".into(),
					plural: "environments".into(),
					short_names: None,
					singular: Some("environment".into()),
					..Default::default()
				},
				scope: "Cluster".to_owned(),
				versions: vec![CustomResourceDefinitionVersion {
					additional_printer_columns: None,
					deprecated: None,
					deprecation_warning: None,
					name: "v1alpha1".into(),
					schema: Some(CustomResourceValidation {
						open_api_v3_schema: schema,
					}),
					selectable_fields: None,
					served: true,
					storage: true,
					subresources: None,
				}],
				..Default::default()
			},
			..Default::default()
		}
	}

	fn crd_name() -> &'static str {
		"environments.tanka.dev"
	}

	fn api_resource() -> ApiResource {
		ApiResource::erase::<Self>(&())
	}

	fn shortnames() -> &'static [&'static str] {
		&[]
	}
}

impl<'a, D: EnvironmentData<'a>> DeepMerge for Environment<'a, D> {
	fn merge_from(&mut self, other: Self) {
		self.metadata.merge_from(other.metadata);
		self.spec.merge_from(other.spec);
		self.data.merge_from(other.data);
	}
}

impl<'a, D: EnvironmentData<'a>> fmt::Debug for Environment<'a, D> {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		if D::present() {
			formatter
				.debug_struct("Environment")
				.field("metadata", &self.metadata)
				.field("spec", &self.spec)
				.field("data", &self.data)
				.finish()
		} else {
			formatter
				.debug_struct("Environment")
				.field("metadata", &self.metadata)
				.field("spec", &self.spec)
				.finish()
		}
	}
}

impl<'de, D: EnvironmentData<'de>> Deserialize<'de> for Environment<'de, D> {
	fn deserialize<De>(deserializer: De) -> Result<Self, De::Error>
	where
		De: Deserializer<'de>,
	{
		#[derive(Debug)]
		struct EnvironmentDeserializing<'de, D, E, const DATA_REQUIRED: bool>
		where
			D: EnvironmentData<'de>,
			E: de::Error,
		{
			metadata: Option<ObjectMeta>,
			spec: Option<EnvironmentSpec>,
			data: Option<D>,
			has_api_version: bool,
			has_kind: bool,
			_phantom_1: PhantomData<&'de D>,
			_phantom_2: PhantomData<E>,
		}

		impl<'de, D, E, const DATA_REQUIRED: bool> Default
			for EnvironmentDeserializing<'de, D, E, DATA_REQUIRED>
		where
			D: EnvironmentData<'de>,
			E: de::Error,
		{
			fn default() -> Self {
				EnvironmentDeserializing {
					metadata: None,
					spec: None,
					data: None,
					has_api_version: false,
					has_kind: false,
					_phantom_1: PhantomData,
					_phantom_2: PhantomData,
				}
			}
		}

		impl<'de, D, E, const DATA_REQUIRED: bool>
			TryFrom<EnvironmentDeserializing<'de, D, E, DATA_REQUIRED>> for Environment<'de, D>
		where
			D: EnvironmentData<'de>,
			E: de::Error,
		{
			type Error = E;

			fn try_from(
				environment: EnvironmentDeserializing<'de, D, E, DATA_REQUIRED>,
			) -> Result<Self, Self::Error> {
				if environment.metadata.is_none() {
					return Err(<Self::Error as de::Error>::missing_field("metadata"));
				}
				if environment.spec.is_none() {
					return Err(<Self::Error as de::Error>::missing_field("spec"));
				}
				if DATA_REQUIRED && environment.data.is_none() {
					return Err(<Self::Error as de::Error>::missing_field("data"));
				}
				if !environment.has_api_version {
					return Err(<Self::Error as de::Error>::missing_field("apiVersion"));
				}
				if !environment.has_kind {
					return Err(<Self::Error as de::Error>::missing_field("kind"));
				}

				let metadata = environment.metadata.expect("metadata was checked");
				let spec = environment.spec.expect("spec was checked");
				let data = environment.data.unwrap_or_default();
				// For some reason the solver fails to narrow this without
				// explicit types to Environment<'de, D> from Environment<'_, D>
				Ok::<Environment<'de, D>, Self::Error>(Environment::<'de, D> {
					metadata,
					spec,
					data,
					..Default::default()
				})
			}
		}

		#[derive(Debug)]
		pub struct EnvironmentVisitor<'de, D, const DATA_REQUIRED: bool>(PhantomData<&'de D>)
		where
			D: EnvironmentData<'de>;

		impl<'de, D, const DATA_REQUIRED: bool> Visitor<'de> for EnvironmentVisitor<'de, D, DATA_REQUIRED>
		where
			D: EnvironmentData<'de>,
		{
			type Value = Environment<'de, D>;

			fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
				write!(formatter, "an environment with data")
			}

			fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
			where
				A: MapAccess<'de>,
			{
				let mut deserializing = EnvironmentDeserializing::<_, _, DATA_REQUIRED>::default();

				while let Some(key) = map.next_key::<String>()? {
					match key.as_str() {
						"apiVersion" => {
							if let api_version = map.next_value::<String>()?
								&& api_version != "tanka.dev/v1alpha1"
							{
								return Err(<A::Error as de::Error>::invalid_value(
									Unexpected::Str(&api_version),
									&"tanka.dev/v1alpha1",
								));
							}
							deserializing.has_api_version = true;
						}
						"kind" => {
							if let kind = map.next_value::<String>()?
								&& kind != "Environment"
							{
								return Err(<A::Error as de::Error>::invalid_value(
									Unexpected::Str(&kind),
									&"Environment",
								));
							}
							deserializing.has_kind = true;
						}
						"metadata" => deserializing.metadata = Some(map.next_value()?),
						"spec" => deserializing.spec = Some(map.next_value()?),
						"data" if DATA_REQUIRED => deserializing.data = Some(map.next_value()?),
						_ => {
							map.next_value::<de::IgnoredAny>()?;
						}
					}
				}

				Ok(deserializing.try_into()?)
			}
		}

		if D::present() {
			deserializer.deserialize_struct(
				"Environment",
				&["apiVersion", "kind", "metadata", "spec", "data"],
				EnvironmentVisitor::<'de, D, true>(PhantomData),
			)
		} else {
			deserializer.deserialize_struct(
				"Environment",
				&["apiVersion", "kind", "metadata", "spec"],
				EnvironmentVisitor::<'de, D, false>(PhantomData),
			)
		}
	}
}

impl<'a, D: EnvironmentData<'a>> HasSpec for Environment<'a, D> {
	type Spec = EnvironmentSpec;
	fn spec(&self) -> &Self::Spec {
		&self.spec
	}
	fn spec_mut(&mut self) -> &mut Self::Spec {
		&mut self.spec
	}
}

impl<'a, D: EnvironmentData<'a>> JsonSchema for Environment<'a, D> {
	fn schema_id() -> Cow<'static, str> {
		if D::present() {
			concat!(
				module_path!(),
				"::Environment<'a, { D::is_present() == true }>"
			)
			.into()
		} else {
			concat!(
				module_path!(),
				"::Environment<'a, { D::is_present() == false }>"
			)
			.into()
		}
	}

	fn schema_name() -> Cow<'static, str> {
		"Environment".into()
	}

	fn json_schema(generator: &mut SchemaGenerator) -> Schema {
		if D::present() {
			json_schema!({
				"type": "object",
				"properties": {
					"spec": generator.subschema_for::<EnvironmentSpec>(),
					"data": { "type": "object" },
				},
				"required": ["spec", "data"],
			})
		} else {
			json_schema!({
				"type": "object",
				"properties": {
					"spec": generator.subschema_for::<EnvironmentSpec>(),
				},
				"required": ["spec"],
			})
		}
	}
}

impl<'a, D: EnvironmentData<'a>> Resource for Environment<'a, D> {
	type DynamicType = ();
	type Scope = ClusterResourceScope;

	fn group(_: &Self::DynamicType) -> Cow<'_, str> {
		"tanka.dev".into()
	}
	fn kind(_: &Self::DynamicType) -> Cow<'_, str> {
		"Environment".into()
	}
	fn version(_: &Self::DynamicType) -> Cow<'_, str> {
		"v1alpha1".into()
	}
	fn api_version(_: &Self::DynamicType) -> Cow<'_, str> {
		"tanka.dev/v1alpha1".into()
	}
	fn plural(_: &Self::DynamicType) -> Cow<'_, str> {
		"environments".into()
	}
	fn meta(&self) -> &ObjectMeta {
		&self.metadata
	}
	fn meta_mut(&mut self) -> &mut ObjectMeta {
		&mut self.metadata
	}
}

impl<'a, D: EnvironmentData<'a>> Serialize for Environment<'a, D> {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		if D::present() {
			#[derive(Debug, Serialize)]
			#[serde(rename_all = "camelCase")]
			struct EnvironmentWithDataSerializing<'a, 'b, D>
			where
				D: EnvironmentData<'a>,
			{
				api_version: &'static str,
				kind: &'static str,
				metadata: &'b ObjectMeta,
				spec: &'b EnvironmentSpec,
				data: &'b D,
				_phantom: PhantomData<&'a D>,
			}

			impl<'a, 'b, D> From<&'b Environment<'a, D>> for EnvironmentWithDataSerializing<'a, 'b, D>
			where
				D: EnvironmentData<'a>,
			{
				fn from(environment: &'b Environment<'a, D>) -> Self {
					EnvironmentWithDataSerializing {
						api_version: "tanka.dev/v1alpha1",
						kind: "Environment",
						metadata: &environment.metadata,
						spec: &environment.spec,
						data: &environment.data,
						_phantom: PhantomData,
					}
				}
			}

			EnvironmentWithDataSerializing::from(self).serialize(serializer)
		} else {
			#[derive(Debug, Serialize)]
			#[serde(rename_all = "camelCase")]
			struct EnvironmentWithoutDataSerializing<'a> {
				api_version: &'static str,
				kind: &'static str,
				metadata: &'a ObjectMeta,
				spec: &'a EnvironmentSpec,
			}

			impl<'a, 'b, D> From<&'b Environment<'a, D>> for EnvironmentWithoutDataSerializing<'b>
			where
				D: EnvironmentData<'a>,
			{
				fn from(environment: &'b Environment<'a, D>) -> Self {
					EnvironmentWithoutDataSerializing {
						api_version: "tanka.dev/v1alpha1",
						kind: "Environment",
						metadata: &environment.metadata,
						spec: &environment.spec,
					}
				}
			}

			EnvironmentWithoutDataSerializing::from(self).serialize(serializer)
		}
	}
}

pub trait EnvironmentData<'a>:
	'a + Default + fmt::Debug + DeepMerge + Deserialize<'a> + Serialize
{
	fn present() -> bool;
}

/// The `spec` of an [`Environment`].
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct EnvironmentSpec {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub api_server: Option<Url>,
	#[serde(default)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub context_names: Vec<Box<str>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub namespace: Option<Box<str>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub diff_strategy: Option<Strategy>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub apply_strategy: Option<Strategy>,
	#[serde(default)]
	#[serde(skip_serializing_if = "crate::v1alpha1::common::bool_is_false")]
	pub inject_labels: bool,
	#[serde(default)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub tanka_env_label_from_fields: Vec<Box<str>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub resource_defaults: Option<ResourceDefaults>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub expect_versions: Option<Versions>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub export_jsonnet_implementation: Option<JsonentImplementationOrConfig>,
}

impl EnvironmentSpec {
	/// The namespace resources without one of their own are exported into.
	///
	/// Tanka defaults this to `default` when a `spec.json` (or an inline
	/// environment) leaves `spec.namespace` unset, so anything that injects
	/// namespaces must go through here rather than reading the field.
	pub fn namespace(&self) -> &str {
		const DEFAULT_NAMESPACE: &str = "default";

		self.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE)
	}
}

impl DeepMerge for EnvironmentSpec {
	fn merge_from(&mut self, other: Self) {
		if let Some(api_server) = other.api_server {
			self.api_server = Some(api_server);
		}

		merge_strategies::list::set(&mut self.context_names, other.context_names);

		if let Some(namespace) = other.namespace {
			self.namespace = Some(namespace);
		}

		self.diff_strategy.merge_from(other.diff_strategy);
		self.apply_strategy.merge_from(other.apply_strategy);

		self.inject_labels = self.inject_labels || other.inject_labels;

		merge_strategies::list::set(
			&mut self.tanka_env_label_from_fields,
			other.tanka_env_label_from_fields,
		);

		self.resource_defaults.merge_from(other.resource_defaults);
		self.expect_versions.merge_from(other.expect_versions);
		self.export_jsonnet_implementation
			.merge_from(other.export_jsonnet_implementation);
	}
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDefaults {
	#[serde(default)]
	#[serde(skip_serializing_if = "FxHashMap::is_empty")]
	pub annotations: FxHashMap<Box<str>, Box<str>>,
	#[serde(default)]
	#[serde(skip_serializing_if = "FxHashMap::is_empty")]
	pub labels: FxHashMap<Box<str>, Box<str>>,
}

impl DeepMerge for ResourceDefaults {
	fn merge_from(&mut self, other: Self) {
		merge_strategies::hashmap::granular(&mut self.annotations, other.annotations, |a, b| {
			*a = b
		});
		merge_strategies::hashmap::granular(&mut self.labels, other.labels, |a, b| *a = b);
	}
}
