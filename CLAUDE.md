# Claude Agent Notes

Project-specific context for AI agents working on rustanka (rtk).

## Agent Behavior

- **Never run git commands** unless explicitly requested by the user
- **Always run `make fmt`** after making changes

## Code Organization

- Prefer private methods when behavior naturally belongs to an existing type. A helper that takes that type as its primary state, mutates it, or consumes it should normally be an inherent method.
- Functions that construct a domain type should generally be private associated constructors on that type.
- Introduce a small private state type when several related values are repeatedly passed together through an operation or recursive traversal.
- Keep free functions for genuinely stateless algorithms, parser primitives, externally prescribed callbacks, command entry points, and transformations without a natural owner.
- Do not use free functions merely as substitutes for private methods.

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

`cmds/rtk` no longer has an exporter, a Jsonnet evaluator or spec types of its
own: `show`, `diff`, `apply`, `prune`, `env` and `validate` all load through
`rtk_environments::Engine`, so a fix to evaluation or manifest processing
reaches every command at once. What is left in the command crate is the
Kubernetes client (`cmds/rtk/src/k8s/`) and the YAML serializer the diff bodies
still use (`cmds/rtk/src/yaml.rs`).

### Where an Environment Lives

`JPath` resolves the **nearest** project root, as Tanka's `FindParentFile`
does — an environment carrying its own `jsonnetfile.json` is its own project.
The one way an outer directory wins is marker precedence: a `tkrc.yaml`
anywhere above beats a `jsonnetfile.json` sitting beside the entrypoint, which
is how Tanka documents per-environment vendoring. There is no `tkrc.yml`.

Exporting then resolves each environment a second time, because tk does:
`parallelLoadEnvironments` keeps only an environment's name and namespace and
reloads it from `Join(FindRoot(namespace), namespace)`. A namespace is relative
to its own project root while `FindRoot` resolves against the working
directory, so the round trip is the identity for ordinary layouts and lands
somewhere else for an environment that vendors for itself inside another
project. `Options::working_directory` names the directory this resolves
against, defaulting to the process working directory; the golden harness sets
it so a staged fixture can be exported without `chdir`.

This is why `rtk show` and `rtk export` can disagree about a nested project,
and tk disagrees with itself in the same way and for the same reason. The
`nested_project_root_env` fixture pins it.

### Helm Cache

`--helm-cache` persists successful `helmTemplate` results under each Tanka
project's `target/helm/v1/` directory. Entries are individual CBOR files named
by a SHA-256 digest of the release, render options, complete chart contents and
Helm version. The cache is shared by all environments rooted in that project;
an export spanning several projects uses each project's own target directory.

Cache reads and writes are best-effort. Missing, corrupt or incompatible entries
are misses, and write failures never replace a successful Helm render with an
export failure. Writes use temporary files and an atomic persist so parallel rtk
processes cannot expose partial entries. As with the in-memory cache, charts that
deliberately generate random or time-dependent output are frozen by the cache.

### rtkMemoize

`std.native('rtkMemoize')` cannot use the serde-based native-function ABI:
deserializing its second argument would evaluate it even on a cache hit. Each
Jsonnet implementation therefore registers this native manually and stores its
own native value type.

The jrsonnet implementation caches `Val` directly for the lifetime of the OS
thread. This preserves object identity, lazy fields, functions and assertions;
it also means separate evaluator instances on one worker share entries, while
different workers do not. Its TLS cache must initialize jrsonnet's thread-local
GC object space before itself so cached values are dropped before that object
space during thread teardown.

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

An environment is recognised as asking for this when the implementation is `jrsonnet`, or a `binary:` path *ending* in `jrsonnet`. It is applied per environment, so one inline environment can ask for it while its neighbour does not. There is no way to ask for the individual formatting choices on their own: an environment either asks for a jrsonnet binary or it does not.

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
