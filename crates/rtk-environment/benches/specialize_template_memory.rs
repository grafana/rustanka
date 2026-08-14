//! What baking an environment's values into a filename template allocates.

use std::alloc::System;
use std::hint::black_box;

use rtk_environments::benchmarking::{format_referring_to_labels, specialize};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const ITERATIONS: usize = 1_000;

#[expect(
	clippy::cast_precision_loss,
	reason = "an average of counters that never come close to the mantissa"
)]
fn measure(labels: usize) {
	let format = format_referring_to_labels(labels);
	let mut allocated = 0usize;
	let mut allocations = 0usize;

	for _ in 0..ITERATIONS {
		let region = Region::new(GLOBAL);
		let specialized = specialize(&format, labels);
		let stats = region.change();
		black_box(specialized);

		allocated += stats.bytes_allocated;
		allocations += stats.allocations;
	}

	let bytes = allocated as f64 / ITERATIONS as f64;
	let count = allocations as f64 / ITERATIONS as f64;
	println!("  labels={labels:>4} bytes={bytes:>10.2} allocations={count:>8.2}");
}

fn main() {
	println!("specialize_template, per call:");
	for labels in [5usize, 25, 100] {
		measure(labels);
	}
}
