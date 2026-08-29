//! Imports subcommand handler.
//!
//! Lists all transitive imports of an environment.

use std::{
	io::Write,
	path::{Path, PathBuf},
	process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json;

use crate::jsonnet::{imports as imports_impl, jpath};

/// Exit code when an imported file changed (matches tk behavior).
pub const EXIT_CODE_REBUILD_REQUIRED: i32 = 16;

#[derive(Args)]
pub struct ImportsArgs {
	/// Path to the environment (directory or main.jsonnet file)
	pub path: PathBuf,

	/// Git commit hash to check against
	#[arg(short = 'c', long)]
	pub check: Option<String>,
}

/// Run the imports subcommand.
///
/// Lists all files that are transitively imported by the environment's main.jsonnet.
/// Output is a JSON array of file paths (matching tk's output format).
pub fn run<W: Write>(args: ImportsArgs, mut writer: W) -> Result<bool> {
	let check = args.check.as_deref().filter(|commit| !commit.is_empty());
	let changed_files = check
		.map(git_changed_files)
		.transpose()
		.context("invoking git")?;

	let imports = imports_impl::transitive_imports(&args.path.to_string_lossy())?;
	if let (Some(commit), Some(changed_files)) = (check, changed_files) {
		let git_root = git_root().context("invoking git")?;
		let environment_path = std::fs::canonicalize(&args.path).context("loading environment")?;
		let environment_root = jpath::resolve(environment_path)?.root;

		if let Some(import) = changed_import(&imports, &environment_root, &git_root, &changed_files)
		{
			writeln!(
				writer,
				"Rebuild required. File `{}` imports `{}`, which has been changed in `{}`.",
				args.path.display(),
				import,
				commit
			)?;
			return Ok(true);
		}

		writeln!(
			writer,
			"Rebuild not required, because no imported files have been changed in `{}`.",
			commit
		)?;
		return Ok(false);
	}

	// Output as JSON array (matching tk's output format)
	let json = serde_json::to_string(&imports)?;
	writeln!(writer, "{}", json)?;
	Ok(false)
}

fn changed_import<'a>(
	imports: &'a [String],
	environment_root: &Path,
	git_root: &Path,
	changed_files: &[String],
) -> Option<&'a str> {
	changed_files.iter().find_map(|changed| {
		let changed = git_root.join(changed);
		imports
			.iter()
			.find(|import| environment_root.join(import) == changed)
			.map(String::as_str)
	})
}

fn git_root() -> Result<PathBuf> {
	Ok(PathBuf::from(git(&["rev-parse", "--show-toplevel"])?))
}

fn git_changed_files(commit: &str) -> Result<Vec<String>> {
	Ok(
		git(&["diff-tree", "--no-commit-id", "--name-only", "-r", commit])?
			.lines()
			.map(str::to_owned)
			.collect(),
	)
}

fn git(args: &[&str]) -> Result<String> {
	let output = Command::new("git")
		.args(args)
		.stderr(Stdio::inherit())
		.output()
		.context("running git")?;
	if !output.status.success() {
		bail!("git exited with {}", output.status);
	}

	String::from_utf8(output.stdout)
		.context("reading git output")
		.map(|output| output.trim_end_matches('\n').to_owned())
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::*;

	fn test_root() -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/importTree")
	}

	#[test]
	fn test_imports_command_output() {
		let args = ImportsArgs {
			path: test_root(),
			check: None,
		};
		let mut output = Vec::new();

		let rebuild_required = run(args, &mut output).expect("imports should succeed");

		let output_str = String::from_utf8(output).unwrap();
		// Output is JSON array format (matching tk)
		let imports: Vec<String> = serde_json::from_str(output_str.trim()).unwrap();

		assert_eq!(
			imports,
			vec![
				"main.jsonnet",
				"trees.jsonnet",
				"trees/apple.jsonnet",
				"trees/cherry.jsonnet",
				"trees/generic.libsonnet",
				"trees/peach.jsonnet",
			]
		);
		assert!(!rebuild_required);
	}

	#[test]
	fn test_changed_import_returns_first_changed_dependency() {
		let imports = vec!["main.jsonnet".to_owned(), "trees/apple.jsonnet".to_owned()];
		let changed = vec![
			"unrelated.txt".to_owned(),
			"env/trees/apple.jsonnet".to_owned(),
		];

		assert_eq!(
			changed_import(
				&imports,
				Path::new("/repo/env"),
				Path::new("/repo"),
				&changed,
			),
			Some("trees/apple.jsonnet")
		);
	}

	#[test]
	fn test_changed_import_returns_none_for_unrelated_files() {
		let imports = vec!["main.jsonnet".to_owned()];
		let changed = vec!["README.md".to_owned()];

		assert_eq!(
			changed_import(
				&imports,
				Path::new("/repo/env"),
				Path::new("/repo"),
				&changed,
			),
			None
		);
	}
}
