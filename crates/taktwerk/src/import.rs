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

/// Run the import: print the proposal, or write it with `-o`.
///
/// # Errors
/// The header cannot be read or parsed, or the file exists without `--force`.
pub fn run(args: &Args) -> anyhow::Result<ExitCode> {
    let proposal = taktwerk_raw::import_header(&args.header)?;
    let text = proposal.to_toml()?;
    match &args.output {
        None => print!("{text}"),
        Some(file) => {
            if file.exists() && !args.force {
                anyhow::bail!("{} exists; pass --force to overwrite", file.display());
            }
            std::fs::write(file, text)
                .map_err(|e| anyhow::anyhow!("write {}: {e}", file.display()))?;
            eprintln!(
                "wrote {}: review its notes and set abi.confirmed = true",
                file.display()
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}
