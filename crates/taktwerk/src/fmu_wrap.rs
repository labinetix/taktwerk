//! `taktwerk fmu-wrap`: a confirmed raw model package as an FMI 3 co-simulation FMU.
//!
//! Dry run by default: prints the variables, structural parameters and targets the FMU would
//! have. `--write` generates the wrapper, compiles it per target and writes the `.fmu`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use taktwerk_raw::fmu::build::{BuildOptions, Target, build, plan};

use crate::summary;

/// Arguments of `fmu-wrap`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The raw model package (directory with `taktwerk-model.toml` and `lib/<arch>/`).
    pub package: PathBuf,
    /// The `.fmu` to write; default `<modelIdentifier>.fmu` in the working directory.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Target architecture (`aarch64`, `x86_64`), repeatable; default the host.
    ///
    /// Another target needs `zig` on PATH (`zig cc -target <arch>-linux-gnu.2.25`) and the
    /// package's library for it.
    #[arg(long = "target", value_name = "ARCH")]
    pub targets: Vec<String>,
    /// A further library to ship beside the model's, repeatable.
    ///
    /// A bare file name is taken from the package's `lib/<arch>/` per target, a path is copied
    /// as is; both land in `binaries/<arch>-linux/`.
    #[arg(long = "bundle", value_name = "LIB")]
    pub bundle: Vec<PathBuf>,
    /// Build and write the FMU instead of printing what it would contain.
    #[arg(long)]
    pub write: bool,
}

/// Run `fmu-wrap`.
///
/// # Errors
/// A descriptor, generation, compiler or packaging failure.
pub fn run(args: &Args) -> anyhow::Result<ExitCode> {
    let mut targets = args
        .targets
        .iter()
        .map(|t| Target::parse(t))
        .collect::<Result<Vec<_>, _>>()?;
    if targets.is_empty() {
        targets.push(Target::host()?);
    }
    let probe = BuildOptions {
        targets: targets.clone(),
        bundle: args.bundle.clone(),
        output: PathBuf::new(),
    };
    let (descriptor, wrapper) = plan(&args.package, &probe)?;
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{}.fmu", wrapper.model_identifier)));

    println!(
        "model {} -> {} (modelIdentifier {}, token {})",
        descriptor.interface.name,
        output.display(),
        wrapper.model_identifier,
        wrapper.token
    );
    println!(
        "co-simulation: variable step {}, reset {}, single instance {}",
        yes_no(wrapper.variable_step),
        yes_no(wrapper.resettable),
        yes_no(descriptor.interface.instances == taktwerk_core::model::Instances::Single)
    );
    if !wrapper.structural.is_empty() {
        println!("\nstructural parameters");
        let mut rows = vec![
            ["name", "vr", "start", "min", "max"]
                .map(str::to_owned)
                .to_vec(),
        ];
        for s in &wrapper.structural {
            rows.push(vec![
                s.name.clone(),
                s.vr.to_string(),
                s.start.to_string(),
                s.min.map_or("-".to_owned(), |v| v.to_string()),
                s.max.map_or("-".to_owned(), |v| v.to_string()),
            ]);
        }
        print!("{}", summary::table(&rows));
    }
    println!("\nvariables");
    let mut rows = vec![
        ["name", "vr", "type", "causality", "shape"]
            .map(str::to_owned)
            .to_vec(),
    ];
    for v in &wrapper.variables {
        rows.push(vec![
            v.name.clone(),
            v.vr.to_string(),
            v.fmi_type.clone(),
            v.causality.clone(),
            v.shape.clone(),
        ]);
    }
    print!("{}", summary::table(&rows));
    println!(
        "\ntargets: {}",
        targets
            .iter()
            .map(|t| t.platform().to_owned())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !args.bundle.is_empty() {
        println!(
            "bundled: {}",
            args.bundle
                .iter()
                .map(|b| b.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    if !args.write {
        println!("\ndry run: pass --write to build {}", output.display());
        return Ok(ExitCode::SUCCESS);
    }
    let report = build(
        &args.package,
        &BuildOptions {
            targets,
            bundle: args.bundle.clone(),
            output: output.clone(),
        },
    )
    .with_context(|| format!("building {}", output.display()))?;
    for (target, command) in &report.compiled {
        eprintln!("{}: {command}", target.platform());
    }
    eprintln!("wrote {}", report.fmu.display());
    Ok(ExitCode::SUCCESS)
}

const fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}
