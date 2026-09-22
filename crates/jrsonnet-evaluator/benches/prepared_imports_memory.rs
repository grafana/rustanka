use std::alloc::System;

use jrsonnet_evaluator::PreparedImportCache;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

mod support;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn main() {
	for fields in [16, 256] {
		let library = support::Library::new(fields);
		for environments in [1, 20, 100] {
			for cached in [false, true] {
				let region = Region::new(GLOBAL);
				library.batch(environments, cached);
				let stats = region.change();
				println!(
					"fields={fields} envs={environments} cached={cached} allocated={} allocations={} freed={}",
					stats.bytes_allocated, stats.allocations, stats.bytes_deallocated
				);
			}
		}
		let region = Region::new(GLOBAL);
		let cache = PreparedImportCache::default();
		library.evaluate(Some(&cache), 0);
		let stats = region.change();
		println!(
			"fields={fields} retained_cache_bytes={}",
			stats.bytes_allocated as i128 - stats.bytes_deallocated as i128
		);
		drop(cache);
	}
}
