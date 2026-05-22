use std::alloc::System;

use rtk::benchmarking::{build_render_filename_input, internal_bench::render_filename_simple};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn run_case(size: usize, iterations: usize) {
	let (template, manifest, env) = build_render_filename_input(size, size);

	let mut total_bytes = 0usize;
	let mut total_allocations = 0usize;
	let mut total_reallocations = 0usize;
	let mut total_deallocations = 0usize;

	for _ in 0..iterations {
		let region = Region::new(GLOBAL);
		let rendered = render_filename_simple(&template, &manifest, &env)
			.expect("render_filename_simple should succeed");
		let stats = region.change();
		std::hint::black_box(rendered);

		total_bytes += stats.bytes_allocated as usize;
		total_allocations += stats.allocations as usize;
		total_reallocations += stats.reallocations as usize;
		total_deallocations += stats.deallocations as usize;
	}

	println!(
		"size={size:>3} iters={iterations:>5} avg_bytes={:>8.1} avg_allocs={:>6.2} avg_reallocs={:>6.2} avg_deallocs={:>6.2}",
		total_bytes as f64 / iterations as f64,
		total_allocations as f64 / iterations as f64,
		total_reallocations as f64 / iterations as f64,
		total_deallocations as f64 / iterations as f64,
	);
}

fn main() {
	println!("memory benchmark: render_filename_simple");
	run_case(5, 5_000);
	run_case(25, 5_000);
	run_case(100, 5_000);
}
