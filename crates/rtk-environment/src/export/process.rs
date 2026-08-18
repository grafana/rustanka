//! Turning an evaluated environment into the Kubernetes manifests it exports.
//!
//! Mirrors Tanka's `pkg/process`: manifests are collected out of the
//! environment's `data`, filtered by `--target`, then have namespaces, labels
//! and resource defaults injected before being serialized.

use std::cmp::Ordering;

use anyhow::Context as _;
use rtk_jsonnet::{EvaluationArray, EvaluationValue, Hidden};
use rtk_spec::canonical::{Environment, EnvironmentSpec};
use rtk_spec::v1alpha1::EnvironmentData;
use serde_json::{Map, Value};

use crate::export::Error;

/// Label Tanka tags exported resources with, when `spec.injectLabels` is set.
const ENVIRONMENT_LABEL: &str = "tanka.dev/environment";

/// Collect the Kubernetes manifests below `data`.
///
/// Walks the evaluated value lazily, forcing only what it has to, and manifests
/// each Kubernetes object it finds through the Jsonnet implementation (rather
/// than through serde) so that numbers are formatted exactly as tk formats
/// them. `Environment` objects are unwrapped to their `data`, and `List` objects
/// are expanded into their items, both as Tanka does.
///
/// `path` is the JSON path walked so far, used for error messages.
pub(crate) fn collect_manifests(
	value: &EvaluationValue,
	path: &str,
	buffer: &mut String,
	manifests: &mut Vec<Value>,
) -> Result<(), Error> {
	let Some(object) = value.as_object() else {
		// Arrays are walked element by element; anything else cannot hold a
		// manifest and is skipped, matching Tanka's walk.
		let Some(array) = value.as_array() else {
			return Ok(());
		};
		for (index, element) in array.into_values().enumerate() {
			let element = element.map_err(Error::from)?;
			collect_manifests(&element, &format!("{path}[{index}]"), buffer, manifests)?;
		}
		return Ok(());
	};

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
			// Tanka environments are not exported themselves, only their
			// contents.
			Some("Environment") => {
				if let Some(data) = object.get("data", Hidden::Skip)? {
					collect_manifests(&data, path, buffer, manifests)?;
				}
			}
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
					collect_manifests(&item, &format!("{path}.items[{index}]"), buffer, manifests)?;
				}
			}
			_ => {
				buffer.clear();
				value.manifest_into(buffer)?;
				let manifest = serde_json::from_str(buffer)?;
				validate_manifest(&manifest, path)?;
				manifests.push(manifest);
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
		collect_manifests(&value, &format!("{path}.{field}"), buffer, manifests)?;
	}

	Ok(())
}

fn validate_manifest(manifest: &Value, path: &str) -> Result<(), Error> {
	let metadata = manifest.get("metadata").and_then(Value::as_object);
	let mut problems = Vec::new();
	if metadata.is_none() {
		problems.push("metadata: missing or not an object");
	}
	if !metadata.is_some_and(|metadata| {
		metadata.get("name").is_some_and(Value::is_string)
			|| metadata.get("generateName").is_some_and(Value::is_string)
	}) {
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
pub(crate) fn keep_target(manifest: &Value, matchers: &[TargetMatcher]) -> bool {
	if matchers.is_empty() {
		return true;
	}

	let kind_name = kind_name(manifest);
	let matched = matchers
		.iter()
		.any(|matcher| matcher.negate || matcher.regex.is_match(&kind_name));
	let excluded = matchers
		.iter()
		.any(|matcher| matcher.negate && matcher.regex.is_match(&kind_name));

	matched && !excluded
}

/// Identify a manifest in a diagnostic, without dumping the whole thing.
pub(crate) fn describe(manifest: &Value) -> String {
	let kind_name = kind_name(manifest);
	if kind_name == "/" {
		// Nothing identifying at all: a truncated dump beats saying nothing.
		let mut dumped = manifest.to_string();
		dumped.truncate(200);
		return dumped;
	}

	match manifest.get("apiVersion").and_then(Value::as_str) {
		Some(api_version) => format!("{api_version} {kind_name}"),
		None => kind_name,
	}
}

/// `kind/name` for matcher input. Missing fields become empty strings, matching
/// Tanka's behavior on unidentified manifests.
fn kind_name(manifest: &Value) -> String {
	let kind = manifest.get("kind").and_then(Value::as_str).unwrap_or("");
	let name = manifest
		.pointer("/metadata/name")
		.and_then(Value::as_str)
		.unwrap_or("");
	format!("{kind}/{name}")
}

/// Applies everything Tanka applies to an environment's manifests on their way
/// out.
///
/// Built once per environment: it borrows the spec and computes the environment
/// label up front, rather than hashing it again for every manifest. Holds nothing
/// from the evaluation, so it can be shared with the threads doing the
/// serializing.
#[derive(Debug)]
pub(crate) struct Processor<'e> {
	spec: &'e EnvironmentSpec,
	/// The `tanka.dev/environment` label, when `spec.injectLabels` asks for one.
	label: Option<String>,
}

impl<'e> Processor<'e> {
	pub(crate) fn new<'a, D>(environment: &'e Environment<'a, D>) -> Processor<'e>
	where
		D: EnvironmentData<'a>,
	{
		Processor {
			spec: &environment.spec,
			label: environment
				.spec
				.inject_labels
				.then(|| environment_label(&environment.metadata)),
		}
	}

	/// Apply everything Tanka applies to a manifest on its way out.
	pub(crate) fn process(&self, manifest: &mut Value) {
		inject_namespace(manifest, self.spec);
		if let Some(label) = self.label.as_deref() {
			inject_environment_label(manifest, label);
		}
		inject_resource_defaults(manifest, self.spec);
		strip_empty_metadata(manifest);
	}
}

/// Kinds that are not namespaced, from Tanka's `pkg/process/namespace.go`.
pub(crate) fn is_cluster_wide(kind: &str) -> bool {
	matches!(
		kind,
		"APIService"
			| "CertificateSigningRequest"
			| "ClusterRole"
			| "ClusterRoleBinding"
			| "ComponentStatus"
			| "CSIDriver"
			| "CSINode"
			| "CustomResourceDefinition"
			| "MutatingWebhookConfiguration"
			| "Namespace"
			| "Node" | "NodeMetrics"
			| "PersistentVolume"
			| "PodSecurityPolicy"
			| "PriorityClass"
			| "RuntimeClass"
			| "SelfSubjectAccessReview"
			| "SelfSubjectRulesReview"
			| "StorageClass"
			| "SubjectAccessReview"
			| "TokenReview"
			| "ValidatingWebhookConfiguration"
			| "VolumeAttachment"
	)
}

/// Whether a manifest should be given the environment's namespace, honoring the
/// `tanka.dev/namespaced` annotation override.
pub(crate) fn is_namespaced(manifest: &Value) -> bool {
	let kind = manifest.get("kind").and_then(Value::as_str).unwrap_or("");

	// `tanka.dev/namespaced`, escaped as a JSON pointer token.
	manifest
		.pointer("/metadata/annotations/tanka.dev~1namespaced")
		.and_then(Value::as_str)
		.map_or_else(|| !is_cluster_wide(kind), |namespaced| namespaced == "true")
}

/// Set `metadata.namespace` from the environment on namespaced resources that do
/// not have one. Mirrors Tanka's `pkg/process/namespace.go`.
fn inject_namespace(manifest: &mut Value, spec: &EnvironmentSpec) {
	if !is_namespaced(manifest) {
		return;
	}

	let namespace = spec.namespace();
	if namespace.is_empty() {
		return;
	}

	let Some(metadata) = metadata_mut(manifest) else {
		return;
	};

	let has_namespace = metadata
		.get("namespace")
		.and_then(Value::as_str)
		.is_some_and(|namespace| !namespace.is_empty());
	if !has_namespace {
		metadata.insert("namespace".to_owned(), Value::String(namespace.to_owned()));
	}
}

/// Tag the manifest with `tanka.dev/environment`, mirroring Tanka's
/// `pkg/process/process.go`.
fn inject_environment_label(manifest: &mut Value, label: &str) {
	let Some(metadata) = metadata_mut(manifest) else {
		return;
	};
	let Some(labels) = entry_object_mut(metadata, "labels") else {
		return;
	};
	labels.insert(
		ENVIRONMENT_LABEL.to_owned(),
		Value::String(label.to_owned()),
	);
}

/// The `tanka.dev/environment` label value: Tanka's `NameLabel()`, the first 48
/// characters of the SHA256 of `<name>:<namespace>`.
fn environment_label(
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

/// Apply `spec.resourceDefaults`, without overwriting anything the manifest
/// already sets.
fn inject_resource_defaults(manifest: &mut Value, spec: &EnvironmentSpec) {
	let Some(defaults) = spec.resource_defaults.as_ref() else {
		return;
	};
	if defaults.annotations.is_empty() && defaults.labels.is_empty() {
		return;
	}

	let Some(metadata) = metadata_mut(manifest) else {
		return;
	};

	for (field, values) in [
		("annotations", &defaults.annotations),
		("labels", &defaults.labels),
	] {
		if values.is_empty() {
			continue;
		}
		let Some(target) = entry_object_mut(metadata, field) else {
			continue;
		};
		for (key, value) in values {
			if !target.contains_key(&**key) {
				target.insert(key.to_string(), Value::String(value.to_string()));
			}
		}
	}
}

/// Drop `metadata.annotations`/`metadata.labels` when they are null or empty, as
/// Kubernetes and Tanka omit them from output.
fn strip_empty_metadata(manifest: &mut Value) {
	let Some(Value::Object(metadata)) = manifest.get_mut("metadata") else {
		return;
	};

	for field in ["annotations", "labels"] {
		let empty = match metadata.get(field) {
			Some(Value::Null) => true,
			Some(Value::Object(object)) => object.is_empty(),
			_ => false,
		};
		if empty {
			metadata.remove(field);
		}
	}
}

/// `manifest.metadata`, inserting an empty object if it is missing, as long as
/// the manifest is an object and `metadata` is not something else entirely.
fn metadata_mut(manifest: &mut Value) -> Option<&mut Map<String, Value>> {
	let manifest = manifest.as_object_mut()?;
	entry_object_mut(manifest, "metadata")
}

/// `object[field]` as a map, inserting an empty one if the field is missing or
/// null. Returns `None` if the field holds something that is not a map.
fn entry_object_mut<'m>(
	object: &'m mut Map<String, Value>,
	field: &str,
) -> Option<&'m mut Map<String, Value>> {
	let entry = object
		.entry(field)
		.or_insert_with(|| Value::Object(Map::new()));
	if entry.is_null() {
		*entry = Value::Object(Map::new());
	}
	entry.as_object_mut()
}

/// Sort every object's keys the way go-yaml v3 does, so serialization output
/// matches tk's.
pub(crate) fn sort_keys(value: Value) -> Value {
	match value {
		Value::Object(object) => {
			let mut entries: Vec<(String, Value)> = object.into_iter().collect();
			entries.sort_by(|(left, _), (right, _)| compare_keys(left, right));
			Value::Object(
				entries
					.into_iter()
					.map(|(key, value)| (key, sort_keys(value)))
					.collect(),
			)
		}
		Value::Array(array) => Value::Array(array.into_iter().map(sort_keys).collect()),
		other => other,
	}
}

/// go-yaml v3's key comparison (`sorter.go`): a natural sort where digit runs
/// compare numerically, letters sort before non-letters directly after digits,
/// and non-letters sort first otherwise.
fn compare_keys(left: &str, right: &str) -> Ordering {
	let left: Vec<char> = left.chars().collect();
	let right: Vec<char> = right.chars().collect();
	let mut digits = false;

	for index in 0..left.len().min(right.len()) {
		if left[index] == right[index] {
			digits = left[index].is_ascii_digit();
			continue;
		}

		let left_alphabetic = left[index].is_alphabetic();
		let right_alphabetic = right[index].is_alphabetic();

		if left_alphabetic && right_alphabetic {
			return left[index].cmp(&right[index]);
		}

		if left_alphabetic || right_alphabetic {
			return if digits == left_alphabetic {
				Ordering::Less
			} else {
				Ordering::Greater
			};
		}

		// Both are non-letters: compare digit runs numerically, treating a
		// leading zero as significant only when no non-zero digit precedes it.
		let mut left_number: i64 = 0;
		let mut right_number: i64 = 0;

		if left[index] == '0' || right[index] == '0' {
			let mut preceding = index;
			while preceding > 0 && left[preceding - 1].is_ascii_digit() {
				preceding -= 1;
				if left[preceding] != '0' {
					left_number = 1;
					right_number = 1;
					break;
				}
			}
		}

		let mut left_end = index;
		while left_end < left.len() && left[left_end].is_ascii_digit() {
			left_number = left_number * 10 + i64::from(left[left_end] as u32 - '0' as u32);
			left_end += 1;
		}

		let mut right_end = index;
		while right_end < right.len() && right[right_end].is_ascii_digit() {
			right_number = right_number * 10 + i64::from(right[right_end] as u32 - '0' as u32);
			right_end += 1;
		}

		if left_number != right_number {
			return left_number.cmp(&right_number);
		}
		if left_end != right_end {
			return left_end.cmp(&right_end);
		}
		return left[index].cmp(&right[index]);
	}

	left.len().cmp(&right.len())
}

/// Serialize a manifest as tk does: go-yaml v2 formatting, keys sorted like
/// go-yaml v3 sorts them.
pub(crate) fn serialize(manifest: Value) -> Result<String, Error> {
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
	serde_saphyr::to_fmt_writer_with_options(&mut serialized, &sort_keys(manifest), options)
		.context("serializing manifest")
		.map_err(Error::Serialize)?;
	Ok(serialized)
}

#[cfg(test)]
mod tests {
	use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
	use rtk_spec::canonical::{Environment, ResourceDefaults};
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

	fn processed(mut manifest: Value, environment: &Environment<'static>) -> Value {
		Processor::new(environment).process(&mut manifest);
		manifest
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
				annotations: std::iter::once(("owner".into(), "platform".into())).collect(),
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
		assert_eq!(manifest["metadata"]["labels"]["managed-by"], "rtk");
		assert_eq!(
			manifest["metadata"]["labels"]["tier"], "mine",
			"a label the manifest sets itself should survive"
		);
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
				&manifest,
				&TargetMatcher::compile(patterns).expect("valid patterns"),
			)
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
		let sorted = sort_keys(json!({
			"b": 1, "a": 2, "a10": 3, "a2": 4, "_x": 5, "A": 6,
			"nested": { "z": 1, "y": 2 },
		}));
		let keys: Vec<&str> = sorted
			.as_object()
			.expect("an object")
			.keys()
			.map(String::as_str)
			.collect();
		// Numbers compare numerically, so a2 precedes a10.
		assert_eq!(keys, vec!["_x", "A", "a", "a2", "a10", "b", "nested"]);
		let nested: Vec<&str> = sorted["nested"]
			.as_object()
			.expect("an object")
			.keys()
			.map(String::as_str)
			.collect();
		assert_eq!(nested, vec!["y", "z"]);
	}

	#[test]
	fn describes_manifests_for_diagnostics() {
		assert_eq!(
			describe(
				&json!({ "apiVersion": "v1", "kind": "ConfigMap", "metadata": { "name": "a" } })
			),
			"v1 ConfigMap/a"
		);
		assert_eq!(
			describe(&json!({ "unidentified": true })),
			r#"{"unidentified":true}"#
		);
	}
}
