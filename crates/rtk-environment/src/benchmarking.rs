//! Inputs and reachable internals for the benchmarks.
//!
//! Behind the `benchmarking` feature, because the filename machinery these reach
//! into is the export's own business: it is exposed here to be measured, not to
//! be used.

use rtk_spec::canonical::{Empty, Environment, EnvironmentSpec};
use rustc_hash::FxHashMap;
use serde_json::json;

use std::borrow::Cow;

use crate::export::template::{self, FilenameTemplate};

/// A template with an environment's values already baked in.
///
/// Wrapped rather than exposed: rendering filenames is the export's own
/// business, and this is here to be measured.
pub struct Specialized(template::SpecializedTemplate);

impl Specialized {
	pub fn render(&self, manifest: &serde_json::Value) -> String {
		self.0.render(manifest).expect("a filename")
	}
}

/// Make a path segment safe to write, as exporting does.
pub fn sanitize(segment: &str) -> Cow<'_, str> {
	template::sanitize(segment)
}

/// Compile `format` and bake in the values of an environment with `labels`
/// labels.
pub fn specialize(format: &str, labels: usize) -> Specialized {
	Specialized(
		FilenameTemplate::new(format)
			.expect("a valid format")
			.specialize(&environment(labels))
			.expect("the environment's values to bake in"),
	)
}

/// An environment with `labels` labels, named the way the benchmarks expect.
pub fn environment(labels: usize) -> Environment<'static, Empty> {
	let mut metadata = k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
		name: Some("my-environment".to_owned()),
		..Default::default()
	};

	metadata.labels = Some(
		(0..labels)
			.map(|index| (format!("envKey{index}"), format!("envValue{index}")))
			.collect(),
	);

	let mut spec = EnvironmentSpec::default();
	spec.namespace = Some("mynamespace".into());

	Environment::new()
		.with_metadata(metadata)
		.with_spec(spec)
		.build()
		.expect("a valid environment")
}

/// A format referring to every label of an environment with `labels` of them,
/// plus one it does not have.
pub fn format_referring_to_labels(labels: usize) -> String {
	let mut parts = Vec::with_capacity(labels + 4);
	parts.push("{{ env.spec.namespace }}".to_owned());
	parts.push("{{ env.metadata.name }}".to_owned());
	for index in 0..labels {
		parts.push(format!("{{{{ env.metadata.labels.envKey{index} }}}}"));
	}
	parts.push("{{ env.metadata.labels.missingLabel }}".to_owned());
	parts.push("apps/v1/{{ .kind }}-{{ .metadata.name }}".to_owned());
	parts.join("/")
}

/// What a path segment is built out of.
#[derive(Clone, Copy, Debug)]
pub enum Segment {
	/// Holds characters that have to be replaced.
	NeedingReplacement,
	/// Already acceptable, so sanitizing it should borrow rather than allocate.
	CleanAscii,
}

/// A path segment of `parts` parts, to sanitize.
pub fn segment(parts: usize, kind: Segment) -> String {
	let mut segment = String::with_capacity(parts * 32);

	for index in 0..parts {
		if index > 0 {
			segment.push(match kind {
				Segment::NeedingReplacement => '/',
				Segment::CleanAscii => '-',
			});
		}
		segment.push_str(match kind {
			Segment::NeedingReplacement => "apps/v1:Deployment_",
			Segment::CleanAscii => "apps-v1:Deployment_",
		});
		segment.push_str(&index.to_string());
	}

	segment
}

/// A Deployment-shaped manifest, with metadata big enough to be worth walking.
pub fn manifest(labels: usize, annotations: usize) -> serde_json::Value {
	let labels: FxHashMap<String, String> = (0..labels)
		.map(|index| (format!("label{index}"), format!("label-value-{index}")))
		.collect();
	let annotations: FxHashMap<String, String> = (0..annotations)
		.map(|index| {
			(
				format!("annotation{index}"),
				format!("annotation-value-{index}"),
			)
		})
		.collect();

	let containers: Vec<serde_json::Value> = (0..4)
		.map(|index| {
			json!({
				"name": format!("container-{index}"),
				"image": format!("registry.example.com/app:{index}"),
				"ports": [
					{ "containerPort": 8080 + index, "name": "http" },
					{ "containerPort": 9090 + index, "name": "metrics" },
				],
				"env": [
					{ "name": "ENV_A", "value": "a" },
					{ "name": "ENV_B", "value": "b" },
				],
			})
		})
		.collect();

	json!({
		"apiVersion": "apps/v1",
		"kind": "Deployment",
		"metadata": {
			"name": "my-deployment",
			"namespace": "mynamespace",
			"labels": labels,
			"annotations": annotations,
		},
		"spec": {
			"replicas": 3,
			"selector": { "matchLabels": { "app": "my-app" } },
			"template": {
				"metadata": { "labels": { "app": "my-app" } },
				"spec": { "containers": containers },
			},
		},
	})
}

/// A template specialized the way the export loop feeds one filenames.
pub fn specialized(labels: usize) -> Specialized {
	specialize(
		"{{ env.spec.namespace }}/{{ .kind }}-{{ .metadata.name }}",
		labels,
	)
}
