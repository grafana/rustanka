//! What rendering one manifest's filename allocates.
//!
//! Every exported manifest goes through this, so what it allocates is paid for
//! per resource rather than per environment.

use std::alloc::System;
use std::hint::black_box;

use rtk_environments::benchmarking::{manifest, specialized};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const ITERATIONS: usize = 10_000;

#[expect(
	clippy::cast_precision_loss,
	reason = "an average of counters that never come close to the mantissa"
)]
fn measure(size: usize) {
	let template = specialized(size);
	let manifest = manifest(size, size);
	let mut allocated = 0usize;
	let mut allocations = 0usize;

	for _ in 0..ITERATIONS {
		let region = Region::new(GLOBAL);
		let rendered = template.render(&manifest);
		let stats = region.change();
		black_box(rendered);

		allocated += stats.bytes_allocated;
		allocations += stats.allocations;
	}

	let bytes = allocated as f64 / ITERATIONS as f64;
	let count = allocations as f64 / ITERATIONS as f64;
	println!("  size={size:>4} bytes={bytes:>10.2} allocations={count:>8.2}");
}

fn main() {
	println!("render_filename, per manifest:");
	for size in [5usize, 25, 100] {
		measure(size);
	}
}
