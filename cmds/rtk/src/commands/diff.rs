//! Diff command handler.
//!
//! Compares local Tanka environment manifests against the live Kubernetes cluster state.

use std::{
	fmt,
	io::Write,
	path::{Path, PathBuf},
	sync::Arc,
};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use super::common::{
	create_tokio_runtime, evaluate_manifests, get_or_create_connection, setup_diff_engine,
	DiffEngineConfig,
};
use crate::{
	k8s::diff::DiffStrategy,
	k8s::{
		client::ClusterConnection,
		diff::{DiffEngine, DiffStatus, ResourceDiff},
		output::DiffOutput,
	},
};

/// Color output mode for diff display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
	/// Color if stdout is a TTY.
	#[default]
	Auto,

	/// Always emit ANSI color codes.
	Always,

	/// No colors (plain text).
	Never,
}

impl ColorMode {
	/// Determine if colors should be used based on mode and terminal detection.
	pub fn should_colorize(&self) -> bool {
		match self {
			ColorMode::Auto => std::io::IsTerminal::is_terminal(&std::io::stdout()),
			ColorMode::Always => true,
			ColorMode::Never => false,
		}
	}
}

impl fmt::Display for ColorMode {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			ColorMode::Auto => write!(f, "auto"),
			ColorMode::Always => write!(f, "always"),
			ColorMode::Never => write!(f, "never"),
		}
	}
}

/// Exit code when differences are found (matches tk behavior).
pub const EXIT_CODE_DIFF_FOUND: i32 = 16;

#[derive(Args)]
pub struct DiffArgs {
	/// Path to the Tanka environment
	pub path: PathBuf,

	/// Color output mode
	#[arg(long, default_value = "auto", value_enum)]
	pub color: ColorMode,

	/// Force the diff-strategy to use. Automatically chosen if not set.
	#[arg(long, value_enum)]
	pub diff_strategy: Option<DiffStrategy>,

	/// Exit with 0 even when differences are found
	#[arg(short = 'z', long)]
	pub exit_zero: bool,

	/// String that only a single inline environment contains in its name
	#[arg(long)]
	pub name: Option<String>,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,

	/// Print summary of the differences, not the actual contents
	#[arg(short = 's', long)]
	pub summarize: bool,

	/// Regex filter on '<kind>/<name>'. See https://tanka.dev/output-filtering
	#[arg(short = 't', long)]
	pub target: Vec<String>,

	/// Include objects deleted from the configuration in the differences
	#[arg(short = 'p', long)]
	pub with_prune: bool,

	/// List environments with changes
	#[arg(long)]
	pub list_modified_envs: bool,
}

/// Result of running the diff command.
struct DiffResult {
	/// Whether any differences were found.
	has_changes: bool,
}

/// Run the diff command.
///
/// Returns `Ok(true)` if differences were found and `--exit-zero` was not passed,
/// indicating the caller should exit with `EXIT_CODE_DIFF_FOUND`.
pub fn run<W: Write>(args: DiffArgs, writer: W) -> Result<bool> {
	let exit_zero = args.exit_zero;
	let list_modified_envs = args.list_modified_envs;

	let runtime = create_tokio_runtime()?;
	let result = runtime.block_on(run_async(args, writer))?;

	// --list-modified-envs always exits 0
	if list_modified_envs {
		return Ok(false);
	}

	// Return whether we should exit with non-zero code
	// (has changes AND --exit-zero was not passed)
	Ok(result.has_changes && !exit_zero)
}

/// Options for running a diff operation.
#[derive(Default, bon::Builder)]
pub struct DiffOpts {
	/// Diff strategy to use.
	pub strategy: Option<DiffStrategy>,
	/// Whether to include pruned resources.
	#[builder(default)]
	pub with_prune: bool,
	/// Color output mode.
	#[builder(default)]
	pub color: ColorMode,
	/// Whether to print summary instead of full diff.
	#[builder(default)]
	pub summarize: bool,
	/// Target filters.
	#[builder(default)]
	pub target: Vec<String>,
	/// Filter environments by name (exact match first, then substring).
	pub name: Option<String>,
}

/// Run diff on an environment path against cluster state.
///
/// Evaluates the Jsonnet environment, extracts manifests, and compares them
/// against the current state in the connected cluster. If no connection is
/// provided, one is created from the environment's spec.
#[instrument(skip_all, fields(path = %path.display()))]
pub async fn diff_environment<W: Write>(
	path: &Path,
	connection: Option<ClusterConnection>,
	jsonnet: rtk_jsonnet::Options,
	opts: DiffOpts,
	writer: W,
) -> Result<Vec<ResourceDiff>> {
	let evaluated = evaluate_manifests(path, jsonnet, opts.name.as_deref(), &opts.target)?;
	let manifests = evaluated.manifests;
	tracing::debug!(manifest_count = manifests.len(), "found manifests to diff");

	if manifests.is_empty() {
		tracing::warn!("no manifests found in environment");
		return Ok(Vec::new());
	}

	let connection = get_or_create_connection(connection, evaluated.spec.as_ref()).await?;

	diff_manifests(
		manifests,
		connection,
		evaluated.spec.as_ref(),
		evaluated.environment_label.as_deref(),
		opts,
		writer,
	)
	.await
}

/// Run diff on manifests against cluster state.
///
/// Compares the provided manifests against the current state in the connected
/// cluster, returning the differences for each resource.
#[instrument(skip_all, fields(manifest_count = manifests.len()))]
pub async fn diff_manifests<W: Write>(
	manifests: Vec<serde_json::Value>,
	connection: ClusterConnection,
	spec: Option<&rtk_spec::canonical::EnvironmentSpec>,
	environment_label: Option<&str>,
	opts: DiffOpts,
	mut writer: W,
) -> Result<Vec<ResourceDiff>> {
	// Set up diff engine
	let setup = setup_diff_engine(DiffEngineConfig {
		connection: &connection,
		spec,
		manifests: &manifests,
		with_prune: opts.with_prune,
		diff_strategy_override: opts.strategy,
	})
	.await?;
	let engine = setup.engine;
	let strategy = setup.strategy;

	// Get environment label for prune detection (SHA256 hash of name:namespace)
	// Check if inject_labels is enabled (required for prune detection)
	let inject_labels = spec.is_some_and(|spec| spec.inject_labels);

	// Compute diffs
	tracing::debug!("computing differences");
	let diffs = engine
		.diff_all(
			&manifests,
			opts.with_prune,
			environment_label,
			inject_labels,
		)
		.await
		.context("computing diffs")?;

	// Output results if writer is provided
	let has_changes = diffs.iter().any(|d| d.has_changes());
	let mut output = DiffOutput::new(&mut writer, opts.color, strategy)?;

	if opts.summarize {
		output.write_summary(&diffs)?;
	} else {
		for diff in &diffs {
			if diff.status != DiffStatus::Unchanged {
				output.write_diff(diff)?;
			}
		}

		if !has_changes {
			eprintln!("No differences.");
		}
	}

	Ok(diffs)
}

/// Async implementation of the diff command.
#[instrument(skip_all, fields(path = %args.path.display()))]
async fn run_async<W: Write>(args: DiffArgs, mut writer: W) -> Result<DiffResult> {
	// Handle --list-modified-envs mode: find all environments and check each for changes
	if args.list_modified_envs {
		return list_modified_environments(&args, &mut writer).await;
	}

	let jsonnet = args.jsonnet.into_options();
	let opts = DiffOpts {
		strategy: args.diff_strategy,
		with_prune: args.with_prune,
		color: args.color,
		summarize: args.summarize,
		target: args.target,
		name: args.name,
	};

	let diffs = diff_environment(&args.path, None, jsonnet, opts, writer).await?;
	let has_changes = diffs.iter().any(|d| d.has_changes());

	Ok(DiffResult { has_changes })
}

/// List environments that have changes.
///
/// Discovers all environments in the path, checks each for changes in parallel,
/// and prints the names of environments with differences.
#[instrument(skip_all, fields(path = %args.path.display()))]
async fn list_modified_environments<W: Write>(
	args: &DiffArgs,
	writer: &mut W,
) -> Result<DiffResult> {
	// Discover all environments in the path
	tracing::debug!(path = %args.path.display(), "discovering environments");
	let jsonnet = args.jsonnet.options();
	let engine = rtk_environments::Engine::new(rtk_jsonnet::Engine::new(jsonnet.clone()));
	let envs: Vec<rtk_environments::Discovered> = engine
		.discover_all(vec![args.path.clone()])
		.map_err(|error| anyhow::anyhow!("discovering environments: {error}"))?;

	// Filter environments by --name if specified, preferring an exact match
	let envs: Vec<_> = if let Some(ref target_name) = args.name {
		let name_of = |env: &rtk_environments::Discovered| {
			env.environment.metadata.name.clone().unwrap_or_default()
		};

		let exact: Vec<_> = envs
			.iter()
			.filter(|e| name_of(e) == *target_name)
			.cloned()
			.collect();

		if exact.is_empty() {
			envs.into_iter()
				.filter(|e| name_of(e).contains(target_name))
				.collect()
		} else {
			exact
		}
	} else {
		envs
	};

	if envs.is_empty() {
		eprintln!("No environments with changes.");
		return Ok(DiffResult { has_changes: false });
	}

	tracing::debug!(env_count = envs.len(), "found environments");

	// Check all environments in parallel using JoinSet with concurrency limit
	const MAX_PARALLEL: usize = 8;
	let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_PARALLEL));
	let target = std::sync::Arc::new(args.target.clone());
	let mut join_set = tokio::task::JoinSet::new();

	for env in &envs {
		let env_path = env.path.to_string_lossy().to_string();
		let display_name = env
			.environment
			.metadata
			.name
			.clone()
			.unwrap_or_else(|| env_path.clone());

		let selected_name = env.selected_by().map(str::to_owned);
		let jsonnet = jsonnet.clone();

		let diff_strategy = args.diff_strategy;
		let with_prune = args.with_prune;
		let target = Arc::clone(&target);
		let sem = semaphore.clone();

		join_set.spawn(async move {
			let _permit = sem.acquire().await.expect("semaphore closed");
			tracing::debug!(env_path = %env_path, "checking environment");
			match check_environment_for_changes(
				env_path.clone(),
				jsonnet,
				selected_name,
				diff_strategy,
				with_prune,
				target,
			)
			.await
			{
				Ok(true) => Some(display_name),
				Ok(false) => {
					tracing::debug!(env_path = %env_path, "no changes");
					None
				}
				Err(e) => {
					tracing::warn!(env_path = %env_path, error = %e, "failed to check environment");
					None
				}
			}
		});
	}

	let mut changed_envs = Vec::new();
	while let Some(result) = join_set.join_next().await {
		if let Ok(Some(name)) = result {
			changed_envs.push(name);
		}
	}

	// Print results
	if changed_envs.is_empty() {
		eprintln!("No environments with changes.");
		Ok(DiffResult { has_changes: false })
	} else {
		changed_envs.sort();
		for name in &changed_envs {
			writeln!(writer, "{}", name)?;
		}
		Ok(DiffResult { has_changes: true })
	}
}

/// Check if a single environment has changes.
#[instrument(skip_all, fields(path = %path))]
async fn check_environment_for_changes(
	path: String,
	jsonnet: rtk_jsonnet::Options,
	name: Option<String>,
	diff_strategy: Option<DiffStrategy>,
	with_prune: bool,
	target: Arc<Vec<String>>,
) -> Result<bool> {
	let evaluated = evaluate_manifests(Path::new(&path), jsonnet, name.as_deref(), target.as_ref())
		.context("evaluating environment")?;
	let manifests = evaluated.manifests;
	if manifests.is_empty() {
		return Ok(false);
	}

	// Connect to the cluster
	let spec_for_connection = evaluated.spec.clone().unwrap_or_default();
	let connection = ClusterConnection::from_spec(&spec_for_connection).await?;

	// Determine diff strategy
	let strategy = diff_strategy.unwrap_or_else(|| {
		if let Some(spec) = evaluated.spec.as_ref() {
			DiffStrategy::from_spec(spec, connection.server_version())
		} else {
			DiffStrategy::Native
		}
	});

	// Get default namespace
	let default_namespace = evaluated
		.spec
		.as_ref()
		.map(|spec| spec.namespace().to_owned())
		.unwrap_or_else(|| connection.default_namespace().to_string());

	// Create diff engine
	let engine = DiffEngine::new(
		connection,
		strategy,
		default_namespace,
		&manifests,
		with_prune,
	)
	.await?;

	// Get environment label for prune detection
	let inject_labels = evaluated
		.spec
		.as_ref()
		.is_some_and(|spec| spec.inject_labels);

	// Compute diffs
	let diffs = engine
		.diff_all(
			&manifests,
			with_prune,
			evaluated.environment_label.as_deref(),
			inject_labels,
		)
		.await?;

	// Check if any resource has changes
	Ok(diffs.iter().any(|d| d.has_changes()))
}

#[cfg(test)]
mod tests {

	use super::*;

	#[test]
	fn test_build_eval_opts() {
		use crate::commands::common::EvaluatorImplementation;

		let args = DiffArgs {
			path: PathBuf::from("test"),
			color: ColorMode::Auto,
			diff_strategy: None,
			exit_zero: false,
			name: Some("my-env".to_string()),
			jsonnet: crate::commands::JsonnetArgs {
				ext_code: vec![("code1".into(), "{}".into())],
				ext_str: vec![("str1".into(), "value1".into())],
				implementation: EvaluatorImplementation::default(),
				max_stack: 500,
				tla_code: vec![("tla1".into(), "true".into())],
				tla_str: vec![("tla2".into(), "hello".into())],
			},
			summarize: false,
			target: vec![],
			with_prune: false,
			list_modified_envs: false,
		};

		let opts = args.jsonnet.into_options();
		assert_eq!(opts.ext_variables.get("str1").map(|v| &**v), Some("value1"));
		assert_eq!(opts.ext_code.get("code1").map(|v| &**v), Some("{}"));
		assert_eq!(
			opts.top_level_arguments.get("tla2").map(|v| &**v),
			Some("hello")
		);
		assert_eq!(opts.top_level_code.get("tla1").map(|v| &**v), Some("true"));
		assert_eq!(opts.max_stack, Some(500));
	}
}
