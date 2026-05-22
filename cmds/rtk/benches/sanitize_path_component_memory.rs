use rtk::benchmarking::{
	build_sanitize_path_component_input_kind, internal_bench::sanitize_path_component,
	SanitizeInputKind,
};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};
use std::alloc::System;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn run_case(component_count: usize, kind: SanitizeInputKind, label: &str, iterations: usize) {
	let component = build_sanitize_path_component_input_kind(component_count, kind);

	let mut total_bytes = 0usize;
	let mut total_allocations = 0usize;
	let mut total_reallocations = 0usize;
	let mut total_deallocations = 0usize;

	for _ in 0..iterations {
		let region = Region::new(GLOBAL);
		let sanitized = sanitize_path_component(&component);
		let stats = region.change();
		std::hint::black_box(sanitized);

		total_bytes += stats.bytes_allocated as usize;
		total_allocations += stats.allocations as usize;
		total_reallocations += stats.reallocations as usize;
		total_deallocations += stats.deallocations as usize;
	}

	println!(
		"kind={label:>18} components={component_count:>3} iters={iterations:>5} avg_bytes={:>8.2} avg_allocs={:>6.2} avg_reallocs={:>6.2} avg_deallocs={:>6.2}",
		total_bytes as f64 / iterations as f64,
		total_allocations as f64 / iterations as f64,
		total_reallocations as f64 / iterations as f64,
		total_deallocations as f64 / iterations as f64,
	);
}

fn main() {
	println!("memory benchmark: sanitize_path_component");
	for component_count in [1usize, 5, 25] {
		run_case(
			component_count,
			SanitizeInputKind::WithReplacements,
			"with_replacements",
			10_000,
		);
		run_case(
			component_count,
			SanitizeInputKind::CleanAscii,
			"clean_ascii",
			10_000,
		);
	}
}
