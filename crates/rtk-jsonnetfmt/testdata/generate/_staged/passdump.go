// Dumps the AST each formatter pass leaves behind, so rtk's passes can be
// graded against real values rather than against a reading of Go's source.
//
// # Why this one has to be staged into a checkout
//
// `internal/formatter` is an internal package: only code whose import path
// starts with `github.com/google/go-jsonnet/` may import it. So no program in
// the generate module can construct a `FixTrailingCommas` and run it, and the
// public `formatter` package exports only `Format`, `FormatNode` and
// `SnippetToRawAST` — whole pipelines, never one pass.
//
// Text-level isolation through `Options` does not close the gap either. Three
// passes have a flag (`PrettyFieldNames`, and `Indent` gates `FixIndentation`)
// but `FixTrailingCommas`, `FixNewlines`, `FixParens` and
// `NoRedundantSliceColon` are unconditional, so there is no option setting
// under which their effect can be observed alone.
//
// Hence this: `make update-fmt-pass-oracle` copies this file and
// nodedump/dump.go into `<checkout>/rtkpassdump/` and runs it there, where the
// internal import is legal. It is the same staging trick
// `make update-fmt-lexer-oracle` uses for the token oracle, minus the
// `_test.go` part — a `package main` inside the module is enough.
//
// It lives under `_staged/` because the go tool ignores directories whose name
// begins with an underscore: this file will not compile in the generate module
// (the internal import is refused there) and must not be offered to it.
//
// # One pass at a time, on a freshly parsed tree
//
// Each pass is applied **in isolation** to its own parse of the source, not to
// the state the pipeline would have reached by that point. That is deliberate,
// and it is what makes the oracle usable while the port is unfinished: a
// cumulative dump is only correct once everything before it is correct, so it
// could not grade `FixTrailingCommas` until `SortImports` and `FixNewlines`
// were also written — which is exactly the incremental landing the quarantine
// ratchet exists to allow. The order the passes run in is a fourteen-line list
// read off `FormatNode`, and the end-to-end corpus grades it.
//
// A pass that changes nothing on a source records `unchanged` instead of the
// tree, which keeps the oracle small: most passes are no-ops on most files,
// because most Jsonnet in the repository is already `tk fmt`-clean. The Rust
// side then has to assert its own no-op, which is a real claim — a pass that
// mangles an already-formatted file fails there.
//
// # The one gap
//
// `removeInitialNewlines` and `removeExtraTrailingNewlines` are unexported
// functions, not passes, so they cannot be reached even from here; they would
// need this program to become a `_test.go` inside `internal/formatter`. Both
// are four lines and both are covered end-to-end by the corpus. Everything
// else in `FormatNode` is reachable: `SortImports` is exported, the visitors
// expose `File` through the embedded `pass.Base`, and `FixIndentation` exposes
// `VisitFile`.
//
// Usage:
//
//	go run ./rtkpassdump files <repo root> <corpus manifest> <out>
//	go run ./rtkpassdump snippets <snippets file> <out>
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/google/go-jsonnet/ast"
	iformatter "github.com/google/go-jsonnet/internal/formatter"
	iparser "github.com/google/go-jsonnet/internal/parser"
)

// passNames is the steps of FormatNode this oracle records, in the order
// FormatNode runs them.
//
// `AddPlusObject` is the `UseImplicitPlus: false` branch, so it and
// `RemovePlusObject` are alternatives rather than consecutive steps; both are
// dumped, because upstream's own `TestFormatNoImplicitPlus` exercises the
// other branch and the Phase 0 fixtures port those nine cases.
//
// **The three strip passes are deliberately absent.** All three are skipped
// under `DefaultOptions`, which is the only configuration `tk fmt` uses, and
// `docs/rtk-fmt-plan.md` schedules none of them. They are also what made this
// oracle unreviewable: each rewrites *every* tree it touches, so
// `StripAllButComments` and `StripEverything` alone accounted for roughly 270
// of the 356 changed cells over the corpus and most of a 23 MB file — a diff
// nobody could read, regenerated on every go-jsonnet bump. Adding one back is
// one line here.
var passNames = []string{
	"SortImports",
	"EnforceMaxBlankLines",
	"FixNewlines",
	"FixTrailingCommas",
	"FixParens",
	"RemovePlusObject",
	"AddPlusObject",
	"NoRedundantSliceColon",
	"PrettyFieldNames",
	"EnforceStringStyle",
	"EnforceCommentStyle",
	"FixIndentation",
}

// PassDump is what one pass did to one source.
type PassDump struct {
	Pass string `json:"pass"`
	// Unchanged is set where the pass left the tree exactly as parsed, in
	// which case Entries is absent. Most cells are this one.
	Unchanged bool    `json:"unchanged,omitempty"`
	Entries   []Entry `json:"entries,omitempty"`
	// Panic records a pass that crashed on this input. Kept rather than
	// aborting: one pathological file must not cost the whole oracle, and a
	// crash is itself worth pinning — a port that does not crash there has
	// diverged.
	Panic string `json:"panic,omitempty"`
}

// PassFileDump is one source file's answers, one per pass.
type PassFileDump struct {
	Source string `json:"source"`
	// Error is the parser's refusal, in which case Passes is absent.
	Error  string     `json:"error,omitempty"`
	Passes []PassDump `json:"passes,omitempty"`
}

// applyPass runs exactly one step of FormatNode over a whole file.
//
// The visitors are invoked through `File`, which `pass.Base` provides and
// which is what upstream's own `visitFile` helper calls. `SortImports` is a
// free function and `FixIndentation` has its own entry point, exactly as
// `FormatNode` treats them.
func applyPass(name string, node *ast.Node, finalFodder *ast.Fodder) {
	options := iformatter.DefaultOptions()

	// The strip passes are constructible here the same way the rest are; they
	// are simply not in `passNames`. See the note there before adding one.
	switch name {
	case "SortImports":
		iformatter.SortImports(node)
	case "EnforceMaxBlankLines":
		p := &iformatter.EnforceMaxBlankLines{Options: options}
		p.File(p, node, finalFodder)
	case "FixNewlines":
		p := &iformatter.FixNewlines{}
		p.File(p, node, finalFodder)
	case "FixTrailingCommas":
		p := &iformatter.FixTrailingCommas{}
		p.File(p, node, finalFodder)
	case "FixParens":
		p := &iformatter.FixParens{}
		p.File(p, node, finalFodder)
	case "RemovePlusObject":
		p := &iformatter.RemovePlusObject{}
		p.File(p, node, finalFodder)
	case "AddPlusObject":
		p := &iformatter.AddPlusObject{}
		p.File(p, node, finalFodder)
	case "NoRedundantSliceColon":
		p := &iformatter.NoRedundantSliceColon{}
		p.File(p, node, finalFodder)
	case "StripComments":
		p := &iformatter.StripComments{}
		p.File(p, node, finalFodder)
	case "StripAllButComments":
		p := &iformatter.StripAllButComments{}
		p.File(p, node, finalFodder)
	case "StripEverything":
		p := &iformatter.StripEverything{}
		p.File(p, node, finalFodder)
	case "PrettyFieldNames":
		p := &iformatter.PrettyFieldNames{}
		p.File(p, node, finalFodder)
	case "EnforceStringStyle":
		p := &iformatter.EnforceStringStyle{Options: options}
		p.File(p, node, finalFodder)
	case "EnforceCommentStyle":
		p := &iformatter.EnforceCommentStyle{Options: options}
		p.File(p, node, finalFodder)
	case "FixIndentation":
		// Not an ASTPass: FormatNode calls this entry point directly.
		visitor := iformatter.FixIndentation{Options: options}
		visitor.VisitFile(*node, *finalFodder)
	default:
		panic("passdump: unknown pass " + name)
	}
}

func parse(source, content string) (ast.Node, ast.Fodder, error) {
	// The same call formatter.Format makes.
	return iparser.SnippetToRawAST(ast.DiagnosticFileName(source), "", content)
}

// runPass parses afresh, applies one pass, and reports the tree it produced —
// or `unchanged`, or the panic it died with.
func runPass(name, source, content string, raw []byte) (result PassDump) {
	result = PassDump{Pass: name}

	node, finalFodder, err := parse(source, content)
	if err != nil {
		// Unreachable: the caller only gets here for a source that parsed.
		result.Panic = "reparse failed: " + err.Error()
		return result
	}

	defer func() {
		if recovered := recover(); recovered != nil {
			result = PassDump{Pass: name, Panic: fmt.Sprint(recovered)}
		}
	}()

	applyPass(name, &node, &finalFodder)

	entries := dumpTree(node, finalFodder)
	encoded, err := json.Marshal(entries)
	if err != nil {
		fail(err)
	}
	if string(encoded) == string(raw) {
		result.Unchanged = true
		return result
	}
	result.Entries = entries
	return result
}

func dumpOne(source, content string) PassFileDump {
	dump := PassFileDump{Source: source}

	node, finalFodder, err := parse(source, content)
	if err != nil {
		dump.Error = err.Error()
		return dump
	}

	// The tree as parsed, so each pass can say `unchanged` instead of
	// repeating it.
	raw, err := json.Marshal(dumpTree(node, finalFodder))
	if err != nil {
		fail(err)
	}

	for _, name := range passNames {
		dump.Passes = append(dump.Passes, runPass(name, source, content, raw))
	}
	return dump
}

func main() {
	usage := "usage: go run ./rtkpassdump files <repo root> <corpus manifest> <out>\n" +
		"       go run ./rtkpassdump snippets <snippets file> <out>"

	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, usage)
		os.Exit(2)
	}

	var dumps []PassFileDump

	switch os.Args[1] {
	case "files":
		if len(os.Args) != 5 {
			fmt.Fprintln(os.Stderr, usage)
			os.Exit(2)
		}
		repoRoot, manifestPath, out := os.Args[2], os.Args[3], os.Args[4]

		var manifest Manifest
		if err := readJSON(manifestPath, &manifest); err != nil {
			fail(err)
		}
		for _, entry := range manifest.Entries {
			content, err := os.ReadFile(filepath.Join(repoRoot, entry.Source))
			if err != nil {
				fail(err)
			}
			dumps = append(dumps, dumpOne(entry.Source, string(content)))
		}
		if err := writeJSON(out, dumps); err != nil {
			fail(err)
		}

	case "snippets":
		if len(os.Args) != 4 {
			fmt.Fprintln(os.Stderr, usage)
			os.Exit(2)
		}
		snippetsPath, out := os.Args[2], os.Args[3]

		var snippets []Snippet
		if err := readJSON(snippetsPath, &snippets); err != nil {
			fail(err)
		}
		for _, snippet := range snippets {
			dumps = append(dumps, dumpOne(snippet.Name, snippet.Source))
		}
		if err := writeJSON(out, dumps); err != nil {
			fail(err)
		}

	default:
		fmt.Fprintln(os.Stderr, usage)
		os.Exit(2)
	}

	changed, unchanged, panicked, refused := 0, 0, 0, 0
	for _, dump := range dumps {
		if dump.Error != "" {
			refused++
		}
		for _, p := range dump.Passes {
			switch {
			case p.Panic != "":
				panicked++
			case p.Unchanged:
				unchanged++
			default:
				changed++
			}
		}
	}
	fmt.Fprintf(os.Stderr,
		"dumped %d sources (%d refused): %d passes changed something, %d were no-ops, %d panicked\n",
		len(dumps), refused, changed, unchanged, panicked)
}
