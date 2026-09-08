# rtk-yaml

YAML writing for Rustanka, preserving Tanka's go-yaml v2/v3 formatting.
Reading is handled by the upstream `serde-saphyr` crate.

The serializer is derived from grafana/serde-saphyr at
`0feecc80245a415ae6f7760d07fc6124a682c949` (MIT), with standalone scalar
quoting from `a26710c8e7ff5b0fa1f15131436d9917fe175313`.
Natural key ordering uses Rustanka’s existing go-yaml-compatible comparator.

Use `to_fmt_writer_with_options` for explicit go-yaml formatting options.
`compare_string_keys` provides go-yaml natural key ordering.
