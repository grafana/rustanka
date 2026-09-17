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

### YAML reading and writing

- Use crates.io serde-saphyr for YAML reading; do not use a custom fork
- Tanka-compatible YAML writing belongs to `crates/rtk-yaml`
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

When implementing YAML serialization in rtk-yaml, **add parameters as needed** to support the different formatting behaviors required by each use case.

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

### What an Environment Can Read About Itself

`std.extVar('tanka.dev/environment')` is bound for every command that
evaluates, `eval` included — tk's `StaticLoader.Eval` hands the environment over
exactly as its `Load` does, so `eval` and `export` evaluate the same program.
Which environment is offered is decided the way tk's `DetectLoader` decides it,
on whether a `spec.json` sits beside the entrypoint and not on what evaluating
it turns out to declare, so the spec is read straight from the file and nothing
is evaluated twice.

An inline environment cannot be handed its own spec, since evaluating it is how
the spec becomes known. tk binds the variable to an `error` explaining that,
which costs nothing until something reads it, and rtk binds the same one.

`resourceDefaults` and `expectVersions` are always serialized. Go declares them
as plain structs rather than pointers, so `encoding/json` marshals them whatever
they hold and an absent one appears as `{}` — in the extVar, in `env list
--json` and in a written `spec.json` alike. Deserialization still treats them as
optional.

### Target Filtering

`-t/--target` expressions are compiled once, by
`rtk_environments::export::Targets`, and answer two different questions with the
same rule: which of an environment's own manifests to act on, and — when
pruning — which of the cluster's resources count as orphans. The second lives
with the Kubernetes client, which is why `Targets` is public; tk has one
`process.Filter` and so does rtk.

`Targets::kind_hints` lets prune leave whole resource types unasked for, but
only when every positive target names a kind outright. A pattern, or a
negative-only filter, withholds the answer: narrowing on a guess would hide
resources that should have been pruned.

### What Counts as a Manifest

Extraction mirrors tk's `walkJSON`. Anything carrying an `apiVersion` and a
`kind` — both present, both strings, both non-empty — is a manifest and is taken
whole. Anything else is a container to walk into, and **reaching a value that
cannot be walked fails the export**, as tk does: a manifest whose `kind` was
misspelled would otherwise leave the export in silence, which is what rtk used
to do while exiting zero.

Three details are load-bearing and each is pinned by a test:

- Fields are walked in **sorted** order, because which failure gets reported
  depends on it. `{metadata: …, data: …}` blames `.data`.
- A `null` field is skipped rather than treated as unwalkable, or every `if`
  without an `else` in a Tanka library would fail the export.
- `__ksonnet` is dropped before anything is decided, so it neither blocks the
  walk nor reaches the output.

The reported path is the *containing object's*, while the reason is the innermost
enclosing object's — which is why a bad value inside a list reports the list's
parent and the reason of the object above it. Two things are deliberately not
reproduced: tk appends a YAML dump of the offending object, and where nothing
encloses the value at all tk prints its nil error as `%!s(<nil>)`.

A nested `Environment` is refused by `Engine::manifests` and kept by `export`,
which is tk's own split: `Load` filters `(?i)^Environment/.*$` after processing
and refuses, so `show`, `diff` and `apply` reject one, while exporting writes it.

### Creating an Environment

`env add` writes what tk writes, byte for byte. Both files are pinned by tests
because every part of them was wrong once: `spec.json` gets only
`metadata.name` (the namespace is the entrypoint discovery derives, not a stored
field), the namespace tk defaults in `v1alpha1.New()` before any flag applies,
and the trailing newline `writeJSON` appends.

An inline environment is the same marshalled `Environment` passed through
go-jsonnet's formatter, which rewrites JSON's syntax without reflowing it. So
the layout is the JSON's — one member per line, trailing commas, empty objects
inline — and rendering the spec from its own serialization is what keeps its
fields and their order those of `spec.json`. Quoting follows the formatter:
single, switching to double as soon as the text holds a single quote.

Two divergences are deliberate. `tk env set` mutates the spec, prints what it
changed, then tries to reach the cluster and exits 1 **without writing**,
whichever flag was given — so on a machine with no matching context it never
persists anything; rtk writes and exits 0. `tk env add` does the same for
`--server-from-context` and `--context-name`, the only two flags that make it
call `Connect()`. Reproducing either would make the commands useless without a
live cluster. `rtk init` is a stub that exits 1; completing it means vendoring.

### Helm Cache

`--helm-cache` persists successful `helmTemplate` results under each Tanka
project's `target/helm/v1/` directory. Entries are individual CBOR files named
by a SHA-256 digest of the release, render options, complete chart contents and
Helm version. The cache is shared by all environments rooted in that project;
an export spanning several projects uses each project's own target directory.

The engine is shared for the whole of a command, so the in-memory half of the
cache spans every environment it touches: a chart twenty environments share is
rendered once. `diff --list-modified-envs` used to build an engine per
environment and so rendered it twenty times. `--helm-cache` is declared on the
shared Jsonnet arguments rather than on `export`, so a diff or a show reuses a
chart it rendered on a previous run exactly as an export does.

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

## Formatting

`rtk fmt` is complete and byte-identical to `tk fmt`. It is graded two ways: 857
corpus files against go-jsonnet's own `formatter.Format` (the identical call
`tanka.Format` makes, so matching it *is* matching `tk fmt`), and 3,016 files of
real Grafana Jsonnet — 1,452 of them vendored — through the real `tk` binary.
`crates/rtk-jsonnetfmt/quarantine.toml` is **empty and stays empty**: an entry
appearing in it again is a divergence, which for fmt means a bug.

The port is `crates/rtk-jsonnetfmt`; the CLI is
`cmds/rtk/src/commands/fmt.rs`. `rtk_jsonnetfmt::format` runs all fourteen steps
of go-jsonnet's `FormatNode` in the order the doc comment on `format` records,
read off upstream rather than inferred. A file that does not parse fails with
go-jsonnet's message, which is what `tk fmt` prints before aborting the run.

The phase-by-phase plan that drove this port (`docs/rtk-fmt-plan.md`) has been
retired from the repository. Where a comment refers to *the fmt port plan* or to
a phase number, this section is the surviving record; the plan itself is kept
outside the repo.

**`crates/jrsonnet-formatter` and `cmds/jrsonnet-fmt` are not tk-compatible and
stay untouched.** That crate is upstream's dprint-based, width-driven
pretty-printer, which re-lays-out from scratch; `jsonnetfmt` is
fodder-preserving and only normalises indentation, trailing commas, redundant
parens, quote style, comment style, blank-line runs and import order. These are
different algorithms, not different settings, so wiring `FmtArgs` into that
formatter would rewrite every file in a Grafana repo differently from `tk fmt`.
Leaving both alone also keeps upstream jrsonnet syncs against
`.jrsonnet-upstream-base` clean.

### Three rules that outlive the port

- **Never add a convergence loop.** `jsonnetfmt` is not a fixed point, and
  matching `tk fmt` on **one** run is the contract. A loop turns
  `a['fo\u006f']` into `a.foo` in a single run and diverges on the first
  escaped field lookup in a Grafana repo. `cmds/jrsonnet-fmt` needs
  `--conv-limit` because the dprint formatter is a different algorithm;
  `rtk fmt` must not gain one.
- **Reproduce upstream's oddities; do not fix them.** There is deliberately no
  `fmt_golden_override/` — for fmt every override would be a divergence from
  `tk fmt`. Each oddity is documented at its site in `crates/rtk-jsonnetfmt/src/`
  with a snippet pinning it.
- **Write the snippets before the pass.** Jsonnet that is already `tk fmt`-clean
  gives a pass nothing to do, so the breadth corpus grades each pass *least*
  where it is newest — four of the twelve change **zero** in-repo corpus files,
  and `AddPlusObject` can never be graded by it at all, the corpus being
  generated with `DefaultOptions`. `testdata/pass-snippets.json` is where a pass
  is really graded, and `testdata/pass-oracle.json` answers "which passes change
  this file's AST" exactly. Query the oracle; never read an
  input-against-golden diff, which says what differs and never which pass does
  it.

### Expect these to be reported as rtk bugs

Five upstream mechanisms leave `tk fmt` unsettled or destructive on a single
run, and rtk reproduces all five. Twelve inputs move on a second run; five are
worse — `tk fmt` writes output the parser then **refuses**, in place.

Moves on a second run:

- `PrettyFieldNames` (step 10) reads a string's *stored* value and
  `EnforceStringStyle` (step 11) then unescapes it, so brackets survive
  justified by an escape that is no longer there.
- `FixParens` collapses one redundant level per run — `(((1)))` → `((1))` →
  `(1)` — and its `FodderMoveFront` writes to the file's opening fodder for
  parens on the leftmost spine, four steps after `removeInitialNewlines`
  cleaned it. Three shapes therefore produce first-run output that looks
  broken and is exactly what `tk fmt` prints: a leading blank line, a body left
  unindented, and a file starting with two spaces.
- `SortImports` (step 1) keys on the stored value too, so a second run reorders
  the imports and carries their comments with them. The only one of the three
  that moves code rather than layout.

Refused on a second run, which is the worse class and kept in its own list:

- `PrettyFieldNames` has no `ObjectComp` guard, so **any** string-literal field
  name in an object comprehension loses its brackets — quoted or not — and the
  parser then says `Object comprehensions can only have [e] fields`. A
  genuinely computed name, `{ [k]: 1 for k in … }`, survives. This is the only
  oddity reachable from `DefaultOptions` that turns a working file into one
  that will not parse.
- A `|||` block of nothing but newlines loses the indent that held it together
  and comes back as `Text block not terminated with |||`.

`tests/idempotence.rs` and `tests/corpus.rs` each carry `KNOWN_NON_CONVERGENT`
and `OUTPUT_DOES_NOT_REPARSE` — two lists rather than one, so the worse class
cannot hide among the ordinary one. All four ratchet **both** ways: a listed
name that starts settling fails as loudly as an unlisted one that moves.

One further oddity changes what a file *evaluates to*: `AddPlusObject`'s switch
has no `ast.Slice` case, so `{a:1} {b:2}[1:2]` is written back as
`{a:1} + {b:2}[1:2]`, a different tree. It reaches nothing in practice —
`Options::default` has `use_implicit_plus` on, so `tk fmt` runs
`RemovePlusObject` and never runs `AddPlusObject` at all. The only thing that
reaches it is the `no_implicit_plus/` fixtures.

### Before touching a pass

- **Spacing is not fodder.** Fodder records line ends, blank counts, indents and
  comments and nothing else. Every space *within* a line is regenerated by the
  unparser from `crowded`, `separate_token` and `PadArrays`/`PadObjects`, so
  `{a:1,b:2}` becomes `{ a: 1, b: 2 }` with no pass involved. Indentation is the
  exception: it comes from `fodder.indent`, which the lexer filled counting a
  **tab as 8**, so a tab-indented file still needs `FixIndentation`. Check which
  of the two a spacing diff is before reaching for a pass.
- **Fodder is not a trivia stream.** It is a normalised model of vertical space
  with invariants in `MakeFodderElement` and across elements in `FodderAppend` —
  a `LineEnd` may not follow a `LineEnd` or a `Paragraph`. Extend fodder only
  through `Fodder::append`, never by pushing.
- **The round trip is not the identity, in go-jsonnet either**, with every pass
  disabled: a line-end comment is always preceded by exactly two spaces, indent
  is re-emitted as spaces, `\r` is dropped from block strings, and trailing
  horizontal whitespace is stripped at lex time and never restored. Do not
  reinstate a round-trip gate.
- **Four fodder slots the traversal never visits**, each pinned by a unit test,
  so a fodder pass silently will not reach them: `InSuper`'s `in_fodder` and
  `super_fodder`, `Index`'s `right_bracket_fodder` when the index is an
  identifier, `Apply`'s `tail_strict_fodder` without `tailstrict`, and a
  `Parameter`'s `eq_fodder` without a default. `FixIndentation` reaches all four,
  having its own walk.
- **Four of the fourteen steps are not passes**, so do not look for them in
  `src/passes/`: `SortImports` (`src/sort_imports.rs`, a free function that
  rebuilds the top of the tree), `removeInitialNewlines` and
  `removeExtraTrailingNewlines` (inherent methods on `ast::Node` and
  `fodder::Fodder`), and `FixIndentation` (`src/fix_indentation.rs`, its own
  walk). `FixParens` at step 6 must stay before step 7.

### The three deliberate Go-library ports

A Rust crate that does the job well but not identically is unusable when
identical is the contract.

- **`src/go_sort.rs`** ports Go's `sort.Slice` (pdqsort). Two imports can share
  a path, and "sorted" does not say which comes first: **ties invert from
  n = 13 upwards**, so `Vec::sort_by` is measurably wrong and `sort_unstable_by`
  is wrong differently. Graded by `testdata/go-sort-truth-table.json`, which
  records the Go version — the tie order belongs to the toolchain that built
  `tk`, so keep the table regenerable rather than sorting differently.
- **`crates/rtk-gobwas-glob`** compiles `--exclude` patterns. **Do not reach for
  `globset`:** `tk` calls `glob.Compile(e)` with no separator arguments, and
  with no separators gobwas treats `*` and `**` alike, so **`*` crosses `/`**.
  That is also why tk's default exclude list ships each pattern twice (`".*"`
  *and* `"**/.*"`). Graded by a truth table generated by the Go library itself,
  which is the oracle — gobwas' own syntax documentation disagrees with it about
  separators.
- **`crates/rtk-masterminds`** ports tk's semver constraint syntax and holds
  `TANKA_COMPATIBLE_VERSION`.

### The CLI contract

The order of operations is part of the contract and is numbered in the module
documentation on `fmt.rs`. **The streams are where this can be wrong while
looking right:** `--verbose` and `--stdout` write to stdout, while every summary
line, `--verbose`'s single trailing blank line and `--stdout`'s per-file
separator write to stderr. A test that merges the two cannot tell a correct
implementation from one that puts everything on either stream, which is why
`cmds/rtk/tests/fmt_parity_test.rs` runs the binary and captures them apart.

Four upstream behaviours read like bugs and each has its own test:

- **`--test` beats `--stdout`** — nothing printed, nothing written.
- **The output mode runs for every discovered file, changed or not**, so the
  default mode rewrites a file it did not change and moves its mtime.
- **`-` is honoured only as the sole argument**, and that branch returns before
  the excludes are compiled, so `rtk fmt - --exclude '[a'` succeeds where
  `rtk fmt . --exclude '[a'` does not.
- **A file named twice is formatted twice**, and counted twice under `--test`
  and `--stdout` but **once** in the default mode, because the unconditional
  write perturbs the second read.

`fmt` and `lint` are both `ArgsMin(1)`; neither defaults a path in. Exit 16 on
`--test` with changes is `commands::diff::EXIT_CODE_DIFF_FOUND`, reused rather
than respelled.

**Formatting runs on a 1 GiB stack, and that is parity rather than caution.**
The crate recurses to the nesting depth of its input in several places with
**no depth limit anywhere, deliberately** — a limit would refuse a file
`tk fmt` formats, Go growing a goroutine stack to 1 GiB. So `fmt` spawns its
work on a scoped `thread::Builder` with a 1 GiB `stack_size` and re-raises a
panic with `resume_unwind`. This port spends roughly **20 KiB of stack per level
of nesting**, measured; the test generates 10,000 levels, and both margins are
load-bearing — too deep and it fails on the command's own stack, too shallow and
it would pass with the large-stack thread deleted.

### Finding Jsonnet files

`tk fmt` and `tk lint` share Tanka's `jsonnet.FindFiles`, so rtk shares
`rtk_jsonnetfmt::files`. Six surprising behaviours are enumerated in
`src/files.rs` and pinned in `tests/discovery.rs`. The two that bite hardest:

- **A named regular file bypasses everything.** `tk fmt vendor/foo.libsonnet`
  formats it despite the default `vendor/**` exclude, and `tk fmt README.md`
  hands `README.md` to the Jsonnet formatter.
- **Walked paths are cleaned**, so a `./foo` argument yields `foo/bar`. Since
  `*` crosses `/`, an uncleaned `./` prefix would make `.*` exclude the whole
  tree.

### Generated artifacts, and what a go-jsonnet bump moves

Everything below is committed and checked for staleness, because correctness
against a *stale* oracle is green and means nothing:

- `make update-fmt-corpus` / `check-fmt-corpus` — the 857 files in two sets.
  `in_repo` (138) keeps one golden each under `testdata/corpus/`;
  `go_jsonnet_testdata` (719) commits **both halves** of every entry in
  `testdata/go-jsonnet-corpus.json`, since its inputs are not in this
  repository and an exact ratchet cannot hold over an environment-dependent
  denominator. `files` is asserted as well as `matching`, and a missing artifact
  is a failure rather than a skip.
- `make update-fmt-node-oracle`, `update-fmt-lexer-oracle`,
  `update-fmt-pass-oracle` — the AST, token/fodder and per-pass oracles. The
  lexer and pass oracles stage a program into a go-jsonnet checkout, because
  `internal/parser` and `internal/formatter` may only be imported from inside
  that module.
- `make update-go-sort-truth-table` / `check-go-sort-truth-table`,
  `make update-glob-truth-table` / `check-glob-truth-table`.
- `make go-jsonnet-checkout` clones the pinned go-jsonnet to
  `target/go-jsonnet`, which `GO_JSONNET_FOR_TESTS` overrides. Without it the
  `go_jsonnet/` fixture family grades **12 of 15** instead of 15, and two of the
  three it drops carry answers nothing else in the suite has.
- `make check-generated` runs the three staleness checks; CI runs them in their
  own job, installing the **exact Go the sort table names**, since that file is
  compared with a plain `diff -u`.

**The in-repo corpus set deliberately does not grow.** The node, pass and lexer
oracles are all driven by `corpus/manifest.json`, so adding files invalidates
about 19 MB of committed oracle — `node-oracle.json` alone is 11 MB — for
essentially no new node kind or fodder slot. Text-level breadth stays at the
text level.

### The acceptance gate and the scheduled jobs

`fmt-acceptance.toml`, `scripts/fmt-acceptance-corpus.sh` and
`make fmt-acceptance` run the **real `tk` binary** against the real `rtk` binary
over pinned external repositories, measuring byte parity, the
already-formatted property, whether any output fails to reparse, and discovery
order per root. Five things to know before changing it:

- **It is deliberately not a pull-request gate.** Its corpus is hundreds of
  megabytes of clones tracking moving branches, and a gate that goes red for a
  GitHub outage is one people learn to ignore.
- **Per file through `-`, not per tree.** A parse failure aborts the whole run
  in both tools, so a whole-tree comparison would have them *agree* about
  aborting and go green having compared almost nothing.
- **The corpus excludes dotfiles only**, so `tk fmt`'s own `vendor/**` default is
  dropped on purpose and `vendor_files` measures how much vendored Jsonnet is
  really there. Read the number; do not claim the coverage.
- **`--extra-root` adds a local tree** for a one-off run, reported apart, and
  downgrades the count ratchet to advisory while saying so.
- **`-1` in any count means never measured and fails the gate** rather than
  defaulting to zero, and `error_text_matching` prints `UNGRADED` rather than
  `0 of 0`.

`.github/workflows/tk-latest.yaml` is weekly and non-blocking: it installs tk
latest, runs the fmt CLI cross-check and the golden fixtures, and updates one
issue rather than filing one a week. The corpus is generated from go-jsonnet
with `tk` nowhere in it, so only the cross-check and the fixtures observe tk
moving.

`.github/actions/install-tk` is the one place tk is pinned. It reads
`TANKA_COMPATIBLE_VERSION` out of `crates/rtk-masterminds/src/lib.rs`, and **an
empty read fails the job**. Two details that cost a red CI each: `tk --version`
goes to **stderr** through Go's `log` package, so the capture needs `2>&1`; and
the assertion matches the version **number** rather than the tag, since whether
a release carries a leading `v` in what it prints is not reliable.
`RTK_REQUIRE_TK=1` is set on `cargo test --all` so the tk cross-checks fail
rather than printing a loud `SKIPPED` naming zero scenarios.

### Testing hygiene this port paid for

Each of these was a real green-and-meaningless test here, not a principle:

- **Make the unmeasured state fail rather than read as fine.** A count of `-1`
  fails; a missing artifact fails; `UNGRADED` is printed instead of `0 of 0`.
- **Check the artifact, not the name.** Six separate instances here of a name,
  comment or fallback promising coverage it did not deliver — including a config
  named for Grafana Jsonnet it never held, and a documented fallback to a
  checkout that nothing created.
- **A test that defers a claim elsewhere is only as good as the elsewhere.** An
  assertion deferred parse-error *locations* "to the oracle" while the oracle
  held zero error cells, so 26 of go-jsonnet's 29 error templates could have
  reported anything with the suite green.
- **Ratchets fail in both directions**, including a listed entry the run never
  reached — otherwise an upstream rename leaves an entry describing nothing
  while its count goes down, which reads as an improvement.
- **A skip that depends on the entry point is as bad as a silent one.** A
  fixture family graded under `make test` and skipped under the plain
  `cargo test` anyone actually runs.
- **Write no fodder expectation by hand when a dump can supply it.** Of 16
  derived by reading Go's source, 2 were wrong, and both were about a `LineEnd`
  the *model* inserts that the source does not contain. The same error one level
  out: a grep over inputs is not a measurement over outputs.

## Version Expectations

### Which Tanka rtk answers for

`rtk_masterminds::TANKA_COMPATIBLE_VERSION` names it, `v0.38.0`, spelled the way
tk spells its own — tk builds its version in with `git describe --tags` and
quotes the string verbatim in its messages. rtk answers for the Tanka it
implements rather than for its own version: comparing rtk's `0.5.x` against a
constraint like `>=0.20` would fail every real environment, and the question
`expectVersions.tanka` asks is which Tanka's behaviour is on offer.

It deliberately carries no prerelease. Masterminds treats a prerelease version
as unsatisfying any constraint that did not ask for one, so a `-pre` would fail
nearly everything an environment could write.

### Constraints are Masterminds', not the semver crate's

tk links `github.com/Masterminds/semver` v1.5.0, whose syntax the `semver` crate
does not implement. `rtk-masterminds` is a port, not a translation, because the
differences change answers: a bare `1.2.3` is *equality* there and a caret to the
`semver` crate; `^` is major-only, so `^0.1.2` admits `0.38.0` where the `semver`
crate and npm both stop at `0.2.0`; and `||` and `x` are not syntax the `semver`
crate can parse at all.

It is checked against a table generated by the Go library itself
(`crates/rtk-masterminds/testdata/`, with the generator beside it). That table
caught four divergences a reading of the documentation would have shipped, so
regenerate it rather than trusting the docs if tk ever changes library version.

### Where each expectation is checked

- An environment's `spec.expectVersions.tanka` is checked in
  `processed_manifests`, which is tk's `LoadManifests` boundary: export, show,
  diff, apply and prune reach it, and `eval` and `env list` do not. Both of tk's
  messages are reproduced verbatim. **A dev build of tk skips this check
  entirely**, so verifying parity needs a tk built with
  `-ldflags -X …CurrentVersion=v0.38.0`.
- A project's `tkrc.yaml` is rtk's own; tk only uses the file to mark where a
  project starts and never reads it. Its `expectVersions.tanka` is checked as
  soon as the file is read, so every command that evaluates anything honours it.
- `expectVersions.helm` compares major versions only, and is checked **only when
  a project declared one** — helm has to be run to be asked, and nothing should
  pay for a check it did not ask for.
- `expectVersions.kubectl` and `expectVersions.binaries` are accepted and inert.
  rtk runs no kubectl at all, and the only binaries it executes are helm and
  kustomize.

### Reading tkrc.yaml

Nothing loaded the file until this landed, so all four of its settings were
inert. Reading it activates `disableNativeFunctions`, `maxStackDepth` and
`jsonnetImplementation` as well as the version expectations.

Only `spec` is read, so `apiVersion`, `kind` and `metadata` may be omitted —
deserializing `Rc` directly demanded a `metadata` it has no use for. And
`--max-stack` lost its default of 500: a default was passed on every run and so
always beat the depth a project asked for, which is why `maxStackDepth` could
never take effect. Unset, the project's depth applies; failing that, the same 500
tk uses.

## Performance and Memory

### Benchmarking

`rtk-benchmarks/run-benchmark.py <config> --rtk-binary-path X --rtk-base-binary-path Y`
runs the same comparisons CI does. The jobs validate that rtk's output still
matches tk's, but nothing gates on timing, so a regression lands green.

**Point `TMPDIR` at a real disk.** The fixtures land in a temporary directory,
and on most machines `/tmp` is tmpfs, where `fsync` is a no-op. An export that
had gained a disk flush measured 1.00 locally and 1.37 in CI until `TMPDIR` was
moved onto a real filesystem, where it measured 1.68. Anything touching how
files reach disk is invisible by default.

Compare against a binary built from the commit before the change rather than
against the PR base: the ratio CI prints is against the base, so a regression
introduced mid-branch is diluted by everything else on it.

### The evaluation GC, and why export pays for it

`Drop for Evaluation` runs a full `collect_thread_cycles()`
(`crates/rtk-jsonnet-jrsonnet/src/lib.rs`). Measured on a 200-environment
recursive export, that collection costs **17% of wall time** (130 ms against
111 ms without it) and saves **32% of peak RSS** (23 MB against 30 MB).

**Memory is the deliberate choice here**, so the collection stays. Do not
"optimize" it away without a decision about the memory budget; the speed is
already accounted for and was not judged worth the resident set.

It also explains why `eval` costs about 18% more than it did before it started
binding `tanka.dev/environment` and materializing through `process::materialize`.
`materialize` forces and caches the whole object graph before that single
collection runs, so the collection has the largest possible graph to traverse;
the previous serde round-trip left much less behind. `materialize`'s own logic is
only 5% of eval's runtime, so there is nothing to win by micro-optimizing it —
the cost is the collection, and it is being paid on purpose.

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

**The index describes the directory, whatever happened.** Everything that
reached disk is recorded, including what a failed environment wrote before it
failed and what was written before a conflict was found — a conflict is only
detectable once every environment has been exported, so refusing to write the
index then left files owned by nobody. Nothing is pruned on the strength of a
run that did not finish, and only what pruning actually deleted is forgotten.

The index is written in place, with no temporary and no flush. Making it durable
while the manifests it describes go out through a plain `fs::write` would be
theatre — a crash loses the files as readily as their owners, and an index that
survives to describe missing files is worth nothing. It cost a disk flush on
every export, which is 2-3 ms of a 5 ms one.

**How many environments were skipped depends on the run.** It is however many
workers had not yet started when the export was stopped, which is a fact about
scheduling rather than about the environments, so only `successful + skipped` is
fixed for a given input. What is stable is what matters: exactly the
environments that genuinely failed are counted as failures, the first failure in
discovery order is the one reported, and the exit code follows. Relabelling
environments that did export would make the count look steady while
contradicting `manifest.json`.

tk detects two environments writing the same file with a `stat` immediately
before each write, which two workers can both pass: at its default parallelism
it misses the collision rtk catches, and only reports it reliably at
`--parallel 1` or `2`. rtk checks after the fact, so it always finds it.

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
