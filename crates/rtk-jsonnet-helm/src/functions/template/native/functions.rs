use anyhow::{Result, bail, ensure};
use gtmpl_ng::{FuncError, Template, Value};
use sha2::{Digest, Sha256};

use super::{Scope, from_json, to_json};

pub(super) fn install(template: &mut Template) {
	template.add_funcs(gtmpl_ng::helm_functions::HELM_FUNCTIONS);
	super::methods::install(template);

	template.add_func("tuple", |args| Ok(Value::Array(args.to_vec())));
	for name in ["has", "mustHas"] {
		template.add_func(name, |args| {
			let args = Arguments::new(args, 2)?;
			match &args.0[1] {
				Value::Array(values) => Ok(values.contains(&args.0[0]).into()),
				Value::Nil | Value::NoValue => Ok(false.into()),
				_ => Err(FuncError::Generic("has requires a list".into())),
			}
		});
	}
	template.add_func("fromJsonArray", |args| {
		let args = Arguments::new(args, 1)?;
		let value: serde_json::Value =
			serde_json::from_str(args.text(0)?).map_err(anyhow::Error::from)?;
		if !value.is_array() {
			return Err(FuncError::Generic("fromJsonArray requires an array".into()));
		}
		Ok(from_json(&value))
	});
	template.add_func("kindOf", |args| {
		Ok(Arguments::kind(&Arguments::new(args, 1)?.0[0]).into())
	});
	template.add_func("kindIs", |args| {
		let args = Arguments::new(args, 2)?;
		Ok((args.text(0)? == Arguments::kind(&args.0[1])).into())
	});
	template.add_func("semverCompare", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(super::versions::Version::parse(args.text(1)?)?
			.matches(args.text(0)?)?
			.into())
	});
	template.add_func("semver", |args| {
		let args = Arguments::new(args, 1)?;
		Ok(from_json(
			&super::versions::Version::parse(args.text(0)?)?.fields(),
		))
	});
	template.add_func("lookup", |args| {
		let args = Arguments::new(args, 4)?;
		for index in 0..4 {
			args.text(index)?;
		}
		Ok(from_json(&serde_json::json!({})))
	});
	template.add_func("fromYamlArray", |args| {
		let args = Arguments::new(args, 1)?;
		let value: serde_json::Value =
			serde_saphyr::from_str(args.text(0)?).map_err(anyhow::Error::from)?;
		if !value.is_array() {
			return Err(FuncError::Generic("fromYamlArray requires an array".into()));
		}
		Ok(from_json(&value))
	});
	template.add_func("join", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(args.strings(1).join(args.text(0)?).into())
	});
	template.add_func("sortAlpha", |args| {
		let args = Arguments::new(args, 1)?;
		let mut strings = args.strings(0);
		strings.sort();
		Ok(Value::Array(strings.into_iter().map(Value::from).collect()))
	});
	template.add_func("splitList", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(Value::Array(
			args.split(None)?.into_iter().map(Value::from).collect(),
		))
	});
	template.add_func("split", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(Value::Map(
			args.split(None)?
				.into_iter()
				.enumerate()
				.map(|(index, value)| (format!("_{index}"), value.into()))
				.collect(),
		))
	});
	template.add_func("splitn", |args| {
		let args = Arguments::new(args, 3)?;
		let split =
			Arguments(&[args.0[0].clone(), args.0[2].clone()]).split(Some(args.integer(1)?))?;
		Ok(Value::Map(
			split
				.into_iter()
				.enumerate()
				.map(|(index, value)| (format!("_{index}"), value.into()))
				.collect(),
		))
	});
	template.add_func("substr", |args| {
		let args = Arguments::new(args, 3)?;
		let text = args.text(2)?;
		let start = usize::try_from(args.integer(0)?.max(0))
			.map_err(anyhow::Error::from)?
			.min(text.len());
		let end = args.integer(1)?;
		let end = if end < 0 {
			text.len()
		} else {
			usize::try_from(end)
				.map_err(anyhow::Error::from)?
				.min(text.len())
		};
		Ok(text
			.get(start..end)
			.ok_or_else(|| {
				anyhow::anyhow!("substr has invalid bounds or splits a UTF-8 character")
			})?
			.into())
	});
	template.add_func("trimPrefix", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(args
			.text(1)?
			.strip_prefix(args.text(0)?)
			.unwrap_or(args.text(1)?)
			.into())
	});
	template.add_func("trimAll", |args| {
		let args = Arguments::new(args, 2)?;
		let cutset = args.text(0)?;
		Ok(args
			.text(1)?
			.trim_matches(|character| cutset.contains(character))
			.into())
	});
	template.add_func("toDecimal", |args| {
		let args = Arguments::new(args, 1)?;
		Ok(i64::from_str_radix(&args.0[0].to_string(), 8)
			.unwrap_or_default()
			.into())
	});
	for name in ["toRawJson", "mustToRawJson"] {
		template.add_func(name, |args| {
			let args = Arguments::new(args, 1)?;
			Ok(serde_json::to_string(&to_json(&args.0[0])?)
				.map_err(anyhow::Error::from)?
				.into())
		});
	}
	template.add_func("mustToJson", |args| {
		let args = Arguments::new(args, 1)?;
		Ok(Arguments::json(&to_json(&args.0[0])?)?.into())
	});
	template.add_func("mustFromJson", |args| {
		let args = Arguments::new(args, 1)?;
		Ok(from_json(
			&serde_json::from_str(args.text(0)?).map_err(anyhow::Error::from)?,
		))
	});
	template.add_func("dig", |args| Ok(Arguments(args).dig()?.clone()));
	for name in ["merge", "mustMerge"] {
		template.add_func(name, |args| Ok(Arguments(args).merge(false)?));
	}
	for name in ["mergeOverwrite", "mustMergeOverwrite"] {
		template.add_func(name, |args| Ok(Arguments(args).merge(true)?));
	}
	template.add_func("genPrivateKey", |args| {
		let args = Arguments::new(args, 1)?;
		Ok(super::crypto::Key::generate(args.text(0)?)?.into())
	});
	template.add_func("int", |args| {
		Ok(Arguments::new(args, 1)?.cast_integer(0).into())
	});
	template.add_func("until", |args| {
		let count = Arguments::new(args, 1)?.integer(0)?;
		if count.unsigned_abs() > 1_000_000 {
			return Err(FuncError::Generic(
				"until exceeds one million elements".into(),
			));
		}
		let values = if count >= 0 {
			(0..count).map(Value::from).collect()
		} else {
			((count + 1)..=0).rev().map(Value::from).collect()
		};
		Ok(Value::Array(values))
	});
	template.add_func("add", |args| {
		let args = Arguments(args);
		Ok((0..args.0.len())
			.fold(0_i64, |sum, i| sum.wrapping_add(args.cast_integer(i)))
			.into())
	});
	template.add_func("include", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(Scope::render(args.text(0)?, args.0[1].clone(), None)?.into())
	});
	template.add_func("tpl", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(Scope::render("__rtk_tpl", args.0[1].clone(), Some(args.text(0)?))?.into())
	});
	template.add_func("default", |args| {
		if args.is_empty() {
			return Err(FuncError::Generic(
				"default requires a default value".into(),
			));
		}
		let args = Arguments(args);
		Ok(if args.0.len() == 1 || Arguments::empty(&args.0[1]) {
			&args.0[0]
		} else {
			&args.0[1]
		}
		.clone())
	});
	template.add_func("required", |args| {
		let args = Arguments::new(args, 2)?;
		if Arguments::empty(&args.0[1]) {
			return Err(FuncError::Generic(args.text(0)?.into()));
		}
		Ok(args.0[1].clone())
	});
	template.add_func("fail", |args| {
		Err(FuncError::Generic(Arguments::new(args, 1)?.text(0)?.into()))
	});
	template.add_func("empty", |args| {
		Ok(Arguments::empty(&Arguments::new(args, 1)?.0[0]).into())
	});
	template.add_func("quote", |args| {
		let strings = args
			.iter()
			.filter(|value| !matches!(value, Value::Nil | Value::NoValue))
			.map(|value| serde_json::to_string(&value.to_string()).map_err(anyhow::Error::from))
			.collect::<Result<Vec<_>>>()?;
		Ok(strings.join(" ").into())
	});
	template.add_func("upper", |args| {
		Ok(Arguments::new(args, 1)?.text(0)?.to_uppercase().into())
	});
	template.add_func("lower", |args| {
		Ok(Arguments::new(args, 1)?.text(0)?.to_lowercase().into())
	});
	template.add_func("trim", |args| {
		Ok(Arguments::new(args, 1)?.text(0)?.trim().into())
	});
	template.add_func("trimSuffix", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(args
			.text(1)?
			.strip_suffix(args.text(0)?)
			.unwrap_or(args.text(1)?)
			.into())
	});
	template.add_func("trunc", |args| {
		let args = Arguments::new(args, 2)?;
		let length = args.integer(0)?;
		let text = args.text(1)?;
		let count = usize::try_from(length.unsigned_abs())
			.unwrap_or(usize::MAX)
			.min(text.len());
		let range = if length < 0 {
			text.len() - count..text.len()
		} else {
			0..count
		};
		Ok(text
			.get(range)
			.ok_or_else(|| anyhow::anyhow!("trunc splits a UTF-8 character"))?
			.into())
	});
	template.add_func("indent", |args| {
		Ok(Arguments::new(args, 2)?.indent(false)?.into())
	});
	template.add_func("nindent", |args| {
		Ok(Arguments::new(args, 2)?.indent(true)?.into())
	});
	template.add_func("toYaml", |args| {
		let args = Arguments::new(args, 1)?;
		let mut output = String::new();
		rtk_yaml::to_fmt_writer_with_options(
			&mut output,
			&to_json(&args.0[0])?,
			rtk_yaml::SerializerOptions::tanka_v2(),
		)
		.map_err(anyhow::Error::from)?;
		Ok(output.trim_end_matches('\n').into())
	});
	template.add_func("fromYaml", |args| {
		let args = Arguments::new(args, 1)?;
		let value: serde_json::Value = serde_saphyr::from_str_with_options(
			args.text(0)?,
			serde_saphyr::options! { legacy_octal_numbers: true },
		)
		.map_err(anyhow::Error::from)?;
		if !value.is_object() {
			return Err(FuncError::Generic("fromYaml expects a mapping".into()));
		}
		Ok(from_json(&value))
	});
	template.add_func("toJson", |args| {
		let args = Arguments::new(args, 1)?;
		Ok(Arguments::json(&to_json(&args.0[0])?)?.into())
	});
	template.add_func("fromJson", |args| {
		Ok(from_json(
			&serde_json::from_str(Arguments::new(args, 1)?.text(0)?)
				.map_err(anyhow::Error::from)?,
		))
	});
	template.add_func("dict", |args| {
		let args = Arguments(args);
		let mut map = std::collections::HashMap::new();
		for index in (0..args.0.len()).step_by(2) {
			map.insert(
				args.text(index)?.to_owned(),
				args.0.get(index + 1).cloned().unwrap_or_else(|| "".into()),
			);
		}
		Ok(Value::Map(map))
	});
	template.add_func("list", |args| Ok(Value::Array(args.to_vec())));
	template.add_func("sha256sum", |args| {
		Ok(hex::encode(Sha256::digest(Arguments::new(args, 1)?.text(0)?.as_bytes())).into())
	});
}

struct Arguments<'a>(&'a [Value]);
impl<'a> Arguments<'a> {
	fn kind(value: &Value) -> &'static str {
		match value {
			Value::Nil | Value::NoValue => "invalid",
			Value::Bool(_) => "bool",
			Value::String(_) => "string",
			Value::Array(_) => "slice",
			Value::Map(_) | Value::Object(_) => "map",
			Value::Function(_) => "func",
			Value::Number(number) if number.as_i64().is_some() => "int64",
			Value::Number(number) if number.as_u64().is_some() => "uint64",
			Value::Number(_) => "float64",
		}
	}

	fn new(values: &'a [Value], count: usize) -> Result<Self> {
		ensure!(
			values.len() == count,
			"expected {count} arguments, got {}",
			values.len()
		);
		Ok(Self(values))
	}

	fn dig(&self) -> Result<&Value> {
		ensure!(
			self.0.len() >= 3,
			"dig requires keys, a default and a dictionary"
		);
		let default = &self.0[self.0.len() - 2];
		let mut current = &self.0[self.0.len() - 1];
		for key in &self.0[..self.0.len() - 2] {
			let Value::String(key) = key else {
				bail!("dig keys must be strings");
			};
			let (Value::Map(map) | Value::Object(map)) = current else {
				bail!("dig requires a dictionary");
			};
			let Some(value) = map.get(key) else {
				return Ok(default);
			};
			current = value;
		}
		Ok(current)
	}
	fn merge(&self, overwrite: bool) -> Result<Value> {
		ensure!(
			!self.0.is_empty(),
			"merge requires a destination dictionary"
		);
		let mut values = to_json(&self.0[0])?;
		ensure!(values.is_object(), "merge requires dictionaries");
		for source in &self.0[1..] {
			let source = to_json(source)?;
			ensure!(source.is_object(), "merge requires dictionaries");
			Self::merge_maps(&mut values, &source, overwrite);
		}
		Ok(from_json(&values))
	}
	fn merge_maps(
		destination: &mut serde_json::Value,
		source: &serde_json::Value,
		overwrite: bool,
	) {
		if let (Some(destination), Some(source)) = (destination.as_object_mut(), source.as_object())
		{
			for (key, value) in source {
				match destination.get_mut(key) {
					Some(existing) if existing.is_object() && value.is_object() => {
						Self::merge_maps(existing, value, overwrite);
					}
					Some(existing) if overwrite || existing.is_null() => *existing = value.clone(),
					Some(_) => {}
					None => {
						destination.insert(key.clone(), value.clone());
					}
				}
			}
		}
	}

	fn strings(&self, index: usize) -> Vec<String> {
		match &self.0[index] {
			Value::Array(values) => values
				.iter()
				.filter(|value| !matches!(value, Value::Nil | Value::NoValue))
				.map(ToString::to_string)
				.collect(),
			Value::Nil | Value::NoValue => Vec::new(),
			value => vec![value.to_string()],
		}
	}
	fn split(&self, count: Option<i64>) -> Result<Vec<String>> {
		let separator = self.text(0)?;
		let text = self.text(1)?;
		if count == Some(0) {
			return Ok(Vec::new());
		}
		if separator.is_empty() {
			let mut values: Vec<_> = text
				.chars()
				.map(|character| character.to_string())
				.collect();
			if let Some(count) = count.filter(|count| *count > 0)
				&& usize::try_from(count)? < values.len()
			{
				let remainder = values.split_off(usize::try_from(count - 1)?).join("");
				values.push(remainder);
			}
			return Ok(values);
		}
		Ok(match count.filter(|count| *count > 0) {
			Some(count) => text
				.splitn(usize::try_from(count)?, separator)
				.map(str::to_owned)
				.collect(),
			None => text.split(separator).map(str::to_owned).collect(),
		})
	}
	fn json(value: &serde_json::Value) -> Result<String> {
		Ok(serde_json::to_string(value)?
			.replace('&', "\\u0026")
			.replace('<', "\\u003c")
			.replace('>', "\\u003e")
			.replace('\u{2028}', "\\u2028")
			.replace('\u{2029}', "\\u2029"))
	}

	fn text(&self, index: usize) -> Result<&str> {
		match &self.0[index] {
			Value::String(text) => Ok(text),
			_ => bail!("argument {} must be a string", index + 1),
		}
	}

	fn integer(&self, index: usize) -> Result<i64> {
		if let Value::Number(number) = &self.0[index]
			&& let Some(number) = number.as_i64()
		{
			return Ok(number);
		}
		bail!("argument {} must be an integer", index + 1)
	}

	// Sprig truncates fractional values when converting to an integer.
	#[allow(clippy::cast_possible_truncation)]
	fn cast_integer(&self, index: usize) -> i64 {
		match &self.0[index] {
			Value::Number(number) => number
				.as_i64()
				.unwrap_or_else(|| number.as_f64().unwrap_or_default() as i64),
			Value::Bool(value) => i64::from(*value),
			Value::String(value) => value.parse().unwrap_or_default(),
			_ => 0,
		}
	}

	fn indent(&self, newline: bool) -> Result<String> {
		let width = self.integer(0)?;
		ensure!(
			(0..=65536).contains(&width),
			"indent width must be between 0 and 65536"
		);
		let padding = " ".repeat(usize::try_from(width)?);
		Ok(format!(
			"{}{padding}{}",
			if newline { "\n" } else { "" },
			self.text(1)?.replace('\n', &format!("\n{padding}"))
		))
	}

	fn empty(value: &Value) -> bool {
		match value {
			Value::Nil | Value::NoValue => true,
			Value::Bool(value) => !value,
			Value::String(value) => value.is_empty(),
			Value::Array(value) => value.is_empty(),
			Value::Map(value) | Value::Object(value) => value.is_empty(),
			Value::Number(value) => {
				value.as_i64() == Some(0)
					|| value.as_u64() == Some(0)
					|| value.as_f64() == Some(0.0)
			}
			Value::Function(_) => false,
		}
	}
}
