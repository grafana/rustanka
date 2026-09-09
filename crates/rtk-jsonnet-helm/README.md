# Experimental native Helm rendering

Try the Rust renderer with an existing Tanka environment:

```sh
RTK_HELM_RENDERER=rust cargo run -p rtk -- export /tmp/rtk-native-output path/to/environment
```

The normal backend remains Helm. Unset `RTK_HELM_RENDERER`, or set it to `helm`,
to use it. Other values are errors.

The Rust backend uses a vendored `gtmpl-ng` with reassignment and range fixes
(see [patch notes](../../vendor/gtmpl-ng/RTK-PATCHES.md)) for Go template syntax, serde-saphyr for YAML
reading, and rtk-yaml for `toYaml`. There is no Go code or Helm subprocess in
this backend. Rendering bypasses both Helm caches, even with `--helm-cache`,
so it cannot reuse output from another renderer or invoke Helm for cache metadata.
No automatic fallback occurs on errors.

## Supported experiment

- Unpacked, local `apiVersion: v2` application charts.
- Default values with recursive object overrides, array replacement and null deletion.
- `.Release`, `.Chart`, `.Values` and `.Template` context.
- Named templates, `include`, `tpl`, conditionals, variables, pipelines and array iteration.
- `default`, `required`, `fail`, `empty`, `quote`, `upper`, `lower`, `trim`,
  `trimSuffix`, `trunc`, `indent`, `nindent`, `toYaml`, `fromYaml`, `toJson`,
  `fromJson`, `dict`, `list`, `sha256sum`, `int`, `until` and `add`, plus gtmpl's built-ins.
- CRD inclusion, hook exclusion and Tanka manifest naming.

Every call must provide a non-empty `namespace`. The prototype does not resolve
namespaces through kubeconfig. A project's `expectVersions.helm` is rejected
without invoking Helm: this renderer does not claim compatibility with a Helm
major version.

## Compatibility limits

This is a prototype, not a drop-in replacement for Helm. The test corpus is
small, and successful rendering does not establish parity for other charts.

Archives, subcharts, dependencies, library charts, `.helmignore`, schema
validation and explicit `apiVersions` are rejected. `.Capabilities`, `.Files`,
`lookup`, and unregistered Sprig functions are unavailable. Chart metadata is
exposed with its top-level Go-style field names; nested metadata is not fully
modeled.

Go formatting, missing values, JSON escaping,
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
continue using Helm; tk also continues using Helm. Native rendering currently
bypasses memoization, while the Helm-backed rtk path reuses its in-memory render.

```sh
uv run rtk-benchmarks/run-benchmark.py rtk-benchmarks/helm-template-native.yaml \
  --rtk-binary-path target/release/rtk --rtk-base-binary-path /path/to/base/rtk
```
