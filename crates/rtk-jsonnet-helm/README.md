# Experimental native Helm rendering

Try the Rust renderer with an existing Tanka environment:

```sh
RTK_HELM_RENDERER=rust cargo run -p rtk -- export /tmp/rtk-native-output path/to/environment
```

The normal backend remains Helm. Unset `RTK_HELM_RENDERER`, or set it to `helm`,
to use it. Other values are errors.

The Rust backend uses the released [`gtmpl-ng`](https://github.com/firstdorsal/gtmpl-rust)
for Go template syntax, serde-saphyr for YAML
reading, and rtk-yaml for `toYaml`. There is no Go code or Helm subprocess in
this backend. Identical renders share the engine's in-memory cache, including
across worker threads, under a key distinct from Helm's. Keys cover chart contents
and render options; failed renders are not cached. Native rendering bypasses the
disk cache even with `--helm-cache`, and never invokes Helm for cache metadata.
Set `RTK_HELM_DISABLE_MEMOIZATION=1` to render every call again.
No automatic fallback occurs on errors.

## Supported experiment

- Local directories and gzip chart archives, with v1/v2 application and library dependencies.
- Dependency aliases, globals, conditions, tags and imported values.
- Values schema validation (JSON Schema drafts 4, 6 and 7).
- Default values with recursive object overrides, array replacement and null deletion.
- `.Release`, `.Chart`, `.Values`, `.Template`, `.Subcharts`, `.Files` and `.Capabilities` context.
- File access, globbing, config/secret rendering and explicit API version additions.
- Named templates, `include`, `tpl`, conditionals, variables, pipelines and array iteration.
- Sprig functions from gtmpl's Helm feature, with Helm-compatible adapters for
  dictionaries, strings, JSON/YAML conversion, semantic versions and private keys.
- Offline `lookup` returns an empty dictionary, as `helm template` does.
- CRD inclusion, hook exclusion and Tanka manifest naming.
- Helm-compatible `.helmignore` rules and default exclusion of dotfiles in `templates/`.

Every call must provide a non-empty `namespace`. The prototype does not resolve
namespaces through kubeconfig. A project's `expectVersions.helm` is rejected
without invoking Helm: this renderer does not claim compatibility with a Helm
major version.

## Compatibility limits

This is a prototype, not a drop-in replacement for Helm. The test corpus is
small, and successful rendering does not establish parity for other charts.

Default capabilities are pinned to the checked-in Helm capability snapshot.
Live cluster lookups and unregistered functions are unavailable. Dictionary mutation
through aliases does not reproduce Go template reference semantics. Chart metadata is
exposed with its top-level Go-style field names; nested metadata is not fully
modeled.

Go formatting, missing values,
YAML formatting and conversion error behavior are not fully compatible. In
particular, `fromYaml` accepts mappings only and conversion failures are errors.
`until` is limited to one million elements.
Manifest ordering does not reproduce Helm's kind/hook ordering; avoid resources
that produce duplicate Tanka manifest keys. `include` and `tpl` reparse the chart
and have a 64-call nesting limit. No performance improvement is claimed.

## Validation

The checked-in golden output was produced by Helm v4.2.4:

```sh
helm template example crates/rtk-jsonnet-helm/testdata/native/chart \
  --namespace testing --include-crds \
  --values crates/rtk-jsonnet-helm/testdata/native/values.json \
  > crates/rtk-jsonnet-helm/testdata/native/helm.golden.yaml
cargo test -p rtk-jsonnet-helm
cargo test -p rtk-jsonnet-helm differential_existing_charts -- --ignored
cargo test -p rtk --test helm_cache_test native_renderer_exports_without_helm_or_cache_entries
```

The differential test needs Helm and compares four existing fixture charts with
both CRD settings and both hook settings. The CLI test uses an invalid Helm path,
enables the disk cache, and checks byte-for-byte equality with the existing
`helm_template_env` golden exports while requiring that no cache entries appear.

## Native renderer benchmark

`rtk-benchmarks/helm-template-native.yaml` runs the same heavy chart and 60 inline
environments as the Helm Template benchmark, with `RTK_HELM_RENDERER=rust` applied
to validation and timed commands. Older base binaries ignore the variable and
continue using Helm; tk also continues using Helm. Both rtk renderers now reuse
identical renders in memory.

```sh
uv run rtk-benchmarks/run-benchmark.py rtk-benchmarks/helm-template-native.yaml \
  --rtk-binary-path target/release/rtk --rtk-base-binary-path /path/to/base/rtk
```

Function-level timing and allocation benchmarks compare uncached and memoized
batches, including the first cold render, using the same chart with 2,000 hash
rounds:

```sh
cargo bench -p rtk-jsonnet-helm --features benchmarking --bench native_render
cargo bench -p rtk-jsonnet-helm --features benchmarking --bench native_render_memory
```
