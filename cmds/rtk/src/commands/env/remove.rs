//! Env remove subcommand handler.

use std::{io::Write, path::PathBuf};

use anyhow::Result;
use clap::Args;

use super::shared;

#[derive(Args)]
pub struct RemoveArgs {
	/// Path(s) to the environment(s) to remove
	pub paths: Vec<PathBuf>,
}

/// Run the env remove subcommand.
pub fn run<W: Write>(args: RemoveArgs, _writer: W) -> Result<()> {
	shared::remove(&args.paths)
}
