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

#[cfg(feature = "benchmarking")]
pub mod internal_bench {
	use anyhow::Result;

	use crate::spec::Environment;

	pub fn specialize_template_for_env(
		template: &str,
		env_spec: &Option<Environment>,
	) -> Result<String> {
		crate::environments::export::specialize_template_for_env(template, env_spec)
	}
}
