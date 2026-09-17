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

The plan is `docs/rtk-fmt-plan.md`. **Phases 2, 3 and 4 are complete.** Phases
0, 1, 2a, 2b, 2c, 2d, 2e, 2f, 3 and 4 have landed: the lexer, the AST, the
parser, the unparser, the pass traversal, all twelve passes —
`FixTrailingCommas`, `NoRedundantSliceColon`, `PrettyFieldNames`,
`EnforceStringStyle`, `EnforceCommentStyle`, `EnforceMaxBlankLines`,
`FixNewlines`, `FixIndentation`, `FixParens`, `RemovePlusObject`,
`AddPlusObject`, `SortImports` — and all three of `FormatNode`'s non-pass
steps. `quarantine.toml` is **empty** and `tests/corpus.rs`'s idempotence
allow-list is **empty**. An entry appearing in the quarantine again is a
divergence needing an entry here rather than a line there.

**The corpus is 857 files in two sets, and "138 of 138" is Phase 3's number
rather than the current one.** Phase 4 added go-jsonnet's own root `testdata/`
as a second set of 719; every count in the phase history below still means the
138, and `[sets.in_repo]` in `testdata/corpus-baseline.toml` is where that
number now lives. See **The formatter corpus** below for the two sets and why
they are stored differently.

What is left is Phase 5: acceptance over Grafana's real Jsonnet, and the README
flip. `rtk fmt` works, and it writes in place, so the README row stays ❌ until
Phase 5's already-formatted gate says otherwise.

### The `fmt` CLI

`cmds/rtk/src/commands/fmt.rs` is the whole of it, and it calls
`rtk_jsonnetfmt::format_default` and `find_files_all` and nothing else. **The
order of operations is part of the contract**, so it is numbered in the module
documentation there and the code runs in that order:

1. `ArgsMin(1)`. No path is defaulted in.
2. Stdin, before anything else — and before the excludes are compiled.
3. Compile the excludes, so a malformed glob aborts with nothing read.
4. Pick one output mode, `--test` first.
5. `--verbose`, on **stdout**.
6. The summary, on **stderr**, a three-way switch in that order.

**The streams are where this can be wrong while looking right.** `--verbose`
and `--stdout` write to stdout; every summary line, `--verbose`'s single
trailing blank line and `--stdout`'s per-file separator write to stderr. A test
that merges the two cannot tell a correct implementation from one that puts
everything on either stream, which is why `cmds/rtk/tests/fmt_parity_test.rs`
runs the binary and captures them apart. Stdout goes through the command's
writer, so `rtk fmt --stdout … | head` exits cleanly; stderr is written
directly, as tk writes it.

Four behaviours are upstream's and read like bugs. Each has its own test:

- **`--test` beats `--stdout`.** The mode is chosen by testing `--test` first,
  so nothing is printed and nothing is written.
- **The output mode runs for every discovered file, changed or not**, because
  `outFn` is called unconditionally in `FormatFiles`'s loop. So the default mode
  rewrites a file it did not change — moving its mtime — and `--stdout` prints
  files it did not change. The mtime is what the test observes, set into the
  past with `touch` rather than by sleeping.
- **`-` is honoured only as the sole argument**, and that branch returns before
  the excludes are compiled, so `rtk fmt - --exclude '[a'` succeeds where
  `rtk fmt . --exclude '[a'` does not. Both halves are tested; neither alone
  says the branch sits in the right place.
- **A file named twice is formatted twice**, discovery neither sorting nor
  deduplicating across arguments. Whether it is *counted* twice then depends on
  the output mode, and the plan's CLI-surface section is wrong about this:
  under `--test` and `--stdout` both passes read the same unformatted bytes and
  both count, while **the default mode writes the file on the first pass and so
  reads it back clean on the second, and reports `Formatted 1 files`**. The two
  quirks compose — the unconditional write is what perturbs the second read.
  Measured against tk rather than reasoned about: the standalone test asserted
  2 and failed, and `named-twice-writing` in the cross-check is what says tk
  reports 1 as well.

One ordering was not written down in tk's source as read for the plan, so it is
pinned here and was cross-checked: `printFn` runs before `outFn` inside the
loop, so with `--verbose --stdout` a file's `fmt` line precedes its contents on
stdout. `verbose_and_stdout_share_stdout_in_loop_order` is the assertion, and
the `verbose-and-stdout` scenario of the cross-check confirmed it against tk —
as it confirmed that `--verbose`'s trailing blank line is conditional on
`--verbose` rather than unconditional.

Exit 16 on `--test` with changes is `commands::diff::EXIT_CODE_DIFF_FOUND`,
reused rather than respelled. `run` returns a `bool` and `main` exits, following
`diff` rather than `lint`'s `process::exit`.

**`rtk fmt` must never gain a convergence loop.** Twelve inputs do not settle in
one run and three of them produce first-run output that looks broken — a leading
blank line, a body left unindented, a file starting with two spaces. That is
what `tk fmt` prints; see "And that fodder move is a second, worse
non-convergence" below. Matching `tk fmt` on **one** run is the contract, and
iterating diverges on the first escaped field lookup or doubly-parenthesised
expression in a Grafana repo. Expect the three to be reported as rtk bugs.

**Three more do not merely look broken: their first-run output does not
parse.** `tk fmt` writes them anyway, in place. A computed field inside an
object comprehension is promoted to a plain one the parser refuses, and a `|||`
block of nothing but newlines loses the indent that held it together. Both are
upstream's and both are reproduced; the mechanisms are under `jsonnetfmt` is
not a fixed point below, and `tests/corpus.rs`'s `OUTPUT_DOES_NOT_REPARSE` is
what pins them. **A convergence loop would not save these either** — a second
run does not fix them, it fails.

#### Formatting runs on a 1 GiB stack, and that is parity rather than caution

`crates/rtk-jsonnetfmt` recurses to the nesting depth of its input in several
independent places — `Node::opening_fodder_mut`, the parser, every `AstPass`,
`FixIndentation::visit`, `Group::absorb`, and the drop of the tree itself — and
there is **no depth limit anywhere in it, deliberately**. A limit would refuse a
file `tk fmt` formats, which is a divergence in the one direction this project
never accepts: Go grows a goroutine stack to 1 GiB, so `tk fmt` formats input
that would abort rtk with `thread '…' has overflowed its stack` and a SIGABRT —
no exit code, no summary, and in the default mode some files already rewritten.

So `fmt` spawns its work on a `thread::Builder` with a 1 GiB `stack_size` and
joins it, the way `crates/rtk-environment` gives its rayon pools 8 MiB. A
*scoped* thread, because the work borrows the caller's writer. It wraps
everything from step 2 onwards, stdin included — a deeply nested file is as
likely to arrive down a pipe as to be named — and the whole tree is therefore
also dropped on that stack. A panic is re-raised with `resume_unwind` rather
than flattened into an error the user would read as their file's fault.

**1 GiB is not parity at every depth, and the number is measured.** The test
first used 50,000 nested brackets and **overflowed the 1 GiB** in an
unoptimized build, which puts this port at roughly **20 KiB of stack per level
of nesting**: the parser spends several frames per level, and each of twelve
passes, `FixIndentation` and the drop of the tree spend at least one.
go-jsonnet's frames are far smaller, so tk reaches deeper in the same gigabyte
and always will. Real Jsonnet nests to tens of levels, so the gap is
theoretical; raising the number further is an arms race with no natural
stopping point, and reserving more than Go's own maximum would be hard to
justify.

So `deeply_nested_input_formats_rather_than_overflowing_the_stack` generates
**10,000** — an order of magnitude inside 1 GiB, and one to two orders of
magnitude past the roughly 400 levels an 8 MiB default survives unoptimized.
Both margins are load-bearing: too deep and the test fails on the command's own
stack rather than grading it, too shallow and it would pass with the large-stack
thread deleted. Do not tighten it towards either edge, and note that the
profile decides the arithmetic — `[profile.test]` is `opt-level = 3` while the
binary the test spawns is not.

#### The three `strip_*` options were removed, not implemented

`rtk_jsonnetfmt::Options` used to carry `strip_everything`, `strip_comments` and
`strip_all_but_comments` as `pub` bools that nothing read. Phase 3 is the first
time `Options` is handed to a caller, which is the moment a flag promising to
strip comments and silently keeping them stops being harmless. Four things
decided it: nothing implements them, so an absent field is the only honest way
to say so; `DefaultOptions()` skips all three and `tk` exposes no way to ask for
them; the pass oracle deliberately does not dump them, two of the three
rewriting every tree they touch — about 270 of 356 changed cells and most of a
23 MB file — so they would land ungraded; and nothing in tk parity needs them.

The route back is recorded on `Options`: they are **one
`if / else if / else if` chain** at step 9 of `FormatNode`
(`internal/formatter/jsonnetfmt.go:178-184`) — mutually exclusive and
first-wins, *not* three independent `if`s — and adding their names to
`passNames` in `testdata/generate/_staged/passdump.go` is what would grade them.

`crates/jrsonnet-formatter` is **not** tk-compatible and `cmds/jrsonnet-fmt` is
not either. That crate is upstream's dprint-based, width-driven pretty-printer,
which re-lays-out from scratch; `jsonnetfmt` is fodder-preserving and only
normalises indentation, trailing commas, redundant parens, quote style, comment
style, blank-line runs and import order. These are different algorithms, not
different settings, so wiring `FmtArgs` into that formatter would produce an
`rtk fmt` that rewrites every file in a Grafana repo differently from `tk fmt`.
Both are left untouched so upstream jrsonnet syncs against
`.jrsonnet-upstream-base` stay clean.

`rtk fmt` lives in `crates/rtk-jsonnetfmt` instead. `rtk_jsonnetfmt::format`
runs all fourteen steps of `FormatNode`; the doc comment on `format` carries
the order, read off upstream rather than inferred. A file that does not parse
fails with go-jsonnet's message, which is what `tk fmt` prints before aborting
the run.

### `SortImports`, and the sort underneath it

Step 1, `crates/rtk-jsonnetfmt/src/sort_imports.rs`. Not in `src/passes/`,
because upstream's is a free function that touches nothing in `internal/pass`
and **rebuilds the top of the tree** rather than visiting it: `buildGroupAST`
constructs a fresh chain of `Local` nodes from the end backwards, one bind
each. So `local a = import 'x', b = import 'y';` always comes out as two
nested single-bind locals, whether or not anything was reordered, and every
rebuilt local **loses its location** — Go builds them with
`ast.NodeBase{Fodder: fodder}` and nothing observable reads a location.

Its entry point is `ast::Node::sort_imports`, a method on the type it
transforms, as step 2's `remove_initial_newlines` already is.

Six things about it are load-bearing and each is pinned by a snippet:

- **The sort key is the *stored* value of the string literal.** `SortImports`
  is step 1 and `EnforceStringStyle` is step 11, so a path written
  `'a'` sorts as six characters beginning with a backslash (0x5C), ahead
  of `'_x'` (0x5F) — where the character it denotes, `a` (0x61), would sort
  behind. Comparison is bytewise in both languages, so that part ports
  directly. **This is a non-convergence, not merely the same seam as 2c's**:
  the second run sorts on the unescaped keys and reorders the imports, carrying
  their comments with them. `sort_imports/key_is_the_escaped_value` and
  `..._swaps` are the two cases, and `tests/idempotence.rs` is what says so.
- **`duplicatedVariables` keys on the bind *variable*, not the path**, and one
  duplicate disables sorting for the whole group. The only route is shadowing
  across locals in one group: two binds of a single local sharing a name are
  refused by the parser with "Duplicate local var" before the formatter sees
  the tree.
- **`isGoodLocal` requires *every* bind to be a plain `import` with no `Fun`.**
  `importstr` and `importbin` fail it, because Go asserts `*ast.Import`
  specifically. A local that fails it is not the root of a sortable chain at
  all, so one bad bind at the top of a file leaves every import below it
  untouched.
- **`groupEndsAfter` is narrower than its own doc comment.** A non-interstitial
  element sets a flag and the *next* element ends the group, while any element
  with `Blanks > 0` ends it immediately. So a bare `LineEnd` **continues** the
  group; a `LineEnd` then a `Paragraph` ends it, which is how a comment on its
  own line separates two groups with no blank line; and a `//` comment
  trailing a `;` does *not* separate them, because a comment on a line that is
  not fresh is a `LineEnd` **carrying** a comment. Empty fodder ends nothing
  either, so `local b = …;local a = …;` is one group and its newlines are
  invented by `FodderEnsureCleanNewline`.
- **Fodder travels with the import in front of it.** Each element keeps the
  fodder that *follows* it and `buildGroupAST` puts element `i-1`'s in front of
  element `i`, so a trailing comment moves with the line it was written on and
  the last element's becomes the fodder before the body.
- **`splitFodder` is asymmetric.** The first half is a plain push and the
  second goes through `FodderAppend`, which is what inserts a synthetic
  `LineEnd` in front of a `Paragraph` landing first in an empty second half.
  Blanks are *moved* rather than divided: zeroed on the first half's last
  element and carried by a **fresh** `LineEnd` at the front of the second. So
  the halves concatenate back to something equivalent to the original, not
  equal to it.

**All three of upstream's panics are unreachable, and all three are
reproduced.** `"beforeNext should still be empty."` cannot fire because the
second half is only appended to once the flag is already set and the flip
happens after that append. `"Expected beforeNextFodder to be empty"` cannot
fire because everything that makes the second half non-empty also makes
`groupEndsAfter` true, over the same fodder. `"topLevelImport called with bad
local."` is not even expressible here — `Group::absorb` takes an owned `Local`
the caller could only have got by testing first. No cell of either oracle
records a panic, over 138 files and 440 snippets.

### `sort.Slice` is ported, because sorting has more than one right answer

`crates/rtk-jsonnetfmt/src/go_sort.rs` is a port of Go's `sort.Slice` —
pdqsort, from `sort/zsortfunc.go`. It is the **third deliberate Go-library
port** here, alongside `rtk-gobwas-glob` and `rtk-masterminds`, and it is here
for the same kind of reason: a Rust crate that does the job well but not
identically is unusable when identical is the contract.

Two imports can share a path — `local k = import 'k.libsonnet', kausal =
import 'k.libsonnet';` is an ordinary habit — and "sorted" does not say which
comes first. `sort.Slice` is documented as unstable, and the pass oracle
measured what that costs: **ties invert from n = 13 upwards.** A group of
thirteen whose first and last element share a key comes back with the last one
first; a run of three sharing a key comes back `i09 i01 i04`. `Vec::sort_by`
is therefore measurably wrong, and `sort_unstable_by` is a different pdqsort
with different pivot choices and is wrong differently.

Three details worth having before touching it:

- **`maxInsertion` is 12 tested with `<=`**, so twelve is the *last* size that
  leaves insertion sort, not the first that does not. Reading the constant as
  a strict bound puts the boundary one out, and n = 12 then looks like
  evidence of stability when it is only insertion sort.
- **An all-equal range keeps its order at every length**, because
  `partitionEqual` handles it. That is why every `ties_*_all_equal` snippet is
  a no-op while the mixed arrangements are not — an all-equal group is not
  evidence that the sort is stable.
- **`partialInsertionSort`'s left shift is bounded by the literal `1`, not by
  `a`**, so on a recursive call it can walk below the range it was given.
  Upstream's; reproduced.

The honest caveat: this tie order is an implementation detail of the Go
toolchain that built `tk`, not a promise. It has been pdqsort since Go 1.19,
`make update-go-sort-truth-table` records the Go version in the table, and a
future Go that reorders ties would move `tk fmt`'s output and not rtk's. That
is a reason to keep the table regenerable, not a reason to sort differently —
there is no third answer more correct than the one `tk` prints.

Graded by `testdata/go-sort-truth-table.json`: ten input shapes at **forty-one**
lengths, 410 cases of which 235 have tied keys, generated by the standard
library itself. (`src/go_sort.rs` and `tests/go_sort.rs` have said forty and
thirty-nine; the lengths are 0..=30 plus 40, 49, 50, 51, 63, 64, 100, 128, 200
and 500.) It reaches `breakPatterns` — 43 calls over 37 cases — and it does
**not** reach the heapsort fallback at all: the most `limit` decrements any case
consumes is 2, while heapsort needs `bits.Len(n)` consecutive bad partitions,
which is 4 at the smallest n where it is even possible. `heap_sort`, `sift_down`
and the `limit == 0` branch are graded by nothing, here or anywhere.

Reachability from real input, also measured: the largest import group in this
repository is **12**, which is exactly `maxInsertion`, so all of pdqsort proper
is dead over real files with a margin of one import. `breakPatterns` is not
gated on length 50 — it fires on an unbalanced partition, first reachable at
**n = 14**, where about half of the arrangements with two repeated paths reach
it. So "no import group gets near either" holds for heapsort with room to spare
and for `breakPatterns` by two imports. The table records the
permutation only, over integer keys; string comparison is graded through the
real comparator by the `sort_imports/case_is_bytewise` and
`sort_imports/punctuation_sorts_before_letters` snippets.

Step 7 is one `if` with two branches and not two steps. `Options::default` has
`use_implicit_plus` on, so **`tk fmt` runs `RemovePlusObject` and never runs
`AddPlusObject`**: the only way to reach the latter is
`use_implicit_plus: false`, which nothing but upstream's own
`TestFormatNoImplicitPlus` — the `no_implicit_plus/` fixtures — passes. So the
138-file corpus cannot grade `AddPlusObject` at all, and if that count ever
moves when it changes, step 7 has been wired wrong.

`FixParens` at step 6 running before step 7 is load-bearing: `((e))` is
collapsed before `AddPlusObject` inserts any parentheses, and the ones it
inserts are never collapsed again. Do not reorder them.

### The three semantics-affecting passes

Everything else in the pipeline is cosmetic — a bug moves a comment or an
indent. A bug in `FixParens`, `RemovePlusObject` or `AddPlusObject` changes
what a file **evaluates to**: `{ a: 1 } { b: 2 }.a` has to become
`({ a: 1 } + { b: 2 }).a`, and without the inserted parentheses the formatted
expression is a runtime error rather than a differently-spelled program. They
are also the only three that replace a node rather than rewriting its fodder.

**`RemovePlusObject` only fires when the left side is a `Var` or an `Index`.**
Upstream says so in a comment — "Could relax this to allow more ASTs on the LHS
but this seems OK for now" — so `{ a: 1 } + { b: 2 }` keeps its `+`, and
`tests/golden/string_object_extend.jsonnet` shows both answers on adjacent
lines. Four shapes that look like they should qualify and do not, because Go
types each separately: `a[1:2]` is `ast.Slice`, `super.a` is `ast.SuperIndex`,
`f()` is `ast.Apply` (though `f().a` *does* qualify, the test being on the
outermost node), and on the right `{ [k]: 1 for k in x }` is `ast.ObjectComp`.
Each has a snippet, and the negatives are the ones worth having.

**`AddPlusObject` is the one pass with a `pass.Context`, and Go's trick does
not port.** Its context is the parent node, and it tells the parent's slots
apart with `parent.Target == *node` — a *pointer* comparison which, since
`node` is `&parent.Target` whenever the walk came through that slot, is an
identity check against a place itself. Rust has no answer to that while the
parent is mutably borrowed, so `passes::add_plus_object::Parent` is a
descriptor of the parent refined per slot, filled in by overriding the five
node hooks whose slots upstream's switch distinguishes. Those five restate the
base traversal, because a `pass::base` function takes one `ctx` and hands it to
every slot — which is a real hazard: an override that drops a slot silently
stops converting the `ApplyBrace`s in it. The guard is a snippet per slot plus
a separate counting walk over `pass::base` that no override is shared with.

**The replacement node is what goes down as the parent.**
`c.Base.Visit(p, node, passCtx{parent: *node})` sits outside the `if` and
`*node` is read after the rewrite, so where parentheses were added the children
see the new `Parens`. That is why `{a:1} {b:2} {c:3}.x` comes out with one pair
and not two.

Three things in `AddPlusObject` are inert or unreachable, each verified rather
than assumed and each reproduced anyway:

- **The fodder move is a no-op for every tree that can reach it.** The parser
  builds every `ApplyBrace` with `ast.Fodder{}`, because an `ApplyBrace` is
  left-recursive and its opening fodder lives on the leftmost leaf; and no
  earlier pass writes a node's *own* fodder — they all go through `openFodder`,
  which walks the same spine. So both slots are always empty.
- **The `ast.ApplyBrace` parent panics, and cannot be reached.** Every node
  arrives through `Visit`, which replaces an `ApplyBrace` before descending.
  The panic is reproduced on the `apply_brace` hook — one step earlier in the
  walk than Go's, on the same impossible condition — so it still fires if the
  invariant ever breaks.
- **The `InSuper` branch is a constant.** `precedence(in) <= precedence(+)` is
  8 against 6, so an `e { } in super` never gets parentheses. That is the right
  answer, `+` binding tighter than `in`. Written as the comparison because
  upstream writes it that way.

### `FixParens` collapses one level per run

The **second** non-convergence this port has found; the first is Phase 2c's,
under `jsonnetfmt` is not a fixed point below. Upstream's
`Parens` hook is an `if` and not a loop, and it hands the walk the node that
took the inner Parens' place rather than re-examining the outer one:

```
(((1)))  ->  ((1))  ->  (1)      and  ((((1))))  ->  ((1))
```

Confirmed against the pass oracle, not inferred. It is what `tk fmt` prints and
it must **not** be answered with a convergence loop, for the reason the 2c case
already gives: `rtk fmt` matching `tk fmt` on one run is the contract. No
corpus golden reaches it, so `tests/corpus.rs`'s idempotence allow-list is
still empty.

Both of that pass's fodder moves are `FodderMoveFront`, so the inner Parens'
fodder is *prepended*: a comment written between the two `(` comes out in front
of the surviving one, and with both slots occupied the two comments swap order.

### And that fodder move is a second, worse non-convergence

The same pass, a different mechanism, and six snippets rather than two.
`FodderMoveFront(openFodder(node), &innerParens.Fodder)` writes to
`openFodder(node)`, and `leftRecursive` has no `*ast.Parens` case — so for
redundant parens anywhere on the file's **leftmost spine** that slot *is* the
file's opening fodder, which `removeInitialNewlines` already cleaned at step 2,
four steps earlier. Whatever sat between the two `(` therefore lands at the top
of the file, where nothing will clean it again:

```
(\n  (1)\n)     ->  "\n(1\n)\n"        a leading blank line, body unindented
(\n\n  (1)\n)   ->  "\n\n(1\n)\n"      two leading blank lines
( // a\n(1))    ->  "  // a\n(1\n)\n"  the file starts with two spaces
```

Unlike `(((1)))`, the first run's output here is not merely unsettled — it is
**visibly malformed**, and it is what `tk fmt` prints. Expect it to be reported
as an rtk bug. `fix_parens/three_opens_with_comments` is the sharpest of the
six: it reorders *comments* across runs rather than whitespace.

This is also the answer to what is load-bearing about step 2's position, and it
is not what this file used to claim. Step 2 before step 3 is **not**
load-bearing — `EnforceMaxBlankLines` only writes `Blanks` while
`removeInitialNewlines` deletes whole elements, so deletion subsumes clamping
and swapping them is output-identical. What matters is step 2 running *before*
`FixParens`, which can dirty the slot again afterwards. The same correction
applies to steps 13 and 14: `setIndents` writes only `Indent` and
`removeExtraTrailingNewlines` only `Blanks`, so those two commute as well.

`FixParens` overrides `visit` rather than `parens`, which is where upstream
overrides. `ast::Node` keeps the fodder that `openFodder(node)` returns, and
the `parens` hook is handed a `Parens` payload that does not carry it. The move
is unobservable: `Base.Visit` visits the open fodder and then dispatches, so
upstream's collapse happens after that visit and this one happens before it —
and the pass does not override any fodder hook, so the base traversal over
fodder is a no-op either way.

### A sixth upstream oddity, and the most serious one

`AddPlusObject`'s switch has no `ast.Slice` case, so a slice target takes the
default branch and gets no parentheses: `{a:1} {b:2}[1:2]` is written back as
`{a:1} + {b:2}[1:2]`, which parses as `{a:1} + ({b:2}[1:2])` — a different
tree. A slice binds exactly as tightly as an index, so this is the same bug the
`ast.Index` case exists to prevent, in the one node kind the case does not
name. It is the only one of the upstream oddities this port carries that
changes what a file evaluates to. Reproduced rather than fixed, because
matching `tk fmt` is the contract; `add_plus_object/slice_target` pins it, and
it is graded by the oracle rather than by a reading of the source.

It reaches nothing in practice: `tk fmt` never runs `AddPlusObject`.

### Four of the fourteen steps are not passes

Worth knowing before looking for one of them in `src/passes/`.

`SortImports` (step 1) is a free function over the whole file that rebuilds the
top of the tree rather than visiting it, so `src/pass.rs` does not apply to it
at all. It lives in `src/sort_imports.rs` with its entry point as
`ast::Node::sort_imports`; it *is* reachable by the pass oracle, because
`SortImports` is exported. See the section above.

`removeInitialNewlines` (step 2) and `removeExtraTrailingNewlines` (step 14)
are unexported four-line functions in `jsonnetfmt.go`, so **the staged pass
dumper cannot reach either**. They live as inherent methods on the types they
mutate, `ast::Node` and `fodder::Fodder`.

**The corpus grades them far less than it looks, and this was measured.** Only
**3 of 138** corpus files begin with a blank line, and all three have an
`Object` root — which is not left-recursive, so `opening_fodder_mut()` returns
the node's own fodder and a port that wrote `self.fodder` directly would be
indistinguishable. The spine walk, which is the only subtle thing
`remove_initial_newlines` does, is therefore graded by **nothing**, and the
function has no unit test either. The observable divergence is a file that both
starts with a blank line and has a left-recursive root, e.g.
`\n(import 'a') + (import 'b')`. Worth two snippets.

`remove_extra_trailing_newlines` is weaker still: exactly one corpus file ends
in a comment and it has no blank line after it, so the function is a **no-op on
all 138**. Its two unit tests in `src/fodder.rs` are all that grade it. A file
ending `// x` and two blank lines is the trigger.

Their *position* is not load-bearing in the way this file used to say — see the
`FixParens` fodder-move section above for the correction and for what actually
is.

Phase 2d decided **not** to close that gap by making `_staged/passdump.go` a
`_test.go` inside `internal/formatter`, which the plan had left open. Both
functions are a slice truncation and a field assignment — neither *composes*
fodder, which is the one category this project has measured itself unreliable
at deriving by hand — and the conversion means renaming packages and dropping
the dumper's CLI to grade eight lines the corpus already covers end to end. If
a later phase does want pipeline-level answers for hand-written input, there is
a better route than the `_test.go`: `testdata/generate/main.go` already calls
the **public** `formatter.Format`, so a snippets mode there needs no staged
checkout at all.

`FixIndentation` (step 13) is not a pass for a different reason: `FormatNode`
calls `visitor.VisitFile(node, finalFodder)` on it directly rather than through
upstream's `visitFile` helper, so it has its own `Visit(expr, currIndent,
crowded)` and walks the tree itself. It lives in `src/fix_indentation.rs`, not
`src/passes/`, and `src/pass.rs` does not carry it. The consequence that
matters: **it reaches all four fodder slots `pass::base` skips**, which is why
a `#` comment in `x in super` survives `EnforceCommentStyle` and is still
re-indented.

### `removeExtraTrailingNewlines` only ever fires on a file ending in a comment

Not what the name suggests, and it was a wrong test expectation before it was a
note. The lexer's main loop measures a whitespace run and *then* tests for end
of input, breaking before it adds the line end — so trailing newlines never
become fodder at all. A file of `1` and four newlines has **empty** final
fodder. The only way blank lines survive to the end of a file is on a comment's
own element, whose blanks `lex_until_newline` measures while a following token
is still in prospect.

### What `FixIndentation` gets wrong on purpose

Five shapes where a near-miss would sit, each pinned by a snippet, plus one
upstream bug. The full account is the module documentation on
`src/fix_indentation.rs`; the two that are easiest to transpose:

- **`fill_last`'s last element takes a different indent from the rest, and
  which differs per node.** `Apply`, `Array`, `Object`, `Parens`, `Index` and a
  `local` bind's close fodder all end on `currIndent.base`; `params` ends on
  `currIndent.lineUp`. One character apart in the source.
- **`Slice` never fills `right_bracket_fodder`.** Every other bracketed node
  calls `fill_last` there. So a newline before a slice's `]` keeps whatever
  indent it was lexed with.

The bug is in `specs`: the conditions loop computes its indent from
`openFodder(spec.Expr)` and then calls `Visit(spec.Expr)` — the `for`
expression, **not** `cond.Expr`. So a comprehension's `for` expression is
indented twice at two different columns and the second answer wins, while the
`if` condition is never indented at all. Reproduced, not fixed; it is what
`tk fmt` prints.

### Idempotence is tested, and stays a hard assertion

Two tests, and the weaker one came first.

`tests/corpus.rs` formats each of the 857 answers a second time and requires no
movement. `KNOWN_NON_CONVERGENT` beside it is **empty**: a failure is a bug in
`FixIndentation` or `FixNewlines` until it is traced to a specific cross-pass
interaction in `FormatNode`'s order, at which point it earns an entry here and
an exclusion by name. Do not add a convergence loop — see the non-fixed-point
note below for why that would diverge from `tk fmt` outright.

`OUTPUT_DOES_NOT_REPARSE` beside it holds **three**, all from Phase 4's
go-jsonnet set and none reachable from the in-repo 138. That list is for the
worse class — an answer whose next run the parser refuses — and it exists
separately so that the three cannot hide among ordinary non-convergences. Both
lists ratchet both ways.

**An empty allow-list over the goldens is a weaker statement than it reads**,
because a golden is by construction already a fixed point of go-jsonnet's own
`Format` — which is exactly why the three that *do* break it are worth having.
The
inputs written to be awkward are the snippets, and nothing ran `format` over
any of them until `tests/idempotence.rs`, which sweeps both snippet families and
carries the twelve names that do not settle with the seam behind each. It also
asserts the class that would be worse than any of them — a first run whose
output no longer parses — and it ratchets **both** ways, so a listed name that
starts settling fails as loudly as an unlisted one that moves.

**Phase 2f is the other phase that could plausibly have broken it**, and for a
reason no earlier pass had: `SortImports` is the only step that moves fodder
*between different nodes*, so a second run sees a tree whose comments and blank
lines are attached to different imports than the first run did. It did not
break. The reason is that its output is a fixed point of itself by
construction — every seam has already been through
`FodderEnsureCleanNewline`, so `splitFodder` divides it the same way the second
time, and a sorted group re-sorts to itself. The allow-list is still empty.

Phase 2e is the phase that could most plausibly have broken this — `FixParens`
and the plus-object passes rewrite node structure rather than fodder, and
`FixParens` is itself non-convergent on `(((e)))`. It did not: no corpus golden
contains a doubly-parenthesised expression, and `RemovePlusObject`'s output
reparses directly as the `ApplyBrace` it produced. The list is still empty.

**Whether a pass runs is `format`'s business, never the pass's.** `FormatNode`
gates `EnforceStringStyle` on `StringStyle != Leave` and `EnforceCommentStyle`
on `CommentStyle != Leave`, so `Leave` means the pass is never constructed. Run
with `Leave` anyway, `EnforceStringStyle` would behave as `Double`, since it
asks only whether the style is `Single`.

### Representation: strings and comments

Two passes, and between them they hold most of what a reader would get wrong.

`EnforceStringStyle` overrides `LiteralString`, **not** `Visit`, because
`pass::base::import` reaches an import's filename through that leaf hook only —
overriding `Visit` would restyle every string in a file except the one in
`import 'foo.libsonnet'`. It is also a full `StringUnescape`/`StringEscape`
round trip, so it normalises more than the quote: `"a\/b"` becomes `'a/b'`,
`"\u0041"` becomes `'A'`, and `"\u009F"` becomes `'\u009f'` because
`StringEscape` formats with `%04x`. The option is consulted once and then
overridden by the text — a `'` in it forces double quotes and a `"` forces
single — and a string containing **both** is returned on untouched, keeping the
kind it was written with. Blocks and both verbatim kinds are returned on
unexamined; for the verbatim ones that matters, since the parser has already
collapsed their doubled quotes.

`EnforceCommentStyle` carries `seenFirstFodder`, and **the hashbang guard
`return`s before setting it**. So a spared `#!` never counts as fodder seen and
a second `#!` is spared too. The flag is otherwise set by any non-interstitial
element whether or not anything was rewritten, which makes three things true
and each is pinned by a snippet: one blank line at the top of a file disables
the carve-out, an interstitial does not set the flag (so `/* c */ #!b` on **one
line** still spares it), and a comment already in the target style sets it
without being changed. Reading `addFodder` against `addFodderSafe` in the lexer
is what explains the last group — a multi-line C comment goes through
`FodderAppend`, which inserts a synthetic `LineEnd` in front of a paragraph
appended to empty fodder, and it is that element which sets the flag.

`EnforceCommentStyle` also cannot reach two of the four slots the traversal
skips, so `tk fmt` leaves those comments as written: `{ a: 'b' # c` … `in
super }` and `a. # c` … `b`. The same comment after `super.` *is* rewritten,
because `base::super_index` visits `id_fodder` unconditionally.

### `jsonnetfmt` is not a fixed point

`docs/rtk-fmt-plan.md` assumed it was, and Phase 2c found a two-line
counterexample:

```
a['fo\u006f']   ->   a['foo']   ->   a.foo
```

`PrettyFieldNames` (step 10) is asked whether `fo\u006f` is an identifier,
because a fully escaped string keeps its escapes in its *stored* value. It sees
a backslash and keeps the brackets. `EnforceStringStyle` (step 11) then
unescapes it. So the output carries brackets justified by an escape that is no
longer in it, and formatting that output again promotes the index.

This is upstream's, not a port bug — each pass matches go-jsonnet in isolation
and the order is `FormatNode`'s — and it is what `tk fmt` prints. Two
consequences:

- **Do not add a convergence loop.** `cmds/jrsonnet-fmt` needs `--conv-limit`
  because the dprint formatter is not a fixed point; `rtk fmt` must not gain
  one, or it turns `a['fo\u006f']` into `a.foo` in a single run and diverges
  from `tk fmt` on the first escaped field lookup in a Grafana repo.
- **Idempotence stays the default expectation.** A new non-convergence is a bug
  until it is traced to a specific cross-pass interaction in upstream's order.

There are **five** mechanisms and **seventeen** inputs, and every count is
measured rather than reasoned. `tests/idempotence.rs` formats every snippet
twice; `tests/corpus.rs` formats every corpus answer twice. Between them they
list the twelve that move with the seam that does it, and the five they
destroy. None is a reason for a convergence loop — and for the last two a loop
would not even help, since the second run fails rather than moving.

| mechanism | steps | inputs | class |
| --- | --- | --- | --- |
| `PrettyFieldNames` reads a *stored* value `EnforceStringStyle` then unescapes | 10, 11 | 2 snippets | moves |
| `FixParens`: one level per run, **and** its fodder move outward | 2, 6 | 8 snippets | moves |
| `SortImports` keys on a *stored* value `EnforceStringStyle` then unescapes | 1, 11 | 2 snippets | moves |
| `PrettyFieldNames` has no `ObjectComp` guard | 10 | 2 corpus + 2 snippets | **refused** |
| a `\|\|\|` block of nothing but newlines loses its indent | lexer/unparser | 1 corpus | **refused** |

**The last two are a different class and are kept apart from the first three.**
A file that formats to a different file is unsettled; a file that formats to one
the parser **refuses** is destroyed, and `tk fmt` writes in place. So both
`tests/corpus.rs` and `tests/idempotence.rs` carry two lists rather than one —
`KNOWN_NON_CONVERGENT` and `OUTPUT_DOES_NOT_REPARSE` — and all four ratchet both
ways. Keeping one list per file would let the worse class hide among the
ordinary one.

`idempotence.rs` did not have that second list until Phase 4, and its absence
had been the right default: no snippet reached the class, so the assertion was
unconditional and said so. What changed is what is known about upstream, not the
port.

Phase 4 is what found them, and only its second corpus set can: the in-repo 138
reach none of the five. See **The formatter corpus** below for why 719 files of
go-jsonnet's own testdata bought three inputs rather than a match count.

The two new mechanisms in full:

- **`PrettyFieldNames` un-brackets a computed field inside an object
  comprehension.** `{ ['foo']: 1 for x in [1] }` becomes
  `{ foo: 1 for x in [1] }`, and a comprehension may only have `[e]` fields, so
  the parser then says `Object comprehensions can only have [e] fields`. There
  is no `ObjectComp` guard in the pass, and the field is rewritten wherever it
  is found. This is the **only** oddity the port carries that turns a working
  file into one that will not parse — worse than `AddPlusObject`'s missing
  `ast.Slice` case, which at least produces a valid tree, and worse for being
  reachable from `DefaultOptions`, which `AddPlusObject` is not. The pass
  oracle attributes it to `PrettyFieldNames` and to no other pass.

  **The obvious bound on it is wrong, and was measured wrong on the first
  try.** `pretty_names/field_computed_in_object_comp_not_an_identifier` was
  written as the negative that would confine the damage to identifier-shaped
  names, and it is not one: `{ ['a b']: 1 for x in [1] }` keeps its quotes but
  **still loses its brackets**, coming out as `{ 'a b': 1 for x in [1] }`,
  which is refused exactly as the identifier case is. So the rule is *any
  string-literal field name in an object comprehension*, not any
  identifier-shaped one. What survives is a genuinely computed name,
  `{ [k]: 1 for k in … }`, and that is measured too: five other snippets in
  `pass-snippets.json` are comprehensions with a computed name and all five
  pass the idempotence sweep, which is why there is no sixth snippet restating
  it.
- **A `|||` block whose value is nothing but newlines.** `write_block_string`
  emits a blank value-line with **no indent** — upstream's, and already
  documented on that function — so a block whose every line is blank comes back
  with no content line carrying `BlockIndent` at all. The lexer's
  leading-blank-line loop then runs *before* the indent is calculated, takes the
  indent from the terminator line, and swallows the `|||` as content:
  `Text block not terminated with |||`. Two lexer snippets pin the halves,
  `text_block_only_whitespace_lines` and `text_block_only_newlines`.

Both were confirmed against the real `tk` rather than traced by reading Go, and
the confirmation is unusually strong: rtk and tk agree byte for byte on the
first run, and on the second they agree on the refusal, its message *and* its
location.

**How the count was got wrong first is the part worth keeping.** The prediction
was five, derived by grepping `pass-snippets.json` for `((`. That pattern cannot
match `(\n  (1)` — which is the shape that actually matters — so six
`FixParens` snippets were invisible to it, and they had been sitting in the file
since Phase 2e. A grep over inputs is not a measurement over outputs. The same
warning is already written three times in this file about deriving fodder by
hand; this was the same error one level out, about deriving *coverage* by hand.

The `SortImports` one had been *named* here and not counted: the bullet in that
pass's section says the sort key is the stored value and calls it "the same seam
as 2c's non-convergence", which stops one inference short of saying it **is**
one. It is the only one of the three that reorders code rather than layout, and
the imports carry their comments with them when they move.

How it surfaced is the reusable part: a Phase 2b unit test asserted that
`format("a['fo\u006f']")` came back unchanged, and it passed for a whole phase
because the pass that disproves it did not exist yet. **A whole-pipeline
assertion written while the pipeline is half-built records the half-built
answer.** Prefer asserting one pass's own effect — with the other passes'
options set to `Leave` where that isolates them — and keep pipeline answers in
tests that say so.

### The pass traversal

`src/pass.rs` ports `internal/pass/pass.go`, and ten of the twelve passes are
an override of one or two of its methods, so its shape decides theirs. The
other two are `FixIndentation`, which has its own walk, and `SortImports`,
which is not a visitor at all. Two things to know before writing a pass:

- **Go's `p ASTPass` first parameter is gone**, because a Rust trait with
  provided methods already dispatches to the outer pass — that parameter is Go
  simulating virtual dispatch through embedding. What Rust lacks is `super`, so
  the base traversal is a free function per hook in `pass::base`; the trait's
  defaults delegate to those, and an overriding pass calls the same function
  where Go writes `c.Base.Array(p, node, ctx)`. They are not free functions
  standing in for private methods.
- **`Context` is an associated type**, because Go's is `interface{}` and exactly
  one pass uses it: `AddPlusObject` carries the parent node. It compares the
  parent's child against the current node by pointer, which Rust cannot do
  while the parent is mutably borrowed, so Phase 2e carries
  `passes::add_plus_object::Parent` — a descriptor of the parent refined per
  slot — instead. That was scoped when the trait was written rather than
  discovered afterwards, and it landed without changing the trait. It is also
  why `AddPlusObject` is the one pass that overrides more than two hooks: the
  refinement has to happen in the hooks, since a `pass::base` function takes
  one `ctx` and hands it to every slot, and so the five it overrides restate
  the base traversal. An override that drops a slot silently stops converting
  the `ApplyBrace`s in it, which no fodder pass could have done — see the
  Formatting section above for the guard.

Four slots the traversal **never visits** are each pinned by a unit test,
because a pass that rewrites fodder will silently not reach them: `InSuper`'s
`in_fodder` and `super_fodder` (so a comment in `x /* c */ in super` is never
touched), `Index`'s `right_bracket_fodder` when the index is an identifier,
`Apply`'s `tail_strict_fodder` without `tailstrict`, and a `Parameter`'s
`eq_fodder` without a default. All four are upstream's.

### The corpus cannot grade a pass

Worth knowing before reading a corpus count as progress in either direction.
Jsonnet that is already `tk fmt`-clean gives a pass nothing to do, so the
breadth corpus measures each pass least where it is newest. Changed cells over
all 138 files, from the pass oracle: `AddPlusObject` 18, `FixIndentation` 9,
`EnforceStringStyle` 6, `PrettyFieldNames` 3, `RemovePlusObject` 3,
`FixNewlines` 1, `SortImports` 1, `FixTrailingCommas` 1, and **zero** for
`NoRedundantSliceColon`, `EnforceCommentStyle`, `EnforceMaxBlankLines` and
`FixParens`. `Options` does not isolate them either — three passes have a flag,
the rest are unconditional in `FormatNode`.

So **write the snippets before the pass**, every time. `FixIndentation` is the
pass the plan calls hardest and it gets nine corpus cells; `EnforceCommentStyle`
gets none.

Phase 2f is the case where this stopped being a coverage argument and started
changing an answer. `SortImports` had **1 corpus cell and 0 of 361 snippets**
going in. 76 snippets went in before a line of the pass, and fifteen of them
exist only to ask what `sort.Slice` does with tied keys — which is how the
decision to port Go's pdqsort instead of calling `sort_by` was reached. The one
corpus file is a plain reversal of fourteen distinct paths; it could not have
raised the question, let alone answered it. **A snippet set is also how you
find out what you do not know yet.**

Phase 2c is the sharpest case of this and generalises the reason. A `#` comment
in a file that is already `tk fmt`-clean has by definition already been
rewritten to `//`, so no breadth corpus of real files can contain the input
`EnforceCommentStyle` exists for. The same argument applies to the three
remaining zero-cell passes. 2c added 57 snippets before either pass was
written, taking `testdata/pass-snippets.json` from 56 to 113.

One authoritative `tk fmt` answer for `EnforceCommentStyle` does exist outside
the snippets and is free: go-jsonnet's own
`formatter/testdata/empty_comment.fmt.golden` is `#` above an empty object
formatting to `//`, graded as the `go_jsonnet/empty_comment` fixture whenever
`GO_JSONNET_FOR_TESTS` is set.

Phase 2d is the sharpest case *numerically*, and it is the one to cite next
time. `EnforceMaxBlankLines` was graded by **nothing at all** before 2d — 0 of
138 corpus files and 0 of the 113 snippets then in the file — for the same
structural reason: a `tk fmt`-clean file has no run of three blank lines in it
by definition. `FixNewlines` had 1 and 1, and `FixIndentation` 9 and 2, and
both of those snippet hits were accidents of the comment-style group rather
than cases written for the pass. 2d added 141 snippets before a line of pass
code, taking `testdata/pass-snippets.json` from 113 to 254, and all three
passes then matched go-jsonnet node for node on every one and on all 138 corpus
files on the first compile — as the lexer, the node dumper and the 2b passes
each had.

**The count split is the argument in one line.** 2d's three small steps moved
the corpus by **zero**; `FixIndentation` moved it by **ten**. Had the phase
been graded on the corpus alone, four of the five things it landed would have
looked like no-ops.

Phase 2e is the sharpest case for **why it matters**, rather than for how big
the gap is. Before it, `FixParens` was graded by 0 of the 138 corpus files and
0 of the 254 snippets — *nothing, in either direction* — while being one of the
two passes whose bugs change what a file evaluates to. `RemovePlusObject` had
three corpus files and no snippets. `AddPlusObject` had 18 oracle cells and one
snippet, and that one hit was an accident of 2d's `indentation/apply_brace`
rather than a case written for it; worse, its 18 cells can never become corpus
*flips*, because the corpus is generated with `DefaultOptions`, which takes
`RemovePlusObject` and skips `AddPlusObject` entirely. So the corpus grades it
at zero by construction and always will.

2e added 107 snippets before a line of pass code, taking
`testdata/pass-snippets.json` from 254 to 361: `fix_parens` 25,
`remove_plus_object` 32, `add_plus_object` 50. Over them the oracle records
`FixParens` changing 23, `RemovePlusObject` 19 and `AddPlusObject` 50 — against
0, 3 and 18 over the real files. All three then matched go-jsonnet node for
node on every snippet and all 138 corpus files on the first compile, as every
phase since the lexer has.

The 13 `RemovePlusObject` no-ops are all *intended* negatives and are the most
valuable part of that group: they are what says the `Var`-or-`Index`
restriction, the plain-`Object` requirement and the `+`-only test are each
real. A pass that over-generalised any of them would still be green on every
positive case.

### The oracle is a lower bound on the work, not a sufficient set

The refinement 2d adds to the two warnings above, and the one to carry into 2e.
`pass-oracle.json` answers "which passes change this file's AST" exactly. It
does **not** answer "which passes does this file need to match its golden",
and the two come apart.

`test_fixtures/golden_envs/yaml_line_wrapping_env/main.jsonnet` is the only
corpus file the oracle attributes to `FixNewlines` — and landing `FixNewlines`
did not flip it. It took `FixIndentation` as well, because `FixNewlines` only
ever inserts a bare `LineEnd(0, 0)` and something then has to indent it. So the
corpus staying at 124 through the first half of 2d was correct rather than a
regression, and a prediction built by counting oracle cells per pass would have
called that half a one-file win.

Use the oracle to say which files a pass *can* affect and to grade the pass
node for node. Use the corpus, and only the corpus, to say what is finished.

The converse is just as wrong, and 2c made that mistake after being warned
about this one: **an input-against-golden diff says what is different, never
which pass does it.** `tests/suite/sjsonnet_issue_1029.jsonnet` is one line
whose diff is dominated by `x*x` -> `x * x` and `[1,2,` -> `[1, 2,`, which the
round trip fixes for free; the `error "3"` -> `error '3'` in the same line was
read as part of that and the file was filed as already passing, which put the
2c corpus prediction one file low. `pass-oracle.json` answers the question
exactly, per file and per pass. Query it rather than reading a diff.

One inference to avoid, because it was made here and was wrong: **a pass
changing a file is not the same as that file's output changing.**
`FixTrailingCommas` does change `tests/suite/std_param_names.jsonnet`, and
`PrettyFieldNames` does change `tests/golden/builtin_strings_string.jsonnet`,
but both files still fail the corpus because they also need passes that do not
exist, so the effect is masked in the byte comparison. Reading the
input-against-golden diff had said neither pass touched anything.

So `make update-fmt-pass-oracle` records the AST each pass leaves behind, in
the node oracle's notation, and `tests/pass_parity.rs` grades it at full parity.
Three things about it:

- It is **staged into a go-jsonnet checkout**, like the lexer oracle, because
  `internal/formatter` may only be imported from inside that module. Unlike the
  lexer's it needs no `_test.go` trick: everything it touches is exported from
  the internal package, so a `package main` in `<checkout>/rtkpassdump/` is
  enough. The source lives in `testdata/generate/_staged/`, named for the go
  tool's rule that a `_`-prefixed directory is ignored — it would not compile
  in the generate module.
- Each pass runs **in isolation on a fresh parse**, not on the pipeline's
  accumulated state. A cumulative dump is only correct once everything before
  it is written, so it could grade nothing until Phase 2f; isolation is what
  lets passes land one at a time.
- Most cells record `unchanged` rather than the tree, and the Rust side then
  has to prove its own pass is a no-op there too. `testdata/pass-snippets.json`
  holds the inputs, which are safe to write by hand — the answers are not, and
  that is the point.

**The three strip passes are deliberately not dumped.** Each rewrites every
tree it touches, so two of them alone were about 270 of 356 changed cells and
most of a 23 MB oracle — regenerated whole on every go-jsonnet bump and
unreadable in a diff. All three are skipped under `DefaultOptions`, the only
configuration `tk fmt` uses. Adding one back is a line in `passNames`.

The counts are the argument for the oracle existing: over the snippets
`FixTrailingCommas` changes 15, `PrettyFieldNames` 15, `NoRedundantSliceColon`
2 — against 1, 3 and 0 over the 138 real files. All three then matched
go-jsonnet node for node and slot for slot on every snippet and every corpus
file, which is what Phase 2b rests on.

`removeInitialNewlines` and `removeExtraTrailingNewlines` are unexported
functions rather than passes, so even the staged program cannot reach them.
Both are four lines and the corpus covers them end to end.

`AddPlusObject` is the one pass the oracle grades and the corpus cannot, for
the structural reason above rather than for want of coverage. Its end-to-end
grading is the nine `no_implicit_plus/` fixtures, which run with
`use_implicit_plus: false` — the only configuration that reaches it.

Every fixture the current state gets wrong is listed in
`crates/rtk-jsonnetfmt/quarantine.toml` with a reason, and `tests/fixtures.rs`
asserts that list only ever shrinks: an unlisted failure, a listed fixture that
now passes, and an entry naming no fixture all fail the test. That is how passes
land one at a time without deleting cases.

### Spacing is not fodder

The single most useful thing to know before writing a pass. Fodder records line
ends, blank counts, indents and comments — **and nothing else**. Every space
*within* a line is regenerated by the unparser from `crowded`, `separate_token`
and `PadArrays`/`PadObjects`, so `{a:1,b:2}` becomes `{ a: 1, b: 2 }` with no
pass involved at all. Eleven corpus files improved when `format` started
parsing, and only three of those were the parse errors.

Indentation is the exception and is **not** free: it comes from
`fodder.indent`, which the lexer filled counting a tab as 8, so a tab-indented
file still needs `FixIndentation`. Do not reach for a pass to fix a spacing
diff until you have checked which of the two it is.

### The node oracle

`make update-fmt-node-oracle` dumps the AST go-jsonnet's parser produces —
every node, and every named fodder slot — to `testdata/node-oracle.json`, and
`tests/node_parity.rs` grades the port against it. It asserts **full parity**
with no baseline ratchet, because a parser that is right for 130 of 138 files is
not a partial formatter but a wrong parser, and every pass built on it inherits
the error. A divergence reports its path, so a misplaced `CommaFodder` reads as
`$.Fields[2].CommaFodder` rather than as a whitespace diff hundreds of lines
away.

Unlike the lexer oracle it needs **no staged checkout**: every fodder slot is an
exported field of package `ast` and `formatter.SnippetToRawAST` is public, so
`testdata/generate/nodedump/` is an ordinary program in the generate module.
Building it before the parser is what made the parser match on its first
compile, and it caught a wrong expectation before a line of parser existed —
`a[::]` parses to the same tree as `a[:]`, because `::` lexes as one operator
token and the parser's `::` branch never assigns `StepColonFodder`.

### Parse errors are graded separately, and locations only here

`testdata/parse-error-snippets.json` is a third family out of the same
`nodedump` program, graded by
`node_parity::the_parser_matches_go_jsonnets_refusals_exactly`. It exists
because the node oracles **drop every location on purpose** — the formatter
reads one in exactly one place and discards it — while
`staticError.Error()` is `"{loc} {msg}"` and a parse failure aborts the whole
`tk fmt` run. So the position is contract and the tree oracles cannot see it.

`dumpOne` already records `err.Error()`, so this file needed no Go change. What
it must not do is live in `node-snippets.json`: `node_oracle.rs` asserts
go-jsonnet refuses none of those, because a snippet it refuses pins no fodder
slot. Here the requirement inverts and **every** snippet must be refused,
which is asserted — `first_divergence` falls through to an AST comparison on an
empty error, so a snippet that started parsing would otherwise pass while
grading nothing.

What it closed is worth remembering as a shape, not just a gap.
`src/parser.rs`'s `assert_message` asserts the body with `ends_with` and defers
the location "to the oracle", and the node and pass snippet oracles hold
**zero** error cells between them over 63 and 437 entries. So the deferral was
to nothing, and 26 of go-jsonnet's 29 parser-error templates could have
reported any line, any column and any of `LocationRange`'s three renderings
with the suite green. **A test that defers a claim elsewhere is only as good as
the elsewhere; check that the other place actually asserts it.**

The three pre-existing location answers were an accident rather than a design:
the three corpus files that happen not to parse. And one of the three
`tests/fixtures/parse_errors/` goldens — `unclosed_brace`, at `2:1` — is still
the only graded location go-jsonnet never produced; its README says it was
derived by reading. The two remaining lexer text-block errors went into
`testdata/lexer-snippets.json` at the same time, which is where that family
lives.

### Five upstream oddities the port reproduces

Each is verified in go-jsonnet's source, each is what `tk fmt` prints, and none
should be "fixed":

- `tokenStringToAst` validates a string by wrapping `StringUnescape`'s
  already-rendered error in a fresh one at the same token, so **the location
  appears twice** in the message.
- Both malformed-unicode-escape messages interpolate `s[0:4]` — the first four
  bytes of the *whole string*, not of the offending escape — so `'ab\uZZZZ'`
  reports `ab\u`.
- `NamedArgument.EqFodder` is filled by the parser and never read by the
  unparser, which writes `=` directly. So `f(b /* x */ = 2)` **loses that
  comment**. The parser must still store it or the AST will not match.
- `PrettyFieldNames.Index` does `index.RightBracketFodder = lit.Fodder` — an
  assignment, not a `FodderMoveFront` — and that slot doubles as the fodder
  before a `]` and the fodder before an identifier. So `a['foo' /* c */]`
  formats to `a.foo` and **drops the comment**. The object-field path in the
  same pass uses `FodderMoveFront` and keeps everything, which is what makes
  this look like an oversight rather than a decision; reproduce it anyway.
- `EnforceCommentStyle`'s hashbang guard `return`s **before** setting
  `seenFirstFodder`, so a spared `#!` never marks fodder as seen and a second
  `#!` is spared as well. See the representation section above for the three
  things that *do* set the flag.

Phase 2e added two more, each with a section of its own above because each is
about behaviour rather than about a slot: `FixParens` collapsing one level of
redundant parentheses per run, and `AddPlusObject`'s switch having no
`ast.Slice` case.

Phase 4 added two more again, found by extending the corpus with go-jsonnet's
own testdata, and they are **the worst two**: `PrettyFieldNames` promoting a
computed field inside an object comprehension, and a `|||` block of nothing but
newlines losing its indent across the round trip. Both make `tk fmt` write a
file that no longer parses. The account of each is under `jsonnetfmt` is not a
fixed point above.

So the ranking, since it keeps being asked: `AddPlusObject`'s missing
`ast.Slice` case changes what a file evaluates to but is unreachable under
`DefaultOptions`; the two Phase 4 oddities destroy the file outright and are
reachable from the defaults `tk fmt` uses. Everything else on this page moves a
comment or an indent.

There is deliberately no `fmt_golden_override/`. For fmt, every override would
be a divergence from `tk fmt` — a bug.

`GO_JSONNET_FOR_TESTS` names a go-jsonnet checkout, and grades
`formatter/testdata/*.fmt.golden` as the `go_jsonnet/` fixture family. Those
goldens are free and authoritative: they are generated by `Format(name, input,
DefaultOptions())`, the identical call `tanka.Format` makes, so matching them
*is* matching `tk fmt` with no `tk` binary in the loop.

**Unset, it falls back to `target/go-jsonnet`**; `go_jsonnet_checkout` in
`tests/fixtures.rs` does it. This used to say the oracle targets leave a
checkout there, and **none did** — `update-fmt-node-oracle` takes go-jsonnet
from the module cache, and the lexer and pass oracles clone into `mktemp -d`
and delete it — so outside the nix devShell, CI included, the family went on
grading 12 of 15. Phase 4 added `make go-jsonnet-checkout`, which clones the
pinned version there and is what makes the fallback true; run it once and every
later `cargo test` grades all fifteen. CI gets it as a side effect of
`check-fmt-corpus`, whose second corpus set reads the same checkout. Before any
of this the variable appeared only in the oracle-regeneration targets, so an
ordinary run graded the family at zero — and
there are only three fixtures in it, but two carry answers nothing else in the
suite has: `empty_comment` is the only end-to-end `EnforceCommentStyle` case, a
pass that changes **0** of 138 corpus files, and `regular_expression` is the
only authoritative answer for a parse error's doubled location *and* its range
shape (`3:11-29` — an end column, not a point). With the checkout present the
family count is **15 graded, 15 passing**; without it, 12.

The fallback lives in the test and **not in the Makefile**, which is where it
was put first. That version graded the family under `make test` while a plain
`cargo test -p rtk-jsonnetfmt` still skipped it and printed `12 graded` — and
the second is the command anyone actually runs while working. A skip that
depends on the entry point is the same hazard as a skip that depends on a
missing file: green either way, and saying nothing.

### Fodder, and why the round trip is not the identity

`src/fodder.rs` ports `ast/fodder.go`. **Fodder is not a trivia stream**, and
mistaking it for one is the way to get every layout pass subtly wrong. It is a
normalised model of vertical space: blank lines are a *count*, indentation is a
count of *spaces* with a tab counted as **8** by the lexer, and comments are
pre-trimmed lines. `MakeFodderElement` enforces per-kind invariants, and
`FodderAppend` enforces one across elements — a `LineEnd` may not follow a
`LineEnd` or a `Paragraph`, so appending one either merges it into its
predecessor or promotes it to a `Paragraph`. Extend fodder only through
`Fodder::append`, never by pushing.

`src/unparse.rs` ports the unparser's `fodderFill`. It *generates* text from
that model rather than copying source, which is why **`unparse(parse(x)) == x`
is false in go-jsonnet itself**, with every pass disabled:

- a line-end comment is always preceded by exactly **two** spaces
- indentation is `indent` spaces, so a tab-indented file comes back spaced
- `\r` is dropped from block strings — "Formatter always outputs in unix mode"
- trailing horizontal whitespace is stripped at lex time and never restored

`docs/rtk-fmt-plan.md` originally gated Phase 2a on that round trip. It is
corrected in place; do not reinstate it.

### The lexer, and the oracle that grades it

`src/lexer.rs` ports `internal/parser/lexer.go` — the **formatter's** lexer,
which keeps whitespace and comments as fodder rather than discarding them. That
is why jrsonnet's lexer cannot be reused for this.

Graded by `make update-fmt-lexer-oracle`, which is worth understanding before
changing it. Fodder is invisible from outside go-jsonnet: `internal/parser` is
an internal package, and even inside the module `token`'s fields are
unexported. So the target stages a `_test.go` into `internal/parser/` of a
checkout and dumps the real values from inside it. It reuses
`GO_JSONNET_FOR_TESTS` when set and otherwise clones the pinned version.

**All 138 corpus files match token for token and fodder for fodder.** Read that
for what it covers: 35 of the 37 token kinds, all three fodder kinds, 55 block
strings, 644 paragraphs and 5 interstitials — but **no verbatim strings and not
one lexer error**, so none of the messages are graded by it. The unit tests in
`src/lexer.rs` exist for exactly that gap, and a new one belongs there whenever
the oracle cannot reach a behaviour.

`testdata/lexer-snippets.json` covers that gap with an oracle rather than by
hand: the Go dumper has a snippets mode, so those expectations come from
go-jsonnet too. It exists because the hand-written version was measurably
unreliable — of 16 expectations derived by reading Go's source, **14 were right
and 2 were wrong**, and both misses were the same kind of error: a `LineEnd` the
*model* inserts that the source does not contain. `FodderAppend` puts one in
front of a paragraph appended to empty fodder, so a file beginning with a
multi-line C comment carries a synthetic line end — which is a large part of
why `removeInitialNewlines` exists. **Write no fodder expectation by hand when a
dump can supply it**; that is the standing lesson for the parser, where fodder
is composed rather than read at nearly every node.

Three upstream oddities are reproduced on purpose. Do not "fix" them:

- **A tab counts as 8 spaces** of indent, and nothing downstream recovers it.
- **The operator wind-back reads its deciding character once.** Upstream's
  `for r = rune(l.input[l.pos.byteNo-1]); …; l.pos.byteNo--` never reassigns
  `r`, so it winds back to a single rune whenever that character is one of
  `+ - ~ ! $`. `+++` therefore lexes as three separate `+` operators rather
  than one operator.
- **The `allStar` hack in C-style comments is dead code.** It indents lines
  beginning with `*`, but only when *every* line does, and the first line of a
  comment always begins with the `/` of `/*`. It is computed anyway so the port
  does not silently diverge if upstream ever fixes it.

### The formatter corpus

`make update-fmt-corpus` runs **go-jsonnet's `formatter.Format`** over every
Jsonnet file in the repository and writes `testdata/corpus/` plus a manifest
recording which go-jsonnet produced it. That is the identical call
`tanka.Format` makes, so matching it is matching `tk fmt` — with no `tk` binary,
and no temporary path baked into a golden through the diagnostic filename.
Calling the library also allows varying `Options`, which `tk` does not expose
and the `UseImplicitPlus: false` cases need. What it cannot reach is fodder:
`internal/parser` is an internal package, so the oracle is text-level.

`tests/corpus.rs` requires the match count to equal
`testdata/corpus-baseline.toml` **exactly, in both directions**. Fewer is a
regression; more is progress and the number is updated in the same commit. The
small fixture families get one `quarantine.toml` entry each because each names a
behaviour worth arguing about; 857 real files would need 857 entries saying only
"the formatter is not finished", so breadth is ratcheted on the count instead
and the test prints which files differ.

**107 of 138 match with `format` still an identity stub.** That is worth knowing
before reading a match count as progress: most Jsonnet here is already exactly
`tk fmt`-clean, so only the other 31 exercise the passes at all, and a pass that
moves the number by one may still be wrong on the files that were already
passing for free. `crates/jrsonnet-formatter/src/tests/*` is over-represented
among the 31, which figures — those fixtures were written to be awkward.

#### Two sets, stored differently, and the reason is the ratchet

Phase 4 added a second set, and the two are stored differently because of where
their **inputs** live. `corpus-baseline.toml` carries the full argument.

| set | files | inputs | answers |
| --- | --- | --- | --- |
| `in_repo` | 138 | already in the repository | `testdata/corpus/`, one golden each, plus `manifest.json` |
| `go_jsonnet_testdata` | 719 | committed, in the same file | `testdata/go-jsonnet-corpus.json` |

The second set is go-jsonnet's own root `testdata/`, whose inputs are not in
this repository. Committing only its answers would make the file count depend
on whether a checkout happened to be present — and an **exact** assertion over
an environment-dependent denominator cannot hold in both environments. That is
the `GO_JSONNET_FOR_TESTS` hazard above, one level up and worse: there a missing
checkout made a family grade silently at zero, here it would make the ratchet
unenforceable. So that artifact carries both halves of every entry and the
numbers are the same on a laptop, in CI and in the devShell.

Three consequences, each a guard against a green run that measured nothing:

- **`files` is asserted as well as `matching`.** A regenerated artifact that
  lost entries — a checkout with no `testdata/`, a walk that found nothing —
  would otherwise pass a match-count check while grading almost nothing.
- **A missing artifact is a failure, not a skip.** Both are committed, so
  absence means a broken checkout. The in-repo loader used to return early.
- **The generator takes the checkout as a required argument.** There is no
  invocation that produces a corpus with one set missing, which
  `check-fmt-corpus` would then diff and call up to date.

**The in-repo set deliberately does not grow.** The node, pass and lexer
oracles are all driven by `corpus/manifest.json`, and
`tests/node_oracle.rs` asserts the node oracle covers the same list in the same
order — so adding files invalidates about 19 MB of committed oracle,
`node-oracle.json` alone being 11 MB, regenerated whole on every go-jsonnet
bump. Those oracles exist to grade node kinds, fodder slots and one pass at a
time, and `node_oracle.rs` already asserts every kind and slot is reached. 719
tiny one-liners would add megabytes for essentially no new slot, so text-level
breadth stays at the text level.

`cmds/rtk/testdata` and `crates/rtk-jsonnet/testdata` are the in-repo Jsonnet
not in either set: 173 files, 32 KB between them, almost all sub-100-byte
discovery fixtures already in exactly the shape `tk fmt` writes. They would
raise the denominator by more than they raise the grading.

#### What the second set bought, which was not a match count

Worth knowing before reading 857 as "719 more chances to be wrong". The match
count was predicted as 719 of 719 and measured as 719 of 719, so as breadth
against `tk fmt`'s output the set found **nothing**.

What it found was **three files whose formatted output does not parse** — a
class `tests/idempotence.rs` treats as unlistable over the snippets and asserts
empty, and which the in-repo 138 cannot reach. Two are `PrettyFieldNames`
promoting a computed field inside an object comprehension; one is a `|||` block
of nothing but newlines. Both mechanisms are upstream's, both are under
`jsonnetfmt` is not a fixed point above, and both were confirmed against the
real `tk` on both runs.

It also bought **fifteen graded parse errors**, which were not predicted at all.
15 of the 719 do not parse, so their committed answer is go-jsonnet's message
*and location* through the same coalesce-error path the in-repo set uses for its
three. That lands in a family recorded above as thin — before the parse-error
snippets, 26 of go-jsonnet's 29 parser-error templates had no graded location —
and it adds eleven templates end to end, three of them rendered `(1:8)-(3:4)`,
the multi-line form of `LocationRange` that one fixture had been carrying alone.

So the useful framing for any later corpus decision: a breadth corpus of real
files is a poor instrument for the match count, which is what
`corpus-baseline.toml` has said since 2b, and a good one for finding **inputs of
a kind nobody thought to write a snippet for**. 719 files bought three inputs
and four snippets, in a pass that had 27 of them and no comprehension case, plus
fifteen error answers nobody would have written by hand.

### CI, and what it did not check until Phase 4

`.github/workflows/checks.yaml` calls `test.yaml` on every pull request, and
before Phase 4 that ran exactly two things: `make check-golden-fixtures` and
`cargo test --all`.

**Correctness was covered and staleness was not.** `cargo test --all` grades the
formatter against the *committed* corpus, so a broken pass failed CI. What
nothing checked is whether the committed corpus and the two truth tables still
equal what the Go libraries produce: `check-fmt-corpus`,
`check-go-sort-truth-table` and `check-glob-truth-table` existed as Makefile
targets and were called by nobody, so a go-jsonnet bump or a hand edit to a
golden drifted silently with every fmt test green.

They run in a `check-generated` job now — a job rather than steps on `test`,
because they need Go and no Rust, they are independent so three steps give three
independent red checks, and they run in parallel so the wall time is free.
`make check-generated` is the same three locally.

**That job installs the Go the sort table names, and must.**
`testdata/go-sort-truth-table.json` records `"goVersion"` and
`check-go-sort-truth-table` is a plain `diff -u`, so any other Go fails it on
that field before a single permutation is compared. The version is read out of
the table with `jq`, which is the right coupling rather than a workaround: the
table's own caveat is that pdqsort's tie order belongs to the toolchain, so a
deliberate regeneration under a newer Go carries CI with it. A second step then
asserts the Go on `PATH` is the one installed, because a failed install would
leave the runner's own Go there and the failure would read as drift in the
permutations.

#### tk is pinned, in every job that installs it

`.github/actions/install-tk` is the one place. It reads
`TANKA_COMPATIBLE_VERSION` out of `crates/rtk-masterminds/src/lib.rs`, so the
pin lives where rtk already answers for it rather than being restated in YAML,
and **an empty read fails the job** — which is the bug it replaces. The step
before it computed a `TK_VERSION` from the releases API, downloaded
`releases/latest` anyway without using the variable, and printed `tk --version`
without asserting anything about it. It now asserts the reported version,
matching on the **number** rather than the tag; a dev build reports something
else and fails, which matters because a dev build of tk skips the
`expectVersions.tanka` check altogether.

**Matching the number is load-bearing, and that is measured.** A release tk
prints `tk version 0.38.0` — with no leading `v`, while the constant and the
download URL both carry one. An assertion on the full tag would therefore have
failed every correct install, which is the worst kind of CI change: red on the
happy path, so the next person deletes the assertion rather than the bug.

`benchmarks.yaml` uses the same action: those jobs validate that rtk's output
still matches tk's, so an unpinned tk retargets them exactly as it would the
goldens.

`RTK_REQUIRE_TK=1` is set on `cargo test --all`, so
`tk_agrees_on_the_streams_and_the_exit_codes` fails rather than printing a loud
`SKIPPED` naming zero scenarios; `tk_also_refuses_to_run_without_a_path`
honours it too, having skipped unconditionally before.

#### The scheduled job against tk latest

`.github/workflows/tk-latest.yaml` is non-blocking by construction: nothing
calls it and it has no `pull_request` trigger. Weekly, it installs tk latest,
runs the fmt CLI cross-check and the golden fixtures, files one issue that it
updates rather than one per run, and goes red as well — an issue already open
makes the next run look clean otherwise.

**What it runs is the decision.** The corpus is generated from go-jsonnet
directly with `tk` nowhere in it, so running it against tk latest compares
nothing new. The CLI cross-check and the golden fixtures are what observe tk
moving, and the cross-check reaches further than it looks: it compares both
streams, both exit codes **and the files left behind**, so a newer tk carrying a
newer go-jsonnet formatter shows up there too. It also grades its own log —
`RTK_REQUIRE_TK=1` for a missing tk, plus two `grep`s requiring the "compared N
of M scenarios" line and forbidding `SKIPPED`, because that test printed `ok`
whether it compared fifteen scenarios or skipped them all.

### Finding Jsonnet Files

`tk fmt` and `tk lint` share Tanka's `jsonnet.FindFiles`, so rtk shares
`rtk_jsonnetfmt::files`. Six of its behaviours are surprising and each is
pinned by a test in `crates/rtk-jsonnetfmt/tests/discovery.rs`; `src/files.rs`
enumerates them. The two that bite hardest:

- **A named regular file bypasses everything.** `tk fmt vendor/foo.libsonnet`
  formats it despite the default `vendor/**` exclude, and `tk fmt README.md`
  hands `README.md` to the Jsonnet formatter.
- **Walked paths are cleaned.** Child paths go through `filepath.Join`, which
  cleans, so a `./foo` argument yields `foo/bar` and not `./foo/bar`. Since `*`
  crosses `/`, an uncleaned `./` prefix would make `.*` exclude the entire tree.

`lint` used to hand-roll both the walk and the exclude matching. It pruned
excluded directories (`FindFiles` returns `nil`, not `fs.SkipDir`), followed
symlinks where `filepath.WalkDir` does not, and sniffed at the four default
patterns as strings rather than compiling them. It now goes through the port.

It no longer defaults its paths to `"."` either. `tk lint` and `tk fmt` are both
`ArgsMin(1)`, and both rtk commands now are, which the `fmt` CLI phase fixed
along with everything else the two share. `lint_requires_a_path_too` in
`cmds/rtk/tests/fmt_parity_test.rs` is what says so, and it lives there because
that is the file with a harness that can see an exit code coming out of clap.

### Globbing

`--exclude` patterns are compiled by `crates/rtk-gobwas-glob`, a port of
`github.com/gobwas/glob` v0.2.3 — a deliberate Go-library port, like
`rtk-masterminds`.

**Do not reach for `globset`.** `tk` calls `glob.Compile(e)` with no separator
arguments, and with no separators `gobwas` treats `*` and `**` alike, so **`*`
crosses `/`**. `globset` does not behave this way. That is also why tk's default
exclude list ships each pattern twice (`".*"` *and* `"**/.*"`): the un-prefixed
form catches the case where the match string has no leading directory
component, and the prefixed form catches the nested one.

It is graded against a truth table generated by the Go library itself
(`crates/rtk-gobwas-glob/testdata/`, generator beside it,
`make update-glob-truth-table` / `check-glob-truth-table`). The table is the
oracle, not gobwas' syntax documentation, which disagrees with it about
separators. The test skips when the table is absent.

That table earned its keep immediately: it caught nine divergences a careful
reading of the source had shipped, all of them about the **empty subject**,
where gobwas answers from the shape of the matcher its optimiser happened to
build rather than from what the pattern means.

- `Single`, `List` and `Range` decode a rune with `utf8.DecodeRuneInString`,
  which returns `(RuneError, 0)` for `""`. Their guard is `if len(s) > w`, and
  `0 > 0` is false, so they fall through and test **U+FFFD** for membership.
  So `?` matches the empty string, and `[!abc]` does while `[abc]` does not.
- `BTree.Match` loops `for offset < limit`, both zero on empty input, so the
  body never runs and it returns false. Anything composite cannot match `""`
  however zero-width its parts are — which is why `***` matches `""` with no
  separators (the run collapses to one `Super`) and stops matching as soon as
  `/` is declared a separator.

`matches_empty_subject` in `program.rs` reproduces this and `is_match` consults
it before running the program at all. Nothing in rtk can reach it — `FindFiles`
never matches an empty path and tk never passes separators — but the point of
the crate is to be a port rather than an approximation, and the table is what
says which it is.

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
