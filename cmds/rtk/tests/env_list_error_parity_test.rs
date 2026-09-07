//! Env list error parity tests
//!
//! Verify that `rtk env list` errors out on the same inputs as `tk env list`,
//! matching tanka's behavior so that bad metadata evaluations surface to the
//! caller instead of being silently swallowed.
//!
//! Structure:
//! - test_fixtures/env_list_error_parity/<name>/
//!   - main.jsonnet (and other source files)
//!   - jsonnetfile.json (project root marker)
//!   - expected_error.txt (substring expected in both tk and rtk error output)

use std::{
	fs,
	path::PathBuf,
	process::{Command, Stdio},
};

use rtk::commands::env::list::{self, ListArgs};

fn fixtures_path(subpath: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.unwrap()
		.parent()
		.unwrap()
		.join("test_fixtures")
		.join(subpath)
}

fn discover_tests() -> Vec<(String, PathBuf, String)> {
	let dir = fixtures_path("env_list_error_parity");
	let mut tests = Vec::new();
	if !dir.exists() {
		return tests;
	}
	for entry in fs::read_dir(&dir).unwrap() {
		let entry = entry.unwrap();
		let path = entry.path();
		if !path.is_dir() {
			continue;
		}
		let expected = path.join("expected_error.txt");
		if !expected.exists() {
			continue;
		}
		let name = path.file_name().unwrap().to_string_lossy().to_string();
		let pattern = fs::read_to_string(&expected).unwrap().trim().to_string();
		tests.push((name, path, pattern));
	}
	tests.sort_by(|a, b| a.0.cmp(&b.0));
	tests
}

/// Run `tk env list .` from inside `env_path`. Returns Err(combined output)
/// if tk exits non-zero, Ok(combined output) on success.
fn run_tk_env_list(env_path: &PathBuf) -> Result<String, String> {
	let output = Command::new("tk")
		.args(["env", "list", "."])
		.current_dir(env_path)
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.output()
		.unwrap_or_else(|e| panic!("Failed to run tk: {e}. Is tk installed?"));
	let combined = format!(
		"{}{}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr),
	);
	if output.status.success() {
		Ok(combined)
	} else {
		Err(combined)
	}
}

fn run_rtk_env_list(env_path: &PathBuf) -> Result<String, String> {
	let mut buf = Vec::new();
	let args = ListArgs {
		path: Some(env_path.to_string_lossy().into_owned()),
		ext_code: Vec::new(),
		ext_str: Vec::new(),
		json: true,
		jsonnet_implementation: "go".to_owned(),
		max_stack: Some(500),
		names: false,
		selector: None,
		tla_code: Vec::new(),
		tla_str: Vec::new(),
	};
	match list::run(args, &mut buf) {
		Ok(()) => Ok(String::from_utf8_lossy(&buf).to_string()),
		Err(e) => Err(e.to_string()),
	}
}

fn run_test(env_path: &PathBuf, expected: &str) {
	match run_tk_env_list(env_path) {
		Ok(out) => panic!(
			"[tk] env list succeeded but expected failure containing {:?}\noutput:\n{}",
			expected, out
		),
		Err(err) => assert!(
			err.contains(expected),
			"[tk] error did not contain {:?}\nactual:\n{}",
			expected,
			err,
		),
	}

	match run_rtk_env_list(env_path) {
		Ok(out) => panic!(
			"[rtk] env list succeeded but expected failure containing {:?}\noutput:\n{}",
			expected, out
		),
		Err(err) => assert!(
			err.contains(expected),
			"[rtk] error did not contain {:?}\nactual:\n{}",
			expected,
			err,
		),
	}
}

#[test]
fn test_env_list_error_parity() {
	let tests = discover_tests();
	if tests.is_empty() {
		println!("No env_list_error_parity fixtures found");
		return;
	}

	let mut failures = Vec::new();
	for (name, path, expected) in &tests {
		println!("=== {name} ===");
		let result = std::panic::catch_unwind(|| run_test(path, expected));
		if let Err(e) = result {
			let msg = e
				.downcast_ref::<&str>()
				.map(|s| s.to_string())
				.or_else(|| e.downcast_ref::<String>().cloned())
				.unwrap_or_else(|| "Unknown panic".to_string());
			failures.push((name.clone(), msg));
		}
	}

	if !failures.is_empty() {
		let mut msg = format!("\n{} fixture(s) failed:\n", failures.len());
		for (name, m) in &failures {
			msg.push_str(&format!("\n=== {name} ===\n{m}\n"));
		}
		panic!("{msg}");
	}
}
