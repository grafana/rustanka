use anyhow::{Context as _, Result, ensure};
use gtmpl_ng::{Template, Value};

pub(super) fn install(template: &mut Template) {
	template.add_func("now", |args| {
		if !args.is_empty() {
			return Err(gtmpl_ng::FuncError::Generic(
				"now takes no arguments".into(),
			));
		}
		Ok(Clock::now()?.into())
	});
	template.add_func("dateModify", |args| Ok(Clock::modify(args, false)?.into()));
	template.add_func("mustDateModify", |args| {
		Ok(Clock::modify(args, true)?.into())
	});
	template.add_func("unixEpoch", |args| {
		Ok((Clock::timestamp(args, 1, 0)? / 1_000_000_000)
			.to_string()
			.into())
	});
}

struct Clock;
impl Clock {
	fn now() -> Result<i64> {
		Ok(i64::try_from(
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)?
				.as_nanos(),
		)?)
	}
	fn timestamp(args: &[Value], count: usize, index: usize) -> Result<i64> {
		ensure!(args.len() == count, "expected {count} clock arguments");
		let Value::Number(value) = &args[index] else {
			anyhow::bail!("expected a clock timestamp");
		};
		value.as_i64().context("invalid clock timestamp")
	}
	fn modify(args: &[Value], must: bool) -> Result<i64> {
		let timestamp = Self::timestamp(args, 2, 1)?;
		let Value::String(duration) = &args[0] else {
			anyhow::bail!("duration must be a string");
		};
		match Self::duration(duration) {
			Ok(duration) => timestamp
				.checked_add(duration)
				.context("clock timestamp overflow"),
			Err(error) if must => Err(error),
			Err(_) => Ok(timestamp),
		}
	}
	fn duration(text: &str) -> Result<i64> {
		let (negative, text) = text.strip_prefix('-').map_or_else(
			|| (false, text.strip_prefix('+').unwrap_or(text)),
			|text| (true, text),
		);
		if text == "0" {
			return Ok(0);
		}
		static PARTS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
			regex::Regex::new(r"([0-9]+(?:\.[0-9]*)?|\.[0-9]+)(ns|us|µs|μs|ms|s|m|h)").unwrap()
		});
		let mut end = 0;
		let mut total = 0_i128;
		for captures in PARTS.captures_iter(text) {
			let matched = captures.get(0).unwrap();
			ensure!(matched.start() == end, "invalid duration {text}");
			end = matched.end();
			let unit: i128 = match &captures[2] {
				"ns" => 1,
				"us" | "µs" | "μs" => 1_000,
				"ms" => 1_000_000,
				"s" => 1_000_000_000,
				"m" => 60_000_000_000,
				"h" => 3_600_000_000_000,
				_ => unreachable!(),
			};
			let (whole, fraction) = captures[1].split_once('.').unwrap_or((&captures[1], ""));
			let whole = if whole.is_empty() {
				0
			} else {
				whole.parse::<i128>()?
			};
			let fraction: String = fraction.chars().take(18).collect();
			let nanos = if fraction.is_empty() {
				0
			} else {
				fraction.parse::<i128>()? * unit / 10_i128.pow(u32::try_from(fraction.len())?)
			};
			total = total
				.checked_add(whole.checked_mul(unit).context("duration overflow")?)
				.and_then(|total| total.checked_add(nanos))
				.context("duration overflow")?;
		}
		ensure!(end == text.len() && end > 0, "invalid duration {text}");
		Ok(i64::try_from(if negative { -total } else { total })?)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn go_duration_units_signs_and_fractions() {
		for (text, expected) in [
			("1h2m3.5s", 3_723_500_000_000),
			("-.5ms", -500_000),
			("1µs", 1_000),
			("1μs", 1_000),
			("+0", 0),
		] {
			assert_eq!(Clock::duration(text).unwrap(), expected);
		}
		for text in ["", "1d", "1", "1s trailing", "999999999999999999999h"] {
			assert!(Clock::duration(text).is_err());
		}
		assert!(Clock::modify(&["invalid".into(), 42_i64.into()], true).is_err());
		assert_eq!(
			Clock::modify(&["invalid".into(), 42_i64.into()], false).unwrap(),
			42
		);
		assert_eq!(
			Clock::modify(&["0s".into(), 42_i64.into()], true).unwrap(),
			42
		);
	}
}
