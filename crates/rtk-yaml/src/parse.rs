use std::{borrow::Cow, collections::HashSet};

use serde::de::DeserializeOwned;
use serde_saphyr::{Error, ExternalMessageSource, Options, granit_parser::ErrorKind};

struct CompatibleInput<'a> {
	text: Cow<'a, str>,
	indent: Option<usize>,
	repaired: HashSet<usize>,
}

impl<'a> CompatibleInput<'a> {
	fn new(text: &'a str) -> Self {
		Self {
			text: Cow::Borrowed(text),
			indent: None,
			repaired: HashSet::new(),
		}
	}

	fn repair(&mut self, error: &Error) -> bool {
		let Error::ExternalMessage {
			source, location, ..
		} = error.without_snippet()
		else {
			return false;
		};
		let ExternalMessageSource::Parser(source) = source.as_ref() else {
			return false;
		};
		if !matches!(source.kind(), ErrorKind::InvalidQuotedScalarIndent) {
			return false;
		}
		let line = location.line() as usize;
		if line < 2 || !self.repaired.insert(line) {
			return false;
		}
		let indent = *self.indent.get_or_insert_with(|| {
			self.text
				.split(|character| character != ' ')
				.map(str::len)
				.max()
				.unwrap_or(0)
				+ 1
		});
		let Some((offset, _)) = self.text.char_indices().nth(source.marker().index()) else {
			return false;
		};
		// Go accepts these continuations; indentation within a quoted scalar is folded away.
		self.text.to_mut().insert_str(offset, &" ".repeat(indent));
		true
	}
}

/// Parse YAML documents using Go's permissive quoted-scalar continuation indentation.
pub fn parse_documents<T: DeserializeOwned>(text: &str, options: Options) -> Result<Vec<T>, Error> {
	let mut input = CompatibleInput::new(text);
	loop {
		match serde_saphyr::from_multiple_with_options(&input.text, options.clone()) {
			Err(error) if input.repair(&error) => {}
			result => return result,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::parse_documents;
	use serde_json::{Value, json};

	#[test]
	fn quoted_continuations_can_be_less_indented_than_the_key() {
		for quote in ['\'', '"'] {
			for indent in ["", " ", "  "] {
				let yaml = format!(
					"outer:\n  description: {quote}first\n{indent}second\n{indent}third{quote}\n"
				);
				assert_eq!(
					parse_documents::<Value>(&yaml, Default::default()).unwrap(),
					vec![json!({ "outer": { "description": "first second third" } })]
				);
			}
		}
	}

	#[test]
	fn repairs_preserve_blank_lines_escapes_and_other_scalars() {
		let yaml = "single: 'it''s first\n\nsecond'\ndouble: \"third\\n\nfourth\"\nliteral: |\n  fifth\n  sixth\n---\nlast: true\n";
		assert_eq!(
			parse_documents::<Value>(yaml, Default::default()).unwrap(),
			vec![
				json!({ "single": "it's first\nsecond", "double": "third\n fourth", "literal": "fifth\nsixth\n" }),
				json!({ "last": true })
			]
		);
	}

	#[test]
	fn repairs_use_character_offsets_and_accept_cr_line_breaks() {
		for newline in ["\n", "\r\n", "\r"] {
			let yaml = format!("outer:{newline}  description: 'één{newline}twee'{newline}");
			assert_eq!(
				parse_documents::<Value>(&yaml, Default::default()).unwrap(),
				vec![json!({ "outer": { "description": "één twee" } })]
			);
		}
	}

	#[test]
	fn unrelated_syntax_errors_are_still_rejected() {
		for yaml in [
			"value: 'first\nsecond",
			"value: [first\nsecond",
			"value: 'first\nsecond'\nother: [",
		] {
			assert!(parse_documents::<Value>(yaml, Default::default()).is_err());
		}
	}
}
