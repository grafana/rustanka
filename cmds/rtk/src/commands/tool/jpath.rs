//! Jpath subcommand handler.

use std::{
	io::Write,
	path::{Path, PathBuf},
};

use anyhow::Result;
use clap::Args;

use crate::jsonnet::jpath::{self, JpathResult};

#[derive(Args)]
pub struct JpathArgs {
	/// File or directory
	pub path: PathBuf,

	/// Show debug info
	#[arg(short = 'd', long)]
	pub debug: bool,
}

/// Run the jpath subcommand.
///
/// Prints colon-separated `JSONNET_PATH` to stdout with no trailing newline,
/// matching `tk tool jpath`. `--debug` writes path details to stderr.
pub fn run<W: Write>(args: JpathArgs, mut writer: W) -> Result<()> {
	let path = abs_normalize(&args.path)?;
	std::fs::metadata(&path)
		.map_err(|err| anyhow::anyhow!("resolving JPATH: {}: {err}", path.display()))?;

	let result = jpath::resolve(&path).map_err(|err| anyhow::anyhow!("resolving JPATH: {err}"))?;
	let jsonnet_path = result.jsonnet_path();

	if args.debug {
		eprint!("{}", format_debug(&result));
	}

	write!(writer, "{}", join_jsonnet_path(&jsonnet_path))?;
	Ok(())
}

fn abs_normalize(path: &Path) -> Result<PathBuf> {
	let abs = if path.is_absolute() {
		path.to_path_buf()
	} else {
		std::env::current_dir()?.join(path)
	};
	Ok(jpath::normalize_path(&abs))
}

fn join_jsonnet_path(paths: &[PathBuf]) -> String {
	paths
		.iter()
		.map(|p| p.display().to_string())
		.collect::<Vec<_>>()
		.join(":")
}

fn format_go_slice(paths: &[PathBuf]) -> String {
	format!(
		"[{}]",
		paths
			.iter()
			.map(|p| p.display().to_string())
			.collect::<Vec<_>>()
			.join(" ")
	)
}

fn format_debug(result: &JpathResult) -> String {
	let jsonnet_path = result.jsonnet_path();
	format!(
		"main: {}\nrootDir: {}\nbaseDir: {}\njpath: {}\n",
		result.entrypoint.display(),
		result.root.display(),
		result.base.display(),
		format_go_slice(&jsonnet_path),
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn testdata(name: &str) -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("testdata/jpath")
			.join(name)
	}

	fn run_stdout(path: impl AsRef<Path>, debug: bool) -> Result<String> {
		let mut out = Vec::new();
		run(
			JpathArgs {
				path: path.as_ref().to_path_buf(),
				debug,
			},
			&mut out,
		)?;
		Ok(String::from_utf8(out).unwrap())
	}

	fn last_colon_segment(out: &str) -> &str {
		out.rsplit(':').next().unwrap()
	}

	#[test]
	fn test_jpath_stdout_is_colon_separated_without_newline() {
		let env = testdata("valid/environments/default");
		let out = run_stdout(&env, false).expect("jpath should succeed");

		assert!(
			!out.ends_with('\n'),
			"tk tool jpath has no trailing newline"
		);
		let parts: Vec<&str> = out.split(':').collect();
		assert_eq!(parts.len(), 4);

		let root = testdata("valid");
		assert_eq!(
			parts,
			vec![
				root.join("vendor").to_str().unwrap(),
				env.join("vendor").to_str().unwrap(),
				root.join("lib").to_str().unwrap(),
				env.to_str().unwrap(),
			]
		);
	}

	#[test]
	fn test_jpath_debug_format() {
		let env = testdata("valid/environments/default");
		let result = jpath::resolve(&env).unwrap();
		let debug = format_debug(&result);
		let root = testdata("valid");

		assert_eq!(
			debug,
			format!(
				"main: {}\nrootDir: {}\nbaseDir: {}\njpath: [{}]\n",
				env.join("main.jsonnet").display(),
				root.display(),
				env.display(),
				[
					root.join("vendor").display().to_string(),
					env.join("vendor").display().to_string(),
					root.join("lib").display().to_string(),
					env.display().to_string(),
				]
				.join(" ")
			)
		);
	}

	#[test]
	fn test_jpath_nested_dir_resolves_to_env_base() {
		let nested = testdata("valid/environments/default/nestedDir");
		let out = run_stdout(&nested, false).expect("jpath from nested dir should succeed");
		let env = testdata("valid/environments/default");
		assert_eq!(last_colon_segment(&out), env.to_str().unwrap());
	}

	#[test]
	fn jpath_cleans_missing_parent_dir() {
		let env = testdata("valid/environments/default");
		let messy = env.join("does-not-exist").join("..");
		let out = run_stdout(&messy, false).expect("jpath should clean missing/.. like tk");
		assert_eq!(last_colon_segment(&out), env.to_str().unwrap());
	}

	#[test]
	fn test_jpath_missing_path() {
		let err = run_stdout("/nonexistent/rtk-jpath-does-not-exist", false).unwrap_err();
		assert!(err.to_string().contains("resolving JPATH"));
	}

	#[test]
	fn test_jpath_no_base() {
		let err = run_stdout(testdata("noBase/environments/empty"), false).unwrap_err();
		assert!(err.to_string().contains("resolving JPATH"));
		assert!(err.to_string().contains("could not find environment base"));
	}

	#[test]
	fn test_jpath_no_root() {
		let temp = tempfile::TempDir::new().unwrap();
		std::fs::create_dir_all(temp.path().join("environments/default")).unwrap();
		std::fs::write(temp.path().join("environments/default/main.jsonnet"), "{}").unwrap();
		let err = run_stdout(temp.path().join("environments/default"), false).unwrap_err();
		assert!(err.to_string().contains("resolving JPATH"));
		assert!(err.to_string().contains("could not find project root"));
	}

	#[test]
	fn test_jpath_file_path() {
		let main = testdata("valid/environments/default/main.jsonnet");
		let out = run_stdout(&main, false).expect("jpath on main.jsonnet should succeed");
		let env = testdata("valid/environments/default");
		assert_eq!(last_colon_segment(&out), env.to_str().unwrap());
	}
}
