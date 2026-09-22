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

Three cases distinguish cold costs from reuse:

- One environment, one worker: library preparation cannot be reused across
  environments.
- 64 environments, one worker: preparation can be reused for subsequent
  environments in the same process.
- 64 environments, eight workers: each worker prepares its own libraries and
  can reuse them across its environments.

Every timed command starts a fresh process. Filesystem caches may be warm, but
the prepared-import cache starts empty. Output directories are cleared outside
timing. With a base binary, the runner checks exported filenames and contents
byte for byte before timing. Without one, it only checks command success.

Run from the repository root with release binaries built before and after the
cache change:

```sh
uv run rtk-benchmarks/run-benchmark.py rtk-benchmarks/export-libraries.yaml \
  --rtk-binary-path /path/to/rtk-current \
  --rtk-base-binary-path /path/to/rtk-base -- --runs 5
```

Use a disk-backed `TMPDIR` when comparing export timings. The benchmark reports
wall time, not memory; the cold case is retained even if it shows no improvement
or a regression.
