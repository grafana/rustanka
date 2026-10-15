//! Experimental local-chart rendering. Unsupported chart features fail instead of invoking Helm.

use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Context as _, Result, bail, ensure};
use gtmpl_ng::{Context, Template, Value};
use serde_json::{Value as Json, json};

use super::{Options, name_documents};

mod clock;
mod crypto;
mod functions;
mod ignore;
mod methods;
mod namespace;
mod regex;
mod urls;
mod versions;

#[derive(Clone)]
pub(super) struct Chart {
	metadata: Json,
	values: Json,
	templates: Vec<(String, String)>,
	files: std::collections::BTreeMap<String, Vec<u8>>,
	schema: Option<Json>,
	crds: Vec<String>,
	dependencies: Vec<Self>,
}

impl Chart {
	pub(super) fn load(path: &Path) -> Result<Self> {
		let files = if path.is_dir() {
			let rules = ignore::Rules::load(path)?;
			let mut files = std::collections::BTreeMap::new();
			for entry in rules.walk(path, "") {
				let entry = entry?;
				ensure!(
					!entry.file_type().is_symlink(),
					"chart symlinks are not supported: {}",
					entry.path().display()
				);
				if entry.file_type().is_file() {
					let name = entry
						.path()
						.strip_prefix(path)?
						.to_str()
						.context("non-UTF-8 chart path")?
						.replace(std::path::MAIN_SEPARATOR, "/");
					files.insert(name, fs::read(entry.path())?);
				}
			}
			files
		} else {
			Self::archive(&fs::read(path).with_context(|| format!("reading {}", path.display()))?)?
		};
		Self::from_files(files)
	}

	fn archive(bytes: &[u8]) -> Result<std::collections::BTreeMap<String, Vec<u8>>> {
		use std::io::Read as _;
		let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
		let mut files = std::collections::BTreeMap::new();
		let mut root = None;
		let mut total = 0_u64;
		for entry in archive.entries()? {
			let mut entry = entry?;
			let path = entry.path()?.into_owned();
			ensure!(
				path.components()
					.all(|part| matches!(part, std::path::Component::Normal(_))),
				"invalid chart archive path: {}",
				path.display()
			);
			if entry.header().entry_type().is_dir() {
				continue;
			}
			ensure!(
				entry.header().entry_type().is_file(),
				"chart archive contains a non-regular file: {}",
				path.display()
			);
			let path = path.to_str().context("non-UTF-8 chart archive path")?;
			let (prefix, name) = path
				.split_once('/')
				.context("chart archive has no root directory")?;
			if let Some(root) = &root {
				ensure!(root == prefix, "chart archive contains multiple roots");
			} else {
				root = Some(prefix.to_owned());
			}
			let size = entry.size();
			ensure!(
				size <= 5 * 1024 * 1024,
				"chart archive file exceeds 5 MiB: {name}"
			);
			total = total
				.checked_add(size)
				.context("chart archive size overflow")?;
			ensure!(total <= 100 * 1024 * 1024, "chart archive exceeds 100 MiB");
			let mut content = Vec::new();
			entry.read_to_end(&mut content)?;
			ensure!(
				files.insert(name.to_owned(), content).is_none(),
				"duplicate chart archive file: {name}"
			);
		}
		Ok(files)
	}

	fn yaml(bytes: &[u8]) -> Result<Json> {
		let source = std::str::from_utf8(bytes)?.trim_start_matches('\u{feff}');
		serde_saphyr::from_str_with_options(
			source,
			serde_saphyr::options! { legacy_octal_numbers: true, budget: None, duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins },
		)
		.map_err(Into::into)
	}

	fn from_files(mut files: std::collections::BTreeMap<String, Vec<u8>>) -> Result<Self> {
		let mut metadata = Self::yaml(
			&files
				.remove("Chart.yaml")
				.context("Chart.yaml is required")?,
		)?;
		ensure!(metadata.is_object(), "Chart.yaml must contain a mapping");
		if metadata["apiVersion"].is_null() {
			metadata["apiVersion"] = json!("v1");
		}
		ensure!(
			matches!(metadata["apiVersion"].as_str(), Some("v1" | "v2")),
			"Chart.yaml apiVersion must be v1 or v2"
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
			metadata["type"].is_null()
				|| matches!(metadata["type"].as_str(), Some("application" | "library")),
			"invalid chart type"
		);
		if metadata["apiVersion"] == "v1"
			&& let Some(requirements) = files.remove("requirements.yaml")
		{
			metadata["dependencies"] = Self::yaml(&requirements)?["dependencies"].clone();
		}
		let values = files
			.remove("values.yaml")
			.map(|source| Self::yaml(&source))
			.transpose()?
			.filter(|value| !value.is_null())
			.unwrap_or_else(|| json!({}));
		ensure!(values.is_object(), "values.yaml must contain a mapping");
		let schema = files
			.remove("values.schema.json")
			.map(|source| serde_json::from_slice(&source))
			.transpose()?;
		let mut templates = Vec::new();
		let mut crds = Vec::new();
		let mut children =
			std::collections::BTreeMap::<String, std::collections::BTreeMap<String, Vec<u8>>>::new(
			);
		let mut dependencies = Vec::new();
		let mut ordinary_files = std::collections::BTreeMap::new();
		for (name, source) in files {
			if let Some(rest) = name.strip_prefix("charts/") {
				let child_name = rest.split('/').next().unwrap();
				if child_name.starts_with(['.', '_']) {
					continue;
				}
				if let Some((child, path)) = rest.split_once('/') {
					children
						.entry(child.to_owned())
						.or_default()
						.insert(path.to_owned(), source);
				} else if Path::new(rest)
					.extension()
					.is_some_and(|extension| extension == "tgz")
				{
					dependencies.push(Self::from_files(Self::archive(&source)?)?);
				}
			} else if name.starts_with("templates/") {
				templates.push((
					name,
					std::str::from_utf8(&source)?
						.trim_start_matches('\u{feff}')
						.to_owned(),
				));
			} else {
				if name.starts_with("crds/")
					&& matches!(
						Path::new(&name)
							.extension()
							.and_then(|extension| extension.to_str()),
						Some("yaml" | "yml" | "json")
					) {
					crds.push(
						std::str::from_utf8(&source)?
							.trim_start_matches('\u{feff}')
							.to_owned(),
					);
				}
				ordinary_files.insert(name, source);
			}
		}
		for (_, files) in children {
			dependencies.push(Self::from_files(files)?);
		}
		Ok(Self {
			metadata,
			values,
			templates,
			files: ordinary_files,
			schema,
			crds,
			dependencies,
		})
	}

	fn name(&self) -> &str {
		self.metadata["name"].as_str().unwrap()
	}
	fn library(&self) -> bool {
		self.metadata["type"] == "library"
	}

	fn resolve_dependencies(&mut self) -> Result<()> {
		let definitions = self.metadata["dependencies"]
			.as_array()
			.cloned()
			.unwrap_or_default();
		if !definitions.is_empty() {
			let mut dependencies: Vec<_> = self
				.dependencies
				.iter()
				.filter(|child| {
					!definitions
						.iter()
						.any(|definition| definition["name"].as_str() == Some(child.name()))
				})
				.cloned()
				.collect();
			for definition in definitions {
				let name = definition["name"]
					.as_str()
					.context("dependency needs a name")?;
				let mut child = self
					.dependencies
					.iter()
					.find(|child| child.name() == name)
					.cloned()
					.with_context(|| format!("missing chart dependency {name}"))?;
				if let Some(alias) = definition["alias"]
					.as_str()
					.filter(|alias| !alias.is_empty())
				{
					ensure!(
						alias
							.chars()
							.all(|character| character.is_ascii_alphanumeric()
								|| character == '_' || character == '-'),
						"invalid dependency alias {alias}"
					);
					child.metadata["name"] = json!(alias);
				}
				dependencies.push(child);
			}
			self.dependencies = dependencies;
		}
		for child in &mut self.dependencies {
			child.resolve_dependencies()?;
		}
		Ok(())
	}

	fn coalesce(&mut self, overrides: &Json, globals: Option<&Json>) -> Result<()> {
		let mut overrides = overrides.clone();
		ensure!(overrides.is_object(), "chart values must contain a mapping");
		if let Some(globals) = globals.filter(|globals| globals.is_object()) {
			let mut merged = overrides
				.get("global")
				.filter(|value| value.is_object())
				.cloned()
				.unwrap_or_else(|| json!({}));
			merge(&mut merged, globals);
			overrides["global"] = merged;
		}
		let children: std::collections::BTreeSet<_> = self
			.dependencies
			.iter()
			.map(|child| child.name().to_owned())
			.collect();
		Self::merge_values(&mut self.values, &overrides, &children, false);
		let child_overrides: std::collections::BTreeMap<_, _> = self
			.dependencies
			.iter()
			.map(|child| {
				(
					child.name().to_owned(),
					self.values.get(child.name()).cloned(),
				)
			})
			.collect();
		let globals = self
			.values
			.get("global")
			.cloned()
			.unwrap_or_else(|| json!({}));
		for child in &mut self.dependencies {
			let overrides = self
				.values
				.get(child.name())
				.cloned()
				.unwrap_or_else(|| json!({}));
			child.coalesce(&overrides, Some(&globals))?;
			self.values[child.name()] = child.values.clone();
		}
		let definitions = self.metadata["dependencies"]
			.as_array()
			.cloned()
			.unwrap_or_default();
		self.dependencies.retain(|child| {
			let definition = definitions.iter().find(|definition| {
				definition["alias"]
					.as_str()
					.filter(|alias| !alias.is_empty())
					.or_else(|| definition["name"].as_str())
					== Some(child.name())
			});
			let Some(definition) = definition else {
				return true;
			};
			let mut enabled = true;
			if let Some(tags) = definition["tags"].as_array() {
				let states: Vec<_> = tags
					.iter()
					.filter_map(|tag| self.values.get("tags")?.get(tag.as_str()?)?.as_bool())
					.collect();
				if states.contains(&true) {
					enabled = true;
				} else if states.contains(&false) {
					enabled = false;
				}
			}
			if let Some(condition) = definition["condition"].as_str() {
				for path in condition.split(',') {
					if let Some(value) = Self::value_at(&self.values, path).and_then(Json::as_bool)
					{
						enabled = value;
						break;
					}
				}
			}
			enabled
		});

		for (name, values) in child_overrides {
			if self.dependencies.iter().all(|child| child.name() != name) {
				if let Some(values) = values {
					self.values[&name] = values;
				} else {
					self.values.as_object_mut().unwrap().remove(&name);
				}
			}
		}
		if self.metadata.get("dependencies").is_some() {
			let active: Vec<_> = definitions
				.iter()
				.filter_map(|definition| {
					let name = definition["alias"]
						.as_str()
						.filter(|alias| !alias.is_empty())
						.or_else(|| definition["name"].as_str())?;
					self.dependencies
						.iter()
						.any(|child| child.name() == name)
						.then(|| {
							let mut definition = definition.clone();
							definition["name"] = json!(name);
							definition["enabled"] = json!(true);
							definition
						})
				})
				.collect();
			self.metadata["dependencies"] = json!(active);
		}
		for definition in definitions {
			let name = definition["alias"]
				.as_str()
				.filter(|alias| !alias.is_empty())
				.or_else(|| definition["name"].as_str())
				.unwrap_or_default();
			let Some(child) = self.dependencies.iter().find(|child| child.name() == name) else {
				continue;
			};
			let mut imports = json!({});
			for import in definition["import-values"].as_array().into_iter().flatten() {
				let (source, destination) = if let Some(key) = import.as_str() {
					(format!("exports.{key}"), String::new())
				} else {
					(
						import["child"].as_str().unwrap_or_default().to_owned(),
						import["parent"].as_str().unwrap_or_default().to_owned(),
					)
				};
				if let Some(value) =
					Self::value_at(&child.values, &source).filter(|value| value.is_object())
				{
					let mut imported = value.clone();
					for part in destination.split('.').filter(|part| !part.is_empty()).rev() {
						imported = json!({part: imported});
					}
					merge(&mut imports, &imported);
				}
			}
			merge(&mut imports, &self.values);
			self.values = imports;
		}
		Ok(())
	}

	fn merge_values(
		base: &mut Json,
		overrides: &Json,
		children: &std::collections::BTreeSet<String>,
		preserve_null: bool,
	) {
		if let (Some(base), Some(overrides)) = (base.as_object_mut(), overrides.as_object()) {
			for (key, value) in overrides {
				if value.is_null() && base.contains_key(key) && !preserve_null {
					base.remove(key);
				} else {
					Self::merge_values(
						base.entry(key.clone()).or_insert(Json::Null),
						value,
						&std::collections::BTreeSet::new(),
						preserve_null || children.contains(key),
					);
				}
			}
		} else {
			*base = overrides.clone();
		}
	}

	fn value_at<'a>(values: &'a Json, path: &str) -> Option<&'a Json> {
		path.split('.')
			.filter(|part| !part.is_empty())
			.try_fold(values, |value, part| value.get(part))
	}

	fn validate(&self) -> Result<()> {
		if let Some(schema) = &self.schema {
			let draft = match schema["$schema"].as_str() {
				Some(value) if value.contains("draft-04") => jsonschema::Draft::Draft4,
				Some(value) if value.contains("draft-06") => jsonschema::Draft::Draft6,
				_ => jsonschema::Draft::Draft7,
			};
			let validator = jsonschema::options()
				.with_draft(draft)
				.should_validate_formats(true)
				.build(schema)
				.with_context(|| format!("invalid values schema for {}", self.name()))?;
			let errors: Vec<_> = validator
				.iter_errors(&self.values)
				.map(|error| error.to_string())
				.collect();
			ensure!(
				errors.is_empty(),
				"values do not meet the schema for {}: {}",
				self.name(),
				errors.join("; ")
			);
		}
		for child in &self.dependencies {
			child.validate()?;
		}
		Ok(())
	}

	fn sources(&self, path: &str, sources: &mut Sources) {
		sources.files.insert(path.to_owned(), self.files.clone());
		for (name, content) in &self.templates {
			if !self.library()
				|| Path::new(name)
					.file_name()
					.unwrap()
					.to_string_lossy()
					.starts_with('_')
			{
				sources
					.templates
					.push((format!("{path}/{name}"), content.clone()));
			}
		}
		for child in &self.dependencies {
			child.sources(&format!("{path}/charts/{}", child.name()), sources);
		}
	}

	fn context(
		&self,
		path: &str,
		release: &str,
		namespace: &str,
		capabilities: &Json,
		is_root: bool,
	) -> Json {
		let mut metadata = self
			.metadata
			.as_object()
			.unwrap()
			.iter()
			.map(|(key, value)| {
				let name = match key.as_str() {
					"apiVersion" => "APIVersion".to_owned(),
					"appVersion" => "AppVersion".to_owned(),
					_ => {
						let mut chars = key.chars();
						chars.next().map_or_else(String::new, |character| {
							character.to_uppercase().collect::<String>() + chars.as_str()
						})
					}
				};
				(name, value.clone())
			})
			.collect::<serde_json::Map<_, _>>();
		metadata.insert("IsRoot".to_owned(), json!(is_root));
		let subcharts = self
			.dependencies
			.iter()
			.map(|child| {
				(
					child.name().to_owned(),
					child.context(
						&format!("{path}/charts/{}", child.name()),
						release,
						namespace,
						capabilities,
						false,
					),
				)
			})
			.collect::<serde_json::Map<_, _>>();
		json!({"Release": {"Name": release, "Namespace": namespace, "Service": "Helm", "IsInstall": true, "IsUpgrade": false, "Revision": 1}, "Chart": metadata, "Values": self.values, "Capabilities": capabilities, "Subcharts": subcharts, "Template": {"Name": "", "BasePath": format!("{path}/templates")}})
	}

	fn render_into(
		&self,
		path: &str,
		release: &str,
		options: &Options,
		sources: &Rc<Sources>,
		output: &mut Vec<Json>,
		is_root: bool,
	) -> Result<()> {
		for child in self.dependencies.iter().rev() {
			child.render_into(
				&format!("{path}/charts/{}", child.name()),
				release,
				options,
				sources,
				output,
				false,
			)?;
		}
		if options.include_crds {
			for crd in &self.crds {
				Self::documents(crd, false, output)?;
			}
		}
		if self.library() {
			return Ok(());
		}
		let context = self.context(
			path,
			release,
			options.namespace.as_deref().unwrap(),
			&sources.capabilities,
			is_root,
		);
		let context = sources.context(from_json(&context));
		for (name, _) in self.templates.iter().rev() {
			let filename = name.rsplit('/').next().unwrap();
			if filename.starts_with('_') || filename == "NOTES.txt" {
				continue;
			}
			let full_name = format!("{path}/{name}");
			let Value::SharedMap(root) = &context else {
				unreachable!()
			};
			let Some(Value::SharedMap(template)) = root.get("Template") else {
				unreachable!()
			};
			template.insert("Name".into(), full_name.clone().into());
			let _scope = Scope::enter(Rc::clone(sources))?;
			let rendered = sources
				.render(&full_name, context.clone(), None)
				.with_context(|| format!("rendering {full_name}"))?;
			Self::documents(
				&rendered.replace("<no value>", ""),
				options.no_hooks,
				output,
			)?;
		}
		Ok(())
	}

	pub(super) fn render(mut self, release: &str, options: &Options) -> Result<Json> {
		let resolved = options.native_resolved();
		let options = resolved.as_ref();
		ensure!(
			!self.library(),
			"library charts cannot be rendered directly"
		);
		super::validate_release_name(release).map_err(anyhow::Error::msg)?;
		self.resolve_dependencies()?;
		self.coalesce(options.values.as_ref().unwrap_or(&json!({})), None)?;
		self.validate()?;
		let mut sources = Sources::new(&options.api_versions)?;
		self.sources(self.name(), &mut sources);
		// Helm parses deeper charts first, so a parent's helpers take precedence.
		sources.templates.sort_by(|(a, _), (b, _)| {
			b.matches('/')
				.count()
				.cmp(&a.matches('/').count())
				.then_with(|| b.cmp(a))
		});
		sources.parse()?;
		let sources = Rc::new(sources);
		let mut documents = Vec::new();
		self.render_into(
			self.name(),
			release,
			options,
			&sources,
			&mut documents,
			true,
		)?;
		Ok(name_documents(documents, options.name_format.as_deref()))
	}

	fn documents(source: &str, no_hooks: bool, output: &mut Vec<Json>) -> Result<()> {
		static SEPARATORS: std::sync::LazyLock<::regex::Regex> =
			std::sync::LazyLock::new(|| ::regex::Regex::new(r"(?:^|\s*\n)---\s*").unwrap());
		let mut documents = Vec::new();
		// Helm splits rendered templates before passing each manifest to YAML.
		for source in SEPARATORS
			.split(source)
			.filter(|source| !source.trim().is_empty())
		{
			let source = super::normalize_helm_multiline_quotes(source);
			documents.push(serde_saphyr::from_str_with_options::<Json>(
				&source,
				serde_saphyr::options! { legacy_octal_numbers: true, budget: None, duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins },
			)?);
		}

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

struct Sources {
	templates: Vec<(String, String)>,
	files: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<u8>>>,
	capabilities: Json,
	parsed: Option<Template>,
}

impl Sources {
	fn new(extra_versions: &[String]) -> Result<Self> {
		let mut capabilities: Json =
			serde_json::from_str(include_str!("native/capabilities.json"))?;
		let mut api_versions: Vec<_> = capabilities["APIVersions"]
			.as_array()
			.unwrap()
			.iter()
			.map(|value| value.as_str().unwrap().to_owned())
			.collect();
		api_versions.extend_from_slice(extra_versions);
		capabilities["APIVersions"] = json!(api_versions);
		let helm = capabilities["HelmVersion"].clone();
		capabilities["HelmVersion"] = json!({"Version": helm["version"], "GitCommit": helm["git_commit"], "GitTreeState": helm["git_tree_state"], "GoVersion": helm["go_version"]});
		Ok(Self {
			templates: Vec::new(),
			files: std::collections::BTreeMap::new(),
			capabilities,
			parsed: None,
		})
	}

	fn context(&self, mut context: Value) -> Value {
		let Value::Map(ref mut root) = context else {
			return context;
		};
		if let Some(Value::Map(subcharts)) = root.get_mut("Subcharts") {
			for child in subcharts.values_mut() {
				*child = self.context(child.clone());
			}
		}
		let path = match root.get("Template") {
			Some(Value::Map(template)) => template.get("BasePath").and_then(|value| {
				if let Value::String(text) = value {
					text.strip_suffix("/templates")
				} else {
					None
				}
			}),
			_ => None,
		};
		let files = path
			.and_then(|path| self.files.get(path))
			.map(|files| {
				files
					.iter()
					.map(|(name, bytes)| {
						(
							name.clone(),
							match std::str::from_utf8(bytes) {
								Ok(text) => Value::from(text),
								Err(_) => {
									Value::Array(bytes.iter().copied().map(Value::from).collect())
								}
							},
						)
					})
					.collect()
			})
			.unwrap_or_default();
		root.insert("Files".into(), Value::Map(files));
		if let Some(Value::Map(capabilities)) = root.get_mut("Capabilities")
			&& let Some(Value::Map(version)) = capabilities.get_mut("KubeVersion")
		{
			version.insert("GitVersion".into(), version["Version"].clone());
		}
		let Value::Map(root) = context else {
			unreachable!();
		};
		Value::Map(root).into_shared()
	}

	fn parse(&mut self) -> Result<()> {
		let mut template = Template::default();
		// Helper arguments can include the complete byte-backed Files catalog.
		template.limits.function_output = Some(64 * 1024 * 1024);
		functions::install(&mut template);
		for (name, source) in &self.templates {
			template.add_template(name, methods::Actions::rewrite(source))?;
		}
		self.parsed = Some(template);
		Ok(())
	}
	fn render(&self, name: &str, context: Value, dynamic: Option<&str>) -> Result<String> {
		let template = self
			.parsed
			.as_ref()
			.context("chart templates have not been parsed")?;
		let context = Context::from(context);
		if let Some(source) = dynamic {
			let mut overlay = template.clone();
			name.clone_into(&mut overlay.name);
			overlay.parse(methods::Actions::rewrite(source))?;
			Ok(overlay.render(&context)?)
		} else {
			Ok(template.render_named(name, &context)?)
		}
	}
}

thread_local! {
	// include/tpl callbacks need the same parsed chart sources across nested calls.
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
	to_json_at_depth(value, 0)
}

fn to_json_at_depth(value: &Value, depth: usize) -> Result<Json> {
	ensure!(
		depth < 100,
		"cannot serialize a cyclic or deeply nested dictionary"
	);
	Ok(match value {
		Value::Nil | Value::NoValue => Json::Null,
		Value::Bool(value) => json!(value),
		Value::String(value) => json!(value),
		Value::Number(value) => serde_json::from_str(&value.to_string())?,
		Value::Array(values) => Json::Array(
			values
				.iter()
				.map(|value| to_json_at_depth(value, depth + 1))
				.collect::<Result<_>>()?,
		),
		Value::Map(values) | Value::Object(values) => {
			let sorted: std::collections::BTreeMap<_, _> = values.iter().collect();
			Json::Object(
				sorted
					.into_iter()
					.map(|(key, value)| Ok((key.clone(), to_json_at_depth(value, depth + 1)?)))
					.collect::<Result<_>>()?,
			)
		}
		Value::SharedMap(values) => to_json_at_depth(&Value::Map(values.snapshot()), depth + 1)?,
		Value::Function(_) => bail!("cannot serialize a template function"),
	})
}

#[cfg(test)]
mod tests;
