use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use rtk_jsonnet::jpath::JPath;
use rtk_spec::canonical::{Environment, EnvironmentSpec};

pub struct EnvSpecOptions {
	pub namespace: Option<String>,
	pub server: Option<String>,
	pub server_from_context: Option<String>,
	pub context_name: Vec<String>,
	pub diff_strategy: Option<String>,
	pub inject_labels: Option<bool>,
}

fn server_from_kubeconfig_context(context_name: &str) -> Result<String> {
	let kubeconfig =
		kube::config::Kubeconfig::read().context("reading KUBECONFIG for --server-from-context")?;
	let context = kubeconfig
		.contexts
		.iter()
		.find(|context| context.name == context_name)
		.with_context(|| format!("context {context_name:?} not found in KUBECONFIG"))?;
	let cluster_name = context
		.context
		.as_ref()
		.map(|context| context.cluster.as_str())
		.with_context(|| format!("context {context_name:?} has no cluster"))?;
	let cluster = kubeconfig
		.clusters
		.iter()
		.find(|cluster| cluster.name == cluster_name)
		.with_context(|| format!("cluster {cluster_name:?} not found in KUBECONFIG"))?;
	cluster
		.cluster
		.as_ref()
		.and_then(|cluster| cluster.server.clone())
		.with_context(|| format!("cluster {cluster_name:?} has no server"))
}

fn apply_spec_options(spec: &mut EnvironmentSpec, options: &EnvSpecOptions) -> Result<()> {
	if let Some(namespace) = options.namespace.as_deref() {
		spec.namespace = Some(namespace.into());
	}
	if let Some(server) = options.server.as_deref() {
		spec.api_server = Some(server.into());
	}
	if let Some(context) = options.server_from_context.as_deref() {
		spec.api_server = Some(server_from_kubeconfig_context(context)?.into());
	}
	if !options.context_name.is_empty() {
		spec.context_names = options
			.context_name
			.iter()
			.map(|context| context.as_str().into())
			.collect();
	}
	if let Some(strategy) = options.diff_strategy.as_deref() {
		spec.diff_strategy = Some(strategy.into());
	}
	if let Some(inject_labels) = options.inject_labels {
		spec.inject_labels = inject_labels;
	}
	Ok(())
}

pub fn add(path: &Path, inline: bool, options: &EnvSpecOptions) -> Result<()> {
	let path = path.to_path_buf();
	let (root, environment_directory) = if path.is_absolute() {
		let directory = if path.exists() {
			path.canonicalize().unwrap_or(path)
		} else {
			path
		};
		let root = JPath::project_root(directory.parent().unwrap_or(&directory))?;
		(root, directory)
	} else {
		let root = JPath::project_root(std::env::current_dir().context("current_dir")?)?;
		let directory = root.join(path);
		let directory = if directory.exists() {
			directory.canonicalize().unwrap_or(directory)
		} else {
			directory
		};
		(root, directory)
	};

	if environment_directory.exists()
		&& (environment_directory.join("main.jsonnet").exists()
			|| environment_directory.join("spec.json").exists())
	{
		anyhow::bail!(
			"environment already exists at {}",
			environment_directory.display()
		);
	}
	fs::create_dir_all(&environment_directory).context("create environment directory")?;
	let relative = environment_directory
		.strip_prefix(&root)
		.map(|path| path.to_string_lossy().into_owned())
		.unwrap_or_else(|_| environment_directory.display().to_string());

	// tk's `v1alpha1.New()` defaults the namespace before any flag is applied,
	// so an environment created without `--namespace` still names one.
	let mut spec = EnvironmentSpec::default();
	spec.namespace = Some("default".into());
	apply_spec_options(&mut spec, options)?;

	if inline {
		let contents = inline_environment(&relative, &spec)?;
		fs::write(environment_directory.join("main.jsonnet"), contents)
			.context("write main.jsonnet")?;
		return Ok(());
	}

	// tk writes this through the Jsonnet formatter, which terminates the file.
	fs::write(environment_directory.join("main.jsonnet"), "{}\n").context("write main.jsonnet")?;
	// Only the name: tk's `addEnv` sets nothing else, and the namespace rtk used
	// to write here is the entrypoint that discovery derives, not a stored field.
	let environment = Environment::new()
		.with_metadata(ObjectMeta {
			name: Some(relative.clone()),
			..ObjectMeta::default()
		})
		.with_spec(spec)
		.build()
		.context("build spec.json")?;
	let mut contents = serde_json::to_string_pretty(&environment).context("serialize spec.json")?;
	// tk's `writeJSON` appends one.
	contents.push('\n');
	fs::write(environment_directory.join("spec.json"), contents).context("write spec.json")?;
	Ok(())
}

/// The Jsonnet an inline environment is created as.
///
/// tk marshals the same `Environment` it would write to `spec.json`, with an
/// empty `data`, and hands the JSON to the Jsonnet formatter (`writeJsonnet`).
/// The envelope is fixed — `addEnv` fills in nothing but the name — so only the
/// spec has to be rendered, and rendering it from its own serialization is what
/// keeps the fields and their order those of `spec.json`.
fn inline_environment(name: &str, spec: &EnvironmentSpec) -> Result<String> {
	let spec = serde_json::to_value(spec).context("serialize spec")?;
	let spec = jsonnet_from_json(&spec, "  ");
	let name = jsonnet_string(name);
	Ok(format!(
		"{{\n  apiVersion: 'tanka.dev/v1alpha1',\n  kind: 'Environment',\n  \
		 metadata: {{\n    name: {name},\n  }},\n  spec: {spec},\n  data: {{}},\n}}\n"
	))
}

/// Render JSON as the Jsonnet tk would have written for it.
///
/// tk formats with go-jsonnet's formatter, which rewrites JSON's syntax without
/// reflowing it: a quoted key loses its quotes where it is an identifier, a
/// string takes single ones, and every member gains a trailing comma. Since
/// `json.MarshalIndent` already put each member on its own line and left an
/// empty object as `{}`, the JSON's line structure is the Jsonnet's, and this
/// reproduces it rather than deciding a layout of its own.
fn jsonnet_from_json(value: &serde_json::Value, indent: &str) -> String {
	let nested = format!("{indent}  ");
	match value {
		serde_json::Value::Object(members) if !members.is_empty() => {
			let mut rendered = String::from("{\n");
			for (key, member) in members {
				let key = jsonnet_field_name(key);
				let member = jsonnet_from_json(member, &nested);
				rendered.push_str(&format!("{nested}{key}: {member},\n"));
			}
			rendered.push_str(indent);
			rendered.push('}');
			rendered
		}
		serde_json::Value::Array(items) if !items.is_empty() => {
			let mut rendered = String::from("[\n");
			for item in items {
				let item = jsonnet_from_json(item, &nested);
				rendered.push_str(&format!("{nested}{item},\n"));
			}
			rendered.push_str(indent);
			rendered.push(']');
			rendered
		}
		serde_json::Value::String(text) => jsonnet_string(text),
		// Numbers, booleans, null, and the empty object and array, all of which
		// JSON already spells the way Jsonnet does.
		scalar => scalar.to_string(),
	}
}

/// A field name, left bare where Jsonnet would accept it as one.
fn jsonnet_field_name(name: &str) -> String {
	let identifier = !name.is_empty()
		&& !name.starts_with(|first: char| first.is_ascii_digit())
		&& name
			.chars()
			.all(|character| character.is_ascii_alphanumeric() || character == '_');
	if identifier {
		name.to_owned()
	} else {
		jsonnet_string(name)
	}
}

/// A quoted Jsonnet string, in the style the formatter would have chosen.
///
/// go-jsonnet prefers single quotes and switches to double ones as soon as the
/// text contains a single quote — whatever else it contains, so a string holding
/// both kinds is double-quoted with its double quotes escaped.
fn jsonnet_string(text: &str) -> String {
	let quote = if text.contains('\'') { '"' } else { '\'' };
	let mut quoted = String::with_capacity(text.len() + 2);
	quoted.push(quote);
	for character in text.chars() {
		match character {
			'\\' => quoted.push_str("\\\\"),
			'\n' => quoted.push_str("\\n"),
			'\r' => quoted.push_str("\\r"),
			'\t' => quoted.push_str("\\t"),
			_ if character == quote => {
				quoted.push('\\');
				quoted.push(character);
			}
			_ => quoted.push(character),
		}
	}
	quoted.push(quote);
	quoted
}

pub fn remove(paths: &[PathBuf]) -> Result<()> {
	for path in paths {
		let resolved = JPath::resolve(path).with_context(|| {
			format!(
				"could not resolve environment at {} (not an environment or not found)",
				path.display()
			)
		})?;
		let directory = resolved.base_directory;
		if directory.join("spec.json").exists() || directory.join("main.jsonnet").exists() {
			fs::remove_dir_all(&directory)
				.with_context(|| format!("remove {}", directory.display()))?;
		} else {
			anyhow::bail!(
				"not an environment directory (no spec.json or main.jsonnet): {}",
				directory.display()
			);
		}
	}
	Ok(())
}

pub fn set(path: &Path, options: &EnvSpecOptions) -> Result<()> {
	let resolved = JPath::resolve(path).context("resolve environment path")?;
	let spec_path = resolved.base_directory.join("spec.json");
	if !spec_path.exists() {
		anyhow::bail!(
			"environment at {} has no spec.json (inline environment); use env add to create a static environment",
			resolved.base_directory.display()
		);
	}
	let contents = fs::read_to_string(&spec_path).context("read spec.json")?;
	let mut environment: Environment<'_> =
		serde_json::from_str(&contents).context("parse spec.json")?;
	apply_spec_options(&mut environment.spec, options)?;
	let contents = serde_json::to_string_pretty(&environment).context("serialize spec.json")?;
	fs::write(spec_path, contents).context("write spec.json")?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use serde_json::Value;
	use tempfile::TempDir;

	use super::*;

	/// Mark a directory as a Jsonnet project, as `tk init` would.
	fn create_project_root(dir: &Path) {
		fs::write(
			dir.join("jsonnetfile.json"),
			r#"{"version": 1, "dependencies": [], "legacyImports": true}"#,
		)
		.unwrap();
	}

	fn options() -> EnvSpecOptions {
		EnvSpecOptions {
			namespace: None,
			server: None,
			server_from_context: None,
			context_name: Vec::new(),
			diff_strategy: None,
			inject_labels: None,
		}
	}

	fn spec_of(environment: &Path) -> Value {
		let contents = fs::read_to_string(environment.join("spec.json")).unwrap();
		serde_json::from_str(&contents).unwrap()
	}

	#[test]
	fn test_env_add_creates_static_environment() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		create_project_root(root);
		let environment = root.join("environments/dev");

		add(
			&environment,
			false,
			&EnvSpecOptions {
				namespace: Some("my-namespace".to_owned()),
				server: Some("https://kube.example.com".to_owned()),
				inject_labels: Some(true),
				..options()
			},
		)
		.unwrap();

		assert!(environment.is_dir(), "environments/dev should exist");
		assert!(environment.join("main.jsonnet").exists());
		assert!(environment.join("spec.json").exists());
		assert_eq!(
			fs::read_to_string(environment.join("main.jsonnet"))
				.unwrap()
				.trim(),
			"{}"
		);

		let spec = spec_of(&environment);
		assert_eq!(spec["spec"]["namespace"], "my-namespace");
		assert_eq!(spec["spec"]["apiServer"], "https://kube.example.com");
		assert_eq!(spec["spec"]["injectLabels"], true);
	}

	#[test]
	fn test_env_add_inline_creates_main_only() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		create_project_root(root);
		let environment = root.join("env-inline");

		add(
			&environment,
			true,
			&EnvSpecOptions {
				namespace: Some("inline-ns".to_owned()),
				server: Some("https://inline.example.com".to_owned()),
				..options()
			},
		)
		.unwrap();

		assert!(environment.is_dir());
		assert!(environment.join("main.jsonnet").exists());
		assert!(
			!environment.join("spec.json").exists(),
			"inline env should not have spec.json"
		);

		let main = fs::read_to_string(environment.join("main.jsonnet")).unwrap();
		assert!(main.contains("tanka.dev/v1alpha1"));
		assert!(main.contains("inline-ns"));
		assert!(main.contains("https://inline.example.com"));
	}

	#[test]
	fn test_env_add_fails_when_already_exists() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		create_project_root(root);
		let environment = root.join("environments/dev");

		add(
			&environment,
			false,
			&EnvSpecOptions {
				inject_labels: Some(false),
				..options()
			},
		)
		.unwrap();
		let error = add(
			&environment,
			false,
			&EnvSpecOptions {
				inject_labels: Some(false),
				..options()
			},
		)
		.unwrap_err();

		assert!(error.to_string().contains("already exists"));
	}

	#[test]
	fn test_env_set_updates_spec() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		create_project_root(root);
		let environment = root.join("environments/dev");

		add(
			&environment,
			false,
			&EnvSpecOptions {
				namespace: Some("original-ns".to_owned()),
				server: Some("https://original.example.com".to_owned()),
				inject_labels: Some(false),
				..options()
			},
		)
		.unwrap();

		set(
			&environment,
			&EnvSpecOptions {
				namespace: Some("updated-ns".to_owned()),
				context_name: vec!["my-context".to_owned()],
				diff_strategy: Some("server".to_owned()),
				inject_labels: Some(true),
				..options()
			},
		)
		.unwrap();

		let spec = spec_of(&environment);
		assert_eq!(spec["spec"]["namespace"], "updated-ns");
		assert_eq!(
			spec["spec"]["contextNames"],
			serde_json::json!(["my-context"])
		);
		assert_eq!(spec["spec"]["diffStrategy"], "server");
		assert_eq!(spec["spec"]["injectLabels"], true);
	}

	#[test]
	fn test_env_remove_deletes_environment() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		create_project_root(root);
		let environment = root.join("environments/to-remove");

		add(
			&environment,
			false,
			&EnvSpecOptions {
				namespace: Some("default".to_owned()),
				inject_labels: Some(false),
				..options()
			},
		)
		.unwrap();
		assert!(environment.exists());

		remove(std::slice::from_ref(&environment)).unwrap();

		assert!(!environment.exists());
	}

	#[test]
	fn test_env_remove_multiple() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		create_project_root(root);
		let first = root.join("environments/a");
		let second = root.join("environments/b");

		for environment in [&first, &second] {
			add(
				environment,
				false,
				&EnvSpecOptions {
					namespace: Some("default".to_owned()),
					inject_labels: Some(false),
					..options()
				},
			)
			.unwrap();
		}

		remove(&[first.clone(), second.clone()]).unwrap();

		assert!(!first.exists());
		assert!(!second.exists());
	}

	/// Byte for byte what `tk env add environments/e --namespace new-ns` writes.
	///
	/// Verified against tk 0.38, and kept whole rather than as field assertions
	/// because every part of it was wrong at some point: the namespace rtk
	/// invented in `metadata`, the two objects Go always marshals, and the
	/// trailing newline `writeJSON` appends.
	#[test]
	fn a_static_environment_is_written_the_way_tk_writes_it() {
		let temp = TempDir::new().unwrap();
		create_project_root(temp.path());
		let environment = temp.path().join("environments/e");

		let mut options = options();
		options.namespace = Some("new-ns".to_owned());
		add(&environment, false, &options).unwrap();

		assert_eq!(
			fs::read_to_string(environment.join("spec.json")).unwrap(),
			r#"{
  "apiVersion": "tanka.dev/v1alpha1",
  "kind": "Environment",
  "metadata": {
    "name": "environments/e"
  },
  "spec": {
    "namespace": "new-ns",
    "resourceDefaults": {},
    "expectVersions": {}
  }
}
"#
		);
		// tk writes this one through the Jsonnet formatter, which terminates it.
		assert_eq!(
			fs::read_to_string(environment.join("main.jsonnet")).unwrap(),
			"{}\n"
		);
	}

	/// And what `tk env add environments/e --inline ...` writes.
	///
	/// tk formats the marshalled environment as Jsonnet, so the layout is the
	/// JSON's: one member per line, trailing commas, empty objects left inline.
	/// The name keeps its separator — rtk used to replace it with a dash — and no
	/// `apiServer` is invented for an environment that was not given one.
	#[test]
	fn an_inline_environment_is_written_the_way_tk_writes_it() {
		let temp = TempDir::new().unwrap();
		create_project_root(temp.path());
		let environment = temp.path().join("environments/e");

		let mut options = options();
		options.namespace = Some("ns".to_owned());
		options.diff_strategy = Some("native".to_owned());
		options.inject_labels = Some(true);
		add(&environment, true, &options).unwrap();

		assert_eq!(
			fs::read_to_string(environment.join("main.jsonnet")).unwrap(),
			r"{
  apiVersion: 'tanka.dev/v1alpha1',
  kind: 'Environment',
  metadata: {
    name: 'environments/e',
  },
  spec: {
    namespace: 'ns',
    diffStrategy: 'native',
    injectLabels: true,
    resourceDefaults: {},
    expectVersions: {},
  },
  data: {},
}
"
		);
		assert!(
			!environment.join("spec.json").exists(),
			"an inline environment declares itself in its Jsonnet"
		);
	}

	/// An environment created without `--namespace` still names one, because
	/// tk's `v1alpha1.New()` defaults it before any flag is applied.
	#[test]
	fn a_namespace_is_defaulted_the_way_tk_defaults_it() {
		let temp = TempDir::new().unwrap();
		create_project_root(temp.path());
		let environment = temp.path().join("environments/e");

		add(&environment, false, &options()).unwrap();

		assert_eq!(spec_of(&environment)["spec"]["namespace"], "default");
	}

	/// go-jsonnet's formatter prefers single quotes and switches to double ones
	/// as soon as the text holds a single quote, whatever else it holds.
	#[test]
	fn strings_are_quoted_the_way_the_formatter_quotes_them() {
		assert_eq!(jsonnet_string("plain"), "'plain'");
		assert_eq!(jsonnet_string("has'single"), "\"has'single\"");
		assert_eq!(jsonnet_string("has\"double"), "'has\"double'");
		assert_eq!(jsonnet_string("both'and\"quotes"), "\"both'and\\\"quotes\"");
		assert_eq!(jsonnet_string(r"back\slash"), r"'back\\slash'");
		assert_eq!(
			jsonnet_string("line1\nline2\ttabbed"),
			r"'line1\nline2\ttabbed'"
		);
	}

	/// A key that is not an identifier keeps its quotes.
	#[test]
	fn field_names_are_left_bare_only_where_jsonnet_accepts_them() {
		assert_eq!(jsonnet_field_name("apiServer"), "apiServer");
		assert_eq!(jsonnet_field_name("with_underscore"), "with_underscore");
		assert_eq!(
			jsonnet_field_name("tanka.dev/environment"),
			"'tanka.dev/environment'"
		);
		assert_eq!(jsonnet_field_name("1leading"), "'1leading'");
		assert_eq!(jsonnet_field_name(""), "''");
	}

	/// An environment that vendors for itself is its own project, so a new
	/// environment beside it belongs to *it* rather than to whatever encloses it.
	#[test]
	fn adds_into_the_nearest_project_root() {
		let temp = TempDir::new().unwrap();
		let outer = temp.path();
		let inner = outer.join("inner");
		create_project_root(outer);
		fs::create_dir_all(&inner).unwrap();
		create_project_root(&inner);

		let environment = inner.join("environments/dev");
		add(&environment, false, &options()).unwrap();

		// Named relative to `inner`, which vendors for itself, rather than to the
		// project around it.
		let spec = spec_of(&environment);
		assert_eq!(spec["metadata"]["name"], "environments/dev");
		// tk's `addEnv` writes nothing else. An environment's namespace is the
		// entrypoint discovery derives for it, not a field of its `spec.json`.
		assert_eq!(
			spec["metadata"],
			serde_json::json!({ "name": "environments/dev" })
		);
	}

	/// Removing a nested environment must not reach the project around it.
	#[test]
	fn removes_only_the_environment_it_was_given() {
		let temp = TempDir::new().unwrap();
		let outer = temp.path();
		let inner = outer.join("inner");
		create_project_root(outer);
		fs::write(outer.join("main.jsonnet"), "{}").unwrap();
		fs::create_dir_all(&inner).unwrap();
		create_project_root(&inner);
		fs::write(inner.join("main.jsonnet"), "{}").unwrap();

		remove(std::slice::from_ref(&inner)).unwrap();

		assert!(!inner.exists());
		assert!(
			outer.join("main.jsonnet").exists(),
			"the enclosing project must survive removing an environment inside it"
		);
	}

	/// And neither must editing one.
	#[test]
	fn sets_the_spec_of_the_environment_it_was_given() {
		let temp = TempDir::new().unwrap();
		let outer = temp.path();
		let inner = outer.join("inner");
		create_project_root(outer);
		fs::write(outer.join("main.jsonnet"), "{}").unwrap();
		fs::create_dir_all(&inner).unwrap();
		create_project_root(&inner);
		add(
			&inner,
			false,
			&EnvSpecOptions {
				namespace: Some("inner-ns".to_owned()),
				..options()
			},
		)
		.unwrap();
		let outer_spec = r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"outer"},"spec":{"namespace":"outer-ns"}}"#;
		fs::write(outer.join("spec.json"), outer_spec).unwrap();

		set(
			&inner,
			&EnvSpecOptions {
				namespace: Some("updated-ns".to_owned()),
				..options()
			},
		)
		.unwrap();

		assert_eq!(spec_of(&inner)["spec"]["namespace"], "updated-ns");
		assert_eq!(
			spec_of(outer)["spec"]["namespace"],
			"outer-ns",
			"the enclosing project's spec must not be edited"
		);
	}
}
