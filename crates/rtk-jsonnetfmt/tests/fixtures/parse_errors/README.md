# Parse-error fixtures

go-jsonnet's own formatter harness folds a parse failure's **message** into the
golden, via `coalesceError`. It does that because `formatter.Format` returns the
error from `parser.SnippetToRawAST`, and `tk fmt` propagates it out of
`FormatFiles` and aborts the run — so the text is part of the contract, not a
detail.

Each `<name>.jsonnet` here is paired with `<name>.err.golden`.

## What the harness passes as the filename

`format` is called with the fixture's **basename including the extension**
(`unterminated_string.jsonnet`), not its path. go-jsonnet's
`staticError.Error()` is `"<loc> <message>"` and `<loc>` is prefixed with the
`DiagnosticFileName` it was handed, so a path here would bake the checkout
location into the golden.

## Trailing newline

The harness strips exactly one trailing newline from a `.err.golden` before
comparing. A go-jsonnet error message never ends in a newline, so the stripping
is unambiguous, and it lets these stay ordinary newline-terminated text files.
The `*.fmt.golden` files taken from go-jsonnet's own `formatter/testdata` are
compared byte for byte with no stripping at all.

## Where the expected text comes from

Each message and position below was derived from upstream source, not recalled.
Re-verify against the pinned go-jsonnet if any of it ever moves.

| fixture | produced by | upstream |
| --- | --- | --- |
| `unterminated_string` | the lexer, on reaching EOF inside a `'…'` | `internal/parser/lexer.go`, `case '\''` |
| `unclosed_brace` | the parser, popping the EOF token as if it were the next field | `internal/parser/parser.go`, `parseObjectRemainder` |
| `bad_token` | the lexer, on a character that starts nothing | `internal/parser/lexer.go`, `Lex` default arm |

`unclosed_brace` is the interesting one: the message is
`Expected a comma before next field`, which mentions neither the brace nor the
end of the file, and it is reported at line 2 column 1 — the position of the
synthetic end-of-file token. Nothing about that is guessable, which is the
reason to pin it.
