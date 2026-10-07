//! Raw C library model adapter for taktwerk: header import, descriptors, C layout.
//!
//! A raw model is a shared library with an init, a step and optionally a terminate function,
//! described by a descriptor the engine reads. The engine owns every buffer, lays structs out
//! by C's rules for the running target, fills pointers once per call and copies inputs in and
//! outputs out around each step.
//!
//! # Package format
//!
//! ```text
//! <package>/
//!   taktwerk-model.toml      the descriptor
//!   lib/aarch64/<name>.so    one library per architecture; the running one is picked
//!   lib/x86_64/<name>.so
//! ```
//!
//! The descriptor is the size-generic interface ([`taktwerk_core::model::ModelInterface`]:
//! `name`, `dimensions`, `variables`, `instances`) plus an `[abi]` section describing the C
//! calls. A library is called through at most three functions:
//!
//! - `[abi.init]` once per instance, after the engine has allocated and filled the buffers
//!   (parameters, first inputs, dimension lengths);
//! - `[abi.step]` once per step, after inputs and changed tunables were copied in; outputs are
//!   copied out afterwards;
//! - `[abi.terminate]` (optional) once when the instance is released.
//!
//! Each function returns `int` (`returns = "int"`, default) where any value in `ok_codes`
//! (default `[0]`) is success and anything else fails the call, or `void` (`returns = "void"`).
//! Arguments are listed in C order; each is exactly one of
//!
//! | argument                      | C parameter | filled with                                     |
//! |-------------------------------|-------------|-------------------------------------------------|
//! | `{ struct = "s" }`            | `s *`       | the address of the engine's image of `s`        |
//! | `{ array = "v" }`             | `T *`       | the address of variable `v`'s buffer            |
//! | `{ value = "v" }`             | `T`         | scalar variable `v` (input, parameter, tunable) |
//! | `{ dim = "n", type = "int" }` | integer     | the bound length of dimension `n`               |
//! | `{ builtin = "step_size" }`   | `double`    | the instance's step, seconds                    |
//! | `{ builtin = "time" }`        | `double`    | the time the call advances from                 |
//! | `{ handle = "out" }`          | `void **`   | init only: the library stores its handle        |
//! | `{ handle = "in" }`           | `void *`    | the stored handle                               |
//!
//! A struct in `[abi.structs.<s>]` lists its members with their C type in declaration order.
//! A pointer member mapped to a `variable` gets the buffer's address, a scalar member mapped
//! to a `variable` carries the value (inputs, parameters and tunables are written before each
//! call, outputs read after it), a scalar integer member mapped to a `dim` carries the bound
//! length, a `double` member mapped to a `builtin` the step size or time. Unmapped pointer
//! members are `NULL`, unmapped scalars zero. Admitted C types are `double`, `float`, `bool`,
//! `char` and the fixed-width and plain integer types at their 64-bit Linux widths, each
//! optionally `const` and optionally followed by one `*`.
//!
//! Lengths are never inferred: `abi.confirmed = true` states that the model developer checked
//! which pointer takes which dimension. A descriptor without it is refused at load. A `single`
//! library keeps its state in globals, so each further instance loads a private copy of the
//! file; a `multiple` library carries its state behind a handle or inside a struct the engine
//! owns per instance.
//!
//! # Example
//!
//! A discrete state-space model `x' = A x + b u`, `y = k cᵀx`, size-generic in `nx`, with
//! this C interface:
//!
//! ```c
//! typedef struct { int nx; const double *A; const double *b; const double *c; double k; } ss_params;
//! typedef struct { double u; double *x; double y; } ss_io;
//! int  ss_init(const ss_params *p, double dt);
//! int  ss_step(const ss_params *p, ss_io *io, double t);
//! void ss_terminate(void);
//! ```
//!
//! ```toml
//! name = "state-space"
//! instances = "single"
//!
//! [[dimensions]]
//! name = "nx"
//! min = 1
//! max = 64
//!
//! [[variables]]
//! name = "A"
//! causality = "parameter"
//! type = "f64"
//! shape = ["nx", "nx"]
//! layout = "row-major"
//!
//! [[variables]]
//! name = "b"
//! causality = "parameter"
//! type = "f64"
//! shape = ["nx"]
//!
//! [[variables]]
//! name = "c"
//! causality = "parameter"
//! type = "f64"
//! shape = ["nx"]
//!
//! [[variables]]
//! name = "k"
//! causality = "tunable"
//! type = "f64"
//!
//! [[variables]]
//! name = "u"
//! causality = "input"
//! type = "f64"
//!
//! [[variables]]
//! name = "x"
//! causality = "output"
//! type = "f64"
//! shape = ["nx"]
//!
//! [[variables]]
//! name = "y"
//! causality = "output"
//! type = "f64"
//!
//! [abi]
//! confirmed = true
//! ok_codes = [0]
//!
//! [abi.init]
//! symbol = "ss_init"
//! args = [{ struct = "ss_params" }, { builtin = "step_size" }]
//!
//! [abi.step]
//! symbol = "ss_step"
//! args = [{ struct = "ss_params" }, { struct = "ss_io" }, { builtin = "time" }]
//!
//! [abi.terminate]
//! symbol = "ss_terminate"
//! returns = "void"
//!
//! [abi.structs.ss_params]
//! members = [
//!   { name = "nx", type = "int", dim = "nx" },
//!   { name = "A", type = "const double *", variable = "A" },
//!   { name = "b", type = "const double *", variable = "b" },
//!   { name = "c", type = "const double *", variable = "c" },
//!   { name = "k", type = "double", variable = "k" },
//! ]
//!
//! [abi.structs.ss_io]
//! members = [
//!   { name = "u", type = "double", variable = "u" },
//!   { name = "x", type = "double *", variable = "x" },
//!   { name = "y", type = "double", variable = "y" },
//! ]
//! ```
//!
//! A library with a handle instead declares `instances = "multiple"` and, for example,
//! `init.args = [{ handle = "out" }, { dim = "n" }, { builtin = "step_size" }]` with
//! `step.args = [{ handle = "in" }, { array = "sp" }, { array = "pv" }, { array = "out" }]`.
//!
//! # Header import
//!
//! [`import_header`] parses a C header (flat `typedef struct` blocks and function prototypes
//! over the admitted types) and proposes a descriptor with `confirmed = false`: function roles,
//! dimension members and pointer→length relations are guessed by name and listed in the
//! proposal's notes. The developer edits the proposal and sets `confirmed = true`.
//!
//! # Calling convention
//!
//! Supported targets are aarch64 and x86_64 Linux. Up to eight pointer/integer and eight
//! floating-point arguments are passed per call ([`descriptor::MAX_INT_ARGS`]); larger
//! interfaces go through a struct.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests fail loudly"
    )
)]

pub mod adapter;
pub mod descriptor;
pub mod import;
pub mod layout;

mod ffi;

pub use adapter::{RawModel, arch_dir};
pub use descriptor::{DESCRIPTOR_FILE, Descriptor, DescriptorError, Plan};
pub use import::{ImportError, Proposal, import_header, parse_header, propose};

#[cfg(test)]
mod doc_example {
    use super::*;

    /// The example in the crate docs stays a valid, confirmed descriptor.
    #[test]
    fn the_documented_example_validates() {
        let source = include_str!("lib.rs");
        let start = source.find("//! ```toml").unwrap();
        let rest = &source[start + "//! ```toml".len()..];
        let end = rest.find("//! ```").unwrap();
        let toml: String = rest[..end]
            .lines()
            .map(|l| l.trim_start_matches("//!").trim_start_matches(' '))
            .collect::<Vec<_>>()
            .join("\n");
        let d = Descriptor::parse(&toml).unwrap();
        let plan = d.validate().unwrap();
        assert_eq!(plan.structs.len(), 2);
        assert_eq!(plan.structs[1].layout.size(), 40, "ss_params");
        assert_eq!(plan.structs[0].layout.size(), 24, "ss_io");
    }
}
