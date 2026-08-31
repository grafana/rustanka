//! What sanitizing a path segment allocates.
//!
//! A segment that needs nothing done to it should borrow rather than allocate,
//! which is the thing worth watching here.

use std::alloc::System;
use std::hint::black_box;

use rtk_environments::benchmarking::{Segment, sanitize, segment};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const ITERATIONS: usize = 10_000;

#[expect(
	clippy::cast_precision_loss,
	reason = "an average of counters that never come close to the mantissa"
)]
fn measure(parts: usize, kind: Segment, label: &str) {
	let segment = segment(parts, kind);
	let mut allocated = 0usize;
	let mut allocations = 0usize;

	for _ in 0..ITERATIONS {
		let region = Region::new(GLOBAL);
		let sanitized = sanitize(&segment);
		let stats = region.change();
		black_box(sanitized);

		allocated += stats.bytes_allocated;
		allocations += stats.allocations;
	}

	let bytes = allocated as f64 / ITERATIONS as f64;
	let count = allocations as f64 / ITERATIONS as f64;
	println!("  {label:>20} parts={parts:>3} bytes={bytes:>8.2} allocations={count:>6.2}");
}

fn main() {
	println!("sanitize_segment, per call:");
	for parts in [1usize, 5, 25] {
		measure(parts, Segment::NeedingReplacement, "needing_replacement");
		measure(parts, Segment::CleanAscii, "clean_ascii");
	}
}
