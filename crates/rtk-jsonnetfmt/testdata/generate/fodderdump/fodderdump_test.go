// Dumps the tokens and fodder go-jsonnet's lexer produces, as JSON, so rtk's
// lexer port can be graded against it rather than against a reading of the
// source.
//
// # Why this is a _test.go, and why it is copied into a checkout
//
// Fodder is only observable from inside go-jsonnet:
//
//   - `internal/parser` is an internal package, so nothing outside the
//     go-jsonnet module can import it, and there is no public parse-to-AST
//     entry point
//   - `parser.Lex` returns `Tokens`, whose fields — `fodder`, `data`, `kind` —
//     are unexported, so even a program inside the module cannot read them
//     unless it is in package `parser` itself
//
// So `make update-fmt-lexer-oracle` copies this file into
// `internal/parser/` of a go-jsonnet checkout and runs it as a test. That is
// the only way to reach the values, and grading the lexer against real fodder
// is worth the staging step: everything downstream of the lexer inherits its
// mistakes, and the corpus can only see them once they have travelled all the
// way to text.
//
// Usage, from the staged checkout, in either of two modes:
//
//	# Files: FODDERDUMP_SOURCES is newline-separated paths relative to
//	# FODDERDUMP_ROOT, so the recorded names match the corpus manifest's.
//	FODDERDUMP_ROOT=/path/to/rustanka \
//	FODDERDUMP_SOURCES=/path/to/list.txt \
//	FODDERDUMP_OUT=/path/to/lexer-oracle.json \
//	go test -count=1 -run TestDumpFodder ./internal/parser
//
//	# Snippets: FODDERDUMP_SNIPPETS is the lexer-snippets.json file, whose
//	# sources are inline rather than on disk.
//	FODDERDUMP_SNIPPETS=/path/to/lexer-snippets.json \
//	FODDERDUMP_OUT=/path/to/lexer-snippet-oracle.json \
//	go test -count=1 -run TestDumpFodder ./internal/parser
//
// The snippets mode exists because the corpus cannot reach everything: it holds
// no verbatim string and provokes no lexer error, so without it those
// behaviours would be pinned by expectations derived by hand from this source
// rather than by answers taken from it.
package parser

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/google/go-jsonnet/ast"
)

// FodderElementDump mirrors ast.FodderElement. Kind is spelled out rather than
// left as the integer, so a reader of the JSON does not need go-jsonnet's
// iota order in front of them.
type FodderElementDump struct {
	CommentHex []string `json:"commentHex,omitempty"`
	Kind       string   `json:"kind"`
	Blanks     int      `json:"blanks"`
	Indent     int      `json:"indent"`
	Comment    []string `json:"comment,omitempty"`
}

// TokenDump is one token together with the fodder that preceded it.
type TokenDump struct {
	DataHex string `json:"dataHex,omitempty"`
	// One-based lines and byte columns; End is exclusive, including for EOF.
	Begin  [2]int              `json:"begin"`
	End    [2]int              `json:"end"`
	Kind   string              `json:"kind"`
	Data   string              `json:"data,omitempty"`
	Fodder []FodderElementDump `json:"fodder,omitempty"`
	// StringBlockIndent and StringBlockTermIndent only ever appear on a
	// STRING_BLOCK, and the unparser reproduces both verbatim, so a port that
	// loses them is wrong in a way only block strings would reveal.
	StringBlockIndent     string `json:"stringBlockIndent,omitempty"`
	StringBlockTermIndent string `json:"stringBlockTermIndent,omitempty"`
}

// FileDump is one source file's answer.
type FileDump struct {
	Source string      `json:"source"`
	Tokens []TokenDump `json:"tokens,omitempty"`
	// Error is the lexer's message when it refused the file, in which case
	// Tokens is absent. The message is part of the contract: it propagates out
	// of tk fmt and aborts the run.
	Error string `json:"error,omitempty"`
}

func fodderKindName(kind ast.FodderKind) string {
	switch kind {
	case ast.FodderLineEnd:
		return "LineEnd"
	case ast.FodderInterstitial:
		return "Interstitial"
	case ast.FodderParagraph:
		return "Paragraph"
	default:
		return "unknown"
	}
}

// JSON strings cannot preserve invalid UTF-8. Hex is authoritative only
// where the raw value is invalid; ordinary fixtures stay readable.
func rawText(text string) string {
	if utf8.ValidString(text) {
		return ""
	}
	return hex.EncodeToString([]byte(text))
}

func rawComments(comments []string) []string {
	invalid := false
	for _, comment := range comments {
		invalid = invalid || !utf8.ValidString(comment)
	}
	if !invalid {
		return nil
	}
	out := make([]string, len(comments))
	for i, comment := range comments {
		out[i] = hex.EncodeToString([]byte(comment))
	}
	return out
}

func dumpFodder(fodder ast.Fodder) []FodderElementDump {
	out := make([]FodderElementDump, 0, len(fodder))
	for _, element := range fodder {
		out = append(out, FodderElementDump{
			CommentHex: rawComments(element.Comment),
			Kind:       fodderKindName(element.Kind),
			Blanks:     element.Blanks,
			Indent:     element.Indent,
			Comment:    element.Comment,
		})
	}
	if len(out) == 0 {
		return nil
	}
	return out
}

// Snippet is one entry of lexer-snippets.json. `why` is documentation for
// human readers and is ignored here.
type Snippet struct {
	Name      string `json:"name"`
	Source    string `json:"source"`
	SourceHex string `json:"sourceHex"`
}

// dumpOne lexes `content` under the name `source`, recording either its tokens
// or the lexer's refusal.
func dumpOne(source, content string) FileDump {
	dump := FileDump{Source: source}
	// The same two arguments formatter.Format passes: the diagnostic filename,
	// and an empty imported filename.
	tokens, lexErr := Lex(ast.DiagnosticFileName(source), "", content)
	if lexErr != nil {
		dump.Error = lexErr.Error()
		return dump
	}
	for _, token := range tokens {
		dump.Tokens = append(dump.Tokens, TokenDump{
			Begin:                 [2]int{token.loc.Begin.Line, token.loc.Begin.Column},
			End:                   [2]int{token.loc.End.Line, token.loc.End.Column},
			Kind:                  token.kind.String(),
			Data:                  token.data,
			DataHex:               rawText(token.data),
			Fodder:                dumpFodder(token.fodder),
			StringBlockIndent:     token.stringBlockIndent,
			StringBlockTermIndent: token.stringBlockTermIndent,
		})
	}
	return dump
}

func TestDumpFodder(t *testing.T) {
	root := os.Getenv("FODDERDUMP_ROOT")
	sourceList := os.Getenv("FODDERDUMP_SOURCES")
	snippetFile := os.Getenv("FODDERDUMP_SNIPPETS")
	out := os.Getenv("FODDERDUMP_OUT")
	if out == "" || (sourceList == "" && snippetFile == "") {
		t.Skip("FODDERDUMP_OUT plus one of FODDERDUMP_SOURCES or FODDERDUMP_SNIPPETS must be set")
	}

	var dumps []FileDump

	if snippetFile != "" {
		raw, err := os.ReadFile(snippetFile)
		if err != nil {
			t.Fatalf("reading %s: %v", snippetFile, err)
		}
		var snippets []Snippet
		if err := json.Unmarshal(raw, &snippets); err != nil {
			t.Fatalf("parsing %s: %v", snippetFile, err)
		}
		for _, snippet := range snippets {
			source := snippet.Source
			if snippet.SourceHex != "" {
				bytes, err := hex.DecodeString(snippet.SourceHex)
				if err != nil {
					t.Fatal(err)
				}
				source = string(bytes)
			}
			dumps = append(dumps, dumpOne(snippet.Name, source))
		}
	} else {
		if root == "" {
			t.Fatal("FODDERDUMP_ROOT is required with FODDERDUMP_SOURCES")
		}
		listed, err := os.ReadFile(sourceList)
		if err != nil {
			t.Fatalf("reading %s: %v", sourceList, err)
		}
		for _, source := range strings.Split(strings.TrimSpace(string(listed)), "\n") {
			source = strings.TrimSpace(source)
			if source == "" {
				continue
			}
			content, err := os.ReadFile(filepath.Join(root, source))
			if err != nil {
				t.Fatalf("reading %s: %v", source, err)
			}
			dumps = append(dumps, dumpOne(source, string(content)))
		}
	}

	encoded, err := json.MarshalIndent(dumps, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	encoded = append(encoded, '\n')
	if err := os.WriteFile(out, encoded, 0o644); err != nil {
		t.Fatal(err)
	}
	t.Logf("dumped %d files to %s", len(dumps), out)
}

// String-unescaping errors can contain incomplete UTF-8 prefixes. Record
// bytes as hex so neither JSON nor a test harness can replace them.
func TestDumpStrings(t *testing.T) {
	input := os.Getenv("FODDERDUMP_STRINGS")
	output := os.Getenv("FODDERDUMP_OUT")
	if input == "" || output == "" {
		t.Skip("string dump not requested")
	}
	raw, err := os.ReadFile(input)
	if err != nil {
		t.Fatal(err)
	}
	var snippets []Snippet
	if err := json.Unmarshal(raw, &snippets); err != nil {
		t.Fatal(err)
	}
	if len(snippets) == 0 {
		t.Fatal("empty string snippets")
	}
	type result struct {
		Name      string `json:"name"`
		InputHex  string `json:"inputHex"`
		OutputHex string `json:"outputHex"`
		ErrorHex  string `json:"errorHex"`
	}
	var results []result
	for _, snippet := range snippets {
		source := []byte(snippet.Source)
		if snippet.SourceHex != "" {
			source, err = hex.DecodeString(snippet.SourceHex)
			if err != nil {
				t.Fatal(err)
			}
		}
		value, failure := StringUnescape(&ast.LocationRange{}, string(source))
		entry := result{Name: snippet.Name, InputHex: hex.EncodeToString(source), OutputHex: hex.EncodeToString([]byte(value))}
		if failure != nil {
			entry.ErrorHex = hex.EncodeToString([]byte(failure.Error()))
		}
		results = append(results, entry)
	}
	encoded, err := json.MarshalIndent(results, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(output, append(encoded, '\n'), 0o644); err != nil {
		t.Fatal(err)
	}
}
