//! Export command handler.

use std::{io::Write, path::PathBuf};

use anyhow::Result;
use clap::Args;
use rtk_environments::export::{Exported, MergeStrategy, Options};
use rtk_environments::Engine;

use super::common::UnimplementedArgs;

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

	/// Regex filter on '<kind>/<name>'. See https://tanka.dev/output-filtering
	#[arg(short = 't', long)]
	pub target: Vec<String>,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,
}

impl ExportArgs {
	fn into_export_options(self) -> Result<(Vec<PathBuf>, Options, rtk_jsonnet::Options)> {
		let ExportArgs {
			output_dir,
			paths,
			extension,
			format,
			merge_deleted_envs,
			merge_strategy,
			name,
			parallel,
			recursive,
			selector,
			skip_manifest,
			target,
			jsonnet,
			cache_envs: _,
			cache_path: _,
			mem_ballast_size_bytes: _,
		} = self;

		let merge_strategy = merge_strategy
			.as_deref()
			.unwrap_or_default()
			.parse::<MergeStrategy>()?;

		let jsonnet = jsonnet.into_options();

		let options = Options {
			output_dir,
			extension,
			format,
			targets: target,
			merge_strategy,
			merge_deleted_environments: merge_deleted_envs,
			skip_manifest,
			timing: false,
			// Asking for fewer than one environment at a time is asking for one.
			parallelism: parallel.max(1) as usize,
			name,
			selector,
			recursive,
			working_directory: None,
		};

		Ok((paths, options, jsonnet))
	}
}

/// Run the export command.
pub fn run<W: Write>(args: ExportArgs, writer: W) -> Result<()> {
	run_in(args, writer, None)
}

/// Run the export command, resolving environments against a named directory.
///
/// Exporting re-resolves each environment against the working directory, the
/// way tk does. The golden harness exports a fixture staged into a temporary
/// directory and has to say so, since `chdir` is process-wide and the tests run
/// in parallel.
pub fn run_in<W: Write>(
	args: ExportArgs,
	mut writer: W,
	working_directory: Option<PathBuf>,
) -> Result<()> {
	UnimplementedArgs {
		jsonnet_implementation: None,
		cache_envs: Some(&args.cache_envs),
		cache_path: Some(&args.cache_path),
		mem_ballast_size_bytes: Some(&args.mem_ballast_size_bytes),
	}
	.warn_if_set();

	let (paths, mut options, jsonnet) = args.into_export_options()?;
	options.working_directory = working_directory;

	let engine = Engine::new(rtk_jsonnet::Engine::new(jsonnet));
	let exported = engine
		.export_bulk(paths, &options)
		.map_err(|error| anyhow::anyhow!("{}", error.report()))?;

	// tk says nothing here, and neither does the export itself. Finding no
	// environments at all is indistinguishable from a successful export of
	// nothing, though, and pointing at the wrong directory is an easy mistake.
	if exported.reports.is_empty() {
		tracing::warn!("no environments found; nothing was exported");
	}

	report(&exported, &mut writer)?;

	let failed = exported.failed();
	if failed > 0 {
		anyhow::bail!("{failed} environments failed to export");
	}

	Ok(())
}

/// Report what failed, tk-style: nothing at all when everything worked.
fn report<W: Write>(exported: &Exported, writer: &mut W) -> Result<()> {
	// One failure can be the reason for every other, in which case it is the one
	// worth reading and the rest would bury it.
	let fatal = exported
		.reports
		.iter()
		.position(|report| report.error.as_ref().is_some_and(|error| error.fatal()));

	if let Some(report) = fatal.map(|fatal| &exported.reports[fatal]) {
		let error = report.error.as_ref().expect("a fatal error");

		writeln!(writer, "\n{}", "=".repeat(80))?;
		writeln!(writer, "FATAL ERROR during export:")?;
		writeln!(writer, "{}", "=".repeat(80))?;
		writeln!(writer, "  Environment: {:?}", report.source)?;
		writeln!(writer, "  Error: {}", error.report())?;
		writeln!(writer, "{}", "=".repeat(80))?;
		writeln!(writer)?;
	}

	let mut skipped = 0;
	for (index, report) in exported.reports.iter().enumerate() {
		let Some(error) = &report.error else {
			continue;
		};

		if Some(index) == fatal {
			continue;
		}

		// An environment that never got its turn has nothing of its own to say.
		if error.skipped() {
			skipped += 1;
			continue;
		}

		writeln!(writer, "  ✗ {:?}: {}", report.source, error.report())?;
	}

	if skipped > 0 {
		writeln!(
			writer,
			"\nSkipped {skipped} environments due to earlier fatal error"
		)?;
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use clap::Parser;

	use super::ExportArgs;

	#[derive(Parser)]
	struct TestCli {
		#[command(flatten)]
		export: ExportArgs,
	}

	/// The flag lives on the shared Jsonnet arguments now, so every command that
	/// evaluates has it; this checks it still reaches the engine from export.
	#[test]
	fn passes_helm_cache_to_the_jsonnet_engine() {
		let args = TestCli::parse_from(["rtk", "out", "environment", "--helm-cache"]);
		let (_, _, jsonnet) = args.export.into_export_options().unwrap();
		assert!(jsonnet.helm_cache);

		let args = TestCli::parse_from(["rtk", "out", "environment"]);
		let (_, _, jsonnet) = args.export.into_export_options().unwrap();
		assert!(!jsonnet.helm_cache, "off unless asked for");
	}
}
