//! The adapter: a loaded package ([`RawModel`]) and its instances.
//!
//! An instance owns every byte the library sees: one buffer per variable, one image per struct
//! and one storage cell per pointer that carries a `const`, `phase` or `dim`, allocated at
//! `instantiate` and never resized. Each call re-derives the addresses it hands over from those
//! buffers, syncs by-value members, calls, and syncs outputs back; nothing on that path
//! allocates.
//!
//! Lengths a library reports (`reported = true`) are zeroed before init and compared with the
//! bound lengths after init and after every step; the comparison reads a few integers and
//! allocates only to word the error.

use core::ffi::c_void;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use taktwerk_core::model::{
    Causality, InstanceSpec, Instances, ModelAdapter, ModelError, ModelInstance, ModelInterface,
    StepIo,
};
use taktwerk_core::value::{Buffer, Dim, Mismatch, ScalarType};
use tempfile::NamedTempFile;

use crate::descriptor::{
    ArgRole, Builtin, CValue, DESCRIPTOR_FILE, Descriptor, Handle, MemberRole, Phased, Plan,
    ResolvedCall, Returns,
};
use crate::ffi::{self, Lib, RawFn, Regs};

/// Directory name under `lib/` for the running architecture.
#[must_use]
pub const fn arch_dir() -> &'static str {
    std::env::consts::ARCH
}

/// A loaded model package.
#[derive(Debug)]
pub struct RawModel {
    dir: PathBuf,
    library_path: PathBuf,
    descriptor: Descriptor,
    plan: Arc<Plan>,
    library: Arc<Lib>,
    /// `single` only: whether an instance already runs on the library at its own path.
    direct_taken: AtomicBool,
}

impl RawModel {
    /// Load the package at `dir`: parse and validate the descriptor, open the library for the
    /// running architecture and check that every declared symbol exists.
    ///
    /// # Errors
    /// [`ModelError::Load`] naming the file or symbol.
    pub fn load(dir: &Path) -> Result<Self, ModelError> {
        let load = |s: String| ModelError::Load(s);
        let descriptor_path = dir.join(DESCRIPTOR_FILE);
        let text = std::fs::read_to_string(&descriptor_path)
            .map_err(|e| load(format!("{}: {e}", descriptor_path.display())))?;
        let descriptor = Descriptor::parse(&text)
            .map_err(|e| load(format!("{}: {e}", descriptor_path.display())))?;
        let plan = descriptor
            .validate()
            .map_err(|e| load(format!("{}: {e}", descriptor_path.display())))?;
        let library_path = find_library(dir, descriptor.abi.library.as_deref())?;
        let library = Lib::open(&library_path).map_err(load)?;
        for call in [Some(&plan.init), Some(&plan.step), plan.terminate.as_ref()]
            .into_iter()
            .flatten()
        {
            library.function(&call.symbol).map_err(load)?;
        }
        Ok(Self {
            dir: dir.to_owned(),
            library_path,
            descriptor,
            plan: Arc::new(plan),
            library: Arc::new(library),
            direct_taken: AtomicBool::new(false),
        })
    }

    /// The package directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The library file loaded for this architecture.
    #[must_use]
    pub fn library_path(&self) -> &Path {
        &self.library_path
    }

    /// The parsed descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// The library an instance runs on: shared for `multiple`, shared once and then a private
    /// copy per further instance for `single`.
    fn library_for_instance(&self) -> Result<(Arc<Lib>, Option<NamedTempFile>), ModelError> {
        if self.descriptor.interface.instances == Instances::Multiple
            || !self.direct_taken.swap(true, Ordering::AcqRel)
        {
            return Ok((Arc::clone(&self.library), None));
        }
        let fail = |e: String| ModelError::Instantiate(format!("private library copy: {e}"));
        let mut copy = tempfile::Builder::new()
            .prefix("taktwerk-raw-")
            .suffix(".so")
            .tempfile()
            .map_err(|e| fail(e.to_string()))?;
        let mut source = File::open(&self.library_path)
            .map_err(|e| fail(format!("{}: {e}", self.library_path.display())))?;
        std::io::copy(&mut source, copy.as_file_mut()).map_err(|e| fail(e.to_string()))?;
        let lib = Lib::open(copy.path()).map_err(fail)?;
        Ok((Arc::new(lib), Some(copy)))
    }
}

/// `lib/<arch>/<name>.so`, or the only `.so` in that directory.
fn find_library(dir: &Path, name: Option<&str>) -> Result<PathBuf, ModelError> {
    let lib_dir = dir.join("lib").join(arch_dir());
    if let Some(name) = name {
        let path = lib_dir.join(name);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(ModelError::Load(format!("{}: not found", path.display())))
        };
    }
    let entries = std::fs::read_dir(&lib_dir)
        .map_err(|e| ModelError::Load(format!("{}: {e}", lib_dir.display())))?;
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "so"))
        .collect();
    found.sort();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(ModelError::Load(format!(
            "{}: no .so file",
            lib_dir.display()
        ))),
        _ => Err(ModelError::Load(format!(
            "{}: several .so files; set abi.library",
            lib_dir.display()
        ))),
    }
}

impl ModelAdapter for RawModel {
    fn interface(&self) -> &ModelInterface {
        &self.descriptor.interface
    }

    fn instantiate(&self, spec: &InstanceSpec) -> Result<Box<dyn ModelInstance>, ModelError> {
        let fail = |s: String| ModelError::Instantiate(format!("{}: {s}", spec.id));
        let iface = &self.descriptor.interface;

        // Dimensions: every declared one bound or defaulted, within range; nothing undeclared.
        let mut dims = Vec::with_capacity(iface.dimensions.len());
        for d in &iface.dimensions {
            let len = spec
                .dims
                .get(&d.name)
                .copied()
                .or(d.default)
                .ok_or_else(|| fail(format!("dimension {} is not bound", d.name)))?;
            if d.min.is_some_and(|m| len < m) || d.max.is_some_and(|m| len > m) {
                return Err(fail(format!(
                    "dimension {} = {len} is outside [{}, {}]",
                    d.name,
                    d.min.map_or("-".to_owned(), |m| m.to_string()),
                    d.max.map_or("-".to_owned(), |m| m.to_string()),
                )));
            }
            dims.push(len);
        }
        for name in spec.dims.keys() {
            if !iface.dimensions.iter().any(|d| &d.name == name) {
                return Err(fail(format!("unknown dimension {name}")));
            }
        }

        // One buffer per variable.
        let mut arrays = Vec::with_capacity(iface.variables.len());
        for v in &iface.variables {
            let mut len = 1_usize;
            for dim in &v.shape {
                let n = match dim {
                    Dim::Literal(n) => *n,
                    Dim::Symbol(s) => iface
                        .dimensions
                        .iter()
                        .position(|d| &d.name == s)
                        .map(|i| dims[i])
                        .ok_or_else(|| {
                            fail(format!("variable {}: unknown dimension {s}", v.name))
                        })?,
                };
                len = len
                    .checked_mul(n)
                    .ok_or_else(|| fail(format!("variable {}: shape overflow", v.name)))?;
            }
            arrays.push(CArray::zeroed(v.ty, len));
        }

        // Start values for parameters and tunables.
        for (name, value) in &spec.params {
            let idx = iface
                .variables
                .iter()
                .position(|v| &v.name == name)
                .ok_or_else(|| fail(format!("unknown parameter {name}")))?;
            let v = &iface.variables[idx];
            if !matches!(v.causality, Causality::Parameter | Causality::Tunable) {
                return Err(fail(format!("{name} is not a parameter or tunable")));
            }
            arrays[idx].load(value).map_err(|_| {
                fail(format!(
                    "parameter {name}: expected {:?}[{}], got {:?}[{}]",
                    v.ty,
                    arrays[idx].len(),
                    value.ty(),
                    value.len()
                ))
            })?;
        }

        // Dimension lengths must fit the C integer types they are passed as.
        let plan = &self.plan;
        let check_dim = |idx: usize, ty: ScalarType| -> Result<(), ModelError> {
            if int_fits(ty, dims[idx]) {
                Ok(())
            } else {
                Err(fail(format!(
                    "dimension {} = {} does not fit a {ty:?}",
                    iface.dimensions[idx].name, dims[idx]
                )))
            }
        };
        for s in &plan.structs {
            for m in &s.members {
                if let MemberRole::Dim(idx, ty)
                | MemberRole::DimPointer { dim: idx, ty, .. }
                | MemberRole::Reported { dim: idx, ty, .. } = *m
                {
                    check_dim(idx, ty)?;
                }
            }
        }
        for c in [Some(&plan.init), Some(&plan.step), plan.terminate.as_ref()]
            .into_iter()
            .flatten()
        {
            for a in &c.args {
                if let ArgRole::Dim(idx, ty) = *a {
                    check_dim(idx, ty)?;
                }
            }
        }

        let images = plan
            .structs
            .iter()
            .map(|s| Image::zeroed(s.layout.size(), s.layout.align()))
            .collect();

        let (library, private_copy) = self.library_for_instance()?;
        let bind = |c: &ResolvedCall| -> Result<BoundCall, ModelError> {
            Ok(BoundCall {
                f: library.function(&c.symbol).map_err(fail)?,
                returns: c.returns,
            })
        };
        let init = bind(&plan.init)?;
        let step = bind(&plan.step)?;
        let terminate = plan.terminate.as_ref().map(bind).transpose()?;

        Ok(Box::new(RawInstance {
            id: spec.id.clone(),
            plan: Arc::clone(plan),
            ok_codes: self.descriptor.abi.ok_codes.clone(),
            dims,
            dim_names: iface.dimensions.iter().map(|d| d.name.clone()).collect(),
            step_size: spec.step_size,
            arrays,
            images,
            cells: vec![0; plan.cells],
            handle: 0,
            init,
            step,
            terminate,
            terminated: false,
            _library: library,
            _private_copy: private_copy,
        }))
    }
}

/// Whether `len` is representable as the C integer type `ty`.
fn int_fits(ty: ScalarType, len: usize) -> bool {
    let len = len as u64;
    match ty {
        ScalarType::I64 | ScalarType::U64 => len <= i64::MAX as u64,
        ScalarType::I32 => len <= i32::MAX as u64,
        ScalarType::U32 => len <= u64::from(u32::MAX),
        ScalarType::I16 => len <= i16::MAX as u64,
        ScalarType::U16 => len <= u64::from(u16::MAX),
        ScalarType::I8 => len <= i8::MAX as u64,
        ScalarType::U8 => len <= u64::from(u8::MAX),
        ScalarType::F64 | ScalarType::F32 | ScalarType::Bool => false,
    }
}

/// A call bound to a loaded symbol; its arguments stay in the plan.
#[derive(Debug, Clone, Copy)]
struct BoundCall {
    f: RawFn,
    returns: Returns,
}

/// One running instance.
struct RawInstance {
    id: String,
    plan: Arc<Plan>,
    ok_codes: Vec<i32>,
    /// Bound length per declared dimension.
    dims: Vec<usize>,
    /// Declared dimension names, for messages.
    dim_names: Vec<String>,
    step_size: f64,
    /// One buffer per variable, interface order.
    arrays: Vec<CArray>,
    /// One image per struct, plan order.
    images: Vec<Image>,
    /// Storage cells pointer members and arguments point at, one scalar each.
    cells: Vec<u64>,
    /// The library's instance handle (`handle = "out"`), as an address.
    handle: usize,
    init: BoundCall,
    step: BoundCall,
    terminate: Option<BoundCall>,
    terminated: bool,
    /// Keeps the symbols alive. Field order: the library is closed before its private copy
    /// is unlinked.
    _library: Arc<Lib>,
    _private_copy: Option<NamedTempFile>,
}

impl RawInstance {
    /// Inputs (and tunables, when changed) from `io` into the C buffers.
    fn copy_in(&mut self, io: &StepIo, call: &'static str) -> Result<(), ModelError> {
        let io_error = |what: &str| {
            ModelError::Instantiate(format!(
                "{}: {call}: {what} buffers do not match the bound interface",
                self.id
            ))
        };
        if io.inputs.len() != self.plan.inputs.len() {
            return Err(io_error("input"));
        }
        for (slot, src) in self.plan.inputs.iter().zip(&io.inputs) {
            self.arrays[*slot]
                .load(src)
                .map_err(|_| io_error("input"))?;
        }
        if io.tunables_changed {
            if io.tunables.len() != self.plan.tunables.len() {
                return Err(io_error("tunable"));
            }
            for (slot, src) in self.plan.tunables.iter().zip(&io.tunables) {
                self.arrays[*slot]
                    .load(src)
                    .map_err(|_| io_error("tunable"))?;
            }
        }
        Ok(())
    }

    /// Outputs from the C buffers into `io`.
    fn copy_out(&self, io: &mut StepIo, call: &'static str) -> Result<(), ModelError> {
        if io.outputs.len() != self.plan.outputs.len() {
            return Err(ModelError::Instantiate(format!(
                "{}: {call}: output buffers do not match the bound interface",
                self.id
            )));
        }
        for (slot, dst) in self.plan.outputs.iter().zip(&mut io.outputs) {
            self.arrays[*slot].store(dst).map_err(|_| {
                ModelError::Instantiate(format!(
                    "{}: {call}: output buffers do not match the bound interface",
                    self.id
                ))
            })?;
        }
        Ok(())
    }

    /// Write the storage cells of every struct and of `args` for `phase`. Runs before any
    /// address of a cell is taken for this call.
    fn sync_cells(&mut self, args: &[ArgRole], phase: Phase) -> Result<(), Mismatch> {
        let plan = Arc::clone(&self.plan);
        let cells = &mut self.cells;
        let mut set = |cell: usize, value: CValue| -> Result<(), Mismatch> {
            let slot = cells.get_mut(cell).ok_or(Mismatch)?;
            let mut bytes = [0_u8; 8];
            let width = scalar_width(value.ty);
            if !value.write(bytes.get_mut(..width).ok_or(Mismatch)?) {
                return Err(Mismatch);
            }
            *slot = u64::from_ne_bytes(bytes);
            Ok(())
        };
        for s in &plan.structs {
            for role in &s.members {
                match *role {
                    MemberRole::Fixed {
                        value,
                        cell: Some(c),
                    } => set(c, phase.pick(value))?,
                    MemberRole::DimPointer { dim, ty, cell } => {
                        set(cell, dim_value(ty, self.dims[dim]))?;
                    }
                    MemberRole::Reported {
                        cell: Some(c), ty, ..
                    } if phase == Phase::Init => {
                        set(c, dim_value(ty, 0))?;
                    }
                    _ => {}
                }
            }
        }
        for a in args {
            if let ArgRole::Fixed {
                value,
                cell: Some(c),
            } = *a
            {
                set(c, phase.pick(value))?;
            }
        }
        Ok(())
    }

    /// Address of storage cell `cell`. `Vec::as_mut_ptr` materializes no reference, so
    /// addresses taken here stay valid together.
    fn cell_addr(&mut self, cell: usize) -> usize {
        self.cells
            .as_mut_ptr()
            .wrapping_add(cell)
            .expose_provenance()
    }

    /// Write every struct image: pointers, dimension lengths, builtins, fixed values and
    /// non-output values.
    fn sync_structs_before(&mut self, time: f64, phase: Phase) -> Result<(), Mismatch> {
        let plan = Arc::clone(&self.plan);
        for (si, s) in plan.structs.iter().enumerate() {
            for (member, role) in s.layout.members().iter().zip(&s.members) {
                let is_output = |v: usize| plan.outputs.contains(&v);
                let cell = match *role {
                    MemberRole::Fixed { cell: Some(c), .. }
                    | MemberRole::DimPointer { cell: c, .. }
                    | MemberRole::Reported { cell: Some(c), .. } => Some(self.cell_addr(c)),
                    _ => None,
                };
                let array = match *role {
                    MemberRole::Pointer(Some(v)) => Some(self.arrays[v].addr()),
                    _ => None,
                };
                let image = self.images.get_mut(si).ok_or(Mismatch)?;
                let slot = image.bytes_mut().get_mut(member.range()).ok_or(Mismatch)?;
                if let Some(addr) = cell.or(array) {
                    slot.copy_from_slice(&addr.to_ne_bytes());
                    continue;
                }
                match *role {
                    MemberRole::Pointer(_) => slot.fill(0),
                    MemberRole::Value(v) if !is_output(v) => self.arrays[v].first_bytes(slot)?,
                    MemberRole::Dim(d, ty) => int_bytes(ty, self.dims[d] as u64, slot)?,
                    MemberRole::Builtin(b) => {
                        let value = match b {
                            Builtin::StepSize => self.step_size,
                            Builtin::Time => time,
                        };
                        slot.copy_from_slice(&value.to_ne_bytes());
                    }
                    MemberRole::Fixed { value, .. } => {
                        if !phase.pick(value).write(slot) {
                            return Err(Mismatch);
                        }
                    }
                    MemberRole::Reported { .. } if phase == Phase::Init => slot.fill(0),
                    MemberRole::Value(_)
                    | MemberRole::Scratch
                    | MemberRole::Reported { .. }
                    | MemberRole::DimPointer { .. } => {}
                }
            }
        }
        Ok(())
    }

    /// Compare every reported length with its bound length.
    fn check_reported(&self, call: &'static str) -> Result<(), ModelError> {
        for (s, image) in self.plan.structs.iter().zip(&self.images) {
            for (member, role) in s.layout.members().iter().zip(&s.members) {
                let MemberRole::Reported { dim, ty, cell } = *role else {
                    continue;
                };
                let width = scalar_width(ty);
                let cell_bytes;
                let bytes = match cell {
                    Some(c) => {
                        cell_bytes = self.cells.get(c).map(|v| v.to_ne_bytes());
                        cell_bytes.as_ref().and_then(|b| b.get(..width))
                    }
                    None => image.bytes().get(member.range()),
                };
                let got = bytes.and_then(|b| read_int(ty, b));
                let bound = self.dims[dim];
                if got != Some(bound as i128) {
                    return Err(ModelError::Instantiate(format!(
                        "{}: {call}: the library reports {}.{} = {} but dimension {} is bound to {bound}",
                        self.id,
                        s.name,
                        member.name,
                        got.map_or_else(|| "?".to_owned(), |g| g.to_string()),
                        self.plan_dim_name(dim),
                    )));
                }
            }
        }
        Ok(())
    }

    fn plan_dim_name(&self, dim: usize) -> String {
        self.dim_names.get(dim).cloned().unwrap_or_default()
    }

    /// Read every by-value output member back into its buffer.
    fn sync_structs_after(&mut self) -> Result<(), Mismatch> {
        let plan = Arc::clone(&self.plan);
        for (s, image) in plan.structs.iter().zip(&mut self.images) {
            for (member, role) in s.layout.members().iter().zip(&s.members) {
                if let MemberRole::Value(v) = *role {
                    if plan.outputs.contains(&v) {
                        let slot = image.bytes_mut().get(member.range()).ok_or(Mismatch)?;
                        self.arrays[v].set_first_bytes(slot)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Fill the register images of one call from the current buffers.
    fn regs(&mut self, args: &[ArgRole], time: f64, phase: Phase) -> Regs {
        let mut regs = Regs::default();
        let (mut ni, mut nf) = (0_usize, 0_usize);
        let mut push_int = |regs: &mut Regs, v: u64| {
            if let Some(slot) = regs.ints.get_mut(ni) {
                *slot = v;
            }
            ni += 1;
        };
        let mut push_float = |regs: &mut Regs, v: f64| {
            if let Some(slot) = regs.floats.get_mut(nf) {
                *slot = v;
            }
            nf += 1;
        };
        for a in args {
            match *a {
                ArgRole::Struct(i) => push_int(&mut regs, self.images[i].addr() as u64),
                ArgRole::Array(v) => push_int(&mut regs, self.arrays[v].addr() as u64),
                ArgRole::Value(v) => match self.arrays[v].first_reg() {
                    Reg::Int(x) => push_int(&mut regs, x),
                    Reg::Float(x) => push_float(&mut regs, x),
                },
                ArgRole::Dim(d, _) => push_int(&mut regs, self.dims[d] as u64),
                ArgRole::Builtin(Builtin::StepSize) => push_float(&mut regs, self.step_size),
                ArgRole::Builtin(Builtin::Time) => push_float(&mut regs, time),
                ArgRole::Handle(Handle::In) => push_int(&mut regs, self.handle as u64),
                ArgRole::Handle(Handle::Out) => {
                    let slot: *mut usize = &mut self.handle;
                    push_int(&mut regs, slot.expose_provenance() as u64);
                }
                ArgRole::Fixed { cell: Some(c), .. } => {
                    let addr = self.cell_addr(c);
                    push_int(&mut regs, addr as u64);
                }
                ArgRole::Fixed { value, cell: None } => match phase.pick(value).reg() {
                    (true, bits) => push_float(&mut regs, f64::from_bits(bits)),
                    (false, bits) => push_int(&mut regs, bits),
                },
            }
        }
        regs
    }

    /// One full call: sync in, call, check the return code, sync out.
    fn invoke(
        &mut self,
        which: Phase,
        name: &'static str,
        time: f64,
        io: &mut StepIo,
    ) -> Result<(), ModelError> {
        if self.terminated {
            return Err(ModelError::Instantiate(format!(
                "{}: {name} after terminate",
                self.id
            )));
        }
        self.copy_in(io, name)?;
        let sync_error = |id: &str| {
            ModelError::Instantiate(format!("{id}: {name}: struct member width mismatch"))
        };
        let plan = Arc::clone(&self.plan);
        let (call, resolved) = match which {
            Phase::Init => (self.init, &plan.init),
            Phase::Step => (self.step, &plan.step),
        };
        self.sync_cells(&resolved.args, which)
            .map_err(|_| sync_error(&self.id))?;
        self.sync_structs_before(time, which)
            .map_err(|_| sync_error(&self.id))?;
        let regs = self.regs(&resolved.args, time, which);
        let rc = ffi::call(call.f, &regs);
        if call.returns == Returns::Int && !self.ok_codes.contains(&rc) {
            return Err(ModelError::Call {
                call: name,
                code: i64::from(rc),
                detail: format!("{}: {}", self.id, resolved.symbol),
            });
        }
        self.check_reported(name)?;
        self.sync_structs_after()
            .map_err(|_| sync_error(&self.id))?;
        self.copy_out(io, name)
    }
}

/// Which call runs; picks the `phase` value. Terminate uses the step value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Init,
    Step,
}

impl Phase {
    const fn pick(self, value: Phased) -> CValue {
        match self {
            Self::Init => value.init,
            Self::Step => value.step,
        }
    }
}

impl ModelInstance for RawInstance {
    fn init(&mut self, start_time: f64, io: &mut StepIo) -> Result<(), ModelError> {
        self.invoke(Phase::Init, "init", start_time, io)
    }

    fn step(&mut self, time: f64, io: &mut StepIo) -> Result<(), ModelError> {
        self.invoke(Phase::Step, "step", time, io)
    }

    fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        let Some(call) = self.terminate else {
            return;
        };
        let plan = Arc::clone(&self.plan);
        let Some(resolved) = plan.terminate.as_ref() else {
            return;
        };
        if self.sync_cells(&resolved.args, Phase::Step).is_err()
            || self.sync_structs_before(0.0, Phase::Step).is_err()
        {
            return;
        }
        let regs = self.regs(&resolved.args, 0.0, Phase::Step);
        let _ = ffi::call(call.f, &regs);
    }
}

// ==========================================================================
// Engine-owned memory.
// ==========================================================================

/// A C-side array of one variable. `bool` is kept as bytes, since the library may write any
/// byte value and a Rust `bool` may hold only `0` or `1`.
#[derive(Debug)]
enum CArray {
    Typed(Buffer),
    Bool(Vec<u8>),
}

/// A by-value scalar's register image.
enum Reg {
    Int(u64),
    Float(f64),
}

impl CArray {
    fn zeroed(ty: ScalarType, len: usize) -> Self {
        match ty {
            ScalarType::Bool => Self::Bool(vec![0; len]),
            other => Self::Typed(Buffer::zeroed(other, len)),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Typed(b) => b.len(),
            Self::Bool(v) => v.len(),
        }
    }

    /// The buffer's address, re-derived from a mutable borrow so the library may write through
    /// it until the next borrow on the Rust side.
    fn addr(&mut self) -> usize {
        let ptr: *mut c_void = match self {
            Self::Typed(Buffer::F64(v)) => v.as_mut_ptr().cast(),
            Self::Typed(Buffer::F32(v)) => v.as_mut_ptr().cast(),
            Self::Typed(Buffer::I64(v)) => v.as_mut_ptr().cast(),
            Self::Typed(Buffer::I32(v)) => v.as_mut_ptr().cast(),
            Self::Typed(Buffer::I16(v)) => v.as_mut_ptr().cast(),
            Self::Typed(Buffer::I8(v)) => v.as_mut_ptr().cast(),
            Self::Typed(Buffer::U64(v)) => v.as_mut_ptr().cast(),
            Self::Typed(Buffer::U32(v)) => v.as_mut_ptr().cast(),
            Self::Typed(Buffer::U16(v)) => v.as_mut_ptr().cast(),
            Self::Typed(Buffer::U8(v)) => v.as_mut_ptr().cast(),
            Self::Typed(Buffer::Bool(v)) => v.as_mut_ptr().cast(),
            Self::Bool(v) => v.as_mut_ptr().cast(),
        };
        ptr.expose_provenance()
    }

    fn load(&mut self, src: &Buffer) -> Result<(), Mismatch> {
        match (self, src) {
            (Self::Typed(dst), src) => dst.copy_from(src),
            (Self::Bool(dst), Buffer::Bool(src)) if dst.len() == src.len() => {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = u8::from(*s);
                }
                Ok(())
            }
            _ => Err(Mismatch),
        }
    }

    fn store(&self, dst: &mut Buffer) -> Result<(), Mismatch> {
        match (self, dst) {
            (Self::Typed(src), dst) => dst.copy_from(src),
            (Self::Bool(src), Buffer::Bool(dst)) if dst.len() == src.len() => {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = *s != 0;
                }
                Ok(())
            }
            _ => Err(Mismatch),
        }
    }

    /// Element 0 as a register image (narrow integers extended to 32 bits, `float` in the low
    /// bits of a `double` register).
    fn first_reg(&self) -> Reg {
        match self {
            Self::Typed(Buffer::F64(v)) => Reg::Float(v.first().copied().unwrap_or(0.0)),
            Self::Typed(Buffer::F32(v)) => Reg::Float(f64::from_bits(u64::from(
                v.first().copied().unwrap_or(0.0).to_bits(),
            ))),
            Self::Typed(Buffer::I64(v)) => Reg::Int(v.first().copied().unwrap_or(0) as u64),
            Self::Typed(Buffer::I32(v)) => {
                Reg::Int(u64::from(v.first().copied().unwrap_or(0) as u32))
            }
            Self::Typed(Buffer::I16(v)) => {
                Reg::Int(u64::from(i32::from(v.first().copied().unwrap_or(0)) as u32))
            }
            Self::Typed(Buffer::I8(v)) => {
                Reg::Int(u64::from(i32::from(v.first().copied().unwrap_or(0)) as u32))
            }
            Self::Typed(Buffer::U64(v)) => Reg::Int(v.first().copied().unwrap_or(0)),
            Self::Typed(Buffer::U32(v)) => Reg::Int(u64::from(v.first().copied().unwrap_or(0))),
            Self::Typed(Buffer::U16(v)) => Reg::Int(u64::from(v.first().copied().unwrap_or(0))),
            Self::Typed(Buffer::U8(v)) => Reg::Int(u64::from(v.first().copied().unwrap_or(0))),
            Self::Typed(Buffer::Bool(v)) => {
                Reg::Int(u64::from(v.first().copied().unwrap_or(false)))
            }
            Self::Bool(v) => Reg::Int(u64::from(v.first().copied().unwrap_or(0) != 0)),
        }
    }

    /// Native bytes of element 0 into `slot`, which must be exactly as wide.
    fn first_bytes(&self, slot: &mut [u8]) -> Result<(), Mismatch> {
        macro_rules! put {
            ($v:expr) => {{
                let bytes = $v.into_iter().next().ok_or(Mismatch)?.to_ne_bytes();
                if bytes.len() != slot.len() {
                    return Err(Mismatch);
                }
                slot.copy_from_slice(&bytes);
                Ok(())
            }};
        }
        match self {
            Self::Typed(Buffer::F64(v)) => put!(v),
            Self::Typed(Buffer::F32(v)) => put!(v),
            Self::Typed(Buffer::I64(v)) => put!(v),
            Self::Typed(Buffer::I32(v)) => put!(v),
            Self::Typed(Buffer::I16(v)) => put!(v),
            Self::Typed(Buffer::I8(v)) => put!(v),
            Self::Typed(Buffer::U64(v)) => put!(v),
            Self::Typed(Buffer::U32(v)) => put!(v),
            Self::Typed(Buffer::U16(v)) => put!(v),
            Self::Typed(Buffer::U8(v)) => put!(v),
            Self::Typed(Buffer::Bool(v)) => put!(v.iter().map(|b| u8::from(*b))),
            Self::Bool(v) => put!(v.iter().map(|b| u8::from(*b != 0))),
        }
    }

    /// Element 0 from the native bytes in `slot`.
    fn set_first_bytes(&mut self, slot: &[u8]) -> Result<(), Mismatch> {
        macro_rules! get {
            ($v:expr, $t:ty) => {{
                let bytes: [u8; size_of::<$t>()] = slot.try_into().map_err(|_| Mismatch)?;
                *$v.first_mut().ok_or(Mismatch)? = <$t>::from_ne_bytes(bytes);
                Ok(())
            }};
        }
        match self {
            Self::Typed(Buffer::F64(v)) => get!(v, f64),
            Self::Typed(Buffer::F32(v)) => get!(v, f32),
            Self::Typed(Buffer::I64(v)) => get!(v, i64),
            Self::Typed(Buffer::I32(v)) => get!(v, i32),
            Self::Typed(Buffer::I16(v)) => get!(v, i16),
            Self::Typed(Buffer::I8(v)) => get!(v, i8),
            Self::Typed(Buffer::U64(v)) => get!(v, u64),
            Self::Typed(Buffer::U32(v)) => get!(v, u32),
            Self::Typed(Buffer::U16(v)) => get!(v, u16),
            Self::Typed(Buffer::U8(v)) => get!(v, u8),
            Self::Typed(Buffer::Bool(v)) => {
                *v.first_mut().ok_or(Mismatch)? = *slot.first().ok_or(Mismatch)? != 0;
                Ok(())
            }
            Self::Bool(v) => {
                *v.first_mut().ok_or(Mismatch)? = *slot.first().ok_or(Mismatch)?;
                Ok(())
            }
        }
    }
}

/// Width in bytes of a C scalar on this target.
const fn scalar_width(ty: ScalarType) -> usize {
    crate::layout::scalar_size_align(ty).0
}

/// A bound length as a [`CValue`] of integer type `ty` (fit checked at instantiate).
const fn dim_value(ty: ScalarType, len: usize) -> CValue {
    CValue {
        ty,
        bits: len as u64,
    }
}

/// The C integer of type `ty` in `bytes`, widened.
fn read_int(ty: ScalarType, bytes: &[u8]) -> Option<i128> {
    macro_rules! get {
        ($t:ty) => {
            Some(i128::from(<$t>::from_ne_bytes(bytes.try_into().ok()?)))
        };
    }
    match ty {
        ScalarType::I64 => get!(i64),
        ScalarType::U64 => get!(u64),
        ScalarType::I32 => get!(i32),
        ScalarType::U32 => get!(u32),
        ScalarType::I16 => get!(i16),
        ScalarType::U16 => get!(u16),
        ScalarType::I8 => get!(i8),
        ScalarType::U8 => get!(u8),
        ScalarType::F64 | ScalarType::F32 | ScalarType::Bool => None,
    }
}

/// Write `value` as the C integer type `ty` into `slot`.
fn int_bytes(ty: ScalarType, value: u64, slot: &mut [u8]) -> Result<(), Mismatch> {
    macro_rules! put {
        ($v:expr) => {{
            let bytes = $v.to_ne_bytes();
            if bytes.len() != slot.len() {
                return Err(Mismatch);
            }
            slot.copy_from_slice(&bytes);
            Ok(())
        }};
    }
    match ty {
        ScalarType::I64 => put!(value as i64),
        ScalarType::U64 => put!(value),
        ScalarType::I32 => put!(value as i32),
        ScalarType::U32 => put!(value as u32),
        ScalarType::I16 => put!(value as i16),
        ScalarType::U16 => put!(value as u16),
        ScalarType::I8 => put!(value as i8),
        ScalarType::U8 => put!(value as u8),
        ScalarType::F64 | ScalarType::F32 | ScalarType::Bool => Err(Mismatch),
    }
}

/// A struct image: `size` zeroed bytes at an address aligned to `align`, inside an
/// over-allocated byte vector that never grows.
#[derive(Debug)]
struct Image {
    bytes: Vec<u8>,
    start: usize,
    size: usize,
}

impl Image {
    fn zeroed(size: usize, align: usize) -> Self {
        let align = align.max(1);
        let mut bytes = vec![0_u8; size + align];
        let base = bytes.as_mut_ptr().addr();
        let start = base.next_multiple_of(align) - base;
        Self { bytes, start, size }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes[self.start..self.start + self.size]
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[self.start..self.start + self.size]
    }

    fn addr(&mut self) -> usize {
        self.bytes
            .as_mut_ptr()
            .wrapping_add(self.start)
            .cast::<c_void>()
            .expose_provenance()
    }
}
