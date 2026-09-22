// Dumps the AST go-jsonnet's parser produces — every node, and every one of
// its named fodder slots — as JSON, so rtk's parser port can be graded against
// real values rather than against a reading of the source.
//
// The dumper itself is in dump.go, shared with the staged `passdump` program.
// This file is the CLI and the parse.
//
// # Why this exists, and why before the parser
//
// docs/rtk-fmt-plan.md argues the order, and the argument is empirical. The
// lexer went 138/138 on a first compile precisely because a token-level dump
// existed before a line of it was written; and of 16 lexer expectations derived
// by reading Go's source, 2 were wrong — both about fodder the model *composes*
// rather than reads. A parser composes fodder at nearly every node, so that is
// its dominant failure mode, and the text-level corpus is far too coarse to
// locate one: a misplaced CommaFodder surfaces as a whitespace diff hundreds of
// lines away, if at all. Here it surfaces as a diff at `$.Fields[2].CommaFodder`.
//
// It worked: the parser matched go-jsonnet on all 138 corpus files and all 63
// snippets, 32,332 entries, on its first compile.
//
// # Why this one needs no staged checkout
//
// The token dumper is a `_test.go` copied into `internal/parser/` of a
// go-jsonnet checkout, because `token`'s fields are unexported and there is no
// way to read them from outside that package. None of that applies here:
//
//   - `formatter.SnippetToRawAST` is exported, and is the same entry point
//     `formatter.Format` calls, so the AST is reachable from any program
//   - every fodder slot is an exported field of package `ast` — `Fodder1`,
//     `OpFodder`, `CommaFodder`, `RightBracketFodder` and the rest
//
// So this is an ordinary program in the existing generate module, pinned to the
// same go-jsonnet the corpus came from, run with `go run ./nodedump`. No clone,
// no staging, no `_test.go` trick. `make update-fmt-node-oracle` wraps it.
//
// The *pass* oracle is not so lucky — `internal/formatter` is internal, so
// nothing outside go-jsonnet's module can run a single pass. See
// ../_staged/passdump.go.
//
// Usage, in either of two modes:
//
//	# Files: sources are read from the corpus manifest, so this oracle and the
//	# corpus always grade the same files.
//	go run ./nodedump files <repo root> <corpus manifest> <out>
//
//	# Snippets: sources are inline in a JSON file, for what the corpus cannot
//	# reach. It holds no `@"..."` at all, and one file each with tailstrict or
//	# importbin, so without these those slots would be graded by nothing.
//	go run ./nodedump snippets <snippets file> <out>
package main

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/google/go-jsonnet/formatter"
)

// FileDump is one source file's answer.
type FileDump struct {
	Source  string  `json:"source"`
	Entries []Entry `json:"entries,omitempty"`
	// Error is the parser's message when it refused the file, in which case
	// Entries is absent. The message is part of the contract: it propagates
	// out of tk fmt and aborts the run.
	Error string `json:"error,omitempty"`
}

// dumpOne parses `content` under the name `source` and records either its AST
// or the parser's refusal.
func dumpOne(source, content string) FileDump {
	dump := FileDump{Source: source}

	// The same call formatter.Format makes.
	node, finalFodder, err := formatter.SnippetToRawAST(source, content)
	if err != nil {
		dump.Error = err.Error()
		return dump
	}

	dump.Entries = dumpTree(node, finalFodder)
	return dump
}

func main() {
	usage := "usage: go run ./nodedump files <repo root> <corpus manifest> <out>\n" +
		"       go run ./nodedump snippets <snippets file> <out>"

	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, usage)
		os.Exit(2)
	}

	var dumps []FileDump

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

	nodes, refused := 0, 0
	for _, dump := range dumps {
		nodes += len(dump.Entries)
		if dump.Error != "" {
			refused++
		}
	}
	fmt.Fprintf(os.Stderr, "dumped %d files (%d refused), %d entries\n",
		len(dumps), refused, nodes)
}
