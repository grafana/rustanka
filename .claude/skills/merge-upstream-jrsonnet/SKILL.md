---
name: merge-upstream-jrsonnet
description: Merge upstream CertainLach/jrsonnet into rustanka in validated stages, preserving rustanka's tk output parity and performance/memory behavior, ending with GPG-signed linear commits. Use when asked to sync/merge/pull in upstream jrsonnet.
---

# Merging upstream jrsonnet into rustanka

rustanka vendors a fork of jrsonnet. The org requires signed commits, so upstream's
history can never be pushed; merges are done as **true merges on a work branch**
(for correct 3-way bases and rerere) and then **linearized into signed commits**.
The 2026-07 full merge (branch `jj/merge-upstream-jrsonnet`, archive
`archive/merge-upstream-jrsonnet-true-merges`) is the reference execution.

## 1. Determine the range

- Last merged upstream commit: `cat .jrsonnet-upstream-base` (fallback: latest
  `Merge upstream jrsonnet <sha>` subject in `git log master`).
- `git fetch jrsonnet` (remote = https://github.com/CertainLach/jrsonnet).
- Range = `$(cat .jrsonnet-upstream-base)..jrsonnet/master`.

## 2. Set up the work branch with real ancestry

Linearized history has no merge-base with upstream. Reestablish it without
changing the tree:

```
git switch -c merge-work master
git merge -s ours $(cat .jrsonnet-upstream-base)   # tree unchanged, ancestry recorded
git config rerere.enabled true
```

(If `archive/merge-upstream-jrsonnet-true-merges` still exists and matches
master's tree, branching from it works too and carries the old merge topology.)

## 3. Plan stages

- Small ranges (< ~40 commits, no architectural rewrites) can be one merge.
- Otherwise pick checkpoints so each stage isolates ONE big refactor.
- **Upstream master has mid-refactor commits that do not compile.** Before
  merging a checkpoint, build it standalone:
  `git worktree add /tmp/chk <sha> && cargo build -p jrsonnet-evaluator` —
  if it fails, walk forward to the next buildable commit (history is linear;
  re-staging is free). This happened twice in 2026-07 (259b3abb, b89fdd32).
- Preview conflicts: `git merge-tree --write-tree --name-only HEAD <sha>`.

## 4. Per-stage loop

```
git merge --no-ff <sha>
# resolve (see §5/§6), then:
make fmt
cargo build --workspace          # zero errors AND zero new warnings
cargo test -p tests
make test
make check-golden-fixtures       # byte-for-byte tk parity — THE arbiter
git commit && git tag merge-work-stage-N
```

If a stage explodes: `git merge --abort` (run `git rerere` first to record
partial resolutions) and pick a nearer checkpoint.

## 5. Rustanka features that MUST survive (check every stage)

Golden tests catch most output regressions, but **auto-merge can silently drop
fork behavior in files that don't conflict** — verify these after each stage:

| Feature | Where | Guarded by |
|---|---|---|
| tk-parity YAML emitter (`ManifestYaml*Settings`, quote/float/timestamp rules) | `jrsonnet-stdlib/src/manifest/yaml.rs` (+lib.rs registration with settings) | `make check-golden-fixtures` |
| Go-style floats (`%.17g`, `set_use_go_style_floats`, `mtype` branch) | `jrsonnet-evaluator/src/manifest.rs`, `stdlib/format.rs` | golden fixtures |
| Go-style number→string in `+` concat | `evaluate/operator.rs` (`format_num_go_style`) | golden fixtures (yaml_output_env) |
| `e { body }` = `e + { body }` for non-object lhs (jsonnet spec; upstream deviates) | `evaluate/mod.rs` `LExpr::ObjExtend` | `tests/golden/string_object_extend.jsonnet` |
| LayeredCores backpointer core list (O(1) `extend_from`/`with_super`) | `obj/mod.rs`, `obj/oop.rs` | `tests/golden/deep_mixin_chain.jsonnet` + §7 mixin benchmark |
| Assertion-recursion guards (`ASSERTION_DEPTH`, `SKIP_ASSERTIONS`, `reset_obj_thread_locals`) | `obj/mod.rs` (guards in `get_idx`, `get_idx_uncached`, `run_assertions`) | `tests/golden/super_assertion_recursion.jsonnet` |
| Weak import cache + relaxed circular imports + `clear_thread_local_state` | `jrsonnet-evaluator/src/lib.rs` (`CachedEvaluation`) | rtk memoize tests; §7 memory |
| `ArrValue::extended` always-flatten | `arr/mod.rs` | `tests/golden/issue30_recursive_slice_concat.jsonnet` |
| `CachedUnbound` weak-key pruning | `val.rs` | memory checks |
| regex natives always enabled (upstream gates behind `exp-regex` — remove the gate everywhere it reappears, incl. new crates) | stdlib/cli/tests/web Cargo.tomls, `Builtins` struct | `tests/suite/std_param_names.jsonnet` |
| Tanka natives stay OUT of stdlib (live in `cmds/rtk`) | stdlib/cli | rtk tests |
| Dependency pins: `grafana/serde-saphyr` git, `grafana/jrsonnet-gcmodule` leak-fix branch | workspace `Cargo.toml` | §7 cycle stress (gcmodule) |
| Fork CI workflows (`.github/workflows/`), no upstream README.adoc | repo root | — |

## 6. Resolution policies

- `tests/cpp_test_suite_golden_override/`, `tests/go_testdata_golden_override/`:
  bulk `git checkout --theirs`.
- `tests/golden/`, `tests/tests/snapshots/`, rowan/formatter snapshots: contain
  real fork tests — resolve per-file, re-bless via insta only when upstream
  itself changed behavior/message text.
- **Never blanket keep-ours on hunks when upstream moved code** — you get
  duplicate definitions. Prefer whole-file from the correct side + surgical
  re-application of the fork delta (extract with
  `git diff <upstream-sha> <ours> -- <file>`).
- Nix/xtask/bindings infra: take theirs. Workspace `Cargo.toml`: take theirs,
  then re-add fork-only entries (`jrsonnet-lint`) and restore pins.
- rtk API seam (all rustanka↔jrsonnet coupling is in `cmds/rtk` + `tests/`):
  compiler-driven; three evaluator-setup sites must change in lockstep
  (`jsonnet/evaluator/jrsonnet/mod.rs`, `commands/validate/common.rs`,
  `environments/mod.rs`).

## 7. Final gates (beyond per-stage battery)

Build previous-release and merged `jrsonnet`/`rtk` binaries and compare:

- **Perf repros** (must stay milliseconds; quadratic regressions hang or blow up):
  `tests/golden/issue30_recursive_slice_concat.jsonnet`,
  `tests/golden/deep_mixin_chain.jsonnet` — additionally run the mixin pattern
  at n≈64000 standalone and confirm time doubles (not quadruples) per doubling.
- **Memory** (`/usr/bin/time -v`, compare Maximum resident set size):
  `test_fixtures/perf/capture-stress.jsonnet` (capture analysis),
  `test_fixtures/perf/cycle-stress.jsonnet` (gcmodule auto-collect;
  unpatched gcmodule ≈2.5x worse).
- **Real-world export**: export a large env tree (e.g. deployment_tools
  grafana-o11y, recursive) with old and new rtk; `diff -rq` must be clean.
- **Key lesson from 2026-07:** a fork feature may only be retired as
  "superseded by upstream" after benchmarking the ORIGINAL failure mode it
  fixed — adjacent stress tests are not sufficient (the LayeredCores
  regression survived every other check), and upstream sometimes reverts the
  superseding mechanism later in the same range (im-rc was dropped again).

## 8. Linearize, sign, finish

Squash each work-branch merge into a signed linear commit with an identical
tree (org signature policy forbids pushing upstream's unsigned commits):

```
prev=$(git rev-parse master)
for c in $(git rev-list --reverse --first-parent master..merge-work); do
  prev=$(GIT_AUTHOR_NAME="$(git log -1 --format=%an $c)" \
         GIT_AUTHOR_EMAIL="$(git log -1 --format=%ae $c)" \
         GIT_AUTHOR_DATE="$(git log -1 --format=%ad $c)" \
         git commit-tree "$c^{tree}" -p "$prev" -S -m "$(git log -1 --format=%B $c)")
done
git branch <final-branch> $prev
```

Then, on the final branch:
- Update `.jrsonnet-upstream-base` to the new upstream SHA (signed commit).
- Verify: `git diff merge-work <final-branch>` empty; every commit `%G?` = G;
  zero merge commits in `master..<final-branch>`.
- Keep/update the archive branch (`archive/...-true-merges`) pointing at the
  work branch for the next sync's ancestry.
- Update this skill if the feature table in §5 changed.
