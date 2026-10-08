//! FMI 2 and FMI 3 co-simulation model adapter for taktwerk.
//!
//! [`FmuAdapter::load`] reads an `.fmu` or an extracted FMU directory and exposes it as a
//! [`ModelAdapter`]. FMI 3 structural parameters become the interface's dimensions; arrays sized
//! by them become symbolic shapes. An FMI 3 `String` variable is a `u8` buffer of literal
//! capacity (a `taktwerk` annotation names it, else [`DEFAULT_TEXT_CAPACITY`]), exchanged
//! NUL-terminated and truncated to the capacity. Model exchange, and Binary and Clock
//! variables, are refused.

mod description;
mod ffi;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use taktwerk_core::model::{
    Causality, InstanceSpec, ModelAdapter, ModelError, ModelInstance, ModelInterface, StepIo,
};
use taktwerk_core::value::{Buffer, Dim, ScalarType};
use tempfile::TempDir;

use description::ModelDescription;
pub use description::{DEFAULT_TEXT_CAPACITY, FmiVersion};

/// The FMU's files on disk: an extracted temp dir or a directory given by the caller.
#[derive(Debug)]
struct Root {
    path: PathBuf,
    _extracted: Option<TempDir>,
}

/// An FMU loaded for co-simulation.
pub struct FmuAdapter {
    desc: ModelDescription,
    interface: ModelInterface,
    root: Arc<Root>,
    binary: PathBuf,
    /// The library every instance shares when the FMU allows several per process.
    shared: Mutex<Option<Arc<ffi::Library>>>,
    /// Single-instance FMUs: whether an instance holds the library at `binary`.
    primary_taken: Arc<AtomicBool>,
}

impl std::fmt::Debug for FmuAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FmuAdapter")
            .field("model", &self.interface.name)
            .field("version", &self.desc.version)
            .field("binary", &self.binary)
            .finish_non_exhaustive()
    }
}

impl FmuAdapter {
    /// Load an `.fmu` archive (extracted to a private temp dir) or an extracted FMU directory,
    /// and pick the binary for the running platform.
    ///
    /// # Errors
    /// [`ModelError::Load`]: unreadable archive or description, no co-simulation interface, an
    /// unsupported variable type, or no binary for this platform.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ModelError> {
        let path = path.as_ref();
        let root = if path.is_dir() {
            Root {
                path: path.to_path_buf(),
                _extracted: None,
            }
        } else {
            extract(path)?
        };
        let xml_path = root.path.join("modelDescription.xml");
        let xml = std::fs::read_to_string(&xml_path)
            .map_err(|e| ModelError::Load(format!("{}: {e}", xml_path.display())))?;
        let desc = description::parse(&xml).map_err(ModelError::Load)?;
        let binary = find_binary(&root.path, &desc)?;
        Ok(Self {
            interface: desc.interface(),
            desc,
            root: Arc::new(root),
            binary,
            shared: Mutex::new(None),
            primary_taken: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The FMI version the FMU implements.
    #[must_use]
    pub const fn fmi_version(&self) -> FmiVersion {
        self.desc.version
    }

    /// Path of the shared library chosen for this platform.
    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// The library for a new instance, and what the instance must keep alive with it.
    fn library(&self) -> Result<(Arc<ffi::Library>, LibraryHold), ModelError> {
        if !self.desc.single_instance {
            let mut shared = self
                .shared
                .lock()
                .map_err(|_| ModelError::Load("library lock poisoned".into()))?;
            if let Some(lib) = shared.as_ref() {
                return Ok((Arc::clone(lib), LibraryHold::None));
            }
            let lib = Arc::new(ffi::Library::load(&self.binary, self.desc.version)?);
            *shared = Some(Arc::clone(&lib));
            return Ok((lib, LibraryHold::None));
        }
        if self
            .primary_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let guard = PrimaryGuard(Arc::clone(&self.primary_taken));
            let lib = ffi::Library::load(&self.binary, self.desc.version)?;
            return Ok((Arc::new(lib), LibraryHold::Primary(guard)));
        }
        // One path shares its globals: a further instance loads a private copy.
        let dir = tempfile::Builder::new()
            .prefix("taktwerk-fmu-copy-")
            .tempdir()
            .map_err(|e| ModelError::Load(format!("temp dir: {e}")))?;
        let copy = dir
            .path()
            .join(format!("{}.so", self.desc.model_identifier));
        std::fs::copy(&self.binary, &copy)
            .map_err(|e| ModelError::Load(format!("copy {}: {e}", self.binary.display())))?;
        let lib = ffi::Library::load(&copy, self.desc.version)?;
        Ok((Arc::new(lib), LibraryHold::Copy(dir)))
    }

    /// Every structural parameter's bound length, in declaration order.
    fn bind_dims(&self, spec: &InstanceSpec) -> Result<Vec<usize>, ModelError> {
        self.interface
            .dimensions
            .iter()
            .map(|d| {
                let n = spec
                    .dims
                    .get(&d.name)
                    .copied()
                    .or(d.default)
                    .ok_or_else(|| {
                        ModelError::Instantiate(format!("dimension {} is not bound", d.name))
                    })?;
                if d.min.is_some_and(|min| n < min) || d.max.is_some_and(|max| n > max) {
                    return Err(ModelError::Instantiate(format!(
                        "dimension {} = {n} outside [{}, {}]",
                        d.name,
                        d.min.map_or("-".into(), |v| v.to_string()),
                        d.max.map_or("-".into(), |v| v.to_string()),
                    )));
                }
                Ok(n)
            })
            .collect()
    }
}

/// Clears the primary flag of a single-instance FMU when its instance goes away.
struct PrimaryGuard(Arc<AtomicBool>);

impl Drop for PrimaryGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// What an instance keeps alive beside its library.
enum LibraryHold {
    None,
    Primary(#[allow(dead_code, reason = "held for its Drop")] PrimaryGuard),
    Copy(#[allow(dead_code, reason = "held for its Drop")] TempDir),
}

fn extract(path: &Path) -> Result<Root, ModelError> {
    let load = |e: &dyn std::fmt::Display| ModelError::Load(format!("{}: {e}", path.display()));
    let file = File::open(path).map_err(|e| load(&e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| load(&e))?;
    let dir = tempfile::Builder::new()
        .prefix("taktwerk-fmu-")
        .tempdir()
        .map_err(|e| load(&e))?;
    zip.extract(dir.path()).map_err(|e| load(&e))?;
    Ok(Root {
        path: dir.path().to_path_buf(),
        _extracted: Some(dir),
    })
}

/// Platform directories under `binaries/` to try, best first.
fn platform_dirs(version: FmiVersion) -> Result<&'static [&'static str], ModelError> {
    if std::env::consts::OS != "linux" {
        return Err(ModelError::Load("FMUs run on Linux only".into()));
    }
    Ok(match (version, std::env::consts::ARCH) {
        (FmiVersion::V3, "x86_64") => &["x86_64-linux"],
        (FmiVersion::V3, "aarch64") => &["aarch64-linux"],
        // FMI 2 names only linux32/linux64; tools use their own names for other architectures.
        (FmiVersion::V2, "x86_64") => &["linux64", "x86_64-linux"],
        (FmiVersion::V2, "aarch64") => &["aarch64-linux", "linuxaarch64", "linux-aarch64"],
        (_, arch) => {
            return Err(ModelError::Load(format!("unsupported architecture {arch}")));
        }
    })
}

fn find_binary(root: &Path, desc: &ModelDescription) -> Result<PathBuf, ModelError> {
    let file = format!("{}.so", desc.model_identifier);
    let binaries = root.join("binaries");
    let dirs = platform_dirs(desc.version)?;
    for dir in dirs {
        let candidate = binaries.join(dir).join(&file);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    let present: Vec<String> = std::fs::read_dir(&binaries)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    Err(ModelError::Load(format!(
        "{}: no {file} for {} (looked in {}; binaries has {})",
        desc.model_name,
        std::env::consts::ARCH,
        dirs.join(", "),
        if present.is_empty() {
            "nothing".to_owned()
        } else {
            present.join(", ")
        },
    )))
}

/// A one-element buffer holding `n` in an integer type.
fn index_buffer(ty: ScalarType, n: usize) -> Option<Buffer> {
    Some(match ty {
        ScalarType::U64 => Buffer::U64(vec![u64::try_from(n).ok()?]),
        ScalarType::U32 => Buffer::U32(vec![u32::try_from(n).ok()?]),
        ScalarType::U16 => Buffer::U16(vec![u16::try_from(n).ok()?]),
        ScalarType::U8 => Buffer::U8(vec![u8::try_from(n).ok()?]),
        ScalarType::I64 => Buffer::I64(vec![i64::try_from(n).ok()?]),
        ScalarType::I32 => Buffer::I32(vec![i32::try_from(n).ok()?]),
        ScalarType::I16 => Buffer::I16(vec![i16::try_from(n).ok()?]),
        ScalarType::I8 => Buffer::I8(vec![i8::try_from(n).ok()?]),
        ScalarType::F64 | ScalarType::F32 | ScalarType::Bool => return None,
    })
}

impl ModelAdapter for FmuAdapter {
    fn interface(&self) -> &ModelInterface {
        &self.interface
    }

    fn instantiate(&self, spec: &InstanceSpec) -> Result<Box<dyn ModelInstance>, ModelError> {
        let dims = self.bind_dims(spec)?;
        let bound = |name: &str| {
            self.interface
                .dimensions
                .iter()
                .position(|d| d.name == name)
                .and_then(|i| dims.get(i).copied())
        };

        // Value slots in interface order, with their bound lengths.
        let mut slots = Vec::with_capacity(self.desc.variables.len());
        for v in &self.desc.variables {
            let mut len = 1usize;
            for d in &v.shape {
                let n = match d {
                    Dim::Literal(n) => *n,
                    Dim::Symbol(s) => bound(s).ok_or_else(|| {
                        ModelError::Instantiate(format!("{}: unknown dimension {s}", v.name))
                    })?,
                };
                len = len.saturating_mul(n);
            }
            slots.push(Slot {
                vr: v.vr,
                ty: v.ty,
                len,
                causality: v.causality,
                text: v.text,
                scratch: if v.text { vec![0; len + 1] } else { Vec::new() },
            });
        }

        // Parameters must name a parameter or tunable and match its type and length.
        let mut params = Vec::with_capacity(spec.params.len());
        for (name, value) in &spec.params {
            let (index, slot) = self
                .desc
                .variables
                .iter()
                .zip(slots.iter().enumerate())
                .find(|(v, _)| &v.name == name)
                .map(|(_, s)| s)
                .filter(|(_, s)| matches!(s.causality, Causality::Parameter | Causality::Tunable))
                .ok_or_else(|| {
                    ModelError::Instantiate(format!("{name}: no parameter of that name"))
                })?;
            if value.ty() != slot.ty || value.len() != slot.len {
                return Err(ModelError::Instantiate(format!(
                    "{name}: expected {} {:?}, got {} {:?}",
                    slot.len,
                    slot.ty,
                    value.len(),
                    value.ty()
                )));
            }
            params.push((index, value));
        }

        let (lib, hold) = self.library()?;
        let mut fmu = ffi::Instance::new(
            lib,
            &spec.id,
            &self.desc.token,
            &self.root.path.join("resources"),
            tracing::enabled!(tracing::Level::DEBUG),
        )?;

        if !self.desc.structural.is_empty() {
            fmu.enter_configuration_mode()?;
            for (s, &n) in self.desc.structural.iter().zip(&dims) {
                let value = index_buffer(s.ty, n).ok_or_else(|| {
                    ModelError::Instantiate(format!("{} = {n} does not fit {:?}", s.name, s.ty))
                })?;
                fmu.set(s.vr, &value)?;
            }
            fmu.exit_configuration_mode()?;
        }
        for (index, value) in params {
            if let Some(slot) = slots.get_mut(index) {
                set_slot(&mut fmu, slot, value)?;
            }
        }

        let pick = |c: Causality| -> Vec<Slot> {
            slots.iter().filter(|s| s.causality == c).cloned().collect()
        };
        Ok(Box::new(FmuInstance {
            inputs: pick(Causality::Input),
            outputs: pick(Causality::Output),
            tunables: pick(Causality::Tunable),
            step_size: spec.step_size,
            state: State::Instantiated,
            fmu: Some(fmu),
            _hold: hold,
            _root: Arc::clone(&self.root),
        }))
    }
}

/// A variable's value reference and bound size.
#[derive(Debug, Clone)]
struct Slot {
    vr: u32,
    ty: ScalarType,
    len: usize,
    causality: Causality,
    /// A `String` variable: `len` is its capacity, the buffer a `u8` one.
    text: bool,
    /// Text slots: `len + 1` bytes for the NUL-terminated copy handed to `fmi3SetString`.
    scratch: Vec<u8>,
}

/// Write `buf` to the slot's variable. No allocation.
fn set_slot(fmu: &mut ffi::Instance, slot: &mut Slot, buf: &Buffer) -> Result<(), ModelError> {
    if !slot.text {
        return fmu.set(slot.vr, buf);
    }
    let Buffer::U8(bytes) = buf else {
        return Err(ModelError::Instantiate(format!(
            "value reference {}: a String variable takes a u8 buffer",
            slot.vr
        )));
    };
    fmu.set_string(slot.vr, bytes, &mut slot.scratch)
}

/// Read the slot's variable into `buf`. No allocation.
fn get_slot(fmu: &mut ffi::Instance, slot: &Slot, buf: &mut Buffer) -> Result<(), ModelError> {
    if !slot.text {
        return fmu.get(slot.vr, buf);
    }
    let Buffer::U8(bytes) = buf else {
        return Err(ModelError::Instantiate(format!(
            "value reference {}: a String variable fills a u8 buffer",
            slot.vr
        )));
    };
    fmu.get_string(slot.vr, bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Instantiated,
    Running,
    Done,
}

/// One running FMU instance.
struct FmuInstance {
    inputs: Vec<Slot>,
    outputs: Vec<Slot>,
    tunables: Vec<Slot>,
    step_size: f64,
    state: State,
    // Dropped (freed) before the library copy or primary flag it depends on.
    fmu: Option<ffi::Instance>,
    _hold: LibraryHold,
    _root: Arc<Root>,
}

fn check_buffers(what: &str, slots: &[Slot], bufs: &[Buffer]) -> Result<(), ModelError> {
    if slots.len() != bufs.len() {
        return Err(ModelError::Instantiate(format!(
            "{what}: expected {} buffers, got {}",
            slots.len(),
            bufs.len()
        )));
    }
    for (i, (s, b)) in slots.iter().zip(bufs).enumerate() {
        if s.ty != b.ty() || s.len != b.len() {
            return Err(ModelError::Instantiate(format!(
                "{what}[{i}]: expected {} {:?}, got {} {:?}",
                s.len,
                s.ty,
                b.len(),
                b.ty()
            )));
        }
    }
    Ok(())
}

/// The cycle path's guard against a buffer swapped after init. No allocation.
fn same_shape(s: &Slot, b: &Buffer) -> Result<(), ModelError> {
    if s.ty == b.ty() && s.len == b.len() {
        Ok(())
    } else {
        Err(ModelError::Call {
            call: "step",
            code: i64::from(s.vr),
            detail: String::new(),
        })
    }
}

const fn terminated(call: &'static str) -> ModelError {
    ModelError::Call {
        call,
        code: 0,
        detail: String::new(),
    }
}

impl FmuInstance {
    fn set_all(&mut self, tunables: bool, io: &StepIo) -> Result<(), ModelError> {
        let fmu = self.fmu.as_mut().ok_or(terminated("set"))?;
        let (slots, bufs) = if tunables {
            (&mut self.tunables, &io.tunables)
        } else {
            (&mut self.inputs, &io.inputs)
        };
        for (s, b) in slots.iter_mut().zip(bufs) {
            same_shape(s, b)?;
            set_slot(fmu, s, b)?;
        }
        Ok(())
    }

    fn get_all(&mut self, tunables: bool, io: &mut StepIo) -> Result<(), ModelError> {
        let fmu = self.fmu.as_mut().ok_or(terminated("get"))?;
        let (slots, bufs) = if tunables {
            (&self.tunables, &mut io.tunables)
        } else {
            (&self.outputs, &mut io.outputs)
        };
        for (s, b) in slots.iter().zip(bufs.iter_mut()) {
            same_shape(s, b)?;
            get_slot(fmu, s, b)?;
        }
        Ok(())
    }
}

impl ModelInstance for FmuInstance {
    /// Also reads back outputs and tunables, so `io` holds the model's values after init.
    fn init(&mut self, start_time: f64, io: &mut StepIo) -> Result<(), ModelError> {
        if self.state != State::Instantiated {
            return Err(ModelError::Call {
                call: "init",
                code: 0,
                detail: "instance already initialized or terminated".to_owned(),
            });
        }
        check_buffers("inputs", &self.inputs, &io.inputs)?;
        check_buffers("outputs", &self.outputs, &io.outputs)?;
        check_buffers("tunables", &self.tunables, &io.tunables)?;
        if io.tunables_changed {
            self.set_all(true, io)?;
        }
        let fmu = self.fmu.as_mut().ok_or(terminated("init"))?;
        fmu.enter_initialization_mode(start_time)?;
        self.set_all(false, io)?;
        let fmu = self.fmu.as_mut().ok_or(terminated("init"))?;
        fmu.exit_initialization_mode()?;
        self.state = State::Running;
        self.get_all(false, io)?;
        self.get_all(true, io)
    }

    fn step(&mut self, time: f64, io: &mut StepIo) -> Result<(), ModelError> {
        if self.state != State::Running {
            return Err(terminated("step"));
        }
        if io.tunables_changed {
            self.set_all(true, io)?;
        }
        self.set_all(false, io)?;
        let h = self.step_size;
        self.fmu
            .as_mut()
            .ok_or(terminated("step"))?
            .do_step(time, h)?;
        self.get_all(false, io)
    }

    fn terminate(&mut self) {
        if let Some(mut fmu) = self.fmu.take()
            && self.state == State::Running
            && let Err(e) = fmu.terminate()
        {
            tracing::warn!("terminate: {e}");
        }
        // Dropping `fmu` frees the instance.
        self.state = State::Done;
    }
}
