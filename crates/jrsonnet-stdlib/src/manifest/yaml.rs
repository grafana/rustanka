use std::{borrow::Cow, fmt::Write};

use jrsonnet_evaluator::{
	bail, in_description_frame,
	manifest::{escape_string_json_buf, ManifestFormat},
	Result, ResultExt, Val,
};

pub struct YamlFormat<'s> {
	/// Padding before fields, i.e
	/// ```yaml
	/// a:
	///   b:
	/// ## <- this
	/// ```
	padding: Cow<'s, str>,
	/// Padding before array elements in objects
	/// ```yaml
	/// a:
	///   - 1
	/// ## <- this
	/// ```
	arr_element_padding: Cow<'s, str>,
	/// Should yaml keys appear unescaped, when possible
	/// ```yaml
	/// "safe_key": 1
	/// # vs
	/// safe_key: 1
	/// ```
	quote_keys: bool,
	/// If true - then order of fields is preserved as written,
	/// instead of sorting alphabetically
	#[cfg(feature = "exp-preserve-order")]
	preserve_order: bool,
}
impl YamlFormat<'_> {
	pub fn cli(
		padding: usize,
		#[cfg(feature = "exp-preserve-order")] preserve_order: bool,
	) -> Self {
		let padding = " ".repeat(padding);
		Self {
			padding: Cow::Owned(padding.clone()),
			arr_element_padding: Cow::Owned(padding),
			quote_keys: false,
			#[cfg(feature = "exp-preserve-order")]
			preserve_order,
		}
	}
	pub fn std_to_yaml(
		indent_array_in_object: bool,
		quote_keys: bool,
		#[cfg(feature = "exp-preserve-order")] preserve_order: bool,
	) -> Self {
		Self {
			padding: Cow::Borrowed("  "),
			arr_element_padding: Cow::Borrowed(if indent_array_in_object { "  " } else { "" }),
			quote_keys,
			#[cfg(feature = "exp-preserve-order")]
			preserve_order,
		}
	}
}
impl ManifestFormat for YamlFormat<'_> {
	fn manifest_buf(&self, val: Val, buf: &mut String) -> Result<()> {
		manifest_yaml_ex_buf(&val, buf, &mut String::new(), self)
	}
}

/// From <https://github.com/chyh1990/yaml-rust/blob/da52a68615f2ecdd6b7e4567019f280c433c1521/src/emitter.rs#L289>
/// With added date check
fn yaml_needs_quotes(string: &str) -> bool {
	fn need_quotes_spaces(string: &str) -> bool {
		string.starts_with(' ') || string.ends_with(' ')
	}

	string.is_empty()
		|| need_quotes_spaces(string)
		|| string.starts_with(['&' , '*' , '?' , '|' , '-' , '!' , '%' , '@'])
		// Go's YAML library only quotes colons when followed by space (creates ambiguity)
		|| string.contains(": ")
		|| string.contains(|c| matches!(c, '{' | '}' | '[' | ']' | '#' | '`' | '\"' | '\'' | '\0'..='\x06' | '\t' | '\n' | '\r' | '\x0e'..='\x1a' | '\x1c'..='\x1f'))
		|| [
			// http://yaml.org/type/bool.html
			"yes", "Yes", "YES", "no", "No", "NO", "True", "TRUE", "true", "False", "FALSE", "false",
			"on", "On", "ON", "off", "Off", "OFF", // http://yaml.org/type/null.html
			"null", "Null", "NULL", "~",
			// > Quoted in std.jsonnet, however, in serde_yaml they were quoted:
			// > Note: 'y', 'Y', 'n', 'N', is not quoted deliberately, as in libyaml. PyYAML also parse
			// > them as string, not booleans, although it is violating the YAML 1.1 specification.
			// > See https://github.com/dtolnay/serde-yaml/pull/83#discussion_r152628088.
			"y", "Y", "n", "N",
			"-.inf", "+.inf", ".inf",
			"-", "---", ""
		].contains(&string)
		|| (string.chars().all(|c| matches!(c, '0'..='9' | '-'))
			&& string.chars().filter(|c| *c == '-').count() == 2)
		|| string.starts_with('.')
		|| string.starts_with("0x")
		|| string.parse::<i64>().is_ok()
		|| string.parse::<f64>().is_ok()
}

#[allow(dead_code)]
fn manifest_yaml_ex(val: &Val, options: &YamlFormat<'_>) -> Result<String> {
	let mut out = String::new();
	manifest_yaml_ex_buf(val, &mut out, &mut String::new(), options)?;
	Ok(out)
}

#[allow(clippy::too_many_lines)]
fn manifest_yaml_ex_buf(
	val: &Val,
	buf: &mut String,
	cur_padding: &mut String,
	options: &YamlFormat<'_>,
) -> Result<()> {
	match val {
		Val::Bool(v) => {
			if *v {
				buf.push_str("true");
			} else {
				buf.push_str("false");
			}
		}
		Val::Null => buf.push_str("null"),
		Val::Str(s) => {
			let s = s.clone().into_flat();
			if s.is_empty() {
				buf.push_str("\"\"");
			} else if let Some(s) = s.strip_suffix('\n') {
				buf.push('|');
				for line in s.split('\n') {
					buf.push('\n');
					buf.push_str(cur_padding);
					buf.push_str(&options.padding);
					buf.push_str(line);
				}
			} else if s.contains('\n') {
				buf.push_str("|-");
				for line in s.split('\n') {
					buf.push('\n');
					buf.push_str(cur_padding);
					buf.push_str(&options.padding);
					buf.push_str(line);
				}
			} else if !options.quote_keys && !yaml_needs_quotes(&s) {
				buf.push_str(&s);
			} else {
				escape_string_json_buf(&s, buf);
			}
		}
		Val::Num(n) => write!(buf, "{}", *n).unwrap(),
		#[cfg(feature = "exp-bigint")]
		Val::BigInt(n) => write!(buf, "{}", *n).unwrap(),
		Val::Arr(a) => {
			let mut had_items = false;
			for (i, item) in a.iter().enumerate() {
				had_items = true;
				let item = item.with_description(|| format!("elem <{i}> evaluation"))?;
				if i != 0 {
					buf.push('\n');
					buf.push_str(cur_padding);
				}
				buf.push('-');
				match &item {
					Val::Arr(a) if !a.is_empty() => {
						buf.push('\n');
						buf.push_str(cur_padding);
						buf.push_str(&options.padding);
					}
					_ => buf.push(' '),
				}
				let prev_len = cur_padding.len();
				match &item {
					// For arrays, add full padding
					Val::Arr(a) if !a.is_empty() => {
						cur_padding.push_str(&options.padding);
					}
					// For objects in arrays, only add 2 spaces to align after "- "
					Val::Obj(o) if !o.is_empty() => {
						cur_padding.push_str("  ");
					}
					_ => {}
				}
				in_description_frame(
					|| format!("elem <{i}> manifestification"),
					|| manifest_yaml_ex_buf(&item, buf, cur_padding, options),
				)?;
				cur_padding.truncate(prev_len);
			}
			if !had_items {
				buf.push_str("[]");
			}
		}
		Val::Obj(o) => {
			let mut had_fields = false;
			for (i, (key, value)) in o
				.iter(
					#[cfg(feature = "exp-preserve-order")]
					options.preserve_order,
				)
				.enumerate()
			{
				had_fields = true;
				let value = value.with_description(|| format!("field <{key}> evaluation"))?;
				if i != 0 {
					buf.push('\n');
					buf.push_str(cur_padding);
				}
				if !options.quote_keys && !yaml_needs_quotes(&key) {
					buf.push_str(&key);
				} else {
					escape_string_json_buf(&key, buf);
				}
				buf.push(':');
				let prev_len = cur_padding.len();
				match &value {
					Val::Arr(a) if !a.is_empty() => {
						buf.push('\n');
						buf.push_str(cur_padding);
						buf.push_str(&options.arr_element_padding);
						cur_padding.push_str(&options.arr_element_padding);
					}
					Val::Obj(o) if !o.is_empty() => {
						buf.push('\n');
						buf.push_str(cur_padding);
						buf.push_str(&options.padding);
						cur_padding.push_str(&options.padding);
					}
					_ => buf.push(' '),
				}
				in_description_frame(
					|| format!("field <{key}> manifestification"),
					|| manifest_yaml_ex_buf(&value, buf, cur_padding, options),
				)?;
				cur_padding.truncate(prev_len);
			}
			if !had_fields {
				buf.push_str("{}");
			}
		}
		Val::Func(_) => bail!("tried to manifest function"),
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use jrsonnet_evaluator::{val::NumValue, ObjValueBuilder};

	#[test]
	fn test_array_of_objects_indentation() {
		// Test that objects inside arrays have correct indentation
		// This is a regression test for the bug where object fields in arrays
		// were indented with full padding instead of 2 spaces after the dash
		let mut arr = Vec::new();

		// Create first object
		let mut obj1 = ObjValueBuilder::new();
		obj1.field("name").value("item1");
		obj1.field("value")
			.value(Val::Num(NumValue::new(100.0).unwrap()));
		arr.push(Val::Obj(obj1.build()));

		// Create second object
		let mut obj2 = ObjValueBuilder::new();
		obj2.field("name").value("item2");
		obj2.field("value")
			.value(Val::Num(NumValue::new(200.0).unwrap()));
		arr.push(Val::Obj(obj2.build()));

		// Create container object
		let mut container = ObjValueBuilder::new();
		container.field("objectArray").value(Val::Arr(arr.into()));

		let val = Val::Obj(container.build());

		let formatter = YamlFormat::cli(
			4,
			#[cfg(feature = "exp-preserve-order")]
			false,
		);
		let yaml = formatter.manifest(val).unwrap();

		// The YAML should have this exact format:
		// objectArray:
		//     - name: item1
		//       value: 100
		//     - name: item2
		//       value: 200
		//
		// Note: "value:" should be at 6 spaces (4 base + 2 for alignment after "- ")
		// NOT at 8 spaces (4 base + 4 padding)
		assert_eq!(
			yaml.trim_end(),
			"objectArray:\n    - name: item1\n      value: 100\n    - name: item2\n      value: 200"
		);

		// Verify the YAML can be parsed back
		use serde_yaml_with_quirks as serde_yaml;
		let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
		assert!(parsed.is_mapping());
	}

	#[test]
	fn test_nested_arrays_and_objects() {
		// Test more complex nesting scenarios
		let mut inner_obj = ObjValueBuilder::new();
		inner_obj.field("key").value("value");

		let mut arr = Vec::new();
		arr.push(Val::Num(NumValue::new(1.0).unwrap()));
		arr.push(Val::from("string"));
		arr.push(Val::Obj(inner_obj.build()));

		let mut container = ObjValueBuilder::new();
		container.field("mixedArray").value(Val::Arr(arr.into()));

		let val = Val::Obj(container.build());

		let formatter = YamlFormat::cli(
			4,
			#[cfg(feature = "exp-preserve-order")]
			false,
		);
		let yaml = formatter.manifest(val).unwrap();

		// Verify it can be parsed
		use serde_yaml_with_quirks as serde_yaml;
		let _parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
	}
}
