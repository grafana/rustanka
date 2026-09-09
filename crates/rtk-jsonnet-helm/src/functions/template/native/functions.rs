use anyhow::{Result, bail, ensure};
use gtmpl_ng::{FuncError, Template, Value};
use sha2::{Digest, Sha256};

use super::{Scope, from_json, to_json};

pub(super) fn install(template: &mut Template) {
	template.add_func("include", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(Scope::render(args.text(0)?, args.0[1].clone(), None)?.into())
	});
	template.add_func("tpl", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(Scope::render("__rtk_tpl", args.0[1].clone(), Some(args.text(0)?))?.into())
	});
	template.add_func("default", |args| {
		let args = Arguments::new(args, 2)?;
		Ok(if Arguments::empty(&args.0[1]) {
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
		Ok(
			serde_json::to_string(&to_json(&Arguments::new(args, 1)?.0[0])?)
				.map_err(anyhow::Error::from)?
				.into(),
		)
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
	fn new(values: &'a [Value], count: usize) -> Result<Self> {
		ensure!(
			values.len() == count,
			"expected {count} arguments, got {}",
			values.len()
		);
		Ok(Self(values))
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
