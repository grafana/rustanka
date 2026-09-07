use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rtk_environments::benchmarking::{Segment, sanitize, segment};

fn sanitizing_a_segment(c: &mut Criterion) {
	let mut group = c.benchmark_group("sanitize_segment");

	for parts in [1usize, 5, 25] {
		for (kind, label) in [
			(Segment::NeedingReplacement, "needing_replacement"),
			(Segment::CleanAscii, "clean_ascii"),
		] {
			let segment = segment(parts, kind);
			let id = format!("{label}/{parts}");
			group.bench_with_input(BenchmarkId::from_parameter(id), &segment, |b, input| {
				b.iter(|| black_box(sanitize(black_box(input))));
			});
		}
	}

	group.finish();
}

criterion_group!(benches, sanitizing_a_segment);
criterion_main!(benches);
