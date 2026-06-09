//! Export command handler.

use std::{
	fs,
	io::Write,
	path::{Path, PathBuf},
	sync::Arc,
};

use anyhow::{Context, Result};
use clap::Args;
use serde::Deserialize;

use super::common::UnimplementedArgs;
use crate::environments::export::{self as export_impl, ExportMergeStrategy, ExportOpts};

/// How to rank functions in the analysis report.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzeSortBy {
	/// Inclusive (recursion-corrected) time, descending.
	#[default]
	Total,
	/// Self time (excluding callees), descending.
	#[serde(rename = "self")]
	SelfTime,
	/// Call count, descending.
	Calls,
}

/// Configuration for `--analyze-config`, loaded from a JSON file.
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyzeConfig {
	/// Path to write the analysis report to (required).
	pub output_file: PathBuf,
	/// How to rank functions. Defaults to total time.
	#[serde(default)]
	pub sort_by: AnalyzeSortBy,
	/// Only include functions whose total (inclusive) time is at least this
	/// many milliseconds. Defaults to 0 (no filtering).
	#[serde(default)]
	pub total_time_threshold: f64,
	/// Only include functions whose self time is at least this many
	/// milliseconds. Defaults to 0 (no filtering).
	#[serde(default)]
	pub self_time_threshold: f64,
}

impl AnalyzeConfig {
	fn load(path: &Path) -> Result<Self> {
		let content = fs::read_to_string(path)
			.with_context(|| format!("reading analyze config {}", path.display()))?;
		serde_json::from_str(&content)
			.with_context(|| format!("parsing analyze config {}", path.display()))
	}
}

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

	/// Regex filter on '<kind>/<name>'. See https://tanka.dev/output-filtering
	#[arg(short = 't', long)]
	pub target: Vec<String>,

	/// Path to a JSON config enabling Jsonnet function-call profiling. The
	/// config requires `output_file` and optionally accepts `sort_by`
	/// ("total" (default), "self", or "calls"), `total_time_threshold`, and
	/// `self_time_threshold` (both in milliseconds, default 0).
	#[arg(long, value_name = "FILE")]
	pub analyze_config: Option<PathBuf>,

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

	let analyze_config = match &args.analyze_config {
		Some(path) => Some(AnalyzeConfig::load(path)?),
		None => None,
	};

	let (paths, opts) = build_export_opts(args)?;
	let result = export_impl::export(&paths, opts)?;

	if let Some(config) = &analyze_config {
		write_analysis_report(config)?;
		writeln!(
			writer,
			"Function analysis written to {}",
			config.output_file.display()
		)?;
	}

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
		analyze: args.analyze_config.is_some(),
	};
	Ok((paths, opts))
}

/// Format a nanosecond duration into a compact, human-readable string.
fn format_ns(ns: u128) -> String {
	if ns >= 1_000_000_000 {
		format!("{:.2}s", ns as f64 / 1_000_000_000.0)
	} else if ns >= 1_000_000 {
		format!("{:.2}ms", ns as f64 / 1_000_000.0)
	} else if ns >= 1_000 {
		format!("{:.2}µs", ns as f64 / 1_000.0)
	} else {
		format!("{ns}ns")
	}
}

/// Truncate `s` to `max` characters, keeping the right-hand side (most useful
/// for file paths, where the filename is at the end).
fn truncate_left(s: &str, max: usize) -> String {
	if s.chars().count() <= max {
		return s.to_string();
	}
	let tail: String = s
		.chars()
		.rev()
		.take(max - 1)
		.collect::<Vec<_>>()
		.into_iter()
		.rev()
		.collect();
	format!("…{tail}")
}

/// Truncate `s` to `max` characters, keeping the left-hand side.
fn truncate_right(s: &str, max: usize) -> String {
	if s.chars().count() <= max {
		return s.to_string();
	}
	let head: String = s.chars().take(max - 1).collect();
	format!("{head}…")
}

/// Write a ranked report of profiled Jsonnet function calls to the file
/// configured in `config`.
///
/// Functions are ranked per `config.sort_by` (default: total inclusive,
/// recursion-corrected time) and filtered by the configured time thresholds.
/// Call count, self time (excluding callees), and the defining file (when
/// known) are shown.
fn write_analysis_report(config: &AnalyzeConfig) -> Result<()> {
	let mut entries = jrsonnet_evaluator::profile::collect();
	jrsonnet_evaluator::profile::set_enabled(false);

	let total_threshold_ns = (config.total_time_threshold * 1_000_000.0).max(0.0) as u128;
	let self_threshold_ns = (config.self_time_threshold * 1_000_000.0).max(0.0) as u128;
	entries
		.retain(|e| e.stat.total_ns >= total_threshold_ns && e.stat.self_ns >= self_threshold_ns);

	match config.sort_by {
		AnalyzeSortBy::Total => entries.sort_by(|a, b| {
			b.stat
				.total_ns
				.cmp(&a.stat.total_ns)
				.then_with(|| b.stat.count.cmp(&a.stat.count))
		}),
		AnalyzeSortBy::SelfTime => entries.sort_by(|a, b| {
			b.stat
				.self_ns
				.cmp(&a.stat.self_ns)
				.then_with(|| b.stat.count.cmp(&a.stat.count))
		}),
		AnalyzeSortBy::Calls => entries.sort_by(|a, b| {
			b.stat
				.count
				.cmp(&a.stat.count)
				.then_with(|| b.stat.total_ns.cmp(&a.stat.total_ns))
		}),
	}

	let total_calls: u64 = entries.iter().map(|e| e.stat.count).sum();

	const NAME_W: usize = 32;
	const FILE_W: usize = 40;

	let mut out = String::new();
	out.push_str(&"=".repeat(96));
	out.push('\n');
	out.push_str(&format!(
		"Function analysis ({} functions shown, {total_calls} total calls, sorted by {:?})\n",
		entries.len(),
		config.sort_by,
	));
	out.push_str(&"=".repeat(96));
	out.push('\n');
	out.push_str(&format!(
		"{:<NAME_W$} {:<FILE_W$} {:>8} {:>10} {:>10}\n",
		"FUNCTION", "FILE", "CALLS", "TOTAL", "SELF"
	));
	out.push_str(&"-".repeat(96));
	out.push('\n');

	for entry in &entries {
		let file = entry.file.as_deref().unwrap_or("-");
		out.push_str(&format!(
			"{:<NAME_W$} {:<FILE_W$} {:>8} {:>10} {:>10}\n",
			truncate_right(&entry.name, NAME_W),
			truncate_left(file, FILE_W),
			entry.stat.count,
			format_ns(entry.stat.total_ns),
			format_ns(entry.stat.self_ns),
		));
	}
	out.push_str(&"=".repeat(96));
	out.push('\n');

	if let Some(parent) = config.output_file.parent() {
		if !parent.as_os_str().is_empty() {
			fs::create_dir_all(parent)
				.with_context(|| format!("creating output directory {}", parent.display()))?;
		}
	}
	fs::write(&config.output_file, out).with_context(|| {
		format!(
			"writing analysis report to {}",
			config.output_file.display()
		)
	})?;

	Ok(())
}
