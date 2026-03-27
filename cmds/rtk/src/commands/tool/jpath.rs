//! Jpath subcommand handler.

use std::{io::Write, path::PathBuf};

use anyhow::Result;
use clap::Args;

#[derive(Args)]
pub struct JpathArgs {
	/// File or directory
	pub path: PathBuf,

	/// Show debug info
	#[arg(short = 'd', long)]
	pub debug: bool,
}

/// Run the jpath subcommand.
pub fn run<W: Write>(_args: JpathArgs, _writer: W) -> Result<()> {
	anyhow::bail!("not implemented")
}
