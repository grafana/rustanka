// Tanka-compatible native functions
// These are wrappers around the existing stdlib functions to provide
// Tanka-compatible API accessible via std.native()

use std::{
	cell::RefCell,
	collections::{HashMap, HashSet},
	io::{BufReader, Read, Write},
	process::{Command, Stdio},
	rc::Rc,
	sync::{Arc, Condvar, Mutex, OnceLock, RwLock},
	thread,
};

use jrsonnet_evaluator::{
	error::{ErrorKind::*, Result},
	manifest::JsonFormat,
	IStr, ObjValue, Thunk, Val,
};
use jrsonnet_macros::builtin;
use jrsonnet_stdlib::RegexCacheInner;
use serde_json;
use sha2::{Digest, Sha256};

// Global Helm template cache - caches the rendered helm output to avoid
// redundant helm invocations (same optimization as Go Tanka), shared across
// all worker threads so identical helmTemplate calls in different environments
// only run helm once.
//
// The cached value is the *manifested JSON* of the final keyed resource map
// (the object that `helmTemplate` returns), NOT the raw YAML. Storing the
// post-parse projection lets cache hits skip YAML parsing entirely - a hit is
// a single `serde_json::from_str` into a `Val`. We cache a `String` rather than
// a `Val` because `Val` is `Rc`-based and not `Send`/`Sync`. Because the JSON
// map is keyed by `nameFormat`, the cache key must include `nameFormat` too.
static HELM_TEMPLATE_CACHE: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

/// Serializes tests that touch the process-global Helm cache above. Cargo runs
/// tests in parallel within a single process, so without this lock concurrent
/// tests would clobber each other's cache entries.
#[cfg(test)]
pub(crate) static HELM_CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Get or create the Helm template cache
fn get_helm_cache() -> &'static RwLock<Option<HashMap<String, String>>> {
	// Initialize the cache if needed
	// Use unwrap_or_else to recover from poisoned locks - if another thread panicked
	// while holding the lock, we still want to proceed with the cache initialization
	{
		let read = HELM_TEMPLATE_CACHE
			.read()
			.unwrap_or_else(|e| e.into_inner());
		if read.is_some() {
			return &HELM_TEMPLATE_CACHE;
		}
	}
	{
		let mut write = HELM_TEMPLATE_CACHE
			.write()
			.unwrap_or_else(|e| e.into_inner());
		if write.is_none() {
			*write = Some(HashMap::new());
		}
	}
	&HELM_TEMPLATE_CACHE
}

/// State of a single `rtkMemoize` cache slot.
///
/// The value is stored as a JSON string (not a `Val`) because `Val` is not
/// `Send`/`Sync` (it uses `Rc` internally), while the cache is shared across
/// worker threads.
enum MemoState {
	/// A worker is currently evaluating the value for this key.
	Computing,
	/// The value has been computed and is available as JSON.
	Done(String),
	/// Computation failed; the next waiter should retry the computation.
	Failed,
}

/// A per-key slot guarding a single memoized value.
///
/// Only one worker computes the value for a given key. Other workers that
/// request the same key block on the condvar until the result is available
/// (or until the computation fails, in which case they retry).
struct MemoSlot {
	state: Mutex<MemoState>,
	cond: Condvar,
}

/// Global cache for `rtkMemoize`, shared across all worker threads.
static MEMO_CACHE: OnceLock<Mutex<HashMap<String, Arc<MemoSlot>>>> = OnceLock::new();

fn memo_cache() -> &'static Mutex<HashMap<String, Arc<MemoSlot>>> {
	MEMO_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

thread_local! {
	/// Keys this worker thread is currently computing. Used to detect
	/// same-thread re-entrancy (a thunk that memoizes its own key), which
	/// would otherwise deadlock the thread against itself.
	static MEMO_IN_PROGRESS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// RAII guard for the computing worker. On drop it always clears the
/// thread-local in-progress marker, and if the computation did not complete
/// (an error or a panic), it marks the slot as `Failed`, removes it from the
/// global cache, and wakes any waiters so they can retry. This guarantees a
/// slot is never left stuck in `Computing`, even on panic.
struct MemoComputeGuard<'a> {
	key: &'a str,
	slot: Arc<MemoSlot>,
	completed: bool,
}

impl Drop for MemoComputeGuard<'_> {
	fn drop(&mut self) {
		MEMO_IN_PROGRESS.with(|set| {
			set.borrow_mut().remove(self.key);
		});

		if self.completed {
			return;
		}

		// Computation did not finish: remove the slot so a future call can
		// retry, then wake any waiters so they retry too.
		{
			let mut map = memo_cache().lock().unwrap_or_else(|e| e.into_inner());
			map.remove(self.key);
		}
		let mut state = self.slot.state.lock().unwrap_or_else(|e| e.into_inner());
		*state = MemoState::Failed;
		self.slot.cond.notify_all();
	}
}

/// Recursively check whether a value contains any hidden object field.
///
/// The memoization cache stores values as JSON, which silently drops hidden
/// (`::`) fields. To avoid surprising data loss we reject such values up front
/// instead of caching a lossy projection. Hidden fields are detected by name
/// (comparing the full field set against the visible one), so a hidden field's
/// value is never evaluated.
fn contains_hidden_field(val: &Val) -> Result<bool> {
	match val {
		Val::Obj(obj) => {
			let visible: HashSet<IStr> = obj.fields_ex(false).into_iter().collect();
			let all = obj.fields_ex(true);
			// `all` is always a superset of `visible`; a size difference means
			// at least one field is hidden.
			if all.len() != visible.len() {
				return Ok(true);
			}
			for name in all {
				let field = obj
					.get(name)?
					.expect("field listed by fields_ex must exist");
				if contains_hidden_field(&field)? {
					return Ok(true);
				}
			}
			Ok(false)
		}
		Val::Arr(arr) => {
			for item in arr.iter() {
				if contains_hidden_field(&item?)? {
					return Ok(true);
				}
			}
			Ok(false)
		}
		_ => Ok(false),
	}
}

/// Tanka-incompatible (rtk extension) rtkMemoize
///
/// Caches the result of `value` under `key` in a process-global cache shared
/// across all worker threads. The second argument is lazy: it is only
/// evaluated on a cache miss. If one worker is already computing the value for
/// a key, other workers requesting the same key block until the result is
/// ready, then reuse it without re-evaluating their own thunk.
///
/// The cached value is stored as JSON, so the returned value is always the
/// JSON-manifested projection of the thunk. Because that projection silently
/// drops hidden (`::`) fields, a value containing any hidden field (at any
/// depth) is rejected with an error rather than cached lossily. The same JSON
/// value is returned to the computing worker too, so every caller observes an
/// identical value regardless of who wins the race to compute it.
#[builtin]
pub fn rtk_memoize(key: String, value: Thunk<Val>) -> Result<Val> {
	// Guard against a thunk that memoizes its own key on the same thread,
	// which would block the thread waiting on a result only it can produce.
	let reentrant = MEMO_IN_PROGRESS.with(|set| set.borrow().contains(&key));
	if reentrant {
		return Err(RuntimeError(
			format!("rtkMemoize: re-entrant evaluation of key {key:?}").into(),
		)
		.into());
	}

	loop {
		// Decide whether this worker computes the value or waits for another
		// worker that is already computing it.
		let (slot, is_computer) = {
			let mut map = memo_cache().lock().unwrap_or_else(|e| e.into_inner());
			if let Some(existing) = map.get(&key) {
				(existing.clone(), false)
			} else {
				let slot = Arc::new(MemoSlot {
					state: Mutex::new(MemoState::Computing),
					cond: Condvar::new(),
				});
				map.insert(key.clone(), slot.clone());
				(slot, true)
			}
		};

		if is_computer {
			MEMO_IN_PROGRESS.with(|set| {
				set.borrow_mut().insert(key.clone());
			});
			// The guard cleans up on every exit path (error or panic) unless
			// we mark it completed after successfully storing the result.
			let mut guard = MemoComputeGuard {
				key: &key,
				slot: slot.clone(),
				completed: false,
			};

			// Evaluate the (lazy) thunk and fully manifest it to JSON so the
			// result can be shared with other threads. The slot lock is NOT
			// held during evaluation, so other keys make progress and waiters
			// on this key simply block on the condvar.
			let evaluated = value.evaluate()?;
			if contains_hidden_field(&evaluated)? {
				return Err(RuntimeError(
					format!(
						"rtkMemoize: value for key {key:?} contains hidden field(s), \
						 which cannot be memoized (they would be dropped by JSON serialization)"
					)
					.into(),
				)
				.into());
			}
			let json = evaluated.manifest(JsonFormat::default())?;
			let result: Val = serde_json::from_str(&json)
				.map_err(|e| RuntimeError(format!("failed to parse memoized value: {e}").into()))?;

			{
				let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
				*state = MemoState::Done(json);
				slot.cond.notify_all();
			}
			guard.completed = true;
			return Ok(result);
		}

		// Waiter path: block until the computing worker finishes (or fails).
		let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
		loop {
			match &*state {
				MemoState::Done(json) => {
					return serde_json::from_str(json).map_err(|e| {
						RuntimeError(format!("failed to parse memoized value: {e}").into()).into()
					});
				}
				// The computing worker failed; retry the whole operation so
				// this worker (or another) re-attempts the computation.
				MemoState::Failed => break,
				MemoState::Computing => {
					state = slot.cond.wait(state).unwrap_or_else(|e| e.into_inner());
				}
			}
		}
		// The computation failed; drop the lock and retry the whole operation.
		drop(state);
	}
}

/// Generate a key for a manifest using the nameFormat template
/// This is a simplified implementation that handles the common case where nameFormat
/// includes namespace in the key format
fn generate_manifest_key_from_val(val: &Val, name_format: Option<&str>) -> Result<String> {
	// Check if we should use nameFormat or default format
	let use_namespace_in_key = name_format
		.map(|fmt| fmt.contains("metadata.namespace") || fmt.contains(".or .metadata.namespace"))
		.unwrap_or(false);

	if let Val::Obj(ref obj) = val {
		let kind = obj
			.get("kind".into())
			.ok()
			.flatten()
			.and_then(|v| match v {
				Val::Str(s) => Some(to_snake_case(&s.to_string())),
				_ => None,
			})
			.unwrap_or_else(|| "unknown".to_string());

		let metadata = obj.get("metadata".into()).ok().flatten();

		if let Some(Val::Obj(meta)) = metadata {
			let name = meta
				.get("name".into())
				.ok()
				.flatten()
				.and_then(|v| match v {
					Val::Str(s) => Some(to_snake_case(&s.to_string())),
					_ => None,
				})
				.unwrap_or_else(|| "unknown".to_string());

			// If nameFormat suggests using namespace, include it in the key
			if use_namespace_in_key {
				let namespace = meta
					.get("namespace".into())
					.ok()
					.flatten()
					.and_then(|v| match v {
						Val::Str(s) => Some(to_snake_case(&s.to_string())),
						_ => None,
					})
					.unwrap_or_else(|| "cluster".to_string());

				return Ok(format!("{}_{}_{}", namespace, kind, name));
			} else {
				return Ok(format!("{}_{}", kind, name));
			}
		}
	}

	Ok("unknown".to_string())
}

/// Parse YAML output from helm into a Val object
fn parse_helm_yaml_output(yaml_content: &str, name_format: Option<&str>) -> Result<Val> {
	use jrsonnet_evaluator::ObjValueBuilder;
	let mut builder = ObjValueBuilder::new();
	// Use serde-saphyr which properly handles YAML 1.1 features including:
	// - Multiple merge keys (<<) in the same mapping
	// - Octal numbers (0755 -> 493)
	let options = serde_saphyr::Options {
		legacy_octal_numbers: true,
		budget: None, // Disable budget limits - we trust the YAML input
		..Default::default()
	};
	let documents: Vec<Val> = serde_saphyr::from_multiple_with_options(yaml_content, options)
		.map_err(|e| RuntimeError(format!("failed to parse helm output: {e}").into()))?;
	let mut seen_keys = HashMap::new();

	for val in documents {
		// Skip null documents
		if matches!(val, Val::Null) {
			continue;
		}

		// Skip non-object values
		if !matches!(val, Val::Obj(_)) {
			continue;
		}

		// Use the nameFormat-aware key generation
		let key = generate_manifest_key_from_val(&val, name_format)?;

		// Check for duplicate keys and add counter if needed
		let mut final_key = key.clone();
		let mut counter = 2;
		while seen_keys.contains_key(&final_key) {
			final_key = format!("{}_{}", key, counter);
			counter += 1;
		}
		seen_keys.insert(final_key.clone(), ());

		builder.field(&final_key).try_value(val)?;
	}

	Ok(Val::Obj(builder.build()))
}

/// Generate a cache key for a Helm template invocation.
///
/// The key covers the full set of inputs that affect the rendered output: the
/// release name, chart path, namespace, values, CRD/hook flags, API versions,
/// the `nameFormat` (which determines the keys of the returned map), and the
/// chart's `Chart.yaml` contents (so bumping a vendored chart's version
/// invalidates stale cache entries even when the path is unchanged).
fn helm_cache_key(
	name: &str,
	chart_path: &str,
	namespace: Option<&str>,
	values_json: Option<&str>,
	include_crds: bool,
	no_hooks: bool,
	api_versions: &[String],
	name_format: Option<&str>,
	chart_meta: Option<&str>,
) -> String {
	let mut hasher = Sha256::new();
	hasher.update(name.as_bytes());
	hasher.update(b"|");
	hasher.update(chart_path.as_bytes());
	hasher.update(b"|");
	if let Some(ns) = namespace {
		hasher.update(ns.as_bytes());
	}
	hasher.update(b"|");
	if let Some(v) = values_json {
		hasher.update(v.as_bytes());
	}
	hasher.update(b"|");
	hasher.update(if include_crds { b"1" } else { b"0" });
	hasher.update(b"|");
	hasher.update(if no_hooks { b"1" } else { b"0" });
	hasher.update(b"|");
	for av in api_versions {
		hasher.update(av.as_bytes());
		hasher.update(b",");
	}
	hasher.update(b"|");
	if let Some(nf) = name_format {
		hasher.update(nf.as_bytes());
	}
	hasher.update(b"|");
	if let Some(meta) = chart_meta {
		hasher.update(meta.as_bytes());
	}
	format!("{:x}", hasher.finalize())
}

/// Read a chart's `Chart.yaml` contents for inclusion in the cache key. Returns
/// `None` when the file is absent or unreadable; in that case the chart path
/// alone participates in the key.
fn read_chart_meta(chart_path: &str) -> Option<String> {
	let chart_yaml = std::path::Path::new(chart_path).join("Chart.yaml");
	std::fs::read_to_string(chart_yaml).ok()
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

/// Tanka-compatible parseJson
/// Parses a JSON string into a value
#[builtin]
pub fn parse_json(json: String) -> Result<Val> {
	serde_json::from_str(&json)
		.map_err(|e| RuntimeError(format!("failed to parse json: {e}").into()).into())
}

/// Tanka-compatible parseYaml
/// Parses a YAML string (potentially multiple documents) into an array of values
#[builtin]
pub fn parse_yaml(yaml: String) -> Result<Val> {
	// Use serde-saphyr which properly handles YAML 1.1 features including:
	// - Multiple merge keys (<<) in the same mapping
	// - Octal numbers (0755 -> 493)
	let options = serde_saphyr::Options {
		legacy_octal_numbers: true,
		budget: None, // Disable budget limits - we trust the YAML input
		..Default::default()
	};
	let documents: Vec<Val> = serde_saphyr::from_multiple_with_options(&yaml, options)
		.map_err(|e| RuntimeError(format!("failed to parse yaml: {e}").into()))?;

	Ok(Val::Arr(documents.into()))
}

/// Tanka-compatible manifestJsonFromJson
/// Reserializes JSON with custom indentation
#[builtin]
pub fn manifest_json_from_json(json: String, indent: usize) -> Result<String> {
	let parsed: serde_json::Value = serde_json::from_str(&json)
		.map_err(|e| RuntimeError(format!("failed to parse json: {e}").into()))?;

	let indentation = " ".repeat(indent);
	let formatter = serde_json::ser::PrettyFormatter::with_indent(indentation.as_bytes());
	let mut buf = Vec::new();
	let mut serializer = serde_json::Serializer::with_formatter(&mut buf, formatter);

	serde::Serialize::serialize(&parsed, &mut serializer)
		.map_err(|e| RuntimeError(format!("failed to serialize json: {e}").into()))?;

	buf.push(b'\n');
	String::from_utf8(buf)
		.map_err(|e| RuntimeError(format!("failed to convert to utf8: {e}").into()).into())
}

/// Recursively sort JSON object keys using go-yaml v3's natural sort algorithm
/// This matches Go yaml.v3 behavior from sorter.go
fn sort_json_keys_numerically(value: serde_json::Value) -> serde_json::Value {
	match value {
		serde_json::Value::Object(map) => {
			// Collect and sort using go-yaml v3's natural sort
			let mut entries: Vec<(String, serde_json::Value)> = map.into_iter().collect();
			entries.sort_by(|(a, _), (b, _)| yaml_v3_key_compare(a, b));

			// Rebuild the map with sorted keys
			let sorted: serde_json::Map<String, serde_json::Value> = entries
				.into_iter()
				.map(|(k, v)| (k, sort_json_keys_numerically(v)))
				.collect();
			serde_json::Value::Object(sorted)
		}
		serde_json::Value::Array(arr) => {
			serde_json::Value::Array(arr.into_iter().map(sort_json_keys_numerically).collect())
		}
		other => other,
	}
}

/// Implements go-yaml v3's key comparison algorithm (from sorter.go)
/// This is a "natural sort" where:
/// - Numbers are sorted numerically
/// - Letters are sorted before non-letters when transitioning from digits
/// - Non-letters (like '_') are sorted before letters when not in digit context
fn yaml_v3_key_compare(a: &str, b: &str) -> std::cmp::Ordering {
	let ar: Vec<char> = a.chars().collect();
	let br: Vec<char> = b.chars().collect();
	let mut digits = false;

	let min_len = ar.len().min(br.len());
	for i in 0..min_len {
		if ar[i] == br[i] {
			digits = ar[i].is_ascii_digit();
			continue;
		}

		let al = ar[i].is_alphabetic();
		let bl = br[i].is_alphabetic();

		if al && bl {
			return ar[i].cmp(&br[i]);
		}

		if al || bl {
			// One is a letter, one is not
			if digits {
				// After digits: letters come first
				return if al {
					std::cmp::Ordering::Less
				} else {
					std::cmp::Ordering::Greater
				};
			} else {
				// Not after digits: non-letters come first
				return if bl {
					std::cmp::Ordering::Less
				} else {
					std::cmp::Ordering::Greater
				};
			}
		}

		// Both are non-letters - check for numeric sequences
		// Handle leading zeros
		let mut an: i64 = 0;
		let mut bn: i64 = 0;

		if ar[i] == '0' || br[i] == '0' {
			// Check if previous chars were non-zero digits
			let mut j = i;
			while j > 0 && ar[j - 1].is_ascii_digit() {
				j -= 1;
				if ar[j] != '0' {
					an = 1;
					bn = 1;
					break;
				}
			}
		}

		// Parse numeric sequences
		let mut ai = i;
		while ai < ar.len() && ar[ai].is_ascii_digit() {
			an = an * 10 + (ar[ai] as i64 - '0' as i64);
			ai += 1;
		}

		let mut bi = i;
		while bi < br.len() && br[bi].is_ascii_digit() {
			bn = bn * 10 + (br[bi] as i64 - '0' as i64);
			bi += 1;
		}

		if an != bn {
			return an.cmp(&bn);
		}
		if ai != bi {
			return ai.cmp(&bi);
		}
		return ar[i].cmp(&br[i]);
	}

	ar.len().cmp(&br.len())
}

/// Tanka-compatible manifestYamlFromJson
/// Converts JSON string to YAML using Go yaml.v3 compatible settings
#[builtin]
pub fn manifest_yaml_from_json(json: String) -> Result<String> {
	let parsed: serde_json::Value = serde_json::from_str(&json)
		.map_err(|e| RuntimeError(format!("failed to parse json: {e}").into()))?;

	// Sort keys numerically to match Go yaml.v3 behavior
	let sorted = sort_json_keys_numerically(parsed);

	// Use serde-saphyr with Go yaml.v3 compatible settings
	// This matches tk's manifestYamlFromJson which uses go-yaml v3
	// Go yaml.v3's yaml.Marshal() defaults to best_width = 2^31-1 (no wrapping)
	let options = serde_saphyr::SerializerOptions {
		indent_step: 4,     // go-yaml v3 uses 4-space indentation
		indent_array: None, // use indent_step for arrays too
		prefer_block_scalars: true,
		empty_map_as_braces: true,
		empty_array_as_brackets: true,
		block_scalar_indent_in_seq: Some(2), // 2 spaces absolute for block scalar body in arrays
		line_width: None,                    // go-yaml v3's Marshal() doesn't wrap lines by default
		scientific_notation_threshold: Some(1000000), // 1 million - large numbers use scientific notation
		scientific_notation_small_threshold: Some(0.0001), // 1e-4 - small numbers use scientific notation (Go yaml.v3)
		quote_numeric_strings: true,                       // Quote numeric string keys like "12345"
		..Default::default()
	};
	let mut output = String::new();
	serde_saphyr::to_fmt_writer_with_options(&mut output, &sorted, options)
		.map_err(|e| RuntimeError(format!("failed to serialize yaml: {e}").into()))?;

	// Add trailing newline to match Go's yaml.v3 behavior
	// This ensures the outer YAML serializer uses | instead of |-
	if !output.ends_with('\n') {
		output.push('\n');
	}

	Ok(output)
}

/// Tanka-compatible sha256
/// Computes SHA256 hash of a string
#[builtin]
pub fn sha256(str: String) -> String {
	let mut hasher = Sha256::new();
	hasher.update(str.as_bytes());
	format!("{:x}", hasher.finalize())
}

/// Tanka-compatible escapeStringRegex
/// Escapes regex special characters using Go's regexp.QuoteMeta set: \.+*?()|[]{}^$
/// This matches Go's behavior exactly (Rust's regex::escape escapes additional characters like `-`).
#[builtin]
pub fn escape_string_regex(pattern: String) -> String {
	const GO_META: &str = r"\.+*?()|[]{}^$";
	let mut escaped = String::with_capacity(pattern.len() * 2);
	for ch in pattern.chars() {
		if GO_META.contains(ch) {
			escaped.push('\\');
		}
		escaped.push(ch);
	}
	escaped
}

/// Tanka-compatible regexMatch
/// Returns true if the string matches the regex pattern
#[builtin(fields(
    cache: Rc<RegexCacheInner>,
))]
pub fn regex_match(this: &regex_match, regex: IStr, string: String) -> Result<bool> {
	let regex = this.cache.parse(regex)?;
	Ok(regex.is_match(&string))
}

/// Tanka-compatible regexSubst
/// Replaces all matches of regex with replacement string
#[builtin(fields(
    cache: Rc<RegexCacheInner>,
))]
pub fn regex_subst(this: &regex_subst, regex: IStr, src: String, repl: String) -> Result<String> {
	let regex = this.cache.parse(regex)?;
	let replaced = regex.replace_all(&src, repl.as_str());
	Ok(replaced.to_string())
}

/// Tanka-compatible helmTemplate
/// Executes `helm template` and returns the rendered manifests as an object
/// Each manifest is keyed by "<snake_case_kind>_<snake_case_name>"
#[builtin]
pub fn helm_template(name: String, chart: String, opts: ObjValue) -> Result<Val> {
	// calledFrom is required for proper path resolution

	let called_from = opts.get("calledFrom".into())?.ok_or_else(|| {
		RuntimeError("helmTemplate requires calledFrom field (usually std.thisFile)".into())
	})?;

	// Resolve chart path relative to calledFrom
	let chart_path = if let Val::Str(s) = called_from {
		let called_from_str = s.to_string();

		// Check that calledFrom is not empty
		if called_from_str.is_empty() {
			return Err(RuntimeError("calledFrom cannot be an empty string".into()).into());
		}

		let called_from_path = std::path::Path::new(&called_from_str);
		// Get the directory containing the calling file
		if let Some(dir) = called_from_path.parent() {
			// Check if directory exists
			if !dir.exists() {
				return Err(RuntimeError(
					format!("calledFrom directory does not exist: {}", dir.display()).into(),
				)
				.into());
			}
			// Prevent absolute paths by prefixing with '.' if chart starts with '/'
			let chart_relative = if chart.starts_with('/') {
				format!(".{}", chart)
			} else {
				chart
			};
			// Join the chart path with the directory
			let chart_full = dir.join(&chart_relative);

			// Check if the chart path exists
			if !chart_full.exists() {
				return Err(RuntimeError(
					format!("chart path does not exist: {}", chart_full.display()).into(),
				)
				.into());
			}

			chart_full
				.to_str()
				.ok_or_else(|| RuntimeError("invalid chart path".into()))?
				.to_string()
		} else {
			return Err(RuntimeError(
				format!("calledFrom has no parent directory: {}", called_from_str).into(),
			)
			.into());
		}
	} else {
		return Err(RuntimeError("calledFrom must be a string".into()).into());
	};

	// Extract namespace for cache key
	let namespace = if let Some(ns) = opts.get("namespace".into())? {
		if let Val::Str(s) = ns {
			Some(s.to_string())
		} else {
			None
		}
	} else {
		None
	};

	// Extract values and serialize to JSON for cache key
	let values_json =
		if let Some(values) = opts.get("values".into())? {
			Some(serde_json::to_string(&values).map_err(|e| {
				RuntimeError(format!("failed to serialize values to json: {e}").into())
			})?)
		} else {
			None
		};

	// Extract nameFormat if present
	let name_format = if let Some(nf) = opts.get("nameFormat".into())? {
		if let Val::Str(s) = nf {
			Some(s.to_string())
		} else {
			None
		}
	} else {
		None
	};

	// Extract includeCrds if present (defaults to true, matching Go Tanka's behavior)
	// Go Tanka: "default IncludeCRDs to true, as this is the default in the `helm install`"
	let include_crds = if let Some(ic) = opts.get("includeCrds".into())? {
		matches!(ic, Val::Bool(true))
	} else {
		true
	};

	// Extract apiVersions if present (array of strings for --api-versions flag)
	let api_versions: Vec<String> = if let Some(av) = opts.get("apiVersions".into())? {
		if let Val::Arr(arr) = av {
			arr.iter()
				.filter_map(|v| {
					if let Ok(Val::Str(s)) = v {
						Some(s.to_string())
					} else {
						None
					}
				})
				.collect()
		} else {
			Vec::new()
		}
	} else {
		Vec::new()
	};

	// Extract noHooks if present (defaults to false, matching Go Tanka's behavior)
	// When true, passes --no-hooks to helm template to exclude hook resources
	let no_hooks = if let Some(nh) = opts.get("noHooks".into())? {
		matches!(nh, Val::Bool(true))
	} else {
		false
	};

	// Benchmarking escape hatch: when RTK_HELM_DISABLE_MEMOIZATION is set,
	// bypass the in-memory cache entirely so every helmTemplate call invokes
	// helm. Used to measure the true cost of helm rendering without any
	// deduplication.
	let cache_disabled = std::env::var_os("RTK_HELM_DISABLE_MEMOIZATION").is_some();

	// Check cache first
	let chart_meta = read_chart_meta(&chart_path);
	let cache_key = helm_cache_key(
		&name,
		&chart_path,
		namespace.as_deref(),
		values_json.as_deref(),
		include_crds,
		no_hooks,
		&api_versions,
		name_format.as_deref(),
		chart_meta.as_deref(),
	);
	if !cache_disabled {
		let cache = get_helm_cache();
		let read = cache.read().unwrap_or_else(|e| e.into_inner());
		if let Some(ref map) = *read {
			if let Some(cached_json) = map.get(&cache_key) {
				// Cache hit - deserialize the stored manifest map directly,
				// skipping the helm invocation and YAML parsing entirely.
				let val: Val = serde_json::from_str(cached_json).map_err(|e| {
					RuntimeError(format!("failed to parse cached helm output: {e}").into())
				})?;
				return Ok(val);
			}
		}
	}

	let mut cmd = Command::new("helm");
	cmd.arg("template");
	cmd.arg(&name);
	cmd.arg(&chart_path);

	// Add namespace if present
	if let Some(ref ns) = namespace {
		cmd.arg("--namespace");
		cmd.arg(ns);
	}

	// Add --include-crds if requested
	if include_crds {
		cmd.arg("--include-crds");
	}

	// Add --no-hooks if requested (excludes hook resources from template output)
	if no_hooks {
		cmd.arg("--no-hooks");
	}

	// Add --api-versions for each version specified
	for av in &api_versions {
		cmd.arg("--api-versions");
		cmd.arg(av);
	}

	// If we have values, configure stdin and add --values=-
	if values_json.is_some() {
		cmd.arg("--values=-");
		cmd.stdin(Stdio::piped());
	}
	cmd.stdout(Stdio::piped());
	cmd.stderr(Stdio::piped());

	let mut child = cmd
		.spawn()
		.map_err(|e| RuntimeError(format!("failed to execute helm: {e}").into()))?;

	// Write values to stdin if present, then close it
	if let Some(ref json) = values_json {
		if let Some(mut stdin) = child.stdin.take() {
			stdin.write_all(json.as_bytes()).map_err(|e| {
				RuntimeError(format!("failed to write values to helm stdin: {e}").into())
			})?;
			// Close stdin explicitly
			drop(stdin);
		}
	}

	// Take stdout and stderr handles
	let stdout = child
		.stdout
		.take()
		.ok_or_else(|| RuntimeError("failed to capture helm stdout".into()))?;
	let stderr = child
		.stderr
		.take()
		.ok_or_else(|| RuntimeError("failed to capture helm stderr".into()))?;

	// Spawn threads to collect stdout and stderr in parallel
	let stdout_handle = thread::spawn(move || {
		let mut stdout_buf = Vec::new();
		let mut stdout_reader = BufReader::new(stdout);
		stdout_reader.read_to_end(&mut stdout_buf).ok();
		stdout_buf
	});

	let stderr_handle = thread::spawn(move || {
		let mut stderr_buf = Vec::new();
		let mut stderr_reader = BufReader::new(stderr);
		stderr_reader.read_to_end(&mut stderr_buf).ok();
		stderr_buf
	});

	// Wait for the process to complete
	let status = child
		.wait()
		.map_err(|e| RuntimeError(format!("failed to wait for helm: {e}").into()))?;

	// Get stdout from the thread
	let stdout_buf = stdout_handle
		.join()
		.map_err(|_| RuntimeError("failed to join stdout thread".into()))?;

	// Get stderr from the thread
	let stderr_buf = stderr_handle
		.join()
		.map_err(|_| RuntimeError("failed to join stderr thread".into()))?;

	// Check if helm command succeeded
	if !status.success() {
		let stderr = String::from_utf8_lossy(&stderr_buf);
		return Err(RuntimeError(format!("helm template failed: {stderr}").into()).into());
	}

	// Convert stdout to string (YAML content)
	let yaml_content = String::from_utf8(stdout_buf)
		.map_err(|e| RuntimeError(format!("invalid UTF-8 in helm output: {e}").into()))?;

	// Parse the YAML output into the final keyed resource map.
	let val = parse_helm_yaml_output(&yaml_content, name_format.as_deref())?;

	// Store the manifested JSON projection of the map (not the raw YAML), so
	// future hits skip YAML parsing. This is also the exact byte content the
	// export driver persists to the on-disk `helm-cache` directory.
	if !cache_disabled {
		let json = val
			.manifest(JsonFormat::default())
			.map_err(|e| RuntimeError(format!("failed to manifest helm output: {e}").into()))?;
		{
			let cache = get_helm_cache();
			let mut write = cache.write().unwrap_or_else(|e| e.into_inner());
			if let Some(ref mut map) = *write {
				map.insert(cache_key, json);
			}
		}
	}

	Ok(val)
}

/// Tanka-compatible kustomizeBuild
/// Executes `kustomize build` and returns the rendered manifests as an object
/// Each manifest is keyed by "<snake_case_kind>_<snake_case_name>"
#[builtin]
pub fn kustomize_build(path: String, opts: ObjValue) -> Result<Val> {
	// calledFrom is required for proper path resolution
	let called_from = opts.get("calledFrom".into())?.ok_or_else(|| {
		RuntimeError("kustomizeBuild requires calledFrom field (usually std.thisFile)".into())
	})?;

	// Resolve kustomize path relative to calledFrom
	let kustomize_path = if let Val::Str(s) = called_from {
		let called_from_str = s.to_string();

		// Check that calledFrom is not empty
		if called_from_str.is_empty() {
			return Err(RuntimeError("calledFrom cannot be an empty string".into()).into());
		}

		let called_from_path = std::path::Path::new(&called_from_str);
		// Get the directory containing the calling file
		if let Some(dir) = called_from_path.parent() {
			// Check if directory exists
			if !dir.exists() {
				return Err(RuntimeError(
					format!("calledFrom directory does not exist: {}", dir.display()).into(),
				)
				.into());
			}
			// Prevent absolute paths by prefixing with '.' if path starts with '/'
			let path_relative = if path.starts_with('/') {
				format!(".{}", path)
			} else {
				path
			};
			// Join the kustomize path with the directory
			let kustomize_full = dir.join(&path_relative);

			// Check if the kustomize path exists
			if !kustomize_full.exists() {
				return Err(RuntimeError(
					format!(
						"kustomize path does not exist: {}",
						kustomize_full.display()
					)
					.into(),
				)
				.into());
			}

			kustomize_full
				.to_str()
				.ok_or_else(|| RuntimeError("invalid kustomize path".into()))?
				.to_string()
		} else {
			return Err(RuntimeError(
				format!("calledFrom has no parent directory: {}", called_from_str).into(),
			)
			.into());
		}
	} else {
		return Err(RuntimeError("calledFrom must be a string".into()).into());
	};

	let mut cmd = Command::new("kustomize");
	cmd.arg("build");
	cmd.arg(&kustomize_path);
	cmd.stdout(Stdio::piped());
	cmd.stderr(Stdio::piped());

	let mut child = cmd
		.spawn()
		.map_err(|e| RuntimeError(format!("failed to execute kustomize: {e}").into()))?;

	// Take stdout and stderr handles
	let stdout = child
		.stdout
		.take()
		.ok_or_else(|| RuntimeError("failed to capture kustomize stdout".into()))?;
	let stderr = child
		.stderr
		.take()
		.ok_or_else(|| RuntimeError("failed to capture kustomize stderr".into()))?;

	// Spawn a thread to collect stderr
	let stderr_handle = thread::spawn(move || {
		let mut stderr_buf = Vec::new();
		let mut stderr_reader = BufReader::new(stderr);
		stderr_reader.read_to_end(&mut stderr_buf).ok();
		stderr_buf
	});

	// Read stdout and parse YAML output
	use jrsonnet_evaluator::ObjValueBuilder;
	let mut builder = ObjValueBuilder::new();
	let mut stdout_reader = BufReader::new(stdout);
	let mut yaml_content = String::new();
	stdout_reader
		.read_to_string(&mut yaml_content)
		.map_err(|e| RuntimeError(format!("failed to read kustomize output: {e}").into()))?;

	// Use serde-saphyr which properly handles YAML 1.1 features
	let options = serde_saphyr::Options {
		legacy_octal_numbers: true,
		budget: None, // Disable budget limits - we trust the YAML input
		..Default::default()
	};
	let documents: Vec<Val> = serde_saphyr::from_multiple_with_options(&yaml_content, options)
		.map_err(|e| RuntimeError(format!("failed to parse kustomize output: {e}").into()))?;
	let mut seen_keys = HashMap::new();

	for val in documents {
		// Skip null documents
		if matches!(val, Val::Null) {
			continue;
		}

		// Generate a key for this manifest: <snake_case_kind>_<snake_case_name>
		// Note: tk does NOT include namespace in the key, even when present
		let key = if let Val::Obj(ref obj) = val {
			let kind = obj
				.get("kind".into())?
				.and_then(|v| match v {
					Val::Str(s) => Some(to_snake_case(&s.to_string())),
					_ => None,
				})
				.unwrap_or_else(|| "unknown".to_string());

			let metadata = obj.get("metadata".into())?;
			let name = if let Some(Val::Obj(meta)) = metadata {
				meta.get("name".into())?
					.and_then(|v| match v {
						Val::Str(s) => Some(to_snake_case(&s.to_string())),
						_ => None,
					})
					.unwrap_or_else(|| "unknown".to_string())
			} else {
				"unknown".to_string()
			};

			format!("{}_{}", kind, name)
		} else {
			"unknown".to_string()
		};

		// Check for duplicate keys and add counter if needed
		let mut final_key = key.clone();
		let mut counter = 2;
		while seen_keys.contains_key(&final_key) {
			final_key = format!("{}_{}", key, counter);
			counter += 1;
		}
		seen_keys.insert(final_key.clone(), ());

		builder.field(&final_key).try_value(val)?;
	}

	// Wait for the process to complete
	let status = child
		.wait()
		.map_err(|e| RuntimeError(format!("failed to wait for kustomize: {e}").into()))?;

	// Get stderr from the thread
	let stderr_buf = stderr_handle
		.join()
		.map_err(|_| RuntimeError("failed to join stderr thread".into()))?;

	// Check if kustomize command succeeded
	if !status.success() {
		let stderr = String::from_utf8_lossy(&stderr_buf);
		return Err(RuntimeError(format!("kustomize build failed: {stderr}").into()).into());
	}

	Ok(Val::Obj(builder.build()))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_yaml_octal_parsing() {
		// YAML 1.1 octal: 0755 -> 493 decimal
		let yaml = "myval: 0755";
		let result = parse_yaml(yaml.to_string()).unwrap();
		if let Val::Arr(arr) = result {
			let val = arr.get(0).unwrap().unwrap();
			if let Val::Obj(obj) = val {
				let myval = obj.get("myval".into()).unwrap().unwrap();
				if let Val::Num(n) = myval {
					assert_eq!(n.get(), 493.0);
				} else {
					panic!("Expected number, got {:?}", myval);
				}
			} else {
				panic!("Expected object");
			}
		} else {
			panic!("Expected array");
		}

		// Also test double-zero prefix (00755)
		let yaml = "myval: 00755";
		let result = parse_yaml(yaml.to_string()).unwrap();
		if let Val::Arr(arr) = result {
			let val = arr.get(0).unwrap().unwrap();
			if let Val::Obj(obj) = val {
				let myval = obj.get("myval".into()).unwrap().unwrap();
				if let Val::Num(n) = myval {
					assert_eq!(n.get(), 493.0);
				} else {
					panic!("Expected number, got {:?}", myval);
				}
			} else {
				panic!("Expected object");
			}
		} else {
			panic!("Expected array");
		}
	}

	#[test]
	fn test_helm_cache_key_sensitivity() {
		let base = helm_cache_key(
			"rel",
			"/charts/foo",
			Some("ns"),
			Some("{\"a\":1}"),
			true,
			false,
			&["v1".to_string()],
			Some("{{.kind}}"),
			Some("version: 1.0.0"),
		);

		// Identical inputs produce identical keys.
		assert_eq!(
			base,
			helm_cache_key(
				"rel",
				"/charts/foo",
				Some("ns"),
				Some("{\"a\":1}"),
				true,
				false,
				&["v1".to_string()],
				Some("{{.kind}}"),
				Some("version: 1.0.0"),
			)
		);

		// Each parameter that affects rendering changes the key.
		let variants = [
			helm_cache_key(
				"REL2",
				"/charts/foo",
				Some("ns"),
				Some("{\"a\":1}"),
				true,
				false,
				&["v1".to_string()],
				Some("{{.kind}}"),
				Some("version: 1.0.0"),
			),
			helm_cache_key(
				"rel",
				"/charts/bar",
				Some("ns"),
				Some("{\"a\":1}"),
				true,
				false,
				&["v1".to_string()],
				Some("{{.kind}}"),
				Some("version: 1.0.0"),
			),
			helm_cache_key(
				"rel",
				"/charts/foo",
				Some("other"),
				Some("{\"a\":1}"),
				true,
				false,
				&["v1".to_string()],
				Some("{{.kind}}"),
				Some("version: 1.0.0"),
			),
			helm_cache_key(
				"rel",
				"/charts/foo",
				Some("ns"),
				Some("{\"a\":2}"),
				true,
				false,
				&["v1".to_string()],
				Some("{{.kind}}"),
				Some("version: 1.0.0"),
			),
			helm_cache_key(
				"rel",
				"/charts/foo",
				Some("ns"),
				Some("{\"a\":1}"),
				false,
				false,
				&["v1".to_string()],
				Some("{{.kind}}"),
				Some("version: 1.0.0"),
			),
			helm_cache_key(
				"rel",
				"/charts/foo",
				Some("ns"),
				Some("{\"a\":1}"),
				true,
				true,
				&["v1".to_string()],
				Some("{{.kind}}"),
				Some("version: 1.0.0"),
			),
			helm_cache_key(
				"rel",
				"/charts/foo",
				Some("ns"),
				Some("{\"a\":1}"),
				true,
				false,
				&["v2".to_string()],
				Some("{{.kind}}"),
				Some("version: 1.0.0"),
			),
			helm_cache_key(
				"rel",
				"/charts/foo",
				Some("ns"),
				Some("{\"a\":1}"),
				true,
				false,
				&["v1".to_string()],
				Some("{{.metadata.name}}"),
				Some("version: 1.0.0"),
			),
			helm_cache_key(
				"rel",
				"/charts/foo",
				Some("ns"),
				Some("{\"a\":1}"),
				true,
				false,
				&["v1".to_string()],
				Some("{{.kind}}"),
				Some("version: 2.0.0"),
			),
		];
		for variant in variants {
			assert_ne!(base, variant);
		}
	}
}
