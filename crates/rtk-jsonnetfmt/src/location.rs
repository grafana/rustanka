//! A port of the parts of go-jsonnet's `ast/location.go` the formatter needs.
//!
//! Only enough to reproduce error messages. `staticError.Error()` is
//! `"{loc} {msg}"`, and `loc` is `LocationRange::to_string`, so every character
//! of this file is part of what `tk fmt` prints when it refuses a file.

use std::fmt;

/// `ast.Location`. The column is a **byte** offset from the start of the line,
/// one-based — not a character offset, so a multi-byte character before the
/// error moves the column by its byte length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Location {
	pub line: usize,
	pub column: usize,
}

impl Location {
	/// `Location.IsSet`: line 0 means unset.
	pub fn is_set(self) -> bool {
		self.line != 0
	}
}

impl fmt::Display for Location {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}:{}", self.line, self.column)
	}
}

/// `ast.LocationRange`, carrying the diagnostic filename it will be printed
/// with.
///
/// Go keeps the filename on the `Source` the range points at; there is no
/// source here, so it is held directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationRange {
	/// go-jsonnet's `DiagnosticFileName` — the name `Format` was called with.
	pub diagnostic_filename: String,
	pub begin: Location,
	pub end: Location,
}

impl LocationRange {
	/// A range covering a single point, as `makeStaticErrorPoint` builds.
	pub fn point(diagnostic_filename: &str, location: Location) -> Self {
		Self {
			diagnostic_filename: diagnostic_filename.to_owned(),
			begin: location,
			end: location,
		}
	}

	pub fn new(diagnostic_filename: &str, begin: Location, end: Location) -> Self {
		Self {
			diagnostic_filename: diagnostic_filename.to_owned(),
			begin,
			end,
		}
	}

	/// `LocationRange.IsSet`, which asks only about `begin`.
	pub fn is_set(&self) -> bool {
		self.begin.is_set()
	}

	/// `ast.LocationRangeBetween`: from the start of `self` to the end of
	/// `other`.
	pub fn between(&self, other: &Self) -> Self {
		Self {
			diagnostic_filename: self.diagnostic_filename.clone(),
			begin: self.begin,
			end: other.end,
		}
	}
}

impl fmt::Display for LocationRange {
	/// `LocationRange.String()`, which has three shapes and picks by how much
	/// of the range collapses.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		if !self.is_set() {
			// Go returns the bare filename for an unset range.
			return write!(f, "{}", self.diagnostic_filename);
		}

		let prefix = if self.diagnostic_filename.is_empty() {
			String::new()
		} else {
			format!("{}:", self.diagnostic_filename)
		};

		if self.begin.line == self.end.line {
			if self.begin.column == self.end.column {
				return write!(f, "{prefix}{}", self.begin);
			}
			return write!(f, "{prefix}{}-{}", self.begin, self.end.column);
		}

		write!(f, "{prefix}({})-({})", self.begin, self.end)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn at(line: usize, column: usize) -> Location {
		Location { line, column }
	}

	#[test]
	fn a_point_range_prints_one_position() {
		// The shape every lexer error takes.
		let range = LocationRange::point("f.jsonnet", at(1, 6));
		assert_eq!(range.to_string(), "f.jsonnet:1:6");
	}

	#[test]
	fn a_range_within_one_line_prints_only_the_end_column() {
		let range = LocationRange::new("f.jsonnet", at(2, 3), at(2, 9));
		assert_eq!(range.to_string(), "f.jsonnet:2:3-9");
	}

	#[test]
	fn a_multi_line_range_parenthesises_both_ends() {
		let range = LocationRange::new("f.jsonnet", at(2, 3), at(4, 1));
		assert_eq!(range.to_string(), "f.jsonnet:(2:3)-(4:1)");
	}

	#[test]
	fn an_empty_filename_drops_the_prefix_and_its_colon() {
		let range = LocationRange::point("", at(1, 1));
		assert_eq!(range.to_string(), "1:1");
	}

	#[test]
	fn an_unset_range_prints_the_filename_alone() {
		let range = LocationRange::point("f.jsonnet", at(0, 0));
		assert_eq!(range.to_string(), "f.jsonnet");
	}

	#[test]
	fn between_takes_the_start_of_one_and_the_end_of_the_other() {
		let first = LocationRange::new("f.jsonnet", at(1, 1), at(1, 2));
		let second = LocationRange::new("f.jsonnet", at(3, 4), at(3, 8));
		assert_eq!(first.between(&second).to_string(), "f.jsonnet:(1:1)-(3:8)");
	}
}
