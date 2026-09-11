//! Init command handler.

use std::{
	fs,
	io::Write,
	path::Path,
	process::{Command, Stdio},
};

use anyhow::{Context, Result};
use clap::Args;

use super::env::shared::{self, EnvSpecOptions};

const DEFAULT_K8S_VERSION: &str = "1.32";
const K8S_LIBSONNET_COMMIT_REF: &str = "55380470fb7979e6ce0c4316cb9c27a266caf298";

#[derive(Args)]
pub struct InitArgs {
	/// Ignore the working directory not being empty
	#[arg(short = 'f', long)]
	pub force: bool,

	/// Create an inline environment
	#[arg(short = 'i', long)]
	pub inline: bool,

	/// Choose the version of k8s-libsonnet, a package URI, or false to skip
	#[arg(long, default_value = DEFAULT_K8S_VERSION)]
	pub k8s: String,
}

/// Run the init command.
pub fn run<W: Write>(args: InitArgs, writer: W) -> Result<()> {
	let directory = std::env::current_dir().context("current directory")?;
	init(&directory, &args, writer)
}

fn init<W: Write>(directory: &Path, args: &InitArgs, mut writer: W) -> Result<()> {
	let mut entries = fs::read_dir(directory)
		.with_context(|| format!("error listing files in {}", directory.display()))?;
	if entries.next().is_some() && !args.force {
		anyhow::bail!("error: directory not empty. Use `-f` to force");
	}

	write_new_file(&directory.join("jsonnetfile.json"), "{}")
		.context("error creating `jsonnetfile.json`")?;
	fs::create_dir(directory.join("vendor")).context("error creating `vendor/` folder")?;
	fs::create_dir(directory.join("lib")).context("error creating `lib/` folder")?;

	shared::add(
		&directory.join("environments/default"),
		args.inline,
		&EnvSpecOptions {
			namespace: None,
			server: None,
			server_from_context: None,
			context_name: Vec::new(),
			diff_strategy: None,
			inject_labels: None,
		},
	)?;

	let (install_k8s, version) = match parse_bool(&args.k8s) {
		Some(value) => (value, DEFAULT_K8S_VERSION),
		None => (true, args.k8s.as_str()),
	};
	let install_error = install_k8s
		.then(|| install_k8s_lib(directory, version))
		.and_then(Result::err);
	if let Some(error) = &install_error {
		writeln!(writer, "Installing k.libsonnet: {error}")?;
	}

	if args.inline {
		writeln!(
			writer,
			"Directory structure set up! Remember to configure the API endpoint in environments/default/main.jsonnet"
		)?;
	} else {
		writeln!(
			writer,
			"Directory structure set up! Remember to configure the API endpoint:\n`tk env set environments/default --server=https://127.0.0.1:6443`"
		)?;
	}
	if install_error.is_some() {
		writeln!(
			writer,
			"Errors occurred while initializing the project. Check the above logs for details."
		)?;
	}
	Ok(())
}

fn install_k8s_lib(directory: &Path, version: &str) -> Result<()> {
	let jb = jb_binary(std::env::var_os("TANKA_JB_PATH"));
	if !executable_exists(&jb) {
		anyhow::bail!(
			"jsonnet-bundler not found in $PATH. Follow https://tanka.dev/install#jsonnet-bundler for installation instructions"
		);
	}
	let package = k8s_package(version);
	let import_path = package
		.split_once('@')
		.map_or(package.as_str(), |(path, _)| path);
	write_new_file(
		&directory.join("lib/k.libsonnet"),
		&format!("import '{import_path}/main.libsonnet'\n"),
	)?;

	let status = Command::new(jb)
		.current_dir(directory)
		.args([
			"install",
			&package,
			"github.com/grafana/jsonnet-libs/ksonnet-util",
			"github.com/jsonnet-libs/docsonnet/doc-util",
		])
		.stdin(Stdio::inherit())
		.stdout(Stdio::inherit())
		.stderr(Stdio::inherit())
		.status()
		.context("jsonnet-bundler not found in $PATH. Follow https://tanka.dev/install#jsonnet-bundler for installation instructions")?;
	if !status.success() {
		anyhow::bail!("jsonnet-bundler exited with {status}");
	}
	Ok(())
}

fn jb_binary(configured: Option<std::ffi::OsString>) -> std::ffi::OsString {
	configured
		.filter(|path| !path.is_empty())
		.unwrap_or_else(|| "jb".into())
}

fn executable_exists(executable: &std::ffi::OsStr) -> bool {
	let path = Path::new(executable);
	if path.components().count() > 1 {
		return is_executable(path);
	}
	std::env::var_os("PATH").is_some_and(|paths| {
		std::env::split_paths(&paths).any(|directory| is_executable(&directory.join(path)))
	})
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
	use std::os::unix::fs::PermissionsExt;
	path.metadata()
		.is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
	path.is_file()
}

fn parse_bool(value: &str) -> Option<bool> {
	match value {
		"1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
		"0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
		_ => None,
	}
}

fn k8s_package(version: &str) -> String {
	let mut package = if version.contains('/') {
		version.to_owned()
	} else {
		format!("github.com/jsonnet-libs/k8s-libsonnet/{version}")
	};
	if !package.contains('@') {
		package.push('@');
		package.push_str(K8S_LIBSONNET_COMMIT_REF);
	}
	package
}

fn write_new_file(path: &Path, contents: &str) -> Result<()> {
	if !path.exists() {
		fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn args(inline: bool) -> InitArgs {
		InitArgs {
			force: false,
			inline,
			k8s: "false".to_owned(),
		}
	}

	#[test]
	fn initializes_static_project() {
		let directory = tempfile::tempdir().unwrap();
		let mut output = Vec::new();
		init(directory.path(), &args(false), &mut output).unwrap();

		assert_eq!(
			fs::read_to_string(directory.path().join("jsonnetfile.json")).unwrap(),
			"{}"
		);
		assert!(directory.path().join("vendor").is_dir());
		assert_eq!(
			fs::read_to_string(directory.path().join("environments/default/main.jsonnet")).unwrap(),
			"{}\n"
		);
		assert!(directory
			.path()
			.join("environments/default/spec.json")
			.is_file());
		assert!(String::from_utf8(output).unwrap().contains("tk env set"));
	}

	#[test]
	fn initializes_inline_project() {
		let directory = tempfile::tempdir().unwrap();
		init(directory.path(), &args(true), Vec::new()).unwrap();

		let main =
			fs::read_to_string(directory.path().join("environments/default/main.jsonnet")).unwrap();
		assert!(main.contains("kind: 'Environment'"));
		assert!(!directory
			.path()
			.join("environments/default/spec.json")
			.exists());
	}

	#[test]
	fn refuses_non_empty_directory() {
		let directory = tempfile::tempdir().unwrap();
		fs::write(directory.path().join("existing"), "keep").unwrap();
		let error = init(directory.path(), &args(false), Vec::new()).unwrap_err();
		assert_eq!(
			error.to_string(),
			"error: directory not empty. Use `-f` to force"
		);
	}

	#[test]
	fn builds_default_and_explicit_packages() {
		assert_eq!(
			k8s_package("1.32"),
			format!("github.com/jsonnet-libs/k8s-libsonnet/1.32@{K8S_LIBSONNET_COMMIT_REF}")
		);
		assert_eq!(
			k8s_package("github.com/jsonnet-libs/k8s-libsonnet/1.31@main"),
			"github.com/jsonnet-libs/k8s-libsonnet/1.31@main"
		);
	}

	#[test]
	fn accepts_the_boolean_spellings_tk_accepts() {
		for value in ["1", "t", "T", "TRUE", "true", "True"] {
			assert_eq!(parse_bool(value), Some(true));
		}
		for value in ["0", "f", "F", "FALSE", "false", "False"] {
			assert_eq!(parse_bool(value), Some(false));
		}
		assert_eq!(parse_bool("1.32"), None);
	}

	#[test]
	fn empty_jb_override_uses_path_lookup() {
		assert_eq!(jb_binary(Some("".into())), "jb");
		assert_eq!(jb_binary(Some("/opt/bin/jb".into())), "/opt/bin/jb");
	}
}
