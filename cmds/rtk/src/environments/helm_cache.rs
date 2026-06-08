//! On-disk / remote `helmTemplate` cache for exports (experimental).
//!
//! The export driver maintains a single global cache location that stores the
//! rendered output of every `helmTemplate` call as one JSON document per call:
//!
//! ```text
//! <location>/<sha256>.json
//! ```
//!
//! where `<sha256>` is a hash of the entire `helmTemplate` call (release name,
//! chart path + `Chart.yaml`, namespace, values, flags, `nameFormat`, ...) and
//! the JSON file contains the map of all resources that call produced.
//!
//! The cache location can be either a local directory or an S3 prefix, selected
//! via [`parse_location`]:
//!
//! - `file:///abs/path` or a bare path -> [`CacheLocation::Local`]
//! - `s3://bucket/prefix`              -> [`CacheLocation::S3`]
//!
//! The cache is global and is loaded and written exactly once per export run:
//!
//! 1. Before the parallel export loop, [`load_and_clear`] reads every cached
//!    entry into the process-global in-memory Helm cache (shared across all
//!    worker threads, so a chart rendered for one environment is reused by
//!    every other) and begins recording which cache keys get touched. For the
//!    local backend it then deletes the directory so stale entries are pruned.
//! 2. During evaluation, each `helmTemplate` call that hits the in-memory cache
//!    is served without invoking helm or parsing YAML, and its key is recorded
//!    as "touched" (across all threads).
//! 3. After the parallel loop completes, [`save`] writes only the touched
//!    entries back and prunes entries that were not referenced this run.

use std::{
	collections::HashSet,
	path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use tracing::{debug, warn};

use crate::jsonnet::evaluator::jrsonnet::builtins;

/// Name of the metadata directory created inside the export output directory.
pub const HELM_CACHE_DIR: &str = "helm-cache";

/// Where the helmTemplate cache is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLocation {
	/// A local directory of `<sha>.json` files.
	Local(PathBuf),
	/// An S3 bucket + key prefix. `prefix` is empty or ends with `/`.
	S3 { bucket: String, prefix: String },
}

/// What to do when a cache load/save operation fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnError {
	/// Log a warning and continue the export.
	#[default]
	Warn,
	/// Abort the export with an error.
	Fail,
}

impl std::str::FromStr for OnError {
	type Err = anyhow::Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"warn" => Ok(Self::Warn),
			"fail" => Ok(Self::Fail),
			_ => bail!("invalid helm-cache-on-error value `{s}` (expected `warn` or `fail`)"),
		}
	}
}

/// Resolve the default local cache directory inside `output_dir`.
pub fn cache_dir(output_dir: &Path) -> PathBuf {
	output_dir.join(HELM_CACHE_DIR)
}

/// Parse a cache path into a [`CacheLocation`].
///
/// Supports `s3://bucket/prefix`, `file:///path`, and bare local paths.
pub fn parse_location(raw: &str) -> Result<CacheLocation> {
	if let Some(rest) = raw.strip_prefix("s3://") {
		let mut parts = rest.splitn(2, '/');
		let bucket = parts.next().unwrap_or("");
		if bucket.is_empty() {
			bail!("invalid s3 helm-cache path `{raw}`: missing bucket");
		}
		let prefix = normalize_prefix(parts.next().unwrap_or(""));
		Ok(CacheLocation::S3 {
			bucket: bucket.to_owned(),
			prefix,
		})
	} else if let Some(rest) = raw.strip_prefix("file://") {
		Ok(CacheLocation::Local(PathBuf::from(rest)))
	} else {
		Ok(CacheLocation::Local(PathBuf::from(raw)))
	}
}

/// Normalize an S3 key prefix so it is either empty or ends with a single `/`.
fn normalize_prefix(prefix: &str) -> String {
	let trimmed = prefix.trim_start_matches('/');
	if trimmed.is_empty() {
		String::new()
	} else if trimmed.ends_with('/') {
		trimmed.to_owned()
	} else {
		format!("{trimmed}/")
	}
}

/// Begin recording touched cache keys for the whole run and preload any
/// previously persisted entries into the global in-memory cache.
///
/// For the local backend the directory is then deleted so only entries touched
/// this run are written back. Must be called once, before the parallel export
/// loop.
pub fn load_and_clear(loc: &CacheLocation) -> Result<()> {
	builtins::helm_disk_cache_begin();

	match loc {
		CacheLocation::Local(dir) => load_and_clear_local(dir),
		CacheLocation::S3 { bucket, prefix } => load_s3(bucket, prefix),
	}
}

/// Stop recording and persist the entries touched during this run.
///
/// Stale entries that were not referenced this run are pruned. Must be called
/// once, after the parallel export loop completes (single-threaded), so writes
/// never race.
pub fn save(loc: &CacheLocation) -> Result<()> {
	let touched = builtins::helm_disk_cache_take();
	match loc {
		CacheLocation::Local(dir) => save_local(dir, &touched),
		CacheLocation::S3 { bucket, prefix } => save_s3(bucket, prefix, &touched),
	}
}

// --- Local backend -------------------------------------------------------

fn load_and_clear_local(dir: &Path) -> Result<()> {
	if !dir.exists() {
		return Ok(());
	}

	preload_local(dir).with_context(|| format!("preloading helm-cache from {}", dir.display()))?;

	std::fs::remove_dir_all(dir)
		.with_context(|| format!("clearing helm-cache dir {}", dir.display()))?;
	Ok(())
}

fn preload_local(dir: &Path) -> Result<()> {
	let mut loaded = 0usize;
	for entry in std::fs::read_dir(dir).context("reading helm-cache directory")? {
		let entry = entry.context("reading helm-cache entry")?;
		let path = entry.path();
		if path.extension().and_then(|e| e.to_str()) != Some("json") {
			continue;
		}
		let Some(key) = path.file_stem().and_then(|s| s.to_str()) else {
			continue;
		};
		match std::fs::read_to_string(&path) {
			Ok(json) => {
				builtins::helm_cache_put_json(key.to_owned(), json);
				loaded += 1;
			}
			Err(err) => warn!("helm-cache: failed to read {}: {err:#}", path.display()),
		}
	}
	debug!(
		"helm-cache: preloaded {loaded} entries from {}",
		dir.display()
	);
	Ok(())
}

fn save_local(dir: &Path, touched: &HashSet<String>) -> Result<()> {
	if touched.is_empty() {
		return Ok(());
	}

	std::fs::create_dir_all(dir)
		.with_context(|| format!("creating helm-cache dir {}", dir.display()))?;

	let mut written = 0usize;
	for key in touched {
		let Some(json) = builtins::helm_cache_get_json(key) else {
			// The entry was evicted or never stored; nothing to persist.
			continue;
		};
		let path = dir.join(format!("{key}.json"));
		std::fs::write(&path, json)
			.with_context(|| format!("writing helm-cache entry {}", path.display()))?;
		written += 1;
	}
	debug!("helm-cache: wrote {written} entries to {}", dir.display());
	Ok(())
}

// --- S3 backend ----------------------------------------------------------

const S3_SUFFIX: &str = ".json";

/// Build a current-thread tokio runtime for the blocking S3 calls. Cache
/// load/save run single-threaded outside the rayon export pool, so a private
/// runtime here never collides with one already running.
fn s3_runtime() -> Result<tokio::runtime::Runtime> {
	tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.context("creating tokio runtime for s3 helm-cache")
}

async fn s3_client() -> aws_sdk_s3::Client {
	let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
	aws_sdk_s3::Client::new(&config)
}

/// Convert an S3 object key back into the in-memory cache key (the sha stem).
fn s3_cache_key(prefix: &str, object_key: &str) -> Option<String> {
	object_key
		.strip_prefix(prefix)?
		.strip_suffix(S3_SUFFIX)
		.map(ToOwned::to_owned)
}

fn load_s3(bucket: &str, prefix: &str) -> Result<()> {
	let rt = s3_runtime()?;
	rt.block_on(async {
		let client = s3_client().await;
		let mut continuation: Option<String> = None;
		let mut loaded = 0usize;
		loop {
			let mut req = client.list_objects_v2().bucket(bucket).prefix(prefix);
			if let Some(token) = &continuation {
				req = req.continuation_token(token);
			}
			let resp = req
				.send()
				.await
				.with_context(|| format!("listing s3://{bucket}/{prefix}"))?;

			for obj in resp.contents() {
				let Some(object_key) = obj.key() else {
					continue;
				};
				if !object_key.ends_with(S3_SUFFIX) {
					continue;
				}
				let Some(cache_key) = s3_cache_key(prefix, object_key) else {
					continue;
				};
				let out = client
					.get_object()
					.bucket(bucket)
					.key(object_key)
					.send()
					.await
					.with_context(|| format!("getting s3://{bucket}/{object_key}"))?;
				let data = out
					.body
					.collect()
					.await
					.with_context(|| format!("reading s3://{bucket}/{object_key}"))?
					.into_bytes();
				let json = String::from_utf8(data.to_vec())
					.with_context(|| format!("s3://{bucket}/{object_key} is not valid utf-8"))?;
				builtins::helm_cache_put_json(cache_key, json);
				loaded += 1;
			}

			if resp.is_truncated().unwrap_or(false) {
				continuation = resp.next_continuation_token().map(ToOwned::to_owned);
				if continuation.is_none() {
					break;
				}
			} else {
				break;
			}
		}
		debug!("helm-cache: preloaded {loaded} entries from s3://{bucket}/{prefix}");
		Ok::<(), anyhow::Error>(())
	})
}

fn save_s3(bucket: &str, prefix: &str, touched: &HashSet<String>) -> Result<()> {
	let rt = s3_runtime()?;
	rt.block_on(async {
		let client = s3_client().await;

		// Write the entries touched this run.
		let mut written = 0usize;
		for key in touched {
			let Some(json) = builtins::helm_cache_get_json(key) else {
				continue;
			};
			let object_key = format!("{prefix}{key}{S3_SUFFIX}");
			client
				.put_object()
				.bucket(bucket)
				.key(&object_key)
				.body(aws_sdk_s3::primitives::ByteStream::from(json.into_bytes()))
				.send()
				.await
				.with_context(|| format!("writing s3://{bucket}/{object_key}"))?;
			written += 1;
		}

		// Prune entries that were not referenced this run.
		let mut continuation: Option<String> = None;
		let mut pruned = 0usize;
		loop {
			let mut req = client.list_objects_v2().bucket(bucket).prefix(prefix);
			if let Some(token) = &continuation {
				req = req.continuation_token(token);
			}
			let resp = req
				.send()
				.await
				.with_context(|| format!("listing s3://{bucket}/{prefix}"))?;

			for obj in resp.contents() {
				let Some(object_key) = obj.key() else {
					continue;
				};
				if !object_key.ends_with(S3_SUFFIX) {
					continue;
				}
				let Some(cache_key) = s3_cache_key(prefix, object_key) else {
					continue;
				};
				if touched.contains(&cache_key) {
					continue;
				}
				client
					.delete_object()
					.bucket(bucket)
					.key(object_key)
					.send()
					.await
					.with_context(|| format!("deleting s3://{bucket}/{object_key}"))?;
				pruned += 1;
			}

			if resp.is_truncated().unwrap_or(false) {
				continuation = resp.next_continuation_token().map(ToOwned::to_owned);
				if continuation.is_none() {
					break;
				}
			} else {
				break;
			}
		}
		debug!("helm-cache: wrote {written} and pruned {pruned} entries on s3://{bucket}/{prefix}");
		Ok::<(), anyhow::Error>(())
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::jsonnet::evaluator::jrsonnet::builtins::{
		helm_disk_cache_begin, helm_disk_cache_take, record_helm_disk_touch, HELM_CACHE_TEST_LOCK,
	};

	#[test]
	fn test_cache_dir() {
		assert_eq!(
			cache_dir(std::path::Path::new("/out")),
			std::path::PathBuf::from("/out/helm-cache"),
		);
	}

	#[test]
	fn test_parse_location_local() {
		assert_eq!(
			parse_location("/tmp/foo").unwrap(),
			CacheLocation::Local(PathBuf::from("/tmp/foo"))
		);
		assert_eq!(
			parse_location("file:///tmp/foo").unwrap(),
			CacheLocation::Local(PathBuf::from("/tmp/foo"))
		);
	}

	#[test]
	fn test_parse_location_s3() {
		assert_eq!(
			parse_location("s3://my-bucket/some/prefix").unwrap(),
			CacheLocation::S3 {
				bucket: "my-bucket".to_owned(),
				prefix: "some/prefix/".to_owned(),
			}
		);
		assert_eq!(
			parse_location("s3://my-bucket").unwrap(),
			CacheLocation::S3 {
				bucket: "my-bucket".to_owned(),
				prefix: String::new(),
			}
		);
		assert_eq!(
			parse_location("s3://my-bucket/already/slash/").unwrap(),
			CacheLocation::S3 {
				bucket: "my-bucket".to_owned(),
				prefix: "already/slash/".to_owned(),
			}
		);
		assert!(parse_location("s3:///no-bucket").is_err());
	}

	#[test]
	fn test_on_error_from_str() {
		assert_eq!("warn".parse::<OnError>().unwrap(), OnError::Warn);
		assert_eq!("fail".parse::<OnError>().unwrap(), OnError::Fail);
		assert!("nope".parse::<OnError>().is_err());
	}

	#[test]
	fn test_s3_cache_key() {
		assert_eq!(
			s3_cache_key("prefix/", "prefix/abc123.json").as_deref(),
			Some("abc123")
		);
		assert_eq!(s3_cache_key("prefix/", "other/abc123.json"), None);
		assert_eq!(s3_cache_key("", "abc123.json").as_deref(), Some("abc123"));
	}

	#[test]
	fn test_save_writes_only_touched_present_entries() {
		let _guard = HELM_CACHE_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let tmp = tempfile::tempdir().unwrap();
		let dir = tmp.path().join("helm-cache");

		// Unique keys so other tests' global-cache entries cannot interfere.
		let touched_present = "save_present_0001";
		let touched_absent = "save_absent_0001";

		builtins::helm_cache_put_json(touched_present.to_string(), "{\"k\":1}".to_string());

		helm_disk_cache_begin();
		record_helm_disk_touch(touched_present);
		// Touched but never stored in the cache: must be skipped, not error.
		record_helm_disk_touch(touched_absent);
		save(&CacheLocation::Local(dir.clone())).unwrap();

		let present_file = dir.join(format!("{touched_present}.json"));
		assert_eq!(std::fs::read_to_string(&present_file).unwrap(), "{\"k\":1}");
		assert!(!dir.join(format!("{touched_absent}.json")).exists());
	}

	#[test]
	fn test_save_empty_touched_creates_nothing() {
		let _guard = HELM_CACHE_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let tmp = tempfile::tempdir().unwrap();
		let dir = tmp.path().join("helm-cache");

		helm_disk_cache_begin();
		save(&CacheLocation::Local(dir.clone())).unwrap();

		assert!(!dir.exists());
	}

	#[test]
	fn test_load_and_clear_preloads_and_removes() {
		let _guard = HELM_CACHE_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let tmp = tempfile::tempdir().unwrap();
		let dir = tmp.path().join("helm-cache");
		std::fs::create_dir_all(&dir).unwrap();

		// A unique key not present in the global cache, plus a non-json file
		// that must be ignored.
		let key = "load_key_0001";
		std::fs::write(dir.join(format!("{key}.json")), "{\"loaded\":true}").unwrap();
		std::fs::write(dir.join("README.txt"), "ignore me").unwrap();
		assert_eq!(builtins::helm_cache_get_json(key), None);

		load_and_clear(&CacheLocation::Local(dir.clone())).unwrap();

		// Entry is now in the global in-memory cache, and the directory is gone.
		assert_eq!(
			builtins::helm_cache_get_json(key),
			Some("{\"loaded\":true}".to_string())
		);
		assert!(!dir.exists());

		// Recording was enabled by load_and_clear; clean it up for other tests.
		helm_disk_cache_take();
	}

	#[test]
	fn test_load_and_clear_missing_dir_is_ok() {
		let _guard = HELM_CACHE_TEST_LOCK
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let tmp = tempfile::tempdir().unwrap();
		let dir = tmp.path().join("does-not-exist");

		// Must not panic, and recording is still enabled.
		load_and_clear(&CacheLocation::Local(dir.clone())).unwrap();
		assert!(!dir.exists());

		helm_disk_cache_take();
	}
}
