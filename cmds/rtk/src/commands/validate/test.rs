//! Validate test command handler.
//!
//! Runs `*_test.jsonnet` files that test the corresponding validation files.
//!
//! A test file `check_labels_test.jsonnet` corresponds to `check_labels.jsonnet`.
//! It must evaluate to an array of test cases:
//!
//! ```jsonnet
//! [
//!   {
//!     name: "test case name",
//!     // For manifestTest: a single manifest object
//!     // For namespaceTest: an array of manifest objects
//!     input: { apiVersion: "v1", kind: "ConfigMap", metadata: { name: "test" } },
//!     // Which function to test: "manifestTest" or "namespaceTest"
//!     testType: "manifestTest",
//!     // null if expecting the test to pass, or the expected error string
//!     expectedError: null,
//!   },
//! ]
//! ```

use std::{io::Write, path::PathBuf};

use anyhow::Result;
use clap::Args;

use super::common;

#[derive(Args)]
pub struct TestArgs {
	/// Directory containing Jsonnet validation and test files
	pub tests_dir: String,
}

/// A single test case from a _test.jsonnet file.
#[derive(Debug)]
struct TestCase {
	name: String,
	input: serde_json::Value,
	test_type: String,
	expected_error: Option<String>,
}

/// Result of running a single test case.
struct TestCaseResult {
	/// Name from the test case
	name: String,
	/// Pass or fail
	passed: bool,
	/// Detail message (only for failures)
	detail: Option<String>,
}

/// Run the validate test command.
pub fn run<W: Write>(args: TestArgs, mut writer: W) -> Result<()> {
	let tests_dir = PathBuf::from(&args.tests_dir);

	if !tests_dir.is_dir() {
		anyhow::bail!("tests directory does not exist: {}", tests_dir.display());
	}

	let test_files = common::collect_test_files(&tests_dir)?;
	if test_files.is_empty() {
		writeln!(
			writer,
			"No test files (*_test.jsonnet) found in {}",
			tests_dir.display()
		)?;
		return Ok(());
	}

	writeln!(writer, "Found {} test files", test_files.len())?;
	writeln!(writer)?;

	let mut total_passed = 0;
	let mut total_failed = 0;
	let mut all_results: Vec<(String, Vec<TestCaseResult>)> = Vec::new();

	for test_file in &test_files {
		let test_file_display = test_file
			.strip_prefix(&tests_dir)
			.unwrap_or(test_file)
			.to_string_lossy()
			.to_string();

		// Find corresponding validation file
		let validation_file = match common::find_validation_file_for_test(test_file) {
			Some(f) => f,
			None => {
				writeln!(
					writer,
					"SKIP  {}: no corresponding validation file found",
					test_file_display
				)?;
				continue;
			}
		};

		// Evaluate the test file to get the array of test cases
		let test_cases = match load_test_cases(test_file, &tests_dir) {
			Ok(cases) => cases,
			Err(e) => {
				writeln!(
					writer,
					"ERR   {}: failed to load test cases: {}",
					test_file_display, e
				)?;
				total_failed += 1;
				continue;
			}
		};

		if test_cases.is_empty() {
			writeln!(writer, "SKIP  {}: no test cases defined", test_file_display)?;
			continue;
		}

		// Run each test case
		let import_paths = vec![tests_dir.clone()];
		let mut file_results = Vec::new();

		for tc in &test_cases {
			let input_json = serde_json::to_string(&tc.input)?;

			let actual_result = common::run_validation_function(
				&validation_file,
				&tc.test_type,
				&input_json,
				&import_paths,
			);

			let result = match actual_result {
				Ok(actual_error) => match (&tc.expected_error, &actual_error) {
					// Both null: test passes (no error expected, no error returned)
					(None, None) => TestCaseResult {
						name: tc.name.clone(),
						passed: true,
						detail: None,
					},
					// Expected error matches actual error
					(Some(expected), Some(actual)) if expected == actual => TestCaseResult {
						name: tc.name.clone(),
						passed: true,
						detail: None,
					},
					// Expected null but got error
					(None, Some(actual)) => TestCaseResult {
						name: tc.name.clone(),
						passed: false,
						detail: Some(format!("expected no error, got: {}", actual)),
					},
					// Expected error but got null
					(Some(expected), None) => TestCaseResult {
						name: tc.name.clone(),
						passed: false,
						detail: Some(format!("expected error '{}', got no error", expected)),
					},
					// Expected different error
					(Some(expected), Some(actual)) => TestCaseResult {
						name: tc.name.clone(),
						passed: false,
						detail: Some(format!("expected error '{}', got: '{}'", expected, actual)),
					},
				},
				Err(e) => TestCaseResult {
					name: tc.name.clone(),
					passed: false,
					detail: Some(format!("evaluation error: {}", e)),
				},
			};

			if result.passed {
				total_passed += 1;
			} else {
				total_failed += 1;
			}
			file_results.push(result);
		}

		all_results.push((test_file_display, file_results));
	}

	// Print results
	for (file_display, results) in &all_results {
		for result in results {
			if result.passed {
				writeln!(writer, "PASS  {} :: {}", file_display, result.name)?;
			} else {
				writeln!(
					writer,
					"FAIL  {} :: {}: {}",
					file_display,
					result.name,
					result.detail.as_deref().unwrap_or("unknown")
				)?;
			}
		}
	}

	writeln!(writer)?;
	writeln!(
		writer,
		"Results: {} passed, {} failed, {} total",
		total_passed,
		total_failed,
		total_passed + total_failed
	)?;

	if total_failed > 0 {
		anyhow::bail!("{} test(s) failed", total_failed);
	}

	Ok(())
}

/// Load test cases from a _test.jsonnet file.
///
/// The file must evaluate to an array of objects with fields:
/// - `name` (string): test case name
/// - `input` (any): the data to pass to the validation function
/// - `testType` (string): "manifestTest" or "namespaceTest"
/// - `expectedError` (null or string): expected error, or null for no error expected
fn load_test_cases(
	test_file: &std::path::Path,
	tests_dir: &std::path::Path,
) -> Result<Vec<TestCase>> {
	let test_file_abs = test_file.canonicalize()?;

	let snippet = format!(
		"import '{}'",
		test_file_abs.to_string_lossy().replace('\\', "/"),
	);

	let value = common::eval_jsonnet_snippet(&snippet, &[tests_dir.to_path_buf()])?;

	let arr = value
		.as_array()
		.ok_or_else(|| anyhow::anyhow!("test file must evaluate to an array"))?;

	let mut cases = Vec::new();
	for (i, item) in arr.iter().enumerate() {
		let obj = item
			.as_object()
			.ok_or_else(|| anyhow::anyhow!("test case {} must be an object", i))?;

		let name = obj
			.get("name")
			.and_then(|v| v.as_str())
			.ok_or_else(|| anyhow::anyhow!("test case {} missing 'name' string field", i))?
			.to_string();

		let input = obj
			.get("input")
			.ok_or_else(|| anyhow::anyhow!("test case '{}' missing 'input' field", name))?
			.clone();

		let test_type = obj
			.get("testType")
			.and_then(|v| v.as_str())
			.ok_or_else(|| anyhow::anyhow!("test case '{}' missing 'testType' string field", name))?
			.to_string();

		if test_type != "manifestTest" && test_type != "namespaceTest" {
			anyhow::bail!(
				"test case '{}': testType must be 'manifestTest' or 'namespaceTest', got '{}'",
				name,
				test_type
			);
		}

		let expected_error = obj.get("expectedError").and_then(|v| {
			if v.is_null() {
				None
			} else {
				v.as_str().map(|s| s.to_string())
			}
		});

		cases.push(TestCase {
			name,
			input,
			test_type,
			expected_error,
		});
	}

	Ok(cases)
}
