use anyhow::{Context as _, Result, ensure};

pub(super) struct Version {
	pub(super) value: semver::Version,
	original: String,
}

impl Version {
	pub(super) fn parse(text: &str) -> Result<Self> {
		let original = text.to_owned();
		let text = text.trim().trim_start_matches('v');
		let end = text.find(['-', '+']).unwrap_or(text.len());
		let core = &text[..end];
		let count = core.split('.').count();
		ensure!(
			(1..=3).contains(&count),
			"invalid semantic version {original}"
		);
		let completed = format!("{core}{}{}", ".0".repeat(3 - count), &text[end..]);
		Ok(Self {
			value: completed
				.parse()
				.with_context(|| format!("invalid semantic version {original}"))?,
			original,
		})
	}

	pub(super) fn matches(&self, text: &str) -> Result<bool> {
		for alternative in text.split("||") {
			let normalized = Self::conjunction(alternative)?;
			let mut matches = true;
			for term in normalized.split(',') {
				let term = term.trim();
				if let Some(caret) = term.strip_prefix('^') {
					let minimum = Self::parse(caret)?;
					let count = caret
						.trim()
						.trim_start_matches('v')
						.split(['-', '+'])
						.next()
						.unwrap()
						.split('.')
						.count();
					let upper = if minimum.value.major > 0 || count == 1 {
						format!(
							"{}.0.0",
							minimum
								.value
								.major
								.checked_add(1)
								.context("version overflow")?
						)
					} else if minimum.value.minor > 0 || count == 2 {
						format!(
							"0.{}.0",
							minimum
								.value
								.minor
								.checked_add(1)
								.context("version overflow")?
						)
					} else {
						format!(
							"0.0.{}",
							minimum
								.value
								.patch
								.checked_add(1)
								.context("version overflow")?
						)
					};
					matches &= rtk_masterminds::Constraints::parse(&format!(
						">={},<{upper}",
						minimum.value
					))?
					.matches(&self.value);
				} else {
					matches &= rtk_masterminds::Constraints::parse(term)?.matches(&self.value);
				}
			}
			if matches {
				return Ok(true);
			}
		}
		Ok(false)
	}

	fn conjunction(text: &str) -> Result<String> {
		if rtk_masterminds::Constraints::parse(text).is_ok() {
			return Ok(text.to_owned());
		}
		static TERMS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
			regex::Regex::new(r"(?:>=|<=|!=|=>|=<|~>|>|<|=|~|\^)?\s*v?[0-9xX*]+(?:\.[0-9xX*]+){0,2}(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?").unwrap()
		});
		let mut end = 0;
		let mut terms = Vec::new();
		for term in TERMS.find_iter(text) {
			ensure!(
				text[end..term.start()]
					.trim_matches([' ', '\t', ','])
					.is_empty(),
				"invalid semantic version constraint {text}"
			);
			terms.push(term.as_str().trim());
			end = term.end();
		}
		ensure!(
			!terms.is_empty() && text[end..].trim().is_empty(),
			"invalid semantic version constraint {text}"
		);
		Ok(terms.join(","))
	}

	pub(super) fn fields(&self) -> serde_json::Value {
		serde_json::json!({"Major": self.value.major, "Minor": self.value.minor, "Patch": self.value.patch, "Prerelease": self.value.pre.as_str(), "Metadata": self.value.build.as_str(), "Original": self.original})
	}
}
