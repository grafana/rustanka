# Lexer compatibility and downstream contract

This crate ports the lexical model in go-jsonnet v0.22.0:
[lexer](https://github.com/google/go-jsonnet/blob/v0.22.0/internal/parser/lexer.go),
[fodder](https://github.com/google/go-jsonnet/blob/v0.22.0/ast/fodder.go),
[locations](https://github.com/google/go-jsonnet/blob/v0.22.0/ast/location.go), and
[string utilities](https://github.com/google/go-jsonnet/blob/v0.22.0/internal/parser/string_util.go).
The formatter compatibility target is Tanka v0.38.0. This PR contains no full
formatter or `rtk fmt` command, so passing lexer tests is not full-format parity.

## FMT-14: why the dedicated lexer remains

`jrsonnet-lexer::Lexeme` already exposes kind, original text, and byte spans,
including whitespace and comments. Missing source attribution is **not** a
reason for a separate lexer. The distinction is Go compatibility:

| Input | jrsonnet lexer | Go-compatible lexer |
| --- | --- | --- |
| `a ==! b` | `==`, `!` | `=`, `=`, `!` (Go's operator wind-back) |
| `a ?? b` | null-coalescing extension | error at column 3 |
| `1e` | number `1`, identifier `e` | numeric lexer error at column 3 |
| `1_x` | number `1`, identifier `_x` | numeric lexer error at column 3 |
| `@x` | generic lexing error | Go's specific verbatim-string error |
| `/*/` | short-comment error kind | Go's unterminated-comment diagnostic |

These were compared against the current jrsonnet lexer in an isolated executable;
the Go behavior is also pinned by the saved snippets (including numeric EOF).
Both lexers accept a normal block string, but jrsonnet returns its original
lexeme while this model needs decoded content, body/terminator indents and Go
fodder. An adapter can reconstruct these from source, but must still implement
Go's fodder construction, block processing, error messages/positions, split and
merge operator tokens, and re-lex malformed numeric input. Both original APIs
also accepted only UTF-8; using jrsonnet does not solve FMT-03.

The agreed choice is to retain the dedicated lexer on this evidence. This is
not a measured proof that every possible adapter is larger. Any replacement
must pass token, fodder, location, diagnostic and downstream full-format parity
before removing the dedicated implementation.

## FMT-03 / FMT-07 / FMT-10: byte interfaces

- `lexer::lex_bytes(name, &[u8])` consumes the original bytes. `lex(name, &str)`
  delegates to it for UTF-8 callers.
- Token `data` and each fodder comment are `Text`, an owned byte string.
  `as_bytes()` is exact; `as_str()` validates UTF-8 and can fail. There is no
  implicit lossy conversion. Block indents remain ASCII `String` values.
- Go's UTF-8 decoder consumes **one byte** for each invalid rune. For example,
  `e2 82` produces two replacement characters, not one. It is applied only where
  Go decodes runes: verbatim/block strings, line comments, multiline C comments,
  and string escaping/unescaping. Quoted token data and one-line C comments
  retain the original bytes. A blanket lossy conversion changes behavior.
- `string_unescape_bytes` validates/decodes raw quoted token content;
  `string_escape_bytes` follows Go's rune-based escaping. The `&str` wrappers
  remain available for callers that already have valid UTF-8.
- Locations have one-based lines and **original-byte columns**, with an
  exclusive end. A tab advances the column by one byte, although it contributes
  eight spaces to fodder indentation. EOF has an empty range. The oracle checks
  every token's begin/end, including EOF, against Go.
- `Error::message_bytes()` and `Error::write_to()` preserve exact diagnostics,
  without a trailing newline. `Token::display_bytes()` preserves token bytes
  when constructing parser diagnostics. The string/Display projections are
  for human-readable contexts only; they cannot represent invalid UTF-8.

PR #60 and its descendants must adapt parser/AST/pass/unparser consumers to
`Text`, use byte constructors for diagnostics, and keep bytes until the same
normalization point as Go. PR #66 must read both files and stdin as bytes, call
the byte formatter entrypoint, write output and diagnostics as bytes, and
compare captured stdout/stderr as `Vec<u8>`. Its file/stdin integration tests
remain owned by that PR. Native filenames are a separate FMT-02 concern.

## FMT-08 / FMT-09 / FMT-11: required saved evidence

Run `cargo test -p rtk-jsonnetfmt` without Go. Checked-in oracles are mandatory:
missing, unreadable, malformed, empty, or structurally incomplete data fails.
The source manifest must match the corpus oracle; snippet names and string
inputs are checked against their corresponding saved results. Failure-mode
tests use disposable files/directories rather than renaming repository data.
The original decoded-character tests remain alongside actual Unicode escape,
surrogate boundary, and malformed-sequence tests.

Run `make update-fmt-lexer-oracle` to regenerate all three artifacts:

- `testdata/lexer-oracle.json` from `lexer-sources.json` (138 existing sources);
- `testdata/lexer-snippet-oracle.json` from `lexer-snippets.json`;
- `testdata/string-oracle.json` from `string-snippets.json` (raw unescape bytes).

The target requires Python 3 and Go. It copies pinned go-jsonnet v0.22.0 into a
fresh temporary directory, stages the internal parser dumper, and only writes
saved files after all generation succeeds. It does not need the later PR's
formatter corpus manifest and does not invoke Git. Module checksums are checked
by Go. Invalid token/comment/diagnostic bytes use hex in the JSON so JSON's UTF-8
replacement cannot hide a difference.

`make check-fmt-lexer-oracle` regenerates into a temporary directory and compares
all artifacts byte for byte without rewriting them. The CI job runs this with
Go 1.27.1. Regular Rust tests keep using required saved fixtures; live Go is a
regeneration check, not a prerequisite for every Rust test run.

## FMT-02: inherited dependency

PR #58 is still based on discovery commit `868be41fa0a54dbf7820301cfdc4251756270b3c`.
Corrected PR #57 head `0cee1d86ba06732e93365f09f6a38c440ed2e46f` owns native
`PathBuf` discovery, parent cleaning and Unicode glob fixes. Those changes were
verified separately and were not duplicated here. Stack maintenance must carry
that head forward before downstream CLI/acceptance adaptation. No branch,
history, review, or GitHub state change is part of these local fixes.
