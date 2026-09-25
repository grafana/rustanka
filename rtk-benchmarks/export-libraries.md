# Shared-library export benchmark

`export-libraries.yaml` isolates repeated library preparation across independent
evaluations. It complements the small-library, runtime-heavy `export-large`
benchmark; it is synthetic, not a prediction of production export speedups.

The generator writes eight Jsonnet libraries with 512 constructors each and an
8,192-record JSON inventory. These are literal source definitions, not Jsonnet
comprehensions: the benchmark needs substantial source to parse and analyze,
rather than substantial computation to build the library at runtime.

Each of 64 static environments imports every library and the inventory, selects
different constructors and records, and emits eight ConfigMaps. This models
using a small portion of a large shared API or inventory. Generation happens once
outside timing, in the runner's temporary directory; no private deployment data
or downloads are required.

Three cases distinguish cold preparation, reuse within one worker, and reuse
across workers:

- One environment, one worker: each timed process prepares the libraries cold.
- 64 environments, one worker: preparation can be reused for subsequent
  environments in the same process.
- 64 environments, eight workers: serialized prepared imports are shared by
  worker threads in the same process.

Every timed command starts a fresh process. Filesystem caches may be warm, but
the prepared-import cache starts cold and disappears when that process exits.
Output directories are cleared outside timing. The runner checks exported filenames
and contents against Tanka byte for byte before timing, including
`manifest.json`. Timings compare current rtk, Tanka, and the base binary when
supplied.

Run from the repository root with release binaries built before and after the
cache change:

```sh
uv run rtk-benchmarks/run-benchmark.py rtk-benchmarks/export-libraries.yaml \
  --rtk-binary-path /path/to/rtk-current \
  --rtk-base-binary-path /path/to/rtk-base -- --runs 5
```

Use a disk-backed `TMPDIR` when comparing export timings. The benchmark reports
wall time, not memory.
