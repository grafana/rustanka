//! Validate environments command — export environments in memory, then run validations.

use std::{
	io::Write,
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Args;
use rayon::prelude::*;
use tracing::debug;

use super::{common, manifests};
use crate::environments::{
	discover::Discover,
	export::{export_discovered_env_in_memory, ExportOpts, MemoryManifest},
};
use crate::jsonnet::evaluator::{DefaultEvaluator, Evaluator};

#[derive(Args)]
pub struct EnvironmentsArgs {
	/// Tanka environment paths to export and validate
	#[arg(required = true)]
	pub environments: Vec<PathBuf>,

	/// Directory containing Jsonnet validation files
	#[arg(long)]
	pub tests_dir: String,

	/// Log the N slowest work items at the end of the run
	#[arg(long)]
	pub log_slowest: Option<usize>,

	/// Regex filter on '<kind>/<name>'. See https://tanka.dev/output-filtering
	#[arg(short = 't', long)]
	pub target: Vec<String>,

	#[command(flatten)]
	pub jsonnet: super::super::JsonnetArgs,
}

/// Run the validate environments command.
pub fn run<W: Write>(args: EnvironmentsArgs, mut writer: W) -> Result<()> {
	let tests_dir = PathBuf::from(&args.tests_dir);

	debug!(
		environments = ?args.environments.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
		tests_dir = %tests_dir.display(),
		"starting validate environments"
	);

	if args.environments.is_empty() {
		anyhow::bail!("at least one environment path is required");
	}
	if !tests_dir.is_dir() {
		anyhow::bail!("tests directory does not exist: {}", tests_dir.display());
	}

	if common::any_validation_defines_namespace_test(&tests_dir)? {
		writeln!(
			writer,
			"WARN: one or more validation files define namespaceTest. Results may be inaccurate \
			 when multiple environments target the same Kubernetes namespace. For that case, use \
			 `rtk export` followed by `rtk validate manifests` instead."
		)?;
		writeln!(writer)?;
	}

	let eval_opts = args.jsonnet.into_global_evaluator_options();
	let export_opts = ExportOpts {
		output_dir: PathBuf::from("."), // unused for in-memory export
		target: args.target,
		eval_opts,
		skip_manifest: true,
		..ExportOpts::default()
	};

	let discover = Discover::new(
		DefaultEvaluator::new(export_opts.eval_opts.clone()),
		args.environments,
	);
	let discovered: Vec<_> = discover
		.collect::<Result<Vec<_>>>()
		.context("discovering environments")?;

	if discovered.is_empty() {
		anyhow::bail!("no Tanka environments found in the given paths");
	}

	writeln!(
		writer,
		"Exporting {} environment(s) in memory",
		discovered.len()
	)?;

	let pool = rayon::ThreadPoolBuilder::new()
		.num_threads(export_opts.parallelism)
		.stack_size(8 * 1024 * 1024)
		.build()
		.context("building export thread pool")?;

	let export_results: Vec<anyhow::Result<(String, Vec<MemoryManifest>)>> = pool.install(|| {
		discovered
			.par_iter()
			.map(|env| {
				let (env_namespace, manifests) =
					export_discovered_env_in_memory(env, &export_opts)?;
				Ok((env_namespace, manifests))
			})
			.collect()
	});
	jrsonnet_gcmodule::collect_thread_cycles();

	let mut all_manifests = Vec::new();
	for (idx, result) in export_results.into_iter().enumerate() {
		let (env_namespace, memory_manifests) = result
			.with_context(|| format!("exporting environment {}", discovered[idx].path.display()))?;
		writeln!(
			writer,
			"  {}: {} manifest(s)",
			env_namespace,
			memory_manifests.len()
		)?;
		for MemoryManifest {
			relative_path,
			value,
		} in memory_manifests
		{
			let source_file = join_source_path(&env_namespace, &relative_path);
			all_manifests.push(manifests::parsed_manifest_from_json(source_file, value));
		}
	}
	writeln!(writer)?;

	manifests::run_on_manifests(all_manifests, &tests_dir, args.log_slowest, writer)
}

fn join_source_path(env_namespace: &str, relative_path: &Path) -> String {
	let rel = relative_path.to_string_lossy().replace('\\', "/");
	format!("{env_namespace}/{rel}")
}
