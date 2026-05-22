use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use rtk::benchmarking::{build_render_filename_input, internal_bench::render_filename_simple};

fn bench_render_filename_simple(c: &mut Criterion) {
	let mut group = c.benchmark_group("export_render_filename_simple");
	for size in [5usize, 25, 100] {
		let (template, manifest, env) = build_render_filename_input(size, size);
		group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
			b.iter(|| {
				black_box(
					render_filename_simple(
						black_box(&template),
						black_box(&manifest),
						black_box(&env),
					)
					.expect("render_filename_simple should succeed"),
				)
			});
		});
	}
	group.finish();
}

criterion_group!(benches, bench_render_filename_simple);
criterion_main!(benches);
