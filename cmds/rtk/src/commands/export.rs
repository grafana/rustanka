//! Export command handler.

use std::{io::Write, path::PathBuf, sync::Arc};

use anyhow::Result;
use clap::Args;

use super::common::UnimplementedArgs;
use crate::environments::export::{self as export_impl, ExportMergeStrategy, ExportOpts};

#[derive(Args)]
pub struct ExportArgs {
	/// Output directory
	pub output_dir: PathBuf,

	/// Paths to export
	pub paths: Vec<PathBuf>,

	/// Regexes which define which environment should be cached (if caching is enabled)
	#[arg(short = 'e', long)]
	pub cache_envs: Vec<String>,

	/// Local file path where cached evaluations should be stored
	#[arg(short = 'c', long)]
	pub cache_path: Option<PathBuf>,

	/// File extension
	#[arg(long, default_value = "yaml", overrides_with = "extension")]
	pub extension: String,

	/// https://tanka.dev/exporting#filenames
	#[arg(
		long,
		default_value = "{{.apiVersion}}.{{.kind}}-{{or .metadata.name .metadata.generateName}}",
		overrides_with = "format"
	)]
	pub format: String,

	/// Size of memory ballast to allocate. This may improve performance for large environments.
	#[arg(long)]
	pub mem_ballast_size_bytes: Option<i64>,

	/// Tanka main files that have been deleted. This is used when using a merge strategy to also delete the files of these deleted environments.
	#[arg(long)]
	pub merge_deleted_envs: Vec<String>,

	/// What to do when exporting to an existing directory. The default setting is to disallow exporting to an existing directory. Values: 'fail-on-conflicts', 'replace-envs'
	#[arg(long)]
	pub merge_strategy: Option<String>,

	/// String that only a single inline environment contains in its name
	#[arg(long)]
	pub name: Option<String>,

	/// Number of environments to process in parallel
	#[arg(short = 'p', long, default_value = "8", overrides_with = "parallel")]
	pub parallel: i32,

	/// Look recursively for Tanka environments
	#[arg(short = 'r', long)]
	pub recursive: bool,

	/// Label selector. Uses the same syntax as kubectl does
	#[arg(short = 'l', long)]
	pub selector: Option<String>,

	/// Skip generating manifest.json file that tracks exported files
	#[arg(long)]
	pub skip_manifest: bool,

	/// Experimental: maintain a `helm-cache/` directory in the output dir to
	/// cache helmTemplate results across runs and environments.
	#[arg(long)]
	pub helm_cache: bool,

	/// Experimental: location to store the helmTemplate cache. Accepts a local
	/// path, a `file://` URL, or an `s3://bucket/prefix` URL. Defaults to
	/// `<output_dir>/helm-cache`. Setting this enables the helm cache.
	#[arg(long)]
	pub helm_cache_path: Option<String>,

	/// Experimental: what to do when loading or saving the helm cache fails.
	#[arg(long, default_value = "warn")]
	pub helm_cache_on_error: String,

	/// Regex filter on '<kind>/<name>'. See https://tanka.dev/output-filtering
	#[arg(short = 't', long)]
	pub target: Vec<String>,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,
}

/// Run the export command.
pub fn run<W: Write>(args: ExportArgs, mut writer: W) -> Result<()> {
	UnimplementedArgs {
		jsonnet_implementation: None,
		cache_envs: Some(&args.cache_envs),
		cache_path: Some(&args.cache_path),
		mem_ballast_size_bytes: Some(&args.mem_ballast_size_bytes),
	}
	.warn_if_set();

	let (paths, opts) = build_export_opts(args)?;
	let result = export_impl::export(&paths, opts)?;

	// Match tk behavior: silent on success, errors reported via the provided writer
	// But report fatal errors prominently and summarize skipped ones
	let mut fatal_error: Option<(Arc<PathBuf>, String)> = None;
	let mut env_errors = Vec::new();
	let mut skipped_count = 0;

	for env_result in &result.results {
		if let Some(ref error) = env_result.error {
			if error.starts_with("FATAL:") && fatal_error.is_none() {
				// Capture the first fatal error
				fatal_error = Some((env_result.env_path.clone(), error.clone()));
			} else if error == "Skipped due to earlier fatal error" {
				skipped_count += 1;
			} else {
				// Regular environment error
				env_errors.push((env_result.env_path.clone(), error.clone()));
			}
		}
	}

	// Report fatal error first if present
	if let Some((path, error)) = fatal_error {
		writeln!(writer, "\n{}", "=".repeat(80))?;
		writeln!(writer, "FATAL ERROR during export:")?;
		writeln!(writer, "{}", "=".repeat(80))?;
		writeln!(writer, "  Environment: {:?}", path)?;
		writeln!(
			writer,
			"  Error: {}",
			error.strip_prefix("FATAL: ").unwrap_or(&error)
		)?;
		writeln!(writer, "{}", "=".repeat(80))?;
		writeln!(writer)?;
	}

	// Report individual environment errors
	for (path, error) in &env_errors {
		writeln!(writer, "  ✗ {:?}: {}", path, error)?;
	}

	// Summarize skipped environments
	if skipped_count > 0 {
		writeln!(
			writer,
			"\nSkipped {} environments due to earlier fatal error",
			skipped_count
		)?;
	}

	if result.failed > 0 {
		anyhow::bail!("{} environments failed to export", result.failed);
	}

	Ok(())
}

fn build_export_opts(args: ExportArgs) -> Result<(Vec<PathBuf>, ExportOpts)> {
	let eval_opts = args.jsonnet.into_global_evaluator_options();

	// Parse merge strategy
	let merge_strategy = if let Some(ref strategy) = args.merge_strategy {
		strategy.parse::<ExportMergeStrategy>()?
	} else {
		ExportMergeStrategy::default()
	};

	let paths = args.paths;
	let opts = ExportOpts {
		output_dir: args.output_dir,
		extension: args.extension,
		format: args.format,
		parallelism: args.parallel as usize,
		eval_opts,
		name: args.name,
		recursive: args.recursive,
		selector: args.selector,
		skip_manifest: args.skip_manifest,
		target: args.target,
		merge_strategy,
		merge_deleted_envs: args.merge_deleted_envs,
		show_timing: false,
		helm_cache: args.helm_cache,
		helm_cache_path: args.helm_cache_path,
		helm_cache_on_error: args.helm_cache_on_error.parse()?,
	};
	Ok((paths, opts))
}
