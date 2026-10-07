//! Building the `.fmu`: compile the generated wrapper per target, pack it with the library.
//!
//! The host target compiles with `cc`; another target with `zig cc -target <arch>-linux-gnu.2.25`
//! when `zig` is on `PATH`. The wrapper, the package's library for that architecture and any
//! bundled dependency land in `binaries/<arch>-linux/`, the wrapper source and the FMI headers
//! in `sources/`.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::descriptor::{DESCRIPTOR_FILE, Descriptor};

use super::{FMI3_HEADERS, WrapOptions, Wrapper, generate};

/// A target architecture (Linux).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    /// `aarch64-linux`.
    Aarch64,
    /// `x86_64-linux`.
    X86_64,
}

impl Target {
    /// The running architecture.
    ///
    /// # Errors
    /// Not aarch64 or x86_64.
    pub fn host() -> Result<Self, BuildError> {
        Self::parse(std::env::consts::ARCH)
    }

    /// `aarch64` or `x86_64`.
    ///
    /// # Errors
    /// Anything else.
    pub fn parse(name: &str) -> Result<Self, BuildError> {
        match name {
            "aarch64" => Ok(Self::Aarch64),
            "x86_64" => Ok(Self::X86_64),
            other => Err(BuildError(format!(
                "target `{other}`: supported are aarch64 and x86_64"
            ))),
        }
    }

    /// Directory name under the package's `lib/`.
    #[must_use]
    pub const fn arch(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }

    /// FMI 3 platform directory under `binaries/`.
    #[must_use]
    pub const fn platform(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64-linux",
            Self::X86_64 => "x86_64-linux",
        }
    }
}

/// How to build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOptions {
    /// Targets to build; the host at least.
    pub targets: Vec<Target>,
    /// Further libraries to ship beside the model's: a bare file name is taken from the
    /// package's `lib/<arch>/` per target, a path is copied as is into every target.
    pub bundle: Vec<PathBuf>,
    /// The `.fmu` to write.
    pub output: PathBuf,
}

/// What a build did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildReport {
    /// The written `.fmu`.
    pub fmu: PathBuf,
    /// The generated contents.
    pub wrapper: Wrapper,
    /// Per target: the compiler command used.
    pub compiled: Vec<(Target, String)>,
}

/// A build failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("fmu-wrap: {0}")]
pub struct BuildError(pub String);

impl From<super::WrapError> for BuildError {
    fn from(e: super::WrapError) -> Self {
        Self(e.0)
    }
}

fn io(path: &Path, e: &std::io::Error) -> BuildError {
    BuildError(format!("{}: {e}", path.display()))
}

/// Load and validate the package's descriptor.
///
/// # Errors
/// Unreadable, malformed or unconfirmed.
pub fn load_descriptor(package: &Path) -> Result<Descriptor, BuildError> {
    let path = package.join(DESCRIPTOR_FILE);
    let text = fs::read_to_string(&path).map_err(|e| io(&path, &e))?;
    let d = Descriptor::parse(&text).map_err(|e| BuildError(format!("{}: {e}", path.display())))?;
    d.validate()
        .map_err(|e| BuildError(format!("{}: {e}", path.display())))?;
    Ok(d)
}

/// The library file for `target`: `abi.library`, or the only `.so` in `lib/<arch>/`.
///
/// # Errors
/// No such file, or several candidates.
pub fn library_for(package: &Path, d: &Descriptor, target: Target) -> Result<PathBuf, BuildError> {
    let dir = package.join("lib").join(target.arch());
    if let Some(name) = &d.abi.library {
        let path = dir.join(name);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(BuildError(format!("{}: not found", path.display())))
        };
    }
    let mut found: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| io(&dir, &e))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "so"))
        .collect();
    found.sort();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(BuildError(format!("{}: no .so file", dir.display()))),
        _ => Err(BuildError(format!(
            "{}: several .so files; set abi.library",
            dir.display()
        ))),
    }
}

fn file_name(path: &Path) -> Result<String, BuildError> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or_else(|| BuildError(format!("{}: no file name", path.display())))
}

/// The generated wrapper for `package`, as the dry run shows it: the host's library name and
/// the bundled names.
///
/// # Errors
/// A descriptor or generation problem.
pub fn plan(package: &Path, options: &BuildOptions) -> Result<(Descriptor, Wrapper), BuildError> {
    let d = load_descriptor(package)?;
    let plan = d.validate().map_err(|e| BuildError(e.to_string()))?;
    let host = options.targets.first().copied().unwrap_or(Target::host()?);
    let library = file_name(&library_for(package, &d, host)?)?;
    let bundled = options
        .bundle
        .iter()
        .map(|b| file_name(b))
        .collect::<Result<Vec<_>, _>>()?;
    let wrapper = generate(&d, &plan, &WrapOptions { library, bundled })?;
    Ok((d, wrapper))
}

/// The compiler for `target` on this host: `cc`, or `zig cc` for a cross build.
fn compiler(target: Target) -> Result<Vec<String>, BuildError> {
    if target == Target::host()? {
        return Ok(vec!["cc".to_owned()]);
    }
    let zig = which("zig").ok_or_else(|| {
        BuildError(format!(
            "target {}: cross-building needs `zig` on PATH (zig cc -target {}-linux-gnu.2.25); \
             not found",
            target.arch(),
            target.arch()
        ))
    })?;
    Ok(vec![
        zig.to_string_lossy().into_owned(),
        "cc".to_owned(),
        "-target".to_owned(),
        format!("{}-linux-gnu.2.25", target.arch()),
    ])
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Build the FMU of `package` into `options.output`.
///
/// # Errors
/// Descriptor, generation, compiler or packaging failures, each named.
pub fn build(package: &Path, options: &BuildOptions) -> Result<BuildReport, BuildError> {
    let (d, wrapper) = plan(package, options)?;
    let work = tempfile::Builder::new()
        .prefix("taktwerk-fmu-wrap-")
        .tempdir()
        .map_err(|e| BuildError(format!("temp dir: {e}")))?;
    let root = work.path();
    let write = |rel: &str, text: &str| -> Result<(), BuildError> {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io(parent, &e))?;
        }
        fs::write(&path, text).map_err(|e| io(&path, &e))
    };
    write("modelDescription.xml", &wrapper.model_description)?;
    let source_rel = format!("sources/{}.c", wrapper.model_identifier);
    write(&source_rel, &wrapper.source)?;
    for (name, text) in FMI3_HEADERS {
        write(&format!("sources/fmi3/{name}"), text)?;
    }
    write(
        "sources/README.txt",
        &format!(
            "{}.c is the FMI 3 wrapper taktwerk fmu-wrap generated from the package's \
             descriptor; it loads the model library from binaries/<platform>/.\n",
            wrapper.model_identifier
        ),
    )?;

    let mut compiled = Vec::new();
    let mut targets = options.targets.clone();
    targets.sort();
    targets.dedup();
    for target in targets {
        let library = library_for(package, &d, target)?;
        let bin = root.join("binaries").join(target.platform());
        fs::create_dir_all(&bin).map_err(|e| io(&bin, &e))?;
        let lib_name = file_name(&library)?;
        fs::copy(&library, bin.join(&lib_name)).map_err(|e| io(&library, &e))?;
        for b in &options.bundle {
            let source = if b.components().count() == 1 {
                package.join("lib").join(target.arch()).join(b)
            } else {
                b.clone()
            };
            let name = file_name(&source)?;
            fs::copy(&source, bin.join(name)).map_err(|e| io(&source, &e))?;
        }
        let out = bin.join(format!("{}.so", wrapper.model_identifier));
        let mut cmd = compiler(target)?;
        cmd.extend(
            [
                "-std=c11",
                "-shared",
                "-fPIC",
                "-O2",
                "-fvisibility=hidden",
                "-Wall",
                "-Wextra",
            ]
            .map(str::to_owned),
        );
        cmd.push(format!("-DMODEL_LIBRARY=\"{lib_name}\""));
        cmd.push(format!("-I{}", root.join("sources/fmi3").display()));
        cmd.push("-o".to_owned());
        cmd.push(out.to_string_lossy().into_owned());
        cmd.push(root.join(&source_rel).to_string_lossy().into_owned());
        cmd.push("-ldl".to_owned());
        cmd.push("-Wl,-rpath,$ORIGIN".to_owned());
        let (program, args) = cmd
            .split_first()
            .ok_or_else(|| BuildError("empty compiler command".to_owned()))?;
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| BuildError(format!("{program}: {e}")))?;
        let shown = cmd.join(" ");
        if !output.status.success() {
            return Err(BuildError(format!(
                "target {}: `{shown}` failed:\n{}",
                target.arch(),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        compiled.push((target, shown));
    }

    zip_dir(root, &options.output)?;
    Ok(BuildReport {
        fmu: options.output.clone(),
        wrapper,
        compiled,
    })
}

/// Zip `root` (deflated, relative names with `/`) into `fmu`.
fn zip_dir(root: &Path, fmu: &Path) -> Result<(), BuildError> {
    let file = fs::File::create(fmu).map_err(|e| io(fmu, &e))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    // An importer hands the wrapper `resources/` as its resource path; ship the directory.
    zip.add_directory("resources/", options)
        .map_err(|e| BuildError(format!("{}: {e}", fmu.display())))?;
    let mut files = Vec::new();
    walk(root, &mut files)?;
    files.sort();
    for path in files {
        let rel = path
            .strip_prefix(root)
            .map_err(|e| BuildError(e.to_string()))?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let bytes = fs::read(&path).map_err(|e| io(&path, &e))?;
        zip.start_file(rel, options)
            .map_err(|e| BuildError(format!("{}: {e}", fmu.display())))?;
        zip.write_all(&bytes)
            .map_err(|e| BuildError(format!("{}: {e}", fmu.display())))?;
    }
    zip.finish()
        .map_err(|e| BuildError(format!("{}: {e}", fmu.display())))?;
    Ok(())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), BuildError> {
    for entry in fs::read_dir(dir).map_err(|e| io(dir, &e))? {
        let path = entry.map_err(|e| io(dir, &e))?.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}
