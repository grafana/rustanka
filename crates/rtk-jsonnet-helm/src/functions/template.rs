use std::borrow::Cow;
use std::env;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;

use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::Context;
use rtk_jsonnet_core::EvaluatorError as _;
use rustc_hash::{FxBuildHasher, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::State;
use crate::cache::{Key, KeyBuilder};

#[derive(Debug)]
pub struct Function {
	state: Arc<State>,
}

impl Function {
	pub(crate) fn new(state: Arc<State>) -> Function {
		Function { state }
	}
}

impl<E> jsonnet::Function<E> for Function
where
	E: jsonnet::Evaluator<Context = E> + Context<Evaluator = E>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(3, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["name", "chart", "opts"])
	}

	fn call<'b>(&self, evaluator: &E, arguments: E::Arguments) -> Result<E::Value, E::Error> {
		let (name, chart, options) = <(String, String, Options)>::deserialize(arguments)?;

		let called_from = &options.called_from;

		let chart_path = {
			if called_from.is_empty() {
				return Err(E::Error::custom("calledFrom cannot be an empty string"));
			}

			let called_from_path: &Path = called_from.as_ref();

			let Some(called_from_dir) = called_from_path.parent() else {
				return Err(E::Error::custom(format!(
					"calledFrom has no parent directory: {called_from}"
				)));
			};

			if !called_from_dir.exists() {
				return Err(E::Error::custom(format!(
					"calledFrom directory does not exist: {}",
					called_from_dir.display()
				)));
			}

			let chart_relative = relative_chart_path(Path::new(&chart));
			let chart_absolute = called_from_dir.join(&*chart_relative);

			if !chart_absolute.exists() {
				return Err(E::Error::custom(format!(
					"chart path does not exist: {}",
					chart_absolute.display()
				)));
			}

			chart_absolute
		};

		// Benchmarking escape hatch: when RTK_HELM_DISABLE_MEMOIZATION is set,
		// bypass the in-memory cache entirely so every helmTemplate call invokes
		// helm. Used to measure the true cost of helm rendering without any
		// deduplication.
		let cache_disabled = env::var_os("RTK_HELM_DISABLE_MEMOIZATION").is_some();

		let value = if cache_disabled {
			self.render::<E>(&name, &chart_path, &options)?
		} else {
			self.cached_or_render::<E>(&name, &chart_path, &options)?
		};

		let serializer = evaluator.create_serializer();
		Ok(value.serialize(serializer)?)
	}
}

fn relative_chart_path(chart: &Path) -> Cow<'_, Path> {
	if !chart.has_root() && !matches!(chart.components().next(), Some(Component::Prefix(_))) {
		return Cow::Borrowed(chart);
	}

	let mut relative = PathBuf::new();
	for component in chart.components() {
		if !matches!(component, Component::Prefix(_) | Component::RootDir) {
			relative.push(component.as_os_str());
		}
	}
	Cow::Owned(relative)
}

impl Function {
	fn cached_or_render<E>(
		&self,
		name: &str,
		chart_path: &Path,
		options: &Options,
	) -> Result<serde_json::Value, E::Error>
	where
		E: jsonnet::Evaluator<Context = E> + Context<Evaluator = E>,
	{
		self.cached_or_compute(name, chart_path, options, || {
			self.render::<E>(name, chart_path, options)
		})
	}

	fn cached_or_compute<T>(
		&self,
		name: &str,
		chart_path: &Path,
		options: &Options,
		render: impl FnOnce() -> Result<serde_json::Value, T>,
	) -> Result<serde_json::Value, T> {
		let mut directory = self
			.state
			.cache
			.directory(Path::new(&options.called_from))
			.map(|path| crate::cache::canonicalize_with_missing(&path));
		if let Some(cache_directory) = &directory {
			let chart_directory = chart_path
				.canonicalize()
				.unwrap_or_else(|_| chart_path.to_owned());
			if cache_directory.starts_with(&chart_directory) {
				tracing::warn!(
					chart = ?chart_path,
					cache = ?cache_directory,
					"helm disk cache cannot be stored inside its chart"
				);
				directory = None;
			}
		}
		// When the caller named a namespace, helm takes it as an override and
		// nothing else in the environment can reach `.Release.Namespace`.
		// Otherwise helm resolves one, and its answer is part of what the render
		// depends on — so a render that cannot be asked cannot be keyed either.
		let resolved_namespace = match options.namespace.as_deref() {
			Some(_) => None,
			None => match self.state.helm_namespace() {
				Some(namespace) => Some(namespace),
				None => return render(),
			},
		};

		let key = match Key::render(
			name,
			chart_path,
			options,
			directory.as_deref(),
			resolved_namespace,
		) {
			Ok(key) => key,
			Err(error) => {
				tracing::warn!(chart = ?chart_path, %error, "helm cache key is unavailable");
				return render();
			}
		};

		if let Some(value) = self.state.cache.get(key) {
			return Ok(value);
		}

		let computation = self.state.cache.computation(key);
		let _guard = computation
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		if let Some(value) = self.state.cache.get(key) {
			return Ok(value);
		}

		let helm_identity = directory
			.as_deref()
			.and_then(|_| self.state.helm_identity());
		if let (Some(directory), Some(helm_identity)) = (directory.as_deref(), helm_identity)
			&& let Some(value) = crate::cache::Cache::read_disk(key, directory, helm_identity)
		{
			self.state.cache.insert(key, value.clone());
			return Ok(value);
		}

		let value = render()?;
		if !matches!(
			Key::render(
				name,
				chart_path,
				options,
				directory.as_deref(),
				resolved_namespace
			),
			Ok(current_key) if current_key == key
		) {
			tracing::warn!(chart = ?chart_path, "helm inputs changed while rendering; result was not cached");
			return Ok(value);
		}
		self.state.cache.insert(key, value.clone());
		if let (Some(directory), Some(helm_identity)) = (directory.as_deref(), helm_identity) {
			crate::cache::Cache::write_disk(key, directory, helm_identity, &value);
		}
		Ok(value)
	}

	fn render<E>(
		&self,
		name: &str,
		chart_path: &Path,
		options: &Options,
	) -> Result<serde_json::Value, E::Error>
	where
		E: jsonnet::Evaluator<Context = E> + Context<Evaluator = E>,
	{
		let mut command = self.state.helm_command();
		command.arg("template");
		command.arg(name);
		command.arg(chart_path);

		if let Some(namespace) = options.namespace.as_ref() {
			command.arg("--namespace");
			command.arg(namespace);
		}

		if options.include_crds {
			command.arg("--include-crds");
		}

		if options.no_hooks {
			command.arg("--no-hooks");
		}

		for api_version in &options.api_versions {
			command.arg("--api-versions");
			command.arg(api_version);
		}

		if options.values.is_some() {
			command.arg("--values=-");
			command.stdin(Stdio::piped());
		}

		command.stdout(Stdio::piped());
		command.stderr(Stdio::piped());

		let mut child = command
			.spawn()
			.map_err(|error| E::Error::custom(format!("failed to execute helm: {error}")))?;

		if let Some(json) = options.values.as_ref() {
			let mut stdin = child
				.stdin
				.take()
				.ok_or_else(|| E::Error::custom("failed to capture helm stdin"))?;
			serde_json::to_writer(&mut stdin, json).map_err(|error| {
				E::Error::custom(format!("failed to write values to helm stdin: {error}"))
			})?;
			stdin.flush().map_err(|error| {
				E::Error::custom(format!("failed to write values to helm stdin: {error}"))
			})?;
		}

		let (status, stdout_buffer, stderr_buffer) =
			drain_output(child).map_err(E::Error::custom)?;

		if !status.success() {
			let stderr = String::from_utf8_lossy(&stderr_buffer);
			return Err(E::Error::custom(format!("helm template failed: {stderr}")));
		}

		let yaml_content = String::from_utf8(stdout_buffer)
			.map_err(|error| E::Error::custom(format!("invalid UTF-8 in helm output: {error}")))?;

		let value = parse_helm_yaml_output(&yaml_content, options.name_format.as_deref())
			.map_err(E::Error::custom)?;
		Ok(value)
	}
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
	#[serde(default)]
	api_versions: Vec<String>,
	called_from: String,
	#[serde(default)]
	no_hooks: bool,
	namespace: Option<String>,
	name_format: Option<String>,
	#[serde(default = "default_true")]
	include_crds: bool,
	values: Option<serde_json::Value>,
}

impl Options {
	pub(crate) fn hash_cache_key(&self, builder: &mut KeyBuilder) {
		builder.bytes(&(self.api_versions.len() as u64).to_le_bytes());
		for api_version in &self.api_versions {
			builder.string(api_version);
		}
		builder.boolean(self.no_hooks);
		builder.optional_string(self.namespace.as_deref());
		builder.optional_string(self.name_format.as_deref());
		builder.boolean(self.include_crds);
		builder.boolean(self.values.is_some());
		if let Some(values) = &self.values {
			builder.json(values);
		}
	}
}

const fn default_true() -> bool {
	true
}

fn drain_output(mut child: Child) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), String> {
	let mut stdout = child
		.stdout
		.take()
		.ok_or_else(|| "failed to capture helm stdout".to_owned())?;
	let mut stderr = child
		.stderr
		.take()
		.ok_or_else(|| "failed to capture helm stderr".to_owned())?;

	let stdout_handle = thread::Builder::new()
		.name("helm-stdout".to_owned())
		.spawn(move || {
			let mut output = Vec::with_capacity(4096);
			stdout.read_to_end(&mut output).map(|_| output)
		})
		.map_err(|error| format!("failed to spawn helm stdout thread: {error}"))?;
	let stderr_handle = thread::Builder::new()
		.name("helm-stderr".to_owned())
		.spawn(move || {
			let mut output = Vec::with_capacity(4096);
			stderr.read_to_end(&mut output).map(|_| output)
		})
		.map_err(|error| format!("failed to spawn helm stderr thread: {error}"))?;

	let status = child.wait();
	let stdout = stdout_handle.join();
	let stderr = stderr_handle.join();

	let status = status.map_err(|error| format!("failed to wait for helm: {error}"))?;
	let stdout = stdout
		.map_err(|_| "failed to join helm stdout thread".to_owned())?
		.map_err(|error| format!("failed to read helm stdout: {error}"))?;
	let stderr = stderr
		.map_err(|_| "failed to join helm stderr thread".to_owned())?
		.map_err(|error| format!("failed to read helm stderr: {error}"))?;

	Ok((status, stdout, stderr))
}

fn parse_helm_yaml_output(
	yaml_content: &str,
	name_format: Option<&str>,
) -> Result<serde_json::Value, String> {
	let parse_options = serde_saphyr::Options {
		legacy_octal_numbers: true,
		budget: None,
		..Default::default()
	};
	let documents =
		serde_saphyr::from_multiple_with_options::<serde_json::Value>(yaml_content, parse_options)
			.map_err(|error| format!("failed to parse helm output: {error}"))?;

	let mut output = serde_json::Map::with_capacity(documents.len());
	let mut seen_keys = FxHashSet::with_capacity_and_hasher(documents.len(), FxBuildHasher);
	for document in documents {
		let serde_json::Value::Object(document) = document else {
			continue;
		};

		let base_key = manifest_key(&document, name_format);
		let mut final_key = base_key.clone();
		let mut counter = 2;
		while seen_keys.contains(&final_key) {
			final_key = format!("{base_key}_{counter}");
			counter += 1;
		}
		seen_keys.insert(final_key.clone());
		output.insert(final_key, serde_json::Value::Object(document));
	}

	Ok(serde_json::Value::Object(output))
}

fn manifest_key(
	document: &serde_json::Map<String, serde_json::Value>,
	name_format: Option<&str>,
) -> String {
	let use_namespace = name_format.is_some_and(|format| {
		format.contains("metadata.namespace") || format.contains(".or .metadata.namespace")
	});
	let kind = document
		.get("kind")
		.and_then(serde_json::Value::as_str)
		.map_or_else(|| "unknown".to_owned(), to_snake_case);
	let Some(metadata) = document
		.get("metadata")
		.and_then(serde_json::Value::as_object)
	else {
		return "unknown".to_owned();
	};
	let name = metadata
		.get("name")
		.and_then(serde_json::Value::as_str)
		.map_or_else(|| "unknown".to_owned(), to_snake_case);

	if use_namespace {
		let namespace = metadata
			.get("namespace")
			.and_then(serde_json::Value::as_str)
			.map_or_else(|| "cluster".to_owned(), to_snake_case);
		format!("{namespace}_{kind}_{name}")
	} else {
		format!("{kind}_{name}")
	}
}

/// Convert a string to snake_case (lowercase with underscores)
/// Matches Go Tanka's naming behavior which inserts underscores:
/// - Before uppercase letters (CamelCase -> camel_case)
/// - Between letter-digit-letter sequences (k8s -> k_8s)
///
/// Does not insert an underscore when a digit is at a word boundary (`flux2` stays `flux2`).
fn to_snake_case(s: &str) -> String {
	let mut result = String::new();
	let chars: Vec<char> = s.chars().collect();

	for (i, &ch) in chars.iter().enumerate() {
		if ch.is_uppercase() {
			// Add underscore before uppercase letters (except at start)
			if !result.is_empty() {
				result.push('_');
			}
			// to_lowercase() always returns at least one char, but use unwrap_or for safety
			result.push(ch.to_lowercase().next().unwrap_or(ch));
		} else if ch == '-' {
			// Replace hyphens with underscores
			result.push('_');
		} else if ch.is_ascii_digit() {
			// Add underscore between letter and digit ONLY if there's a letter eventually
			// after the consecutive digits. This matches Go Tanka:
			// - k8s -> k_8s (letter after digit)
			// - o11y -> o_11y (letter eventually after digits)
			// - flux2 -> flux2 (no letter after digit, at end or before hyphen)
			let prev_is_letter = i > 0 && chars[i - 1].is_ascii_alphabetic();
			if prev_is_letter {
				// Look ahead past all consecutive digits to see if there's a letter
				let has_letter_after_digits = chars[i..]
					.iter()
					.find(|character| !character.is_ascii_digit())
					.is_some_and(char::is_ascii_alphabetic);
				if has_letter_after_digits {
					result.push('_');
				}
			}
			result.push(ch);
		} else {
			result.push(ch);
		}
	}

	result
}

#[cfg(test)]
mod tests {
	use std::fs;
	use std::path::{Path, PathBuf};
	use std::sync::Arc;
	use std::sync::atomic::{AtomicUsize, Ordering};

	use serde_json::json;
	use tempfile::tempdir;

	use super::{Function, Options, parse_helm_yaml_output, relative_chart_path};
	use crate::State;

	fn cache_directory(called_from: &Path) -> Option<PathBuf> {
		Some(called_from.parent()?.join("target/helm"))
	}

	fn function(disk: bool) -> Function {
		let state = Arc::new(State::new(disk.then_some(cache_directory)));
		state
			.helm_identity
			.set(Ok(b"helm test version".to_vec().into_boxed_slice()))
			.unwrap();
		Function::new(state)
	}

	fn cache_options(called_from: &Path) -> Options {
		serde_json::from_value(json!({
			"calledFrom": called_from,
			"namespace": "default",
			"values": { "enabled": true },
		}))
		.unwrap()
	}

	#[test]
	fn include_crds_defaults_to_true_and_accepts_false() {
		let defaults: Options = serde_json::from_value(json!({
			"calledFrom": "/tmp/main.jsonnet"
		}))
		.unwrap();
		assert!(defaults.include_crds);

		let disabled: Options = serde_json::from_value(json!({
			"calledFrom": "/tmp/main.jsonnet",
			"includeCrds": false
		}))
		.unwrap();
		assert!(!disabled.include_crds);
	}

	#[test]
	fn rooted_chart_paths_are_made_relative() {
		#[cfg(unix)]
		assert_eq!(
			relative_chart_path(Path::new("/charts/example")),
			Path::new("charts/example")
		);

		#[cfg(windows)]
		for chart in [
			Path::new(r"C:\charts\example"),
			Path::new(r"\charts\example"),
			Path::new(r"\\server\share\charts\example"),
		] {
			assert_eq!(relative_chart_path(chart), Path::new(r"charts\example"));
		}
	}

	#[test]
	fn a_fresh_function_reuses_the_persisted_render() {
		let temp = tempdir().unwrap();
		let called_from = temp.path().join("main.jsonnet");
		let chart = temp.path().join("chart");
		fs::create_dir_all(chart.join("templates")).unwrap();
		fs::write(chart.join("Chart.yaml"), "name: test\nversion: 1.0.0\n").unwrap();
		fs::write(chart.join("templates/value.yaml"), "value").unwrap();
		let options = cache_options(&called_from);
		let renders = AtomicUsize::new(0);

		let first = function(true)
			.cached_or_compute("release", &chart, &options, || {
				renders.fetch_add(1, Ordering::SeqCst);
				Ok::<_, ()>(json!({ "rendered": true }))
			})
			.unwrap();
		let second = function(true)
			.cached_or_compute("release", &chart, &options, || {
				renders.fetch_add(1, Ordering::SeqCst);
				Ok::<_, ()>(json!({ "rendered": false }))
			})
			.unwrap();

		assert_eq!(first, json!({ "rendered": true }));
		assert_eq!(second, first);
		assert_eq!(renders.load(Ordering::SeqCst), 1);
	}

	#[test]
	fn failed_renders_are_not_cached() {
		let temp = tempdir().unwrap();
		let called_from = temp.path().join("main.jsonnet");
		let chart = temp.path().join("chart.tgz");
		fs::write(&chart, "chart").unwrap();
		let options = cache_options(&called_from);
		let function = function(true);

		let failed = function.cached_or_compute("release", &chart, &options, || {
			Err::<serde_json::Value, _>("render failed")
		});
		assert_eq!(failed, Err("render failed"));
		assert_eq!(
			function
				.cached_or_compute("release", &chart, &options, || Ok::<_, &str>(json!("ok")))
				.unwrap(),
			json!("ok")
		);
	}

	#[test]
	fn inputs_changed_during_render_are_not_cached() {
		let temp = tempdir().unwrap();
		let called_from = temp.path().join("main.jsonnet");
		let chart = temp.path().join("chart.tgz");
		fs::write(&chart, "before").unwrap();
		let options = cache_options(&called_from);
		let function = function(true);
		let renders = AtomicUsize::new(0);

		let first = function
			.cached_or_compute("release", &chart, &options, || {
				renders.fetch_add(1, Ordering::SeqCst);
				fs::write(&chart, "after").unwrap();
				Ok::<_, ()>(json!("first"))
			})
			.unwrap();
		let second = function
			.cached_or_compute("release", &chart, &options, || {
				renders.fetch_add(1, Ordering::SeqCst);
				Ok::<_, ()>(json!("second"))
			})
			.unwrap();

		assert_eq!(first, json!("first"));
		assert_eq!(second, json!("second"));
		assert_eq!(renders.load(Ordering::SeqCst), 2);
	}

	#[test]
	fn disabled_disk_cache_is_scoped_to_each_function_state() {
		let temp = tempdir().unwrap();
		let called_from = temp.path().join("main.jsonnet");
		let chart = temp.path().join("chart.tgz");
		fs::write(&chart, "chart").unwrap();
		let options = cache_options(&called_from);
		let renders = AtomicUsize::new(0);

		for _ in 0..2 {
			function(false)
				.cached_or_compute("release", &chart, &options, || {
					renders.fetch_add(1, Ordering::SeqCst);
					Ok::<_, ()>(json!("rendered"))
				})
				.unwrap();
		}

		assert_eq!(renders.load(Ordering::SeqCst), 2);
		assert!(!temp.path().join("target").exists());
	}

	#[test]
	fn a_project_root_chart_does_not_cache_inside_itself() {
		let temp = tempdir().unwrap();
		let called_from = temp.path().join("main.jsonnet");
		fs::write(
			temp.path().join("Chart.yaml"),
			"name: test\nversion: 1.0.0\n",
		)
		.unwrap();
		let options = cache_options(&called_from);
		let renders = AtomicUsize::new(0);

		for _ in 0..2 {
			function(true)
				.cached_or_compute("release", temp.path(), &options, || {
					renders.fetch_add(1, Ordering::SeqCst);
					Ok::<_, ()>(json!("rendered"))
				})
				.unwrap();
		}

		assert_eq!(renders.load(Ordering::SeqCst), 2);
		assert!(!temp.path().join("target").exists());
	}

	#[test]
	fn suffixes_duplicate_keys_across_documents() {
		let output = parse_helm_yaml_output(
			r"
kind: ConfigMap
metadata:
  name: settings
---
kind: ConfigMap
metadata:
  name: settings
---
kind: ConfigMap
metadata:
  name: settings
",
			None,
		)
		.unwrap();
		let keys: Vec<_> = output
			.as_object()
			.unwrap()
			.keys()
			.map(String::as_str)
			.collect();
		assert_eq!(
			keys,
			[
				"config_map_settings",
				"config_map_settings_2",
				"config_map_settings_3"
			]
		);
	}

	#[test]
	fn duplicate_suffixes_do_not_overwrite_existing_keys() {
		let output = parse_helm_yaml_output(
			r"
kind: ConfigMap
metadata:
  name: settings
---
kind: ConfigMap
metadata:
  name: settings_2
---
kind: ConfigMap
metadata:
  name: settings
",
			None,
		)
		.unwrap();
		let keys: Vec<_> = output
			.as_object()
			.unwrap()
			.keys()
			.map(String::as_str)
			.collect();
		assert_eq!(
			keys,
			[
				"config_map_settings",
				"config_map_settings_2",
				"config_map_settings_3"
			]
		);
	}

	#[test]
	fn namespace_aware_keys_default_to_cluster() {
		let output = parse_helm_yaml_output(
			r"
kind: Service
metadata:
  name: api
  namespace: production
---
kind: Service
metadata:
  name: api
",
			Some("{{ .metadata.namespace }}"),
		)
		.unwrap();
		let keys: Vec<_> = output
			.as_object()
			.unwrap()
			.keys()
			.map(String::as_str)
			.collect();
		assert_eq!(keys, ["production_service_api", "cluster_service_api"]);
	}
}
