//! Tanka-compatible YAML writing with go-yaml v2/v3 formatting.
//!
//! YAML reading belongs to serde-saphyr. This crate owns output compatibility.

#![forbid(unsafe_code)]

mod ser;
pub mod ser_error;
mod ser_quoting;
mod serializer_options;
mod sort;

pub use ser::*;
pub use serializer_options::{ChompIndicator, SerializerOptions};
pub use sort::compare_string_keys;

use serde::Serialize;
use std::fmt::{self, Write};

pub fn to_string<T: Serialize>(value: &T) -> Result<String> {
	let mut output = String::new();
	to_fmt_writer(&mut output, value)?;
	Ok(output)
}

pub fn to_fmt_writer<W: Write, T: Serialize>(output: &mut W, value: &T) -> Result<()> {
	to_fmt_writer_with_options(output, value, SerializerOptions::default())
}

pub fn to_fmt_writer_with_options<W: Write, T: Serialize>(
	output: &mut W,
	value: &T,
	mut options: SerializerOptions,
) -> Result<()> {
	options.consistent()?;
	let mut serializer = YamlSer::with_options(output, &mut options);
	value.serialize(&mut serializer)
}

pub fn to_io_writer<W: std::io::Write, T: Serialize>(output: &mut W, value: &T) -> Result<()> {
	to_io_writer_with_options(output, value, SerializerOptions::default())
}

pub fn to_io_writer_with_options<W: std::io::Write, T: Serialize>(
	output: &mut W,
	value: &T,
	mut options: SerializerOptions,
) -> Result<()> {
	struct Adapter<'a, W: std::io::Write> {
		output: &'a mut W,
		last_error: Option<std::io::Error>,
	}

	impl<W: std::io::Write> Write for Adapter<'_, W> {
		fn write_str(&mut self, value: &str) -> fmt::Result {
			self.output.write_all(value.as_bytes()).map_err(|error| {
				self.last_error = Some(error);
				fmt::Error
			})
		}
	}

	options.consistent()?;
	let mut adapter = Adapter {
		output,
		last_error: None,
	};
	let result = {
		let mut serializer = YamlSer::with_options(&mut adapter, &mut options);
		value.serialize(&mut serializer)
	};
	match (result, adapter.last_error) {
		(_, Some(error)) => Err(Error::from(error)),
		(result, None) => result,
	}
}
