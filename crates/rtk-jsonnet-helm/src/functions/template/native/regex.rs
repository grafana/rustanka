use anyhow::{Result, ensure};
use gtmpl_ng::{Template, Value};

pub(super) fn install(template: &mut Template) {
	macro_rules! install {
        ($($name:literal),*) => { $(template.add_func($name, |args| Ok(Pattern::call($name, args)?));)* };
    }
	install!(
		"regexMatch",
		"mustRegexMatch",
		"regexFind",
		"mustRegexFind",
		"regexFindAll",
		"mustRegexFindAll",
		"regexReplaceAll",
		"mustRegexReplaceAll",
		"regexReplaceAllLiteral",
		"mustRegexReplaceAllLiteral",
		"regexSplit",
		"mustRegexSplit"
	);
}

struct Pattern;
impl Pattern {
	fn call(name: &str, arguments: &[Value]) -> Result<Value> {
		ensure!(!arguments.is_empty(), "regex requires a pattern");
		let mut arguments = arguments.to_vec();
		arguments[0] = Self::normalize(&arguments[0].to_string()).into();
		let (_, function) = gtmpl_ng::helm_functions::HELM_FUNCTIONS
			.iter()
			.find(|(candidate, _)| *candidate == name)
			.unwrap();
		Ok(function(&arguments)?)
	}
	fn normalize(pattern: &str) -> String {
		let mut output = String::new();
		let mut characters = pattern.chars().peekable();
		let mut class = false;
		while let Some(character) = characters.next() {
			match character {
				'\\' => {
					output.push(character);
					if let Some(next) = characters.next() {
						output.push(next);
					}
				}
				'[' => {
					class = true;
					output.push(character);
				}
				']' => {
					class = false;
					output.push(character);
				}
				'{' if !class => {
					let remaining: String = characters.clone().collect();
					let quantifier = remaining.split_once('}').filter(|(count, _)| {
						let mut parts = count.split(',');
						let first = parts.next().unwrap();
						!first.is_empty()
							&& first.chars().all(|character| character.is_ascii_digit())
							&& parts.next().is_none_or(|last| {
								last.chars().all(|character| character.is_ascii_digit())
							}) && parts.next().is_none()
					});
					if let Some((count, _)) = quantifier {
						output.push('{');
						output.push_str(count);
						output.push('}');
						for _ in 0..=count.chars().count() {
							characters.next();
						}
					} else {
						output.push_str("\\{");
					}
				}
				'}' if !class => output.push_str("\\}"),
				_ => output.push(character),
			}
		}
		output
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn re2_literal_braces_are_not_rust_repetition_errors() {
		let pattern = Pattern::normalize(r"\$(?:__(?:env|file|vault))?{[^}]+}");
		assert!(
			regex::Regex::new(&pattern)
				.unwrap()
				.is_match("$__env{PASSWORD}")
		);
		assert_eq!(Pattern::normalize(r"a{2,3}[{}]\{"), r"a{2,3}[{}]\{");
	}
}
