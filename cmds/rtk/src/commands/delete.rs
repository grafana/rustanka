//! Delete command handler.

use std::{io::Write, path::PathBuf};

use anyhow::Result;
use clap::Args;

#[derive(Args)]
pub struct DeleteArgs {
	/// Path to delete
	pub path: PathBuf,

	/// Skip interactive approval. Only for automation! Allowed values: 'always', 'never', 'if-no-changes'
	#[arg(long)]
	pub auto_approve: Option<String>,

	/// Controls color in diff output, must be "auto", "always", or "never"
	#[arg(long, default_value = "auto")]
	pub color: String,

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

/// Run the delete command.
pub fn run<W: Write>(args: DeleteArgs, _writer: W) -> Result<()> {
	let _ = &args.jsonnet; // consume jsonnet args

	anyhow::bail!("not implemented")
}
