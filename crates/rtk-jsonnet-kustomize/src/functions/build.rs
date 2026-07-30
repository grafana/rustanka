use std::borrow::Cow;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;

use rtk_jsonnet_core as jsonnet;
use rtk_jsonnet_core::EvaluatorError as _;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Function;

impl<'a, E> jsonnet::Function<'a, E> for Function
where
	E: jsonnet::Evaluator<'a>,
{
	fn argv(&self) -> (usize, Option<usize>) {
		(2, None)
	}

	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["path", "opts"])
	}

	fn call<'b>(
		&self,
		evaluator: &E,
		arguments: <E as jsonnet::Evaluator<'a>>::Arguments<'b>,
	) -> Result<<E as jsonnet::Evaluator<'a>>::Value, <E as jsonnet::Evaluator<'a>>::Error> {
		let (path, options) = <(String, Options)>::deserialize(arguments)?;
		let called_from = options.called_from.ok_or_else(|| {
			<E as jsonnet::Evaluator<'a>>::Error::custom(
				"kustomizeBuild requires calledFrom field (usually std.thisFile)",
			)
		})?;
		let kustomize_path = resolve_path(&path, &called_from)
			.map_err(<E as jsonnet::Evaluator<'a>>::Error::custom)?;

		let mut command = Command::new("kustomize");
		command
			.arg("build")
			.arg(kustomize_path)
			.stdout(Stdio::piped())
			.stderr(Stdio::piped());

		let child = command.spawn().map_err(|error| {
			<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
				"failed to execute kustomize: {error}"
			))
		})?;
		let (status, stdout, stderr) =
			drain_output(child).map_err(<E as jsonnet::Evaluator<'a>>::Error::custom)?;

		if !status.success() {
			return Err(<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
				"kustomize build failed: {}",
				String::from_utf8_lossy(&stderr)
			)));
		}

		let yaml = String::from_utf8(stdout).map_err(|error| {
			<E as jsonnet::Evaluator<'a>>::Error::custom(format!(
				"failed to read kustomize output: {error}"
			))
		})?;
		let value = parse_output(&yaml).map_err(<E as jsonnet::Evaluator<'a>>::Error::custom)?;

		Ok(value.serialize(evaluator.create_serializer())?)
	}
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
	called_from: Option<String>,
}

fn resolve_path(path: &str, called_from: &str) -> Result<PathBuf, String> {
	if called_from.is_empty() {
		return Err("calledFrom cannot be an empty string".to_owned());
	}

	let called_from_path = Path::new(called_from);
	let called_from_dir = called_from_path
		.parent()
		.ok_or_else(|| format!("calledFrom has no parent directory: {called_from}"))?;
	if !called_from_dir.exists() {
		return Err(format!(
			"calledFrom directory does not exist: {}",
			called_from_dir.display()
		));
	}

	let relative_path = if path.starts_with('/') {
		Cow::Owned(format!(".{path}"))
	} else {
		Cow::Borrowed(path)
	};
	let full_path = called_from_dir.join(relative_path.as_ref());
	if !full_path.exists() {
		return Err(format!(
			"kustomize path does not exist: {}",
			full_path.display()
		));
	}
	if full_path.to_str().is_none() {
		return Err("invalid kustomize path".to_owned());
	}

	Ok(full_path)
}

fn drain_output(mut child: Child) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), String> {
	let mut stdout = child
		.stdout
		.take()
		.ok_or_else(|| "failed to capture kustomize stdout".to_owned())?;
	let mut stderr = child
		.stderr
		.take()
		.ok_or_else(|| "failed to capture kustomize stderr".to_owned())?;

	let stdout_handle = thread::Builder::new()
		.name("kustomize-stdout".to_owned())
		.spawn(move || {
			let mut output = Vec::new();
			stdout.read_to_end(&mut output).map(|_| output)
		})
		.map_err(|error| format!("failed to spawn kustomize stdout thread: {error}"))?;
	let stderr_handle = thread::Builder::new()
		.name("kustomize-stderr".to_owned())
		.spawn(move || {
			let mut output = Vec::new();
			stderr.read_to_end(&mut output).map(|_| output)
		})
		.map_err(|error| format!("failed to spawn kustomize stderr thread: {error}"))?;

	// Join both drainers even if waiting or either read fails, so neither pipe can block the child.
	let status = child.wait();
	let stdout = stdout_handle.join();
	let stderr = stderr_handle.join();

	let status = status.map_err(|error| format!("failed to wait for kustomize: {error}"))?;
	let stdout = stdout
		.map_err(|_| "failed to join kustomize stdout thread".to_owned())?
		.map_err(|error| format!("failed to read kustomize output: {error}"))?;
	let stderr = stderr
		.map_err(|_| "failed to join kustomize stderr thread".to_owned())?
		.map_err(|error| format!("failed to read kustomize stderr: {error}"))?;

	Ok((status, stdout, stderr))
}

fn parse_output(yaml: &str) -> Result<serde_json::Value, String> {
	let parse_options = serde_saphyr::Options {
		legacy_octal_numbers: true,
		budget: None,
		..Default::default()
	};
	let documents =
		serde_saphyr::from_multiple_with_options::<serde_json::Value>(yaml, parse_options)
			.map_err(|error| format!("failed to parse kustomize output: {error}"))?;

	let mut output = serde_json::Map::with_capacity(documents.len());
	let mut seen_keys = HashSet::with_capacity(documents.len());
	for document in documents {
		let serde_json::Value::Object(document) = document else {
			continue;
		};

		let base_key = manifest_key(&document);
		let mut key = base_key.clone();
		let mut suffix = 2;
		while seen_keys.contains(&key) {
			key = format!("{base_key}_{suffix}");
			suffix += 1;
		}
		seen_keys.insert(key.clone());
		output.insert(key, serde_json::Value::Object(document));
	}

	Ok(serde_json::Value::Object(output))
}

fn manifest_key(document: &serde_json::Map<String, serde_json::Value>) -> String {
	let kind = document
		.get("kind")
		.and_then(serde_json::Value::as_str)
		.map_or_else(|| "unknown".to_owned(), to_snake_case);
	let name = document
		.get("metadata")
		.and_then(serde_json::Value::as_object)
		.and_then(|metadata| metadata.get("name"))
		.and_then(serde_json::Value::as_str)
		.map_or_else(|| "unknown".to_owned(), to_snake_case);

	format!("{kind}_{name}")
}

fn to_snake_case(value: &str) -> String {
	let mut result = String::new();
	let characters: Vec<char> = value.chars().collect();

	for (index, &character) in characters.iter().enumerate() {
		if character.is_uppercase() {
			if !result.is_empty() {
				result.push('_');
			}
			result.push(character.to_lowercase().next().unwrap_or(character));
		} else if character == '-' {
			result.push('_');
		} else if character.is_ascii_digit() {
			let preceded_by_letter = index > 0 && characters[index - 1].is_ascii_alphabetic();
			let followed_by_letter = characters[index..]
				.iter()
				.find(|character| !character.is_ascii_digit())
				.is_some_and(char::is_ascii_alphabetic);
			if preceded_by_letter && followed_by_letter {
				result.push('_');
			}
			result.push(character);
		} else {
			result.push(character);
		}
	}

	result
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn leading_slash_is_resolved_relative_to_called_from() {
		let temporary = tempfile::tempdir().unwrap();
		let overlay = temporary.path().join("overlay");
		std::fs::create_dir(&overlay).unwrap();
		let called_from = temporary.path().join("main.jsonnet");

		let resolved = resolve_path("/overlay", called_from.to_str().unwrap()).unwrap();

		assert_eq!(resolved, temporary.path().join("./overlay"));
	}

	#[test]
	fn path_validation_rejects_empty_called_from_and_missing_target() {
		assert_eq!(
			resolve_path("overlay", "").unwrap_err(),
			"calledFrom cannot be an empty string"
		);

		let temporary = tempfile::tempdir().unwrap();
		let called_from = temporary.path().join("main.jsonnet");
		let error = resolve_path("missing", called_from.to_str().unwrap()).unwrap_err();
		assert!(error.starts_with("kustomize path does not exist:"));
	}

	#[test]
	fn parses_yaml_1_1_and_filters_non_objects() {
		let output = parse_output(
			r"
---
null
---
- not
- an
- object
---
kind: ConfigMap
metadata:
  name: permissions
mode: 0755
",
		)
		.unwrap();

		let object = output.as_object().unwrap();
		assert_eq!(object.len(), 1);
		assert_eq!(object["config_map_permissions"]["mode"], 493);
	}

	#[test]
	fn keys_ignore_namespace_and_suffix_duplicates() {
		let output = parse_output(
			r"
kind: HTTPRoute
metadata:
  name: k8s-App
  namespace: first
---
kind: HTTPRoute
metadata:
  name: k8s-App
  namespace: second
---
kind: HTTPRoute
metadata:
  name: k8s-App
",
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
				"h_t_t_p_route_k_8s__app",
				"h_t_t_p_route_k_8s__app_2",
				"h_t_t_p_route_k_8s__app_3",
			]
		);
	}

	#[test]
	fn missing_fields_use_unknown_key_parts() {
		let output = parse_output("metadata: {}\n").unwrap();
		assert!(output["unknown_unknown"].is_object());
	}
}
