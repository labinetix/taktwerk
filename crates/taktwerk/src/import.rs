//! `taktwerk import-header`: propose a raw model descriptor from a C header.

use std::path::PathBuf;
use std::process::ExitCode;

/// Arguments of `import-header`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// C header declaring the model's functions and structs.
    pub header: PathBuf,
    /// Write the descriptor to this file instead of printing it.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Overwrite an existing file.
    #[arg(long)]
    pub force: bool,
}

/// Run the import.
///
/// # Errors
/// The import is not available in this build.
pub fn run(_args: &Args) -> anyhow::Result<ExitCode> {
    anyhow::bail!("header import is not available in this build")
}
