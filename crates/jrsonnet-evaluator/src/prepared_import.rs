use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use jrsonnet_gcmodule::Acyclic;
use rustc_hash::FxHashMap;

use crate::{
	IStr, SourcePath,
	analyze::{LExpr, LocalId},
};

/// Reusable import code, without evaluated values or environment bindings.
///
/// Clones share a thread-bound cache. Each state still loads files and creates
/// its own context; reuse requires the same resolved path, contents and ordered
/// root binding names/slots. Changed files replace the previous entry.
///
/// FIFO eviction bounds retention to 4096 files and 32 MiB of source text.
/// Lowered code and map overhead consume additional memory. Oversized files are
/// evaluated without shared retention, and dropping all clones frees the cache.
#[derive(Clone, Default, Acyclic)]
pub struct PreparedImportCache(Rc<RefCell<Cache>>);

#[derive(Acyclic)]
struct Entry {
	code: IStr,
	externals: Vec<(IStr, LocalId)>,
	lir: Rc<LExpr>,
}

#[derive(Acyclic)]
struct Cache {
	entries: FxHashMap<SourcePath, Entry>,
	order: VecDeque<SourcePath>,
	source_bytes: usize,
	max_entries: usize,
	max_source_bytes: usize,
}

impl Default for Cache {
	fn default() -> Self {
		Self {
			entries: FxHashMap::default(),
			order: VecDeque::new(),
			source_bytes: 0,
			max_entries: 4096,
			max_source_bytes: 32 * 1024 * 1024,
		}
	}
}

impl PreparedImportCache {
	pub(crate) fn get(
		&self,
		path: &SourcePath,
		code: &IStr,
		externals: &[(IStr, LocalId)],
	) -> Option<Rc<LExpr>> {
		let cache = self.0.borrow();
		let entry = cache.entries.get(path)?;
		(entry.code == *code && entry.externals == externals).then(|| entry.lir.clone())
	}

	pub(crate) fn insert(
		&self,
		path: SourcePath,
		code: IStr,
		externals: Vec<(IStr, LocalId)>,
		lir: Rc<LExpr>,
	) {
		let mut cache = self.0.borrow_mut();
		if cache.max_entries == 0 || code.len() > cache.max_source_bytes {
			return;
		}
		// Replacing an edit keeps its FIFO position so changing files cannot
		// accumulate obsolete code or duplicate eviction records.
		if let Some(old) = cache.entries.remove(&path) {
			cache.source_bytes -= old.code.len();
		} else {
			cache.order.push_back(path.clone());
		}
		cache.source_bytes += code.len();
		cache.entries.insert(
			path,
			Entry {
				code,
				externals,
				lir,
			},
		);
		while cache.entries.len() > cache.max_entries || cache.source_bytes > cache.max_source_bytes
		{
			let path = cache.order.pop_front().expect("cache is nonempty");
			let entry = cache
				.entries
				.remove(&path)
				.expect("queued cache entry exists");
			cache.source_bytes -= entry.code.len();
		}
	}
}

#[cfg(test)]
mod tests;
