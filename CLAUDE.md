# Claude Agent Notes

Project-specific context for AI agents working on rustanka (rtk).

## Agent Behavior

- **Never run git commands** unless explicitly requested by the user
- **Always run `make fmt`** after making changes

## Project Overview

rustanka/rtk is a Rust implementation aiming to be a drop-in replacement for [Tanka](https://github.com/grafana/tanka) (tk). The primary goal is **exact output compatibility with Tanka**.

## Upstream jrsonnet Syncs

Use the `merge-upstream-jrsonnet` skill (`.claude/skills/merge-upstream-jrsonnet/`)
for merging in upstream jrsonnet. The last merged upstream commit is recorded in
`.jrsonnet-upstream-base`; the skill covers staged merging, the rustanka features
that must survive, and the signed-commit linearization the org requires.

## Key Dependencies

### serde-saphyr (YAML Serialization)

- Used for YAML output generation
- A local clone may exist alongside this repo for development - check with user if modifications are needed for Go yaml.v3 compatibility
- Workspace `Cargo.toml` is the source of truth for this dependency
- If adding serde-saphyr to a new crate, use `serde-saphyr.workspace = true`

## YAML Libraries in Tanka

**CRITICAL**: Tanka uses different YAML libraries for different operations:

| Operation | Go Library | Notes |
|-----------|-----------|-------|
| `std.native('manifestYamlFromJson')` | gopkg.in/yaml.v3 | |
| `std.manifestYamlDoc` | go-jsonnet built-in | Custom serializer in [builtins.go](https://github.com/google/go-jsonnet/blob/master/builtins.go) |
| **Manifest export** | gopkg.in/yaml.v2 | Main export output |
| `std.native('helmTemplate')` | gopkg.in/yaml.v3 | |

When implementing YAML serialization in serde-saphyr, **add parameters as needed** to support the different formatting behaviors required by each use case.

## YAML Export Behavior

The rtk export should produce **byte-for-byte identical output** to Tanka where possible. When debugging mismatches, compare against actual Tanka output to identify the difference.

### go-yaml v2 Line Wrapping (for exports)

- go-yaml v2.4.0 has line wrapping behavior controlled by `best_width`
- Line wrapping happens at space characters when `column > best_width`
- The condition also requires `!spaces` (previous char was not a space)
- This affects flow-style quoted scalars in YAML output

## Where Exporting Lives

`rtk export` is a thin wrapper (`cmds/rtk/src/commands/export.rs`) over the
exporter in `crates/rtk-environment` (package `rtk-environments`), which
evaluates through `crates/rtk-jsonnet`. The command translates arguments,
renders failures and picks an exit code; everything else — discovery,
evaluation, manifest processing, filenames, `manifest.json`, writing — belongs
to the crate, and so do the tests for it.

`cmds/rtk` no longer has an exporter of its own. It does still have its own
Jsonnet evaluator (`cmds/rtk/src/jsonnet/`) and spec types
(`cmds/rtk/src/spec.rs`), which `show`, `diff`, `apply`, `prune`, `env` and
`validate` use. Those are a second, older stack; moving them over is unfinished
work, so expect two of most things until it is done.

### Known gaps against the old exporter

- **`std.native('rtkMemoize')` is gone.** Its point was not evaluating its
  second argument on a cache hit, which the current native-function ABI cannot
  express — it hands functions a deserializer, and deserializing forces the
  value. Restoring it needs a lazy-argument path in `rtk-jsonnet-core` first.
- **`--helm-cache` is accepted and does nothing.** The in-memory
  `helmTemplate` cache still works within a single export; the `helm-cache/`
  directory that outlived one is not written. It needs a stable cache key
  first: the current one is an `FxHasher` value, which is neither stable across
  builds nor collision-resistant enough to name a file by.

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

## spec.json Configuration

### exportJsonnetImplementation

In tk's `spec.json`, `exportJsonnetImplementation: binary:/usr/local/bin/jrsonnet` configures tk to use jrsonnet for Jsonnet evaluation instead of go-jsonnet. tk still handles manifest exporting.

**rtk does not hand over to another implementation, but it does imitate one.** It always evaluates with its own jrsonnet, and when an environment asks for a jrsonnet binary it formats the result the way that binary would have:

- `std.manifestYamlDoc` quotes values only when it quotes keys, rather than always
- `std.manifestYamlStream` renders an empty stream as `...` rather than `---`
- floats render as the shortest representation rather than Go's `%.17g`
- **Tanka's native functions are not registered at all**, since the binary being imitated has never heard of them; an environment may probe for them with `std.native('…') != null` and take another path

An environment is recognised as asking for this when the implementation is `jrsonnet`, or a `binary:` path *ending* in `jrsonnet`. It is applied per environment, so one inline environment can ask for it while its neighbour does not. A project's `tkrc.yaml` overrides any of the formatting choices individually.

Two golden fixtures depend on all of this: `yaml_output_env_jrsonnet` and `inline_env_export_impl_mixed`.

## Common Issues

### Config hash differences in comparisons

When comparing rtk vs tk output, config hash differences (e.g., `mimir-config-exporter-hash`, `envoy-hash`) can generally be ignored. These are derived hashes of other resources (typically ConfigMaps), so they differ only because the underlying ConfigMap content differs.

### An export that finds no environments

Discovery walks with `walkdir`'s `filter_entry`, which skips dotted
directories — including the directory the walk *starts* from. Pointing an export
at `/tmp/.tmpXYZ` (which is what `tempfile` produces) or at any path under a
dotted directory therefore finds nothing at all. tk's own behaviour here is
unconfirmed, so this is left as it is rather than fixed.

Finding nothing is otherwise indistinguishable from exporting an environment
that produces nothing: no output directory is created, no `manifest.json` is
written, the exit code is zero. The command logs a warning; the library does not.

### Conflicting filenames are reported differently from tk

When two resources want the same file, both tools write what they have so far
and then abort, but they say different things:

- tk: `file '<absolute path>' already exists. Aborting`
- rtk: `file '<name relative to the output dir>' written by multiple environments: '<entrypoint>' and '<entrypoint>'`

rtk names the same entrypoint twice when the two resources come from one
environment, which reads oddly but is accurate. Long-standing in both rtk
exporters, and not something the golden fixtures cover.

### Two versions of serde-saphyr compiling

If you see both local and git versions compiling, ensure all crates use `serde-saphyr.workspace = true` instead of direct git references.
