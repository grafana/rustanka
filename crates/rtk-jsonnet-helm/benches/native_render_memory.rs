use std::alloc::System;
use std::hint::black_box;

use rtk_jsonnet_helm::benchmarking::NativeRenderer;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn main() {
	for calls in [8, 60] {
		for (mode, memoization) in [("uncached", false), ("memoized", true)] {
			let region = Region::new(GLOBAL);
			let renderer = NativeRenderer::new(memoization);
			for _ in 0..calls {
				black_box(renderer.render());
			}
			let stats = region.change();
			println!(
				"{mode}/{calls}: allocated={} allocations={} retained={}",
				stats.bytes_allocated,
				stats.allocations,
				stats
					.bytes_allocated
					.saturating_sub(stats.bytes_deallocated)
			);
		}
	}
}
