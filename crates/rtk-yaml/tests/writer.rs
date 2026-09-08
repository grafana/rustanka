use std::cmp::Ordering;
use std::io;

use rtk_yaml::{SerializerOptions, compare_string_keys, to_fmt_writer_with_options, to_io_writer};
use serde_json::json;

#[test]
fn tanka_presets_preserve_sequence_indentation_and_negative_zero() {
	let value = json!({"items": [{"value": -0.0}], "empty": {}});
	let mut output = String::new();
	to_fmt_writer_with_options(&mut output, &value, SerializerOptions::tanka_v2()).unwrap();
	assert_eq!(output, "items:\n- value: -0\nempty: {}\n");
	output.clear();
	to_fmt_writer_with_options(&mut output, &value, SerializerOptions::tanka_v3()).unwrap();
	assert_eq!(output, "items:\n    - value: -0.0\nempty: {}\n");
}

#[test]
fn natural_sort_matches_go_yaml_and_handles_long_numeric_keys() {
	for (left, right) in [("item2", "item10"), ("a", "a1"), ("item1", "item01")] {
		assert_eq!(compare_string_keys(left, right), Ordering::Less);
		assert_eq!(compare_string_keys(right, left), Ordering::Greater);
	}
	let left = "item99999999999999999999";
	let right = "item100000000000000000000";
	assert_eq!(
		compare_string_keys(left, right),
		compare_string_keys(right, left).reverse()
	);
}

#[test]
fn io_writer_preserves_the_underlying_error() {
	struct FailingWriter;
	impl io::Write for FailingWriter {
		fn write(&mut self, _: &[u8]) -> io::Result<usize> {
			Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed output"))
		}
		fn flush(&mut self) -> io::Result<()> {
			Ok(())
		}
	}
	let error = to_io_writer(&mut FailingWriter, &json!({"key": "value"})).unwrap_err();
	match error {
		rtk_yaml::Error::IO { error } => {
			assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
			assert_eq!(error.to_string(), "closed output");
		}
		other => panic!("expected original IO error, got {other}"),
	}
}
