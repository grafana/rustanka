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
   order. A file named twice is formatted twice and counted twice in
   `Formatted N files`.

`filepath.WalkDir` does not follow symlinks. `rtk lint` currently uses
`follow_links(true)` — a divergence to fix alongside.

### CLI surface

- `ArgsMin(1)`: no argument is an error. Do **not** copy `lint.rs`'s
  default-to-`"."`.
- `-` reads stdin, formats, prints to stdout via bare `fmt.Print` (no wrapper
  text), and with `--test` exits `ExitStatusDiff` if changed.
- `--test`: writes nothing; on changes prints
  `The following files are not properly formatted:` then one path per line to
  stderr, exit **16** (reuse `commands::diff::EXIT_CODE_DIFF_FOUND`).
- `--stdout`: prints `// {name}\n{content}` to stdout, then a blank line to
  stderr per file.
- `--verbose`: prints `fmt {path}` or `ok  {path}` (two spaces after `ok`) per
  file, then one trailing blank line to stderr.
- Summary on stderr: `All discovered files are already formatted. No changes were made`
  when nothing changed, else `Formatted {n} files`.
- In-place writes are whole-file at mode `0644`.
- A parse failure propagates out of `FormatFiles` and aborts the run — **error
  text is part of the contract.**

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
| pass traversal | `src/pass.rs` | unit tests on the four slots it does *not* visit |
| `FixTrailingCommas` | `src/passes/fix_trailing_commas.rs` | the pass oracle |
| `NoRedundantSliceColon` | `src/passes/no_redundant_slice_colon.rs` | the pass oracle |
| `PrettyFieldNames` | `src/passes/pretty_field_names.rs` | the pass oracle, and 2 corpus files |
| `EnforceStringStyle` | `src/passes/enforce_string_style.rs` | the pass oracle, and 3 corpus files |
| `EnforceCommentStyle` | `src/passes/enforce_comment_style.rs` | the pass oracle, and **no** corpus file |
| `EnforceMaxBlankLines` | `src/passes/enforce_max_blank_lines.rs` | the pass oracle, and **no** corpus file |
| `FixNewlines` | `src/passes/fix_newlines.rs` | the pass oracle, and 1 corpus file |
| `FixIndentation` | `src/fix_indentation.rs` — **not** a pass | the pass oracle, and 9 corpus files |
| `removeInitialNewlines` | `ast::Node::remove_initial_newlines` | the corpus only; see 2d |
| `removeExtraTrailingNewlines` | `fodder::Fodder::remove_extra_trailing_newlines` | the corpus only; see 2d |
| pass oracle | `testdata/pass-oracle.json` | `make update-fmt-pass-oracle` |
| pass snippets | `testdata/pass-snippets.json` | the same target |
| idempotence | `tests/corpus.rs` | the goldens, formatted twice |

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

**2e — semantics-affecting.** `FixParens`, `RemovePlusObject`. Graded by the
Phase 0 unit tests. A bug here is a correctness bug, not a cosmetic one.

**2f — `SortImports`.** Runs first in the pipeline but last to implement: it is
self-contained and only touches the top-of-file group.

**Exit per step:** quarantine list shrinks, no entry added, round-trip and
idempotence properties still hold.

**Exit for the phase:** quarantine empty; `format(format(x)) == format(x)` for
every fixture. A port that needs a second pass to converge has a bug in
`FixIndentation` or `FixNewlines` — jsonnetfmt is a one-pass fixed point.

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

`FmtArgs` in `cmds/rtk/src/commands/fmt.rs` already mirrors tk's flags. What is
missing is the `-` stdin path, and `run()` is `bail!("not implemented")`.

Implement discovery + the four output modes + exit codes + exact stderr strings
per the reference section. Test as CLI parity, separate from the formatter,
following `cmds/rtk/tests/error_parity_test.rs` and
`env_list_error_parity_test.rs`.

**Exit:** parity tests pin every string and exit code listed above, including
`ArgsMin(1)`, the duplicate-argument double count, and stdin with and without
`--test`.

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

5. **Pin tk.** `.github/workflows/test.yaml` computes `TK_VERSION` and then
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
| Idempotence | one-pass fixed point | property, no fixtures | no |
| Already-formatted | zero-diff on clean repo | property | no |
| CLI parity | flags, modes, exits, stderr | tk | yes |
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
