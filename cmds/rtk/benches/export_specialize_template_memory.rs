use rtk::benchmarking::{
	build_specialize_template_input, internal_bench::specialize_template_for_env,
};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};
use std::alloc::System;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn run_case(label_count: usize, iterations: usize) {
	let (template, env) = build_specialize_template_input(label_count);

	let mut total_bytes = 0usize;
	let mut total_allocations = 0usize;
	let mut total_reallocations = 0usize;
	let mut total_deallocations = 0usize;

	for _ in 0..iterations {
		let region = Region::new(GLOBAL);
		let rendered = specialize_template_for_env(&template, &env)
			.expect("template specialization should succeed");
		let stats = region.change();
		std::hint::black_box(rendered);

		total_bytes += stats.bytes_allocated as usize;
		total_allocations += stats.allocations as usize;
		total_reallocations += stats.reallocations as usize;
		total_deallocations += stats.deallocations as usize;
	}

	println!(
		"labels={label_count:>3} iters={iterations:>5} avg_bytes={:>8.1} avg_allocs={:>6.2} avg_reallocs={:>6.2} avg_deallocs={:>6.2}",
		total_bytes as f64 / iterations as f64,
		total_allocations as f64 / iterations as f64,
		total_reallocations as f64 / iterations as f64,
		total_deallocations as f64 / iterations as f64,
	);
}

fn main() {
	println!("memory benchmark: specialize_template_for_env");
	run_case(5, 5_000);
	run_case(25, 5_000);
	run_case(100, 5_000);
}
