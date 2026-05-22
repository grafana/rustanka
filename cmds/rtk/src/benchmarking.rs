use std::collections::BTreeMap;

use serde_json::json;

use crate::spec::Environment;

pub fn build_specialize_template_input(label_count: usize) -> (String, Option<Environment>) {
	let mut env = Environment::new();
	env.metadata.name = Some("my-environment".to_string());
	env.spec.namespace = "mynamespace".to_string();

	let mut labels = BTreeMap::new();
	for idx in 0..label_count {
		labels.insert(format!("key{idx}"), format!("value{idx}"));
	}
	env.metadata.labels = Some(labels);

	let mut parts = Vec::with_capacity(label_count + 4);
	parts.push("{{ env.spec.namespace }}".to_string());
	parts.push("{{ env.metadata.name }}".to_string());
	for idx in 0..label_count {
		parts.push(format!("{{{{ env.metadata.labels.key{idx} }}}}"));
	}
	parts.push("{{ env.metadata.labels.missingLabel }}".to_string());
	parts.push("apps/v1/{{ .kind }}-{{ .metadata.name }}".to_string());

	(parts.join("/"), Some(env))
}

pub fn build_sanitize_path_component_input(component_count: usize) -> String {
	build_sanitize_path_component_input_kind(component_count, SanitizeInputKind::WithReplacements)
}

#[derive(Clone, Copy, Debug)]
pub enum SanitizeInputKind {
	/// Input contains `/` characters that get replaced with `-`.
	WithReplacements,
	/// Input is already clean ASCII; the Cow fast path should borrow.
	CleanAscii,
}

pub fn build_sanitize_path_component_input_kind(
	component_count: usize,
	kind: SanitizeInputKind,
) -> String {
	let mut component = String::with_capacity(component_count * 32);
	for idx in 0..component_count {
		if idx > 0 {
			match kind {
				SanitizeInputKind::WithReplacements => component.push('/'),
				SanitizeInputKind::CleanAscii => component.push('-'),
			}
		}
		match kind {
			SanitizeInputKind::WithReplacements => component.push_str("apps/v1:Deployment_"),
			SanitizeInputKind::CleanAscii => component.push_str("apps-v1:Deployment_"),
		}
		component.push_str(&idx.to_string());
	}
	component
}

/// Build inputs for benchmarking `render_filename_simple`. The template is
/// pre-specialized via `specialize_template_for_env` and parsed by
/// `gtmpl::Template`, matching what the parallel export loop feeds the function.
/// The manifest is a Deployment-shaped object with the requested number of
/// labels and annotations so we exercise the metadata sub-tree conversion.
pub fn build_render_filename_input(
	label_count: usize,
	annotation_count: usize,
) -> (gtmpl::Template, serde_json::Value, Option<Environment>) {
	let mut env = Environment::new();
	env.metadata.name = Some("my-environment".to_string());
	env.spec.namespace = "mynamespace".to_string();

	let mut env_labels = BTreeMap::new();
	for idx in 0..label_count {
		env_labels.insert(format!("envKey{idx}"), format!("envValue{idx}"));
	}
	env.metadata.labels = Some(env_labels);
	let env_spec = Some(env);

	let mut labels = serde_json::Map::new();
	for idx in 0..label_count {
		labels.insert(
			format!("label{idx}"),
			serde_json::Value::String(format!("label-value-{idx}")),
		);
	}

	let mut annotations = serde_json::Map::new();
	for idx in 0..annotation_count {
		annotations.insert(
			format!("annotation{idx}"),
			serde_json::Value::String(format!("annotation-value-{idx}")),
		);
	}

	let mut containers = Vec::with_capacity(4);
	for idx in 0..4 {
		containers.push(json!({
			"name": format!("container-{idx}"),
			"image": format!("registry.example.com/app:{idx}"),
			"ports": [
				{ "containerPort": 8080 + idx, "name": "http" },
				{ "containerPort": 9090 + idx, "name": "metrics" },
			],
			"env": [
				{ "name": "ENV_A", "value": "a" },
				{ "name": "ENV_B", "value": "b" },
			],
		}));
	}

	let manifest = json!({
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
	});

	let format = "{{ env.spec.namespace }}/{{ .kind }}-{{ .metadata.name }}";
	let specialized = crate::environments::export::specialize_template_for_env(format, &env_spec)
		.expect("template specialization should succeed");
	let mut template = gtmpl::Template::default();
	template
		.parse(&specialized)
		.expect("specialized template should parse");

	(template, manifest, env_spec)
}

#[cfg(feature = "benchmarking")]
pub mod internal_bench {
	use std::borrow::Cow;

	use anyhow::Result;

	use crate::spec::Environment;

	pub fn sanitize_path_component(component: &str) -> Cow<'_, str> {
		crate::environments::export::sanitize_path_component(component)
	}

	pub fn specialize_template_for_env(
		template: &str,
		env_spec: &Option<Environment>,
	) -> Result<String> {
		crate::environments::export::specialize_template_for_env(template, env_spec)
	}

	pub fn render_filename_simple(
		template: &gtmpl::Template,
		manifest: &serde_json::Value,
		env_spec: &Option<Environment>,
	) -> Result<String> {
		crate::environments::export::render_filename_simple(template, manifest, env_spec)
	}
}
