use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use rtk::benchmarking::{
	build_sanitize_path_component_input_kind, internal_bench::sanitize_path_component,
	SanitizeInputKind,
};

fn bench_sanitize_path_component(c: &mut Criterion) {
	let mut group = c.benchmark_group("sanitize_path_component");
	for component_count in [1usize, 5, 25] {
		for (kind, kind_label) in [
			(SanitizeInputKind::WithReplacements, "with_replacements"),
			(SanitizeInputKind::CleanAscii, "clean_ascii"),
		] {
			let component = build_sanitize_path_component_input_kind(component_count, kind);
			let id = format!("{kind_label}/{component_count}");
			group.bench_with_input(BenchmarkId::from_parameter(id), &component, |b, input| {
				b.iter(|| {
					black_box(sanitize_path_component(black_box(input)));
				});
			});
		}
	}
	group.finish();
}

criterion_group!(benches, bench_sanitize_path_component);
criterion_main!(benches);
