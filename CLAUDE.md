# Claude Agent Notes

Project-specific context for AI agents working on rustanka (rtk).

## Project Overview

rustanka/rtk is a Rust implementation aiming to be a drop-in replacement for [Tanka](https://github.com/grafana/tanka) (tk). The primary goal is **exact output compatibility with Tanka**.

## Key Dependencies

### serde-saphyr (YAML Serialization)

- Used for YAML output generation
- A local clone may exist alongside this repo for development - check with user if modifications are needed for Go yaml.v3 compatibility
- Workspace `Cargo.toml` is the source of truth for this dependency
- If adding serde-saphyr to a new crate, use `serde-saphyr.workspace = true`

## YAML Export Behavior

The rtk export should produce **byte-for-byte identical output** to Tanka where possible. When debugging mismatches, compare against actual Tanka output to identify the difference.

## Testing

### Test Priority

**The tk golden tests are the source of truth.** When fixing issues:

1. **Golden tests (tk output) must pass first** - These represent real Tanka behavior
2. **Never remove test cases** because they're hard to fix - rtk MUST match tk output
3. **Adapt other tests afterwards** - If serde-saphyr or other internal tests conflict with tk behavior, update those tests to match tk's expected behavior

### Golden Tests

- Located in `test_fixtures/golden_envs/`
- Each env has a `golden/` subdirectory with expected output
- Run specific golden tests: `cargo test -p rtk --test golden_fixtures_test`

### Debugging Output Mismatches

Golden fixtures are generated from **tk** (real Tanka), and the test verifies that **rtk** produces identical output.

When investigating rtk vs tk differences:
1. Reproduce the issue in a golden test by adding a test case to `test_fixtures/golden_envs/`
2. Run `make update-golden-fixtures` to regenerate golden files using tk
3. Run `make test` to verify the test fails (showing rtk doesn't match tk)
4. Fix the issue in rtk code (may require serde-saphyr changes)
5. Run `make test` to verify rtk now matches tk output
6. Update any serde-saphyr internal tests that now fail to match the new (correct) behavior

### Running All Tests

```bash
make test
```

## Common Issues

### Config hash differences in comparisons

When comparing rtk vs tk output, config hash differences (e.g., `mimir-config-exporter-hash`, `envoy-hash`) can generally be ignored. These are derived hashes of other resources (typically ConfigMaps), so they differ only because the underlying ConfigMap content differs.

### Two versions of serde-saphyr compiling

If you see both local and git versions compiling, ensure all crates use `serde-saphyr.workspace = true` instead of direct git references.
