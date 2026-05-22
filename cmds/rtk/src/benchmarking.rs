use std::collections::BTreeMap;

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
}
