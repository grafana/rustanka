use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use rtk::benchmarking::{
	build_specialize_template_input, internal_bench::specialize_template_for_env,
};

fn bench_specialize_template_for_env(c: &mut Criterion) {
	let mut group = c.benchmark_group("export_specialize_template_for_env");
	for label_count in [5usize, 25, 100] {
		let (template, env) = build_specialize_template_input(label_count);
		group.bench_with_input(
			BenchmarkId::from_parameter(label_count),
			&label_count,
			|b, _| {
				b.iter(|| {
					black_box(
						specialize_template_for_env(black_box(&template), black_box(&env))
							.expect("template specialization should succeed"),
					)
				});
			},
		);
	}
	group.finish();
}

criterion_group!(benches, bench_specialize_template_for_env);
criterion_main!(benches);
