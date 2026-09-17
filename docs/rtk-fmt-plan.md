# `rtk fmt`: porting jsonnetfmt for tk parity

Plan for [#11](https://github.com/grafana/rustanka/issues/11).

## Goal

`rtk fmt` produces **byte-identical output to `tk fmt`** for every input, and an
identical CLI surface (discovery, exit codes, stderr text). Formatting is
destructive and in-place, so "close" is worse than unimplemented: a near-miss
formatter rewrites files tk would have left alone, and the damage lands in a
user's working tree.

## Non-goals

- Exposing formatter options. tk exposes none; `DefaultOptions()` is the only
  configuration that has to be correct.
- Touching `crates/jrsonnet-formatter` or `cmds/jrsonnet-fmt`. They stay as they
  are (see [Decision: new crate](#decision-new-crate)).
- `rtk fmt` as a general-purpose Jsonnet formatter. It is a tk-compatibility
  surface.

## Why the existing formatter cannot be used

`crates/jrsonnet-formatter` exists and `cmds/jrsonnet-fmt` already drives it,
but it is **not** jsonnetfmt. It is a dprint-based, width-driven pretty-printer
that re-lays-out from scratch. Its own snapshot shows the divergence:

```
{ foo: 'bar', baz: 'qux', nested: { a: 1, b: 2 }, unhidden::: 1 }
```

jsonnetfmt is *fodder-preserving*: it keeps the author's line structure and only
normalises indentation, trailing commas, redundant parens, quote style, comment
style, blank-line runs, and import order. The defaults also differ (3-space
output, `max_width: 100`, tabs by default in the CLI).

These are different algorithms, not different settings. Wiring `FmtArgs` into
the existing formatter is a few lines and yields an `rtk fmt` that rewrites
every file in a Grafana repo differently from `tk fmt`. It is not a shortcut; it
is the failure mode.

Related: the dprint formatter is not a one-pass fixed point, which is why
`cmds/jrsonnet-fmt` needs `--conv-limit`. `formatter.Format` has no convergence
loop at all, which is a useful correctness signal (see
[Phase 2](#phase-2--the-formatter-port)).

> **Correction, from Phase 2c.** This paragraph used to say jsonnetfmt *is* a
> fixed point. It is not — an index whose field name is written with a `\u`
> escape formats to the unescaped spelling and then to a dotted index (the
> counterexample is spelled out under [Phase 2](#phase-2--the-formatter-port)),
> confirmed against `tk fmt`. The asymmetry is still real and still
> the reason not to add a convergence loop here, but it is about `Format`
> having no loop rather than about it not needing one. Phase 2's exit criterion
> carries the counterexample and what replaces the property.

## Reference behaviour

Everything below is verified against upstream source, not recalled. Re-verify
against the pinned tk version if it ever moves.

### What `tk fmt` is

`cmd/tk/fmt.go` → `tanka.FormatFiles` → `formatter.Format(name, content, formatter.DefaultOptions())`
from go-jsonnet. There is no Tanka-specific formatting logic whatsoever. The
entire task is reproducing go-jsonnet's formatter under one fixed option set.

### `DefaultOptions()`

From `internal/formatter/jsonnetfmt.go`:

| Field | Default |
| --- | --- |
| `Indent` | `2` |
| `MaxBlankLines` | `2` |
| `StringStyle` | `StringStyleSingle` |
| `CommentStyle` | `CommentStyleSlash` |
| `UseImplicitPlus` | `true` |
| `PrettyFieldNames` | `true` |
| `PadArrays` | `false` |
| `PadObjects` | `true` |
| `SortImports` | `true` |
| `StripEverything` | `false` (absent from `DefaultOptions`, Go zero value) |
| `StripComments` | `false` (ditto) |
| `StripAllButComments` | `false` (ditto) |

`PadArrays` and `PadObjects` are consumed by the unparser, not by any pass.

### Pass pipeline

Exact order from `FormatNode`, annotated for `DefaultOptions()`:

1. `SortImports(&node)` — free function, not a visitor. **Runs.**
2. `removeInitialNewlines(node)` — always.
3. `EnforceMaxBlankLines` — runs (`MaxBlankLines > 0`).
4. `FixNewlines` — unconditional.
5. `FixTrailingCommas` — unconditional.
6. `FixParens` — unconditional.
7. `RemovePlusObject` — runs (`UseImplicitPlus: true`). `AddPlusObject` is the
   else-branch and is **skipped**.
8. `NoRedundantSliceColon` — unconditional.
9. Strip chain — **all three skipped** under defaults.
10. `PrettyFieldNames` — runs.
11. `EnforceStringStyle` — runs (`Single != Leave`).
12. `EnforceCommentStyle` — runs (`Slash != Leave`).
13. `FixIndentation` — runs (`Indent > 0`). Invoked as
    `visitor.VisitFile(node, finalFodder)`, **not** through the `pass.ASTPass`
    machinery.
14. `removeExtraTrailingNewlines(finalFodder)` — always.
15. `unparser`: `unparse(node, false)`, then `fillFinal(finalFodder, true, false)`,
    then append `"\n"` if `finalFodder` is empty or its last entry is
    `FodderInterstitial` — final whitespace is stripped at lex time, so this is
    what guarantees a trailing newline.

There is **no** `FixRepresentation` pass. `SortImports` is first, not last.

### File discovery

`pkg/jsonnet/files.go`, `FindFiles(target, excludes)`. Six behaviours that are
each surprising and each must be reproduced:

1. **A named regular file bypasses everything.** `os.Stat` + `IsRegular()`
   returns `[]string{target}` before any glob or extension check. So
   `tk fmt vendor/foo.libsonnet` formats it despite the default `vendor/**`
   exclude, and `tk fmt README.md` hands `README.md` to the Jsonnet formatter.
2. **Directories are never pruned.** `if d.IsDir() { return nil }` returns
   *before* the exclude loop and returns `nil`, not `fs.SkipDir`. Every file is
   tested individually; a `**/vendor/**` exclude still walks the entire vendor
   tree and discards file by file.
3. **Globs match the walk path as given**, slash-normalised via
   `filepath.ToSlash` — absolute if the argument was absolute, relative if
   relative. This is why the default exclude list ships each pattern twice
   (`".*"` *and* `"**/.*"`, `"vendor/**"` *and* `"**/vendor/**"`): the
   un-prefixed form catches the case where the match string has no leading
   directory component.
4. **`*` crosses `/`.** `glob.Compile(e)` is called with no separator arguments,
   so gobwas treats `*` and `**` alike. `globset` does not behave this way.
5. **Extensions are exactly `.jsonnet` and `.libsonnet`**, case-sensitive.
6. **No sorting, no dedup across arguments.** `filepath.WalkDir` walks lexically
   within a tree; `FormatFiles` concatenates per-argument results in argument
   order. A file named twice is formatted twice.

   > **Correction, from Phase 3.** This used to end "and counted twice in
   > `Formatted N files`", which is only true of a mode that does not write.
   > The default mode writes the file on the first pass, so the second pass
   > reads it back clean and it reports `Formatted 1 files`; `--test` and
   > `--stdout` leave it alone and report 2. The two quirks compose, the
   > unconditional write perturbing the second read. Measured against tk, not
   > reasoned about — the test asserted 2, failed, and the cross-check's
   > `named-twice-writing` scenario says tk reports 1 as well.

`filepath.WalkDir` does not follow symlinks. `rtk lint` currently uses
`follow_links(true)` — a divergence to fix alongside.

### CLI surface

Read off `cmd/tk/fmt.go` and `pkg/tanka/format.go`. The order of operations is
itself part of the contract, so it is given as an order.

**1. `ArgsMin(1)`.** No argument is an error. Do **not** copy `lint.rs`'s
default-to-`"."` — and `lint.rs` should lose it at the same time, since
`tk lint` is `ArgsMin(1)` too.

**2. Stdin, before anything else.** `-` is honoured only as the **sole**
argument (`len(args) == 1 && args[0] == "-"`), and this branch returns before
the excludes are compiled — so `rtk fmt - --exclude '[a'` succeeds where
`rtk fmt . --exclude '[a'` does not. It reads all of stdin, formats under the
diagnostic filename `<stdin>`, and prints the result to **stdout** with a bare
`fmt.Print` — no `// name` header, no trailing newline of its own. With
`--test` it prints the formatted output **anyway** and only then exits 16 if it
differs.

**3. Compile the excludes.** Before any file is read, so a malformed glob
aborts the run untouched and the message is gobwas' own — which
`rtk-gobwas-glob` already reproduces.

**4. Pick one output mode.** `--test` is checked first, so **`--test` beats
`--stdout`**: nothing is printed and nothing is written. Otherwise `--stdout`
prints `// {name}\n{content}` to stdout and one blank line to **stderr** per
file. Otherwise files are written in place, whole-file, mode `0644`.

**The output mode runs for every discovered file, changed or not.** `outFn` is
called unconditionally in `FormatFiles`'s loop. So `--stdout` prints every file
and the default mode **rewrites every file**, touching the mtime of a file it
did not change. Reproduce it.

**5. `--verbose` prints to STDOUT, not stderr.** `FormatFiles`'s `printFn` is
`fmt.Println(i...)`. One line per file: `fmt {path}` for changed, `ok  {path}`
for unchanged — two spaces, because `Println` inserts one between its operands
and the literal is already `"ok "`. Only the single trailing blank line, after
the loop, goes to stderr.

**6. The summary, on stderr**, and it is a three-way switch in this order:

- `--test` **and** something changed: `The following files are not properly
  formatted:` then one path per line, then exit **16** (reuse
  `commands::diff::EXIT_CODE_DIFF_FOUND`).
- nothing changed: `All discovered files are already formatted. No changes were
  made`. Note this is also what `--test` prints when everything is clean, and
  it exits 0.
- something changed: `Formatted {n} files`.

**A parse failure aborts the whole run** and returns before the summary, so no
count is printed. The message is go-jsonnet's, which `format` already produces.
A discovery failure is wrapped `finding Jsonnet files`.

## Decision: new crate

The port lands in a new `crates/rtk-jsonnetfmt`, beside `rtk-masterminds` as a
deliberate Go-library port whose whole purpose is behavioural equivalence.

- `crates/jrsonnet-formatter` stays untouched, so upstream jrsonnet syncs against
  `.jrsonnet-upstream-base` stay clean.
- `cmds/jrsonnet-fmt` stays as-is. It is upstream's dprint CLI and editing it
  buys merge surface for no parity gain. Document that it is **not**
  tk-compatible and that `rtk fmt` is the tk-compatible entry point.

### Substrate

**Open, to be settled by prototyping both on `FixIndentation`.**

The original reading was: use `jrsonnet-rowan-parser`'s lossless CST, with
trivia as the analogue of go-jsonnet's *fodder*; `children_between` /
`trivia_before` in `crates/jrsonnet-formatter` show the access pattern. Reading
both sides properly complicates that.

**Fodder is not a trivia stream.** It is a normalised model of vertical space
with invariants enforced in `MakeFodderElement`, counts rather than characters
(`Blanks`, and `Indent` in spaces with a tab counted as 8), and merge rules in
`FodderAppend` — a `LineEnd` may not follow a `LineEnd` or a `Paragraph`, so
appending one merges it or promotes it to a `Paragraph`. The unparser then reads
*named slots per node type*, and their identity matters: `Index` reuses
`RightBracketFodder` as its id fodder, and `Slice` treats the emptiness of
`StepColonFodder` as meaningful.

So the fodder model and an AST carrying those slots have to be built either
way. rowan saves parsing the grammar — real, but the smaller half — and costs an
impedance layer plus any route to go-jsonnet's exact parse-error messages, which
the Phase 0 fixtures already pin.

| | port go-jsonnet's front end | build on `jrsonnet-rowan-parser` |
| --- | --- | --- |
| passes port | mechanically, one to one | re-derived against a different shape |
| parse errors | exact, for free | no obvious route |
| fodder model | needed | needed |
| grammar | ~1000 lines to port | already done |
| first result | slow | fast |

The tiebreaker is evidence, not preference: in `rtk-gobwas-glob`, the two files
that mirrored Go's structure had **zero** divergences against the generated
oracle, and the one file that deliberately departed from it shipped **nine**.
`FixIndentation` is the pass where that would hurt most, so it is the pass to
prototype.

`crates/rtk-jsonnetfmt/src/fodder.rs` and `src/unparse.rs` are already ported
and are substrate-independent, so neither prototype starts from nothing.

## Phases

Each phase is a landable PR or small stack. Exit criteria are testable.

### Phase 0 — red baseline

No formatter work. Stand up the harness that will grade everything after it.

1. New test reusing the existing `GO_JSONNET_FOR_TESTS` hook (see
   `tests/tests/cpp_test_suite.rs`, which already reads it and skips gracefully
   when unset): glob `formatter/testdata/*.jsonnet` from the go-jsonnet checkout
   and compare against the sibling `*.fmt.golden`.

   This corpus is **free and authoritative**: those goldens are generated by
   `Format(name, input, DefaultOptions())`, the identical call `tanka.Format`
   makes. Matching them *is* matching `tk fmt`, with no tk binary and no
   regeneration step. It is small (~3 pairs), so it is a smoke test, not
   coverage.

2. Port `TestFormatNoImplicitPlus`'s nine inline cases as unit tests. They cover
   `FixParens` / `RemovePlusObject`, the only passes whose bugs change what a
   file *evaluates to* (`{ a: 1 } { b: 2 }.a` must become
   `({ a: 1 } + { b: 2 }).a`). Note these run with `UseImplicitPlus: false`, so
   they exercise `AddPlusObject`, which `DefaultOptions()` skips — keep them as
   pass-level tests, not end-to-end ones.

3. Broken-input fixtures. go-jsonnet's own harness uses `coalesceError`, folding
   the parse error *message* into the golden. Add fixtures for unterminated
   string, unclosed brace, bad token, and pin the messages.

4. A `quarantine.toml` listing fixtures known to fail, with a test asserting the
   list only ever **shrinks**. This is how passes land incrementally without
   deleting cases, per the CLAUDE.md rule. Entries carry a reason.

**Exit:** `cargo test` red in a bounded, enumerated way with no formatter code
written. Everything downstream is measured against this.

### Phase 1 — discovery and globbing

Independent of the formatter; unblocks `lint` at the same time.

1. `gobwas`-compatible glob matching in a shared helper. Not `globset` —
   `*` must cross `/` (see reference §4).
2. Truth-table test generated by gobwas/glob in Go, committed with its generator,
   following `crates/rtk-masterminds/testdata/`. Cover: the four default
   patterns, `*` vs `**` crossing `/`, character classes, alternation `{a,b}`,
   escapes, and absolute vs relative match strings.
3. `FindFiles` equivalent reproducing all six behaviours, including the
   named-file bypass and the no-prune walk.
4. Repoint `lint.rs::is_excluded` at the helper; drop `follow_links(true)` to
   match `WalkDir`. Note the behaviour change in the PR.

**Exit:** truth table green; a discovery test asserting `rtk`'s file list equals
tk's for a fixture tree with symlinks, dotfiles, a vendor dir, a non-Jsonnet
named file, and a duplicated argument.

### Phase 2 — the formatter port

The bulk. Order is chosen so each step is gradeable.

**2a — fodder model, AST and unparser.** The keystone. This is also where
`PadArrays` / `PadObjects` land, since the unparser is the only thing that
consumes them.

> **Correction.** This step originally read: *assert `unparse(parse(x)) == x`
> byte-for-byte over the whole in-repo corpus; if the round trip is not the
> identity, no pass result can be trusted.* **That gate cannot hold**, and
> asking rtk for it would have blocked the phase on a mistake.
>
> go-jsonnet's unparser *renders* text from the fodder model rather than
> copying source, so the round trip is not the identity in go-jsonnet either,
> with every pass disabled. From `unparser.go` and `internal/parser/lexer.go`:
>
> - a line-end comment is always written as exactly two spaces then the
>   comment, so `x // c` comes back as `x  // c`
> - indentation is `fod.Indent` spaces, and the lexer counts a **tab as 8**, so
>   a tab-indented file is re-emitted with spaces
> - block strings drop `\r` outright — "Formatter always outputs in unix mode"
> - trailing horizontal whitespace is stripped at lex time and never restored
>
> The replacement gate is the generated corpus, brought forward from Phase 4:
> `crates/rtk-jsonnetfmt/testdata/generate` runs `formatter.Format` over every
> Jsonnet file in the repository and writes the answers. That is stricter than a
> round trip — it is the real target — and it is available now.

**Substrate: decided.** go-jsonnet's front end is being ported (see
[Substrate](#substrate)). The bake-off between it and the rowan route was
dropped once it became clear both candidates needed most of a front end, because
the unparser's spacing decisions live at AST sites rather than in the fodder —
so neither could be prototyped, or even tested, without the AST already in
go-jsonnet's shape.

### What has landed

| piece | file | graded by |
| --- | --- | --- |
| fodder model | `src/fodder.rs` | unit tests on the documented invariants |
| fodder renderer | `src/unparse.rs` | unit tests on Go's four flag combinations |
| locations and errors | `src/location.rs` | unit tests on the three range shapes |
| token kinds | `src/token.rs` | unit tests, plus the lexer oracle |
| lexer | `src/lexer.rs` | **138/138 against real tokens and fodder** |
| breadth corpus | `testdata/corpus/` | `make update-fmt-corpus` |
| lexer oracle | `testdata/lexer-oracle.json` | `make update-fmt-lexer-oracle` |
| snippet oracle | `testdata/lexer-snippets.json` | the same target |
| AST with fodder slots | `src/ast.rs` | the node oracle, via the parser |
| parser | `src/parser.rs` | **138/138 files and 63/63 snippets, node for node and slot for slot** |
| unparser's node walk | `src/unparse.rs` | the corpus, now that `format` runs |
| string escaping | `src/string_util.rs` | unit tests on each message, including two upstream bugs |
| node oracle | `testdata/node-oracle.json` | `make update-fmt-node-oracle` |
| node snippets | `testdata/node-snippets.json` | the same target |
| parse-error snippets | `testdata/parse-error-snippets.json` | the same target — the only grading of an error's **location** |
| pass traversal | `src/pass.rs` | unit tests on the four slots it does *not* visit |
| `FixTrailingCommas` | `src/passes/fix_trailing_commas.rs` | the pass oracle |
| `NoRedundantSliceColon` | `src/passes/no_redundant_slice_colon.rs` | the pass oracle |
| `PrettyFieldNames` | `src/passes/pretty_field_names.rs` | the pass oracle, and 2 corpus files |
| `EnforceStringStyle` | `src/passes/enforce_string_style.rs` | the pass oracle, and 3 corpus files |
| `EnforceCommentStyle` | `src/passes/enforce_comment_style.rs` | the pass oracle, and **no** corpus file |
| `EnforceMaxBlankLines` | `src/passes/enforce_max_blank_lines.rs` | the pass oracle, and **no** corpus file |
| `FixNewlines` | `src/passes/fix_newlines.rs` | the pass oracle, and 1 corpus file |
| `FixIndentation` | `src/fix_indentation.rs` — **not** a pass | the pass oracle, and 9 corpus files |
| `FixParens` | `src/passes/fix_parens.rs` | the pass oracle, and **no** corpus file |
| `RemovePlusObject` | `src/passes/remove_plus_object.rs` | the pass oracle, and 3 corpus files |
| `AddPlusObject` | `src/passes/add_plus_object.rs` | the pass oracle, and the 9 `no_implicit_plus/` fixtures — the corpus **cannot** grade it |
| `removeInitialNewlines` | `ast::Node::remove_initial_newlines` | the corpus only; see 2d |
| `removeExtraTrailingNewlines` | `fodder::Fodder::remove_extra_trailing_newlines` | the corpus only; see 2d |
| `SortImports` | `src/sort_imports.rs` — **not** a pass, and not a visitor | the pass oracle on 76 snippets, and 1 corpus file |
| `sort.Slice` | `src/go_sort.rs` — a port of Go's pdqsort, not a formatter | `testdata/go-sort-truth-table.json`, 410 cases |
| pass oracle | `testdata/pass-oracle.json` | `make update-fmt-pass-oracle` |
| pass snippets | `testdata/pass-snippets.json` | the same target |
| idempotence, corpus | `tests/corpus.rs` | the goldens, formatted twice — a weak test, since a golden is already a fixed point of `Format` |
| idempotence, snippets | `tests/idempotence.rs` | both snippet families formatted twice, with the twelve known cases listed and ratcheted both ways |
| the CLI | `cmds/rtk/src/commands/fmt.rs` | `cmds/rtk/tests/fmt_parity_test.rs`, streams captured apart, plus a tk cross-check over fourteen scenarios |

### 2a is done

All three pieces landed, and the exit criterion is met: the corpus stands at
**118 of 138** with no pass implemented, up from 107 for the identity stub.

The three items below are kept because the *reasoning* in them is still the
record of why the work was sequenced this way — in particular why the node
oracle came before the parser. What they describe as remaining is now written.

**One correction, from the corpus.** This section says the round trip holds for
already-formatted input because the passes are no-ops on it. True, but it
understates the round trip: eleven files improved, not the three the parse
errors account for. **Inter-token horizontal whitespace is not fodder at all** —
fodder records line ends, blank counts, indents and comments and nothing else,
so every space *within* a line is regenerated by the unparser from `crowded`,
`separate_token` and PadArrays/PadObjects. `{a:1,b:2}` becomes `{ a: 1, b: 2 }`
with no pass involved. Indentation is the exception: it comes from
`fodder.indent`, where the lexer counted a tab as 8, so a tab-indented file
still needs `FixIndentation`. `testdata/corpus-baseline.toml` carries the
arithmetic.

### What 2a consisted of

1. **The AST with fodder slots.** Not a generic tree: the unparser reads *named*
   slots per node type, and their identity is load-bearing — `Index` reuses
   `RightBracketFodder` as its id fodder, and `Slice` treats the emptiness of
   `StepColonFodder` as meaningful. Port the shape, not an approximation of it.
2. **The parser**, `internal/parser/parser.go`. The largest single file left.
   Its error messages are already pinned by the Phase 0 fixtures.

   **Extend the fodder dumper to node level before writing it, not after.**
   `testdata/generate/fodderdump/` already dumps fodder per *token*; the parser
   needs it per *AST node and slot* — `Fodder1`, `OpFodder`, `CommaFodder` and
   the rest. Two things make this the order to work in:

   - The lexer went 138/138 on a first compile precisely because a
     token-level dump existed before a line of it was written. The parser is
     the same activity at eight times the volume.
   - Of 16 lexer expectations derived by reading Go's source, **2 were wrong**,
     and both were about fodder the model *composes* rather than reads. A
     parser composes fodder at nearly every node, so that is its dominant
     failure mode, and the text-level corpus is far too coarse to locate one:
     a misplaced `CommaFodder` surfaces as a whitespace diff hundreds of lines
     away, if at all.

   It is a change to a Go file that already exists, staged into a checkout the
   same way, and it turns the parser from an act of faith into the graded
   exercise the lexer was.

   **It worked, and it needed no staged checkout.** Unlike `token`, every
   fodder slot is an exported field of package `ast` and
   `formatter.SnippetToRawAST` is public, so `testdata/generate/nodedump/` is
   an ordinary program in the generate module. The parser then matched
   go-jsonnet on all 138 corpus files and all 63 snippets — 32,332 entries — on
   its first compile, as the lexer had. The oracle also earned its keep before
   the parser existed: a snippet asserting that `a[::]` is what distinguishes
   an empty `StepColonFodder` from an absent one was **wrong**, and the dump
   showed `a[::]` and `a[:]` produce identical trees, because `::` lexes as one
   operator token and the parser's `::` branch never assigns that slot.
3. **The rest of the unparser**, which walks the AST. The fodder half is done.

**Exit for 2a:** the 107 corpus files that are already `tk fmt`-clean
round-trip through parse-then-unparse with **no passes at all**. Go's passes are
no-ops on already-formatted input, so for those files `unparse(parse(x))` must
equal `x`. That is the plan's original round-trip instinct, correctly scoped —
it does not hold on arbitrary input, and does hold here. **Met**, at 118 of 138;
see the correction above for where the extra eleven came from.

**2b — cheap local rewrites.** `FixTrailingCommas`, `NoRedundantSliceColon`,
`PrettyFieldNames`. Independent, small, quick quarantine wins.

### 2b is done

The corpus stands at **120 of 138**, up from 118.

Two of the three passes are not *in* that number, and finding out why is most
of what this step was about.

#### The traversal came first

All three passes are visitors over `internal/pass/pass.go`, which was not
ported, and nine of the twelve passes are an override of one or two of its
methods. So it is part of 2b rather than a detour, and `src/pass.rs` is the
largest thing 2b added.

Two things about the port are worth knowing before writing another pass:

- **Go's `p ASTPass` first parameter is gone.** It exists because `Base` has to
  call the *outer* pass, which struct embedding alone will not do; a Rust trait
  with provided methods dispatches virtually already, so `self` does that job.
  What does not come for free is Go's `c.Base.Array(p, node, ctx)`, since Rust
  has no `super` — so the base traversal is a free function per hook in
  `pass::base`, which the trait's default methods delegate to and which an
  overriding pass calls where Go writes `c.Base.…`.
- **`Context` is an associated type.** Go's is `interface{}`, and exactly one
  pass uses it: `AddPlusObject` carries the parent node to decide whether
  replacing `e {}` with `e + {}` needs parens. It compares the parent's child
  pointer against the current node, which Rust cannot do while the parent is
  mutably borrowed — so Phase 2e will carry a descriptor of the parent instead,
  refined per slot by overriding the handful of node hooks that matter. The
  framework does not constrain that choice; it was checked before the trait was
  written rather than after.

Four slots the base traversal deliberately never visits are each pinned by a
unit test, because a pass that rewrites fodder — `EnforceCommentStyle`,
`EnforceMaxBlankLines`, the strip passes — will silently not reach them:
`InSuper`'s `in_fodder` and `super_fodder`, `Index`'s `right_bracket_fodder`
when the index is an identifier, `Apply`'s `tail_strict_fodder` without
`tailstrict`, and a `Parameter`'s `eq_fodder` without a default.

#### How the passes are graded, and why it took a third oracle

**The corpus cannot grade them.** The count moved by two:
`tests/golden/issue153.jsonnet` and
`test_fixtures/golden_envs/conditional_eval_env/main.jsonnet`, both a quoted
field name losing its quotes, both `PrettyFieldNames`. Jsonnet already
formatted by `tk fmt` gives a pass nothing to do, so a breadth corpus of real
files measures the passes least where they are newest.

The oracle later put numbers on that, and they are worth having before planning
any later phase. Changed cells over all 138 files: `AddPlusObject` 18,
`FixIndentation` 9, `EnforceStringStyle` 6, `PrettyFieldNames` 3,
`RemovePlusObject` 3, `FixNewlines` 1, `SortImports` 1, `FixTrailingCommas` 1,
and **zero** for `NoRedundantSliceColon`, `EnforceCommentStyle`,
`EnforceMaxBlankLines` and `FixParens`. So the thinness is not a Phase 2b
problem: `FixIndentation`, which the risk table below calls the hardest pass
and the place near-misses will cluster, gets nine cells, and
`EnforceCommentStyle` gets none. **Write the snippets before the pass, in every
remaining phase.**

Those figures also corrected a mistake made here. `FixTrailingCommas` *does*
change a corpus file — `tests/suite/std_param_names.jsonnet` — and
`PrettyFieldNames` changes three rather than two; `builtin_strings_string` and
`std_param_names` go on failing only because they also need
`EnforceStringStyle` and `FixIndentation`, which masks the change in the byte
comparison. The claim that neither pass touched anything came from reading the
input-against-golden text diff, which answers a different question: **a pass
changing a file is not the same as that file's output changing.** Exactly the
kind of error the oracle exists to catch, caught on its first run.

**`Options` cannot isolate them either.** `PrettyFieldNames` has a flag and
`Indent` gates `FixIndentation`, but `FixTrailingCommas`, `FixNewlines`,
`FixParens` and `NoRedundantSliceColon` are unconditional in `FormatNode`.
There is no option setting under which their effect is observable alone.

So `make update-fmt-pass-oracle` records **the AST each pass leaves behind**,
in the notation the node oracle already uses. Three decisions in it:

1. **Staged, like the lexer oracle.** `internal/formatter` may only be
   imported from inside go-jsonnet's own module, so no program in the generate
   module can construct a `FixTrailingCommas`. The target copies
   `testdata/generate/_staged/passdump.go` and `nodedump/dump.go` into
   `<checkout>/rtkpassdump/` and runs it there. A `package main` inside the
   module is enough — this one needs no `_test.go` trick, because everything it
   touches is an exported identifier of an internal package: `SortImports` is a
   function, the visitors expose `File` through the embedded `pass.Base`, and
   `FixIndentation` exposes `VisitFile`. `_staged/` is named for the go tool's
   rule that a directory beginning with `_` is ignored, so the file is never
   offered to the generate module, where its import would be refused.
2. **One pass at a time, on a fresh parse — not the pipeline's accumulated
   state.** Cumulative dumps would be the more faithful thing to record and
   would grade nothing: a dump taken after step 5 is only correct once steps
   1–4 are also written, so it could not have graded `FixTrailingCommas` in
   this step at all. Isolation is what makes the quarantine ratchet's
   one-pass-at-a-time landing possible. `FormatNode`'s order is a fourteen-line
   list read straight off upstream, and the corpus grades it end to end.
3. **A no-op is recorded as `unchanged`, and asserted.** Most cells are one:
   the oracle stays small, and rtk then has to prove its own pass changes
   nothing there. That is the failure mode that matters most for a formatter
   that writes in place.

The Rust side is `tests/pass_parity.rs`, at full parity with no ratchet, with
the dumper moved to `tests/astdump/` and shared with `node_parity`. It skips
passes rtk has not written and **prints which**, so the coverage it provides is
stated rather than assumed; and
`the_snippets_reach_every_pass_that_is_written` fails if a written pass changes
no snippet, which is exactly the hole the corpus has.

`testdata/pass-snippets.json` holds the inputs — 56 of them, three groups.
Inputs are safe to write by hand; **answers are not**, which is the whole
point. Of 16 lexer expectations derived by reading Go's source, 2 were wrong,
and both were about fodder the model *composes* rather than reads. These three
passes are almost nothing but fodder composition through `FodderMoveFront`.

#### It worked, and the numbers say why it was needed

The staged program compiled and ran first try, as the lexer and node dumpers
had. Then all three passes matched go-jsonnet node for node and slot for slot,
on all 56 snippets and all 138 corpus files.

The changed-cell counts are the argument for the whole exercise. Over the
snippets: `FixTrailingCommas` 15, `PrettyFieldNames` 15,
`NoRedundantSliceColon` 2. Over the 138 real files: **1, 3 and 0.** The
snippets also confirmed a reading rather than only checking one — six of the
eight slice snippets cannot put anything in `step_colon_fodder` at all, since
`::` lexes as a single token, so `NoRedundantSliceColon` is reachable only by a
comment or a newline written between the two colons.

**The three strip passes are left out of the dump**, which is what keeps it
reviewable. Each rewrites every tree it touches, so `StripAllButComments` and
`StripEverything` alone were about 270 of 356 changed cells and most of a 23 MB
file — regenerated in full on every go-jsonnet bump, and unreadable in a diff.
All three are skipped under `DefaultOptions`, which is the only configuration
`tk fmt` uses, and no phase here schedules them. `passNames` in
`_staged/passdump.go` is a one-line change if that ever stops being true.

**One gap, recorded honestly.** `removeInitialNewlines` and
`removeExtraTrailingNewlines` are unexported *functions*, not passes, so even
the staged program cannot reach them; closing that means making it a `_test.go`
inside `internal/formatter`, which Phase 2d can decide. Both are four lines and
both are covered end to end by the corpus.

#### A fourth upstream oddity

`PrettyFieldNames.Index` does `index.RightBracketFodder = lit.Fodder` — an
assignment, not a `FodderMoveFront` — and that one slot doubles as the fodder
before a `]` and the fodder before an identifier. So `a['foo' /* c */]`
formats to `a.foo` and **the comment is dropped**. It joins the three in
`src/lexer.rs` and the `NamedArgument.EqFodder` one: reproduced, not fixed. The
object-field path in the same pass uses `FodderMoveFront` and keeps everything.

**2c — representation.** `EnforceStringStyle`, `EnforceCommentStyle`. Watch the
documented carve-outs: strings containing `'` or `"` use whichever syntax avoids
escaping, and `#!` hashbang comments are always left alone.

### 2c is done

The corpus stands at **124 of 138**, up from 120, and all four are
`EnforceStringStyle`: `tests/suite/rounding.jsonnet`,
`tests/suite/sjsonnet_issue_127.jsonnet`, `tests/golden/issue195.jsonnet` — a
`"false"` field name that has to *stay* quoted, so it is also what says the
keyword rule survived — and `tests/suite/sjsonnet_issue_1029.jsonnet`. Two more
files the pass genuinely changes stay red because they are tab-indented and so
also need `FixIndentation`: `tests/golden/builtin_strings_string.jsonnet` and
`tests/suite/std_param_names.jsonnet`. Six changed cells, four files flipped,
two masked.

`EnforceCommentStyle` contributes **nothing** to that number, and cannot. Which
is the whole story of this step.

#### The count was predicted as 123 and measured as 124

Worth recording, because it is the same mistake 2b made and it survived being
warned about. `sjsonnet_issue_1029.jsonnet` is one line, and its diff against
its golden is dominated by `x*x` → `x * x` and `[1,2,` → `[1, 2,` —
horizontal whitespace, which the round trip fixes for free and which no pass
touches. The `error "3"` → `error '3'` at the end of that same line was read as
part of the same story, so the file was filed under "already passing".

The general form: **an input-against-golden diff answers "what is different",
never "which pass does it".** 2b learned that at the level of whole files —
`FixTrailingCommas` changing a file whose output still differs — and this is
the same error one level down, inside a single line. `pass-oracle.json` answers
the question exactly, per file and per pass, and takes one query:

```
EnforceStringStyle: changes 6 of 138 corpus files
EnforceCommentStyle: changes 0 of 138 corpus files
```

Read the oracle. Do not read the diff.

#### The corpus grades one of these passes and not the other

`EnforceStringStyle` gets 6 corpus cells. `EnforceCommentStyle` gets **zero**,
for a reason that generalises: a `#` comment in a file that is already
`tk fmt`-clean has by definition already been rewritten to `//`, so a breadth
corpus of real files cannot contain the input this pass exists for. Neither can
`Options` isolate it — and neither can the four remaining zero-cell passes
(`NoRedundantSliceColon`, `EnforceMaxBlankLines`, `FixParens`), which is worth
carrying into 2d and 2e.

So 57 snippets were added before either pass was written, taking
`testdata/pass-snippets.json` from 56 to 113. The standing rule from 2b held:
**write the snippets first, every time.**

There is one authoritative `tk fmt` answer for `EnforceCommentStyle` outside
the snippets, and it is worth knowing about because it is free:
go-jsonnet's own `formatter/testdata/empty_comment.fmt.golden` is `#` above an
empty object formatting to `//`. It is graded as the `go_jsonnet/empty_comment`
fixture whenever `GO_JSONNET_FOR_TESTS` is set, and it confirms the bare-hash
case — where `len(*comment) > 1` fails and the hashbang guard is never
consulted at all.

#### Four things about `EnforceStringStyle` that the option does not decide

1. **The quotes in the text win.** A string containing a `'` takes `"` and one
   containing a `"` takes `'`, whatever `StringStyle` says — the option is
   consulted once and then overridden by either count. A string containing
   **both** is returned on untouched, so it keeps the kind it was written with
   even where the option asked for the other one.
2. **The counting is on the unescaped text**, so `"a\'b"` counts one single
   quote although the source had no bare one, and comes back as `"a'b"` — the
   kind unchanged and the value rewritten. That is the one shape where this
   pass changes a literal without changing its kind.
3. **It is a round trip, so it normalises more than quotes.** `StringUnescape`
   then `StringEscape` drops an escape that was never needed (`"a\/b"` →
   `'a/b'`), collapses an escape whose character needs none (`"\u0041"` → `'A'`,
   `"\u00E9"` → `'é'`), and re-spells one that does in upstream's own
   casing (`"\u009F"` → `'\u009f'`, because `StringEscape` formats with
   `%04x`).
4. **Three kinds are returned on unexamined**: `|||` blocks and both verbatim
   kinds. For the verbatim ones that is not cosmetic — the parser has already
   collapsed their doubled quotes, so restyling one would mean re-doubling
   them, and upstream does not try.

It overrides `LiteralString` and **not** `Visit`, which is not a stylistic
choice: `pass.Base.Import` reaches an import's filename through that leaf hook
only, so a pass overriding `Visit` would restyle every string in a file except
the one in `import "foo.libsonnet"`.

#### A fifth upstream oddity, and it is in the carve-out

`EnforceCommentStyle`'s hashbang guard `return`s **before** setting
`seenFirstFodder`. So a spared `#!` never marks fodder as seen, and a *second*
`#!` is still "first" and is also spared. `comment_style/hashbang_twice` pins
it.

The flag is otherwise set by **any** non-interstitial element, whether or not
anything about it was rewritten, because the assignment sits outside the
`len(Comment) == 1` test. Three consequences, each with a snippet, and each
surprising on its own:

- One **blank line** at the top of a file is a commentless `LineEnd`, and it
  disables the carve-out for the `#!` beneath it.
- An **interstitial** does not set it, so `/* c */ #!b` on one line still
  spares the hashbang. The `#!` has to be on the *same* line: a newline between
  them makes the lexer emit its own `LineEnd`, and that does set it.
- A comment **already in the target style** sets it without being rewritten, so
  `// a` above a `#!b` costs the hashbang its exemption.

A related trap the snippets were written around: a fresh-line `#` comment is a
`FodderParagraph` appended with `addFodder` (a plain append), while a
*multi-line* C comment goes through `addFodderSafe`, and `FodderAppend` puts a
synthetic commentless `LineEnd` in front of a paragraph appended to empty
fodder. So `/* a\n b */` above a `#!` converts the hashbang — not because the
paragraph set the flag, but because that synthetic element did. Reading
`addFodder` against `addFodderSafe` in the lexer is what makes this legible;
guessing from the fodder model alone gets it backwards.

#### Where this pass cannot reach

`EnforceCommentStyle` only sees the fodder `pass::base` walks, and 2b pinned
four slots it never walks. Two of them can hold a `#` comment, so `tk fmt`
leaves it exactly as written:

- `{ a: 'b' # c` … `in super }` — `InSuper`'s `in_fodder`.
- `a. # c` … `b` — `Index`'s `right_bracket_fodder`, doubling as the fodder
  before an identifier.

The same comment after `super.` **is** rewritten, because
`base::super_index` visits `id_fodder` unconditionally. That contrast is what
makes the `Index` case a hole rather than a rule, and it is the payoff for
having written those four unit tests in 2b before any pass needed them.

**2d — layout.** `EnforceMaxBlankLines`, `FixNewlines`, `removeInitialNewlines`,
`removeExtraTrailingNewlines`, then `FixIndentation`. `FixIndentation` is the
single largest and hardest pass — it reasons about continuation lines and is
where near-misses will cluster. Budget accordingly.

### 2d is done

The corpus stands at **134 of 138**, up from 124, and the four that remain are
`RemovePlusObject` (2e) on three files and `SortImports` (2f) on one. Nothing
2d-shaped is left in it.

**The prediction was 134 and the measurement was 134** — the first exact one in
this sequence, after 2b under-counted and 2c predicted 123 for a measured 124.
It was exact because it was derived by querying `pass-oracle.json` per file and
per pass, which is what both earlier phases were told to do and did not.

#### The split between 2d's two halves is the whole case for the snippets

The three small steps moved the corpus by **zero**. `FixIndentation` moved it
by ten. So four of the five things this phase landed are invisible to the
breadth corpus, and one of them — `EnforceMaxBlankLines` — was graded by
*nothing at all* beforehand: 0 of 138 corpus files and 0 of the 113 snippets
then in the file, for the structural reason 2c generalised. A `tk fmt`-clean
file has no run of three blank lines in it by definition.

141 snippets were written before a line of pass code, taking
`testdata/pass-snippets.json` from 113 to 254. All three passes then matched
go-jsonnet node for node and slot for slot on every snippet and all 138 corpus
files, on the first compile — as the lexer, the node dumper and the 2b passes
each had. `FixIndentation` is 812 lines of Go and went green first time; that
is what an oracle buys.

#### The oracle is a lower bound on the work, not a sufficient set

The new lesson, and a refinement of 2b's and 2c's rather than a repeat.
`pass-oracle.json` says exactly which passes change a file's AST. It does not
say which passes a file *needs*.

`test_fixtures/golden_envs/yaml_line_wrapping_env/main.jsonnet` is the only
corpus file the oracle attributes to `FixNewlines`, and landing `FixNewlines`
did not flip it — it took `FixIndentation` too, because `FixNewlines` only
inserts bare `LineEnd(0, 0)`s and something then has to indent them. The count
holding at 124 through the first half of the phase was therefore correct.
Predict with the oracle, but read the union of passes a file might need, not
the one the oracle names.

#### Decision: the two unexported functions are not dumped

The plan left it to 2d whether to reach `removeInitialNewlines` and
`removeExtraTrailingNewlines` by making `_staged/passdump.go` a `_test.go`
inside `internal/formatter`. **It was decided against**, and the reasoning is
recorded because a later phase may want to revisit it.

Both are four lines, and both are a slice truncation and a field assignment —
neither *composes* fodder, which is the one category this project has measured
itself unreliable at deriving by hand (14 of 16, and both misses composed). The
conversion means renaming packages and dropping the dumper's CLI, churning a
working oracle to grade eight lines the corpus already covers end to end. They
live as inherent methods on the types they mutate, with unit tests, and their
*position* in the pipeline is documented as the load-bearing part.

There is a better route than the `_test.go`, and it is worth knowing about:
`testdata/generate/main.go` already calls the **public** `formatter.Format`, so
a snippets mode there would give authoritative whole-pipeline answers for
arbitrary hand-written input with no staged checkout at all. What it would need
is a ratchet, since a whole-pipeline answer is only reproducible once the
pipeline is complete.

#### One finding that was a wrong test before it was a note

`removeExtraTrailingNewlines` can only ever fire on a file that **ends in a
comment**. The lexer's main loop measures a whitespace run and then tests for
end of input, breaking before it adds the line end, so trailing newlines never
become fodder: a file of `1` and four newlines has empty final fodder. The only
route to blanks at end of file is a comment's own element, whose blanks
`lex_until_newline` measures while a following token is still in prospect.

A unit test asserting the opposite was the only failure in the phase, and it
failed for the right reason — it was a hand-derived claim about *composed*
fodder, which is exactly the category the standing rule says not to write by
hand.

#### Idempotence is now tested

`tests/corpus.rs` formats each of the 138 goldens a second time and requires no
movement, with an **empty** allow-list beside it. The plan asks for this per
step and 2d is the step to ask it of, since `FixIndentation` and `FixNewlines`
are the two passes that could plausibly fail to settle. A failure is a bug in
one of them until it is traced to a specific cross-pass interaction in
`FormatNode`'s order — see the Phase 2 correction below for the one that is
already known, and for why a convergence loop is not the answer.

#### Five upstream shapes and one upstream bug

`FixIndentation`'s module documentation carries the full account. The bug is in
`specs`: the conditions loop computes its indent from `openFodder(spec.Expr)`
and then calls `Visit(spec.Expr)` — the `for` expression, not `cond.Expr`. So a
comprehension's `for` expression is indented twice at two different columns and
the second answer wins, while the `if` condition is never indented at all. Two
snippets pin it from either side.

**2e — semantics-affecting.** `FixParens`, `RemovePlusObject`, `AddPlusObject`.
Graded by the Phase 0 unit tests. A bug here is a correctness bug, not a
cosmetic one.

### 2e is done

The corpus stands at **137 of 138**, up from 134, and the one that remains is
`SortImports` (2f) on `tests/realworld/entry-graalvm.jsonnet`.
`quarantine.toml` is **empty**: all nine of its entries were
`no_implicit_plus/` cases naming `FixParens` or `AddPlusObject`, and landing
those two removed them all. Nothing in it needed to become a documented
divergence.

**The prediction was 137 and the measurement was 137**, the second exact one in
a row, and derived the same way — by querying `pass-oracle.json` per file and
per pass and then reading the *union* of passes each of those files needs.
`RemovePlusObject`'s three files are the three, and one of them, the docsonnet
`render.libsonnet`, has a `+` at the start of its own line, so the `LineEnd`
that preceded it moves on to the object and `FixIndentation` then has to put it
back at the right column. That interaction was the one thing the oracle could
not answer, because it runs each pass on a fresh parse and so grades no *pair*
of them. The corpus is what said it was right.

#### Snippets before passes, at its most extreme

The standing rule from 2b, and 2e was the phase with the least to lose by
ignoring it and the most to lose by getting it wrong. Before the phase, over
138 corpus files and 254 snippets:

```
FixParens          corpus 0/138   snippets 0/254
RemovePlusObject   corpus 3/138   snippets 0/254
AddPlusObject      corpus 18/138  snippets 1/254
```

`FixParens` was graded by **nothing in either direction**, while being one of
the two passes whose bugs change what a file *evaluates to*. That is strictly
worse than `EnforceMaxBlankLines` in 2d, which was equally ungraded but only
cosmetic. `AddPlusObject`'s one snippet was an accident of 2d's
`indentation/apply_brace`.

Worse still, and new: **`AddPlusObject`'s 18 corpus cells can never become
corpus flips.** The corpus is generated with `DefaultOptions`, which takes
`RemovePlusObject` and skips `AddPlusObject` entirely, so those cells exist
only because the oracle runs each pass in isolation. Its end-to-end grading is
the nine `no_implicit_plus/` fixtures and nothing else — the corpus count
standing still when it landed is the *safety check* that step 7 is one `if`
rather than two steps.

107 snippets went in before a line of pass code, taking
`testdata/pass-snippets.json` from 254 to 361: `fix_parens` 25,
`remove_plus_object` 32, `add_plus_object` 50. Over them the oracle records
`FixParens` changing 23, `RemovePlusObject` 19 and `AddPlusObject` 50. All
three then matched go-jsonnet node for node and slot for slot on every snippet
and all 138 corpus files, on the first compile — as the lexer, the node dumper
and every pass phase before it had.

The 13 `RemovePlusObject` no-ops are the most valuable cells in the group.
Every one is a deliberate negative, and together they are what says the
`Var`-or-`Index` restriction on the left, the plain-`Object` requirement on the
right and the `+`-only operator test are each real. A pass that
over-generalised any of the three would be green on every positive case.

#### The Context design, which was the phase's real work

`AddPlusObject` is the one pass that uses `pass.Context`, and Go's context is
the parent node. It tells the parent's slots apart with
`parent.Target == *node` — a **pointer** comparison. Since `node` is
`&parent.Target` whenever the walk arrived through that slot, the comparison is
an identity check of a place against itself: trivially true there, false
everywhere else. What it is really asking is *which slot did the walk come
through*.

Rust has no answer to that while the parent is mutably borrowed, and
`src/pass.rs` said so when the trait was written rather than discovering it
afterwards. The replacement it scoped is what landed:
`passes::add_plus_object::Parent`, a descriptor of the parent refined per slot,
filled in by overriding the five node hooks whose slots upstream's switch
distinguishes — `Apply`, `Index`, `Binary`, `Unary`, `InSuper`. The trait did
not change.

The cost lands entirely in the pass: those five overrides **restate the base
traversal**, because a `pass::base` function takes one `ctx` and hands it to
every slot. That is a hazard no fodder pass had — an override that drops a slot
silently stops converting the `ApplyBrace`s in it, and nothing about the pass
itself would look wrong. Two things guard it: a snippet putting an `e { }` in
every slot of all five, and a separate counting walk over `pass::base` that
shares no override, so it can still see an `ApplyBrace` in a slot the pass
never visited.

#### Four readings checked against the oracle rather than asserted

Each of these was derived by reading Go and then confirmed from the regenerated
oracle before being written down, which is the habit 2c and 2d were told to
form:

1. **`FixParens` is an `if`, not a loop.** `(((1)))` comes out with two levels
   of parens and `((((1))))` also with two. So four halve to two and three
   become two, and **`jsonnetfmt` is not a fixed point** here either — the
   second counterexample after 2c's escaped field name, and a much simpler one.
   It must not be answered with a convergence loop, for the reason 2c already
   gives.
2. **`AddPlusObject` has no `ast.Slice` case.** `{a:1} {b:2}[1:2]` keeps its
   slice target as a bare `Binary`, so the output reparses as
   `{a:1} + ({b:2}[1:2])` — a different tree. A slice binds exactly as tightly
   as an index, so this is the same bug the `ast.Index` case exists to prevent
   in the one node kind the case does not name. It is the only upstream oddity
   this port carries that changes what a file evaluates to. Reproduced, because
   matching `tk fmt` is the contract, and reached by nothing in practice since
   `tk fmt` never runs the pass.
3. **The `InSuper` branch is a constant false**, and correctly so: `+` binds
   tighter than `in`, so no parens are needed. The oracle shows the InSuper's
   index as a `Binary`, not a `Parens`.
4. **The replacement node is what goes down as the parent.**
   `{a:1} {b:2} {c:3}.x` gives `Index.Target = Parens`,
   `Parens.Inner = Binary`, `Binary.Left = Binary` — one pair of parens and not
   two, because the inner `ApplyBrace` saw the new `Binary` as its parent
   rather than the original `ApplyBrace`.

#### Two things in `AddPlusObject` that are inert, and stay

Both were traced rather than assumed, and both are ported anyway so that an
upstream change cannot silently diverge — the same treatment the lexer's
`allStar` hack gets.

- **The fodder move never moves anything.** Upstream builds the `Parens` with
  `Fodder: binary.NodeBase.Fodder` and then nils the `Binary`'s, which looks
  like the difference between a comment landing inside or outside the parens.
  It is not: the parser constructs every `ApplyBrace` with `ast.Fodder{}`,
  because an `ApplyBrace` is left-recursive and its opening fodder is stored on
  the leftmost leaf — and no earlier pass writes a node's *own* fodder, since
  they all go through `openFodder`, which walks that same spine. Both slots are
  always empty.
- **The `ast.ApplyBrace` parent panic is unreachable.** Every node arrives
  through `Visit`, and `Visit` replaces an `ApplyBrace` before descending, so
  no child can see one as its parent. The panic is reproduced on the
  `apply_brace` hook — one step earlier in the walk than Go's, on the same
  impossible condition — so it still fires if the invariant breaks.

#### `FixParens` overrides `visit`, where upstream overrides `Parens`

The one place the port's shape had to differ. Upstream's `*ast.Parens` embeds
`NodeBase`, so its `Parens` hook can reach the node's own fodder for
`FodderMoveFront(openFodder(node), …)`. `src/ast.rs` keeps that fodder on
`Node`, so the `parens` hook is handed a payload that does not carry it.

The move is unobservable. `Base.Visit` visits the open fodder and *then*
dispatches to `p.Parens`, so upstream's collapse happens after that visit and
this one happens before it — but `FixParens` overrides no fodder hook, so the
base traversal over fodder is a no-op either way and the resulting tree, which
is what the oracle grades, is the same. `openFodder` on a `Parens` is likewise
provably its own fodder: `leftRecursive` has no `*ast.Parens` case.

#### Idempotence survived, and 2e was the phase most likely to break it

`tests/corpus.rs`'s allow-list is still **empty**. This was the phase with node
structure being rewritten rather than fodder, and with a pass that is itself
non-convergent — but no corpus golden holds a doubly-parenthesised expression,
and `RemovePlusObject`'s output reparses directly as the `ApplyBrace` it
produced, so it is a fixed point of itself.

**2f — `SortImports`.** Runs first in the pipeline but last to implement: it is
self-contained and only touches the top-of-file group.

### 2f is done, and so is Phase 2

The corpus stands at **138 of 138**, `quarantine.toml` is **empty**, and
`tests/corpus.rs`'s idempotence allow-list is **empty**. The prediction was 138
and the measurement was 138 — the third exact one in a row, and derived the
same way, by querying `pass-oracle.json` per file and per pass.

The one file is `tests/realworld/entry-graalvm.jsonnet`, exactly as the oracle
named it in Phase 2d, and it flipped with nothing else needed: the rest of the
file is already clean, so unlike 2d's `FixNewlines` case and 2e's docsonnet
case this is the rare one where the oracle's single cell and the file's single
flip are the same thing.

#### It is unlike every pass before it, in three ways

All three were known going in and all three cost something.

1. **It is not a visitor.** `SortImports(file *ast.Node)` is a free function
   over 229 lines and ten helpers, none of which touches `pass.ASTPass`. So
   `src/pass.rs` bought nothing here and it does not live in `src/passes/`.
   `FixIndentation` is the precedent for that, for a different reason.
2. **It rebuilds the tree rather than rewriting nodes.** `buildGroupAST`
   constructs a fresh chain of `ast.Local` nodes from the end backwards, one
   bind each — so a multi-bind local is always split, whether or not anything
   was reordered, and every rebuilt local loses its location. This is §18 of
   `docs/learning-rust.md` at much larger scale, and it is where recursion and
   ownership actually bit: the shape that works is popping elements off the
   *back*, because the fodder each new node needs belongs to the element that
   is then `last_mut` and will be popped next. Reaching for indices means
   borrowing the vector while also moving out of it.
3. **It runs first**, at step 1, before `removeInitialNewlines` — so the fodder
   it divides has not been truncated and a file's leading blank run is still
   there to be carried into the first group's own fodder.

#### Snippets before the pass, and this time they changed the answer

Going in, over 138 corpus files and 361 snippets:

```
SortImports    corpus 1/138   snippets 0/361
```

Worse than `FixParens` before 2e in one respect: only seven snippets contained
the word `import` at all and all seven were written for `EnforceStringStyle` or
`FixIndentation`. 76 went in before a line of the pass, taking
`testdata/pass-snippets.json` from 361 to 437, and the oracle records
`SortImports` changing 52 of them.

The new lesson is not about coverage. **The snippets are also how you find out
what you did not know.** Fifteen of the 76 exist only to ask what `sort.Slice`
does with tied keys, and the answer changed the design — see below. The one
corpus file is a reversal of fourteen distinct paths and could not have raised
the question, never mind settled it.

#### The sort had to be ported, which was not in the plan

`sortGroup` calls `sort.Slice`, which is **not stable**, and two imports can
share a path: `local k = import 'k.libsonnet', kausal = import 'k.libsonnet';`
is an ordinary Jsonnet habit. So which of them comes first is decided by
pdqsort's pivot choices, and the oracle measured it: **ties invert from n = 13
upwards.** A group of thirteen whose first and last element share a key comes
back with the last one first; a run of three sharing a key comes back
`i09 i01 i04`. `Vec::sort_by` is therefore measurably wrong, and
`sort_unstable_by` is a different pdqsort and wrong differently.

Two readings that would have shipped a bug, both corrected by measurement:

- **Twelve is not the boundary.** `maxInsertion` is 12 tested with `<=`, so
  twelve is the *last* insertion-sorted size and thirteen the first that can
  reorder ties. A battery that stopped at twelve would have concluded the sort
  was stable.
- **An all-equal group is not evidence either.** `partitionEqual` preserves
  it at every length, so all six `ties_*_all_equal` snippets are no-ops. Only
  *mixed* arrangements invert.

So `crates/rtk-jsonnetfmt/src/go_sort.rs` ports `sort.Slice` — the third
deliberate Go-library port here, after `rtk-gobwas-glob` and
`rtk-masterminds`, and for the same kind of reason each time. It is graded by
`make update-go-sort-truth-table`: ten input shapes at forty lengths, 410 cases
of which 235 have tied keys, generated by the standard library itself and
recording the Go version that produced it. That table is also the only thing
that reaches the heapsort fallback and `breakPatterns`, which need
`bits.Len(n)` consecutive bad partitions and one respectively — unreachable
from any real import group.

The alternative was a stable sort plus a documented divergence, and it was
rejected rather than skipped: a group of thirteen imports is ordinary, aliasing
one path twice is ordinary, and the two together are a file `rtk fmt` would
rewrite and `tk fmt` would not — which is the exact failure mode this plan
exists to prevent. The honest caveat is recorded on the module: the tie order
is an implementation detail of the Go that built `tk`, not a promise, and if a
future Go changes it then `tk fmt` moves and rtk does not.

#### The three panics are all unreachable, and all reproduced

Traced before being relied on, as 2d traced two dead branches and 2e traced
`AddPlusObject`'s dead `ApplyBrace` panic.

- `"beforeNext should still be empty."` — the second half is only appended to
  once the flag is set, and the flip happens after that append.
- `"Expected beforeNextFodder to be empty"` — everything that makes the second
  half non-empty also makes `groupEndsAfter` true, over *the same fodder*.
- `"topLevelImport called with bad local."` — not expressible in the port;
  `Group::absorb` takes an owned `Local` the caller could only have got by
  testing first.

No cell of either oracle records a panic, over 138 files and 437 snippets.

#### Idempotence survived the one pass most likely to break it

`SortImports` is the only step that moves fodder **between different nodes**, so
a second run sees comments and blank lines attached to different imports than
the first run did. The allow-list is still empty, and the reason is
structural: every seam its output contains has already been through
`FodderEnsureCleanNewline`, so `splitFodder` divides it identically the second
time, and a sorted group re-sorts to itself.

**Exit per step:** quarantine list shrinks, no entry added, round-trip and
idempotence properties still hold.

**Exit for the phase:** quarantine empty; `format(format(x)) == format(x)` for
every fixture. A port that needs a second pass to converge has a bug in
`FixIndentation` or `FixNewlines` — jsonnetfmt is a one-pass fixed point.

> **Met**, with the 2c correction below attached: 138 of 138 on the breadth
> corpus, an empty `quarantine.toml`, and an empty idempotence allow-list.
> The known non-convergences are upstream's and none is reached by any fixture.
> There are **three mechanisms and twelve inputs**, all measured by
> `tests/idempotence.rs` rather than reasoned about; `CLAUDE.md` carries the
> table and the account. The prediction beforehand was five, and the six it
> missed were all `FixParens`, invisible to a grep for `((` because the shape
> that matters is `(\n  (1)`.

> **Correction, from Phase 2c.** **`jsonnetfmt` is not a fixed point**, and the
> counterexample is two lines of Jsonnet:
>
> ```
> a['fo\u006f']   ->   a['foo']   ->   a.foo
> ```
>
> `FormatNode` runs `PrettyFieldNames` at step 10 and `EnforceStringStyle` at
> step 11. Step 10 is asked whether `fo\u006f` is an identifier — the *stored*
> value of a fully escaped string keeps its escapes — finds a backslash, and
> keeps the brackets. Step 11 then unescapes it to `foo`. So the output carries
> brackets justified by an escape that is no longer in it, and formatting that
> output promotes the index, because by then the value really is an identifier.
>
> Nothing here is a port bug. Each pass matches go-jsonnet in isolation on all
> 113 snippets and all 138 corpus files, and the order is read off `FormatNode`;
> the non-convergence is upstream's, and it is what `tk fmt` prints. Confirm
> with `tk fmt -` twice over that input.
>
> So the exit criterion above is wrong as an unconditional property. What
> replaces it: idempotence is required for every corpus file and fixture, and
> **a new failure is a bug until it is traced to a specific cross-pass
> interaction in upstream's order**, at which point it is recorded here and
> excluded. The original reasoning — that a second convergence pass would
> paper over a `FixIndentation` or `FixNewlines` bug — still holds, and is why
> the default stays "this is a bug".
>
> It is also why `rtk fmt` must not gain a `--conv-limit` (see
> [Why the existing formatter cannot be used](#why-the-existing-formatter-cannot-be-used)).
> Iterating to convergence would turn `a['fo\u006f']` into `a.foo` in one run
> and diverge from `tk fmt` on the first file in a Grafana repo that has an
> escaped field lookup.
>
> How it surfaced is worth keeping too. A **Phase 2b unit test asserted the
> wrong answer** — `pretty("a['fo\u006f']")` was expected to come back
> unchanged — and it passed for a whole phase because the pass that proves it
> wrong did not exist yet. A whole-pipeline assertion written while the
> pipeline is half-built records the half-built answer. The test is now split:
> the claim about *this pass* runs with `string_style: Leave`, and the pipeline
> answer is its own test.

### Phase 3 — CLI

The library is finished. `FmtArgs` in `cmds/rtk/src/commands/fmt.rs` already
mirrors tk's flags and its `--exclude` default already comes from
`rtk_gobwas_glob::TANKA_DEFAULT_EXCLUDES`; what is missing is the `-` stdin
path, and `run()` is `bail!("not implemented")`.

Implement the [CLI surface](#cli-surface) exactly, in the order given there.
Discovery is `rtk_jsonnetfmt::find_files_all` and formatting is
`rtk_jsonnetfmt::format_default`; neither needs anything added.

**The thing to be careful about is the streams**, because that is where this
phase can be wrong while looking right. `--verbose` goes to **stdout**, its
trailing blank line to stderr, `--stdout` content to stdout and its separating
blank line to stderr, and every summary line to stderr. A parity test that
merges the two streams cannot see any of that, so **capture them separately**.

Two behaviours worth a test of their own because they read like bugs:

- `--test` with `--stdout` prints nothing at all.
- The default mode **rewrites unchanged files**, so an unchanged file's mtime
  moves. `--stdout` likewise prints files it did not change.

**Also fix `lint.rs`'s default-to-`"."`** in this phase. `tk lint` is
`ArgsMin(1)` as well, and `CLAUDE.md` has been carrying that divergence with a
note pointing here.

#### Formatting must run on a large stack, and the reason is parity

The port recurses to the nesting depth of the input in several independent
places — `Node::opening_fodder_mut`, the parser's `parse`/`parse_terminal`,
every `AstPass`, `FixIndentation::visit` and `Group::absorb` — and there is no
depth limit anywhere in `crates/rtk-jsonnetfmt`. `cmds/rtk/src/main.rs` sets no
stack size for any command, so today `fmt` would run on whatever the main
thread was given; only `rtk-environment`'s discover and export spawn 8 MiB
workers.

Go grows a goroutine stack to 1 GiB, so `tk fmt` formats a deeply nested file
that rtk would abort on with a stack overflow — SIGSEGV, no message, no exit
code. That is a worse outcome than a wrong byte, because the default output
mode rewrites files as it goes: the run dies part way with some files already
written and nothing said about why.

**So the answer is a bigger stack, not a depth limit.** A limit would refuse a
file `tk fmt` formats, which is a divergence in the one direction this project
never accepts. Spawn the formatting work on a `thread::Builder` with a large
`stack_size`, the way `crates/rtk-environment/src/export/mod.rs` already does,
and join it.

It belongs in this phase rather than earlier because the CLI is what owns
process setup, and until `run()` does something there is nothing to wrap.
Worth a test with a generated input — a few tens of thousands of nested
brackets — asserting rtk formats it rather than dying; the depth that actually
overflows depends on the platform's default, so the test should generate
something comfortably past it rather than probe for the edge.

Test as CLI parity, separate from the formatter, following
`cmds/rtk/tests/error_parity_test.rs` and `env_list_error_parity_test.rs`.

**Exit:** parity tests pin every string, every stream and every exit code in
the CLI surface section — including `ArgsMin(1)`, the duplicate-argument double
count in `Formatted {n} files`, `--test` beating `--stdout`, a malformed
`--exclude` aborting before any file is read, stdin with and without `--test`,
and stdin ignoring a malformed `--exclude`. Plus one test that a deeply nested
file formats rather than overflowing the stack, per the section above.

### Phase 3 is done

`cmds/rtk/src/commands/fmt.rs` implements the CLI surface in the order given
above, `cmds/rtk/tests/fmt_parity_test.rs` grades it, and `CLAUDE.md` carries
the account under **The `fmt` CLI**. Nothing was added to
`crates/rtk-jsonnetfmt` for it: `format_default` and `find_files_all` were
enough, as this section predicted.

Four things are worth recording beyond "it works".

#### The tests run the binary, because that is the only way to see the streams

`run` returns a `bool` and `main` turns it into
`commands::diff::EXIT_CODE_DIFF_FOUND`, so an in-process test can see neither
the exit code nor which stream a line landed on. Both matter here more than
usual: six of the CLI's strings are on stderr and two on stdout, and an
implementation that put them all on one stream would pass any test that merged
them. So `fmt_parity_test.rs` spawns `CARGO_BIN_EXE_rtk`, captures the two
streams separately, and writes each expectation out in full rather than deriving
it from the code under test. It also scrubs `RUST_LOG` and the two OpenTelemetry
endpoint variables, so a developer's shell cannot write to the stderr being
asserted.

The tk cross-check is one test over fifteen scenarios, comparing both streams,
both exit codes and the files left behind. It **prints how many scenarios it
compared**, and it prints a loud `SKIPPED` naming the count as zero when `tk` is
not on `PATH`; `RTK_REQUIRE_TK=1` makes that skip a failure, which is what CI
should set. That shape is deliberate: this project has twice shipped a test that
was green while measuring nothing, and both times what would have caught it was
reading what the test printed.

It also did the one thing the standalone tests could not: it is what said the
plan's own double-count claim was wrong, by agreeing with rtk where the
hand-written expectation disagreed with both.

The no-argument case is the one scenario compared weakly, and deliberately:
rtk's message and exit code there are clap's, tk's are go-clix's, and neither is
the other's. What is compared is that both refuse and that neither formats
anything.

#### One ordering was not in the source as read, and the cross-check settled it

The CLI surface section does not say whether `printFn` or `outFn` runs first
within `FormatFiles`'s loop, and with `--verbose --stdout` both write to stdout,
so the interleaving is observable. It is implemented as `printFn` first — a
file's `fmt` line before that file's contents — pinned by
`verbose_and_stdout_share_stdout_in_loop_order`, and the `verbose-and-stdout`
scenario of the cross-check **confirmed it against tk**. The same applies to
`--verbose`'s trailing blank line being conditional on `--verbose` rather than
unconditional, which `without_verbose_there_is_no_blank_line_on_stderr` pins
because an unconditional one would pass every other test in the file.

That is what the cross-check was for, and it paid for itself on the first run:
all fifteen scenarios agreed, which is a stronger statement about the two
guesses above than any amount of re-reading the plan would have been.

#### The stack is 1 GiB and wraps the whole command

Including the stdin branch: a deeply nested file is as likely to arrive down a
pipe as to be named. A scoped thread, because the work borrows the caller's
writer; the tree is therefore also *dropped* on that stack, which matters as
much as building it.

**And 1 GiB is not parity at every depth — which the test measured rather than
assumed.** It first generated 50,000 nested brackets and overflowed the 1 GiB,
putting this port at roughly **20 KiB of stack per level**: several parser
frames per level, plus one each for twelve passes, `FixIndentation` and the
drop. go-jsonnet's frames are far smaller, so tk reaches deeper in the same
gigabyte and always will. Real Jsonnet nests to tens of levels, so the gap is
theoretical, and raising the number is an arms race with no stopping point.
The test settled at **10,000** — an order of magnitude inside 1 GiB, and one to
two orders past the ~400 levels an 8 MiB default survives unoptimized. Both
margins are load-bearing in opposite directions, and the build profile decides
the arithmetic.

#### The three `strip_*` options were removed rather than implemented

This phase was the first to hand `Options` to a caller, and the question was
left open here. They are gone: nothing implemented them, `tk` exposes no way to
ask for them, the pass oracle deliberately does not dump them, and a `pub` flag
that promises to strip comments while silently keeping them is worse than an
absent one. `Options`' own documentation carries the route back — one
`if / else if / else if` chain at step 9, first-wins, plus a line in
`passNames`.

`rtk lint` lost its default of `"."` in the same change.

### Phase 4 — corpus and CI

1. ~~`tk-compare fmt-fixtures`~~ **Done early, and differently.** The corpus
   landed in Phase 2a as `make update-fmt-corpus` /
   `check-fmt-corpus`, because the substrate prototype needed an oracle to be
   judged against.

   It calls **`formatter.Format` from a small Go program**, not `tk` at all.
   The original note here said to generate via `tk fmt -` (stdin) rather than
   `--stdout`, since `--stdout` prefixes `// {name}\n` and writes spacing to
   stderr. Calling the library directly is better again: no wrapper text to
   strip, no temporary path leaking into a golden through the diagnostic
   filename, and `Options` can be varied — which `tk` exposes no way to do, and
   which the `UseImplicitPlus: false` cases need.

   The one thing this cannot reach is fodder itself: `internal/parser` is an
   internal package, so no external program can call `SnippetToRawAST`. The
   oracle is text-level, which is the level that has to match anyway.

2. Corpus, cheapest first:
   - in-repo: ~120 files across `tests/suite` (30), `tests/golden` (25),
     `tests/realworld` (8), `test_fixtures/golden_envs` (58)
   - go-jsonnet's **root** `testdata/` when `GO_JSONNET_FOR_TESTS` is set —
     hundreds more, and already the corpus rtk trusts for eval
   - **include `vendor/`** for testing even though fmt excludes it by default.
     Vendored libsonnet is the widest variety of third-party style available,
     which is exactly what stresses a fodder-preserving formatter.

3. Consume with `insta::glob!` + `assert_snapshot!`, as `tests/tests/golden.rs`
   and the existing formatter snapshots do.

4. **Compare bytes, not lines.** Trailing newline, tabs vs spaces, CRLF and BOM
   are precisely where a formatter port drifts, and a line-based differ hides all
   four. `rtk_diff::directory::compare_directories_detailed` already does byte
   comparison for the export goldens.

5. **Set `RTK_REQUIRE_TK=1`** in the job that has tk, so
   `tk_agrees_on_the_streams_and_the_exit_codes` fails rather than skipping when
   tk is missing. It compares fifteen scenarios and prints how many it compared;
   without the variable a tk that failed to install turns the whole CLI
   cross-check into a green no-op, which is the one hazard that shape was built
   against.

6. **Pin tk.** `.github/workflows/test.yaml` computes `TK_VERSION` and then
   ignores it, curling `releases/latest`. Pin to the version
   `rtk_masterminds::TANKA_COMPATIBLE_VERSION` names and assert
   `tk --version` matches at job start, so drift fails loudly instead of
   silently retargeting the goldens. Record the go-jsonnet version the goldens
   came from beside the corpus, the way `.jrsonnet-upstream-base` records
   jrsonnet's. Add a separate **non-blocking** scheduled job against tk latest
   that opens an issue on divergence, so upstream formatter changes are still
   noticed.

**Exit:** `make check-fmt-fixtures` green in CI against pinned tk; corpus
regenerable in one command.

### Phase 4 is done

Three of the six items above were already done or superseded before the phase
started — item 1 in Phase 2a and differently (the corpus calls
`formatter.Format` from a Go program rather than shelling out to `tk`), item 3
superseded by `tests/corpus.rs`'s byte comparison and exact count ratchet
rather than `insta`, item 4 by that same comparison. What this phase landed is
the other three, plus one hole the plan does not mention at all and two
pre-existing findings that fell out of looking.

#### The hole: nothing in CI ran the staleness checks

Before this phase CI ran exactly two things — `make check-golden-fixtures` and
`cargo test --all` — while `check-fmt-corpus`, `check-go-sort-truth-table` and
`check-glob-truth-table` existed as Makefile targets called by nobody.

Worth stating precisely, because it is easy to overstate: **correctness was
covered.** `cargo test --all` grades the formatter against the committed
corpus, so a broken pass failed CI. What was covered by nothing is whether the
committed corpus and the two truth tables still equal what the Go libraries
produce. A go-jsonnet bump, or a hand edit to a golden, drifted silently with
every fmt test green.

They now run in a `check-generated` job of test.yaml. A job rather than steps
on `test`: they need Go and no Rust, they are independent of one another so
three steps give three independent red checks, and they run in parallel with
the suite so the wall time is free. `make check-generated` is the same three for
local use.

**One of the three could not have passed as it was, and that is the finding.**
`testdata/go-sort-truth-table.json` records `"goVersion": "go1.27.1"` and
`check-go-sort-truth-table` is a plain `diff -u`, so **any other Go fails it on
that one field**, before a single permutation is compared. So the job installs
the Go the table names, read out of the table with `jq`. That is the right
coupling rather than a workaround: the table's own caveat is that pdqsort's tie
order belongs to the toolchain, so a regeneration under a newer Go should carry
CI with it, and a Go that reorders ties is exactly what would move `tk fmt`'s
output and not rtk's. A second step then asserts the Go on `PATH` is the one
installed, because a failed install would otherwise leave the runner's own Go
there and the failure would read as drift in the permutations.

#### tk is pinned, and the pin has one home

Both jobs had an identical step that computed `TK_VERSION` from the releases
API, then downloaded `releases/latest` anyway without using the variable, and
printed `tk --version` without asserting anything about it.

`.github/actions/install-tk` replaces it. It reads
`TANKA_COMPATIBLE_VERSION` out of `crates/rtk-masterminds/src/lib.rs` with
`sed`, so the pin lives in the one place rtk already answers for rather than
being restated in YAML, and **an empty read fails the job** — falling back to
latest is the bug being fixed. It then asserts `tk --version` reports that
version, matching on the number so a leading `v` on one side cannot fail a
correct install.

`benchmarks.yaml` was curling latest too, and it is **in scope**: those jobs
validate that rtk's output still matches tk's, so an unpinned tk retargets them
exactly as it would the goldens, and a timing ratio against a moving tk is not
a comparison. It uses the same action.

`RTK_REQUIRE_TK=1` is set on `cargo test --all`, so
`tk_agrees_on_the_streams_and_the_exit_codes` fails rather than printing a loud
`SKIPPED` naming zero scenarios. `tk_also_refuses_to_run_without_a_path` in the
same file skipped unconditionally and now honours the variable too — with tk
installed it was the one remaining way that file could quietly compare nothing.

#### The scheduled job, and what it is worth running

`.github/workflows/tk-latest.yaml` is non-blocking by construction: nothing
calls it and it has no `pull_request` trigger, so it cannot gate a merge. It
runs weekly against tk latest, files one issue that it updates rather than one
per Monday, and goes red as well — an issue already open makes the next run
look clean otherwise.

**What it runs is the interesting decision.** The fmt corpus is generated by
calling go-jsonnet's `formatter.Format` directly, pinned, with `tk` nowhere in
it, so running it against tk latest would compare nothing new. What observes tk
moving is the CLI cross-check and the golden fixtures. The cross-check is the
better of the two for this purpose than it looks: it compares both streams,
both exit codes **and the files left behind**, so a newer tk carrying a newer
go-jsonnet formatter shows up there too, not only a change to the CLI surface.

It also grades its own log. `RTK_REQUIRE_TK=1` covers a missing tk, and two
`grep`s then require the "compared N of M scenarios" line and forbid `SKIPPED`
— because that test printed `ok` whether it compared fifteen scenarios or
skipped them all, and nobody reads a passing job's log.

#### The corpus is 857 files in two sets, and 138 is no longer the number

`testdata/corpus-baseline.toml` carries the full argument; the short version is
that the design problem the plan's item 2 walks into is real. `tests/corpus.rs`
asserts its count exactly, in both directions, and go-jsonnet's root
`testdata/` is 719 files whose **inputs are not in this repository** — so
answers alone would make the denominator depend on whether a checkout happened
to be present, which is an exact assertion that cannot hold in both
environments. That is the `GO_JSONNET_FOR_TESTS` hazard `CLAUDE.md` records
about the `go_jsonnet/` family, one level up and worse: there a missing checkout
made a family grade silently at zero, here it would make the ratchet itself
unenforceable.

So the set is committed as one self-contained JSON carrying **both halves** of
every entry, `files` is asserted as well as `matching` — a set that lost entries
would otherwise satisfy a match-count check while grading almost nothing — and a
declared set whose artifact is missing is a **failure, not a skip**, both being
committed. The same applied to the in-repo set, whose loader used to return
early on a missing manifest.

**The in-repo set deliberately did not grow.** The node, pass and lexer oracles
are all driven by `corpus/manifest.json`, and `tests/node_oracle.rs` asserts the
node oracle covers the same list in the same order; adding files invalidates
about 19 MB of committed oracle, `node-oracle.json` alone being 11 MB. Those
oracles grade node kinds, fodder slots and one pass at a time, and
`node_oracle.rs` already asserts every kind and slot is reached. Text-level
breadth belongs at the text level.

**Not added, and each for a reason.** `cmds/rtk/testdata` and
`crates/rtk-jsonnet/testdata` are 173 in-repo files, 32 KB between them, almost
all sub-100-byte discovery fixtures already in exactly the shape `tk fmt`
writes; they would raise the denominator by more than they raise the grading,
which is what this file and `corpus-baseline.toml` have been warning about since
2b.

#### The prediction was 719 and the measurement was 719

The fourth exact one in a row, and the first made **without an oracle to
query**: `pass-oracle.json` holds no cell for a file outside the manifest, so
unlike 2d, 2e and 2f this could not be derived per file and per pass. What it
rested on instead was that the formatter is at full parity everywhere it is
already graded, and that the shapes the set adds which the in-repo corpus lacks
are each covered by the snippet oracles —
measured over the 719: 3 files with verbatim strings, 12 with `|||` blocks, 8
tab-indented, 18 non-ASCII, 162 with no trailing newline, and zero with `#`
comments, `\u` escapes, CRLF or a BOM. The named risk was the 18 non-ASCII
files through `EnforceStringStyle`'s escape round trip. Nothing missed, and the
named risk was not where anything interesting turned out to be.

**One premise in that list was stated as fact and was wrong.** It also said
every file in the set parses, derived by grepping the set's `.golden` files for
"STATIC ERROR" and finding none — the wrong instrument, since those goldens
record *evaluation* output and a file the parser refuses has no evaluation
golden to say so. Fifteen of the 719 do not parse. The conclusion survived, the
reason did not, and a right answer resting on a wrong reason is worth flagging:
it is the same shape as deriving coverage by grepping inputs, which this plan
already records for the idempotence count.

The fifteen are a gain rather than a caveat. Their committed answer is
go-jsonnet's error **message and location**, and they add eleven distinct
parser-error templates end to end — including three rendered `(1:8)-(3:4)`, the
multi-line form of `LocationRange`, which one fixture had been carrying alone.
Two of them are `object_comp_bad_field.jsonnet` and `..._bad_field2.jsonnet`,
whose answer is the very message `PrettyFieldNames` manufactures below: the set
holds that error as an input and as an output, from opposite directions.

#### What the set actually bought: three files whose output does not parse

The match count was exact. The **idempotence** count was not, and this is the
result worth carrying out of the phase.

Three of the 719 format to something go-jsonnet's own parser then refuses, and
**no file among the in-repo 138 reaches that class at all**:

- `object_comp_err_elem.jsonnet` and `object_literal_in_object_comp.jsonnet` —
  `PrettyFieldNames` promotes `['x']:` to `x:` with no `ObjectComp` guard, and a
  comprehension may only have `[e]` fields, so the second run fails with
  `Object comprehensions can only have [e] fields`.
- `escaped_fields.jsonnet` — a `|||` block whose value is nothing but newlines.
  The unparser writes a blank value-line with no indent, so no content line
  carries the block indent; the lexer's leading-blank-line loop then runs before
  the indent is calculated, takes it from the terminator line and swallows the
  `|||` as content: `Text block not terminated with |||`.

All three are upstream's, and **that was settled by measurement rather than by
reading Go**. rtk and tk agree byte for byte on the first run — which the corpus
already proves, all three being inside the 719 — and on the second they agree on
the refusal, its message *and* its location. The first mechanism is the only
oddity this port carries that turns a working file into one that will not parse
while being reachable from `DefaultOptions`; `AddPlusObject`'s missing
`ast.Slice` case changes what a file evaluates to but `tk fmt` never runs that
pass.

`tests/corpus.rs` keeps two lists rather than one. `KNOWN_NON_CONVERGENT` is
still empty; `OUTPUT_DOES_NOT_REPARSE` holds those three. The split is the
finding, not bookkeeping: a file that formats to a different file is unsettled,
while a file that formats to one the parser refuses is destroyed, and `tk fmt`
writes in place. `tests/idempotence.rs` already treated this class as
unlistable over the snippets and asserted it empty; the corpus is where it
turned out to be reachable. The mechanism table in `CLAUDE.md` goes from three
mechanisms and twelve inputs to **five and fifteen**, with a class column.

#### And the attribution was measured too, which changed what got written down

The two comprehension files were predicted here as `FixIndentation`'s `specs`
bug — the documented one that indents a comprehension's `for` expression twice
and never indents the `if`. **Wrong**: it is `PrettyFieldNames`, a pass whose
27 snippets did not include a single computed field inside a comprehension. The
text-block one was predicted to produce different-but-valid output; it produces
a file that does not lex.

So two `pretty_names` snippets and two lexer snippets went in, and the pass
oracle then attributed both comprehension cases to `PrettyFieldNames` and to no
other pass. Attributing a change to a pass by reading an input-against-output
diff is the mistake this plan records for 2b and again for 2c, and it would have
been made a third time here if the guess had not been checked. The snippets are
also the only per-pass grading these will ever have: the pass oracle cannot see
a file outside `corpus/manifest.json`, and by design that is where the in-repo
manifest stays.

**And the second snippet was written as a negative and turned out to widen the
bug.** `pretty_names/field_computed_in_object_comp_not_an_identifier` was meant
to confine the damage to identifier-shaped names —
`{ ['a b']: 1 for x in [1] }` should have kept its brackets. It keeps its
*quotes* and loses its brackets, coming out as `{ 'a b': 1 for x in [1] }`,
which the parser refuses exactly as the identifier case does. So the rule is
**any string-literal field name in an object comprehension**, and only a
genuinely computed name survives — which five existing comprehension snippets
already demonstrate by passing the sweep. Two wrong predictions in one finding,
both caught by running it. A negative that fails is worth more than a positive
that passes.

That also gave `tests/idempotence.rs` its second list. It had no allow-list for
the refused class and asserted it empty, which was the right default while no
snippet reached the class; the two that now do are named there, ratcheted both
ways, alongside the three in `tests/corpus.rs`.

#### `main.go`'s `roots` comment claimed coverage that did not exist

It said "`vendor` is deliberately included even though `tk fmt` excludes it by
default". There is no top-level `vendor/` in the repository and none of the
roots is one — the same class of error as the parser asserting locations
"deferred to the oracle" while the oracle held zero error cells.

Half of it was true, and the comment now says which half: one root *contains* a
real vendored tree, `test_fixtures/golden_envs/kustomize_job_hash_env/vendor`,
eight files of jsonnet-libs/docsonnet, and the walk does not exclude it — its
`doc-util/render.libsonnet` is one of the three files Phase 2e's
`RemovePlusObject` flipped, so third-party style has earned its keep. The
repository's other four vendor directories are ten files averaging 30 bytes and
are named as deliberately left out. Real vendor breadth is Phase 5's
`tk-compare-grafana.toml`, and the comment says so.

#### A second pre-existing finding: `target/go-jsonnet` was nobody's job

`tests/fixtures.rs` falls back to `target/go-jsonnet` when
`GO_JSONNET_FOR_TESTS` is unset, and its comment — and the Makefile's, and
`CLAUDE.md`'s — all said the oracle targets leave a checkout there. **None
did.** `update-fmt-node-oracle` is an ordinary program in the generate module
and takes go-jsonnet from the module cache; the lexer and pass oracles clone
into `mktemp -d` and delete it.

So outside the nix devShell — CI included — the `go_jsonnet/` family graded **12
of 15**, saying so only in an eprintln. That cost more than three fixtures:
`empty_comment` is the only end-to-end `EnforceCommentStyle` case in the suite,
a pass that changes 0 of the corpus files, and `regular_expression` is the only
authoritative answer for a parse error's doubled location and its range shape.
`make go-jsonnet-checkout` is what makes the sentence true, the corpus's second
set needs the same checkout, and CI gets it as a side effect of
`check-fmt-corpus`.

It is the third instance of one shape in this port, and the shape is worth
naming: **a fallback, a deferral or a skip is only as good as the thing it
points at, and this project has now been wrong about that thing three times** —
the parser deferring locations to an oracle with no error cells, the Makefile
fallback that graded the family only under `make test`, and this.

### Phase 5 — acceptance and ship

1. Fmt parity over `tk-compare-grafana.toml` — Grafana's real Jsonnet, vendor
   included. Thousands of files by people who never thought about this
   formatter. This is the gate that says shippable.
2. **The already-formatted case**, which is what actually bites users: over a
   tk-formatted corpus, require **zero** changes and `--test` exit 0. A formatter
   that is 99% right still rewrites half a repo on first run.
3. README feature table: `fmt` ❌ → ✅. Close #11.
4. Note in `CLAUDE.md`: where the port lives, that `cmds/jrsonnet-fmt` is *not*
   tk-compatible, and that tk is pinned for fmt goldens.

## Test strategy summary

| Layer | What it grades | Oracle | tk needed? |
| --- | --- | --- | --- |
| `formatter/testdata` goldens | pass pipeline, smoke | go-jsonnet's own `*.fmt.golden` | no |
| `NoImplicitPlus` unit tests | `FixParens`, `*PlusObject` | upstream inline cases | no |
| Parse-error goldens | error text | upstream `coalesceError` goldens | no |
| Glob truth table | exclude semantics | generated by gobwas/glob in Go | no |
| Discovery test | `FindFiles` six behaviours | tk on a fixture tree | regen only |
| Corpus goldens | breadth | `tk fmt -` | regen only |
| Generated corpus | breadth, and the Phase 2a gate | `formatter.Format` over every in-repo Jsonnet file | no |
| Pass oracle | one pass at a time, node for node | each pass run in isolation in a staged checkout | no |
| Go sort table | the permutation `sort.Slice` gives, ties included | Go's own `sort` package over ten shapes at forty lengths | no |
| Idempotence | one-pass fixed point | property, no fixtures | no |
| Already-formatted | zero-diff on clean repo | property | no |
| CLI parity | flags, modes, exits, and which stream each line is on | the CLI surface section above, written out in full | no |
| CLI cross-check | the same, against the real thing | tk, over fourteen scenarios | yes, and it says when it has no tk |
| `tk-compare-grafana` | acceptance | tk on real repos | yes |

## Rules

**No `fmt_golden_override/` directory.** `tests/go_testdata_golden_override/`
(231 files) is legitimate because rtk's *evaluation* genuinely differs from Go's
in documented places. For fmt, every override is a divergence from `tk fmt` —
i.e. a bug. Per CLAUDE.md, a case is not dropped because it is hard to fix. If an
override is ever genuinely warranted it earns a documented entry in CLAUDE.md,
like the `env set` and conflict-message divergences.

**The quarantine list only shrinks.** It is the mechanism for landing passes
incrementally; CI asserts monotonicity so it cannot become a dumping ground.

**Byte comparison everywhere.** See Phase 4.4.

## Risks

| Risk | Mitigation |
| --- | --- |
| `FixIndentation` is most of the work and most of the near-misses | Land it last in Phase 2 with the corpus already green on everything else, so failures isolate to it |
| Fodder/trivia mismatch on the chosen substrate | Prototype both on `FixIndentation` and judge against the generated corpus before committing to either |
| Unpinned tk changes goldens with no rtk change | Phase 4.5 |
| `globset` reached for by reflex, `*` stops at `/` | Phase 1 truth table generated from gobwas, not from documentation |
| In-place destructive writes on a near-miss formatter | `--test` and already-formatted gates in Phase 5 before the README flips to ✅ |
| Go formatter changes upstream | Non-blocking scheduled job against tk latest |
