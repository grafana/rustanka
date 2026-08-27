//! Prune command handler.
//!
//! Removes Kubernetes resources that exist in the cluster but are no longer
//! defined in the Tanka environment manifests.

use std::{
	io::Write,
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Args;
use tracing::instrument;

use super::common::{
	create_tokio_runtime, evaluate_manifests, get_or_create_connection, prompt_confirmation,
	setup_diff_engine, validate_dry_run, DiffEngineConfig,
};
use super::diff::ColorMode;

// Re-export AutoApprove for backwards compatibility
pub use super::common::AutoApprove;
use crate::{
	k8s::diff::DiffStrategy,
	k8s::{
		apply::ApplyEngine,
		client::ClusterConnection,
		diff::{DiffStatus, ResourceDiff},
		output::DiffOutput,
	},
};

#[derive(Args)]
pub struct PruneArgs {
	/// Path to prune
	pub path: PathBuf,

	/// Skip interactive approval. Only for automation! Allowed values: 'always', 'never', 'if-no-changes'
	#[arg(long, value_enum)]
	pub auto_approve: Option<AutoApprove>,

	/// Controls color in diff output, must be "auto", "always", or "never"
	#[arg(long, default_value = "auto", value_enum)]
	pub color: ColorMode,

	/// Force the diff-strategy to use. Automatically chosen if not set.
	/// One of `native`, `server`, `subset` or `validate`. Checked here so an
	/// unknown name is refused in tk's words.
	#[arg(long, value_name = "DIFF_STRATEGY")]
	pub diff_strategy: Option<String>,

	/// --dry-run parameter to pass down to kubectl, must be "none", "server", or "client"
	#[arg(long)]
	pub dry_run: Option<String>,

	/// Force applying (kubectl apply --force)
	#[arg(long)]
	pub force: bool,

	/// String that only a single inline environment contains in its name
	#[arg(long)]
	pub name: Option<String>,

	/// Regex filter on '<kind>/<name>'. See https://tanka.dev/output-filtering
	#[arg(short = 't', long)]
	pub target: Vec<String>,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,
}

/// Run the prune command.
pub fn run<W: Write>(args: PruneArgs, writer: W) -> Result<()> {
	validate_dry_run(args.dry_run.as_deref())?;

	let runtime = create_tokio_runtime()?;
	runtime.block_on(run_async(args, writer))
}

/// Options for running a prune operation.
#[derive(Default)]
pub struct PruneOpts {
	/// Diff strategy to use.
	pub diff_strategy: Option<DiffStrategy>,
	/// Auto-approval setting.
	pub auto_approve: AutoApprove,
	/// Dry-run mode (none, client, or server).
	pub dry_run: Option<String>,
	/// Force delete.
	pub force: bool,
	/// Color output mode.
	pub color: ColorMode,
	/// Target filters.
	pub target: Vec<String>,
	/// Filter environments by name.
	pub name: Option<String>,
}

/// Prune orphaned resources from the cluster.
///
/// Returns the list of deleted resources.
#[instrument(skip_all, fields(path = %path.display()))]
pub async fn prune_environment<W: Write>(
	path: &Path,
	connection: Option<ClusterConnection>,
	jsonnet: rtk_jsonnet::Options,
	opts: PruneOpts,
	mut writer: W,
) -> Result<Vec<ResourceDiff>> {
	let evaluated = evaluate_manifests(path, jsonnet, opts.name.as_deref(), &opts.target)?;
	let spec = evaluated.spec.as_ref();

	// Prune requires injectLabels to be enabled
	let inject_labels = spec.is_some_and(|spec| spec.inject_labels);
	if !inject_labels {
		anyhow::bail!(
			"spec.injectLabels is set to false in your spec.json. Tanka needs to add \
			 a label to your resources to reliably detect which were removed from Jsonnet. \
			 See https://tanka.dev/garbage-collection for more details"
		);
	}

	let manifests = evaluated.manifests;
	tracing::debug!(manifest_count = manifests.len(), "found manifests");

	let connection = get_or_create_connection(connection, spec).await?;

	// Set up diff engine with prune enabled
	let setup = setup_diff_engine(DiffEngineConfig {
		connection: &connection,
		spec,
		manifests: &manifests,
		with_prune: true,
		diff_strategy_override: opts.diff_strategy,
		// Diffing on its own, with no apply to take a lead from.
		apply_strategy: None,
	})
	.await?;
	let diff_engine = setup.engine;
	let diff_strategy = setup.strategy;
	let default_namespace = setup.default_namespace;

	// Get environment label for prune detection
	// Compute diffs with prune
	tracing::debug!("computing differences with prune detection");
	let diffs = diff_engine
		.diff_all(
			&manifests,
			true,
			evaluated.environment_label.as_deref(),
			true,
		)
		.await
		.context("computing diffs")?;

	// Filter to only deleted resources
	let to_delete: Vec<_> = diffs
		.iter()
		.filter(|d| d.status == DiffStatus::Deleted)
		.collect();

	if to_delete.is_empty() {
		eprintln!("Nothing to prune.");
		return Ok(Vec::new());
	}

	// Display what will be deleted
	let mut output = DiffOutput::new(&mut writer, opts.color, diff_strategy)?;
	for diff in &to_delete {
		output.write_diff(diff)?;
	}

	eprintln!("\n{} resource(s) will be deleted:", to_delete.len());
	for diff in &to_delete {
		eprintln!(
			"  {} {}/{}",
			diff.gvk.kind,
			diff.namespace.as_deref().unwrap_or(""),
			diff.name
		);
	}

	// Check if we're in dry-run mode
	let is_dry_run = opts
		.dry_run
		.as_deref()
		.is_some_and(|d| d != "none" && !d.is_empty());
	if is_dry_run {
		eprintln!("\nDry-run mode: no resources will be deleted.");
		return Ok(to_delete.into_iter().cloned().collect());
	}

	// Determine if we should proceed
	let should_prune = match opts.auto_approve {
		AutoApprove::Always => true,
		AutoApprove::IfNoChanges => to_delete.is_empty(),
		AutoApprove::Never => {
			// Prompt for confirmation
			if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
				anyhow::bail!(
					"cannot prompt for confirmation in non-interactive mode. \
					 Use --auto-approve=always to skip confirmation."
				);
			}
			prompt_confirmation("Delete these resources?")?
		}
	};

	if !should_prune {
		eprintln!("Prune cancelled.");
		return Ok(Vec::new());
	}

	// Create apply engine for deletion
	let apply_engine = ApplyEngine::new(
		connection.client().clone(),
		default_namespace,
		false, // server_side doesn't matter for delete
		opts.force,
	);

	// Delete orphaned resources
	eprintln!("\nDeleting resources...");
	let mut deleted = Vec::new();
	for diff in to_delete {
		match apply_engine
			.delete_resource(&diff.gvk, &diff.name, diff.namespace.as_deref())
			.await
		{
			Ok(_) => {
				eprintln!(
					"  {} {}/{} deleted",
					diff.gvk.kind,
					diff.namespace.as_deref().unwrap_or(""),
					diff.name
				);
				deleted.push(diff.clone());
			}
			Err(e) => {
				return Err(e).context(format!("failed to delete {}/{}", diff.gvk.kind, diff.name));
			}
		}
	}

	eprintln!("\nPrune complete. {} resource(s) deleted.", deleted.len());
	Ok(deleted)
}

/// Async implementation of the prune command.
#[instrument(skip_all, fields(path = %args.path.display()))]
async fn run_async<W: Write>(args: PruneArgs, writer: W) -> Result<()> {
	let jsonnet = args.jsonnet.into_options();
	let opts = PruneOpts {
		diff_strategy: args
			.diff_strategy
			.as_deref()
			.map(DiffStrategy::named)
			.transpose()?,
		auto_approve: args.auto_approve.unwrap_or_default(),
		dry_run: args.dry_run,
		force: args.force,
		color: args.color,
		target: args.target,
		name: args.name,
	};

	prune_environment(&args.path, None, jsonnet, opts, writer).await?;
	Ok(())
}
