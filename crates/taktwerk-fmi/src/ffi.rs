//! The FMI 2 and FMI 3 C API, loaded from an FMU's shared library.
//!
//! Function types follow the standard headers (`fmi2FunctionTypes.h`, `fmi3FunctionTypes.h`).
//! Everything outside this module is safe code.

// The adapter's only FFI boundary: dlopen, symbol lookup and calls into the FMU.
#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use taktwerk_core::model::ModelError;
use taktwerk_core::value::Buffer;

use crate::description::FmiVersion;

type Status = c_int;

type Fmi3Log = unsafe extern "C" fn(*mut c_void, Status, *const c_char, *const c_char);
type Fmi3InstantiateCoSimulation = unsafe extern "C" fn(
    *const c_char,   // instanceName
    *const c_char,   // instantiationToken
    *const c_char,   // resourcePath
    bool,            // visible
    bool,            // loggingOn
    bool,            // eventModeUsed
    bool,            // earlyReturnAllowed
    *const u32,      // requiredIntermediateVariables
    usize,           // nRequiredIntermediateVariables
    *mut c_void,     // instanceEnvironment
    Option<Fmi3Log>, // logMessage
    *const c_void,   // intermediateUpdate
) -> *mut c_void;
type Fmi3Call = unsafe extern "C" fn(*mut c_void) -> Status;
type Fmi3Free = unsafe extern "C" fn(*mut c_void);
type Fmi3EnterInit = unsafe extern "C" fn(*mut c_void, bool, f64, f64, bool, f64) -> Status;
type Fmi3DoStep = unsafe extern "C" fn(
    *mut c_void,
    f64,       // currentCommunicationPoint
    f64,       // communicationStepSize
    bool,      // noSetFMUStatePriorToCurrentPoint
    *mut bool, // eventHandlingNeeded
    *mut bool, // terminateSimulation
    *mut bool, // earlyReturn
    *mut f64,  // lastSuccessfulTime
) -> Status;
type Fmi3Set<T> = unsafe extern "C" fn(*mut c_void, *const u32, usize, *const T, usize) -> Status;
type Fmi3Get<T> = unsafe extern "C" fn(*mut c_void, *const u32, usize, *mut T, usize) -> Status;

/// `fmi2CallbackLogger` is variadic; stable Rust cannot define a variadic function. The fixed
/// arguments arrive in the same registers on the supported Linux ABIs (x86_64 System V,
/// AArch64), so the callback reads those and logs the format string unexpanded.
type Fmi2Log =
    unsafe extern "C" fn(*mut c_void, *const c_char, Status, *const c_char, *const c_char);
type Fmi2Instantiate = unsafe extern "C" fn(
    *const c_char, // instanceName
    c_int,         // fmuType
    *const c_char, // fmuGUID
    *const c_char, // fmuResourceLocation
    *const Fmi2CallbackFunctions,
    c_int, // visible
    c_int, // loggingOn
) -> *mut c_void;
type Fmi2SetupExperiment = unsafe extern "C" fn(*mut c_void, c_int, f64, f64, c_int, f64) -> Status;
type Fmi2DoStep = unsafe extern "C" fn(*mut c_void, f64, f64, c_int) -> Status;
type Fmi2Set<T> = unsafe extern "C" fn(*mut c_void, *const c_uint, usize, *const T) -> Status;
type Fmi2Get<T> = unsafe extern "C" fn(*mut c_void, *const c_uint, usize, *mut T) -> Status;

/// `fmi2CallbackFunctions`.
#[repr(C)]
struct Fmi2CallbackFunctions {
    logger: Option<Fmi2Log>,
    allocate_memory: Option<unsafe extern "C" fn(usize, usize) -> *mut c_void>,
    free_memory: Option<unsafe extern "C" fn(*mut c_void)>,
    step_finished: *const c_void,
    component_environment: *mut c_void,
}

unsafe extern "C" {
    fn calloc(nobj: usize, size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

/// `fmi2CoSimulation`.
const FMI2_CO_SIMULATION: c_int = 1;

macro_rules! api {
    ($name:ident { $($field:ident: $ty:ty = $sym:literal,)* } optional { $($ofield:ident: $oty:ty = $osym:literal,)* }) => {
        struct $name {
            $($field: $ty,)*
            $($ofield: Option<$oty>,)*
        }

        impl $name {
            /// Look up every symbol; the copied function pointers are valid while `lib` is loaded.
            fn load(lib: &libloading::Library) -> Result<Self, ModelError> {
                Ok(Self {
                    // SAFETY: the symbol types are the standard header's signatures.
                    $($field: *unsafe { lib.get::<$ty>(concat!($sym, "\0").as_bytes()) }
                        .map_err(|e| ModelError::Load(format!("{}: {e}", $sym)))?,)*
                    // SAFETY: as above.
                    $($ofield: unsafe { lib.get::<$oty>(concat!($osym, "\0").as_bytes()) }
                        .ok().map(|s| *s),)*
                })
            }
        }
    };
}

api!(Fmi3Api {
    instantiate: Fmi3InstantiateCoSimulation = "fmi3InstantiateCoSimulation",
    free_instance: Fmi3Free = "fmi3FreeInstance",
    enter_init: Fmi3EnterInit = "fmi3EnterInitializationMode",
    exit_init: Fmi3Call = "fmi3ExitInitializationMode",
    terminate: Fmi3Call = "fmi3Terminate",
    do_step: Fmi3DoStep = "fmi3DoStep",
} optional {
    enter_config: Fmi3Call = "fmi3EnterConfigurationMode",
    exit_config: Fmi3Call = "fmi3ExitConfigurationMode",
    set_f64: Fmi3Set<f64> = "fmi3SetFloat64",
    get_f64: Fmi3Get<f64> = "fmi3GetFloat64",
    set_f32: Fmi3Set<f32> = "fmi3SetFloat32",
    get_f32: Fmi3Get<f32> = "fmi3GetFloat32",
    set_i64: Fmi3Set<i64> = "fmi3SetInt64",
    get_i64: Fmi3Get<i64> = "fmi3GetInt64",
    set_i32: Fmi3Set<i32> = "fmi3SetInt32",
    get_i32: Fmi3Get<i32> = "fmi3GetInt32",
    set_i16: Fmi3Set<i16> = "fmi3SetInt16",
    get_i16: Fmi3Get<i16> = "fmi3GetInt16",
    set_i8: Fmi3Set<i8> = "fmi3SetInt8",
    get_i8: Fmi3Get<i8> = "fmi3GetInt8",
    set_u64: Fmi3Set<u64> = "fmi3SetUInt64",
    get_u64: Fmi3Get<u64> = "fmi3GetUInt64",
    set_u32: Fmi3Set<u32> = "fmi3SetUInt32",
    get_u32: Fmi3Get<u32> = "fmi3GetUInt32",
    set_u16: Fmi3Set<u16> = "fmi3SetUInt16",
    get_u16: Fmi3Get<u16> = "fmi3GetUInt16",
    set_u8: Fmi3Set<u8> = "fmi3SetUInt8",
    get_u8: Fmi3Get<u8> = "fmi3GetUInt8",
    set_bool: Fmi3Set<bool> = "fmi3SetBoolean",
    get_bool: Fmi3Get<bool> = "fmi3GetBoolean",
    set_string: Fmi3Set<*const c_char> = "fmi3SetString",
    get_string: Fmi3Get<*const c_char> = "fmi3GetString",
});

api!(Fmi2Api {
    instantiate: Fmi2Instantiate = "fmi2Instantiate",
    free_instance: Fmi3Free = "fmi2FreeInstance",
    setup_experiment: Fmi2SetupExperiment = "fmi2SetupExperiment",
    enter_init: Fmi3Call = "fmi2EnterInitializationMode",
    exit_init: Fmi3Call = "fmi2ExitInitializationMode",
    terminate: Fmi3Call = "fmi2Terminate",
    do_step: Fmi2DoStep = "fmi2DoStep",
    set_real: Fmi2Set<f64> = "fmi2SetReal",
    get_real: Fmi2Get<f64> = "fmi2GetReal",
    set_integer: Fmi2Set<c_int> = "fmi2SetInteger",
    get_integer: Fmi2Get<c_int> = "fmi2GetInteger",
    set_boolean: Fmi2Set<c_int> = "fmi2SetBoolean",
    get_boolean: Fmi2Get<c_int> = "fmi2GetBoolean",
} optional {});

enum Api {
    V2(Fmi2Api),
    V3(Fmi3Api),
}

/// A loaded FMU shared library with its resolved FMI functions.
pub(crate) struct Library {
    api: Api,
    // Dropped last: the function pointers in `api` point into it.
    _lib: libloading::Library,
}

impl Library {
    /// `dlopen` the binary at `path` (local symbols) and resolve the FMI functions.
    pub fn load(path: &Path, version: FmiVersion) -> Result<Self, ModelError> {
        // SAFETY: loading runs the library's initialisers; an FMU binary is trusted code by
        // design (models run in-process).
        let lib = unsafe { libloading::Library::new(path) }
            .map_err(|e| ModelError::Load(format!("{}: {e}", path.display())))?;
        let api = match version {
            FmiVersion::V2 => Api::V2(Fmi2Api::load(&lib)?),
            FmiVersion::V3 => Api::V3(Fmi3Api::load(&lib)?),
        };
        Ok(Self { api, _lib: lib })
    }
}

/// Context handed to the FMU's logger callback.
struct LogEnv {
    instance: String,
}

/// # Safety
/// `env` is null or the `LogEnv` passed at instantiation; strings are null or NUL-terminated.
unsafe fn log(env: *mut c_void, status: Status, category: *const c_char, message: *const c_char) {
    let text = |p: *const c_char| {
        if p.is_null() {
            String::new()
        } else {
            // SAFETY: the FMU passes NUL-terminated strings valid for the call.
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };
    let instance = if env.is_null() {
        ""
    } else {
        // SAFETY: `env` is the `LogEnv` this module passed at instantiation; it outlives the
        // FMU instance.
        unsafe { &*env.cast::<LogEnv>() }.instance.as_str()
    };
    let (category, message) = (text(category), text(message));
    match status {
        0 => tracing::debug!(instance, category, "{message}"),
        1 => tracing::warn!(instance, category, "{message}"),
        _ => tracing::error!(instance, category, status, "{message}"),
    }
}

unsafe extern "C" fn log3(
    env: *mut c_void,
    status: Status,
    category: *const c_char,
    message: *const c_char,
) {
    // SAFETY: the FMU passes back the environment given at instantiation and C strings.
    unsafe { log(env, status, category, message) };
}

unsafe extern "C" fn log2(
    env: *mut c_void,
    _instance_name: *const c_char,
    status: Status,
    category: *const c_char,
    message: *const c_char,
) {
    // SAFETY: the FMU passes back the environment given at instantiation and C strings.
    unsafe { log(env, status, category, message) };
}

fn status_name(code: Status) -> &'static str {
    match code {
        0 => "OK",
        1 => "Warning",
        2 => "Discard",
        3 => "Error",
        4 => "Fatal",
        5 => "Pending",
        _ => "unknown status",
    }
}

/// `OK` and `Warning` pass; everything else is an error of `call`.
fn check(call: &'static str, code: Status) -> Result<(), ModelError> {
    if code == 0 || code == 1 {
        Ok(())
    } else {
        Err(ModelError::Call {
            call,
            code: i64::from(code),
            detail: status_name(code).to_owned(),
        })
    }
}

fn missing(call: &'static str) -> ModelError {
    ModelError::Call {
        call,
        code: -1,
        detail: "not exported by the FMU".to_owned(),
    }
}

fn unsupported(version: &str, buf: &Buffer) -> ModelError {
    ModelError::Instantiate(format!("{version} has no {:?} variables", buf.ty()))
}

fn c_string(s: &str) -> Result<CString, ModelError> {
    CString::new(s).map_err(|_| ModelError::Instantiate(format!("{s:?} contains NUL")))
}

/// One FMU instance. Freed on drop.
pub(crate) struct Instance {
    handle: NonNull<c_void>,
    lib: Arc<Library>,
    // Referenced by the FMU for its lifetime; boxed so the addresses stay fixed.
    _env: Box<LogEnv>,
    _callbacks: Option<Box<Fmi2CallbackFunctions>>,
}

// SAFETY: an FMU instance may be used from any thread as long as calls are not concurrent;
// `Instance` is not `Sync` and every call takes `&mut self`.
unsafe impl Send for Instance {}

impl Instance {
    /// Instantiate a co-simulation instance. `resources` is the FMU's `resources` directory.
    pub fn new(
        lib: Arc<Library>,
        name: &str,
        token: &str,
        resources: &Path,
        logging: bool,
    ) -> Result<Self, ModelError> {
        let c_name = c_string(name)?;
        let c_token = c_string(token)?;
        let env = Box::new(LogEnv {
            instance: name.to_owned(),
        });
        let env_ptr = ptr::from_ref::<LogEnv>(&env).cast_mut().cast::<c_void>();
        let mut callbacks = None;
        let handle = match &lib.api {
            Api::V3(api) => {
                let mut path = resources.to_string_lossy().into_owned();
                if !path.ends_with('/') {
                    path.push('/');
                }
                let c_path = c_string(&path)?;
                // SAFETY: all strings are NUL-terminated and live for the call; `env_ptr`
                // stays valid until the instance is freed (owned by `Self`).
                unsafe {
                    (api.instantiate)(
                        c_name.as_ptr(),
                        c_token.as_ptr(),
                        c_path.as_ptr(),
                        false,
                        logging,
                        false,
                        false,
                        ptr::null(),
                        0,
                        env_ptr,
                        Some(log3),
                        ptr::null(),
                    )
                }
            }
            Api::V2(api) => {
                let c_uri = c_string(&file_uri(resources))?;
                let cb = Box::new(Fmi2CallbackFunctions {
                    logger: Some(log2),
                    allocate_memory: Some(calloc),
                    free_memory: Some(free),
                    step_finished: ptr::null(),
                    component_environment: env_ptr,
                });
                // SAFETY: as above; `cb` is boxed and owned by `Self`, so the pointer the FMU
                // keeps stays valid until the instance is freed.
                let h = unsafe {
                    (api.instantiate)(
                        c_name.as_ptr(),
                        FMI2_CO_SIMULATION,
                        c_token.as_ptr(),
                        c_uri.as_ptr(),
                        ptr::from_ref::<Fmi2CallbackFunctions>(&cb),
                        0,
                        c_int::from(logging),
                    )
                };
                callbacks = Some(cb);
                h
            }
        };
        let handle = NonNull::new(handle).ok_or_else(|| ModelError::Call {
            call: "instantiate",
            code: 0,
            detail: format!("{name}: the FMU returned no instance"),
        })?;
        Ok(Self {
            handle,
            lib,
            _env: env,
            _callbacks: callbacks,
        })
    }

    fn h(&self) -> *mut c_void {
        self.handle.as_ptr()
    }

    /// FMI 3 only: enter configuration mode.
    pub fn enter_configuration_mode(&mut self) -> Result<(), ModelError> {
        let Api::V3(api) = &self.lib.api else {
            return Err(missing("fmi3EnterConfigurationMode"));
        };
        let f = api
            .enter_config
            .ok_or_else(|| missing("fmi3EnterConfigurationMode"))?;
        // SAFETY: valid instance handle.
        check("fmi3EnterConfigurationMode", unsafe { f(self.h()) })
    }

    /// FMI 3 only: leave configuration mode.
    pub fn exit_configuration_mode(&mut self) -> Result<(), ModelError> {
        let Api::V3(api) = &self.lib.api else {
            return Err(missing("fmi3ExitConfigurationMode"));
        };
        let f = api
            .exit_config
            .ok_or_else(|| missing("fmi3ExitConfigurationMode"))?;
        // SAFETY: valid instance handle.
        check("fmi3ExitConfigurationMode", unsafe { f(self.h()) })
    }

    /// Enter initialization mode at `start_time`, no tolerance, no stop time.
    pub fn enter_initialization_mode(&mut self, start_time: f64) -> Result<(), ModelError> {
        let h = self.h();
        match &self.lib.api {
            // SAFETY: valid instance handle.
            Api::V3(api) => check("fmi3EnterInitializationMode", unsafe {
                (api.enter_init)(h, false, 0.0, start_time, false, 0.0)
            }),
            Api::V2(api) => {
                // SAFETY: valid instance handle.
                check("fmi2SetupExperiment", unsafe {
                    (api.setup_experiment)(h, 0, 0.0, start_time, 0, 0.0)
                })?;
                // SAFETY: valid instance handle.
                check("fmi2EnterInitializationMode", unsafe {
                    (api.enter_init)(h)
                })
            }
        }
    }

    /// Leave initialization mode.
    pub fn exit_initialization_mode(&mut self) -> Result<(), ModelError> {
        let h = self.h();
        match &self.lib.api {
            // SAFETY: valid instance handle.
            Api::V3(api) => check("fmi3ExitInitializationMode", unsafe { (api.exit_init)(h) }),
            // SAFETY: valid instance handle.
            Api::V2(api) => check("fmi2ExitInitializationMode", unsafe { (api.exit_init)(h) }),
        }
    }

    /// Advance from `time` by `step`. No allocation.
    pub fn do_step(&mut self, time: f64, step: f64) -> Result<(), ModelError> {
        let h = self.h();
        match &self.lib.api {
            Api::V3(api) => {
                let (mut event, mut terminate, mut early) = (false, false, false);
                let mut last = time;
                // SAFETY: valid instance handle; the out-pointers point to live locals.
                let code = unsafe {
                    (api.do_step)(
                        h,
                        time,
                        step,
                        true,
                        &raw mut event,
                        &raw mut terminate,
                        &raw mut early,
                        &raw mut last,
                    )
                };
                check("fmi3DoStep", code)?;
                if terminate {
                    return Err(ModelError::Call {
                        call: "fmi3DoStep",
                        code: i64::from(code),
                        detail: "the model requested termination".to_owned(),
                    });
                }
                Ok(())
            }
            // SAFETY: valid instance handle.
            Api::V2(api) => check("fmi2DoStep", unsafe { (api.do_step)(h, time, step, 1) }),
        }
    }

    /// Terminate the simulation.
    pub fn terminate(&mut self) -> Result<(), ModelError> {
        let h = self.h();
        match &self.lib.api {
            // SAFETY: valid instance handle.
            Api::V3(api) => check("fmi3Terminate", unsafe { (api.terminate)(h) }),
            // SAFETY: valid instance handle.
            Api::V2(api) => check("fmi2Terminate", unsafe { (api.terminate)(h) }),
        }
    }

    /// Write `buf` to variable `vr`; the FMU expects exactly `buf.len()` values. No allocation.
    pub fn set(&mut self, vr: u32, buf: &Buffer) -> Result<(), ModelError> {
        let h = self.h();
        let vrp = &raw const vr;
        match &self.lib.api {
            Api::V3(api) => {
                macro_rules! set3 {
                    ($f:ident, $call:literal, $v:expr) => {{
                        let f = api.$f.ok_or_else(|| missing($call))?;
                        // SAFETY: one value reference, `$v.len()` values in a live slice.
                        check($call, unsafe { f(h, vrp, 1, $v.as_ptr(), $v.len()) })
                    }};
                }
                match buf {
                    Buffer::F64(v) => set3!(set_f64, "fmi3SetFloat64", v),
                    Buffer::F32(v) => set3!(set_f32, "fmi3SetFloat32", v),
                    Buffer::I64(v) => set3!(set_i64, "fmi3SetInt64", v),
                    Buffer::I32(v) => set3!(set_i32, "fmi3SetInt32", v),
                    Buffer::I16(v) => set3!(set_i16, "fmi3SetInt16", v),
                    Buffer::I8(v) => set3!(set_i8, "fmi3SetInt8", v),
                    Buffer::U64(v) => set3!(set_u64, "fmi3SetUInt64", v),
                    Buffer::U32(v) => set3!(set_u32, "fmi3SetUInt32", v),
                    Buffer::U16(v) => set3!(set_u16, "fmi3SetUInt16", v),
                    Buffer::U8(v) => set3!(set_u8, "fmi3SetUInt8", v),
                    Buffer::Bool(v) => set3!(set_bool, "fmi3SetBoolean", v),
                }
            }
            Api::V2(api) => {
                let n = buf.len();
                match buf {
                    // SAFETY: `n` value references and values; FMI 2 is scalar, so the
                    // interface sizes every buffer to one element.
                    Buffer::F64(v) if n == 1 => check("fmi2SetReal", unsafe {
                        (api.set_real)(h, vrp, 1, v.as_ptr())
                    }),
                    // SAFETY: as above.
                    Buffer::I32(v) if n == 1 => check("fmi2SetInteger", unsafe {
                        (api.set_integer)(h, vrp, 1, v.as_ptr())
                    }),
                    Buffer::Bool(v) if n == 1 => {
                        let b = c_int::from(v[0]);
                        // SAFETY: one value reference, one live local value.
                        check("fmi2SetBoolean", unsafe {
                            (api.set_boolean)(h, vrp, 1, &raw const b)
                        })
                    }
                    _ => Err(unsupported("FMI 2", buf)),
                }
            }
        }
    }

    /// Read variable `vr` into `buf`; the FMU writes exactly `buf.len()` values. No allocation.
    pub fn get(&mut self, vr: u32, buf: &mut Buffer) -> Result<(), ModelError> {
        let h = self.h();
        let vrp = &raw const vr;
        match &self.lib.api {
            Api::V3(api) => {
                macro_rules! get3 {
                    ($f:ident, $call:literal, $v:expr) => {{
                        let f = api.$f.ok_or_else(|| missing($call))?;
                        // SAFETY: one value reference, room for `$v.len()` values.
                        check($call, unsafe { f(h, vrp, 1, $v.as_mut_ptr(), $v.len()) })
                    }};
                }
                match buf {
                    Buffer::F64(v) => get3!(get_f64, "fmi3GetFloat64", v),
                    Buffer::F32(v) => get3!(get_f32, "fmi3GetFloat32", v),
                    Buffer::I64(v) => get3!(get_i64, "fmi3GetInt64", v),
                    Buffer::I32(v) => get3!(get_i32, "fmi3GetInt32", v),
                    Buffer::I16(v) => get3!(get_i16, "fmi3GetInt16", v),
                    Buffer::I8(v) => get3!(get_i8, "fmi3GetInt8", v),
                    Buffer::U64(v) => get3!(get_u64, "fmi3GetUInt64", v),
                    Buffer::U32(v) => get3!(get_u32, "fmi3GetUInt32", v),
                    Buffer::U16(v) => get3!(get_u16, "fmi3GetUInt16", v),
                    Buffer::U8(v) => get3!(get_u8, "fmi3GetUInt8", v),
                    // fmi3Boolean is C `bool`: one byte, 0 or 1, as Rust `bool`.
                    Buffer::Bool(v) => get3!(get_bool, "fmi3GetBoolean", v),
                }
            }
            Api::V2(api) => {
                let n = buf.len();
                match buf {
                    // SAFETY: one value reference, room for one value.
                    Buffer::F64(v) if n == 1 => check("fmi2GetReal", unsafe {
                        (api.get_real)(h, vrp, 1, v.as_mut_ptr())
                    }),
                    // SAFETY: as above.
                    Buffer::I32(v) if n == 1 => check("fmi2GetInteger", unsafe {
                        (api.get_integer)(h, vrp, 1, v.as_mut_ptr())
                    }),
                    Buffer::Bool(v) if n == 1 => {
                        let mut b: c_int = 0;
                        // SAFETY: one value reference, one live local value.
                        check("fmi2GetBoolean", unsafe {
                            (api.get_boolean)(h, vrp, 1, &raw mut b)
                        })?;
                        v[0] = b != 0;
                        Ok(())
                    }
                    _ => Err(unsupported("FMI 2", buf)),
                }
            }
        }
    }
}

impl Instance {
    /// FMI 3 only: write the text in `bytes` (up to its first NUL) to String variable `vr`.
    /// `scratch` must hold `bytes.len() + 1`; it is not grown, so nothing allocates.
    pub fn set_string(
        &mut self,
        vr: u32,
        bytes: &[u8],
        scratch: &mut Vec<u8>,
    ) -> Result<(), ModelError> {
        let Api::V3(api) = &self.lib.api else {
            return Err(missing("fmi3SetString"));
        };
        let f = api.set_string.ok_or_else(|| missing("fmi3SetString"))?;
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        if scratch.capacity() < end + 1 {
            return Err(ModelError::Instantiate(format!(
                "value reference {vr}: text scratch too small"
            )));
        }
        scratch.clear();
        scratch.extend_from_slice(&bytes[..end]);
        scratch.push(0);
        let text: *const c_char = scratch.as_ptr().cast();
        let vrp = &raw const vr;
        // SAFETY: one value reference, one NUL-terminated string that lives for the call.
        check("fmi3SetString", unsafe {
            f(self.h(), vrp, 1, &raw const text, 1)
        })
    }

    /// FMI 3 only: read String variable `vr` into `out`, truncated to its length and zero
    /// padded. No allocation.
    pub fn get_string(&mut self, vr: u32, out: &mut [u8]) -> Result<(), ModelError> {
        let Api::V3(api) = &self.lib.api else {
            return Err(missing("fmi3GetString"));
        };
        let f = api.get_string.ok_or_else(|| missing("fmi3GetString"))?;
        let mut text: *const c_char = ptr::null();
        let vrp = &raw const vr;
        // SAFETY: one value reference, room for one pointer; the FMU keeps the string valid
        // until the next call.
        check("fmi3GetString", unsafe {
            f(self.h(), vrp, 1, &raw mut text, 1)
        })?;
        out.fill(0);
        if text.is_null() {
            return Ok(());
        }
        // SAFETY: the FMU returned a NUL-terminated string valid until the next call.
        let bytes = unsafe { CStr::from_ptr(text) }.to_bytes();
        let n = bytes.len().min(out.len());
        out[..n].copy_from_slice(&bytes[..n]);
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let h = self.h();
        match &self.lib.api {
            // SAFETY: valid handle, freed exactly once; the library is still loaded (`lib`).
            Api::V3(api) => unsafe { (api.free_instance)(h) },
            // SAFETY: as above.
            Api::V2(api) => unsafe { (api.free_instance)(h) },
        }
    }
}

/// `file://` URI of an absolute path, percent-encoding everything but unreserved characters
/// and `/`.
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(char::from(b));
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_encodes() {
        assert_eq!(
            file_uri(Path::new("/tmp/a b/resources")),
            "file:///tmp/a%20b/resources"
        );
    }
}
