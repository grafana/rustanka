//! Grades `rtk_jsonnetfmt::go_sort` against the permutation Go's own
//! `sort.Slice` produces.
//!
//! # Why this table exists
//!
//! `SortImports` sorts imports by path, two imports can share a path, and
//! "sorted" does not say which of those two comes first. `sort.Slice` is
//! documented as unstable, and the pass oracle measured what its instability
//! actually does: ties invert from n = 13 upwards. So `Vec::sort_by` is
//! measurably wrong and `sort_unstable_by` is wrong differently, and
//! `src/go_sort.rs` is a port rather than a call.
//!
//! The pass oracle grades that port end to end, on the 15 `sort_imports/ties_*`
//! snippets. This table grades it directly and far wider — ten input shapes at
//! forty lengths — and, more to the point, it reaches two branches no import
//! group can: `heapSort_func` needs `bits.Len(n)` consecutive bad partitions
//! and `breakPatterns_func` needs one.
//!
//! # Full parity, and skipped when absent
//!
//! No ratchet, like `node_parity` and `pass_parity`: a sort that agrees on most
//! inputs is not a partial sort, it is the wrong permutation. The table needs
//! Go to regenerate, so the test skips with a message when it is not
//! committed — the same shape as the gobwas glob truth table's test.

use std::{fs, path::PathBuf};

use rtk_jsonnetfmt::go_sort;
use serde::Deserialize;

const HOW: &str = "make update-go-sort-truth-table";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
	shape: String,
	length: usize,
	keys: Vec<i64>,
	order: Vec<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Table {
	go_version: String,
	cases: Vec<Case>,
}

fn table_path() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/go-sort-truth-table.json")
}

/// Sort `keys` through the port and report where each element came from.
///
/// The pairs are `(key, origin)` and the comparison is on the key alone,
/// exactly as `sortGroup` compares `importElem`s on their path alone — so this
/// exercises the same comparison sequence the real caller does.
fn permutation(keys: &[i64]) -> Vec<usize> {
	let mut elements: Vec<(i64, usize)> = keys.iter().copied().zip(0..).collect();
	go_sort::slice(&mut elements, |left, right| left.0 < right.0);
	elements.into_iter().map(|(_, origin)| origin).collect()
}

#[test]
fn the_permutation_matches_go_sort_slice_on_every_case() {
	let path = table_path();
	let Ok(raw) = fs::read_to_string(&path) else {
		eprintln!(
			"skipping: {} is not committed; run `{HOW}` (requires Go)",
			path.display()
		);
		return;
	};

	let table: Table = serde_json::from_str(&raw).expect("the truth table is valid JSON");
	assert!(!table.cases.is_empty(), "the truth table is empty");

	let mut divergences: Vec<String> = Vec::new();
	let mut tied = 0;

	for case in &table.cases {
		assert_eq!(
			case.keys.len(),
			case.length,
			"{}/{}: the table's own length disagrees with its keys",
			case.shape,
			case.length
		);

		let got = permutation(&case.keys);
		if got != case.order {
			// Report the first position that differs and the keys around it;
			// a bare pair of 500-element arrays is unreadable.
			let at = got
				.iter()
				.zip(&case.order)
				.position(|(ours, theirs)| ours != theirs)
				.unwrap_or_else(|| got.len().min(case.order.len()));
			divergences.push(format!(
				"{}/{}: first differs at position {at}\n    \
				 go:  {:?}\n    rtk: {:?}",
				case.shape,
				case.length,
				&case.order[at..(at + 6).min(case.order.len())],
				&got[at..(at + 6).min(got.len())],
			));
		}

		let mut keys = case.keys.clone();
		keys.sort_unstable();
		if keys.windows(2).any(|pair| pair[0] == pair[1]) {
			tied += 1;
		}
	}

	assert!(
		divergences.is_empty(),
		"{} of {} cases diverge from Go's sort.Slice (table from {}):\n\n{}\n\n\
		 If Go itself changed, regenerate with `{HOW}` and revisit \
		 src/go_sort.rs — the port is pinned to an implementation detail and \
		 says so.",
		divergences.len(),
		table.cases.len(),
		table.go_version,
		divergences.join("\n\n")
	);

	eprintln!(
		"go_sort parity: {} cases matched, {tied} of them with tied keys (table from {})",
		table.cases.len(),
		table.go_version
	);
}

#[test]
fn the_table_reaches_the_sizes_the_port_turns_on() {
	// The table is only worth what it covers, so what it covers is asserted
	// rather than trusted. Twelve and thirteen are the boundary
	// (`maxInsertion` is 12 tested with `<=`); fifty is `shortestNinther` and
	// `shortestShifting`; the long cases are what reach heapsort and
	// `breakPatterns` at all.
	let Ok(raw) = fs::read_to_string(table_path()) else {
		eprintln!("skipping: the truth table is not committed; run `{HOW}`");
		return;
	};
	let table: Table = serde_json::from_str(&raw).expect("the truth table is valid JSON");

	for length in [0usize, 1, 12, 13, 49, 50, 51, 500] {
		let tied_at_this_length = table.cases.iter().filter(|case| {
			case.length == length && {
				let mut keys = case.keys.clone();
				keys.sort_unstable();
				keys.windows(2).any(|pair| pair[0] == pair[1])
			}
		});
		let count = tied_at_this_length.count();
		if length < 2 {
			// Nothing can tie in zero or one element; presence is the claim.
			assert!(
				table.cases.iter().any(|case| case.length == length),
				"the table has no case of length {length}"
			);
		} else {
			assert!(
				count > 0,
				"the table has no *tied* case of length {length}, so it cannot \
				 grade tie order there"
			);
		}
	}
}
