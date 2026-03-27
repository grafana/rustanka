//! Status command handler.

use std::{io::Write, path::PathBuf};

use anyhow::Result;
use clap::Args;

#[derive(Args)]
pub struct StatusArgs {
	/// Path to check status
	pub path: PathBuf,

	/// String that only a single inline environment contains in its name
	#[arg(long)]
	pub name: Option<String>,

	/// Regex filter on '<kind>/<name>'. See https://tanka.dev/output-filtering
	#[arg(short = 't', long)]
	pub target: Vec<String>,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,
}

/// Run the status command.
pub fn run<W: Write>(args: StatusArgs, _writer: W) -> Result<()> {
	let _ = &args.jsonnet; // consume jsonnet args

	anyhow::bail!("not implemented")
}
