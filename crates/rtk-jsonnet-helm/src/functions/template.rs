use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::env;
use std::fmt::Write;
use std::fs;
use std::io::{BufWriter, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::EvaluatorError as _;
use rustc_hash::FxBuildHasher;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::State;

#[derive(Debug)]
pub struct Function {
	state: &'static State,
}

impl Function {
	pub(crate) fn new(state: &'static State) -> Function {
		Function { state }
	}
}

impl<'a, E> jsonnet::Function<'a, E> for Function
where
	E: jsonnet::Evaluator<'a>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(3, None)
	}

	fn call<'b>(
		&self,
		evaluator: &E,
		arguments: <E as jsonnet::Evaluator<'a>>::Arguments<'b>,
	) -> Result<<E as jsonnet::Evaluator<'a>>::Value, <E as jsonnet::Evaluator<'a>>::Error> {
		let (name, chart, options) = <(&'b str, &'b str, Options<'b>)>::deserialize(arguments)?;

		let called_from = &*options.called_from;

		let mut chart_path = {
			if called_from.is_empty() {
				return Err(<E as jsonnet::Evaluator<'a>>::Error::custom(
					"calledFrom cannot be an empty string",
				));
			}

			let called_from_path: &Path = called_from.as_ref();

			let Some(called_from_dir) = called_from_path.parent() else {
				return Err(<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
					"calledFrom has no parent directory: {called_from}"
				)));
			};

			if !called_from_dir.exists() {
				return Err(<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
					"calledFrom directory does not exist: {}",
					called_from_dir.display()
				)));
			}

			let chart_relative = if chart.starts_with('/') {
				Cow::Owned(format!(".{chart}"))
			} else {
				Cow::Borrowed(chart)
			};
			let chart_absolute = called_from_dir.join(&*chart_relative);

			if !chart_absolute.exists() {
				return Err(<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
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

		let cache_key = if !cache_disabled {
			chart_path.push("Chart.yaml");
			let chart_meta = fs::read_to_string(&chart_path).ok();
			chart_path.pop();

			let cache_key = State::cache_key(name, &chart_path, chart_meta.as_deref(), &options);

			let cache_read = self
				.state
				.template_cache
				.read()
				.expect("the cache should not be poisoned");

			if let Some(cached_json) = cache_read.get(&cache_key) {
				let serializer = evaluator.create_serializer();
				let value = cached_json.serialize(serializer)?;
				return Ok(value);
			}

			Some(cache_key)
		} else {
			None
		};

		let mut command = Command::new("helm");
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

		for api_version in options.api_versions {
			command.arg("--api-versions");
			command.arg(api_version);
		}

		if options.values.is_some() {
			command.arg("--values=-");
			command.stdin(Stdio::piped());
		}

		command.stdout(Stdio::piped());
		command.stderr(Stdio::piped());

		let mut child = command.spawn().map_err(|error| {
			<E as jsonnet::Evaluator<'a>>::Error::custom(format!("failed to execute helm: {error}"))
		})?;

		if let Some(json) = options.values.as_ref() {
			if let Some(stdin) = child.stdin.take() {
				let writer = BufWriter::new(stdin);
				if let Err(error) = serde_json::to_writer(writer, json) {
					return Err(<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
						"failed to write values to helm stdin: {error}"
					)));
				}
			}
		}

		let stdout_handle = thread::spawn({
			let mut stdout = child.stdout.take().ok_or_else(|| {
				<E as jsonnet::Evaluator<'a>>::Error::custom("failed to capture helm stdout")
			})?;
			move || {
				let mut stdout_buffer = Vec::with_capacity(4096);
				let _ = stdout.read_to_end(&mut stdout_buffer);
				stdout_buffer
			}
		});

		let stderr_handle = thread::spawn({
			let mut stderr = child.stderr.take().ok_or_else(|| {
				<E as jsonnet::Evaluator<'a>>::Error::custom("failed to capture helm stdout")
			})?;
			move || {
				let mut stderr_buffer = Vec::with_capacity(4096);
				let _ = stderr.read_to_end(&mut stderr_buffer);
				stderr_buffer
			}
		});

		let status = child.wait().map_err(|error| {
			<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
				"failed to wait for helm: {error}"
			))
		})?;

		let stdout_buffer = stdout_handle.join().map_err(|_| {
			<E as jsonnet::Evaluator<'a>>::Error::custom("failed to join stdout thread")
		})?;

		let stderr_buffer = stderr_handle.join().map_err(|_| {
			<E as jsonnet::Evaluator<'a>>::Error::custom("failed to join stderr thread")
		})?;

		if !status.success() {
			let stderr = String::from_utf8_lossy(&stderr_buffer);
			return Err(<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
				"helm telmplate failed:\n{stderr}"
			)));
		}

		let yaml_content = String::from_utf8(stdout_buffer).map_err(|error| {
			<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
				"invalid utf-8 in helm output: {error}"
			))
		})?;

		let value = (|| -> Result<_, <E as jsonnet::Evaluator<'a>>::Error> {
			let mut value = serde_json::value::Map::with_capacity(16);

			let parse_options = serde_saphyr::Options {
				legacy_octal_numbers: true,
				budget: None,
				..Default::default()
			};

			let documents = serde_saphyr::from_multiple_with_options::<serde_json::Value>(
				&yaml_content,
				parse_options,
			)
			.map_err(|error| {
				<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
					"failed to parse helm output: {error}"
				))
			})?;

			for document in documents {
				if matches!(&document, serde_json::Value::Null) {
					continue;
				}

				let serde_json::Value::Object(document) = document else {
					continue;
				};

				let mut seen_keys = FxHashMap::with_hasher(FxBuildHasher::default());

				let manifest_key = (|| -> Result<String, <E as jsonnet::Evaluator<'a>>::Error> {
					// TODO: do this properly?
					let use_namespace_in_key = options
						.name_format
						.map(|fmt| {
							fmt.contains("metadata.namespace")
								|| fmt.contains(".or .metadata.namespace")
						})
						.unwrap_or(false);

					let kind = document
						.get(&"kind".to_owned())
						.and_then(|v| match v {
							serde_json::Value::String(s) => Some(to_snake_case(&s.to_string())),
							_ => None,
						})
						.unwrap_or_else(|| "unknown".to_string());

					let metadata = document.get(&"metadata".to_owned());

					if let Some(serde_json::Value::Object(meta)) = metadata {
						let name = meta
							.get(&"name".to_owned())
							.and_then(|v| match v {
								serde_json::Value::String(s) => Some(to_snake_case(&s.to_string())),
								_ => None,
							})
							.unwrap_or_else(|| "unknown".to_string());

						// If nameFormat suggests using namespace, include it in the key
						if use_namespace_in_key {
							let namespace = meta
								.get(&"namespace".to_owned())
								.and_then(|v| match v {
									serde_json::Value::String(s) => {
										Some(to_snake_case(&s.to_string()))
									}
									_ => None,
								})
								.unwrap_or_else(|| "cluster".to_string());

							return Ok(format!("{}_{}_{}", namespace, kind, name));
						} else {
							return Ok(format!("{}_{}", kind, name));
						}
					}

					Ok("unknown".to_owned())
				})()?;

				let mut final_key = manifest_key.clone();
				match seen_keys.entry(manifest_key) {
					Entry::Occupied(mut entry) => {
						let count = entry.get_mut();
						*count += 1;
						let _ = final_key.write_fmt(format_args!("_{count}"));
					}
					Entry::Vacant(count) => {
						count.insert(1);
					}
				}

				value.insert(final_key, serde_json::Value::Object(document));
			}

			Ok(serde_json::Value::Object(value))
		})()?;

		let serializer = evaluator.create_serializer();
		let evaluator_value = value.serialize(serializer)?;

		if let Some(cache_key) = cache_key {
			let mut write = self
				.state
				.template_cache
				.write()
				.expect("the template cache should not be poisoned");

			write.insert(cache_key, value);
		}

		Ok(evaluator_value)
	}
}

#[derive(Debug, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct Options<'a> {
	#[serde(default)]
	api_versions: Vec<&'a str>,
	called_from: &'a str,
	#[serde(default)]
	no_hooks: bool,
	namespace: Option<&'a str>,
	name_format: Option<&'a str>,
	#[serde(default)]
	include_crds: bool,
	values: Option<serde_json::Value>,
}

/// Convert a string to snake_case (lowercase with underscores)
/// Matches Go Tanka's naming behavior which inserts underscores:
/// - Before uppercase letters (CamelCase -> camel_case)
/// - Between letter-digit-letter sequences (k8s -> k_8s)
/// Note: Does NOT insert underscore when digit is at word boundary (flux2 stays flux2)
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
					.skip_while(|c| c.is_ascii_digit())
					.next()
					.map(|c| c.is_ascii_alphabetic())
					.unwrap_or(false);
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
