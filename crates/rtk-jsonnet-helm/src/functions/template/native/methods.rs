//! Adapt Helm's method calls to function calls supported by the template engine.

use anyhow::{Result, ensure};
use gtmpl_ng::{FuncError, Template, Value};

pub(super) fn install(template: &mut Template) {
	template.add_shared_func("__rtkAsMap", |args| {
		let args = Arguments::new(args, 1)?;
		let empty = match &args.0[0] {
			Value::SharedMap(values) => values.with(std::collections::HashMap::is_empty),
			Value::Map(values) => values.is_empty(),
			_ => return Err(FuncError::Generic("AsMap requires values".into())),
		};
		Ok(if empty {
			Value::Map(std::collections::HashMap::new())
		} else {
			args.0[0].clone()
		})
	});
	template.add_func("__rtkLiteral", |args| {
		use base64::Engine as _;
		let args = Arguments::new(args, 1)?;
		let decoded = base64::engine::general_purpose::STANDARD
			.decode(args.0[0].to_string())
			.map_err(anyhow::Error::from)?;
		Ok(String::from_utf8(decoded)
			.map_err(anyhow::Error::from)?
			.into())
	});
	template.add_func("__rtkField", |args| {
		let args = Arguments::new(args, 2)?;
		let mut value = args.0[0].clone();
		for field in args.0[1]
			.to_string()
			.split('.')
			.filter(|field| !field.is_empty())
		{
			value = match value.into_snapshot() {
				Value::Map(fields) | Value::Object(fields) => {
					fields.get(field).cloned().unwrap_or(Value::NoValue)
				}
				Value::Nil | Value::NoValue => Value::NoValue,
				_ => {
					return Err(FuncError::Generic(
						"field access requires a dictionary".into(),
					));
				}
			};
		}
		Ok(value)
	});
	template.add_func("__rtkHas", |args| {
		let args = Arguments::new(args, 2)?;
		let Value::Array(versions) = &args.0[0] else {
			return Err(FuncError::Generic("Has requires an API version set".into()));
		};
		Ok(versions.iter().any(|version| version == &args.0[1]).into())
	});
	template.add_func("__rtkGet", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(String::from_utf8_lossy(&args.file()?).into_owned().into())
	});
	template.add_func("__rtkGetBytes", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(Value::Array(
			args.file()?.into_iter().map(Value::from).collect(),
		))
	});
	template.add_func("__rtkLines", |args| {
		let args = Arguments::new(args, 2)?;
		let bytes = args.file()?;
		let text = String::from_utf8_lossy(&bytes);
		Ok(Value::Array(
			text.strip_suffix('\n')
				.unwrap_or(&text)
				.split('\n')
				.map(Value::from)
				.collect(),
		))
	});
	template.add_func("__rtkGlob", |args| {
		let args = Arguments::new(args, 2)?;
		let pattern = args.0[1].to_string();
		let matcher = globset::GlobBuilder::new(&pattern)
			.literal_separator(true)
			.build()
			.map(|pattern| pattern.compile_matcher())
			.ok();
		let Value::Map(files) = &args.0[0] else {
			return Err(FuncError::Generic("Glob requires chart files".into()));
		};
		Ok(Value::Map(
			files
				.iter()
				.filter(|(name, _)| {
					matcher
						.as_ref()
						.is_none_or(|matcher| matcher.is_match(name))
				})
				.map(|(name, value)| (name.clone(), value.clone()))
				.collect(),
		))
	});
	template.add_func("__rtkAsConfig", |args| {
		Ok(Arguments::new(args, 1)?.yaml(false)?.into())
	});
	template.add_func("__rtkAsSecrets", |args| {
		Ok(Arguments::new(args, 1)?.yaml(true)?.into())
	});
}

struct Arguments<'a>(&'a [Value]);
impl<'a> Arguments<'a> {
	fn new(args: &'a [Value], count: usize) -> Result<Self> {
		ensure!(args.len() == count, "expected {count} method arguments");
		Ok(Self(args))
	}
	fn file(&self) -> Result<Vec<u8>> {
		let Value::Map(files) = &self.0[0] else {
			anyhow::bail!("file access requires chart files");
		};
		let Value::String(name) = &self.0[1] else {
			anyhow::bail!("filename must be a string");
		};
		files
			.get(name)
			.map(Self::bytes)
			.transpose()
			.map(Option::unwrap_or_default)
	}
	fn bytes(value: &Value) -> Result<Vec<u8>> {
		if let Value::String(text) = value {
			return Ok(text.as_bytes().to_vec());
		}
		let Value::Array(bytes) = value else {
			anyhow::bail!("invalid chart file bytes");
		};
		bytes
			.iter()
			.map(|byte| {
				let Value::Number(number) = byte else {
					anyhow::bail!("invalid chart file byte");
				};
				Ok(u8::try_from(
					number.as_u64().context("invalid chart file byte")?,
				)?)
			})
			.collect()
	}
	fn yaml(&self, secrets: bool) -> Result<String> {
		use base64::Engine as _;
		let Value::Map(files) = &self.0[0] else {
			anyhow::bail!("file formatting requires chart files");
		};
		let mut values = serde_json::Map::new();
		let sorted: std::collections::BTreeMap<_, _> = files.iter().collect();
		for (name, value) in sorted {
			let bytes = Self::bytes(value)?;
			let text = if secrets {
				base64::engine::general_purpose::STANDARD.encode(bytes)
			} else {
				String::from_utf8_lossy(&bytes).into_owned()
			};
			values.insert(
				name.rsplit('/').next().unwrap().to_owned(),
				serde_json::json!(text),
			);
		}
		let mut output = String::new();
		rtk_yaml::to_fmt_writer_with_options(
			&mut output,
			&serde_json::Value::Object(values),
			rtk_yaml::SerializerOptions::tanka_v2(),
		)
		.map_err(anyhow::Error::from)?;
		Ok(output.trim_end_matches('\n').to_owned())
	}
}
use anyhow::Context as _;

pub(super) struct Actions;
impl Actions {
	pub(super) fn rewrite(source: &str) -> String {
		let mut output = String::new();
		let mut rest = source;
		while let Some(start) = rest.find("{{") {
			output.push_str(&rest[..start + 2]);
			rest = &rest[start + 2..];
			if rest.starts_with('-')
				&& rest
					.as_bytes()
					.get(1)
					.is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
				&& let Some(tail) = rest.strip_prefix('-')
			{
				output.push('-');
				rest = tail;
			}
			let Some(end) = Self::end(rest) else {
				output.push_str(rest);
				return output;
			};
			let (action, tail) = rest.split_at(end);
			let (action, trim) = action
				.strip_suffix('-')
				.map_or((action, false), |action| (action, true));
			if action.trim_start().starts_with("/*") {
				output.push_str(action);
			} else {
				output.push(' ');
				output.push_str(&Action::new(action).rewrite());
				output.push(' ');
			}
			if trim {
				output.push('-');
			}
			output.push_str("}}");
			rest = &tail[2..];
		}
		output.push_str(rest);
		output
	}

	fn end(source: &str) -> Option<usize> {
		let bytes = source.as_bytes();
		let mut index = 0;
		let mut quote = None;
		while index + 1 < bytes.len() {
			if let Some(delimiter) = quote {
				if bytes[index] == b'\\' && delimiter != b'`' {
					index += 2;
					continue;
				}
				if bytes[index] == delimiter {
					quote = None;
				}
			} else if bytes[index..].starts_with(b"/*") {
				index += source[index + 2..].find("*/")? + 4;
				continue;
			} else if matches!(bytes[index], b'"' | b'\'' | b'`') {
				quote = Some(bytes[index]);
			} else if bytes[index..].starts_with(b"}}") {
				return Some(index);
			}
			index += 1;
		}
		None
	}
}

#[derive(Clone)]
struct Lexeme {
	text: String,
	joined: bool,
}
impl From<&str> for Lexeme {
	fn from(text: &str) -> Self {
		Self {
			text: text.into(),
			joined: false,
		}
	}
}
impl From<String> for Lexeme {
	fn from(text: String) -> Self {
		Self {
			text,
			joined: false,
		}
	}
}
impl std::ops::Deref for Lexeme {
	type Target = str;
	fn deref(&self) -> &str {
		&self.text
	}
}
impl PartialEq<&str> for Lexeme {
	fn eq(&self, other: &&str) -> bool {
		self.text == *other
	}
}

struct Action {
	tokens: Vec<Lexeme>,
	original: String,
}
impl Action {
	fn new(source: &str) -> Self {
		let mut tokens = Vec::new();
		let chars: Vec<_> = source.chars().collect();
		let mut index = 0;
		while index < chars.len() {
			if chars[index].is_whitespace() {
				index += 1;
				continue;
			}
			let start = index;
			let character = chars[index];
			index += 1;
			if matches!(character, '\"' | '\'' | '`') {
				while index < chars.len() {
					if chars[index] == '\\' && character != '`' {
						index = (index + 2).min(chars.len());
						continue;
					}
					let done = chars[index] == character;
					index += 1;
					if done {
						break;
					}
				}
			} else if !matches!(character, '(' | ')' | '|') {
				while index < chars.len()
					&& !chars[index].is_whitespace()
					&& !matches!(chars[index], '(' | ')' | '|')
				{
					index += 1;
				}
			}
			tokens.push(Lexeme {
				text: chars[start..index].iter().collect(),
				joined: start > 0 && chars[start] == '.' && chars[start - 1] == ')',
			});
		}
		Self {
			tokens,
			original: source.to_owned(),
		}
	}

	fn rewrite(mut self) -> String {
		let mut index = 0;
		let mut changed = false;
		for token in &mut self.tokens {
			if let Some(raw) = token
				.text
				.strip_prefix('`')
				.and_then(|text| text.strip_suffix('`'))
			{
				token.text = serde_json::to_string(&raw.replace('\r', "")).unwrap();
				changed = true;
			}
		}
		for token in &mut self.tokens {
			if token.text.starts_with('"')
				&& token.text.contains("\\\\")
				&& let Ok(decoded) = serde_json::from_str::<String>(&token.text)
			{
				use base64::Engine as _;
				token.text = format!(
					"(__rtkLiteral {:?})",
					base64::engine::general_purpose::STANDARD.encode(decoded)
				);
				changed = true;
			}
		}

		while index < self.tokens.len() {
			let token = &self.tokens[index];
			if !token.starts_with(['.', '$'])
				|| !token.chars().all(|character| {
					character.is_alphanumeric() || matches!(character, '.' | '$' | '_')
				}) {
				index += 1;
				continue;
			}
			let Some((receiver, method)) = token.rsplit_once('.') else {
				index += 1;
				continue;
			};
			let no_arguments = matches!(method, "AsConfig" | "AsSecrets" | "AsMap");
			let function = match method {
				"Has" => "__rtkHas",
				"Get" => "__rtkGet",
				"GetBytes" => "__rtkGetBytes",
				"Lines" => "__rtkLines",
				"Glob" => "__rtkGlob",
				"AsConfig" => "__rtkAsConfig",
				"AsSecrets" => "__rtkAsSecrets",
				"AsMap" => "__rtkAsMap",
				_ => {
					if token.joined && index > 0 && self.tokens[index - 1] == ")" {
						let field = token.text.clone();
						let mut depth = 0;
						let start = (0..index).rev().find(|&position| {
							if self.tokens[position] == ")" {
								depth += 1;
							} else if self.tokens[position] == "(" {
								depth -= 1;
							}
							depth == 0
						});
						if let Some(start) = start {
							self.tokens.insert(start, "(".into());
							self.tokens.insert(start + 1, "__rtkField".into());
							self.tokens.splice(
								index + 2..=index + 2,
								[serde_json::to_string(&field).unwrap().into(), ")".into()],
							);
							index += 4;
							changed = true;
							continue;
						}
					}
					index += 1;
					continue;
				}
			};
			let receiver = receiver.to_owned();
			if token.joined && index > 0 && self.tokens[index - 1] == ")" {
				let mut depth = 0;
				let start = (0..index).rev().find(|&position| {
					if self.tokens[position] == ")" {
						depth += 1;
					} else if self.tokens[position] == "(" {
						depth -= 1;
					}
					depth == 0
				});
				let Some(start) = start else {
					index += 1;
					continue;
				};
				let suffix = Lexeme {
					text: receiver.clone(),
					joined: true,
				};
				if no_arguments {
					self.tokens.insert(start, "(".into());
					self.tokens.insert(start + 1, function.into());
					let replacement = if receiver.is_empty() {
						vec![")".into()]
					} else {
						vec![suffix, ")".into()]
					};
					let length = replacement.len();
					self.tokens.splice(index + 2..=index + 2, replacement);
					index += 2 + length;
				} else {
					self.tokens.insert(start, function.into());
					if receiver.is_empty() {
						self.tokens.remove(index + 1);
					} else {
						self.tokens[index + 1] = suffix;
					}
					index += 1;
				}
			} else {
				let receiver = if receiver.is_empty() {
					".".to_owned()
				} else {
					receiver
				};
				let replacement = if no_arguments {
					vec!["(".into(), function.into(), receiver.into(), ")".into()]
				} else {
					vec![function.into(), receiver.into()]
				};
				let length = replacement.len();
				self.tokens.splice(index..=index, replacement);
				index += length;
			}
			changed = true;
		}
		if !changed {
			return self.original;
		}
		let mut output = String::new();
		for token in self.tokens {
			if !output.is_empty() && !token.joined {
				output.push(' ');
			}
			output.push_str(&token.text);
		}
		output
	}
}
