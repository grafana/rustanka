//! Turning an evaluated environment into the Kubernetes manifests it exports.
//!
//! Mirrors Tanka's `pkg/process`: manifests are collected out of the
//! environment's `data`, filtered by `--target`, then have namespaces, labels
//! and resource defaults injected before being serialized.

use anyhow::Context as _;
use rtk_jsonnet::{EvaluationArray, EvaluationValue, Hidden};
use rtk_spec::canonical::Environment;
use rtk_spec::v1alpha1::EnvironmentData;
use serde::ser::{Error as _, SerializeMap as _, SerializeSeq as _};
use serde::{Serialize, Serializer};

use crate::export::Error;

/// Label Tanka tags exported resources with, when `spec.injectLabels` is set.
const ENVIRONMENT_LABEL: &str = "tanka.dev/environment";

/// Hidden field attached to processed manifests so validation can inspect the
/// value before namespace, label, and resource-default injection.
pub(crate) const ORIGINAL_MANIFEST_FIELD: &str = "$rtk.dev/originalManifest";
const PROCESSED_MANIFEST_FIELD: &str = "$rtk.dev/processedManifest";

const CLUSTER_WIDE_KINDS: &[&str] = &[
	"APIService",
	"CertificateSigningRequest",
	"ClusterRole",
	"ClusterRoleBinding",
	"ComponentStatus",
	"CSIDriver",
	"CSINode",
	"CustomResourceDefinition",
	"MutatingWebhookConfiguration",
	"Namespace",
	"Node",
	"NodeMetrics",
	"PersistentVolume",
	"PodSecurityPolicy",
	"PriorityClass",
	"RuntimeClass",
	"SelfSubjectAccessReview",
	"SelfSubjectRulesReview",
	"StorageClass",
	"SubjectAccessReview",
	"TokenReview",
	"ValidatingWebhookConfiguration",
	"VolumeAttachment",
];

/// Jsonnet helpers that apply Tanka's manifest processing without manifesting
/// values through JSON first.
pub(crate) fn processing_script<'a, D>(environment: &Environment<'a, D>, configured: bool) -> String
where
	D: EnvironmentData<'a>,
{
	let defaults = environment.spec.resource_defaults.as_ref();
	let annotations = defaults
		.map(|defaults| defaults.annotations.clone())
		.unwrap_or_default();
	let labels = defaults
		.map(|defaults| defaults.labels.clone())
		.unwrap_or_default();
	let config = serde_json::json!({
		"clusterWideKinds": CLUSTER_WIDE_KINDS,
		"defaults": {
			"annotations": annotations,
			"labels": labels,
		},
		"environmentLabel": environment
			.spec
			.inject_labels
			.then(|| environment_label(&environment.metadata)),
		"namespace": if configured { environment.spec.namespace() } else { "" },
	});

	format!(
		r#"
local rtkConfig = {config};
local rtkOriginalManifest = '{ORIGINAL_MANIFEST_FIELD}';
local rtkProcessedManifest = '{PROCESSED_MANIFEST_FIELD}';

local mergedStringMap(metadata, field, defaults, overrides) =
  local current = if std.objectHas(metadata, field) then metadata[field] else null;
  if current != null && !std.isObject(current)
  then current
  else
    local materializedCurrent =
      if current == null
      then {{}}
      else {{ [key]: current[key] for key in std.objectFields(current) }};
    defaults + materializedCurrent + overrides;

local processMetadata(metadata, manifest) =
  if metadata != null && !std.isObject(metadata)
  then metadata
  else
    local normalizedMetadata =
      if metadata == null
      then {{}}
      else {{ [field]: metadata[field] for field in std.objectFields(metadata) }};
    local annotations = mergedStringMap(normalizedMetadata, 'annotations', rtkConfig.defaults.annotations, {{}});
    local environmentLabels =
      if rtkConfig.environmentLabel == null
      then {{}}
      else {{ '{ENVIRONMENT_LABEL}': rtkConfig.environmentLabel }};
    local labels = mergedStringMap(normalizedMetadata, 'labels', rtkConfig.defaults.labels, environmentLabels);
    local namespaceAnnotations =
      if std.objectHas(normalizedMetadata, 'annotations')
         && std.isObject(normalizedMetadata.annotations)
      then normalizedMetadata.annotations
      else {{}};
    local namespacedOverride =
      if std.objectHas(namespaceAnnotations, 'tanka.dev/namespaced')
         && std.isString(namespaceAnnotations['tanka.dev/namespaced'])
      then namespaceAnnotations['tanka.dev/namespaced'] == 'true'
      else null;
    local kind = if std.isString(manifest.kind) then manifest.kind else '';
    local namespaced =
      if namespacedOverride != null
      then namespacedOverride
      else !std.member(rtkConfig.clusterWideKinds, kind);
    local hasNamespace =
      std.objectHas(normalizedMetadata, 'namespace')
      && std.isString(normalizedMetadata.namespace)
      && normalizedMetadata.namespace != '';
    local withValues = normalizedMetadata
      + {{ annotations: annotations, labels: labels }}
      + if namespaced && rtkConfig.namespace != '' && !hasNamespace
        then {{ namespace: rtkConfig.namespace }}
        else {{}};
    {{
      [field]: withValues[field]
      for field in std.objectFields(withValues)
      if !(
        (field == 'annotations' || field == 'labels')
        && (
          withValues[field] == null
          || (std.isObject(withValues[field]) && std.length(withValues[field]) == 0)
        )
      )
    }};

local processManifest(manifest) =
  local materializedManifest = {{
    [field]: manifest[field]
    for field in std.objectFields(manifest)
  }};
  {{
    [rtkOriginalManifest]:: manifest,
    [rtkProcessedManifest]:: materializedManifest {{
      metadata: processMetadata(
        if std.objectHas(manifest, 'metadata') then manifest.metadata else null,
        manifest
      ),
    }},
  }};

local processValue(value) =
  if std.isObject(value)
  then
    if std.objectHas(value, 'apiVersion') && std.objectHas(value, 'kind')
    then
      if std.isString(value.kind) && value.kind == 'List'
      then
        if std.objectHas(value, 'items') && std.isArray(value.items)
        then value {{ items: std.map(processValue, value.items) }}
        else value
      else processManifest(value)
    else std.mapWithKey(function(field, child) processValue(child), value)
  else if std.isArray(value)
  then std.map(processValue, value)
  else value;

local processEnvironments(value) =
  if std.isObject(value)
  then
    if std.objectHas(value, 'apiVersion')
       && std.objectHas(value, 'kind')
       && std.isString(value.kind)
       && value.kind == 'Environment'
    then
      if std.objectHas(value, 'data')
      then
        local materializedEnvironment = {{
          [field]: value[field]
          for field in std.objectFields(value)
        }};
        materializedEnvironment {{ data: processValue(value.data) }}
      else value
    else std.mapWithKey(function(field, child) processEnvironments(child), value)
  else if std.isArray(value)
  then std.map(processEnvironments, value)
  else value;
"#
	)
}

/// Collect the Kubernetes manifests below `data`.
///
/// Walks containers lazily, then forces each Kubernetes object before target
/// filtering to preserve Tanka's evaluation behavior. `List` objects are
/// expanded into their items, as Tanka does.
///
/// `path` is the JSON path walked so far, used for error messages.
pub(crate) fn collect_manifests(
	value: &EvaluationValue,
	path: &str,
	manifests: &mut Vec<EvaluationValue>,
) -> Result<(), Error> {
	let Some(object) = value.as_object() else {
		// Arrays are walked element by element; anything else cannot hold a
		// manifest and is skipped, matching Tanka's walk.
		let Some(array) = value.as_array() else {
			return Ok(());
		};
		for (index, element) in array.into_values().enumerate() {
			let element = element.map_err(Error::from)?;
			collect_manifests(&element, &format!("{path}[{index}]"), manifests)?;
		}
		return Ok(());
	};
	let processed = object.get(PROCESSED_MANIFEST_FIELD, Hidden::Include)?;
	let original = object.get(ORIGINAL_MANIFEST_FIELD, Hidden::Include)?;
	if !object.has(PROCESSED_MANIFEST_FIELD, Hidden::Skip)?
		&& !object.has(ORIGINAL_MANIFEST_FIELD, Hidden::Skip)?
		&& object.field_names().is_empty()
		&& let (Some(processed), Some(original)) = (processed, original)
	{
		force_manifest(&original)?;
		validate_manifest(&original, path)?;
		force_manifest(&processed)?;
		manifests.push(processed);
		return Ok(());
	}

	// Presence is asked about rather than read, so that an object which is just a
	// container does not have fields forced out of it. Hidden fields are skipped
	// throughout: this walk mirrors what would be written out, and hidden fields
	// are exactly what is not.
	let has_kind = object.has("kind", Hidden::Skip)?;

	if object.has("apiVersion", Hidden::Skip)? && has_kind {
		// A `kind` that is not a string means this is not a manifest Tanka
		// recognizes; it is exported as it stands.
		match object
			.get("kind", Hidden::Skip)?
			.and_then(|kind| kind.as_str())
			.as_deref()
		{
			// Lists are exported as their items, one file each.
			Some("List") => {
				let items = object
					.get("items", Hidden::Skip)?
					.and_then(|items| items.as_array());
				for (index, item) in items
					.into_iter()
					.flat_map(EvaluationArray::into_values)
					.enumerate()
				{
					let item = item.map_err(Error::from)?;
					collect_manifests(&item, &format!("{path}.items[{index}]"), manifests)?;
				}
			}
			_ => {
				force_manifest(value)?;
				validate_manifest(value, path)?;
				manifests.push(value.clone());
			}
		}

		return Ok(());
	}

	// An object that looks like a Kubernetes manifest but has no apiVersion is
	// a mistake rather than a container to walk into, and Tanka says so.
	if has_kind && object.has("metadata", Hidden::Skip)? {
		return Err(Error::MissingApiVersion { path: path.into() });
	}

	for (field, value) in object.into_fields() {
		let value = value.map_err(Error::from)?;
		collect_manifests(&value, &format!("{path}.{field}"), manifests)?;
	}

	Ok(())
}

fn force_manifest(value: &EvaluationValue) -> Result<(), Error> {
	if let Some(array) = value.as_array() {
		for value in array.into_values() {
			force_manifest(&value?)?;
		}
		return Ok(());
	}
	if let Some(object) = value.as_object() {
		object.run_assertions()?;
		for field in object.field_names() {
			force_manifest(&object.get_or_bail(&field, Hidden::Skip)?)?;
		}
		return Ok(());
	}
	if value.is_null()
		|| value.as_bool().is_some()
		|| value.as_number().is_some()
		|| value.as_str().is_some()
	{
		return Ok(());
	}

	// Produce the evaluator's normal function diagnostic.
	value.manifest()?;
	unreachable!("every Jsonnet value kind handled")
}

fn validate_manifest(manifest: &EvaluationValue, path: &str) -> Result<(), Error> {
	let metadata = match manifest.as_object() {
		Some(manifest) => manifest
			.get("metadata", Hidden::Skip)?
			.and_then(|metadata| metadata.as_object()),
		None => None,
	};
	let mut problems = Vec::new();
	if metadata.is_none() {
		problems.push("metadata: missing or not an object");
	}
	let has_name = match metadata {
		Some(metadata) => {
			metadata
				.get("name", Hidden::Skip)?
				.is_some_and(|name| name.as_str().is_some())
				|| metadata
					.get("generateName", Hidden::Skip)?
					.is_some_and(|name| name.as_str().is_some())
		}
		None => false,
	};
	if !has_name {
		problems.push("metadata.name: missing or not of string type");
	}

	if problems.is_empty() {
		Ok(())
	} else {
		Err(Error::InvalidManifest {
			path: if path.is_empty() {
				".".into()
			} else {
				path.into()
			},
			reason: problems.join("; "),
		})
	}
}

/// A compiled `-t/--target` expression.
///
/// Mirrors Tanka's `pkg/process/filter.go`: patterns are anchored with `^…$`,
/// case-insensitive, and a leading `!` inverts the match.
#[derive(Clone, Debug)]
pub(crate) struct TargetMatcher {
	regex: regex::Regex,
	negate: bool,
}

impl TargetMatcher {
	/// Compile raw `-t` arguments. Mirrors Tanka's `process.StrExps`.
	pub(crate) fn compile<I, S>(patterns: I) -> Result<Vec<TargetMatcher>, Error>
	where
		I: IntoIterator<Item = S>,
		S: AsRef<str>,
	{
		patterns
			.into_iter()
			.map(|pattern| {
				let pattern = pattern.as_ref();
				let (negate, body) = match pattern.strip_prefix('!') {
					Some(rest) => (true, rest),
					None => (false, pattern),
				};
				let regex = regex::RegexBuilder::new(&format!("^{body}$"))
					.case_insensitive(true)
					.build()
					.map_err(|source| Error::InvalidTarget {
						target: pattern.into(),
						source,
					})?;
				Ok(TargetMatcher { regex, negate })
			})
			.collect()
	}
}

/// Whether a manifest survives a set of `--target` matchers.
///
/// Mirrors Tanka's `process.Filter`: keep it if at least one matcher matches and
/// no negative matcher does. A negative matcher always satisfies the "matches at
/// least one" gate (Tanka's `NegMatcher.MatchString` is unconditionally true), so
/// a query of only `!…` patterns keeps everything but the exclusions.
pub(crate) fn keep_target(
	manifest: &EvaluationValue,
	matchers: &[TargetMatcher],
) -> Result<bool, Error> {
	if matchers.is_empty() {
		return Ok(true);
	}

	let kind_name = kind_name(manifest)?;
	let matched = matchers
		.iter()
		.any(|matcher| matcher.negate || matcher.regex.is_match(&kind_name));
	let excluded = matchers
		.iter()
		.any(|matcher| matcher.negate && matcher.regex.is_match(&kind_name));

	Ok(matched && !excluded)
}

/// Identify a manifest in a diagnostic, without dumping the whole thing.
pub(crate) fn describe(manifest: &EvaluationValue) -> Result<String, Error> {
	let kind_name = kind_name(manifest)?;
	if kind_name == "/" {
		// Nothing identifying at all: a truncated dump beats saying nothing.
		let mut dumped = manifest.manifest()?;
		dumped.truncate(200);
		return Ok(dumped);
	}

	let api_version = match manifest.as_object() {
		Some(manifest) => manifest
			.get("apiVersion", Hidden::Skip)?
			.and_then(|api_version| api_version.as_str()),
		None => None,
	};
	Ok(match api_version {
		Some(api_version) => format!("{api_version} {kind_name}"),
		None => kind_name,
	})
}

/// `kind/name` for matcher input. Missing fields become empty strings, matching
/// Tanka's behavior on unidentified manifests.
fn kind_name(manifest: &EvaluationValue) -> Result<String, Error> {
	let Some(manifest) = manifest.as_object() else {
		return Ok("/".into());
	};
	let kind = manifest
		.get("kind", Hidden::Skip)?
		.and_then(|kind| kind.as_str())
		.map_or_else(String::new, |kind| kind.to_string());
	let name = match manifest.get("metadata", Hidden::Skip)? {
		Some(metadata) => match metadata.as_object() {
			Some(metadata) => metadata
				.get("name", Hidden::Skip)?
				.and_then(|name| name.as_str())
				.map_or_else(String::new, |name| name.to_string()),
			None => String::new(),
		},
		None => String::new(),
	};
	Ok(format!("{kind}/{name}"))
}

/// The `tanka.dev/environment` label value: Tanka's `NameLabel()`, the first 48
/// characters of the SHA256 of `<name>:<namespace>`.
pub(crate) fn environment_label(
	metadata: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
) -> String {
	use std::fmt::Write as _;

	use sha2::{Digest, Sha256};

	let mut hasher = Sha256::new();
	hasher.update(metadata.name.as_deref().unwrap_or_default().as_bytes());
	hasher.update(b":");
	hasher.update(metadata.namespace.as_deref().unwrap_or_default().as_bytes());

	let digest = hasher.finalize();
	let mut label = String::with_capacity(48);
	for byte in digest {
		if label.len() >= 48 {
			break;
		}
		let _ = write!(&mut label, "{byte:02x}");
	}
	label.truncate(48);
	label
}

struct ExportValue<'a>(&'a EvaluationValue);

impl Serialize for ExportValue<'_> {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		let value = self.0;
		if value.is_null() {
			return serializer.serialize_none();
		}
		if let Some(boolean) = value.as_bool() {
			return serializer.serialize_bool(boolean);
		}
		if let Some(number) = value.as_number() {
			// Jsonnet numbers are float64s, but canonical JSON spells whole values
			// as integers when they fit. Preserve that distinction so serde-saphyr
			// applies the same go-yaml formatting as the old JSON round-trip.
			if number == 0.0 && number.is_sign_negative() {
				return serde_saphyr::RawScalar("-0.0").serialize(serializer);
			}
			if !number.is_sign_negative()
				&& number.fract() == 0.0
				&& number < 18_446_744_073_709_551_616.0
			{
				return serializer.serialize_u64(number as u64);
			}
			if number.fract() == 0.0
				&& number >= -9_223_372_036_854_775_808.0
				&& number < 9_223_372_036_854_775_808.0
			{
				return serializer.serialize_i64(number as i64);
			}
			return serializer.serialize_f64(number);
		}
		if let Some(string) = value.as_str() {
			return serializer.serialize_str(&string);
		}
		if let Some(array) = value.as_array() {
			let mut sequence = serializer.serialize_seq(None)?;
			for value in array.into_values() {
				let value = value.map_err(S::Error::custom)?;
				sequence.serialize_element(&ExportValue(&value))?;
			}
			return sequence.end();
		}
		if let Some(object) = value.as_object() {
			let mut fields = object.field_names();
			fields.sort_by(|left, right| saphyr::compare_string_keys(left, right));
			let mut map = serializer.serialize_map(Some(fields.len()))?;
			for field in fields {
				let value = object
					.get_or_bail(&field, Hidden::Skip)
					.map_err(S::Error::custom)?;
				map.serialize_entry(&field, &ExportValue(&value))?;
			}
			return map.end();
		}

		Err(S::Error::custom("tried to manifest function"))
	}
}

/// Serialize a manifest as tk does: go-yaml v2 formatting, keys sorted like
/// go-yaml v3 sorts them.
pub(crate) fn serialize(manifest: &EvaluationValue) -> Result<String, Error> {
	let options = serde_saphyr::SerializerOptions {
		indent_step: 2,
		indent_array: Some(0),
		prefer_block_scalars: true,
		empty_map_as_braces: true,
		empty_array_as_brackets: true,
		line_width: Some(80),
		// 1 million, and small floats like 0.00001, become exponents.
		scientific_notation_threshold: Some(1_000_000),
		scientific_notation_small_threshold: Some(0.0001),
		// `y`, `n`, `yes`, `no`, `12`, `12.5` and friends stay quoted, as
		// go-yaml v3 quotes them.
		quote_ambiguous_keys: true,
		quote_numeric_strings: true,
		..Default::default()
	};

	let mut serialized = String::new();
	serde_saphyr::to_fmt_writer_with_options(&mut serialized, &ExportValue(manifest), options)
		.context("serializing manifest")
		.map_err(Error::Serialize)?;
	Ok(serialized)
}

/// Materialize a final processed manifest for consumers that need an owned,
/// thread-safe JSON value.
pub(crate) fn materialize(manifest: &EvaluationValue) -> Result<serde_json::Value, Error> {
	if manifest.is_null() {
		return Ok(serde_json::Value::Null);
	}
	if let Some(boolean) = manifest.as_bool() {
		return Ok(boolean.into());
	}
	if let Some(number) = manifest.as_number() {
		let number = if !number.is_sign_negative()
			&& number.fract() == 0.0
			&& number < 18_446_744_073_709_551_616.0
		{
			serde_json::Number::from(number as u64)
		} else if number.fract() == 0.0
			&& number >= -9_223_372_036_854_775_808.0
			&& number < 9_223_372_036_854_775_808.0
		{
			serde_json::Number::from(number as i64)
		} else {
			serde_json::Number::from_f64(number)
				.ok_or_else(|| Error::Serialize(anyhow::anyhow!("non-finite Jsonnet number")))?
		};
		return Ok(serde_json::Value::Number(number));
	}
	if let Some(string) = manifest.as_str() {
		return Ok(serde_json::Value::String(string.to_string()));
	}
	if let Some(array) = manifest.as_array() {
		let mut values = Vec::new();
		for value in array.into_values() {
			values.push(materialize(&value?)?);
		}
		return Ok(serde_json::Value::Array(values));
	}
	if let Some(object) = manifest.as_object() {
		let fields = object.field_names();
		let mut values = serde_json::Map::with_capacity(fields.len());
		for field in fields {
			let value = object.get_or_bail(&field, Hidden::Skip)?;
			values.insert(field.into(), materialize(&value)?);
		}
		return Ok(serde_json::Value::Object(values));
	}

	manifest.manifest()?;
	unreachable!("every Jsonnet value kind handled")
}

#[cfg(test)]
mod tests {
	use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
	use rtk_spec::canonical::{Environment, EnvironmentSpec, ResourceDefaults};
	use serde_json::Value;
	use serde_json::json;

	use super::*;

	fn environment(build: impl FnOnce(&mut EnvironmentSpec)) -> Environment<'static> {
		let mut spec = EnvironmentSpec::default();
		build(&mut spec);

		Environment::new()
			.with_metadata(ObjectMeta {
				name: Some("environments/demo".to_owned()),
				namespace: Some("environments/demo/main.jsonnet".to_owned()),
				..ObjectMeta::default()
			})
			.with_spec(spec)
			.build()
			.expect("a valid environment")
	}

	fn evaluated(value: &Value) -> EvaluationValue {
		rtk_jsonnet::Engine::new(Default::default())
			.create_evaluator()
			.evaluate_snippet(value.to_string())
			.expect("valid Jsonnet")
			.into_value()
	}

	fn processed(manifest: Value, environment: &Environment<'static>) -> Value {
		let script = format!(
			"local main = {manifest};\n{}\nprocessValue(main)",
			processing_script(environment, true)
		);
		let value = rtk_jsonnet::Engine::new(Default::default())
			.create_evaluator()
			.evaluate_snippet(script)
			.expect("valid processing Jsonnet")
			.into_value();
		let processed = value
			.as_object()
			.expect("a processing wrapper")
			.get_or_bail(PROCESSED_MANIFEST_FIELD, Hidden::Include)
			.expect("the processed manifest");
		serde_json::from_str(&processed.manifest().expect("manifestable")).expect("valid JSON")
	}

	#[test]
	fn injects_the_environment_namespace() {
		let environment = environment(|spec| spec.namespace = Some("demo".into()));

		let manifest = processed(
			json!({ "apiVersion": "v1", "kind": "ConfigMap", "metadata": { "name": "a" } }),
			&environment,
		);
		assert_eq!(manifest["metadata"]["namespace"], "demo");

		// A namespace the manifest sets itself wins.
		let manifest = processed(
			json!({
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "a", "namespace": "other" },
			}),
			&environment,
		);
		assert_eq!(manifest["metadata"]["namespace"], "other");

		// An empty one does not count as set.
		let manifest = processed(
			json!({
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "a", "namespace": "" },
			}),
			&environment,
		);
		assert_eq!(manifest["metadata"]["namespace"], "demo");
	}

	#[test]
	fn defaults_the_namespace_when_the_spec_has_none() {
		let manifest = processed(
			json!({ "apiVersion": "v1", "kind": "ConfigMap", "metadata": { "name": "a" } }),
			&environment(|_| {}),
		);
		assert_eq!(manifest["metadata"]["namespace"], "default");
	}

	#[test]
	fn leaves_cluster_wide_kinds_unnamespaced() {
		let environment = environment(|spec| spec.namespace = Some("demo".into()));

		for kind in ["Namespace", "ClusterRole", "CustomResourceDefinition"] {
			let manifest = processed(
				json!({ "apiVersion": "v1", "kind": kind, "metadata": { "name": "a" } }),
				&environment,
			);
			assert!(
				manifest["metadata"].get("namespace").is_none(),
				"{kind} should not be namespaced"
			);
		}

		// The annotation overrides both ways.
		let manifest = processed(
			json!({
				"apiVersion": "v1",
				"kind": "Namespace",
				"metadata": { "name": "a", "annotations": { "tanka.dev/namespaced": "true" } },
			}),
			&environment,
		);
		assert_eq!(manifest["metadata"]["namespace"], "demo");

		let manifest = processed(
			json!({
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "a", "annotations": { "tanka.dev/namespaced": "false" } },
			}),
			&environment,
		);
		assert!(manifest["metadata"].get("namespace").is_none());
	}

	#[test]
	fn injects_the_environment_label_only_when_asked() {
		let manifest = processed(
			json!({ "apiVersion": "v1", "kind": "ConfigMap", "metadata": { "name": "a" } }),
			&environment(|_| {}),
		);
		assert!(manifest["metadata"].get("labels").is_none());

		let manifest = processed(
			json!({ "apiVersion": "v1", "kind": "ConfigMap", "metadata": { "name": "a" } }),
			&environment(|spec| spec.inject_labels = true),
		);
		let label = manifest["metadata"]["labels"][ENVIRONMENT_LABEL]
			.as_str()
			.expect("the environment label");
		// The first 48 characters of sha256("<name>:<namespace>").
		assert_eq!(label.len(), 48);
		assert_eq!(
			label,
			&environment_label(&ObjectMeta {
				name: Some("environments/demo".to_owned()),
				namespace: Some("environments/demo/main.jsonnet".to_owned()),
				..ObjectMeta::default()
			})
		);
	}

	#[test]
	fn injects_resource_defaults_without_overwriting() {
		let environment = environment(|spec| {
			spec.resource_defaults = Some(ResourceDefaults {
				annotations: [
					("owner".into(), "platform".into()),
					("tanka.dev/namespaced".into(), "false".into()),
				]
				.into_iter()
				.collect(),
				labels: [
					("managed-by".into(), "rtk".into()),
					("tier".into(), "default".into()),
				]
				.into_iter()
				.collect(),
			});
		});

		let manifest = processed(
			json!({
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "a", "labels": { "tier": "mine" } },
			}),
			&environment,
		);
		assert_eq!(manifest["metadata"]["annotations"]["owner"], "platform");
		assert_eq!(
			manifest["metadata"]["namespace"], "default",
			"default annotations are applied after namespace selection"
		);
		assert_eq!(manifest["metadata"]["labels"]["managed-by"], "rtk");
		assert_eq!(
			manifest["metadata"]["labels"]["tier"], "mine",
			"a label the manifest sets itself should survive"
		);
	}

	#[test]
	fn processing_does_not_hide_reserved_user_fields() {
		let mut input = json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "a" },
		});
		input[ORIGINAL_MANIFEST_FIELD] = json!("original user value");
		input[PROCESSED_MANIFEST_FIELD] = json!("processed user value");
		let manifest = processed(input, &environment(|_| {}));

		assert_eq!(manifest[ORIGINAL_MANIFEST_FIELD], "original user value");
		assert_eq!(manifest[PROCESSED_MANIFEST_FIELD], "processed user value");
	}

	#[test]
	fn processing_preserves_original_self_semantics() {
		let script = format!(
			r#"
local main = {{
  apiVersion: 'v1',
  kind: 'ConfigMap',
  metadata: {{ name: 'a' }},
  sawInjectedNamespace:
    if std.objectHas(self.metadata, 'namespace') then true else false,
}};
{}
processValue(main)[rtkProcessedManifest]
"#,
			processing_script(&environment(|_| {}), true)
		);
		let value = rtk_jsonnet::Engine::new(Default::default())
			.create_evaluator()
			.evaluate_snippet(script)
			.expect("valid processing Jsonnet")
			.into_value();
		let manifest: Value =
			serde_json::from_str(&value.manifest().expect("manifestable")).expect("valid JSON");

		assert_eq!(manifest["sawInjectedNamespace"], false);
		assert_eq!(manifest["metadata"]["namespace"], "default");
	}

	#[test]
	fn strips_empty_metadata_maps() {
		let manifest = processed(
			json!({
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "a", "labels": {}, "annotations": null },
			}),
			&environment(|_| {}),
		);
		assert!(manifest["metadata"].get("labels").is_none());
		assert!(manifest["metadata"].get("annotations").is_none());
	}

	#[test]
	fn filters_by_target() {
		let manifest = json!({ "kind": "ConfigMap", "metadata": { "name": "settings" } });

		let keep = |patterns: &[&str]| {
			keep_target(
				&evaluated(&manifest),
				&TargetMatcher::compile(patterns).expect("valid patterns"),
			)
			.expect("target fields are valid")
		};

		assert!(keep(&[]));
		// Anchored and case-insensitive, as tk's are.
		assert!(keep(&["configmap/settings"]));
		assert!(keep(&["ConfigMap/.*"]));
		assert!(!keep(&["configmap/setting"]));
		assert!(!keep(&["secret/.*"]));
		// A negative pattern excludes, and on its own keeps everything else.
		assert!(!keep(&["!configmap/.*"]));
		assert!(keep(&["!secret/.*"]));
		assert!(!keep(&["configmap/.*", "!.*/settings"]));

		assert!(TargetMatcher::compile(["["]).is_err());
	}

	#[test]
	fn sorts_keys_like_go_yaml_does() {
		let value = evaluated(&json!({
			"b": 1, "a": 2, "a10": 3, "a2": 4, "_x": 5, "A": 6,
			"nested": { "z": 1, "y": 2 },
		}));
		assert_eq!(
			serialize(&value).expect("serializable"),
			"_x: 5\nA: 6\na: 2\na2: 4\na10: 3\nb: 1\nnested:\n  \"y\": 2\n  z: 1\n"
		);
	}

	#[test]
	fn describes_manifests_for_diagnostics() {
		assert_eq!(
			describe(&evaluated(
				&json!({ "apiVersion": "v1", "kind": "ConfigMap", "metadata": { "name": "a" } })
			))
			.unwrap(),
			"v1 ConfigMap/a"
		);
		assert_eq!(
			describe(&evaluated(&json!({ "unidentified": true }))).unwrap(),
			"{\n    \"unidentified\": true\n}"
		);
	}
}
