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
    /// The one function that serves as init and step (single entry point).
    #[arg(long)]
    pub entry: Option<String>,
    /// Which struct an opaque `char *`/`void *` parameter of the entry carries, as
    /// `<param>=<struct>`; repeatable.
    #[arg(long = "arg-struct", value_name = "PARAM=STRUCT", value_parser = parse_arg_struct)]
    pub arg_structs: Vec<(String, String)>,
}

/// `<param>=<struct>`.
fn parse_arg_struct(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((param, st)) if !param.is_empty() && !st.is_empty() => {
            Ok((param.to_owned(), st.to_owned()))
        }
        _ => Err(format!("`{text}`: expected <param>=<struct>")),
    }
}

/// Run the import: print the proposal, or write it with `-o`.
///
/// # Errors
/// The header cannot be read or parsed, or the file exists without `--force`.
pub fn run(args: &Args) -> anyhow::Result<ExitCode> {
    let options = taktwerk_raw::ImportOptions {
        entry: args.entry.clone(),
        arg_structs: args.arg_structs.clone(),
    };
    let proposal = taktwerk_raw::import_header_with(&args.header, &options)?;
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

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn arg_structs_parse() {
        assert_eq!(
            parse_arg_struct("in=blob_input").unwrap(),
            ("in".to_owned(), "blob_input".to_owned())
        );
        assert!(parse_arg_struct("in").is_err());
        assert!(parse_arg_struct("=x").is_err());
    }
}
