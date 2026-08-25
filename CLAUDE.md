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

Where the re-resolution lands somewhere that declares no matching environment,
tk fails the whole export; rtk keeps the environment discovery actually found.
Reproducing that failure would mean aborting an export over a layout rtk can
resolve perfectly well, so this one is a deliberate divergence.

### Finding Environments

Discovery answers two different questions, and `Search` says which:

- `Search::Environment` takes the path as the environment, as tk's `Peek`
  does. It is what a non-recursive export and `load_single` want.
- `Search::Tree` walks everything below the path, as tk's `FindFiles` does. It
  is what `--recursive`, `env list` and `diff` want.

tk chooses on the command rather than on what the path turns out to hold, so
rtk does too. The case that tells them apart is a project whose root is an
entrypoint in its own right: walking has to descend past it, or every
environment underneath disappears. tk stopped at the first valid environment
until v0.27.0, and the docstring in `find.go` still says it does.

An entrypoint is imported by its **absolute** path, as tk imports
`jpath.Entrypoint`. A relative name is resolved against the importing file
first, and the generated snippet has no file, so the process working directory
would decide — which quietly loaded the wrong entrypoint for exactly the layout
above.

Naming a file names the entrypoint, whatever it is called, as `jpath.Filename`
does. Walking is the exception and keeps only `main.jsonnet`, because
`FindFiles` does: a custom entrypoint is reachable by naming it and by nothing
else, and naming one recursively finds nothing at all.

### Selecting by Name

`--name` means two different things in tk, chosen by command:

- `--recursive` compares `metadata.name` exactly, so part of a name selects
  nothing rather than everything containing it.
- Everything else loads one environment through a loader. The inline loader
  matches a substring, because `SingleEnvEvalScript` asks `std.member`, and
  prefers a full match among what survives; the static loader ignores the
  filter entirely, a static environment being named after where it lives.

What survives can still be several environments, and that is refused with tk's
own wording. tk never compares the name against a filesystem path.

A recursive export that matches nothing is not an error: `--name` and
`--selector` are a filter over what was walked, and tk exports what survived
and exits zero. Asking for one environment and not finding it still fails.

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

**What the key covers, and why so little of the environment.** helm reads a
great deal of it, and almost none can change what `helm template` writes for a
chart already on disk: repository and registry configuration is never consulted
for a local path, a plugin cannot claim `--values=-` or a filesystem chart, and
rtk never passes `--validate`, so no request is made and nothing about kube
transport, authentication or the kubeconfig matters. `PATH` reaches nothing
without an explicit `--post-renderer`.

What is left is the clock and the namespace. `TZ` is read by any chart calling
sprig's `now` or `date`. A namespace the caller names reaches helm as a
client-go override, and an override short-circuits the resolution chain, so in
that case — which is every real caller — nothing else can decide
`.Release.Namespace`. Where none is named, `helm env HELM_NAMESPACE` reports the
same `settings.Namespace()` that rendering asks, so helm answers for itself
rather than rtk reimplementing client-go's precedence.

Entries are also keyed on the build that filled them: `build.rs` supplies the
commit and whether the tree is dirty, because the stored value is the
*post-processed* render and a change here has to invalidate it. A dirty tree
shares one identity, so use `RTK_HELM_DISABLE_MEMOIZATION` while iterating on
that crate. A disk hit logs at debug level, which is the first thing to look at
when a cache appears not to be working.

Only abandoned temporaries are swept, and only once they are old enough to be
nobody's — a concurrent rtk process holds a live one. Entries themselves are
never evicted: they are bounded by how many distinct renders a project has, and
they live under `target`.

Note that the `helm_*` golden fixtures render with whatever helm the CI runner
image provides, so an image bump that changes helm's output breaks them.

### rtkMemoize

`std.native('rtkMemoize')` cannot use the serde-based native-function ABI:
deserializing its second argument would evaluate it even on a cache hit. Each
Jsonnet implementation therefore registers this native manually and stores its
own native value type.

**It exists to cross evaluations.** Jsonnet already memoizes within one — a
thunk is computed once, and an object caches its fields — so a cache scoped to
a single evaluation would do nothing at all. What it saves is the work a worker
would otherwise repeat for every environment it exports, which is why the cache
outlives the evaluator that filled it.

So a memoized value must not depend on *which* environment computed it.
Anything that varies per environment belongs in the key, the way a caller
already writes `per_cluster-<hash of the labels>`. Two things make that concrete,
and both are pinned by tests:

- The value keeps the external variables, native functions and YAML formatting
  of the environment that computed it, so one reading `std.extVar` reports that
  environment's answer to every later one.
- An `import` inside it is resolved when the value is *forced*, so it resolves
  against whichever evaluation forces it, with that evaluation's import paths.
  Float formatting and the stack limit follow the same rule.

There is deliberately no evaluator fingerprint in the key. Environments differ
in their import paths by construction, so a key that accounted for them would
never hit, leaving only what Jsonnet does for free.

The jrsonnet implementation caches `Val` directly for the lifetime of the OS
thread. This preserves object identity, lazy fields, functions and assertions;
it also means separate evaluator instances on one worker share entries, while
different workers do not, so an N-worker export computes each key up to N
times. Nothing is ever evicted: entries are bounded by how many distinct keys
are used, and the process is a short-lived CLI. Its TLS cache must initialize
jrsonnet's thread-local GC object space before itself so cached values are
dropped before that object space during thread teardown.

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

### An export interrupted part way through

A `--recursive` export streams discovery through the worker pool, so that
evaluating one environment overlaps with discovering the next. tk discovers
everything up front instead, in `FindEnvsFromPaths`, and only then writes
anything. So an entrypoint that cannot even be discovered — an inline one whose
Jsonnet fails; a static one is only read, and fails later as itself — lands
differently:

- tk writes nothing at all, having failed before `ExportEnvironments`.
- rtk has already exported the environments discovered before it.

Both exit 1. rtk keeps the streaming and makes the outcome coherent instead:
whatever was written is recorded, so `manifest.json` describes the directory
rather than the export that was meant to happen. Leaving files behind that the
index does not mention is what breaks the *next* export — `fail-on-conflicts`
cannot protect a file it has no owner for, and `replace-envs` will not prune it.

Discovery failing stops the export: nothing after it is evaluated or written,
and those environments are reported as skipped. Which failure is reported is
decided on results put back into discovery order, not by whichever worker
recorded one first.

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
