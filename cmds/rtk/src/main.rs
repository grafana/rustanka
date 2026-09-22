use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use commands::common::BrokenPipeGuard;

mod commands;
mod k8s;
mod profiling;
mod telemetry;
#[cfg(test)]
pub mod test_utils;

#[cfg(all(feature = "mimalloc", not(feature = "system-alloc")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "rtk")]
#[command(about = "Tanka dummy CLI", long_about = None)]
#[command(version = env!("RTK_VERSION"))]
struct Cli {
	/// Log level (error, warn, info, debug, trace). Falls back to RUST_LOG env var.
	#[arg(long, global = true)]
	log_level: Option<tracing::Level>,

	#[command(subcommand)]
	command: Commands,
}

#[derive(Subcommand)]
enum Commands {
	/// Apply the configuration to the cluster
	Apply(commands::apply::ApplyArgs),

	/// Jsonnet as yaml
	Show(commands::show::ShowArgs),

	/// Differences between the configuration and the cluster
	Diff(commands::diff::DiffArgs),

	/// Delete resources removed from Jsonnet
	Prune(commands::prune::PruneArgs),

	/// Delete the environment from cluster
	Delete(commands::delete::DeleteArgs),

	/// Manipulate environments
	Env(commands::env::EnvArgs),

	/// Display an overview of the environment, including contents and metadata
	Status(commands::status::StatusArgs),

	/// Export environments found in path(s)
	Export(commands::export::ExportArgs),

	/// Format Jsonnet code
	Fmt(commands::fmt::FmtArgs),

	/// Lint Jsonnet code
	Lint(commands::lint::LintArgs),

	/// Evaluate the jsonnet to json
	Eval(commands::eval::EvalArgs),

	/// Create the directory structure
	Init(commands::init::InitArgs),

	/// Handy utilities for working with jsonnet
	Tool(commands::tool::ToolArgs),

	/// Validate manifests and configurations
	Validate(commands::validate::ValidateArgs),

	/// Install CLI completions
	Complete(commands::complete::CompleteArgs),
}

fn main() -> Result<()> {
	clap_complete::CompleteEnv::with_factory(Cli::command).complete();
	let cli = Cli::parse();

	let telemetry_guard = telemetry::init(cli.log_level)?;
	let profiling_guard = profiling::init()?;
	let result = run(cli);
	let shutdown_result = profiling_guard.shutdown();
	drop(telemetry_guard);
	let exit_code = result?;
	shutdown_result?;
	if exit_code != 0 {
		std::process::exit(exit_code);
	}
	Ok(())
}

fn run(cli: Cli) -> Result<i32> {
	let stdout = BrokenPipeGuard::new(std::io::stdout());

	match cli.command {
		Commands::Apply(args) => commands::apply::run(args, stdout).map(|_| 0),
		Commands::Show(args) => commands::show::run(args, stdout).map(|_| 0),
		Commands::Diff(args) => {
			if commands::diff::run(args, stdout)? {
				return Ok(commands::diff::EXIT_CODE_DIFF_FOUND);
			}
			Ok(0)
		}
		Commands::Prune(args) => commands::prune::run(args, stdout).map(|_| 0),
		Commands::Delete(args) => commands::delete::run(args, stdout).map(|_| 0),
		Commands::Env(args) => commands::env::run(args, stdout).map(|_| 0),
		Commands::Status(args) => commands::status::run(args, stdout).map(|_| 0),
		Commands::Export(args) => commands::export::run(args, stdout).map(|_| 0),
		Commands::Fmt(args) => {
			// `--test` with something to change exits 16 — the same code
			// `diff` uses, and the one `tk fmt --test` exits with.
			if commands::fmt::run(args, stdout)? {
				return Ok(commands::diff::EXIT_CODE_DIFF_FOUND);
			}
			Ok(0)
		}
		Commands::Lint(args) => commands::lint::run(args, stdout).map(|_| 0),
		Commands::Eval(args) => commands::eval::run(
			args.path.as_ref(),
			args.jsonnet.into_options(),
			args.eval.as_deref(),
			stdout,
		)
		.map(|_| 0),
		Commands::Init(args) => commands::init::run(args, stdout).map(|_| 0),
		Commands::Tool(args) => {
			if commands::tool::run(args, stdout)? {
				return Ok(commands::tool::imports::EXIT_CODE_REBUILD_REQUIRED);
			}
			Ok(0)
		}
		Commands::Validate(args) => commands::validate::run(args, stdout).map(|_| 0),
		Commands::Complete(args) => commands::complete::run(args, stdout).map(|_| 0),
	}
}
