//! Utilities for command handlers.

use std::{
	fmt,
	io::{self, ErrorKind, Write},
	path::{Path, PathBuf},
	str::FromStr,
};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use rtk_environments::export::Error as EnvironmentError;
use rtk_spec::canonical::EnvironmentSpec;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::k8s::{
	client::ClusterConnection,
	diff::{DiffEngine, DiffStrategy},
};

#[derive(Clone, Debug, Default)]
pub enum EvaluatorImplementation {
	#[default]
	Jrsonnet,
	Binary(String),
}

impl fmt::Display for EvaluatorImplementation {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			EvaluatorImplementation::Jrsonnet => formatter.write_str("jrsonnet"),
			EvaluatorImplementation::Binary(path) => write!(formatter, "binary:{path}"),
		}
	}
}

impl FromStr for EvaluatorImplementation {
	type Err = anyhow::Error;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value {
			"jrsonnet" => Ok(Self::Jrsonnet),
			value if value.starts_with("binary:") && value.ends_with("jrsonnet") => {
				tracing::warn!("Treating {value} as the local jrsonnet implementation");
				Ok(Self::Jrsonnet)
			}
			value if value.starts_with("binary:") => {
				Ok(Self::Binary(value["binary:".len()..].to_owned()))
			}
			_ => anyhow::bail!("invalid value '{value}': expected 'jrsonnet' or 'binary:<path>'"),
		}
	}
}

pub struct EvaluatedManifests {
	pub spec: Option<EnvironmentSpec>,
	pub environment_label: Option<String>,
	pub manifests: Vec<serde_json::Value>,
}

/// Evaluate and fully process manifests before they cross into async Kubernetes
/// work. Evaluated Jsonnet values are dropped in this function.
pub fn evaluate_manifests(
	path: &Path,
	jsonnet: rtk_jsonnet::Options,
	name: Option<&str>,
	targets: &[String],
) -> Result<EvaluatedManifests> {
	let engine = rtk_environments::Engine::new(rtk_jsonnet::Engine::new(jsonnet));
	let environment = engine.load_single(path, name).map_err(environment_error)?;
	let manifests = engine
		.manifests(&environment, targets)
		.map_err(environment_error)?;
	Ok(EvaluatedManifests {
		spec: environment.spec().cloned(),
		environment_label: environment.environment_label(),
		manifests,
	})
}

fn environment_error(error: EnvironmentError) -> anyhow::Error {
	anyhow::anyhow!(error.report())
}

/// Common Jsonnet evaluator arguments shared across commands.
#[derive(Args)]
pub struct JsonnetArgs {
	/// Set code value of extVar (Format: key=<code>)
	#[arg(long, value_parser = JsonnetArgs::parse_key_value)]
	pub ext_code: Vec<(Box<str>, Box<str>)>,

	/// Set string value of extVar (Format: key=value)
	#[arg(short = 'V', long, value_parser = JsonnetArgs::parse_key_value)]
	pub ext_str: Vec<(Box<str>, Box<str>)>,

	/// This argument is ignored- it will always be "jrsonnet".
	#[arg(long = "jsonnet-implementation", default_value_t)]
	pub implementation: EvaluatorImplementation,

	/// Jsonnet VM max stack. Increase this if you get: max stack frames exceeded
	#[arg(long, default_value = "500")]
	pub max_stack: usize,

	/// Set code value of top level function (Format: key=<code>)
	#[arg(long, value_parser = JsonnetArgs::parse_key_value)]
	pub tla_code: Vec<(Box<str>, Box<str>)>,

	/// Set string value of top level function (Format: key=value)
	#[arg(short = 'A', long, value_parser = JsonnetArgs::parse_key_value)]
	pub tla_str: Vec<(Box<str>, Box<str>)>,
}

impl JsonnetArgs {
	/// The options for the Jsonnet engine the exporter evaluates with.
	pub fn into_options(self) -> rtk_jsonnet::Options {
		self.options()
	}

	pub fn options(&self) -> rtk_jsonnet::Options {
		rtk_jsonnet::Options {
			ext_code: self.ext_code.iter().cloned().collect(),
			ext_variables: self.ext_str.iter().cloned().collect(),
			top_level_arguments: self.tla_str.iter().cloned().collect(),
			top_level_code: self.tla_code.iter().cloned().collect(),
			max_stack: Some(self.max_stack),
			..rtk_jsonnet::Options::default()
		}
	}
}

impl JsonnetArgs {
	/// Parse a `key=value` CLI argument into a `(Box<str>, Box<str>)` tuple.
	pub fn parse_key_value(s: &str) -> Result<(Box<str>, Box<str>), String> {
		let (k, v) = s
			.split_once('=')
			.ok_or_else(|| format!("invalid key=value pair: no '=' in '{s}'"))?;
		Ok((k.into(), v.into()))
	}
}

/// Warn about unimplemented CLI arguments that are accepted for Tanka compatibility
/// but don't do anything in Rustanka.
pub struct UnimplementedArgs<'a> {
	pub jsonnet_implementation: Option<&'a str>,
	pub cache_envs: Option<&'a [String]>,
	pub cache_path: Option<&'a Option<PathBuf>>,
	pub mem_ballast_size_bytes: Option<&'a Option<i64>>,
	pub helm_cache: Option<bool>,
}

impl<'a> UnimplementedArgs<'a> {
	/// Log warnings for any unimplemented arguments that were provided.
	pub fn warn_if_set(&self) {
		if let Some(impl_str) = self.jsonnet_implementation {
			if impl_str != "go" {
				warn!(
					"--jsonnet-implementation is unimplemented in rtk and has no effect; \
					 rtk always uses the built-in jrsonnet evaluator"
				);
			}
		}

		if let Some(envs) = self.cache_envs {
			if !envs.is_empty() {
				warn!("--cache-envs is unimplemented in rtk and has no effect");
			}
		}

		if let Some(Some(_)) = self.cache_path {
			warn!("--cache-path is unimplemented in rtk and has no effect");
		}

		if let Some(Some(_)) = self.mem_ballast_size_bytes {
			warn!("--mem-ballast-size-bytes is unimplemented in rtk and has no effect");
		}

		if self.helm_cache == Some(true) {
			warn!(
				"--helm-cache is unimplemented in rtk and has no effect; helmTemplate \
				 results are still cached for the duration of a single export"
			);
		}
	}

	/// Convenience method to warn only about jsonnet_implementation.
	///
	/// Most commands only have the jsonnet_implementation flag as an unimplemented
	/// option. This helper avoids the boilerplate of constructing the full struct.
	pub fn warn_jsonnet_impl(jsonnet_implementation: &str) {
		UnimplementedArgs {
			jsonnet_implementation: Some(jsonnet_implementation),
			cache_envs: None,
			cache_path: None,
			mem_ballast_size_bytes: None,
			helm_cache: None,
		}
		.warn_if_set();
	}
}

/// A writer wrapper that silently handles broken pipe errors.
///
/// When the underlying writer returns a broken pipe error (EPIPE), this wrapper
/// converts it to a successful write. This allows commands to exit cleanly when
/// output is piped to a process that closes early (e.g., `rtk eval . | head -1`).
pub struct BrokenPipeGuard<W> {
	inner: W,
}

impl<W> BrokenPipeGuard<W> {
	pub fn new(inner: W) -> Self {
		Self { inner }
	}
}

impl<W: Write> Write for BrokenPipeGuard<W> {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		match self.inner.write(buf) {
			Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(buf.len()),
			other => other,
		}
	}

	fn flush(&mut self) -> io::Result<()> {
		match self.inner.flush() {
			Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
			other => other,
		}
	}
}

/// Prompt the user for confirmation with a custom prompt.
pub fn prompt_confirmation(prompt: &str) -> Result<bool> {
	eprint!("\n{} [y/N]: ", prompt);
	std::io::stderr().flush()?;

	let mut input = String::new();
	std::io::stdin().read_line(&mut input)?;

	let input = input.trim().to_lowercase();
	Ok(input == "y" || input == "yes")
}

/// Auto-approve settings for apply and prune commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutoApprove {
	/// Always require manual approval.
	#[default]
	Never,

	/// Always auto-approve without prompting.
	Always,

	/// Auto-approve only if there are no changes (no-op).
	IfNoChanges,
}

impl fmt::Display for AutoApprove {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			AutoApprove::Never => write!(f, "never"),
			AutoApprove::Always => write!(f, "always"),
			AutoApprove::IfNoChanges => write!(f, "if-no-changes"),
		}
	}
}

/// Get or create a cluster connection from the spec.
///
/// If a connection is already provided, returns it. Otherwise, creates
/// a new connection from the spec.
pub async fn get_or_create_connection(
	connection: Option<ClusterConnection>,
	spec: Option<&EnvironmentSpec>,
) -> Result<ClusterConnection> {
	match connection {
		Some(conn) => Ok(conn),
		None => {
			let spec_for_connection = spec.cloned().unwrap_or_default();
			tracing::debug!("connecting to Kubernetes cluster");
			let conn = ClusterConnection::from_spec(&spec_for_connection)
				.await
				.context("connecting to Kubernetes cluster")?;
			tracing::debug!(
				cluster = %conn.cluster_identifier(),
				server_version = %format!("{}.{}", conn.server_version().major, conn.server_version().minor),
				"connected to cluster"
			);
			Ok(conn)
		}
	}
}

/// Validate the dry-run option value.
///
/// Returns an error if the value is not one of: "", "none", "client", "server".
pub fn validate_dry_run(dry_run: Option<&str>) -> Result<()> {
	if let Some(value) = dry_run {
		match value {
			"" | "none" | "client" | "server" => {}
			_ => {
				anyhow::bail!("--dry-run must be either: \"\", \"none\", \"server\" or \"client\"")
			}
		}
	}
	Ok(())
}

/// Create a multi-threaded tokio runtime.
pub fn create_tokio_runtime() -> Result<tokio::runtime::Runtime> {
	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.context("creating tokio runtime")
}

/// Configuration for setting up a diff engine.
pub struct DiffEngineConfig<'a> {
	/// Connection to the Kubernetes cluster.
	pub connection: &'a ClusterConnection,
	/// Optional spec for strategy selection.
	pub spec: Option<&'a EnvironmentSpec>,
	/// Manifests to diff against.
	pub manifests: &'a [serde_json::Value],
	/// Whether to enable prune detection.
	pub with_prune: bool,
	/// Optional override for diff strategy.
	pub diff_strategy_override: Option<DiffStrategy>,
}

/// Result of setting up a diff engine.
pub struct DiffEngineSetup {
	/// The configured diff engine.
	pub engine: DiffEngine,
	/// The diff strategy being used.
	pub strategy: DiffStrategy,
	/// The default namespace for resources.
	pub default_namespace: String,
}

/// Set up a diff engine with strategy and namespace resolution.
///
/// This consolidates the common pattern of:
/// 1. Determining diff strategy from override, spec, or default
/// 2. Resolving default namespace from spec or connection
/// 3. Creating the diff engine
pub async fn setup_diff_engine(config: DiffEngineConfig<'_>) -> Result<DiffEngineSetup> {
	// Determine diff strategy
	let strategy = config.diff_strategy_override.unwrap_or_else(|| {
		if let Some(s) = config.spec {
			DiffStrategy::from_spec(s, config.connection.server_version())
		} else {
			DiffStrategy::Native
		}
	});
	tracing::debug!(strategy = %strategy, "using diff strategy");

	// Get default namespace from spec or connection
	let default_namespace = config
		.spec
		.map(|s| s.namespace().to_owned())
		.unwrap_or_else(|| config.connection.default_namespace().to_string());

	// Create diff engine
	let engine = DiffEngine::new(
		config.connection.clone(),
		strategy,
		default_namespace.clone(),
		config.manifests,
		config.with_prune,
	)
	.await
	.context("creating diff engine")?;

	Ok(DiffEngineSetup {
		engine,
		strategy,
		default_namespace,
	})
}
