//! Command-line arguments and dispatch.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};

use crate::kinds::ModelKind;
use crate::{fmu_wrap, import, inspect, run, scaffold, tui};

/// Fixed-step model execution engine for Linux with its own OPC UA server.
#[derive(Debug, Parser)]
#[command(name = "taktwerk", version, about)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Load a project, its models and connectors, resolve and bind it, print the plan.
    Check {
        /// Project file.
        project: PathBuf,
    },
    /// Run a project until SIGINT or SIGTERM.
    Run {
        /// Project file.
        project: PathBuf,
    },
    /// Print a model's interface: dimensions, variables, shapes.
    Inspect {
        /// Model path: an .fmu, an extracted FMU or a raw package directory.
        model: PathBuf,
        /// Model kind.
        #[arg(long, value_enum)]
        kind: ModelKind,
    },
    /// Scaffold a project file for one model; prints it unless asked to write.
    New {
        /// Model path.
        model: PathBuf,
        /// Model kind.
        #[arg(long, value_enum)]
        kind: ModelKind,
        /// Write the project to this file instead of printing it.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Write the project to `taktwerk.toml` (or `--output`).
        #[arg(long)]
        write: bool,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
        /// Base tick, milliseconds.
        #[arg(long, default_value_t = 10.0)]
        tick_ms: f64,
        /// Port of the own OPC UA server.
        #[arg(long, default_value_t = 4840)]
        port: u16,
    },
    /// Propose a raw model descriptor from a C header; prints it unless asked to write.
    ImportHeader(import::Args),
    /// Wrap a confirmed raw model package as an FMI 3 co-simulation FMU; a dry run unless
    /// asked to write.
    FmuWrap(fmu_wrap::Args),
    /// Monitor a running engine over OPC UA and edit its inputs and tunables.
    Tui {
        /// The engine's server, e.g. `opc.tcp://127.0.0.1:4840`.
        endpoint: String,
        /// Namespace URI of the signal nodes.
        #[arg(long, default_value = "urn:taktwerk")]
        namespace: String,
        /// The engine's `system_prefix`.
        #[arg(long, default_value = "taktwerk")]
        prefix: String,
    },
}

/// Run `command`.
///
/// # Errors
/// Anything that stops the command before it can report on its own.
pub fn dispatch(command: Command) -> anyhow::Result<ExitCode> {
    match command {
        Command::Check { project } => run::check(&project),
        Command::Run { project } => run::run(&project),
        Command::Inspect { model, kind } => {
            let adapter = kind.load(&model).map_err(anyhow::Error::msg)?;
            print!("{}", inspect::interface(kind.as_str(), adapter.interface()));
            Ok(ExitCode::SUCCESS)
        }
        Command::New {
            model,
            kind,
            output,
            write,
            force,
            tick_ms,
            port,
        } => new(&model, kind, output, write, force, tick_ms, port),
        Command::ImportHeader(args) => import::run(&args),
        Command::FmuWrap(args) => fmu_wrap::run(&args),
        Command::Tui {
            endpoint,
            namespace,
            prefix,
        } => tui::run(&endpoint, &namespace, &prefix),
    }
}

fn new(
    model: &Path,
    kind: ModelKind,
    output: Option<PathBuf>,
    write: bool,
    force: bool,
    tick_ms: f64,
    port: u16,
) -> anyhow::Result<ExitCode> {
    if !(tick_ms.is_finite() && tick_ms > 0.0) {
        bail!("--tick-ms must be a positive number");
    }
    let adapter = kind.load(model).map_err(anyhow::Error::msg)?;
    let iface = adapter.interface();
    let target = match (output, write) {
        (Some(path), _) => Some(path),
        (None, true) => Some(PathBuf::from("taktwerk.toml")),
        (None, false) => None,
    };
    let base = target
        .as_deref()
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let path = scaffold::relative(&std::path::absolute(model)?, &std::path::absolute(base)?);
    let stem = model
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let id = scaffold::id_from(if iface.name.is_empty() {
        stem
    } else {
        &iface.name
    });
    let text = scaffold::project(&scaffold::Scaffold {
        id,
        kind: kind.as_str(),
        path: path.to_string_lossy().into_owned(),
        interface: iface,
        tick_ms,
        port,
    });
    match target {
        None => print!("{text}"),
        Some(file) => {
            if file.exists() && !force {
                bail!("{} exists; pass --force to overwrite", file.display());
            }
            std::fs::write(&file, text).with_context(|| format!("write {}", file.display()))?;
            eprintln!("wrote {}", file.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}
