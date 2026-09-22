use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

mod support;

fn prepared_imports(c: &mut Criterion) {
	let mut group = c.benchmark_group("prepared_imports");
	group.sample_size(20);
	group.warm_up_time(Duration::from_millis(500));
	group.measurement_time(Duration::from_secs(2));
	for fields in [16, 256] {
		let library = support::Library::new(fields);
		for environments in [1, 20, 100] {
			for cached in [false, true] {
				let mode = if cached { "cached" } else { "uncached" };
				group.bench_function(
					BenchmarkId::new(mode, format!("{fields}-fields/{environments}-envs")),
					|b| b.iter(|| library.batch(environments, cached)),
				);
			}
		}
	}
	group.finish();
}

criterion_group!(benches, prepared_imports);
criterion_main!(benches);
