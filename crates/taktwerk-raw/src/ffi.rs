//! The crate's only `unsafe`: opening a library, looking a symbol up and calling it.
//!
//! The call shim passes arguments by register class rather than by exact prototype. On the two
//! supported targets (aarch64 AAPCS64 and x86_64 System V) pointer and integer arguments fill
//! the integer registers in order and floating-point arguments fill the FP registers in order,
//! independently of each other. A C function taking any mix of up to eight of each therefore
//! reads exactly the registers a call to [`RawFn`] fills; unused registers and the two integer
//! stack slots x86_64 needs for arguments seven and eight are ignored by a callee with fewer
//! parameters. A `float` is placed in the low 32 bits of its register, a narrow integer is
//! extended to 32 bits as C requires.
//!
//! Soundness rests on the descriptor: the symbol must have the parameter kinds the descriptor
//! states and must only touch the memory the engine hands it at the stated sizes. That is the
//! model developer's confirmation (`abi.confirmed`), the same trust any C caller extends.
#![allow(
    unsafe_code,
    reason = "dynamic loading and calling foreign functions has no safe spelling; every block carries a SAFETY comment"
)]

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("taktwerk-raw supports aarch64 and x86_64 only (register-class call shim)");

use std::path::Path;

use libloading::Library;

/// Register-class call type: eight integer-class and eight float-class arguments.
pub type RawFn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> i32;

/// The register images of one call.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Regs {
    /// Integer-class arguments (pointers, integers, bools), in C order among themselves.
    pub ints: [u64; 8],
    /// Float-class arguments (`double`, `float`), in C order among themselves.
    pub floats: [f64; 8],
}

/// An open shared library.
#[derive(Debug)]
pub struct Lib(Library);

impl Lib {
    /// `dlopen` the file at `path`.
    ///
    /// # Errors
    /// The loader's message when the file cannot be opened or its dependencies resolved.
    pub fn open(path: &Path) -> Result<Self, String> {
        // SAFETY: loading runs the library's constructors. A model library is code the model
        // developer ships for exactly this purpose; nothing in this crate can make it safe
        // beyond opening only the file the package names.
        unsafe { Library::new(path) }
            .map(Self)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Look a function up by name.
    ///
    /// # Errors
    /// The symbol is not exported.
    pub fn function(&self, name: &str) -> Result<RawFn, String> {
        // SAFETY: the symbol is used only through `call`, whose register-class convention
        // matches any C function with up to eight integer-class and eight float-class
        // parameters (module docs). The pointer stays valid while `self` is alive, and every
        // holder of a `RawFn` also holds the `Lib` it came from.
        let symbol = unsafe { self.0.get::<RawFn>(name.as_bytes()) }
            .map_err(|e| format!("symbol {name}: {e}"))?;
        Ok(*symbol)
    }
}

/// Call `f` with the given register images and return its `int` result (garbage for `void`).
#[must_use]
pub fn call(f: RawFn, regs: &Regs) -> i32 {
    let i = &regs.ints;
    let d = &regs.floats;
    // SAFETY: `f` came from `Lib::function` on a library that outlives this call; every
    // pointer in `ints` addresses an engine-owned buffer sized from the bound dimensions the
    // descriptor declares for it, alive for the instance's lifetime; the callee's parameter
    // kinds match the descriptor by the developer's confirmation (module docs).
    unsafe {
        f(
            i[0], i[1], i[2], i[3], i[4], i[5], i[6], i[7], d[0], d[1], d[2], d[3], d[4], d[5],
            d[6], d[7],
        )
    }
}
