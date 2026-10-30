//! The compiled matcher shapes used by gobwas/glob v0.2.3.
//!
//! Compilation is observable: Row slices at the first byte of its last rune,
//! and BTree mixes rune lengths with byte offsets. A conventional character
//! NFA therefore selects different Unicode filenames. Keep those shapes and
//! their byte-oriented Match/Index operations, including invalid UTF-8 input.

use crate::ast::{Kind, Tree};

#[derive(Debug, Clone)]
pub struct Program {
	matcher: Matcher,
}

impl Program {
	pub fn compile(tree: &Tree, separators: &[char]) -> Self {
		Self {
			matcher: Matcher::compile(tree, Tree::ROOT, separators),
		}
	}

	pub fn is_match(&self, subject: &[u8]) -> bool {
		self.matcher.is_match(subject)
	}
}

#[derive(Debug, Clone)]
enum Matcher {
	Nothing,
	Text(Vec<u8>),
	Any(Vec<char>),
	Super,
	Single(Vec<char>),
	List(Vec<char>, bool),
	Range(char, char, bool),
	AnyOf(Vec<Self>),
	EveryOf(Vec<Self>),
	Row(usize, Vec<Self>),
	BTree {
		value: Box<Self>,
		left: Option<Box<Self>>,
		right: Option<Box<Self>>,
	},
	Min(usize),
	Max(usize),
	Contains(Vec<u8>, bool),
	Prefix(Vec<u8>),
	Suffix(Vec<u8>),
	PrefixAny(Vec<u8>, Vec<char>),
	SuffixAny(Vec<u8>, Vec<char>),
	PrefixSuffix(Vec<u8>, Vec<u8>),
}

impl Matcher {
	fn compile(tree: &Tree, index: usize, separators: &[char]) -> Self {
		let node = &tree.nodes[index];
		let matcher = match &node.kind {
			Kind::Pattern => Self::sequence(
				node.children
					.iter()
					.map(|child| Self::compile(tree, *child, separators))
					.collect(),
			),
			Kind::AnyOf => Self::alternatives(tree, &node.children, separators),
			Kind::Text(text) => Self::Text(text.as_bytes().to_vec()),
			Kind::Any => Self::Any(separators.to_vec()),
			Kind::Super => Self::Super,
			Kind::Single => Self::Single(separators.to_vec()),
			Kind::List { chars, not } => Self::List(chars.chars().collect(), *not),
			Kind::Range { lo, hi, not } => Self::Range(*lo, *hi, *not),
		};
		matcher.optimize()
	}

	fn alternatives(tree: &Tree, children: &[usize], separators: &[char]) -> Self {
		// The upstream alternation minimizer factors common AST children before
		// compiling them. Factoring after compilation changes Row boundaries.
		let shortest = children
			.iter()
			.min_by_key(|child| tree.nodes[**child].children.len());
		let Some(shortest) = shortest.filter(|_| children.len() > 1) else {
			return Self::AnyOf(
				children
					.iter()
					.map(|child| Self::compile(tree, *child, separators))
					.collect(),
			);
		};
		let reference = &tree.nodes[*shortest].children;
		let (mut left, mut right) = (0, 0);
		let (mut stop_left, mut stop_right) = (false, false);
		for i in 0..reference.len() {
			if left + right >= reference.len() || (stop_left && stop_right) {
				break;
			}
			let j = reference.len() - 1 - i;
			for child in children.iter().filter(|child| *child != shortest) {
				let rest = &tree.nodes[*child].children;
				stop_left |= !tree.equivalent(reference[i], rest[i]);
				stop_right |= (!stop_left && j <= i)
					|| !tree.equivalent(reference[j], rest[rest.len() - 1 - i]);
			}
			if !stop_left {
				left += 1;
			}
			if !stop_right {
				right += 1;
			}
		}
		if left + right == 0 {
			return Self::AnyOf(
				children
					.iter()
					.map(|child| Self::compile(tree, *child, separators))
					.collect(),
			);
		}
		let mut parts = Vec::new();
		if left > 0 {
			parts.push(
				Self::sequence(
					reference[..left]
						.iter()
						.map(|child| Self::compile(tree, *child, separators))
						.collect(),
				)
				.optimize(),
			);
		}
		let mut unique: Vec<&[usize]> = Vec::new();
		for child in children {
			let nodes = &tree.nodes[*child].children;
			let middle = &nodes[left..nodes.len() - right];
			if !unique.iter().any(|other| {
				other.len() == middle.len()
					&& other
						.iter()
						.zip(middle)
						.all(|(a, b)| tree.equivalent(*a, *b))
			}) {
				unique.push(middle);
			}
		}
		if unique.len() > 1 || unique.first().is_some_and(|nodes| !nodes.is_empty()) {
			parts.push(
				Self::AnyOf(
					unique
						.iter()
						.map(|nodes| {
							Self::sequence(
								nodes
									.iter()
									.map(|child| Self::compile(tree, *child, separators))
									.collect(),
							)
							.optimize()
						})
						.collect(),
				)
				.optimize(),
			);
		}
		if right > 0 {
			parts.push(
				Self::sequence(
					reference[reference.len() - right..]
						.iter()
						.map(|child| Self::compile(tree, *child, separators))
						.collect(),
				)
				.optimize(),
			);
		}
		Self::sequence(parts)
	}

	fn sequence(mut parts: Vec<Self>) -> Self {
		if parts.is_empty() {
			return Self::Nothing;
		}
		// minimizeMatchers chooses the longest gluable span, preferring a
		// longer fixed rune length when both candidates have one.
		loop {
			let mut best: Option<(usize, usize, Self)> = None;
			for left in 0..parts.len() {
				for right in (left + 2..=parts.len()).rev() {
					if let Some(glued) = Self::glue(&parts[left..right]) {
						let replace = best.as_ref().is_none_or(|(l, r, old)| {
							matches!((old.len(), glued.len()), (Some(a), Some(b)) if b > a)
								|| right - left > r - l
						});
						if replace {
							best = Some((left, right, glued));
						}
					}
				}
			}
			let Some((left, right, glued)) = best else {
				break;
			};
			parts.splice(left..right, [glued]);
		}
		Self::compile_matchers(parts)
	}

	fn compile_matchers(mut parts: Vec<Self>) -> Self {
		if parts.len() == 1 {
			return parts.pop().expect("one matcher");
		}
		if let Some(glued) = Self::glue(&parts) {
			return glued;
		}
		let index = parts
			.iter()
			.enumerate()
			.filter_map(|(i, m)| m.len().map(|len| (i, len)))
			.max_by_key(|(_, len)| *len)
			.map_or(0, |(i, _)| i);
		let right = parts.split_off(index + 1);
		let value = Box::new(parts.pop().expect("nonempty matchers"));
		Self::BTree {
			value,
			left: (!parts.is_empty()).then(|| Box::new(Self::compile_matchers(parts))),
			right: (!right.is_empty()).then(|| Box::new(Self::compile_matchers(right))),
		}
	}

	fn glue(parts: &[Self]) -> Option<Self> {
		if parts.len() <= 1 {
			return None;
		}
		if let Some(every) = Self::glue_every(parts) {
			return Some(every);
		}
		let len: Option<usize> = parts.iter().map(Self::len).sum();
		len.map(|len| Self::Row(len, parts.to_vec()))
	}

	fn glue_every(parts: &[Self]) -> Option<Self> {
		let (mut any, mut super_, mut min) = (false, false, 0);
		let mut separators: Option<&[char]> = None;
		for part in parts {
			let sep: &[char] = match part {
				Self::Super => {
					super_ = true;
					&[]
				}
				Self::Any(sep) => {
					any = true;
					sep
				}
				Self::Single(sep) | Self::List(sep, true) => {
					min += 1;
					sep
				}
				_ => return None,
			};
			if separators.is_some_and(|previous| previous != sep) {
				return None;
			}
			separators = Some(sep);
		}
		let separators = separators.unwrap_or_default();
		if super_ && !any && min == 0 {
			return Some(Self::Super);
		}
		if any && !super_ && min == 0 {
			return Some(Self::Any(separators.to_vec()));
		}
		if (any || super_) && min > 0 && separators.is_empty() {
			return Some(Self::Min(min));
		}
		let mut every = Vec::new();
		if min > 0 {
			every.push(Self::Min(min));
			if !any && !super_ {
				every.push(Self::Max(min));
			}
		}
		if !separators.is_empty() {
			every.push(Self::Contains(
				separators.iter().collect::<String>().into_bytes(),
				true,
			));
		}
		Some(Self::EveryOf(every))
	}

	fn optimize(self) -> Self {
		match self {
			Self::Any(sep) if sep.is_empty() => Self::Super,
			Self::AnyOf(mut parts) if parts.len() == 1 => parts.pop().expect("one alternative"),
			Self::List(chars, false) if chars.len() == 1 => {
				Self::Text(chars.iter().collect::<String>().into_bytes())
			}
			Self::BTree { value, left, right } => {
				let left = left.map(|m| Box::new(m.optimize()));
				let right = right.map(|m| Box::new(m.optimize()));
				if let Self::Text(text) = value.as_ref() {
					match (left.as_deref(), right.as_deref()) {
						(None, None) => return Self::Text(text.clone()),
						(Some(Self::Super), Some(Self::Super)) => {
							return Self::Contains(text.clone(), false);
						}
						(Some(Self::Super), None) => return Self::Suffix(text.clone()),
						(None, Some(Self::Super)) => return Self::Prefix(text.clone()),
						(None, Some(Self::Suffix(suffix))) => {
							return Self::PrefixSuffix(text.clone(), suffix.clone());
						}
						(Some(Self::Prefix(prefix)), None) => {
							return Self::PrefixSuffix(prefix.clone(), text.clone());
						}
						(Some(Self::Any(sep)), None) => {
							return Self::SuffixAny(text.clone(), sep.clone());
						}
						(None, Some(Self::Any(sep))) => {
							return Self::PrefixAny(text.clone(), sep.clone());
						}
						_ => {}
					}
				}
				Self::BTree { value, left, right }
			}
			other => other,
		}
	}

	fn len(&self) -> Option<usize> {
		match self {
			Self::Nothing => Some(0),
			Self::Text(text) => Some(Runes::new(text).count()),
			Self::Single(_) | Self::List(..) | Self::Range(..) => Some(1),
			Self::Row(len, _) => Some(*len),
			Self::BTree { value, left, right } => Some(
				value.len()?
					+ left.as_ref().map_or(Some(0), |m| m.len())?
					+ right.as_ref().map_or(Some(0), |m| m.len())?,
			),
			Self::AnyOf(parts) => {
				let mut length = None;
				for part in parts {
					// AnyOf.Len resets its sentinel after a variable-length
					// first alternative, as the Go implementation does.
					if length.is_none() {
						length = part.len();
					} else if part.len() != length {
						return None;
					}
				}
				length
			}
			Self::EveryOf(parts) if parts.is_empty() => Some(0),
			_ => None,
		}
	}

	fn accepts_rune(&self, rune: char) -> bool {
		match self {
			Self::Single(sep) => !sep.contains(&rune),
			Self::List(chars, not) => chars.contains(&rune) != *not,
			Self::Range(lo, hi, not) => (*lo..=*hi).contains(&rune) != *not,
			_ => false,
		}
	}

	fn is_match(&self, s: &[u8]) -> bool {
		match self {
			Self::Nothing => s.is_empty(),
			Self::Text(text) => s == text,
			Self::Super => true,
			Self::Any(sep) => !Runes::new(s).any(|(_, r, _)| sep.contains(&r)),
			Self::Single(_) | Self::List(..) | Self::Range(..) => {
				let (rune, size) = decode_rune(s);
				s.len() <= size && self.accepts_rune(rune)
			}
			Self::AnyOf(parts) => parts.iter().any(|m| m.is_match(s)),
			Self::EveryOf(parts) => parts.iter().all(|m| m.is_match(s)),
			Self::Row(len, parts) => Runes::new(s).count() == *len && Self::row_match(parts, s),
			Self::BTree { value, left, right } => {
				self.tree_match(value, left.as_deref(), right.as_deref(), s)
			}
			Self::Min(min) => Runes::new(s).count() >= *min,
			Self::Max(max) => Runes::new(s).count() <= *max,
			Self::Contains(needle, not) => find(s, needle).is_some() != *not,
			Self::Prefix(prefix) => s.starts_with(prefix),
			Self::Suffix(suffix) => s.ends_with(suffix),
			Self::PrefixAny(prefix, sep) => s
				.strip_prefix(prefix.as_slice())
				.is_some_and(|tail| !Runes::new(tail).any(|(_, r, _)| sep.contains(&r))),
			Self::SuffixAny(suffix, sep) => s
				.strip_suffix(suffix.as_slice())
				.is_some_and(|head| !Runes::new(head).any(|(_, r, _)| sep.contains(&r))),
			Self::PrefixSuffix(prefix, suffix) => s.starts_with(prefix) && s.ends_with(suffix),
		}
	}

	fn row_match(parts: &[Self], s: &[u8]) -> bool {
		let mut offset = 0;
		for part in parts {
			let length = part.len().expect("Row contains fixed-length matchers");
			let Some(tail) = s.get(offset..) else {
				return false;
			};
			let mut count = 0;
			let mut next = 0;
			for (index, _, _) in Runes::new(tail) {
				next = index;
				count += 1;
				if count == length {
					break;
				}
			}
			// Deliberately +1 byte, not the width of the final rune. This is
			// gobwas Row.matchAll, including its handling of zero-length parts.
			let Some(slice) = tail.get(..next + 1) else {
				return false;
			};
			if count < length || !part.is_match(slice) {
				return false;
			}
			offset += next + 1;
		}
		true
	}

	fn tree_match(
		&self,
		value: &Self,
		left: Option<&Self>,
		right: Option<&Self>,
		s: &[u8],
	) -> bool {
		if self.len().is_some_and(|len| len > s.len()) {
			return false;
		}
		let mut offset = left.and_then(Self::len).unwrap_or(0);
		let right_len = right.and_then(Self::len).unwrap_or(0);
		let Some(limit) = s.len().checked_sub(right_len) else {
			return false;
		};
		while offset < limit {
			let Some((index, segments)) = value.index(&s[offset..limit]) else {
				return false;
			};
			let at = offset + index;
			if left.map_or(at == 0, |m| m.is_match(&s[..at])) {
				for length in segments.iter().rev() {
					let tail = s.get(at + length..).unwrap_or_default();
					if right.map_or(tail.is_empty(), |m| m.is_match(tail)) {
						return true;
					}
				}
			}
			let (_, step) = decode_rune(&s[at..]);
			if step == 0 {
				break;
			}
			offset = at + step;
		}
		false
	}

	/// Go's Index returns the earliest byte offset and candidate byte lengths.
	fn index(&self, s: &[u8]) -> Option<(usize, Vec<usize>)> {
		match self {
			Self::Nothing => Some((0, vec![0])),
			Self::Text(text) => find(s, text).map(|i| (i, vec![text.len()])),
			Self::Single(_) | Self::List(..) | Self::Range(..) => Runes::new(s)
				.find(|(_, r, _)| self.accepts_rune(*r))
				.map(|(i, r, _)| (i, vec![r.len_utf8()])),
			Self::Super => Some((0, boundaries(s))),
			Self::Any(sep) => {
				let end = Runes::new(s)
					.find(|(_, r, _)| sep.contains(r))
					.map_or(s.len(), |(i, _, _)| i);
				Some((0, boundaries(&s[..end])))
			}
			Self::Row(len, parts) => Runes::new(s)
				.find(|(i, _, _)| s.len() - i >= *len && Self::row_match(parts, &s[*i..]))
				.map(|(i, _, _)| (i, vec![*len])),
			Self::BTree { .. } => None,
			Self::AnyOf(parts) => {
				let mut result: Option<(usize, Vec<usize>)> = None;
				for part in parts {
					if let Some((i, segments)) = part.index(s) {
						match &mut result {
							Some((best, lengths)) if i == *best => {
								lengths.extend(segments);
								lengths.sort_unstable();
								lengths.dedup();
							}
							Some((best, _)) if i > *best => {}
							_ => result = Some((i, segments)),
						}
					}
				}
				result
			}
			Self::EveryOf(parts) => {
				let (mut index, mut offset) = (0, 0);
				let mut current = Vec::new();
				for (i, part) in parts.iter().enumerate() {
					let (idx, segments) = part.index(&s[index..])?;
					if i == 0 {
						current = segments;
					} else {
						current = segments
							.into_iter()
							.filter(|n| current.iter().any(|ex| ex + index == n + idx + offset))
							.collect();
						if current.is_empty() {
							return None;
						}
					}
					index = idx + offset;
					offset += idx;
				}
				Some((index, current))
			}
			Self::Min(min) => {
				let segments: Vec<_> = Runes::new(s)
					.enumerate()
					.filter(|(n, _)| n + 1 >= *min)
					.map(|(_, (i, r, _))| i + r.len_utf8())
					.collect();
				(!segments.is_empty()).then_some((0, segments))
			}
			Self::Max(max) => Some((
				0,
				std::iter::once(0)
					.chain(Runes::new(s).take(*max).map(|(i, r, _)| i + r.len_utf8()))
					.collect(),
			)),
			Self::Contains(needle, not) => {
				let found = find(s, needle);
				if *not {
					Some((0, boundaries(&s[..found.unwrap_or(s.len())])))
				} else {
					let offset = found? + needle.len();
					Some((
						0,
						boundaries(&s[offset..])
							.into_iter()
							.map(|i| i + offset)
							.collect(),
					))
				}
			}
			Self::Prefix(prefix) | Self::PrefixAny(prefix, _) => {
				let i = find(s, prefix)?;
				let tail = &s[i + prefix.len()..];
				let end = if let Self::PrefixAny(_, sep) = self {
					Runes::new(tail)
						.find(|(_, r, _)| sep.contains(r))
						.map_or(tail.len(), |(i, _, _)| i)
				} else {
					tail.len()
				};
				Some((
					i,
					std::iter::once(prefix.len())
						.chain(
							Runes::new(&tail[..end])
								.map(|(i, r, _)| prefix.len() + i + r.len_utf8()),
						)
						.collect(),
				))
			}
			Self::Suffix(suffix) | Self::SuffixAny(suffix, _) => {
				let i = find(s, suffix)?;
				let start = if let Self::SuffixAny(_, sep) = self {
					Runes::new(&s[..i])
						.filter(|(_, r, _)| sep.contains(r))
						.last()
						.map_or(0, |(i, _, _)| i + 1)
				} else {
					0
				};
				Some((start, vec![i + suffix.len() - start]))
			}
			Self::PrefixSuffix(prefix, suffix) => {
				let i = find(s, prefix)?;
				if suffix.is_empty() {
					return Some((i, vec![s.len() - i]));
				}
				let mut tail = &s[i..];
				let mut segments = Vec::new();
				while let Some(end) = tail.windows(suffix.len()).rposition(|w| w == suffix) {
					segments.push(end + suffix.len());
					tail = &tail[..end];
				}
				segments.reverse();
				(!segments.is_empty()).then_some((i, segments))
			}
		}
	}
}

/// Decode as Go does: each invalid UTF-8 byte is one RuneError of width one.
fn decode_rune(bytes: &[u8]) -> (char, usize) {
	let Some(first) = bytes.first() else {
		return (char::REPLACEMENT_CHARACTER, 0);
	};
	let width = match first {
		0..=0x7f => 1,
		0xc2..=0xdf => 2,
		0xe0..=0xef => 3,
		0xf0..=0xf4 => 4,
		_ => 0,
	};
	if width > 0
		&& let Some(text) = bytes.get(..width).and_then(|s| std::str::from_utf8(s).ok())
	{
		return (text.chars().next().expect("nonempty rune"), width);
	}
	(char::REPLACEMENT_CHARACTER, 1)
}

struct Runes<'a> {
	bytes: &'a [u8],
	offset: usize,
}
impl<'a> Runes<'a> {
	fn new(bytes: &'a [u8]) -> Self {
		Self { bytes, offset: 0 }
	}
}
impl Iterator for Runes<'_> {
	type Item = (usize, char, usize);
	fn next(&mut self) -> Option<Self::Item> {
		if self.offset == self.bytes.len() {
			return None;
		}
		let (rune, width) = decode_rune(&self.bytes[self.offset..]);
		let offset = self.offset;
		self.offset += width;
		Some((offset, rune, width))
	}
}

fn boundaries(s: &[u8]) -> Vec<usize> {
	Runes::new(s)
		.map(|(i, _, _)| i)
		.chain(std::iter::once(s.len()))
		.collect()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
	if needle.is_empty() {
		Some(0)
	} else {
		haystack.windows(needle.len()).position(|w| w == needle)
	}
}
