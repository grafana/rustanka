use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rtk_jsonnet_helm::benchmarking::NativeRenderer;

fn native_render(c: &mut Criterion) {
	assert_eq!(
		NativeRenderer::new(false).render(),
		NativeRenderer::new(true).render()
	);
	let mut group = c.benchmark_group("native_render");
	group
		.sample_size(10)
		.measurement_time(Duration::from_secs(5));
	for calls in [8, 60] {
		for (mode, memoization) in [("uncached", false), ("memoized", true)] {
			group.bench_with_input(BenchmarkId::new(mode, calls), &calls, |b, &calls| {
				b.iter(|| {
					// Each batch includes its cold render and cache ownership costs.
					let renderer = NativeRenderer::new(memoization);
					for _ in 0..calls {
						black_box(renderer.render());
					}
				});
			});
		}
	}
	group.finish();
}

criterion_group!(benches, native_render);
criterion_main!(benches);
