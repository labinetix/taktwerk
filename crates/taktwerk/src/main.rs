//! The `taktwerk` command: check, run, inspect and scaffold model projects, and monitor a
//! running engine.
//!
//! ```text
//! taktwerk new models/plant.fmu --kind fmi -o plant.toml
//! taktwerk check plant.toml
//! taktwerk run plant.toml
//! taktwerk tui opc.tcp://127.0.0.1:4840
//! taktwerk fmu-wrap models/plant-pkg --write
//! ```
//!
//! `run` serves the process image on the project's OPC UA server, stops on SIGINT or SIGTERM and
//! exits non-zero when a model or connector failed. Logging goes to stderr and follows `RUST_LOG`
//! (default `info` for `run`, `warn` otherwise).

mod cli;
mod fmu_wrap;
mod import;
mod inspect;
mod kinds;
mod run;
mod scaffold;
mod setup;
mod summary;
mod tui;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Tui { .. } => {}
        cli::Command::Run { .. } => init_logging("info"),
        _ => init_logging("warn"),
    }
    match cli::dispatch(cli.command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Log to stderr, filtered by `RUST_LOG`, else at `level`.
fn init_logging(level: &str) {
    // Without a secure endpoint the server has no certificate, which the OPC UA stack reports as
    // an error on every connection and a warning at start; keep the stack quiet otherwise.
    let default = format!(
        "{level},opcua_server=error,opcua_core=warn,opcua_crypto=warn,\
         opcua_crypto::certificate_store=off,opcua_core::comms::secure_channel=off"
    );
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
