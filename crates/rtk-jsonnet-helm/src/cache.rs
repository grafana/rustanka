use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use rustc_hash::{FxBuildHasher, FxHashMap};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use crate::CacheDirectoryResolver;
use crate::functions::template::Options;

/// Bumped when the shape of a stored entry changes.
///
/// What an entry *contains* is covered by the build identity `build.rs`
/// computes, so a change to how a render is post-processed does not need a bump
/// here — only a change to [`Entry`] itself does.
const CACHE_SCHEMA: u32 = 2;
const CACHE_VERSION_DIRECTORY: &str = "v1";
const RENDER_KEY_DOMAIN: &[u8] = b"rtk helm render key v1";
const DISK_KEY_DOMAIN: &[u8] = b"rtk helm disk key v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(super) struct Key([u8; 32]);

impl Key {
	pub(super) fn render(
		name: &str,
		chart_path: &Path,
		options: &Options,
		cache_directory: Option<&Path>,
	) -> io::Result<Key> {
		let mut builder = KeyBuilder::new(RENDER_KEY_DOMAIN);
		builder.string(name);
		options.hash_cache_key(&mut builder);
		builder.helm_environment();
		builder.boolean(cache_directory.is_some());
		if let Some(cache_directory) = cache_directory {
			builder.path(cache_directory);
		}
		builder.chart(chart_path, cache_directory)?;
		Ok(builder.finish())
	}

	fn disk(self, helm_identity: &[u8]) -> Key {
		let mut builder = KeyBuilder::new(DISK_KEY_DOMAIN);
		builder.string(env!("RTK_HELM_BUILD"));
		builder.bytes(&CACHE_SCHEMA.to_le_bytes());
		builder.bytes(&self.0);
		builder.bytes(helm_identity);
		builder.finish()
	}

	fn filename(self) -> String {
		format!("{}.cbor", hex::encode(self.0))
	}
}

#[derive(Debug)]
pub(super) struct Cache {
	values: RwLock<FxHashMap<Key, serde_json::Value>>,
	computations: Mutex<FxHashMap<Key, Arc<Mutex<()>>>>,
	cache_directory: Option<CacheDirectoryResolver>,
}

impl Cache {
	pub(super) fn new(cache_directory: Option<CacheDirectoryResolver>) -> Cache {
		Cache {
			values: RwLock::new(FxHashMap::with_hasher(FxBuildHasher)),
			computations: Mutex::new(FxHashMap::with_hasher(FxBuildHasher)),
			cache_directory,
		}
	}

	pub(super) fn directory(&self, called_from: &Path) -> Option<PathBuf> {
		self.cache_directory.map(|resolve| resolve(called_from))?
	}

	pub(super) fn get(&self, key: Key) -> Option<serde_json::Value> {
		self.values
			.read()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.get(&key)
			.cloned()
	}

	pub(super) fn insert(&self, key: Key, value: serde_json::Value) {
		self.values
			.write()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.insert(key, value);
	}

	pub(super) fn computation(&self, key: Key) -> Arc<Mutex<()>> {
		Arc::clone(
			self.computations
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.entry(key)
				.or_insert_with(|| Arc::new(Mutex::new(()))),
		)
	}

	pub(super) fn read_disk(
		key: Key,
		directory: &Path,
		helm_identity: &[u8],
	) -> Option<serde_json::Value> {
		let key = key.disk(helm_identity);
		let path = directory.join(CACHE_VERSION_DIRECTORY).join(key.filename());
		let metadata = match fs::symlink_metadata(&path) {
			Ok(metadata) if metadata.file_type().is_file() => metadata,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
			Ok(_) => {
				tracing::warn!(path = ?path, "ignored non-regular helm cache entry");
				return None;
			}
			Err(error) => {
				tracing::warn!(path = ?path, %error, "failed to inspect helm cache entry");
				return None;
			}
		};
		let file = match File::open(&path) {
			Ok(file) => file,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
			Err(error) => {
				tracing::warn!(path = ?path, %error, "failed to read helm cache entry");
				return None;
			}
		};
		if !file
			.metadata()
			.is_ok_and(|opened| opened.is_file() && opened.len() == metadata.len())
		{
			tracing::warn!(path = ?path, "ignored replaced helm cache entry");
			return None;
		}
		let entry: Entry = match ciborium::from_reader(BufReader::new(file)) {
			Ok(entry) => entry,
			Err(error) => {
				tracing::warn!(path = ?path, %error, "ignored corrupt helm cache entry");
				return None;
			}
		};
		if entry.schema != CACHE_SCHEMA || entry.key != key {
			tracing::warn!(path = ?path, "ignored incompatible helm cache entry");
			return None;
		}
		if entry.value_checksum != value_checksum(&entry.value) {
			tracing::warn!(path = ?path, "ignored corrupt helm cache entry");
			return None;
		}
		Some(entry.value)
	}

	pub(super) fn write_disk(
		key: Key,
		directory: &Path,
		helm_identity: &[u8],
		value: &serde_json::Value,
	) {
		let key = key.disk(helm_identity);
		let directory = directory.join(CACHE_VERSION_DIRECTORY);
		if let Err(error) = fs::create_dir_all(&directory) {
			tracing::warn!(path = ?directory, %error, "failed to create helm cache directory");
			return;
		}
		let path = directory.join(key.filename());
		let mut temporary = match NamedTempFile::new_in(&directory) {
			Ok(temporary) => temporary,
			Err(error) => {
				tracing::warn!(path = ?directory, %error, "failed to create helm cache entry");
				return;
			}
		};
		let entry = Entry {
			schema: CACHE_SCHEMA,
			key,
			value_checksum: value_checksum(value),
			value: value.clone(),
		};
		if let Err(error) = ciborium::into_writer(&entry, temporary.as_file_mut()) {
			tracing::warn!(path = ?path, %error, "failed to encode helm cache entry");
			return;
		}
		if let Err(error) = temporary.as_file_mut().flush() {
			tracing::warn!(path = ?path, %error, "failed to flush helm cache entry");
			return;
		}
		if let Err(error) = temporary.persist(&path) {
			tracing::warn!(path = ?path, error = %error.error, "failed to persist helm cache entry");
		}
	}
}

#[derive(Deserialize, Serialize)]
struct Entry {
	schema: u32,
	key: Key,
	value_checksum: Key,
	value: serde_json::Value,
}

fn value_checksum(value: &serde_json::Value) -> Key {
	let mut builder = KeyBuilder::new(b"rtk helm cache value v1");
	builder.json(value);
	builder.finish()
}

pub(crate) struct KeyBuilder(Sha256);

impl KeyBuilder {
	fn new(domain: &[u8]) -> KeyBuilder {
		let mut builder = KeyBuilder(Sha256::new());
		builder.bytes(domain);
		builder
	}

	pub(crate) fn bytes(&mut self, value: &[u8]) {
		self.0.update((value.len() as u64).to_le_bytes());
		self.0.update(value);
	}

	fn file_contents(&mut self, path: &Path) -> io::Result<()> {
		let mut file = File::open(path)?;
		let expected_length = file.metadata()?.len();
		self.0.update(expected_length.to_le_bytes());
		let mut actual_length = 0_u64;
		let mut buffer = [0; 16 * 1024];
		loop {
			let read = file.read(&mut buffer)?;
			if read == 0 {
				break;
			}
			self.0.update(&buffer[..read]);
			actual_length = actual_length
				.checked_add(read as u64)
				.ok_or_else(|| io::Error::other("chart file length overflow"))?;
		}
		if actual_length != expected_length {
			return Err(io::Error::other("chart file changed while hashing"));
		}
		Ok(())
	}

	pub(crate) fn string(&mut self, value: &str) {
		self.bytes(value.as_bytes());
	}

	fn os_str(&mut self, value: &OsStr) {
		#[cfg(unix)]
		{
			use std::os::unix::ffi::OsStrExt;
			self.bytes(value.as_bytes());
		}
		#[cfg(windows)]
		{
			use std::os::windows::ffi::OsStrExt;
			let bytes = value
				.encode_wide()
				.flat_map(u16::to_le_bytes)
				.collect::<Vec<_>>();
			self.bytes(&bytes);
		}
		#[cfg(not(any(unix, windows)))]
		self.string(&value.to_string_lossy());
	}

	pub(crate) fn path(&mut self, value: &Path) {
		self.os_str(value.as_os_str());
	}

	pub(crate) fn boolean(&mut self, value: bool) {
		self.bytes(&[u8::from(value)]);
	}

	pub(crate) fn optional_string(&mut self, value: Option<&str>) {
		self.boolean(value.is_some());
		if let Some(value) = value {
			self.string(value);
		}
	}

	pub(crate) fn json(&mut self, value: &serde_json::Value) {
		match value {
			serde_json::Value::Null => self.bytes(b"null"),
			serde_json::Value::Bool(value) => {
				self.bytes(b"bool");
				self.boolean(*value);
			}
			serde_json::Value::Number(value) => {
				self.bytes(b"number");
				self.string(&value.to_string());
			}
			serde_json::Value::String(value) => {
				self.bytes(b"string");
				self.string(value);
			}
			serde_json::Value::Array(values) => {
				self.bytes(b"array");
				self.bytes(&(values.len() as u64).to_le_bytes());
				for value in values {
					self.json(value);
				}
			}
			serde_json::Value::Object(values) => {
				self.bytes(b"object");
				self.bytes(&(values.len() as u64).to_le_bytes());
				let mut keys = values.keys().collect::<Vec<_>>();
				keys.sort_unstable();
				for key in keys {
					self.string(key);
					self.json(&values[key]);
				}
			}
		}
	}

	fn helm_environment(&mut self) {
		for name in [
			"HELM_CACHE_HOME",
			"HELM_CONFIG_HOME",
			"HELM_DATA_HOME",
			"HELM_KUBECONTEXT",
			"HELM_NAMESPACE",
			"HELM_PLUGINS",
			"HOMEDRIVE",
			"HOMEPATH",
			"HOME",
			"KUBECONFIG",
			"PATH",
			"USERPROFILE",
		] {
			self.string(name);
			let value = env::var_os(name);
			self.boolean(value.is_some());
			if let Some(value) = value {
				self.os_str(&value);
			}
		}

		let mut kubeconfig: Vec<PathBuf> = env::var_os("KUBECONFIG")
			.filter(|paths| !paths.is_empty())
			.map_or_else(
				|| {
					let mut configs = [env::var_os("HOME"), env::var_os("USERPROFILE")]
						.into_iter()
						.flatten()
						.map(PathBuf::from)
						.map(|home| home.join(".kube/config"))
						.collect::<Vec<_>>();
					if let (Some(mut drive), Some(path)) =
						(env::var_os("HOMEDRIVE"), env::var_os("HOMEPATH"))
					{
						drive.push(path);
						configs.push(PathBuf::from(drive).join(".kube/config"));
					}
					configs
				},
				|paths| env::split_paths(&paths).collect(),
			);
		kubeconfig.sort_unstable();
		kubeconfig.dedup();
		for path in kubeconfig {
			self.path(&path);
			match fs::read(path) {
				Ok(contents) => self.bytes(&contents),
				Err(error) => self.string(&format!("unreadable:{:?}", error.kind())),
			}
		}
	}

	fn chart(&mut self, chart_path: &Path, cache_directory: Option<&Path>) -> io::Result<()> {
		let chart_path = chart_path
			.canonicalize()
			.unwrap_or_else(|_| chart_path.to_owned());
		let cache_directory = cache_directory.map(canonicalize_with_missing);
		if cache_directory
			.as_ref()
			.is_some_and(|cache| cache.starts_with(&chart_path))
		{
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				"helm cache directory is inside the chart",
			));
		}
		let chart_type = fs::metadata(&chart_path)?.file_type();
		if chart_type.is_file() {
			self.bytes(b"file-chart");
			self.file_contents(&chart_path)?;
			return Ok(());
		}
		if !chart_type.is_dir() {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				"helm chart is not a regular file or directory",
			));
		}

		self.bytes(b"directory-chart");
		let mut entries = WalkDir::new(&chart_path)
			.follow_links(true)
			.into_iter()
			.collect::<Result<Vec<_>, _>>()
			.map_err(io::Error::other)?;
		entries.sort_unstable_by(|left, right| left.path().cmp(right.path()));

		for entry in entries.into_iter().skip(1) {
			if cache_directory
				.as_ref()
				.is_some_and(|cache| entry.path().canonicalize().is_ok_and(|path| path == *cache))
			{
				return Err(io::Error::new(
					io::ErrorKind::InvalidInput,
					"chart links to its helm cache directory",
				));
			}
			let file_type = entry.file_type();
			let is_symlink = entry.path_is_symlink();
			if file_type.is_dir() && !is_symlink {
				continue;
			}
			let relative = entry
				.path()
				.strip_prefix(&chart_path)
				.map_err(io::Error::other)?;
			self.path(relative);
			if is_symlink {
				self.bytes(b"symlink");
				self.path(&fs::read_link(entry.path())?);
				if file_type.is_file() {
					self.file_contents(entry.path())?;
				} else if file_type.is_dir() {
					self.bytes(b"directory");
				} else {
					return Err(io::Error::new(
						io::ErrorKind::InvalidInput,
						"chart symlink target is not a regular file or directory",
					));
				}
			} else if file_type.is_file() {
				self.bytes(b"file");
				self.file_contents(entry.path())?;
			} else {
				return Err(io::Error::new(
					io::ErrorKind::InvalidInput,
					"chart contains a non-regular file",
				));
			}
		}
		Ok(())
	}

	fn finish(self) -> Key {
		Key(self.0.finalize().into())
	}
}

pub(crate) fn helm_identity(binary: &Path, version: &str) -> Box<[u8]> {
	let mut builder = KeyBuilder::new(b"rtk helm executable identity v1");
	builder.path(binary);
	builder.string(version);
	builder.finish().0.into()
}

pub(crate) fn canonicalize_with_missing(path: &Path) -> PathBuf {
	let mut existing = path;
	let mut missing = Vec::new();
	loop {
		if let Ok(mut canonical) = existing.canonicalize() {
			for component in missing.iter().rev() {
				canonical.push(component);
			}
			return canonical;
		}
		let Some(name) = existing.file_name() else {
			return path.to_owned();
		};
		missing.push(name.to_owned());
		let Some(parent) = existing.parent() else {
			return path.to_owned();
		};
		existing = parent;
	}
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::thread;
	use std::time::Duration;

	use serde_json::json;
	use tempfile::tempdir;

	use super::*;

	fn options(value: serde_json::Value) -> Options {
		serde_json::from_value(value).unwrap()
	}

	fn base_options(called_from: &Path) -> serde_json::Value {
		json!({
			"apiVersions": ["example.test/v1"],
			"calledFrom": called_from,
			"includeCrds": true,
			"nameFormat": "{{.kind}}-{{.metadata.name}}",
			"namespace": "default",
			"noHooks": false,
			"values": { "a": 1, "b": [true, null] },
		})
	}

	fn cache_directory(called_from: &Path) -> Option<PathBuf> {
		Some(called_from.parent()?.join("target").join("helm"))
	}

	#[test]
	fn render_key_covers_chart_contents_and_rendering_options() {
		let temp = tempdir().unwrap();
		let chart = temp.path().join("chart");
		fs::create_dir_all(chart.join("templates")).unwrap();
		fs::write(chart.join("Chart.yaml"), "name: example\nversion: 1.0.0\n").unwrap();
		fs::write(chart.join("templates/deployment.yaml"), "first").unwrap();
		let called_from = temp.path().join("main.jsonnet");
		let base = base_options(&called_from);
		let base_key = Key::render("release", &chart, &options(base.clone()), None).unwrap();

		for (field, value) in [
			("apiVersions", json!(["example.test/v2"])),
			("includeCrds", json!(false)),
			("nameFormat", json!("{{.metadata.name}}")),
			("namespace", json!("other")),
			("noHooks", json!(true)),
			("values", json!({ "a": 2, "b": [true, null] })),
		] {
			let mut changed = base.clone();
			changed[field] = value;
			assert_ne!(
				base_key,
				Key::render("release", &chart, &options(changed), None).unwrap(),
				"{field} was omitted from the key"
			);
		}

		assert_ne!(
			base_key,
			Key::render("other-release", &chart, &options(base.clone()), None).unwrap()
		);
		fs::write(chart.join("templates/deployment.yaml"), "second").unwrap();
		assert_ne!(
			base_key,
			Key::render("release", &chart, &options(base), None).unwrap()
		);
	}

	#[test]
	fn render_key_canonicalizes_json_objects_and_ignores_called_from_spelling() {
		let temp = tempdir().unwrap();
		let chart = temp.path().join("chart.tgz");
		fs::write(&chart, "chart bytes").unwrap();
		let first = options(
			serde_json::from_str(r#"{"calledFrom":"one/main.jsonnet","values":{"a":1,"b":2}}"#)
				.unwrap(),
		);
		let second = options(
			serde_json::from_str(r#"{"calledFrom":"two/main.jsonnet","values":{"b":2,"a":1}}"#)
				.unwrap(),
		);

		assert_eq!(
			Key::render("release", &chart, &first, None).unwrap(),
			Key::render("release", &chart, &second, None).unwrap()
		);
	}

	#[test]
	fn render_key_is_scoped_to_its_project_cache() {
		let temp = tempdir().unwrap();
		let chart = temp.path().join("chart.tgz");
		fs::write(&chart, "chart bytes").unwrap();
		let options = options(json!({ "calledFrom": temp.path().join("main.jsonnet") }));

		assert_ne!(
			Key::render(
				"release",
				&chart,
				&options,
				Some(&temp.path().join("first/target/helm")),
			)
			.unwrap(),
			Key::render(
				"release",
				&chart,
				&options,
				Some(&temp.path().join("second/target/helm")),
			)
			.unwrap(),
		);
	}

	#[cfg(unix)]
	#[test]
	fn render_key_follows_symlinked_chart_directories() {
		use std::os::unix::fs::symlink;

		let temp = tempdir().unwrap();
		let chart = temp.path().join("chart");
		let shared = temp.path().join("shared");
		fs::create_dir_all(&chart).unwrap();
		fs::create_dir_all(&shared).unwrap();
		fs::write(shared.join("value.yaml"), "first").unwrap();
		symlink(&shared, chart.join("linked")).unwrap();
		let options = options(json!({ "calledFrom": temp.path().join("main.jsonnet") }));
		let first = Key::render("release", &chart, &options, None).unwrap();

		fs::write(shared.join("value.yaml"), "second").unwrap();
		let second = Key::render("release", &chart, &options, None).unwrap();
		assert_ne!(first, second);
	}

	#[cfg(unix)]
	#[test]
	fn render_key_rejects_symlinks_to_non_regular_files() {
		use std::os::unix::fs::symlink;
		use std::os::unix::net::UnixListener;

		let temp = tempdir().unwrap();
		let chart = temp.path().join("chart");
		let socket = temp.path().join("template.sock");
		fs::create_dir_all(&chart).unwrap();
		let _listener = UnixListener::bind(&socket).unwrap();
		symlink(&socket, chart.join("linked.sock")).unwrap();
		let options = options(json!({ "calledFrom": temp.path().join("main.jsonnet") }));

		assert!(Key::render("release", &chart, &options, None).is_err());
	}

	#[cfg(unix)]
	#[test]
	fn canonicalizes_an_existing_symlink_before_a_missing_cache_leaf() {
		use std::os::unix::fs::symlink;

		let temp = tempdir().unwrap();
		let inside_chart = temp.path().join("chart/cache");
		fs::create_dir_all(&inside_chart).unwrap();
		symlink(&inside_chart, temp.path().join("target")).unwrap();

		assert_eq!(
			canonicalize_with_missing(&temp.path().join("target/helm")),
			inside_chart.canonicalize().unwrap().join("helm")
		);
	}

	#[test]
	fn disk_entries_round_trip_and_separate_helm_versions() {
		let temp = tempdir().unwrap();
		let called_from = temp.path().join("main.jsonnet");
		let directory = cache_directory(&called_from).unwrap();
		let key = Key([7; 32]);
		let first = json!({ "rendered": "first" });
		let second = json!({ "rendered": "second" });

		Cache::write_disk(key, &directory, b"helm-v1", &first);
		Cache::write_disk(key, &directory, b"helm-v2", &second);

		assert_eq!(Cache::read_disk(key, &directory, b"helm-v1"), Some(first));
		assert_eq!(Cache::read_disk(key, &directory, b"helm-v2"), Some(second));
	}

	#[test]
	fn corrupt_entries_are_misses_and_can_be_replaced() {
		let temp = tempdir().unwrap();
		let directory = temp.path().join("target/helm");
		let key = Key([8; 32]);
		let disk_key = key.disk(b"helm-v1");
		let path = directory
			.join(CACHE_VERSION_DIRECTORY)
			.join(disk_key.filename());
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(&path, b"not cbor").unwrap();

		assert_eq!(Cache::read_disk(key, &directory, b"helm-v1"), None);
		assert!(path.exists());

		let value = json!(["recovered"]);
		Cache::write_disk(key, &directory, b"helm-v1", &value);
		assert_eq!(Cache::read_disk(key, &directory, b"helm-v1"), Some(value));
	}

	#[test]
	fn valid_cbor_with_a_wrong_value_checksum_is_a_miss() {
		let temp = tempdir().unwrap();
		let directory = temp.path().join("target/helm");
		let key = Key([9; 32]);
		let disk_key = key.disk(b"helm-v1");
		let path = directory
			.join(CACHE_VERSION_DIRECTORY)
			.join(disk_key.filename());
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		let entry = Entry {
			schema: CACHE_SCHEMA,
			key: disk_key,
			value_checksum: Key([0; 32]),
			value: json!("altered"),
		};
		ciborium::into_writer(&entry, File::create(path).unwrap()).unwrap();

		assert_eq!(Cache::read_disk(key, &directory, b"helm-v1"), None);
	}

	#[cfg(unix)]
	#[test]
	fn symlinked_disk_entries_are_misses() {
		use std::os::unix::fs::symlink;

		let temp = tempdir().unwrap();
		let directory = temp.path().join("target/helm");
		let key = Key([10; 32]);
		Cache::write_disk(key, &directory, b"helm-v1", &json!("rendered"));
		let path = directory
			.join(CACHE_VERSION_DIRECTORY)
			.join(key.disk(b"helm-v1").filename());
		let target = temp.path().join("moved-entry.cbor");
		fs::rename(&path, &target).unwrap();
		symlink(target, path).unwrap();

		assert_eq!(Cache::read_disk(key, &directory, b"helm-v1"), None);
	}

	#[test]
	fn disk_write_failures_do_not_affect_the_rendered_value() {
		let temp = tempdir().unwrap();
		let blocked = temp.path().join("not-a-directory");
		fs::write(&blocked, "file").unwrap();

		Cache::write_disk(Key([11; 32]), &blocked, b"helm-v1", &json!(1));
	}

	#[test]
	fn computation_lock_coalesces_parallel_misses() {
		let cache = Arc::new(Cache::new(None));
		let renders = Arc::new(AtomicUsize::new(0));
		let key = Key([12; 32]);
		let threads = (0..8)
			.map(|_| {
				let cache = Arc::clone(&cache);
				let renders = Arc::clone(&renders);
				thread::spawn(move || {
					if cache.get(key).is_some() {
						return;
					}
					let computation = cache.computation(key);
					let _guard = computation.lock().unwrap();
					if cache.get(key).is_none() {
						renders.fetch_add(1, Ordering::SeqCst);
						thread::sleep(Duration::from_millis(10));
						cache.insert(key, json!("rendered"));
					}
				})
			})
			.collect::<Vec<_>>();
		for thread in threads {
			thread.join().unwrap();
		}

		assert_eq!(renders.load(Ordering::SeqCst), 1);
	}
}
