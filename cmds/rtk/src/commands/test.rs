//! Run Jsonnet assertion files and jsonnetunit suites.

use std::{
	io::Write,
	num::NonZeroUsize,
	path::{Path, PathBuf},
};

use anyhow::{ensure, Context, Result};
use clap::Args;
use rayon::prelude::*;
use walkdir::WalkDir;

#[derive(Args)]
pub struct TestArgs {
	/// Files or directories containing *_test.jsonnet and *_test.libsonnet
	#[arg(default_value = ".")]
	pub paths: Vec<PathBuf>,

	/// Search directories recursively (skips hidden directories, vendor and target)
	#[arg(short, long)]
	pub recursive: bool,

	/// Number of parallel evaluation workers (defaults to available CPUs)
	#[arg(short = 'p', long, alias = "jobs")]
	pub parallel: Option<NonZeroUsize>,

	/// Additional Jsonnet import search paths, in precedence order
	#[arg(short = 'J', long = "jpath")]
	pub jpaths: Vec<PathBuf>,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,
}

impl TestArgs {
	fn discover(&self) -> Result<Vec<PathBuf>> {
		let mut files = Vec::new();
		for path in &self.paths {
			if path.is_file() {
				files.push(path.canonicalize()?);
				continue;
			}
			let depth = if self.recursive { usize::MAX } else { 1 };
			for entry in WalkDir::new(path)
				.max_depth(depth)
				.into_iter()
				.filter_entry(|entry| {
					entry.depth() == 0 || !entry.file_type().is_dir() || {
						let name = entry.file_name().to_string_lossy();
						!name.starts_with('.') && name != "vendor" && name != "target"
					}
				}) {
				let entry = entry.with_context(|| format!("searching {}", path.display()))?;
				if entry.file_type().is_file() && is_test_file(entry.path()) {
					files.push(entry.path().canonicalize()?);
				}
			}
		}
		files.sort();
		files.dedup();
		Ok(files)
	}
}

fn is_test_file(path: &Path) -> bool {
	path.file_name()
		.and_then(|name| name.to_str())
		.is_some_and(|name| name.ends_with("_test.jsonnet") || name.ends_with("_test.libsonnet"))
}

struct Runner {
	engine: rtk_jsonnet::Engine,
	jpaths: Vec<PathBuf>,
}

impl Runner {
	fn evaluate(&self, path: &Path) -> Result<()> {
		let mut imports = self.jpaths.clone();
		let options = match rtk_jsonnet::jpath::JPath::resolve(path) {
			Ok(project) => {
				let options = self
					.engine
					.options()
					.for_project(&project)
					.map_err(|error| anyhow::anyhow!(error.to_string()))?;
				imports.extend(project.import_paths);
				options
			}
			Err(rtk_jsonnet::jpath::Error::CouldNotFindRoot { .. }) => {
				self.engine.options().clone()
			}
			Err(error) => return Err(error.into()),
		};
		// Even standalone files need a filesystem resolver for relative imports.
		imports.push(
			path.parent()
				.context("test file has no parent directory")?
				.to_path_buf(),
		);
		let mut evaluator = self.engine.create_evaluator();
		options
			.apply(&mut evaluator)
			.map_err(|error| anyhow::anyhow!(error.to_string()))?;
		evaluator
			.with_import_paths(imports)
			.map_err(|error| anyhow::anyhow!(error.to_string()))?;
		// Force every visible field so assertions outside the suite summary also run.
		let value: serde_json::Value = evaluator
			.evaluate_file(path)
			.map_err(|error| anyhow::anyhow!(error.to_string()))?
			.into_value()
			.deserialize()
			.map_err(|error| anyhow::anyhow!(error.to_string()))?;
		ensure!(
			value.get("allTestsPassed") != Some(&serde_json::Value::Bool(false)),
			"allTestsPassed=false\n{}",
			serde_json::to_string_pretty(&value)?
		);
		Ok(())
	}
}

pub fn run<W: Write>(args: TestArgs, mut writer: W) -> Result<i32> {
	let files = args.discover()?;
	let pool = rayon::ThreadPoolBuilder::new()
		.num_threads(args.parallel.map_or(0, NonZeroUsize::get))
		// Jsonnet evaluation recurses deeply, beyond the default Rayon stack.
		.stack_size(8 * 1024 * 1024)
		.build()?;
	let runner = Runner {
		engine: rtk_jsonnet::Engine::new(args.jsonnet.into_options()),
		jpaths: args.jpaths,
	};
	let results: Vec<_> = pool.install(|| {
		files
			.par_iter()
			.map(|path| runner.evaluate(path).map_err(|error| format!("{error:#}")))
			.collect()
	});
	let mut failed = 0;
	for (path, result) in files.iter().zip(results) {
		match result {
			Ok(()) => writeln!(writer, "PASS {}", path.display())?,
			Err(error) => {
				failed += 1;
				writeln!(writer, "FAIL {}\n{error}", path.display())?;
			}
		}
	}
	writeln!(writer, "{} passed, {failed} failed", files.len() - failed)?;
	Ok(i32::from(failed != 0))
}

#[cfg(test)]
mod tests {
	use super::*;
	use clap::Parser;

	#[derive(Parser)]
	struct Cli {
		#[command(flatten)]
		args: TestArgs,
	}

	fn args(path: &Path, flags: &[&str]) -> TestArgs {
		Cli::parse_from(
			std::iter::once("test")
				.chain(std::iter::once(path.to_str().unwrap()))
				.chain(flags.iter().copied()),
		)
		.args
	}

	#[test]
	fn discovery_is_sorted_and_deduplicated() {
		let dir = tempfile::tempdir().unwrap();
		for name in [
			"a_test.jsonnet",
			"b_test.libsonnet",
			"nested/c_test.jsonnet",
			"vendor/skip_test.jsonnet",
			".git/skip_test.jsonnet",
			"target/skip_test.jsonnet",
			"not_a_test.json",
		] {
			let path = dir.path().join(name);
			std::fs::create_dir_all(path.parent().unwrap()).unwrap();
			std::fs::write(path, "{}").unwrap();
		}
		let mut all = args(dir.path(), &["-r"]);
		all.paths.push(dir.path().join("a_test.jsonnet"));
		let files = all.discover().unwrap();
		assert_eq!(files.len(), 3);
		assert_eq!(args(dir.path(), &[]).discover().unwrap().len(), 2);
		assert_eq!(
			files,
			[
				"a_test.jsonnet",
				"b_test.libsonnet",
				"nested/c_test.jsonnet"
			]
			.map(|name| dir.path().join(name).canonicalize().unwrap())
		);
	}

	#[test]
	fn evaluates_all_files_and_reports_failures() {
		let dir = tempfile::tempdir().unwrap();
		for (name, code) in [
			("assert_test.jsonnet", "assert true; {}"),
			("pass_test.jsonnet", "{allTestsPassed: true}"),
			(
				"suite_test.libsonnet",
				"{allTestsPassed: false, failure: 'expected 1, got 2'}",
			),
			(
				"lazy_test.jsonnet",
				"{allTestsPassed: true, result: error 'forced failure'}",
			),
			("syntax_test.jsonnet", "{"),
		] {
			std::fs::write(dir.path().join(name), code).unwrap();
		}
		let mut output = Vec::new();
		assert_eq!(run(args(dir.path(), &["-p", "2"]), &mut output).unwrap(), 1);
		let report = String::from_utf8(output).unwrap();
		assert!(report.contains("2 passed, 3 failed"), "{report}");
		assert!(report.contains("expected 1, got 2"));
		assert!(report.contains("forced failure"));
	}

	#[test]
	fn explicit_files_imports_and_external_variables() {
		let dir = tempfile::tempdir().unwrap();
		let imports = dir.path().join("imports");
		std::fs::create_dir(&imports).unwrap();
		std::fs::write(imports.join("value.libsonnet"), "42").unwrap();
		let file = dir.path().join("custom.jsonnet");
		std::fs::write(&file, "assert (import 'value.libsonnet') == 42; assert std.extVar('value') == 'ok'; {allTestsPassed: true}").unwrap();
		assert_eq!(
			run(
				args(&file, &["-J", imports.to_str().unwrap(), "-V", "value=ok"]),
				Vec::new()
			)
			.unwrap(),
			0
		);
	}

	#[test]
	fn project_imports_work_without_flags() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("jsonnetfile.json"), "{}").unwrap();
		std::fs::create_dir(dir.path().join("lib")).unwrap();
		std::fs::create_dir(dir.path().join("tests")).unwrap();
		std::fs::write(dir.path().join("lib/value.libsonnet"), "42").unwrap();
		std::fs::write(
			dir.path().join("tests/project_test.jsonnet"),
			"assert (import 'value.libsonnet') == 42; {}",
		)
		.unwrap();
		assert_eq!(run(args(dir.path(), &["-r"]), Vec::new()).unwrap(), 0);
	}

	#[test]
	fn empty_selections_succeed_but_missing_paths_fail() {
		let dir = tempfile::tempdir().unwrap();
		assert_eq!(run(args(dir.path(), &[]), Vec::new()).unwrap(), 0);
		assert!(run(args(&dir.path().join("missing"), &[]), Vec::new()).is_err());
	}
}
