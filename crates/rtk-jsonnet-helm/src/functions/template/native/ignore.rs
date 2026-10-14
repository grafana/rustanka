//! Helm's ordered ignore rules use Go filepath patterns rather than gitignore patterns.

use std::fs;
use std::iter::Peekable;
use std::path::Path;
use std::str::Chars;

use anyhow::{Context as _, Result, bail, ensure};
use walkdir::{DirEntry, WalkDir};

pub(super) struct Rules(Vec<Rule>);

impl Rules {
	pub(super) fn load(root: &Path) -> Result<Self> {
		let path = root.join(".helmignore");
		let source = match fs::read_to_string(&path) {
			Ok(source) => source,
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
			Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
		};
		Self::parse(&source).with_context(|| format!("parsing {}", path.display()))
	}

	fn parse(source: &str) -> Result<Self> {
		let mut rules = Vec::new();
		for line in source.strip_prefix('\u{feff}').unwrap_or(source).lines() {
			ensure!(line.len() < 65_536, ".helmignore line is too long");
			let line = line.trim();
			if !line.is_empty() && !line.starts_with('#') {
				rules.push(Rule::parse(line)?);
			}
		}
		rules.push(Rule::parse("templates/.?*")?);
		Ok(Self(rules))
	}

	pub(super) fn ignores(&self, name: &str, is_directory: bool) -> bool {
		if matches!(name, "" | "." | "./") {
			return false;
		}
		self.0.iter().any(|rule| {
			let matches = rule.matches(name);
			if rule.negated {
				// Helm rejects nonmatches here; a later rule may still reject a match.
				(rule.directory_only && !is_directory) || !matches
			} else {
				(!rule.directory_only || is_directory) && matches
			}
		})
	}

	pub(super) fn walk<'a>(
		&'a self,
		root: &'a Path,
		directory: &str,
	) -> impl Iterator<Item = walkdir::Result<DirEntry>> + 'a {
		WalkDir::new(root.join(directory))
			.sort_by_file_name()
			.into_iter()
			.filter_entry(move |entry| {
				entry
					.path()
					.strip_prefix(root)
					.ok()
					.and_then(Path::to_str)
					.is_none_or(|name| {
						!self.ignores(
							&name.replace(std::path::MAIN_SEPARATOR, "/"),
							entry.file_type().is_dir(),
						)
					})
			})
	}
}

struct Rule {
	pattern: Option<Glob>,
	basename_only: bool,
	directory_only: bool,
	negated: bool,
}

impl Rule {
	fn parse(raw: &str) -> Result<Self> {
		ensure!(
			!raw.contains("**"),
			"double-star (**) syntax is not supported"
		);
		Glob::parse(raw)?;
		let negated = raw.starts_with('!');
		let pattern = raw.strip_prefix('!').unwrap_or(raw);
		let directory_only = pattern.ends_with('/');
		let pattern = pattern.strip_suffix('/').unwrap_or(pattern);
		let basename_only = !pattern.contains('/');
		let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
		Ok(Self {
			// Helm validates before stripping rule markers, then treats an invalid matcher as a miss.
			pattern: Glob::parse(pattern).ok(),
			basename_only,
			directory_only,
			negated,
		})
	}

	fn matches(&self, name: &str) -> bool {
		let name = if self.basename_only {
			name.trim_end_matches('/').rsplit('/').next().unwrap()
		} else {
			name
		};
		self.pattern
			.as_ref()
			.is_some_and(|pattern| pattern.matches(name))
	}
}

struct Glob(Vec<Token>);

impl Glob {
	fn parse(pattern: &str) -> Result<Self> {
		let mut chars = pattern.chars().peekable();
		let mut tokens = Vec::new();
		while let Some(character) = chars.next() {
			tokens.push(match character {
				'*' => Token::Star,
				'?' => Token::Any,
				'[' => Token::class(&mut chars)?,
				'\\' => Token::Literal(chars.next().context("syntax error in pattern")?),
				character => Token::Literal(character),
			});
		}
		Ok(Self(tokens))
	}

	fn matches(&self, name: &str) -> bool {
		let chars: Vec<_> = name.chars().collect();
		let mut reachable = vec![false; chars.len() + 1];
		reachable[0] = true;
		for token in &self.0 {
			if matches!(token, Token::Star) {
				for index in 1..=chars.len() {
					reachable[index] |= reachable[index - 1] && chars[index - 1] != '/';
				}
			} else {
				for index in (1..=chars.len()).rev() {
					reachable[index] = reachable[index - 1] && token.matches(chars[index - 1]);
				}
				reachable[0] = false;
			}
		}
		reachable[chars.len()]
	}
}

enum Token {
	Star,
	Any,
	Literal(char),
	Class {
		negated: bool,
		ranges: Vec<(char, char)>,
	},
}

impl Token {
	fn class(chars: &mut Peekable<Chars<'_>>) -> Result<Self> {
		let negated = chars.next_if_eq(&'^').is_some();
		let mut ranges = Vec::new();
		loop {
			if !ranges.is_empty() && chars.next_if_eq(&']').is_some() {
				return Ok(Self::Class { negated, ranges });
			}
			let start = Self::class_character(chars)?;
			let end = if chars.next_if_eq(&'-').is_some() {
				Self::class_character(chars)?
			} else {
				start
			};
			ranges.push((start, end));
		}
	}

	fn class_character(chars: &mut Peekable<Chars<'_>>) -> Result<char> {
		match chars.next().context("syntax error in pattern")? {
			'\\' => chars.next().context("syntax error in pattern"),
			'-' | ']' => bail!("syntax error in pattern"),
			character => Ok(character),
		}
	}

	fn matches(&self, character: char) -> bool {
		match self {
			Self::Star => unreachable!(),
			Self::Any => character != '/',
			Self::Literal(expected) => *expected == character,
			Self::Class { negated, ranges } => {
				ranges
					.iter()
					.any(|&(start, end)| start <= character && character <= end)
					!= *negated
			}
		}
	}
}

#[cfg(test)]
mod tests;
