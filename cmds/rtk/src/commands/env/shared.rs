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

	if inline {
		let namespace = options.namespace.as_deref().unwrap_or("default");
		let server = match (
			options.server.as_deref(),
			options.server_from_context.as_deref(),
		) {
			(Some(server), _) => server.to_owned(),
			(None, Some(context)) => server_from_kubeconfig_context(context)?,
			(None, None) => "https://localhost:6443".to_owned(),
		};
		let name = relative.replace('/', "-");
		let contents = format!(
			r#"{{
  apiVersion: 'tanka.dev/v1alpha1',
  kind: 'Environment',
  metadata: {{ name: '{name}' }},
  spec: {{ namespace: '{namespace}', apiServer: '{server}' }},
  data: {{}},
}}"#
		);
		fs::write(environment_directory.join("main.jsonnet"), contents)
			.context("write main.jsonnet")?;
		return Ok(());
	}

	fs::write(environment_directory.join("main.jsonnet"), "{}").context("write main.jsonnet")?;
	let mut spec = EnvironmentSpec::default();
	apply_spec_options(&mut spec, options)?;
	let environment = Environment::new()
		.with_metadata(ObjectMeta {
			name: Some(relative.clone()),
			namespace: Some(format!("{relative}/main.jsonnet")),
			..ObjectMeta::default()
		})
		.with_spec(spec)
		.build()
		.context("build spec.json")?;
	let contents = serde_json::to_string_pretty(&environment).context("serialize spec.json")?;
	fs::write(environment_directory.join("spec.json"), contents).context("write spec.json")?;
	Ok(())
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
