use std::{
	collections::VecDeque,
	sync::{Arc, LazyLock, Mutex},
};

use bincode::Options as _;
use jrsonnet_ir::{Source, SourceFile, SourcePath, SourceVirtual, with_span_source};
use rustc_hash::FxHashMap;
use sha2::{Digest as _, Sha256};

use crate::{
	IStr,
	analyze::{LExpr, LocalId},
};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_ENTRY: usize = 8 * 1024 * 1024;

static SHARED: LazyLock<Mutex<SharedCache>> = LazyLock::new(|| Mutex::new(SharedCache::default()));

#[derive(Default)]
struct SharedCache {
	entries: FxHashMap<[u8; 32], Arc<[u8]>>,
	order: VecDeque<[u8; 32]>,
	bytes: usize,
}

impl SharedCache {
	fn get(key: &[u8; 32]) -> Option<Arc<[u8]>> {
		SHARED.lock().ok()?.entries.get(key).cloned()
	}

	fn insert(key: [u8; 32], bytes: Vec<u8>) {
		if bytes.len() > MAX_ENTRY {
			return;
		}
		let Ok(mut cache) = SHARED.lock() else {
			return;
		};
		if cache.entries.contains_key(&key) {
			return;
		}
		while cache.entries.len() >= MAX_ENTRIES || cache.bytes + bytes.len() > MAX_BYTES {
			let Some(old_key) = cache.order.pop_front() else {
				return;
			};
			let old = cache.entries.remove(&old_key).expect("queued entry exists");
			cache.bytes -= old.len();
		}
		cache.bytes += bytes.len();
		cache.order.push_back(key);
		cache.entries.insert(key, bytes.into());
	}
}

pub(super) fn get(
	path: &SourcePath,
	code: &IStr,
	externals: &[(IStr, LocalId)],
	source: Source,
) -> Option<LExpr> {
	let bytes = SharedCache::get(&key(path, code, externals)?)?;
	with_span_source(source, || {
		bincode::options()
			.with_limit(MAX_ENTRY as u64)
			.deserialize(&bytes)
	})
	.ok()
}

pub(super) fn insert(
	path: &SourcePath,
	code: &IStr,
	externals: &[(IStr, LocalId)],
	source: Source,
	lir: &LExpr,
) {
	let Some(key) = key(path, code, externals) else {
		return;
	};
	if SharedCache::get(&key).is_some() {
		return;
	}
	if let Ok(bytes) = with_span_source(source, || bincode::options().serialize(lir)) {
		SharedCache::insert(key, bytes);
	}
}

fn key(path: &SourcePath, code: &IStr, externals: &[(IStr, LocalId)]) -> Option<[u8; 32]> {
	let mut hash = Sha256::new();
	if let Some(file) = path.downcast_ref::<SourceFile>() {
		hash.update(b"file:");
		hash.update(file.path().to_str()?.as_bytes());
	} else if let Some(virtual_path) = path.downcast_ref::<SourceVirtual>() {
		hash.update(b"virtual:");
		hash.update(virtual_path.0.as_bytes());
	} else {
		return None;
	}
	hash.update((code.len() as u64).to_le_bytes());
	hash.update(code.as_bytes());
	for (name, id) in externals {
		hash.update((name.len() as u64).to_le_bytes());
		hash.update(name.as_bytes());
		hash.update(id.0.to_le_bytes());
	}
	Some(hash.finalize().into())
}
