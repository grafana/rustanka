//! Env list subcommand handler.

use std::{io::Write, path::PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use rtk_environments::export::LabelSelector;
use tabwriter::TabWriter;

use crate::commands::common::{JsonnetArgs, UnimplementedArgs};

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

impl ListArgs {
	fn jsonnet_options(&self) -> Result<rtk_jsonnet::Options> {
		fn values(values: &[String]) -> Result<rustc_hash::FxHashMap<Box<str>, Box<str>>> {
			values
				.iter()
				.map(|value| JsonnetArgs::parse_key_value(value).map_err(anyhow::Error::msg))
				.collect()
		}

		Ok(rtk_jsonnet::Options {
			ext_code: values(&self.ext_code)?,
			ext_variables: values(&self.ext_str)?,
			top_level_code: values(&self.tla_code)?,
			top_level_arguments: values(&self.tla_str)?,
			max_stack: Some(
				self.max_stack
					.try_into()
					.context("max stack must be positive")?,
			),
			..rtk_jsonnet::Options::default()
		})
	}
}

/// Run the env list subcommand.
pub fn run<W: Write>(args: ListArgs, mut writer: W) -> Result<()> {
	UnimplementedArgs::warn_jsonnet_impl(&args.jsonnet_implementation);
	let search_path = args
		.path
		.as_deref()
		.map(PathBuf::from)
		.unwrap_or(std::env::current_dir()?);
	let options = args.jsonnet_options()?;
	let engine = rtk_environments::Engine::new(rtk_jsonnet::Engine::new(options));
	let mut environments = engine
		.discover_all(vec![search_path.clone()])
		.map_err(|error| anyhow::anyhow!("finding environments: {error}"))?;

	if let Some(selector) = args.selector.as_deref() {
		let selector =
			LabelSelector::parse(selector).map_err(|error| anyhow::anyhow!(error.report()))?;
		environments.retain(|environment| selector.matches(&environment.environment));
	}
	environments.sort_by(|left, right| {
		left.environment
			.metadata
			.name
			.cmp(&right.environment.metadata.name)
	});

	if args.names {
		for environment in environments {
			if let Some(name) = environment.environment.metadata.name.as_deref() {
				writeln!(writer, "{name}")?;
			}
		}
		return Ok(());
	}
	if args.json {
		let values = environments
			.iter()
			.map(|environment| environment_json(&environment.environment))
			.collect::<Result<Vec<_>>>()?;
		writeln!(writer, "{}", serde_json::to_string(&values)?)?;
		return Ok(());
	}

	let mut table = TabWriter::new(writer).padding(4);
	writeln!(table, "NAME\tNAMESPACE\tSERVER")?;
	if environments.is_empty() {
		writeln!(table, "No environments found in {}", search_path.display())?;
	} else {
		for discovered in environments {
			let environment = discovered.environment;
			writeln!(
				table,
				"{}\t{}\t{}",
				environment.metadata.name.as_deref().unwrap_or("unnamed"),
				environment.spec.namespace(),
				environment.spec.api_server.as_deref().unwrap_or("-")
			)?;
		}
	}
	table.flush()?;
	Ok(())
}

fn environment_json(
	environment: &rtk_spec::canonical::Environment<'static>,
) -> Result<serde_json::Value> {
	let mut value = serde_json::to_value(environment)?;
	let spec = value
		.get_mut("spec")
		.and_then(serde_json::Value::as_object_mut)
		.context("environment spec did not serialize as an object")?;
	spec.entry("resourceDefaults")
		.or_insert_with(|| serde_json::json!({}));
	spec.entry("expectVersions")
		.or_insert_with(|| serde_json::json!({}));
	Ok(value)
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
