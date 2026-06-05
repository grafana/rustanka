//! On-disk `helmTemplate` cache for exports (experimental).
//!
//! The export driver maintains a single global `helm-cache/` metadata directory
//! inside the export output directory. It stores the rendered output of every
//! `helmTemplate` call under:
//!
//! ```text
//! <output_dir>/helm-cache/<sha256>.json
//! ```
//!
//! where `<sha256>` is a hash of the entire `helmTemplate` call (release name,
//! chart path + `Chart.yaml`, namespace, values, flags, `nameFormat`, ...) and
//! the JSON file contains the map of all resources that call produced.
//!
//! The cache is global and is loaded and written exactly once per export run:
//!
//! 1. Before the parallel export loop, [`load_and_clear`] reads every cached
//!    entry into the process-global in-memory Helm cache (shared across all
//!    worker threads, so a chart rendered for one environment is reused by
//!    every other) and then deletes the directory. It also begins recording
//!    which cache keys get touched.
//! 2. During evaluation, each `helmTemplate` call that hits the in-memory cache
//!    is served without invoking helm or parsing YAML, and its key is recorded
//!    as "touched" (across all threads).
//! 3. After the parallel loop completes, [`save`] writes only the touched
//!    entries back. Stale entries that were not referenced this run are pruned,
//!    because the directory was deleted in step 1 and only touched keys are
//!    rewritten.

use std::{
	collections::HashSet,
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use tracing::{debug, warn};

use crate::jsonnet::evaluator::jrsonnet::builtins;

/// Name of the metadata directory created inside the export output directory.
pub const HELM_CACHE_DIR: &str = "helm-cache";

/// Resolve the single global cache directory inside `output_dir`.
pub fn cache_dir(output_dir: &Path) -> PathBuf {
	output_dir.join(HELM_CACHE_DIR)
}

/// Begin recording touched cache keys for the whole run, preload any previously
/// persisted entries into the global in-memory cache, then delete the directory
/// so only entries touched this run are written back.
///
/// Best-effort: a failure to read the existing cache is logged and ignored.
/// Must be called once, before the parallel export loop.
pub fn load_and_clear(dir: &Path) {
	builtins::helm_disk_cache_begin();

	if !dir.exists() {
		return;
	}

	if let Err(err) = preload(dir) {
		warn!("helm-cache: failed to preload {}: {err:#}", dir.display());
	}

	if let Err(err) = std::fs::remove_dir_all(dir) {
		warn!(
			"helm-cache: failed to clear {} (stale entries may persist): {err:#}",
			dir.display()
		);
	}
}

fn preload(dir: &Path) -> Result<()> {
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

/// Stop recording and persist the entries touched during this run to `dir`.
///
/// Best-effort: write failures are logged and ignored so caching never breaks
/// an otherwise successful export. Must be called once, after the parallel
/// export loop completes (single-threaded), so writes never race.
pub fn save(dir: &Path) {
	let touched: HashSet<String> = builtins::helm_disk_cache_take();
	if touched.is_empty() {
		return;
	}

	if let Err(err) = std::fs::create_dir_all(dir) {
		warn!(
			"helm-cache: failed to create {}: {err:#} (not caching)",
			dir.display()
		);
		return;
	}

	let mut written = 0usize;
	for key in &touched {
		let Some(json) = builtins::helm_cache_get_json(key) else {
			// The entry was evicted or never stored; nothing to persist.
			continue;
		};
		let path = dir.join(format!("{key}.json"));
		match std::fs::write(&path, json) {
			Ok(()) => written += 1,
			Err(err) => warn!("helm-cache: failed to write {}: {err:#}", path.display()),
		}
	}
	debug!("helm-cache: wrote {written} entries to {}", dir.display());
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
		save(&dir);

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
		save(&dir);

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

		load_and_clear(&dir);

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
		load_and_clear(&dir);
		assert!(!dir.exists());

		helm_disk_cache_take();
	}
}
