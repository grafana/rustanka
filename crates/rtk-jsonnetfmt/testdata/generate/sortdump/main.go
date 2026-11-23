// Emits the truth table `crates/rtk-jsonnetfmt/src/go_sort.rs` is tested
// against, using the standard library's own `sort.Slice`.
//
// # Why a table rather than a reading of the source
//
// `go_sort.rs` exists because `sort.Slice` is not stable and rtk has to
// reproduce *which* unstable answer it gives. That answer is a permutation
// produced by pdqsort's pivot choices, partitions and pattern-breaking — the
// least plausible thing in this repository to derive correctly by hand. The
// same argument the lexer, node and pass oracles rest on: of 16 lexer
// expectations derived by reading Go, two were wrong.
//
// It also reaches code no import group ever will. `heapSort_func` needs
// `bits.Len(n)` consecutive unbalanced partitions and `breakPatterns_func`
// needs one, so both are unreachable from any realistic top-of-file import
// list; the long adversarial shapes below are what grade them.
//
// # What it records, and what it does not
//
// The **permutation** only, over integer keys: for each case, where each
// element of the sorted result came from. That is the whole of what differs
// between one correct sort and another, and integer keys keep the table small
// and the comparison sequence identical to the real one.
//
// It deliberately does not grade string comparison. Go's `<` on `string` and
// Rust's `Ord` on `str` are both bytewise, and the real comparator is graded
// through the pass oracle by the `sort_imports/case_is_bytewise` and
// `sort_imports/punctuation_sorts_before_letters` snippets.
//
// # This one needs no staged checkout
//
// Unlike the pass oracle, nothing here is internal: `sort` is an ordinary
// standard-library package, so this is a plain program in the generate
// module. It has no dependency on go-jsonnet either — it shares the module
// only to avoid a fourth go.mod.
//
// Usage:
//
//	go run ./sortdump <out>
//
// or, from the repo root, `make update-go-sort-truth-table`.
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"runtime"
	"sort"
)

// Case is one input and the permutation sort.Slice produced from it.
type Case struct {
	// Shape is the generator that built Keys, and Length is len(Keys). Both
	// are redundant with Keys and both are here so a divergence in the Rust
	// test names itself rather than being a pair of integer arrays.
	Shape  string `json:"shape"`
	Length int    `json:"length"`
	// Keys is the sort key of each element, in input order.
	Keys []int `json:"keys"`
	// Order is the original index of each element of the sorted result. For
	// an input with no ties this is forced; where keys tie, it is the only
	// thing this table exists to record.
	Order []int `json:"order"`
}

// Table is the file, with the toolchain that produced it.
type Table struct {
	// GoVersion is load-bearing, not provenance. sort.Slice is documented as
	// unstable, so this permutation is an implementation detail of the Go
	// that built `tk`; if a future Go changes it, `tk fmt`'s output moves and
	// this field is what says the table has to be regenerated and the port
	// revisited.
	GoVersion string `json:"goVersion"`
	Cases     []Case `json:"cases"`
}

// shapes are the input arrangements, chosen so that ties are present and
// placed differently in each.
//
// Every one of them has repeated keys except `sorted`, `reversed` and
// `shuffled`, because a permutation over distinct keys is the same for any
// correct sort and so grades nothing about tie order.
var shapes = []struct {
	name string
	key  func(at, length int) int
}{
	// Already in order, which is what partialInsertionSort short-circuits.
	{"sorted", func(at, _ int) int { return at }},
	// Fully reversed, which is what the decreasingHint reversal exists for.
	{"reversed", func(at, length int) int { return length - 1 - at }},
	// Every key the same: partitionEqual's branch, and the shape every
	// `ties_*_all_equal` snippet is.
	{"all_equal", func(_, _ int) int { return 0 }},
	// Ties adjacent and in order.
	{"adjacent_pairs", func(at, _ int) int { return at / 2 }},
	// Ties straddling the midpoint, so a partition that swaps across it
	// inverts them. This is `sort_imports/ties_*_pairs`.
	{"straddling_pairs", func(at, length int) int { return min(at, length-1-at) }},
	// Up then down, the classic median-of-three adversary.
	{"organ_pipe", func(at, length int) int {
		if at < length/2 {
			return at
		}
		return length - 1 - at
	}},
	// Many short runs, so most elements tie with something far away.
	{"sawtooth", func(at, _ int) int { return at % 5 }},
	// Two distinct keys, which is the most tied an input can be without
	// being all-equal.
	{"two_values", func(at, _ int) int { return at % 2 }},
	// Deterministically shuffled, as a sanity case: no ties, so any correct
	// sort agrees and a divergence here means the port is simply wrong.
	{"shuffled", func(at, length int) int { return (at*7 + 3) % max(length, 1) }},
	// In order but for one transposed pair every eight, which is what
	// partialInsertionSort's five-step budget is sized for.
	{"almost_sorted", func(at, _ int) int {
		if at%8 == 3 {
			return at + 1
		}
		if at%8 == 4 {
			return at - 1
		}
		return at
	}},
}

// lengths covers every size around the two thresholds that matter, then
// enough long cases to reach the recursion, breakPatterns and heapsort.
//
// 12 and 13 are the pair the whole port turns on: `maxInsertion` is 12 tested
// with `<=`, so 12 is the last insertion-sorted size and 13 the first that can
// reorder ties. 50 is `shortestNinther`, where choosePivot switches to a
// median of medians, and also `shortestShifting` in partialInsertionSort.
func lengths() []int {
	var out []int
	for n := 0; n <= 30; n++ {
		out = append(out, n)
	}
	return append(out, 40, 49, 50, 51, 63, 64, 100, 128, 200, 500)
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: go run ./sortdump <out>")
		os.Exit(2)
	}

	table := Table{GoVersion: runtime.Version()}

	for _, shape := range shapes {
		for _, length := range lengths() {
			keys := make([]int, length)
			for at := range keys {
				keys[at] = shape.key(at, length)
			}

			// Sort (key, origin) pairs by key alone, exactly as sortGroup
			// sorts importElems by path alone, so the comparison sequence —
			// and therefore the permutation — is the one rtk has to match.
			type element struct{ key, origin int }
			elements := make([]element, length)
			for at := range elements {
				elements[at] = element{key: keys[at], origin: at}
			}
			sort.Slice(elements, func(i, j int) bool {
				return elements[i].key < elements[j].key
			})

			order := make([]int, length)
			for at, e := range elements {
				order[at] = e.origin
			}

			table.Cases = append(table.Cases, Case{
				Shape:  shape.name,
				Length: length,
				Keys:   keys,
				Order:  order,
			})
		}
	}

	encoded, err := json.MarshalIndent(table, "", "  ")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	encoded = append(encoded, '\n')
	if err := os.WriteFile(os.Args[1], encoded, 0o644); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}

	// How many cases actually have a tie to get wrong, which is the number
	// that says whether the table is worth having.
	tied := 0
	for _, c := range table.Cases {
		seen := make(map[int]bool, len(c.Keys))
		for _, k := range c.Keys {
			if seen[k] {
				tied++
				break
			}
			seen[k] = true
		}
	}
	fmt.Fprintf(os.Stderr, "wrote %d cases (%d with tied keys) from %s\n",
		len(table.Cases), tied, table.GoVersion)
}
