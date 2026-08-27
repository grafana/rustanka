//! Apply command handler.
//!
//! Applies Tanka environment manifests to the Kubernetes cluster after showing
//! a diff and optionally prompting for confirmation.

use std::{
	fmt,
	io::Write,
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use rtk_spec::canonical::EnvironmentSpec;
use serde::{Deserialize, Serialize};
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

/// Apply strategy for resource updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplyStrategy {
	/// Client-side apply using PATCH with strategic merge.
	#[default]
	Client,

	/// Server-side apply using PATCH with Apply.
	Server,
}

impl fmt::Display for ApplyStrategy {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			ApplyStrategy::Client => write!(f, "client"),
			ApplyStrategy::Server => write!(f, "server"),
		}
	}
}

#[derive(Args)]
pub struct ApplyArgs {
	/// Path to the Tanka environment
	pub path: PathBuf,

	/// Force the apply strategy to use. Automatically chosen if not set.
	///
	/// One of `client` or `server`. Taken as a string and checked here, so an
	/// unknown name is refused in tk's words rather than clap's, and in the same
	/// words as an unknown `spec.applyStrategy`.
	#[arg(long, value_name = "APPLY_STRATEGY")]
	pub apply_strategy: Option<String>,

	/// Skip interactive approval. Allowed values: 'always', 'never', 'if-no-changes'
	#[arg(long, value_enum)]
	pub auto_approve: Option<AutoApprove>,

	/// Controls color in diff output
	#[arg(long, default_value = "auto", value_enum)]
	pub color: ColorMode,

	/// Force the diff strategy to use. Automatically chosen if not set.
	///
	/// One of `native`, `server`, `subset`, `validate`, or `none` to apply
	/// without showing a diff. `none` is accepted here and nowhere else, as in
	/// tk.
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

	/// Validation of resources (kubectl --validate=false)
	#[arg(long, default_value = "true")]
	pub validate: bool,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,
}

/// Run the apply command.
pub fn run<W: Write>(args: ApplyArgs, writer: W) -> Result<()> {
	validate_dry_run(args.dry_run.as_deref())?;

	let runtime = create_tokio_runtime()?;
	runtime.block_on(run_async(args, writer))
}

/// Options for running an apply operation.
#[derive(Default)]
pub struct ApplyOpts {
	/// Diff strategy to use.
	pub diff_strategy: Option<DiffStrategy>,
	/// Whether `--diff-strategy none` asked for the diff not to be shown.
	///
	/// tk skips computing the diff altogether, because it hands every resource
	/// to `kubectl apply` and the diff is only ever informational. rtk applies
	/// what the diff says changed, so it still has to compare; what `none` turns
	/// off is the output.
	pub skip_diff_output: bool,
	/// Apply strategy to use.
	pub apply_strategy: Option<ApplyStrategy>,
	/// Auto-approval setting.
	pub auto_approve: AutoApprove,
	/// Dry-run mode (none, client, or server).
	pub dry_run: Option<String>,
	/// Force apply.
	pub force: bool,
	/// Color output mode.
	pub color: ColorMode,
	/// Target filters.
	pub target: Vec<String>,
	/// Filter environments by name.
	pub name: Option<String>,
}

/// What `--diff-strategy` may name for an apply.
///
/// tk accepts `none` here and nowhere else — `tk diff --diff-strategy none`
/// fails, because that command looks the name up in the same map of differs that
/// has no such entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplyDiffStrategy {
	Native,
	Server,
	Validate,
	Subset,
	/// Do not show the diff before applying.
	None,
}

impl ApplyDiffStrategy {
	/// The strategy of this name, in tk's words when there is none.
	fn named(strategy: &str) -> Result<Self> {
		if strategy == "none" {
			return Ok(ApplyDiffStrategy::None);
		}
		Ok(match DiffStrategy::named(strategy)? {
			DiffStrategy::Native => ApplyDiffStrategy::Native,
			DiffStrategy::Server => ApplyDiffStrategy::Server,
			DiffStrategy::Validate => ApplyDiffStrategy::Validate,
			DiffStrategy::Subset => ApplyDiffStrategy::Subset,
		})
	}

	/// The strategy to diff with, or `None` to show nothing.
	fn strategy(self) -> Option<DiffStrategy> {
		match self {
			ApplyDiffStrategy::Native => Some(DiffStrategy::Native),
			ApplyDiffStrategy::Server => Some(DiffStrategy::Server),
			ApplyDiffStrategy::Validate => Some(DiffStrategy::Validate),
			ApplyDiffStrategy::Subset => Some(DiffStrategy::Subset),
			ApplyDiffStrategy::None => None,
		}
	}
}

impl ApplyStrategy {
	/// The strategy of this name, in tk's words when there is none.
	fn named(strategy: &str) -> Result<Self> {
		match strategy {
			"client" => Ok(ApplyStrategy::Client),
			"server" => Ok(ApplyStrategy::Server),
			// The list is spelled out in tk's `ErrorApplyStrategyUnknown`, in
			// that order.
			other => anyhow::bail!(
				"apply strategy `{other}` does not exist. Pick one of: [server, client]."
			),
		}
	}

	/// Resolve the strategy the way tk's `Apply` resolves it: the flag wins,
	/// then the environment's own spec, then client-side.
	///
	/// rtk used to read the flag and nothing else, so an environment asking for
	/// a server-side apply quietly got a client-side one against a live cluster.
	/// tk settles this before it connects to anything, so a misspelled strategy
	/// is reported without needing a cluster.
	fn resolve(flag: Option<ApplyStrategy>, spec: Option<&EnvironmentSpec>) -> Result<Self> {
		if let Some(strategy) = flag {
			return Ok(strategy);
		}
		match spec.and_then(|spec| spec.apply_strategy.as_deref()) {
			Some(requested) => ApplyStrategy::named(requested),
			None => Ok(ApplyStrategy::Client),
		}
	}
}

/// Apply manifests to the cluster.
///
/// Returns the list of applied resources.
#[instrument(skip_all, fields(path = %path.display()))]
pub async fn apply_environment<W: Write>(
	path: &Path,
	connection: Option<ClusterConnection>,
	jsonnet: rtk_jsonnet::Options,
	opts: ApplyOpts,
	mut writer: W,
) -> Result<Vec<ResourceDiff>> {
	let evaluated = evaluate_manifests(path, jsonnet, opts.name.as_deref(), &opts.target)?;
	let manifests = evaluated.manifests;
	tracing::debug!(manifest_count = manifests.len(), "found manifests to apply");

	// Settled before anything is reached over the network, as tk settles it, so
	// a strategy that does not exist is refused whatever the cluster is doing.
	let apply_strategy = ApplyStrategy::resolve(opts.apply_strategy, evaluated.spec.as_ref())?;
	tracing::debug!(strategy = %apply_strategy, "using apply strategy");

	if manifests.is_empty() {
		tracing::warn!("no manifests found in environment");
		eprintln!("No manifests to apply.");
		return Ok(Vec::new());
	}

	let connection = get_or_create_connection(connection, evaluated.spec.as_ref()).await?;

	// Set up diff engine
	let setup = setup_diff_engine(DiffEngineConfig {
		connection: &connection,
		spec: evaluated.spec.as_ref(),
		manifests: &manifests,
		with_prune: false, // no prune for apply (use prune command)
		diff_strategy_override: opts.diff_strategy,
		apply_strategy: Some(match apply_strategy {
			ApplyStrategy::Client => "client",
			ApplyStrategy::Server => "server",
		}),
	})
	.await?;
	let diff_engine = setup.engine;
	let diff_strategy = setup.strategy;
	let default_namespace = setup.default_namespace;

	// Compute diffs
	tracing::debug!("computing differences");
	let diffs = diff_engine
		.diff_all(&manifests, false, None, false)
		.await
		.context("computing diffs")?;

	// Check if there are changes
	let has_changes = diffs.iter().any(|d| d.has_changes());

	// Display diff, unless `--diff-strategy none` asked for silence.
	if !opts.skip_diff_output {
		let mut output = DiffOutput::new(&mut writer, opts.color, diff_strategy)?;
		for diff in &diffs {
			if diff.status != DiffStatus::Unchanged {
				output.write_diff(diff)?;
			}
		}
	}

	if !has_changes {
		eprintln!("No differences. Nothing to apply.");
		return Ok(diffs);
	}

	// Check if we're in dry-run mode
	let is_dry_run = opts
		.dry_run
		.as_deref()
		.is_some_and(|d| d != "none" && !d.is_empty());
	if is_dry_run {
		eprintln!("\nDry-run mode: no changes will be applied.");
		return Ok(diffs);
	}

	// Determine if we should apply
	let should_apply = match opts.auto_approve {
		AutoApprove::Always => true,
		AutoApprove::IfNoChanges => !has_changes,
		AutoApprove::Never => {
			// Prompt for confirmation
			if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
				anyhow::bail!(
					"cannot prompt for confirmation in non-interactive mode. \
					 Use --auto-approve=always to skip confirmation."
				);
			}
			prompt_confirmation("Apply these changes?")?
		}
	};

	if !should_apply {
		eprintln!("Apply cancelled.");
		return Ok(diffs);
	}

	// Create apply engine
	let apply_engine = ApplyEngine::new(
		connection.client().clone(),
		default_namespace,
		apply_strategy == ApplyStrategy::Server,
		opts.force,
	);

	// Apply changes
	eprintln!("\nApplying changes...");
	let changes_to_apply: Vec<_> = diffs
		.iter()
		.filter(|d| d.has_changes() && d.status != DiffStatus::Deleted)
		.collect();

	for diff in &changes_to_apply {
		// Find the corresponding manifest
		let manifest = manifests.iter().find(|m| {
			let name = m
				.pointer("/metadata/name")
				.and_then(|v| v.as_str())
				.unwrap_or("");
			let kind = m.get("kind").and_then(|v| v.as_str()).unwrap_or("");
			name == diff.name && kind == diff.gvk.kind
		});

		if let Some(manifest) = manifest {
			match apply_engine.apply_manifest(manifest).await {
				Ok(_) => {
					eprintln!(
						"  {} {}/{} applied",
						diff.gvk.kind,
						diff.namespace.as_deref().unwrap_or(""),
						diff.name
					);
				}
				Err(e) => {
					return Err(e)
						.context(format!("failed to apply {}/{}", diff.gvk.kind, diff.name));
				}
			}
		}
	}

	eprintln!(
		"\nApply complete. {} resource(s) changed.",
		changes_to_apply.len()
	);
	Ok(diffs)
}

/// Async implementation of the apply command.
#[instrument(skip_all, fields(path = %args.path.display()))]
async fn run_async<W: Write>(args: ApplyArgs, writer: W) -> Result<()> {
	let jsonnet = args.jsonnet.into_options();
	// Both names are checked before anything else happens, as tk checks them.
	let diff_strategy = args
		.diff_strategy
		.as_deref()
		.map(ApplyDiffStrategy::named)
		.transpose()?;
	let apply_strategy = args
		.apply_strategy
		.as_deref()
		.map(ApplyStrategy::named)
		.transpose()?;
	let opts = ApplyOpts {
		diff_strategy: diff_strategy.and_then(ApplyDiffStrategy::strategy),
		skip_diff_output: diff_strategy == Some(ApplyDiffStrategy::None),
		apply_strategy,
		auto_approve: args.auto_approve.unwrap_or_default(),
		dry_run: args.dry_run,
		force: args.force,
		color: args.color,
		target: args.target,
		name: args.name,
	};

	apply_environment(&args.path, None, jsonnet, opts, writer).await?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn spec(apply_strategy: Option<&str>) -> EnvironmentSpec {
		let mut spec = EnvironmentSpec::default();
		spec.apply_strategy = apply_strategy.map(Into::into);
		spec
	}

	/// tk's precedence: the flag, then the environment's own spec, then client.
	///
	/// rtk read the flag and nothing else, so an environment asking for a
	/// server-side apply quietly got a client-side one.
	#[test]
	fn the_spec_decides_the_apply_strategy_when_no_flag_does() {
		let resolved = ApplyStrategy::resolve(None, Some(&spec(Some("server"))))
			.expect("a strategy the spec names");
		assert_eq!(resolved, ApplyStrategy::Server);

		let resolved =
			ApplyStrategy::resolve(None, Some(&spec(Some("client")))).expect("a known strategy");
		assert_eq!(resolved, ApplyStrategy::Client);
	}

	#[test]
	fn the_flag_wins_over_the_spec() {
		let resolved =
			ApplyStrategy::resolve(Some(ApplyStrategy::Client), Some(&spec(Some("server"))))
				.expect("the flag decides");
		assert_eq!(resolved, ApplyStrategy::Client);
	}

	#[test]
	fn an_environment_naming_nothing_applies_client_side() {
		assert_eq!(
			ApplyStrategy::resolve(None, Some(&spec(None))).expect("the default"),
			ApplyStrategy::Client
		);
		assert_eq!(
			ApplyStrategy::resolve(None, None).expect("the default"),
			ApplyStrategy::Client
		);
	}

	/// Word for word tk's `ErrorApplyStrategyUnknown`, including the order it
	/// spells the two out in.
	#[test]
	fn an_unknown_apply_strategy_is_refused_in_tks_words() {
		let error = ApplyStrategy::resolve(None, Some(&spec(Some("nonsense"))))
			.expect_err("no such strategy");
		assert_eq!(
			error.to_string(),
			"apply strategy `nonsense` does not exist. Pick one of: [server, client]."
		);
	}

	/// `none` says not to show a diff, and is spelled only here.
	#[test]
	fn none_is_a_diff_strategy_only_an_apply_accepts() {
		assert_eq!(
			ApplyDiffStrategy::named("none").expect("apply accepts none"),
			ApplyDiffStrategy::None
		);
		assert_eq!(ApplyDiffStrategy::None.strategy(), None);
		assert_eq!(
			ApplyDiffStrategy::named("subset")
				.expect("a real strategy")
				.strategy(),
			Some(DiffStrategy::Subset)
		);
		assert!(ApplyDiffStrategy::named("nonsense")
			.expect_err("no such strategy")
			.to_string()
			.contains("diff strategy `nonsense` does not exist"));
	}
}
