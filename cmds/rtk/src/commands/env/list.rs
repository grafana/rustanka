//! Env list subcommand handler.

use std::io::Write;

use anyhow::Result;
use clap::Args;

use crate::{commands::common::UnimplementedArgs, environments};

#[derive(Args)]
pub struct ListArgs {
	/// Path to search for environments
	pub path: Option<String>,

	/// Set code value of extVar (Format: key=<code>)
	#[arg(long)]
	pub ext_code: Vec<String>,

	/// Set string value of extVar (Format: key=value)
	#[arg(short = 'V', long)]
	pub ext_str: Vec<String>,

	/// JSON output
	#[arg(long)]
	pub json: bool,

	/// Use `go` to use native go-jsonnet implementation and `binary:<path>` to delegate evaluation to a binary (with the same API as the regular `jsonnet` binary)
	#[arg(long, default_value = "go")]
	pub jsonnet_implementation: String,

	/// Jsonnet VM max stack. Increase this if you get: max stack frames exceeded
	#[arg(long, default_value = "500")]
	pub max_stack: i32,

	/// Plain names output
	#[arg(long)]
	pub names: bool,

	/// Label selector. Uses the same syntax as kubectl does
	#[arg(short = 'l', long)]
	pub selector: Option<String>,

	/// Set code value of top level function (Format: key=<code>)
	#[arg(long)]
	pub tla_code: Vec<String>,

	/// Set string value of top level function (Format: key=value)
	#[arg(short = 'A', long)]
	pub tla_str: Vec<String>,
}

/// Run the env list subcommand.
pub fn run<W: Write>(args: ListArgs, writer: W) -> Result<()> {
	UnimplementedArgs::warn_jsonnet_impl(&args.jsonnet_implementation);

	environments::list_envs_to_writer(
		args.path.as_deref().map(std::path::Path::new),
		args.json,
		writer,
	)
}

#[cfg(test)]
mod tests {
	use std::fs;

	use assert_matches::assert_matches;
	use tempfile::TempDir;

	use super::*;
	use crate::{commands::common::BrokenPipeGuard, test_utils::BrokenPipeWriter};

	fn make_args(path: Option<String>) -> ListArgs {
		ListArgs {
			path,
			ext_code: vec![],
			ext_str: vec![],
			json: false,
			jsonnet_implementation: "go".to_string(),
			max_stack: 500,
			names: false,
			selector: None,
			tla_code: vec![],
			tla_str: vec![],
		}
	}

	#[test]
	fn test_list_exits_cleanly_on_broken_pipe() {
		// Use an isolated fixture so the broken-pipe path doesn't pick up unrelated
		// (possibly-erroring) testdata environments under cargo's working directory.
		let tmp = TempDir::new().unwrap();
		fs::write(
			tmp.path().join("jsonnetfile.json"),
			r#"{"version":1,"dependencies":[],"legacyImports":true}"#,
		)
		.unwrap();
		let env_dir = tmp.path().join("env");
		fs::create_dir_all(&env_dir).unwrap();
		fs::write(
			env_dir.join("main.jsonnet"),
			r#"{
  apiVersion: 'tanka.dev/v1alpha1',
  kind: 'Environment',
  metadata: { name: 'test' },
  spec: { namespace: 'default' },
  data: {},
}"#,
		)
		.unwrap();

		let args = make_args(Some(tmp.path().to_string_lossy().into_owned()));
		// Wrap BrokenPipeWriter with BrokenPipeGuard to test the guard handles broken pipes
		let writer = BrokenPipeGuard::new(BrokenPipeWriter);
		let result = run(args, writer);

		// The command should exit cleanly on broken pipe, not panic or error
		assert_matches!(result, Ok(()));
	}
}
