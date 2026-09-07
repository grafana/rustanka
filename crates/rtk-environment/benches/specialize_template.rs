use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rtk_environments::benchmarking::{format_referring_to_labels, specialize};

fn specializing_a_template(c: &mut Criterion) {
	let mut group = c.benchmark_group("specialize_template");

	for labels in [5usize, 25, 100] {
		let format = format_referring_to_labels(labels);
		group.bench_with_input(BenchmarkId::from_parameter(labels), &labels, |b, _| {
			b.iter(|| black_box(specialize(black_box(&format), black_box(labels))));
		});
	}

	group.finish();
}

criterion_group!(benches, specializing_a_template);
criterion_main!(benches);
