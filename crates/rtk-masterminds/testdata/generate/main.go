// Emits the truth table rtk-masterminds is tested against, using the exact
// library and version tk links.
package main

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/Masterminds/semver"
)

type Case struct {
	Constraint string   `json:"constraint"`
	Error      string   `json:"error,omitempty"`
	Matches    []string `json:"matches,omitempty"`
	Rejects    []string `json:"rejects,omitempty"`
}

func main() {
	constraints := []string{
		// Bare and explicit equality.
		"1.2.3", "=1.2.3", "= 1.2.3", "v1.2.3", "!=1.2.3",
		// Ordering.
		">1.2.3", ">=1.2.3", "=>1.2.3", "<1.2.3", "<=1.2.3", "=<1.2.3",
		// Tilde and caret, including the 0.x cases that differ from Rust/npm.
		"~1.2.3", "~>1.2.3", "~1.2", "~1", "~0.0.0", "^1.2.3", "^0.1.2", "^0.0.3", "^1.2", "^1",
		// Wildcards and partial versions.
		"1.2.x", "1.x", "x", "*", "1.X", "1.2.*",
		// Conjunction, disjunction, hyphen ranges.
		">=1.2.0, <2.0.0", ">=1.2.0 , <2.0.0", ">=0.0.0 || <0.0.0", ">= 0.0.0 || < 0.0.0",
		"<1.0.0 || >=2.0.0", "1.2.0 - 1.4.0", ">=1.0.0 <2.0.0",
		// Prereleases.
		">=1.2.3-alpha", "1.2.3-alpha", ">1.0.0-0", "~1.2.3-beta.1",
		// Whitespace and oddities.
		"  >=1.2.3  ", ">=v1.2.3", "0.38.0", ">=0.38.0", ">0.38.0", "<0.38.0", ">=0.39.0",
		// More prereleases and metadata.
		"^1.2.3-alpha", "!=1.2.3-alpha", "1.2.3+build", ">=1.2.3+build", "~1.2.3-alpha",
		"<1.2.3-beta.1", ">=1.0.0-0",
		// Wildcards with every operator.
		">=1.x", "<=1.x", ">1.x", "<1.x", "!=1.x", "~1.x", "^1.x", "!=1.2",
		"~>1.2", "~>1", "~>0.38", "0.x", "0.38.x",
		// Longer conjunctions and mixed spacing.
		">=1.0.0, <=2.0.0, !=1.5.0", ">=1.0.0,<2.0.0", "1 - 2", "0.38.0 - 0.39.0",
		">=0.38.0, <0.39.0", ">=0.38.0 || >=1.0.0",
		// Malformed.
		"nonsense", ">=", "1.2.3.4", ">>1.0.0", "", "&&1.0.0", ">=1.2.3 ||",
		"1.2.3 -", "- 1.2.3", "~~1.0.0", "1.2.3,", ",1.2.3", "v", "x.y.z",
	}
	versions := []string{
		"0.0.0", "0.1.2", "0.0.3", "0.38.0", "1.0.0", "1.2.0", "1.2.3", "1.2.4", "1.3.0",
		"1.4.0", "2.0.0", "1.2.3-alpha", "1.2.3-beta.1", "1.2.3-alpha.1", "1.0.0-0",
		"0.39.0", "10.0.0",
	}

	out := make([]Case, 0, len(constraints))
	for _, c := range constraints {
		entry := Case{Constraint: c}
		cs, err := semver.NewConstraint(c)
		if err != nil {
			entry.Error = err.Error()
			out = append(out, entry)
			continue
		}
		for _, raw := range versions {
			v, err := semver.NewVersion(raw)
			if err != nil {
				fmt.Fprintf(os.Stderr, "bad test version %q: %s\n", raw, err)
				os.Exit(1)
			}
			if cs.Check(v) {
				entry.Matches = append(entry.Matches, raw)
			} else {
				entry.Rejects = append(entry.Rejects, raw)
			}
		}
		out = append(out, entry)
	}

	encoded, err := json.MarshalIndent(out, "", "  ")
	if err != nil {
		panic(err)
	}
	fmt.Println(string(encoded))
}
