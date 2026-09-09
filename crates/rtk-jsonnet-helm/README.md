# Experimental native Helm rendering

Try the Rust renderer with an existing Tanka environment:

```sh
RTK_HELM_RENDERER=rust cargo run -p rtk -- export /tmp/rtk-native-output path/to/environment
```

The normal backend remains Helm. Unset `RTK_HELM_RENDERER`, or set it to `helm`,
to use it. Other values are errors.

The Rust backend uses `gtmpl-ng` for Go template syntax, serde-saphyr for YAML
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
  `fromJson`, `dict`, `list` and `sha256sum`, plus gtmpl's built-ins.
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

The template dependency has known semantic gaps, including unordered map
iteration and incorrect `range`/`else` behavior. Use array iteration without an
`else` branch for this experiment. Go formatting, missing values, JSON escaping,
YAML formatting and conversion error behavior are not fully compatible. In
particular, `fromYaml` accepts mappings only and conversion failures are errors.
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
