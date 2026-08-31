//! Turning an evaluated environment into the Kubernetes manifests it exports.
//!
//! Mirrors Tanka's `pkg/process`: manifests are collected out of the
//! environment's `data`, filtered by `--target`, then have namespaces, labels
//! and resource defaults injected before being serialized.

use rustc_hash::FxHashSet;

use rtk_jsonnet::{EvaluationValue, Hidden};
use rtk_spec::canonical::Environment;
use rtk_spec::v1alpha1::EnvironmentData;
use serde::ser::{SerializeMap as _, SerializeSeq as _};
use serde::{Serialize, Serializer};

use crate::export::Error;

/// Label Tanka tags exported resources with, when `spec.injectLabels` is set.
const ENVIRONMENT_LABEL: &str = "tanka.dev/environment";

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

/// What an environment's spec says to do to each of its manifests.
///
/// Mirrors Tanka's `pkg/process`: a namespace, an environment label and resource
/// defaults, resolved once per environment rather than per manifest.
#[derive(Clone, Debug, Default)]
pub(crate) struct Processing {
	/// The namespace to give namespaced resources that have none. Empty for a
	/// bare Jsonnet entrypoint, which has no spec to take one from.
	namespace: String,
	environment_label: Option<String>,
	annotations: Vec<(Box<str>, Box<str>)>,
	labels: Vec<(Box<str>, Box<str>)>,
}

impl Processing {
	/// Read what to do from an environment's spec.
	///
	/// `configured` is false for a bare Jsonnet entrypoint: it has no spec, so it
	/// gets no namespace, as tk gives none.
	pub(crate) fn new<'a, D>(environment: &Environment<'a, D>, configured: bool) -> Processing
	where
		D: EnvironmentData<'a>,
	{
		let defaults = environment.spec.resource_defaults.as_ref();
		let sorted = |map: Option<&rustc_hash::FxHashMap<Box<str>, Box<str>>>| {
			let mut entries: Vec<(Box<str>, Box<str>)> = map
				.map(|map| {
					map.iter()
						.map(|(key, value)| (key.clone(), value.clone()))
						.collect()
				})
				.unwrap_or_default();
			// Insertion order decides nothing about the output, which is sorted
			// again on the way out, but it decides this struct's Debug.
			entries.sort_by(|left, right| left.0.cmp(&right.0));
			entries
		};

		Processing {
			namespace: if configured {
				environment.spec.namespace().to_owned()
			} else {
				String::new()
			},
			environment_label: environment
				.spec
				.inject_labels
				.then(|| environment_label(&environment.metadata)),
			annotations: sorted(defaults.map(|defaults| &defaults.annotations)),
			labels: sorted(defaults.map(|defaults| &defaults.labels)),
		}
	}

	/// Apply Tanka's four processing steps to one manifest, in Tanka's order.
	///
	/// The order matters: resource defaults must not overwrite an environment
	/// label that was just injected, and empty maps are stripped only once
	/// everything that might have filled them has run.
	pub(crate) fn apply(&self, manifest: &mut serde_json::Value) {
		self.inject_namespace(manifest);
		self.inject_environment_label(manifest);
		self.inject_resource_defaults(manifest);
		strip_empty_metadata_maps(manifest);
	}

	/// Give a namespaced resource the environment's namespace, unless it named
	/// one itself.
	///
	/// Whether a resource is namespaced is decided by its kind, and overridden by
	/// a `tanka.dev/namespaced` annotation — a string, as tk reads it.
	fn inject_namespace(&self, manifest: &mut serde_json::Value) {
		let Some(kind) = manifest.get("kind").map(json_kind) else {
			return;
		};

		// Created whether or not anything is put in it: tk exports a `metadata`
		// key even for a resource that declared none.
		let Some(metadata) = metadata_mut(manifest) else {
			return;
		};

		let namespaced = match metadata
			.get("annotations")
			.and_then(serde_json::Value::as_object)
			.and_then(|annotations| annotations.get("tanka.dev/namespaced"))
			.and_then(serde_json::Value::as_str)
		{
			Some(namespaced) => namespaced == "true",
			None => !CLUSTER_WIDE_KINDS.contains(&kind.as_str()),
		};
		let has_namespace = metadata
			.get("namespace")
			.and_then(serde_json::Value::as_str)
			.is_some_and(|namespace| !namespace.is_empty());

		if namespaced && !has_namespace && !self.namespace.is_empty() {
			metadata.insert(
				"namespace".to_owned(),
				serde_json::Value::String(self.namespace.clone()),
			);
		}
	}

	/// Tag the resource as belonging to this environment, when asked to.
	fn inject_environment_label(&self, manifest: &mut serde_json::Value) {
		let Some(label) = self.environment_label.as_deref() else {
			return;
		};
		let Some(metadata) = metadata_mut(manifest) else {
			return;
		};
		let Some(labels) = string_map_mut(metadata, "labels") else {
			return;
		};

		// Overwrites: the environment an export came from is not the manifest's to
		// disagree about.
		labels.insert(
			ENVIRONMENT_LABEL.to_owned(),
			serde_json::Value::String(label.to_owned()),
		);
	}

	/// Fill in the spec's default annotations and labels, without replacing any
	/// the manifest set for itself.
	fn inject_resource_defaults(&self, manifest: &mut serde_json::Value) {
		if self.annotations.is_empty() && self.labels.is_empty() {
			return;
		}
		let Some(metadata) = metadata_mut(manifest) else {
			return;
		};

		for (field, defaults) in [("annotations", &self.annotations), ("labels", &self.labels)] {
			if defaults.is_empty() {
				continue;
			}
			let Some(existing) = string_map_mut(metadata, field) else {
				continue;
			};
			for (key, value) in defaults {
				existing
					.entry(key.as_ref().to_owned())
					.or_insert_with(|| serde_json::Value::String(value.as_ref().to_owned()));
			}
		}
	}
}

/// A manifest's `kind`, as a string. A `kind` that is not one is not a kind
/// Tanka knows, and is treated as unnamed rather than as cluster-wide.
fn json_kind(kind: &serde_json::Value) -> String {
	kind.as_str().unwrap_or_default().to_owned()
}

/// A manifest's `metadata`, creating it when absent.
///
/// Absent means absent: a `metadata` that is there but is not an object belongs
/// to the manifest, and is left exactly as it is.
fn metadata_mut(
	manifest: &mut serde_json::Value,
) -> Option<&mut serde_json::Map<String, serde_json::Value>> {
	let manifest = manifest.as_object_mut()?;
	manifest
		.entry("metadata")
		.or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
	manifest.get_mut("metadata")?.as_object_mut()
}

/// A metadata field holding a map of strings, creating it when absent or null.
fn string_map_mut<'a>(
	metadata: &'a mut serde_json::Map<String, serde_json::Value>,
	field: &str,
) -> Option<&'a mut serde_json::Map<String, serde_json::Value>> {
	if !metadata
		.get(field)
		.is_some_and(serde_json::Value::is_object)
	{
		if metadata
			.get(field)
			.is_some_and(|value| !value.is_null() && !value.is_object())
		{
			// Something the manifest meant, whatever it is. Left alone.
			return None;
		}
		metadata.insert(
			field.to_owned(),
			serde_json::Value::Object(serde_json::Map::new()),
		);
	}
	metadata.get_mut(field)?.as_object_mut()
}

/// Drop `annotations` and `labels` that ended up empty, as tk does, so that a
/// manifest which asked for neither does not gain them.
fn strip_empty_metadata_maps(manifest: &mut serde_json::Value) {
	let Some(metadata) = manifest
		.get_mut("metadata")
		.and_then(serde_json::Value::as_object_mut)
	else {
		return;
	};

	for field in ["annotations", "labels"] {
		let empty = match metadata.get(field) {
			Some(serde_json::Value::Null) => true,
			Some(serde_json::Value::Object(map)) => map.is_empty(),
			// A field the manifest meant, whatever it is, or none at all.
			None | Some(_) => false,
		};
		if empty {
			metadata.remove(field);
		}
	}
}

/// Collect the Kubernetes manifests below `data`.
///
/// Mirrors Tanka's `process.Extract`/`walkJSON`. Anything carrying an
/// `apiVersion` and a `kind` is taken whole; anything else is a container to
/// walk into, and reaching a value that cannot be walked at all means the
/// Jsonnet produced something that was never a Kubernetes object. tk refuses
/// the whole export in that case, and so does this: a manifest whose `kind` was
/// misspelled would otherwise leave the export without a word.
///
/// `List` objects are expanded into their items. An `Environment` is an object
/// like any other here: tk exports a nested one rather than unwrapping it.
///
/// `path` is the JSON path walked so far, used for error messages.
pub(crate) fn collect_manifests(
	value: serde_json::Value,
	path: &str,
	manifests: &mut Vec<serde_json::Value>,
) -> Result<(), Error> {
	walk(value, path, manifests).map_err(|interrupted| match interrupted {
		// Nothing enclosed the value, so there is no object to blame — tk
		// formats its nil error as `%!s(<nil>)` here, which is an artifact
		// rather than a message worth reproducing.
		Interrupted::Primitive { path } => Error::InvalidManifest {
			path,
			reason: "not a Kubernetes object".to_owned(),
		},
		Interrupted::Failed(error) => error,
	})
}

/// A walk that ended early.
enum Interrupted {
	/// A value was reached that is neither an object nor an array.
	///
	/// Tanka's `ErrorPrimitiveReached`, which carries no reason of its own: the
	/// innermost object enclosing it supplies one, being the thing that should
	/// have been a manifest and was not. `path` is already that object's path.
	Primitive {
		path: String,
	},
	Failed(Error),
}

impl From<Error> for Interrupted {
	fn from(error: Error) -> Interrupted {
		Interrupted::Failed(error)
	}
}

fn walk(
	value: serde_json::Value,
	path: &str,
	manifests: &mut Vec<serde_json::Value>,
) -> Result<(), Interrupted> {
	match value {
		serde_json::Value::Array(items) => {
			for (index, item) in items.into_iter().enumerate() {
				walk(item, &format!("{path}[{index}]"), manifests)?;
			}
			Ok(())
		}
		serde_json::Value::Object(object) => walk_object(object, path, manifests),
		// The path of the object that should have explained this value, which is
		// this one with its last step removed, as tk's `trace.Base` does.
		_ => Err(Interrupted::Primitive {
			path: parent_path(path),
		}),
	}
}

fn walk_object(
	mut object: serde_json::Map<String, serde_json::Value>,
	path: &str,
	manifests: &mut Vec<serde_json::Value>,
) -> Result<(), Interrupted> {
	// ksonnet's private field, which tk drops before deciding anything and so
	// never exports either.
	object.remove("__ksonnet");

	let defect = match manifest_defect(&object) {
		None => {
			if object.get("kind").and_then(serde_json::Value::as_str) == Some("List") {
				if let Some(serde_json::Value::Array(items)) = object.remove("items") {
					for (index, item) in items.into_iter().enumerate() {
						walk(item, &format!("{path}.items[{index}]"), manifests)?;
					}
				}
			} else {
				let manifest = serde_json::Value::Object(object);
				validate_manifest(&manifest, path)?;
				manifests.push(manifest);
			}
			return Ok(());
		}
		Some(defect) => defect,
	};

	// Sorted, because which failure is reported depends on it and tk sorts.
	let mut fields = object.into_iter().collect::<Vec<_>>();
	fields.sort_by(|(one, _), (other, _)| one.cmp(other));

	for (field, value) in fields {
		// A field left unset by a false condition in Jsonnet, which tk skips
		// rather than treating as a value that cannot be walked.
		if value.is_null() {
			continue;
		}
		walk(value, &format!("{path}.{field}"), manifests).map_err(|interrupted| {
			match interrupted {
				// The innermost object to see this is the one that has to
				// explain it, so it becomes an ordinary failure here and outer
				// frames leave it alone.
				Interrupted::Primitive { path } => Error::InvalidManifest {
					path,
					reason: defect.clone(),
				}
				.into(),
				failed @ Interrupted::Failed(_) => failed,
			}
		})?;
	}

	Ok(())
}

/// A JSON path with its last step removed. Tanka's `trace.Base`.
fn parent_path(path: &str) -> String {
	let base = match path.rfind(['.', '[']) {
		Some(0) | None => "",
		Some(index) => &path[..index],
	};
	if base.is_empty() {
		".".to_owned()
	} else {
		base.to_owned()
	}
}

/// Why an object is not a Kubernetes manifest, or `None` if it is one.
///
/// Tanka's `isKubernetesManifest`: `apiVersion` and `kind` must both be present,
/// be strings, and be non-empty. Checking only that they exist let rtk treat
/// `kind: 42` as a manifest.
fn manifest_defect(object: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
	for attribute in ["apiVersion", "kind"] {
		let defect = match object.get(attribute) {
			None | Some(serde_json::Value::Null) => format!("missing attribute {attribute:?}"),
			Some(serde_json::Value::String(text)) if text.is_empty() => {
				format!("attribute {attribute:?} is empty")
			}
			Some(serde_json::Value::String(_)) => continue,
			Some(other) => format!(
				"attribute {attribute:?} is not a string, it is a {}",
				go_type_name(other)
			),
		};
		return Some(defect);
	}
	None
}

/// Go's name for the dynamic type behind a JSON value, which tk reports.
fn go_type_name(value: &serde_json::Value) -> &'static str {
	match value {
		serde_json::Value::Null => "<nil>",
		serde_json::Value::Bool(_) => "bool",
		serde_json::Value::Number(_) => "float64",
		serde_json::Value::String(_) => "string",
		serde_json::Value::Array(_) => "[]interface {}",
		serde_json::Value::Object(_) => "map[string]interface {}",
	}
}

fn validate_manifest(manifest: &serde_json::Value, path: &str) -> Result<(), Error> {
	let metadata = manifest
		.get("metadata")
		.and_then(serde_json::Value::as_object);
	let mut problems = Vec::new();
	if metadata.is_none() {
		problems.push("metadata: missing or not an object");
	}
	let has_name = metadata.is_some_and(|metadata| {
		metadata
			.get("name")
			.is_some_and(serde_json::Value::is_string)
			|| metadata
				.get("generateName")
				.is_some_and(serde_json::Value::is_string)
	});
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

/// Compiled `-t/--target` expressions.
///
/// Public because the same expressions decide two different things: which of an
/// environment's own manifests to act on, and which of the cluster's resources
/// count as orphans when pruning. The second lives with the Kubernetes client,
/// so it needs to ask this rather than compile its own — Tanka has one
/// `process.Filter`, and so does rtk.
#[derive(Clone, Debug)]
pub struct Targets(Vec<TargetMatcher>);

impl Targets {
	/// Compile raw `-t` arguments. Mirrors Tanka's `process.StrExps`.
	pub fn compile<I, S>(patterns: I) -> Result<Targets, Error>
	where
		I: IntoIterator<Item = S>,
		S: AsRef<str>,
	{
		patterns
			.into_iter()
			.map(TargetMatcher::compile)
			.collect::<Result<Vec<_>, _>>()
			.map(Targets)
	}

	/// Whether a manifest survives these matchers.
	pub(crate) fn keeps(&self, manifest: &serde_json::Value) -> bool {
		self.keeps_kind_name(&kind_name(manifest))
	}

	/// Whether a `kind/name` survives these matchers.
	///
	/// Mirrors Tanka's `process.Filter`: keep it if at least one matcher matches
	/// and no negative matcher does. A negative matcher always satisfies the
	/// "matches at least one" gate (Tanka's `NegMatcher.MatchString` is
	/// unconditionally true), so a query of only `!…` patterns keeps everything
	/// but the exclusions.
	///
	/// Takes the name rather than a manifest, because a resource discovered in
	/// the cluster is not one: pruning asks about things this environment never
	/// produced.
	#[must_use]
	pub fn keeps_kind_name(&self, kind_name: &str) -> bool {
		if self.0.is_empty() {
			return true;
		}

		let matched = self
			.0
			.iter()
			.any(|matcher| matcher.negate || matcher.regex.is_match(kind_name));
		let excluded = self
			.0
			.iter()
			.any(|matcher| matcher.negate && matcher.regex.is_match(kind_name));

		matched && !excluded
	}

	/// The kinds these targets can possibly keep, lowercased, if that is knowable.
	///
	/// A caller listing the cluster can use this to leave whole resource types
	/// unasked for. [`None`] means every kind has to be considered: either
	/// nothing positive was named, or a kind was named as a pattern rather than
	/// as a literal, and narrowing on a guess would hide resources that should
	/// have been pruned.
	#[must_use]
	pub fn kind_hints(&self) -> Option<FxHashSet<String>> {
		/// Anything that makes a kind a pattern rather than a name.
		const META: [char; 13] = [
			'.', '+', '*', '?', '(', ')', '[', ']', '{', '}', '|', '\\', '$',
		];

		let mut kinds = FxHashSet::default();
		let mut named_one = false;

		for matcher in self.0.iter().filter(|matcher| !matcher.negate) {
			named_one = true;
			// Compiled as `^…$`, so the anchors come back off to read the kind.
			let pattern = matcher
				.regex
				.as_str()
				.strip_prefix('^')
				.and_then(|pattern| pattern.strip_suffix('$'))?;
			let kind = pattern.split_once('/').map_or(pattern, |(kind, _)| kind);

			if kind.is_empty() || kind.contains(META) || kind.contains('^') {
				return None;
			}
			kinds.insert(kind.to_lowercase());
		}

		named_one.then_some(kinds)
	}
}

/// One compiled target expression. Patterns are anchored with `^…$`,
/// case-insensitive, and a leading `!` inverts the match.
#[derive(Clone, Debug)]
struct TargetMatcher {
	regex: regex::Regex,
	negate: bool,
}

impl TargetMatcher {
	fn compile(pattern: impl AsRef<str>) -> Result<TargetMatcher, Error> {
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
	}
}

/// Identify a manifest in a diagnostic, without dumping the whole thing.
pub(crate) fn describe(manifest: &serde_json::Value) -> String {
	let kind_name = kind_name(manifest);
	if kind_name == "/" {
		// Nothing identifying at all: a truncated dump beats saying nothing.
		let mut dumped = manifest.to_string();
		dumped.truncate(200);
		return dumped;
	}

	match manifest
		.get("apiVersion")
		.and_then(serde_json::Value::as_str)
	{
		Some(api_version) => format!("{api_version} {kind_name}"),
		None => kind_name,
	}
}

/// `kind/name` for matcher input. Missing fields become empty strings, matching
/// Tanka's behavior on unidentified manifests.
fn kind_name(manifest: &serde_json::Value) -> String {
	if !manifest.is_object() {
		return "/".into();
	}
	let kind = manifest
		.get("kind")
		.and_then(serde_json::Value::as_str)
		.unwrap_or_default();
	let name = manifest
		.pointer("/metadata/name")
		.and_then(serde_json::Value::as_str)
		.unwrap_or_default();
	format!("{kind}/{name}")
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

/// Serializes a manifest the way tk writes it out: keys in go-yaml's order, and
/// negative zero kept as a float.
struct ExportValue<'a>(&'a serde_json::Value);

impl Serialize for ExportValue<'_> {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		match self.0 {
			serde_json::Value::Array(values) => {
				let mut sequence = serializer.serialize_seq(Some(values.len()))?;
				for value in values {
					sequence.serialize_element(&ExportValue(value))?;
				}
				sequence.end()
			}
			serde_json::Value::Object(fields) => {
				// Sorted here rather than by rebuilding the tree: go-yaml orders keys
				// as it writes them, and so does this.
				let mut keys: Vec<&String> = fields.keys().collect();
				keys.sort_by(|left, right| saphyr::compare_string_keys(left, right));
				let mut map = serializer.serialize_map(Some(keys.len()))?;
				for key in keys {
					let value = &fields[key];
					map.serialize_entry(key, &ExportValue(value))?;
				}
				map.end()
			}
			value => value.serialize(serializer),
		}
	}
}

/// Serialize a manifest as tk does: go-yaml v2 formatting, keys sorted like
/// go-yaml v3 sorts them.
pub(crate) fn serialize(manifest: &serde_json::Value) -> Result<String, Error> {
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
		// A negative zero has to stay a float, and go-yaml writes one as `-0`.
		go_style_negative_zero: true,
		..Default::default()
	};

	let mut serialized = String::new();
	serde_saphyr::to_fmt_writer_with_options(&mut serialized, &ExportValue(manifest), options)
		.map_err(|source| Error::Serialize(source.into()))?;
	Ok(serialized)
}

/// Materialize an evaluated value as owned JSON.
///
/// This is the one place the evaluation is read: everything downstream works on
/// the result, which is owned and can cross threads. Whole numbers are spelled
/// as integers where they fit, as canonical JSON does, so that the YAML
/// formatting applied later is the same formatting tk gets from manifesting
/// through JSON text.
pub(crate) fn materialize(value: &EvaluationValue) -> Result<serde_json::Value, Error> {
	if value.is_null() {
		return Ok(serde_json::Value::Null);
	}
	if let Some(boolean) = value.as_bool() {
		return Ok(boolean.into());
	}
	if let Some(number) = value.as_number() {
		return Ok(serde_json::Value::Number(materialize_number(number)?));
	}
	if let Some(string) = value.as_str() {
		return Ok(serde_json::Value::String(string.to_string()));
	}
	if let Some(array) = value.as_array() {
		let mut values = Vec::new();
		for value in array.into_values() {
			values.push(materialize(&value?)?);
		}
		return Ok(serde_json::Value::Array(values));
	}
	if let Some(object) = value.as_object() {
		// An object's assertions hold whether or not anything reads the fields
		// they guard, so they are run rather than waited for.
		object.run_assertions()?;

		let fields = object.field_names(Hidden::Skip);
		let mut values = serde_json::Map::with_capacity(fields.len());
		for field in fields {
			let value = object.get_or_bail(&field, Hidden::Skip)?;
			values.insert(field.to_string(), materialize(&value)?);
		}
		return Ok(serde_json::Value::Object(values));
	}

	// Produce the evaluator's normal function diagnostic.
	value.manifest()?;
	unreachable!("every Jsonnet value kind handled")
}

/// Spell a Jsonnet number the way canonical JSON does.
///
/// Every Jsonnet number is a float64. Whole ones are written as integers where
/// they fit, which is what decides whether the YAML formatting applied later
/// spells them with a decimal point or an exponent.
#[expect(
	clippy::cast_possible_truncation,
	clippy::cast_sign_loss,
	reason = "each cast is guarded by the range check above it"
)]
fn materialize_number(number: f64) -> Result<serde_json::Number, Error> {
	// Negative zero is a float and has to stay one: spelled as an integer it
	// would lose its sign, and tk writes it out as `-0.0`.
	if number == 0.0 && number.is_sign_negative() {
		return serde_json::Number::from_f64(number)
			.ok_or_else(|| Error::Serialize(anyhow::anyhow!("non-finite Jsonnet number")));
	}
	if !number.is_sign_negative() && number.fract() == 0.0 && number < 18_446_744_073_709_551_616.0
	{
		return Ok(serde_json::Number::from(number as u64));
	}
	if number.fract() == 0.0
		&& (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&number)
	{
		return Ok(serde_json::Number::from(number as i64));
	}

	serde_json::Number::from_f64(number)
		.ok_or_else(|| Error::Serialize(anyhow::anyhow!("non-finite Jsonnet number")))
}

#[cfg(test)]
mod tests {
	/// The same expressions decide an environment's own manifests and, when
	/// pruning, which of the cluster's resources count as orphans — so a
	/// `kind/name` has to be answerable without a manifest to read it from.
	#[test]
	fn targets_match_a_kind_and_name_without_a_manifest() {
		let targets = Targets::compile(["Deployment/.*", "service/frontend"]).expect("valid");

		assert!(targets.keeps_kind_name("Deployment/anything"));
		assert!(targets.keeps_kind_name("Service/frontend"));
		assert!(!targets.keeps_kind_name("Service/other"));
		assert!(!targets.keeps_kind_name("ConfigMap/settings"));

		// And it agrees with the manifest-shaped question, being the same rule.
		assert!(targets.keeps(&json!({
			"apiVersion": "apps/v1",
			"kind": "Deployment",
			"metadata": { "name": "anything" },
		})));
	}

	/// Naming no targets keeps everything, as tk's empty filter does.
	#[test]
	fn no_targets_keep_everything() {
		let targets = Targets::compile(Vec::<String>::new()).expect("valid");
		assert!(targets.keeps_kind_name("ConfigMap/anything"));
		assert_eq!(targets.kind_hints(), None, "nothing to narrow on");
	}

	/// A caller listing the cluster can skip whole resource types, but only when
	/// every positive target names a kind outright.
	#[test]
	fn kind_hints_are_given_only_for_literal_kinds() {
		let targets = Targets::compile(["Deployment/.*", "service/frontend", "!Service/ignored"])
			.expect("valid");
		let hints = targets.kind_hints().expect("both kinds are literal");
		assert_eq!(
			hints.into_iter().collect::<std::collections::BTreeSet<_>>(),
			["deployment".to_owned(), "service".to_owned()]
				.into_iter()
				.collect::<std::collections::BTreeSet<_>>()
		);
	}

	/// Narrowing on a guess would hide resources that should have been pruned,
	/// so anything less than a literal kind means every kind must be considered.
	#[test]
	fn kind_hints_are_withheld_when_a_kind_is_a_pattern() {
		for patterns in [
			vec![".*/name"],
			vec!["Deploy.*/name"],
			vec!["(Deployment|Service)/name"],
			vec!["Deployment/name", "Serv.*/other"],
			// Negative-only: nothing was asked for, so nothing can be skipped.
			vec!["!Service/ignored"],
		] {
			let targets = Targets::compile(&patterns).expect("valid");
			assert_eq!(
				targets.kind_hints(),
				None,
				"{patterns:?} should not narrow the kinds listed"
			);
		}
	}

	use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
	use rtk_spec::canonical::{Environment, EnvironmentSpec, ResourceDefaults};
	use serde_json::Value;
	use serde_json::json;

	use super::*;

	/// Collect from a document, reporting the message a caller would see.
	fn collect(value: Value) -> Result<Vec<Value>, String> {
		let mut manifests = Vec::new();
		collect_manifests(value, "", &mut manifests)
			.map(|()| manifests)
			.map_err(|error| error.to_string())
	}

	/// Every shape rtk used to walk past in silence, with the message tk gives.
	///
	/// Reaching a value that cannot be walked means the Jsonnet produced
	/// something that was never a Kubernetes object, and tk refuses the export.
	/// rtk exported nothing and exited zero, so a misspelled `kind` disappeared.
	#[test]
	fn a_value_that_is_not_a_kubernetes_object_fails_the_export() {
		for (document, expected) in [
			(
				json!({ "x": { "apiVersion": "v1", "metadata": { "name": "a" } } }),
				r#"found invalid Kubernetes object (at .x): missing attribute "kind""#,
			),
			(
				json!({ "x": { "kind": "ConfigMap", "metadata": { "name": "a" } } }),
				r#"found invalid Kubernetes object (at .x): missing attribute "apiVersion""#,
			),
			(
				json!({ "x": { "metadata": { "name": "a" }, "data": { "k": "v" } } }),
				r#"found invalid Kubernetes object (at .x.data): missing attribute "apiVersion""#,
			),
			(
				json!({ "x": "just a string" }),
				r#"found invalid Kubernetes object (at .): missing attribute "apiVersion""#,
			),
			(
				json!({ "x": ["a"] }),
				r#"found invalid Kubernetes object (at .x): missing attribute "apiVersion""#,
			),
			(
				json!({ "x": { "apiVersion": "v1", "kind": 42, "metadata": { "name": "a" } } }),
				r#"found invalid Kubernetes object (at .x): attribute "kind" is not a string, it is a float64"#,
			),
			(
				json!({ "x": { "apiVersion": "v1", "kind": "", "metadata": { "name": "a" } } }),
				r#"found invalid Kubernetes object (at .x): attribute "kind" is empty"#,
			),
		] {
			assert_eq!(
				collect(document.clone()).unwrap_err(),
				expected,
				"for {document}"
			);
		}
	}

	/// Which failure is reported depends on the order fields are walked, and tk
	/// sorts them: `data` is reached before `metadata`, so it is `data` that is
	/// blamed even though both are equally unwalkable.
	#[test]
	fn fields_are_walked_in_sorted_order() {
		let error = collect(json!({
			"x": { "metadata": { "name": "a" }, "data": { "k": "v" } }
		}))
		.unwrap_err();

		assert!(error.contains(".x.data"), "{error}");
	}

	/// A field a false condition left unset is skipped rather than treated as a
	/// value that cannot be walked, or every `if` in a Tanka library would fail.
	#[test]
	fn a_null_field_is_skipped() {
		let manifests = collect(json!({
			"absent": null,
			"present": {
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "kept" },
			},
		}))
		.expect("a null beside a manifest is not a failure");

		assert_eq!(manifests.len(), 1);
		assert_eq!(manifests[0]["metadata"]["name"], "kept");
	}

	/// ksonnet's private field is dropped before anything is decided, so it
	/// neither blocks the walk nor reaches the exported manifest.
	#[test]
	fn the_ksonnet_field_is_dropped() {
		let manifests = collect(json!({
			"__ksonnet": "private",
			"present": {
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "kept" },
				"__ksonnet": "private",
			},
		}))
		.expect("a __ksonnet sibling is not a failure");

		assert_eq!(manifests.len(), 1);
		assert_eq!(manifests[0].get("__ksonnet"), None);
	}

	/// A `List` is still expanded into its items rather than exported whole.
	#[test]
	fn a_list_is_expanded() {
		let manifests = collect(json!({
			"apiVersion": "v1",
			"kind": "List",
			"items": [
				{ "apiVersion": "v1", "kind": "ConfigMap", "metadata": { "name": "one" } },
				{ "apiVersion": "v1", "kind": "ConfigMap", "metadata": { "name": "two" } },
			],
		}))
		.expect("a list of manifests");

		assert_eq!(manifests.len(), 2);
	}

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

	fn processed(manifest: Value, environment: &Environment<'static>) -> Value {
		let mut manifest = manifest;
		Processing::new(environment, true).apply(&mut manifest);
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
	fn processing_leaves_the_manifest_the_environment_evaluated() {
		// Injection happens after the environment has been evaluated and
		// materialized, so a manifest reading its own `metadata` sees what it
		// declared rather than what was injected into it afterwards.
		let value = rtk_jsonnet::Engine::new(rtk_jsonnet::Options::default())
			.create_evaluator()
			.evaluate_snippet(
				r"{
  apiVersion: 'v1',
  kind: 'ConfigMap',
  metadata: { name: 'a' },
  sawInjectedNamespace: std.objectHas(self.metadata, 'namespace'),
}",
			)
			.expect("valid Jsonnet")
			.into_value();
		let manifest = processed(
			materialize(&value).expect("materializable"),
			&environment(|_| {}),
		);

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

		let keep = |patterns: &[&str]| Targets::compile(patterns).unwrap().keeps(&manifest);

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

		assert!(Targets::compile(["["]).is_err());
	}

	#[test]
	fn sorts_keys_like_go_yaml_does() {
		let value = json!({
			"b": 1, "a": 2, "a10": 3, "a2": 4, "_x": 5, "A": 6,
			"nested": { "z": 1, "y": 2 },
		});
		assert_eq!(
			serialize(&value).expect("serializable"),
			"_x: 5\nA: 6\na: 2\na2: 4\na10: 3\nb: 1\nnested:\n  \"y\": 2\n  z: 1\n"
		);
	}

	#[test]
	fn describes_manifests_for_diagnostics() {
		assert_eq!(
			describe(&json!({
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "a" },
			})),
			"v1 ConfigMap/a"
		);
		assert_eq!(
			describe(&json!({ "unidentified": true })),
			r#"{"unidentified":true}"#
		);
	}
}
