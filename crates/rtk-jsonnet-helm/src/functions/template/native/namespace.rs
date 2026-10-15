use std::borrow::Cow;
use std::path::PathBuf;

use super::super::Options;

impl Options {
	pub(crate) fn native_resolved(&self) -> Cow<'_, Self> {
		if self
			.namespace
			.as_deref()
			.is_some_and(|namespace| !namespace.is_empty())
		{
			return Cow::Borrowed(self);
		}
		let namespace = std::env::var("HELM_NAMESPACE")
			.ok()
			.filter(|namespace| !namespace.is_empty())
			.or_else(|| {
				let paths = std::env::var_os("KUBECONFIG")
					.filter(|paths| !paths.is_empty())
					.map_or_else(
						|| {
							std::env::var_os("HOME")
								.map(|home| vec![PathBuf::from(home).join(".kube/config")])
								.unwrap_or_default()
						},
						|paths| {
							std::env::split_paths(&paths)
								.filter(|path| !path.as_os_str().is_empty())
								.collect()
						},
					);
				Namespace::from_files(paths)
			})
			.unwrap_or_else(|| "default".into());
		let mut options = self.clone();
		options.namespace = Some(namespace);
		Cow::Owned(options)
	}
}

struct Namespace;
impl Namespace {
	fn from_files(paths: Vec<PathBuf>) -> Option<String> {
		let mut current = None;
		let mut contexts = std::collections::BTreeMap::new();
		for path in paths {
			let Ok(contents) = std::fs::read_to_string(path) else {
				continue;
			};
			let Ok(config) = serde_saphyr::from_str::<serde_json::Value>(&contents) else {
				continue;
			};
			if current.is_none() {
				current = config["current-context"]
					.as_str()
					.filter(|name| !name.is_empty())
					.map(str::to_owned);
			}
			for context in config["contexts"].as_array().into_iter().flatten() {
				if let Some(name) = context["name"].as_str() {
					contexts.entry(name.to_owned()).or_insert_with(|| {
						context["context"]["namespace"]
							.as_str()
							.unwrap_or_default()
							.to_owned()
					});
				}
			}
		}
		contexts
			.get(&current?)
			.filter(|namespace| !namespace.is_empty())
			.cloned()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn kubeconfig_merging_uses_the_first_context_and_first_current_name() {
		let directory = tempfile::tempdir().unwrap();
		let first = directory.path().join("first");
		let second = directory.path().join("second");
		std::fs::write(
			&first,
			"current-context: selected\ncontexts:\n- name: selected\n  context:\n    namespace: first\n",
		)
		.unwrap();
		std::fs::write(
			&second,
			"current-context: other\ncontexts:\n- name: selected\n  context:\n    namespace: second\n",
		)
		.unwrap();
		assert_eq!(
			Namespace::from_files(vec![first, second]),
			Some("first".into())
		);
		assert_eq!(Namespace::from_files(Vec::new()), None);
	}
}
