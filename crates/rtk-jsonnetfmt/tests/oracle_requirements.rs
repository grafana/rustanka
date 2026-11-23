//! Coverage must fail when required Go output or saved inputs disappear.
mod oracle_support;

#[test]
fn missing_malformed_and_empty_oracles_fail_closed() {
	let temporary = tempfile::tempdir().unwrap();
	let path = temporary.path().join("required.json");
	assert!(
		std::panic::catch_unwind(|| oracle_support::read_saved::<serde_json::Value>(&path))
			.is_err()
	);
	for content in [b"".as_slice(), b"not json", b"[]", b"null", b"{}"] {
		std::fs::write(&path, content).unwrap();
		assert!(
			std::panic::catch_unwind(|| oracle_support::read_saved::<serde_json::Value>(&path))
				.is_err(),
			"invalid oracle unexpectedly passed: {content:?}"
		);
	}
	assert!(
		std::panic::catch_unwind(|| oracle_support::read_saved::<serde_json::Value>(
			temporary.path()
		))
		.is_err()
	);
}

#[test]
fn small_saved_node_regressions_are_required() {
	let name = "node-regression-oracle.json";
	let cases = oracle_support::read::<serde_json::Value>(name);
	let source_key = "source";
	let mut sources = std::collections::BTreeSet::new();
	for case in cases {
		let name = case[source_key]
			.as_str()
			.expect("saved regression has a source");
		assert!(
			sources.insert(name.to_owned()),
			"duplicate saved regression: {name}"
		);
		assert!(
			case.get("entries").is_some()
				|| case.get("passes").is_some()
				|| case.get("error").is_some(),
			"saved regression contains no comparison: {name}"
		);
	}
}

#[test]
fn missing_go_fails_runtime_generation_without_writing_an_answer() {
	use std::process::Command;
	let python = Command::new("python3")
		.args(["-c", "import sys; print(sys.executable)"])
		.output()
		.expect("mandatory oracle generation requires Python");
	assert!(python.status.success());
	let python = String::from_utf8(python.stdout).unwrap();
	let temporary = tempfile::tempdir().unwrap();
	let output = temporary.path().join("answer.json");
	let generator =
		std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/generate/ast_oracle.py");
	let kind = "node";
	let result = Command::new(python.trim())
		.arg(&generator)
		.args(["--kind", kind, "--corpus-output"])
		.arg(&output)
		.env("PATH", temporary.path())
		.output()
		.unwrap();
	assert!(
		!result.status.success(),
		"missing Go silently passed for {kind}"
	);
	assert!(!output.exists(), "failed generator wrote an answer");
	assert!(String::from_utf8_lossy(&result.stderr).contains("go"));
}
