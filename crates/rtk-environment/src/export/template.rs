//! Rendering export filenames from the `--format` Go template.
//!
//! Mirrors Tanka's `pkg/tanka/export.go`, including its treatment of `/`: a
//! separator written in the template is an intentional subdirectory, while one
//! that appears in a *rendered value* (`apps/v1`, say) must not create one. Tanka
//! swaps template separators for a placeholder before rendering and swaps them
//! back afterwards, so this does too.

use std::borrow::Cow;
use std::collections::HashMap;

use gtmpl::{Context, FuncError, Template, Value as TemplateValue};
use regex::Regex;
use rtk_jsonnet::{EvaluationValue, Hidden};
use rtk_spec::canonical::Environment;
use rtk_spec::v1alpha1::EnvironmentData;
use serde_json::Value;

use crate::export::Error;
use crate::export::process;

/// Stands in for a `/` that the template asked for, while `/` from rendered
/// values is replaced. Tanka uses the same BEL character.
const SEPARATOR_PLACEHOLDER: char = '\x07';

/// The default `--format`, as tk defines it.
pub const DEFAULT_FORMAT: &str =
	"{{.apiVersion}}.{{.kind}}-{{or .metadata.name .metadata.generateName}}";

/// A `--format` template, compiled once per export.
///
/// Environment references are substituted per environment rather than looked up
/// per manifest ([`Template::specialize`]), which matters when an environment
/// has thousands of manifests.
#[derive(Debug)]
pub(crate) struct FilenameTemplate {
	/// The format with template `/` separators replaced by the placeholder.
	format: String,
	/// Matches every `env.…` reference that gets substituted. Owned here rather
	/// than in a global, so it lives exactly as long as the export.
	environment_references: Regex,
}

impl FilenameTemplate {
	/// Compile `format`, failing if it is not a valid template.
	///
	/// Validation renders a synthetic manifest, so a broken template is
	/// reported before any environment is evaluated.
	pub(crate) fn new(format: &str) -> Result<FilenameTemplate, Error> {
		let mut placeholder = [0u8; 4];
		let template = FilenameTemplate {
			format: replace_outside_actions(
				format,
				"/",
				SEPARATOR_PLACEHOLDER.encode_utf8(&mut placeholder),
			),
			environment_references: Regex::new(
				r"env\.metadata\.labels\.\w+|env\.spec\.namespace|env\.metadata\.name",
			)
			.expect("the environment reference pattern is valid"),
		};

		template.validate().map_err(|source| Error::InvalidFormat {
			format: format.into(),
			source: Box::new(source),
		})?;

		Ok(template)
	}

	/// Bake an environment's values into the template.
	pub(crate) fn specialize<'a, D>(
		&self,
		environment: &Environment<'a, D>,
	) -> Result<SpecializedTemplate, Error>
	where
		D: EnvironmentData<'a>,
	{
		let name = environment.metadata.name.as_deref().unwrap_or_default();
		let namespace = environment.spec.namespace();
		let labels = environment.metadata.labels.as_ref();

		let specialized = self.environment_references.replace_all(
			&self.format,
			|captures: &regex::Captures<'_>| {
				let reference = captures
					.get(0)
					.expect("capture group zero always exists")
					.as_str();
				let value = if let Some(label) = reference.strip_prefix("env.metadata.labels.") {
					labels
						.and_then(|labels| labels.get(label))
						.map_or("", String::as_str)
				} else if reference == "env.spec.namespace" {
					namespace
				} else {
					name
				};
				// Quoted, so the substituted value is a template string
				// literal rather than more template syntax.
				format!("{value:?}")
			},
		);

		let mut template = Template::default();
		template.add_func("default", template_default);
		template
			.parse(specialized.as_ref())
			.map_err(|source| Error::InvalidFormat {
				format: specialized.into_owned(),
				source: Box::new(anyhow::anyhow!("{source:?}")),
			})?;

		Ok(SpecializedTemplate { template })
	}

	fn validate(&self) -> Result<(), anyhow::Error> {
		let environment = Environment::new()
			.with_spec(rtk_spec::canonical::EnvironmentSpec::default())
			.build()?;
		let manifest = serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": {
				"name": "validation",
				"generateName": "validation-",
				"namespace": "default",
				"labels": { "app": "validation" },
			},
		});

		let rendered = self.specialize(&environment)?.render(&manifest)?;
		if rendered.is_empty() {
			anyhow::bail!("the template rendered an empty filename");
		}

		Ok(())
	}
}

/// A [`FilenameTemplate`] with one environment's values baked in.
pub(crate) struct SpecializedTemplate {
	template: Template,
}

impl SpecializedTemplate {
	/// Render `manifest`'s filename, without its extension.
	pub(crate) fn render(&self, manifest: &Value) -> Result<String, Error> {
		let context = Context::from(TemplateValue::Map(template_context(manifest)));
		self.render_context(context)
	}

	/// Render an evaluated manifest without manifesting it through JSON.
	pub(crate) fn render_evaluated(&self, manifest: &EvaluationValue) -> Result<String, Error> {
		let context = Context::from(TemplateValue::Map(evaluation_template_context(manifest)?));
		self.render_context(context)
	}

	fn render_context(&self, context: Context) -> Result<String, Error> {
		let rendered = self
			.template
			.render(&context)
			.map_err(|source| Error::Render(anyhow::anyhow!("{source:?}")))?;

		// Drop empty segments left behind by absent optional fields, but keep
		// `<no value>`, which is what tk writes for cluster-scoped resources.
		let rendered: String = rendered
			.split('/')
			.filter(|segment| !segment.is_empty())
			.collect::<Vec<_>>()
			.join("/");

		// A `/` in a rendered value becomes `-`, then the placeholder becomes the
		// separator it stood in for.
		Ok(rendered
			.replace('/', "-")
			.replace(SEPARATOR_PLACEHOLDER, "/"))
	}
}

fn evaluation_template_context(
	manifest: &EvaluationValue,
) -> Result<HashMap<String, TemplateValue>, Error> {
	let mut context = HashMap::with_capacity(3);
	let Some(manifest) = manifest.as_object() else {
		context.insert("metadata".into(), TemplateValue::Map(HashMap::new()));
		return Ok(context);
	};

	for field in ["kind", "apiVersion"] {
		if let Some(value) = manifest.get(field, Hidden::Skip)? {
			context.insert(field.to_owned(), evaluation_to_template(&value)?);
		}
	}

	let mut mapped = HashMap::new();
	if let Some(metadata) = manifest.get("metadata", Hidden::Skip)?
		&& let Some(metadata) = metadata.as_object()
	{
		for field in metadata.field_names() {
			let value = metadata.get_or_bail(&field, Hidden::Skip)?;
			mapped.insert(field.into(), evaluation_to_template(&value)?);
		}
	}
	mapped
		.entry("labels".to_owned())
		.or_insert_with(|| TemplateValue::Map(HashMap::new()));
	context.insert("metadata".to_owned(), TemplateValue::Map(mapped));

	Ok(context)
}

fn evaluation_to_template(value: &EvaluationValue) -> Result<TemplateValue, Error> {
	if value.is_null() {
		return Ok(TemplateValue::Nil);
	}
	if let Some(boolean) = value.as_bool() {
		return Ok(TemplateValue::Bool(boolean));
	}
	if let Some(number) = value.as_number() {
		if !(number == 0.0 && number.is_sign_negative())
			&& number.fract() == 0.0
			&& number >= -9_223_372_036_854_775_808.0
			&& number < 9_223_372_036_854_775_808.0
		{
			return Ok(TemplateValue::Number((number as i64).into()));
		}
		return Ok(TemplateValue::Number(number.into()));
	}
	if let Some(string) = value.as_str() {
		return Ok(TemplateValue::String(string.to_string()));
	}
	if let Some(array) = value.as_array() {
		let mut mapped = Vec::new();
		for value in array.into_values() {
			mapped.push(evaluation_to_template(&value?)?);
		}
		return Ok(TemplateValue::Array(mapped));
	}
	if let Some(object) = value.as_object() {
		let mut mapped = HashMap::new();
		for field in object.field_names() {
			let value = object.get_or_bail(&field, Hidden::Skip)?;
			mapped.insert(field.into(), evaluation_to_template(&value)?);
		}
		return Ok(TemplateValue::Map(mapped));
	}

	// Produce the evaluator's normal function diagnostic.
	value.manifest()?;
	unreachable!("every Jsonnet value kind handled")
}

/// Only the fields templates can reach: walking the whole manifest would be
/// wasted work on the hot path.
fn template_context(manifest: &Value) -> HashMap<String, TemplateValue> {
	let mut context = HashMap::with_capacity(3);

	for field in ["kind", "apiVersion"] {
		if let Some(value) = manifest.get(field) {
			context.insert(field.to_owned(), json_to_template(value));
		}
	}

	let metadata = manifest.get("metadata").and_then(Value::as_object);
	let mut mapped = HashMap::with_capacity(metadata.map_or(1, |metadata| metadata.len() + 1));
	if let Some(metadata) = metadata {
		for (field, value) in metadata {
			mapped.insert(field.clone(), json_to_template(value));
		}
	}
	// Templates commonly index `.metadata.labels`, which has to exist for that
	// to render rather than fail.
	mapped
		.entry("labels".to_owned())
		.or_insert_with(|| TemplateValue::Map(HashMap::new()));
	context.insert("metadata".to_owned(), TemplateValue::Map(mapped));

	context
}

fn json_to_template(value: &Value) -> TemplateValue {
	match value {
		Value::Null => TemplateValue::Nil,
		Value::Bool(boolean) => TemplateValue::Bool(*boolean),
		Value::Number(number) => number
			.as_i64()
			.map(|number| TemplateValue::Number(number.into()))
			.or_else(|| {
				number
					.as_f64()
					.map(|number| TemplateValue::Number(number.into()))
			})
			.unwrap_or(TemplateValue::Nil),
		Value::String(string) => TemplateValue::String(string.clone()),
		Value::Array(array) => TemplateValue::Array(array.iter().map(json_to_template).collect()),
		Value::Object(object) => TemplateValue::Map(
			object
				.iter()
				.map(|(field, value)| (field.clone(), json_to_template(value)))
				.collect(),
		),
	}
}

/// Sprig's `default`: the first non-empty argument.
///
/// Piped values arrive last (`{{ .value | default "fallback" }}` calls this with
/// `["fallback", .value]`), so arguments are searched back to front.
#[expect(
	clippy::unnecessary_wraps,
	reason = "the signature is gtmpl's, which allows a function to fail"
)]
fn template_default(arguments: &[TemplateValue]) -> Result<TemplateValue, FuncError> {
	for argument in arguments.iter().rev() {
		if !is_empty(argument) {
			return Ok(argument.clone());
		}
	}

	Ok(arguments.first().cloned().unwrap_or(TemplateValue::NoValue))
}

fn is_empty(value: &TemplateValue) -> bool {
	match value {
		TemplateValue::NoValue | TemplateValue::Nil => true,
		TemplateValue::Bool(boolean) => !boolean,
		TemplateValue::String(string) => string.is_empty(),
		TemplateValue::Number(number) => number.as_f64().is_some_and(|number| number == 0.0),
		TemplateValue::Array(array) => array.is_empty(),
		TemplateValue::Map(map) => map.is_empty(),
		TemplateValue::Object(object) => object.is_empty(),
		TemplateValue::Function(_) => false,
	}
}

/// Replace `old` with `new`, but only outside `{{ … }}` actions. Mirrors Tanka's
/// `replaceTmplText`.
fn replace_outside_actions(template: &str, old: &str, new: &str) -> String {
	let mut replaced = String::with_capacity(template.len());
	let mut remaining = template;

	while let Some(start) = remaining.find("{{") {
		let Some(end) = remaining[start..].find("}}").map(|end| start + end + 2) else {
			// An unterminated action is all text as far as tk is concerned.
			break;
		};

		replaced.push_str(&remaining[..start].replace(old, new));
		replaced.push_str(&remaining[start..end]);
		remaining = &remaining[end..];
	}

	replaced.push_str(&remaining.replace(old, new));
	replaced
}

/// Turn a rendered filename into the path to write, extension included.
///
/// Each segment is sanitized separately, so a rendered value can never escape
/// the output directory.
#[cfg(test)]
pub(crate) fn to_relative_path(
	rendered: &str,
	extension: &str,
	manifest: &Value,
) -> Result<std::path::PathBuf, Error> {
	let segments: Vec<Cow<'_, str>> = rendered
		.split('/')
		.map(str::trim)
		.filter(|segment| !segment.is_empty())
		.map(sanitize)
		.filter(|segment| !segment.is_empty())
		.collect();

	let Some((file, directories)) = segments.split_last() else {
		return Err(Error::EmptyFilename {
			manifest: describe_json(manifest),
		});
	};

	let mut path = std::path::PathBuf::new();
	for directory in directories {
		path.push(directory.as_ref());
	}
	path.push(format!("{file}.{extension}"));

	Ok(path)
}

#[cfg(test)]
fn describe_json(manifest: &Value) -> String {
	let kind = manifest.get("kind").and_then(Value::as_str).unwrap_or("");
	let name = manifest
		.pointer("/metadata/name")
		.and_then(Value::as_str)
		.unwrap_or("");
	let kind_name = format!("{kind}/{name}");
	if kind_name == "/" {
		let mut dumped = manifest.to_string();
		dumped.truncate(200);
		return dumped;
	}

	match manifest.get("apiVersion").and_then(Value::as_str) {
		Some(api_version) => format!("{api_version} {kind_name}"),
		None => kind_name,
	}
}

pub(crate) fn to_relative_path_evaluated(
	rendered: &str,
	extension: &str,
	manifest: &EvaluationValue,
) -> Result<std::path::PathBuf, Error> {
	let segments: Vec<Cow<'_, str>> = rendered
		.split('/')
		.map(str::trim)
		.filter(|segment| !segment.is_empty())
		.map(sanitize)
		.filter(|segment| !segment.is_empty())
		.collect();

	let Some((file, directories)) = segments.split_last() else {
		return Err(Error::EmptyFilename {
			manifest: process::describe(manifest)?,
		});
	};

	let mut path = std::path::PathBuf::new();
	for directory in directories {
		path.push(directory.as_ref());
	}
	path.push(format!("{file}.{extension}"));

	Ok(path)
}

/// Replace anything that has no business in a path component.
///
/// Borrows when there is nothing to replace, which is the common case on a path
/// that runs once per exported manifest.
pub(crate) fn sanitize(segment: &str) -> Cow<'_, str> {
	// tk writes this verbatim for cluster-scoped resources.
	if segment == "<no value>" {
		return Cow::Borrowed(segment);
	}

	if segment.chars().all(is_safe) {
		return Cow::Borrowed(segment);
	}

	Cow::Owned(
		segment
			.chars()
			.map(|character| if is_safe(character) { character } else { '-' })
			.collect(),
	)
}

#[inline]
fn is_safe(character: char) -> bool {
	character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
	use rtk_spec::canonical::{Environment, EnvironmentSpec};
	use serde_json::json;

	use super::*;

	fn environment(namespace: &str, name: &str, labels: &[(&str, &str)]) -> Environment<'static> {
		// `EnvironmentSpec` is `#[non_exhaustive]`, so it can only be built up
		// from its default.
		let mut spec = EnvironmentSpec::default();
		spec.namespace = Some(namespace.into());
		let metadata = ObjectMeta {
			name: Some(name.to_owned()),
			labels: (!labels.is_empty()).then(|| {
				labels
					.iter()
					.map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
					.collect()
			}),
			..ObjectMeta::default()
		};

		Environment::new()
			.with_metadata(metadata)
			.with_spec(spec)
			.build()
			.expect("a valid environment")
	}

	fn render(format: &str, manifest: &Value) -> String {
		let environment = environment("demo", "environments/demo", &[("tier", "test")]);
		render_for(&environment, format, manifest)
	}

	fn render_for(environment: &Environment<'static>, format: &str, manifest: &Value) -> String {
		FilenameTemplate::new(format)
			.expect("a valid template")
			.specialize(environment)
			.expect("the template specializes")
			.render(manifest)
			.expect("the template renders")
	}

	fn render_evaluated(format: &str, manifest: &Value) -> String {
		let evaluated = rtk_jsonnet::Engine::new(Default::default())
			.create_evaluator()
			.evaluate_snippet(manifest.to_string())
			.expect("valid Jsonnet")
			.into_value();
		FilenameTemplate::new(format)
			.expect("a valid template")
			.specialize(&environment(
				"demo",
				"environments/demo",
				&[("tier", "test")],
			))
			.expect("the template specializes")
			.render_evaluated(&evaluated)
			.expect("the template renders")
	}

	fn config_map() -> Value {
		json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "settings", "namespace": "demo" },
		})
	}

	#[test]
	fn renders_the_default_format() {
		assert_eq!(
			render(DEFAULT_FORMAT, &config_map()),
			"v1.ConfigMap-settings"
		);
	}

	#[test]
	fn separators_in_the_template_make_directories() {
		assert_eq!(
			render("{{.kind}}/{{.metadata.name}}", &config_map()),
			"ConfigMap/settings"
		);
	}

	#[test]
	fn separators_in_rendered_values_do_not() {
		let manifest = json!({
			"apiVersion": "apps/v1",
			"kind": "Deployment",
			"metadata": { "name": "api" },
		});
		assert_eq!(render(DEFAULT_FORMAT, &manifest), "apps-v1.Deployment-api");
		// Both at once: the template's separator survives, the value's does not.
		assert_eq!(
			render("{{.apiVersion}}/{{.kind}}", &manifest),
			"apps-v1/Deployment"
		);
	}

	#[test]
	fn substitutes_environment_references() {
		assert_eq!(
			render(
				"{{env.spec.namespace}}/{{env.metadata.name}}/{{.kind}}",
				&config_map()
			),
			// The environment's name contains a separator of its own, which is
			// a value, not a directory.
			"demo/environments-demo/ConfigMap"
		);
		assert_eq!(
			render("{{env.metadata.labels.tier}}-{{.kind}}", &config_map()),
			"test-ConfigMap"
		);
		// An absent label renders empty, as tk does.
		assert_eq!(
			render("{{env.metadata.labels.absent}}{{.kind}}", &config_map()),
			"ConfigMap"
		);
	}

	#[test]
	fn supports_sprigs_default_and_gos_or() {
		let generated = json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "generateName": "settings-" },
		});
		assert_eq!(
			render("{{or .metadata.name .metadata.generateName}}", &generated),
			"settings-"
		);
		assert_eq!(
			render(r#"{{.metadata.name | default "fallback"}}"#, &generated),
			"fallback"
		);
		assert_eq!(
			render(r#"{{.metadata.name | default "fallback"}}"#, &config_map()),
			"settings"
		);
	}

	#[test]
	fn evaluated_integer_metadata_keeps_template_integer_semantics() {
		let manifest = json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "settings", "generation": 3 },
		});
		let format = "{{if eq .metadata.generation 3}}three{{else}}other{{end}}";

		assert_eq!(render(format, &manifest), "three");
		assert_eq!(render_evaluated(format, &manifest), "three");
	}

	#[test]
	fn rejects_a_broken_template_before_anything_is_exported() {
		let error = FilenameTemplate::new("{{.kind").expect_err("an unterminated action");
		assert!(matches!(error, Error::InvalidFormat { .. }), "{error:?}");
	}

	#[test]
	fn builds_relative_paths_with_the_extension() {
		let manifest = config_map();
		assert_eq!(
			to_relative_path("dir/file.name", "yaml", &manifest).unwrap(),
			PathBuf::from("dir/file.name.yaml")
		);
		assert_eq!(
			to_relative_path("only", "json", &manifest).unwrap(),
			PathBuf::from("only.json")
		);
		// Empty segments are dropped rather than making empty directories.
		assert_eq!(
			to_relative_path("a//b", "yaml", &manifest).unwrap(),
			PathBuf::from("a/b.yaml")
		);
		assert!(matches!(
			to_relative_path("", "yaml", &manifest),
			Err(Error::EmptyFilename { .. })
		));
	}

	#[test]
	fn sanitizes_path_segments() {
		// Nothing to do: borrowed, not rebuilt.
		assert!(matches!(sanitize("plain-name_1.2:3"), Cow::Borrowed(_)));
		// tk writes this verbatim for cluster-scoped resources.
		assert!(matches!(sanitize("<no value>"), Cow::Borrowed(_)));
		assert_eq!(sanitize("with spaces"), "with-spaces");
		// Separators never reach here (segments are split on them first), but
		// nothing that could escape the output directory survives anyway.
		assert_eq!(sanitize("../escape"), "..-escape");
		assert_eq!(sanitize("unicode-ü"), "unicode-ü");
		assert_eq!(sanitize("emoji-🙂"), "emoji--");
	}

	#[test]
	fn replaces_text_outside_actions_only() {
		assert_eq!(replace_outside_actions("a/b", "/", "!"), "a!b");
		assert_eq!(
			replace_outside_actions("{{ .a/b }}/c", "/", "!"),
			"{{ .a/b }}!c"
		);
		// An unterminated action is all text, as tk treats it.
		assert_eq!(replace_outside_actions("a/{{ .b", "/", "!"), "a!{{ .b");
	}

	#[test]
	fn decides_between_branches_on_an_environments_labels() {
		// Baking a label's value into the template is a textual substitution, so
		// what it leaves behind has to be something a comparison can be written
		// against. This is the shape tk's own users write.
		let format = concat!(
			r#"{{ if not env.metadata.labels.fluxExport }}flux-disabled"#,
			r#"{{ else if eq env.metadata.labels.fluxExport "true" }}flux"#,
			r#"{{ else }}flux-disabled{{ end }}/{{.kind}}"#,
		);

		for (label, expected) in [
			(Some("true"), "flux/ConfigMap"),
			(Some("false"), "flux-disabled/ConfigMap"),
			(Some("something-else"), "flux-disabled/ConfigMap"),
			(None, "flux-disabled/ConfigMap"),
		] {
			let labels = label.map(|label| [("fluxExport", label)]);
			let environment = environment(
				"demo",
				"environments/demo",
				labels.as_ref().map_or(&[][..], |labels| &labels[..]),
			);

			assert_eq!(
				render_for(&environment, format, &config_map()),
				expected,
				"for fluxExport={label:?}"
			);
		}
	}

	#[test]
	fn a_label_an_environment_does_not_have_compares_as_empty() {
		// A missing label leaves an empty string behind rather than nothing at
		// all: the comparison has to stay a comparison instead of becoming a
		// parse error.
		let environment = environment("demo", "environments/demo", &[]);
		let service = json!({
			"apiVersion": "v1",
			"kind": "Service",
			"metadata": { "name": "my-service", "namespace": "demo" },
		});

		let format = concat!(
			r#"{{.kind}}-{{ if eq env.metadata.labels.namespaced "true" }}"#,
			r#"{{ .metadata.namespace | default "global" }}-{{ end }}{{.metadata.name}}"#,
		);

		assert_eq!(
			render_for(&environment, format, &service),
			"Service-my-service"
		);
	}
}
