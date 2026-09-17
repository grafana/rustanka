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
//
// # Two sets, and why they are stored differently
//
// The corpus has two halves, and what splits them is where the *inputs* live.
//
//   - `in_repo` is every Jsonnet file under [roots]. Those inputs are committed
//     here already, so only the answers need writing: one golden per file in
//     the corpus directory, plus manifest.json. This is also the set the node,
//     pass and lexer oracles mirror file for file — tests/node_oracle.rs
//     asserts the node oracle covers the same list in the same order — so it
//     does not grow casually. Each of those oracles is regenerated whole on a
//     go-jsonnet bump and node-oracle.json alone is 11 MB.
//   - `go_jsonnet_testdata` is go-jsonnet's own root testdata/, whose inputs
//     are *not* in this repository. Committing only the answers would make the
//     corpus size depend on whether a checkout happened to be present, and
//     tests/corpus.rs asserts its counts exactly in both directions — an exact
//     assertion over an environment-dependent denominator cannot hold. So this
//     set is one JSON file carrying **both** halves of every entry:
//     self-contained, one reviewable artifact, the same numbers everywhere.
//
// The checkout the second set needs is a **required** argument rather than an
// optional one, deliberately. An invocation that quietly produced a corpus
// with one set missing would leave `make check-fmt-corpus` diffing a corpus
// nobody generated and reporting that it was up to date.
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
	"unicode/utf8"

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

// ExternalEntry is one file of a set whose inputs live outside this
// repository, so it carries the input as well as the answer.
type ExternalEntry struct {
	// Name is what is handed to Format as the diagnostic filename. It is
	// synthetic and says where the file came from, because the real path is a
	// checkout location that must not reach a committed artifact.
	Name string `json:"name"`
	// Input is the file verbatim.
	Input string `json:"input"`
	// Output is what Format returned, or its error message when Error is set —
	// the coalesceError convention the in-repo goldens already use.
	Output string `json:"output"`
	Error  bool   `json:"error,omitempty"`
}

// ExternalCorpus is one self-contained set.
type ExternalCorpus struct {
	GoJsonnetVersion string `json:"goJsonnetVersion"`
	// Set names the set, and tests/corpus.rs checks it: a file generated for a
	// different set would otherwise be graded under the wrong baseline.
	Set    string `json:"set"`
	Origin string `json:"origin"`
	// Files is the entry count, written out so the number is legible in a diff
	// rather than only countable by parsing the array.
	Files   int             `json:"files"`
	Entries []ExternalEntry `json:"entries"`
}

// Roots searched for Jsonnet, cheapest and most varied first.
//
// # About vendored libsonnet
//
// This comment used to claim that "`vendor` is deliberately included even
// though `tk fmt` excludes it by default". That was half true and is now said
// exactly, because a comment claiming coverage that does not exist is the same
// error as a test deferring a claim to an oracle that holds no cells for it.
//
// There is no top-level `vendor/` in this repository and none of these roots
// is one. What is true is that one root *contains* a real vendored tree:
// test_fixtures/golden_envs/kustomize_job_hash_env/vendor, eight files of
// jsonnet-libs/docsonnet, and the walk below does not exclude it — its
// doc-util/render.libsonnet is one of the three files Phase 2e's
// RemovePlusObject flipped, so third-party style has earned its keep here.
//
// The repository's other four vendor directories are deliberately left out:
// cmds/rtk/testdata and crates/rtk-jsonnet/testdata hold ten files between
// them, averaging 30 bytes, written as discovery fixtures rather than as
// Jsonnet — `{ test: 'env-vendor' }` and the like. Adding them would move the
// denominator and grade nothing, which is the failure mode
// testdata/corpus-baseline.toml warns about at length.
//
// Breadth over real third-party style comes from the second set (go-jsonnet's
// own testdata, see the package comment) and, in Phase 5, from
// tk-compare-grafana.toml, which is where vendored Grafana Jsonnet by the
// thousand actually lives.
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

// findExternalJsonnet walks a go-jsonnet checkout's root testdata/ and returns
// the paths, sorted.
//
// It walks rather than reading one directory because a later go-jsonnet may
// nest its fixtures. Today nothing below the top level is Jsonnet — the
// subdirectories there are multi-file *output* goldens and cpp-tests-override —
// so a bump that adds one moves the file count, which tests/corpus.rs asserts
// exactly and so surfaces as a reviewed change rather than as silent drift.
func findExternalJsonnet(testdataDir string) ([]string, error) {
	var sources []string
	err := filepath.WalkDir(testdataDir, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			return nil
		}
		if ext := filepath.Ext(path); ext != ".jsonnet" && ext != ".libsonnet" {
			return nil
		}
		sources = append(sources, path)
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Strings(sources)
	return sources, nil
}

// writeExternalCorpus formats every file of a go-jsonnet checkout's root
// testdata/ and writes inputs and answers together to outPath.
func writeExternalCorpus(checkout, outPath string) error {
	testdataDir := filepath.Join(checkout, "testdata")
	info, err := os.Stat(testdataDir)
	if err != nil || !info.IsDir() {
		return fmt.Errorf(
			"%s is not a directory, so the go_jsonnet_testdata set cannot be generated; "+
				"point the third argument at a go-jsonnet checkout (see `make update-fmt-corpus`)",
			testdataDir,
		)
	}

	sources, err := findExternalJsonnet(testdataDir)
	if err != nil {
		return err
	}
	// A set of nothing would sail through every downstream count as a corpus
	// that simply has no files in it.
	if len(sources) == 0 {
		return fmt.Errorf("%s holds no .jsonnet or .libsonnet files at all", testdataDir)
	}

	corpus := ExternalCorpus{
		GoJsonnetVersion: goJsonnetVersion(),
		Set:              "go_jsonnet_testdata",
		Origin:           "go-jsonnet's own root testdata/, inputs committed here because they are not in this repository",
	}
	failures := 0

	for _, source := range sources {
		content, err := os.ReadFile(source)
		if err != nil {
			return err
		}
		// encoding/json substitutes U+FFFD for invalid UTF-8 rather than
		// refusing, which would commit a corrupted input and grade rtk against
		// an answer for a file nobody has. Refuse instead.
		if !utf8.ValidString(string(content)) {
			return fmt.Errorf("%s is not valid UTF-8, so it cannot be stored in JSON verbatim", source)
		}

		relative, err := filepath.Rel(testdataDir, source)
		if err != nil {
			return err
		}
		// Synthetic, and never the checkout path: the name reaches error
		// messages, so a real path would bake a machine into the artifact.
		entry := ExternalEntry{
			Name:  "go-jsonnet/testdata/" + filepath.ToSlash(relative),
			Input: string(content),
		}
		answer, formatErr := formatter.Format(entry.Name, entry.Input, formatter.DefaultOptions())
		if formatErr != nil {
			answer = formatErr.Error()
			entry.Error = true
			failures++
		}
		entry.Output = answer
		corpus.Entries = append(corpus.Entries, entry)
	}
	corpus.Files = len(corpus.Entries)

	encoded, err := json.MarshalIndent(corpus, "", "  ")
	if err != nil {
		return err
	}
	encoded = append(encoded, '\n')
	if err := os.MkdirAll(filepath.Dir(outPath), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(outPath, encoded, 0o644); err != nil {
		return err
	}

	fmt.Fprintf(os.Stderr,
		"wrote %d go_jsonnet_testdata entries (%d of them parse errors) using go-jsonnet %s\n",
		corpus.Files, failures, corpus.GoJsonnetVersion)
	return nil
}

func main() {
	if len(os.Args) != 5 {
		fmt.Fprintln(os.Stderr,
			"usage: go run . <repo root> <corpus dir> <go-jsonnet checkout> <external corpus file>")
		os.Exit(2)
	}
	repoRoot, corpusDir := os.Args[1], os.Args[2]
	checkout, externalOut := os.Args[3], os.Args[4]

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

	fmt.Fprintf(os.Stderr, "wrote %d in_repo goldens (%d of them parse errors) using go-jsonnet %s\n",
		len(manifest.Entries), failures, manifest.GoJsonnetVersion)

	// Last, and fatal on failure: a run that wrote one set and not the other
	// would leave the two artifacts describing different go-jsonnet states.
	if err := writeExternalCorpus(checkout, externalOut); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
