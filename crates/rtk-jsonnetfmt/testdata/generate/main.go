// Emits the corpus rtk-jsonnetfmt is graded against, by running go-jsonnet's
// formatter over every Jsonnet file already in this repository.
//
// This replaces the round-trip gate docs/rtk-fmt-plan.md originally asked for.
// That gate cannot hold: go-jsonnet's unparser renders text *from* the fodder
// model rather than copying the source, so `unparse(parse(x)) == x` is false
// even with every pass disabled — a tab becomes eight spaces, a line-end
// comment is always preceded by exactly two spaces, and \r is dropped from
// block strings. A corpus of real answers is both achievable and stricter.
//
// It calls formatter.Format directly rather than shelling out to `tk fmt`:
//
//   - `tk fmt --stdout` prefixes "// <name>" and writes spacing to stderr, so
//     the wrapper text would have to be stripped back off
//   - the name reaches error messages, and `tk fmt -` would bake a temporary
//     path into any golden that records one
//   - Options can be varied, which `tk` exposes no way to do — needed for the
//     UseImplicitPlus: false cases
//
// The oracle is necessarily text-level: go-jsonnet's internal/parser is an
// internal package, so nothing outside the module can reach the fodder itself.
package main

import (
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"runtime/debug"
	"sort"
	"strings"

	"github.com/google/go-jsonnet/formatter"
)

// Entry is one formatted file.
type Entry struct {
	// Source is the path relative to the repository root, and is also what is
	// handed to Format as the diagnostic filename — so the goldens do not
	// depend on where the repository is checked out.
	Source string `json:"source"`
	// Golden is the file under the corpus directory holding the answer.
	Golden string `json:"golden"`
	// Error is true when Format failed and the golden holds its message
	// instead of formatted output, the way go-jsonnet's own harness folds
	// errors into goldens with coalesceError.
	Error bool `json:"error,omitempty"`
}

// Manifest is what the Rust test reads, so it does not have to re-derive
// discovery and so a source file that disappears shows up as a stale entry.
type Manifest struct {
	// GoJsonnetVersion records which go-jsonnet produced these answers.
	GoJsonnetVersion string  `json:"goJsonnetVersion"`
	Entries          []Entry `json:"entries"`
}

// Roots searched for Jsonnet, cheapest and most varied first. `vendor` is
// deliberately included even though `tk fmt` excludes it by default: vendored
// libsonnet is the widest variety of third-party style available, which is
// exactly what stresses a fodder-preserving formatter.
var roots = []string{
	"tests/suite",
	"tests/golden",
	"tests/realworld",
	"test_fixtures/golden_envs",
	"crates/jrsonnet-formatter/src/tests",
}

func goJsonnetVersion() string {
	info, ok := debug.ReadBuildInfo()
	if !ok {
		return "unknown"
	}
	for _, dep := range info.Deps {
		if dep.Path == "github.com/google/go-jsonnet" {
			return dep.Version
		}
	}
	return "unknown"
}

// goldenName flattens a source path into a single filename, so the corpus is
// one flat reviewable directory rather than a deep mirror of the repository.
func goldenName(source string) string {
	return strings.ReplaceAll(source, "/", "__") + ".golden"
}

func findJsonnet(repoRoot string) ([]string, error) {
	var sources []string
	for _, root := range roots {
		err := filepath.WalkDir(filepath.Join(repoRoot, root), func(path string, d fs.DirEntry, err error) error {
			if err != nil {
				return err
			}
			if d.IsDir() {
				return nil
			}
			if ext := filepath.Ext(path); ext != ".jsonnet" && ext != ".libsonnet" {
				return nil
			}
			relative, err := filepath.Rel(repoRoot, path)
			if err != nil {
				return err
			}
			sources = append(sources, filepath.ToSlash(relative))
			return nil
		})
		if err != nil {
			return nil, err
		}
	}
	// Sorted so the manifest is stable across runs and filesystems.
	sort.Strings(sources)
	return sources, nil
}

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: go run . <repo root> <corpus dir>")
		os.Exit(2)
	}
	repoRoot, corpusDir := os.Args[1], os.Args[2]

	sources, err := findJsonnet(repoRoot)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}

	// Start from an empty directory so a source file that has been deleted
	// cannot leave an orphaned golden behind.
	if err := os.RemoveAll(corpusDir); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	if err := os.MkdirAll(corpusDir, 0o755); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}

	manifest := Manifest{GoJsonnetVersion: goJsonnetVersion()}
	failures := 0

	for _, source := range sources {
		content, err := os.ReadFile(filepath.Join(repoRoot, source))
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}

		entry := Entry{Source: source, Golden: goldenName(source)}
		answer, formatErr := formatter.Format(source, string(content), formatter.DefaultOptions())
		if formatErr != nil {
			answer = formatErr.Error()
			entry.Error = true
			failures++
		}

		if err := os.WriteFile(filepath.Join(corpusDir, entry.Golden), []byte(answer), 0o644); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		manifest.Entries = append(manifest.Entries, entry)
	}

	encoded, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	encoded = append(encoded, '\n')
	if err := os.WriteFile(filepath.Join(corpusDir, "manifest.json"), encoded, 0o644); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}

	fmt.Fprintf(os.Stderr, "wrote %d goldens (%d of them parse errors) using go-jsonnet %s\n",
		len(manifest.Entries), failures, manifest.GoJsonnetVersion)
}
