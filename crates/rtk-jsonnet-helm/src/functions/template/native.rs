//! Experimental local-chart rendering. Unsupported chart features fail instead of invoking Helm.

use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Context as _, Result, bail, ensure};
use gtmpl_ng::{Context, Template, Value};
use serde_json::{Value as Json, json};
use walkdir::WalkDir;

use super::{Options, name_documents};

mod functions;

pub(super) struct Chart {
	metadata: Json,
	values: Json,
	templates: Rc<Sources>,
	crds: Vec<String>,
}

impl Chart {
	pub(super) fn load(path: &Path) -> Result<Self> {
		ensure!(path.is_dir(), "only unpacked local charts are supported");
		for unsupported in ["values.schema.json", ".helmignore"] {
			ensure!(
				!path.join(unsupported).exists(),
				"{unsupported} is not supported"
			);
		}
		if path.join("charts").exists() {
			ensure!(
				fs::read_dir(path.join("charts"))?.next().is_none(),
				"subcharts are not supported"
			);
		}
		let metadata = Self::read_yaml(&path.join("Chart.yaml"))?;
		ensure!(
			metadata["apiVersion"] == "v2",
			"only apiVersion v2 charts are supported"
		);
		ensure!(
			metadata["name"]
				.as_str()
				.is_some_and(|name| !name.is_empty()),
			"Chart.yaml needs a name"
		);
		ensure!(
			metadata["version"].is_string(),
			"Chart.yaml needs a version"
		);
		ensure!(
			metadata["type"].is_null() || metadata["type"] == "application",
			"only application charts are supported"
		);
		ensure!(
			metadata["dependencies"].is_null() || metadata["dependencies"] == json!([]),
			"chart dependencies are not supported"
		);
		let values = if path.join("values.yaml").exists() {
			Self::read_yaml(&path.join("values.yaml"))?
		} else {
			json!({})
		};
		ensure!(values.is_object(), "values.yaml must contain a mapping");
		let templates = Rc::new(Sources(Self::read_directory(path, "templates")?));
		let crds = Self::read_directory(path, "crds")?
			.into_iter()
			.filter(|(name, _)| {
				matches!(
					Path::new(name)
						.extension()
						.and_then(|extension| extension.to_str()),
					Some("yaml" | "yml" | "json")
				)
			})
			.map(|(_, content)| content)
			.collect();
		Ok(Self {
			metadata,
			values,
			templates,
			crds,
		})
	}

	fn read_yaml(path: &Path) -> Result<Json> {
		let source =
			fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
		serde_saphyr::from_str_with_options(
			&source,
			serde_saphyr::options! { legacy_octal_numbers: true },
		)
		.with_context(|| format!("parsing {}", path.display()))
	}

	fn read_directory(root: &Path, directory: &str) -> Result<Vec<(String, String)>> {
		let path = root.join(directory);
		if !path.exists() {
			return Ok(Vec::new());
		}
		let mut files = Vec::new();
		for entry in WalkDir::new(&path).sort_by_file_name() {
			let entry = entry?;
			ensure!(
				!entry.file_type().is_symlink(),
				"chart symlinks are not supported: {}",
				entry.path().display()
			);
			if entry.file_type().is_file() {
				let name = entry
					.path()
					.strip_prefix(root)?
					.to_str()
					.context("non-UTF-8 template path")?
					.replace('\\', "/");
				files.push((name, fs::read_to_string(entry.path())?));
			}
		}
		Ok(files)
	}

	pub(super) fn render(mut self, release: &str, options: &Options) -> Result<Json> {
		ensure!(
			options.api_versions.is_empty(),
			"apiVersions and .Capabilities are not supported"
		);
		let namespace = options
			.namespace
			.as_deref()
			.filter(|namespace| !namespace.is_empty())
			.context(
				"an explicit namespace is required (kubeconfig resolution is not supported)",
			)?;
		ensure!(
			!release.is_empty()
				&& release.len() <= 53
				&& release.split('.').all(|part| {
					!part.is_empty()
						&& part.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
						&& part.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
						&& part
							.chars()
							.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
				}),
			"invalid release name {release:?}"
		);
		if let Some(overrides) = &options.values {
			ensure!(overrides.is_object(), "values must contain a mapping");
			self.merge_values(overrides);
		}
		let chart = self
			.metadata
			.as_object()
			.context("invalid Chart.yaml")?
			.iter()
			.map(|(key, value)| {
				let name = match key.as_str() {
					"apiVersion" => "APIVersion".to_owned(),
					"appVersion" => "AppVersion".to_owned(),
					_ => {
						let mut chars = key.chars();
						chars.next().map_or_else(String::new, |c| {
							c.to_uppercase().collect::<String>() + chars.as_str()
						})
					}
				};
				(name, value.clone())
			})
			.collect::<serde_json::Map<_, _>>();
		let mut context = json!({
			"Release": { "Name": release, "Namespace": namespace, "Service": "Helm", "IsInstall": true, "IsUpgrade": false, "Revision": 1 },
			"Chart": chart,
			"Values": self.values,
		});
		let _scope = Scope::enter(Rc::clone(&self.templates))?;
		let mut documents = Vec::new();
		if options.include_crds {
			for crd in &self.crds {
				Self::documents(crd, false, &mut documents)?;
			}
		}
		for (name, _) in &self.templates.0 {
			let filename = name.rsplit('/').next().unwrap_or(name);
			if filename.starts_with('_') || filename == "NOTES.txt" {
				continue;
			}
			context["Template"] = json!({"Name": format!("{}/{}", self.metadata["name"].as_str().unwrap(), name), "BasePath": format!("{}/templates", self.metadata["name"].as_str().unwrap())});
			let Value::Map(root) = from_json(&context) else {
				unreachable!()
			};
			let rendered = self
				.templates
				.render(name, Value::Object(root), None)
				.with_context(|| format!("rendering {name}"))?;
			Self::documents(&rendered, options.no_hooks, &mut documents)?;
		}
		Ok(name_documents(documents, options.name_format.as_deref()))
	}

	fn merge_values(&mut self, overrides: &Json) {
		merge(&mut self.values, overrides);
	}

	fn documents(source: &str, no_hooks: bool, output: &mut Vec<Json>) -> Result<()> {
		let documents: Vec<Json> = serde_saphyr::from_multiple_with_options(
			source,
			serde_saphyr::options! { legacy_octal_numbers: true },
		)?;
		for document in documents {
			if document.is_null() {
				continue;
			}
			ensure!(document.is_object(), "rendered manifest must be a mapping");
			if no_hooks
				&& document["metadata"]["annotations"]
					.get("helm.sh/hook")
					.is_some()
			{
				continue;
			}
			output.push(document);
		}
		Ok(())
	}
}

fn merge(base: &mut Json, overrides: &Json) {
	if let (Some(base), Some(overrides)) = (base.as_object_mut(), overrides.as_object()) {
		for (key, value) in overrides {
			if value.is_null() && base.contains_key(key) {
				base.remove(key);
			} else {
				merge(base.entry(key).or_insert(Json::Null), value);
			}
		}
	} else {
		*base = overrides.clone();
	}
}

struct Sources(Vec<(String, String)>);

impl Sources {
	fn render(&self, name: &str, context: Value, dynamic: Option<&str>) -> Result<String> {
		let mut template = Template::with_name(name);
		functions::install(&mut template);
		for (name, source) in &self.0 {
			template.add_template(name, source)?;
		}
		if let Some(source) = dynamic {
			template.parse(source)?;
		}
		Ok(template.render(&Context::from(context))?)
	}
}

thread_local! {
	// gtmpl callbacks are function pointers, so nested include/tpl calls need a scoped chart context.
	static SCOPES: RefCell<Vec<Rc<Sources>>> = const { RefCell::new(Vec::new()) };
}

struct Scope;
impl Scope {
	fn enter(sources: Rc<Sources>) -> Result<Self> {
		SCOPES.with_borrow_mut(|scopes| {
			ensure!(scopes.len() < 64, "include/tpl recursion exceeds 64 calls");
			scopes.push(sources);
			Ok(Self)
		})
	}

	fn render(name: &str, context: Value, dynamic: Option<&str>) -> Result<String> {
		let sources = SCOPES
			.with_borrow(|scopes| scopes.last().cloned())
			.context("no active chart")?;
		let _scope = Self::enter(Rc::clone(&sources))?;
		sources.render(name, context, dynamic)
	}
}
impl Drop for Scope {
	fn drop(&mut self) {
		SCOPES.with_borrow_mut(|scopes| {
			scopes.pop();
		});
	}
}

fn from_json(value: &Json) -> Value {
	match value {
		Json::Null => Value::Nil,
		Json::Bool(value) => Value::from(*value),
		Json::String(value) => Value::from(value.clone()),
		Json::Number(value) => value
			.as_i64()
			.map(Value::from)
			.or_else(|| value.as_u64().map(Value::from))
			.unwrap_or_else(|| Value::from(value.as_f64().unwrap())),
		Json::Array(values) => Value::Array(values.iter().map(from_json).collect()),
		Json::Object(values) => Value::Map(
			values
				.iter()
				.map(|(key, value)| (key.clone(), from_json(value)))
				.collect(),
		),
	}
}

fn to_json(value: &Value) -> Result<Json> {
	Ok(match value {
		Value::Nil | Value::NoValue => Json::Null,
		Value::Bool(value) => json!(value),
		Value::String(value) => json!(value),
		Value::Number(value) => serde_json::from_str(&value.to_string())?,
		Value::Array(values) => Json::Array(values.iter().map(to_json).collect::<Result<_>>()?),
		Value::Map(values) | Value::Object(values) => {
			let sorted: std::collections::BTreeMap<_, _> = values.iter().collect();
			Json::Object(
				sorted
					.into_iter()
					.map(|(key, value)| Ok((key.clone(), to_json(value)?)))
					.collect::<Result<_>>()?,
			)
		}
		Value::Function(_) => bail!("cannot serialize a template function"),
	})
}

#[cfg(test)]
mod tests;
