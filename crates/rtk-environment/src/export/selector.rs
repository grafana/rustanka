//! Parsing `-l/--selector` into a [`Selector`].
//!
//! Matching itself is [`kube_core`]'s, so the semantics are Kubernetes' own;
//! only the string syntax is parsed here, because `kube-core` offers no
//! `FromStr` for [`Selector`].

use std::collections::BTreeSet;

use kube_core::{Expression, Selector};

use crate::export::Error;

/// Parse a label selector in `kubectl` syntax.
///
/// Supports `key=value`, `key==value`, `key!=value`, `key`, `!key`,
/// `key in (a, b)` and `key notin (a, b)`, comma-separated. Mirrors what tk
/// accepts through Kubernetes' `labels.Parse`.
pub(crate) fn parse(input: &str) -> Result<Selector, Error> {
	let mut expressions = Vec::new();

	for requirement in split_requirements(input)? {
		let requirement = requirement.trim();
		if requirement.is_empty() {
			continue;
		}
		expressions.push(parse_requirement(requirement)?);
	}

	let mut selector = Selector::default();
	selector.extend(expressions);
	Ok(selector)
}

/// Split on commas that are not inside a `(…)` set.
fn split_requirements(input: &str) -> Result<Vec<&str>, Error> {
	let mut requirements = Vec::new();
	let mut depth = 0usize;
	let mut start = 0usize;

	for (index, character) in input.char_indices() {
		match character {
			'(' => depth += 1,
			')' => {
				depth = depth.checked_sub(1).ok_or_else(|| Error::InvalidSelector {
					selector: input.into(),
					reason: "unbalanced `)`".into(),
				})?;
			}
			',' if depth == 0 => {
				requirements.push(&input[start..index]);
				start = index + character.len_utf8();
			}
			_ => {}
		}
	}

	if depth != 0 {
		return Err(Error::InvalidSelector {
			selector: input.into(),
			reason: "unbalanced `(`".into(),
		});
	}

	requirements.push(&input[start..]);
	Ok(requirements)
}

fn parse_requirement(requirement: &str) -> Result<Expression, Error> {
	if let Some((key, values)) = split_set_operator(requirement, "notin") {
		return Ok(Expression::NotIn(key, parse_set(requirement, values)?));
	}
	if let Some((key, values)) = split_set_operator(requirement, "in") {
		return Ok(Expression::In(key, parse_set(requirement, values)?));
	}
	if let Some((key, value)) = requirement.split_once("!=") {
		return Ok(Expression::NotEqual(
			key.trim().to_owned(),
			value.trim().to_owned(),
		));
	}
	if let Some((key, value)) = requirement.split_once("==") {
		return Ok(Expression::Equal(
			key.trim().to_owned(),
			value.trim().to_owned(),
		));
	}
	if let Some((key, value)) = requirement.split_once('=') {
		return Ok(Expression::Equal(
			key.trim().to_owned(),
			value.trim().to_owned(),
		));
	}
	if let Some(key) = requirement.strip_prefix('!') {
		return Ok(Expression::DoesNotExist(key.trim().to_owned()));
	}

	// A bare key is an existence check, but only if it really is just a key:
	// anything else is a typo we should not silently reinterpret.
	if requirement.contains(char::is_whitespace) {
		return Err(Error::InvalidSelector {
			selector: requirement.into(),
			reason: "expected `key`, `!key`, `key=value`, `key!=value`, \
			         `key in (…)` or `key notin (…)`"
				.into(),
		});
	}

	Ok(Expression::Exists(requirement.to_owned()))
}

/// Split `key <operator> (values)`, requiring the operator to be a whole word.
fn split_set_operator<'r>(requirement: &'r str, operator: &str) -> Option<(String, &'r str)> {
	let (key, rest) = requirement.split_once(operator)?;

	// `key` must end in whitespace and the values must follow, so that keys like
	// `internal` or `admin` are not mistaken for an `in` operator.
	if !key.ends_with(char::is_whitespace) || key.trim().is_empty() {
		return None;
	}
	if !rest.starts_with(char::is_whitespace) && !rest.starts_with('(') {
		return None;
	}

	Some((key.trim().to_owned(), rest))
}

fn parse_set(requirement: &str, values: &str) -> Result<BTreeSet<String>, Error> {
	let values = values.trim();
	let values = values
		.strip_prefix('(')
		.and_then(|values| values.strip_suffix(')'))
		.ok_or_else(|| Error::InvalidSelector {
			selector: requirement.into(),
			reason: "set values must be parenthesized, as in `key in (a, b)`".into(),
		})?;

	Ok(values
		.split(',')
		.map(str::trim)
		.filter(|value| !value.is_empty())
		.map(str::to_owned)
		.collect())
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use kube_core::SelectorExt;

	use super::*;

	fn labels<const N: usize>(labels: [(&str, &str); N]) -> BTreeMap<String, String> {
		labels
			.into_iter()
			.map(|(key, value)| (key.to_owned(), value.to_owned()))
			.collect()
	}

	fn matches(selector: &str, labels: &BTreeMap<String, String>) -> bool {
		parse(selector).expect("a valid selector").matches(labels)
	}

	#[test]
	fn matches_equality() {
		let labels = labels([("tier", "test"), ("team", "platform")]);
		assert!(matches("tier=test", &labels));
		assert!(matches("tier==test", &labels));
		assert!(matches("tier = test", &labels));
		assert!(!matches("tier=prod", &labels));
		assert!(matches("tier!=prod", &labels));
		assert!(matches("tier=test,team=platform", &labels));
		assert!(!matches("tier=test,team=infra", &labels));
	}

	#[test]
	fn matches_existence() {
		let labels = labels([("tier", "test")]);
		assert!(matches("tier", &labels));
		assert!(!matches("!tier", &labels));
		assert!(matches("!team", &labels));
		// Absent labels satisfy inequality, as Kubernetes defines it.
		assert!(matches("team!=platform", &labels));
	}

	#[test]
	fn matches_sets() {
		let labels = labels([("tier", "test")]);
		assert!(matches("tier in (test, prod)", &labels));
		assert!(!matches("tier in (dev, prod)", &labels));
		assert!(matches("tier notin (dev, prod)", &labels));
		assert!(!matches("tier notin (test)", &labels));
		// A comma inside the set is not a requirement separator.
		assert!(matches("tier in (test,prod),!team", &labels));
	}

	#[test]
	fn keys_containing_operators_stay_keys() {
		let labels = labels([("internal", "yes"), ("index", "1")]);
		assert!(matches("internal", &labels));
		assert!(matches("internal=yes", &labels));
		assert!(matches("index=1", &labels));
	}

	#[test]
	fn empty_selectors_match_everything() {
		let labels = labels([("tier", "test")]);
		assert!(matches("", &labels));
		assert!(matches(",", &labels));
		assert!(parse("").unwrap().selects_all());
	}

	#[test]
	fn rejects_malformed_selectors() {
		assert!(parse("tier in test, prod").is_err());
		assert!(parse("tier in (test").is_err());
		assert!(parse("tier)").is_err());
		assert!(parse("not a selector").is_err());
	}
}
