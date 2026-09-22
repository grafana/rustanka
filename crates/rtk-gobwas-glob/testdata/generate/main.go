// Emits the truth table rtk-gobwas-glob is tested against, using the exact
// library and version tk links.
//
// Read the table, never the documentation: the reason this generator exists is
// that gobwas' separator handling is not what the syntax comment implies. With
// no separators — which is how tk compiles every --exclude — `*` and `**` are
// the same matcher and both cross `/`.
package main

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/gobwas/glob"
)

// Case is one compiled pattern and everything it was asked.
type Case struct {
	Pattern string `json:"pattern"`
	// Separators passed to glob.Compile, as a string of runes. Empty is what
	// tk does.
	Separators string `json:"separators,omitempty"`
	// Error is glob.Compile's message, verbatim, when it rejected the pattern.
	Error   string   `json:"error,omitempty"`
	Matches []string `json:"matches,omitempty"`
	Rejects []string `json:"rejects,omitempty"`
}

func main() {
	patterns := []string{
		// tk's own defaults, which every rtk fmt and rtk lint run compiles.
		"**/.*", ".*", "**/vendor/**", "vendor/**",
		// Plain literals, and the fact that the whole subject must match.
		"main.jsonnet", "", "a", "a/b/c",
		// Any vs super. With no separators these are indistinguishable, which
		// is the whole point.
		"*", "**", "*/*", "**/**", "a*", "*a", "a*b", "a**b", "***",
		// Single.
		"?", "??", "a?c", "a?", "?a", "a/?/b",
		// Character classes and ranges, negated and not.
		"[abc]", "[!abc]", "[a-z]", "[A-Z]", "[!a-z]", "[0-9]", "[a-z][0-9]",
		"a[bc]d", "[.]*", "[/]", "[!/]", "[!/]*",
		// Alternation, including empty and nested alternatives.
		"{a,b}", "{a,b}c", "{,a}b", "{a,}b", "{a,b,c}", "{jsonnet,libsonnet}",
		"*.{jsonnet,libsonnet}", "{a,{b,c}}d", "{a/b,c/d}", "{}", "{a}",
		// Escapes.
		`a\*b`, `\[a\]`, `\{a\}`, `a\?b`, `\\`, `\*`,
		// The combinations the default excludes are made of.
		"**/.*/**", "*/vendor/*", "vendor", "**/node_modules/**",
		// Realistic paths as patterns.
		"environments/**", "environments/*/main.jsonnet", "lib/**/*.libsonnet",
		// The empty subject, where gobwas answers from the shape of the matcher
		// its optimiser built rather than from what the pattern means: `?` and
		// a negated class match "" because DecodeRuneInString yields
		// (RuneError, 0), while any pattern that compiles to a BTree cannot
		// match "" however zero-width its parts are.
		"[a]", "[!a]", "{?,a}", "{[!a],b}", "{**,a}", "{,}", "****", "**?",
		"?**", "**[!a]", "[!a]**",
		// Malformed.
		"[", "[a", "[a-", "[a-]", "[]", "[!]", "[z-a]", "{", "{a", "{a,",
		"[a-bc]", "[ab-]", "]", "}", ",",
	}

	// Subjects are the shapes FindFiles actually hands the excludes, plus the
	// cases that separate `*` from `**`.
	subjects := []string{
		"", "a", "b", "c", "d", "ac", "bc", "ad", "bd", "abc", "abd", "a*b",
		"a?b", `\`, "[a]", "{a}", "q", "Q", "0", "a0", ".",
		"main.jsonnet", "main.libsonnet", "README.md", "a/b", "a/b/c",
		"a//b", "/a", "a/", ".git", ".git/config", ".gitignore",
		"vendor", "vendor/", "vendor/k.libsonnet", "vendor/github.com/a/b.libsonnet",
		"environments/default/main.jsonnet", "environments/vendor/k.libsonnet",
		"lib/foo/bar.libsonnet", "node_modules/x/y.js",
		"/abs/vendor/k.libsonnet", "./vendor/k.libsonnet", "a/.hidden/b.jsonnet",
		"jsonnet", "libsonnet", "a.jsonnet", "a.libsonnet",
	}

	// Every pattern is asked with no separators (what tk does) and with "/"
	// (which is what a reader of the syntax documentation would assume).
	separatorSets := []string{"", "/"}

	out := make([]Case, 0, len(patterns)*len(separatorSets))
	for _, sep := range separatorSets {
		for _, p := range patterns {
			entry := Case{Pattern: p, Separators: sep}
			g, err := glob.Compile(p, []rune(sep)...)
			if err != nil {
				entry.Error = err.Error()
				out = append(out, entry)
				continue
			}
			for _, s := range subjects {
				if g.Match(s) {
					entry.Matches = append(entry.Matches, s)
				} else {
					entry.Rejects = append(entry.Rejects, s)
				}
			}
			out = append(out, entry)
		}
	}

	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(out); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
