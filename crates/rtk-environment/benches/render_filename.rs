use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rtk_environments::benchmarking::{manifest, specialized};

fn rendering_a_filename(c: &mut Criterion) {
	let mut group = c.benchmark_group("render_filename");

	for size in [5usize, 25, 100] {
		let template = specialized(size);
		let manifest = manifest(size, size);
		group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
			b.iter(|| black_box(template.render(black_box(&manifest))));
		});
	}

	group.finish();
}

criterion_group!(benches, rendering_a_filename);
criterion_main!(benches);
