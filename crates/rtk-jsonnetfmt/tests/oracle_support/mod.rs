//! Mandatory live Go comparisons and fail-closed saved regression reads.
#![allow(dead_code)]
use serde::de::DeserializeOwned;
use std::{
	path::{Path, PathBuf},
	process::Command,
	sync::OnceLock,
};

pub fn decode<T: DeserializeOwned>(bytes: &[u8], label: &str) -> Vec<T> {
	let cases: Vec<T> = serde_json::from_slice(bytes)
		.unwrap_or_else(|error| panic!("invalid required oracle {label}: {error}"));
	assert!(!cases.is_empty(), "required oracle {label} is empty");
	cases
}

pub fn read_saved<T: DeserializeOwned>(path: &Path) -> Vec<T> {
	let bytes = std::fs::read(path)
		.unwrap_or_else(|error| panic!("cannot read required oracle {}: {error}", path.display()));
	decode(&bytes, &path.display().to_string())
}

fn generate(kind: &str, corpus: bool) -> Vec<u8> {
	let temporary = tempfile::tempdir().expect("create temporary Go oracle directory");
	let output = temporary.path().join("oracle.json");
	let generator = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/generate/ast_oracle.py");
	let result = Command::new("python3")
		.arg(generator)
		.args(["--kind", kind])
		.arg(if corpus {
			"--corpus-output"
		} else {
			"--snippets-output"
		})
		.arg(&output)
		.output()
		.expect("mandatory Go AST comparisons require python3 and Go on PATH");
	assert!(
		result.status.success(),
		"mandatory {kind} Go comparison generation failed:\n{}\n{}",
		String::from_utf8_lossy(&result.stdout),
		String::from_utf8_lossy(&result.stderr)
	);
	std::fs::read(output).expect("Go generator did not write its required oracle output")
}

pub fn read<T: DeserializeOwned>(name: &str) -> Vec<T> {
	static NODE: OnceLock<Vec<u8>> = OnceLock::new();
	static NODE_SNIPPETS: OnceLock<Vec<u8>> = OnceLock::new();
	static PASS: OnceLock<Vec<u8>> = OnceLock::new();
	static PASS_SNIPPETS: OnceLock<Vec<u8>> = OnceLock::new();
	let runtime = match name {
		"node-oracle.json" => Some(NODE.get_or_init(|| generate("node", true))),
		"node-snippet-oracle.json" => Some(NODE_SNIPPETS.get_or_init(|| generate("node", false))),
		"pass-oracle.json" => Some(PASS.get_or_init(|| generate("pass", true))),
		"pass-snippet-oracle.json" => Some(PASS_SNIPPETS.get_or_init(|| generate("pass", false))),
		_ => None,
	};
	if let Some(bytes) = runtime {
		decode(bytes, name)
	} else {
		read_saved(
			&PathBuf::from(env!("CARGO_MANIFEST_DIR"))
				.join("testdata")
				.join(name),
		)
	}
}
