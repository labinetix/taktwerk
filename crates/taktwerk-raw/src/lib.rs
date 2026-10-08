//! Raw C library model adapter for taktwerk: header import, descriptors, C layout.
//!
//! A raw model is a shared library with an init, a step and optionally a terminate function (or
//! one function serving as both init and step), described by a descriptor the engine reads. The engine owns every buffer, lays structs out
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
//! | `{ const = 7, type = "int" }` | `T`         | the same number on every call                   |
//! | `{ phase = { init = 1, step = 0 }, type = "int" }` | `T` | one number for init, another for step and terminate |
//!
//! `type` (default `int`) applies to `dim`, `const` and `phase`; written as a pointer
//! (`int *`), the argument is the address of engine-owned storage holding the value.
//! `init` and `step` may name the same symbol: a library with a single entry point tells the
//! two calls apart by a `phase` argument or member.
//!
//! A struct in `[abi.structs.<s>]` lists its members with their C type in declaration order;
//! each member maps to at most one of
//!
//! | member (`name`, `type` omitted)          | C member     | holds                                     |
//! |------------------------------------------|--------------|-------------------------------------------|
//! | `{ variable = "v" }`                     | `T *`        | the address of `v`'s buffer               |
//! | `{ variable = "v" }`                     | `T`          | scalar `v` (see below)                    |
//! | `{ dim = "n" }`                          | `int`/`int *`| the bound length of `n`                   |
//! | `{ dim = "n", reported = true }`         | `int`/`int *`| a length the library writes (see below)   |
//! | `{ builtin = "step_size" }`              | `double`     | the step size or time                     |
//! | `{ const = 9 }`                          | `T`/`T *`    | the same number on every call             |
//! | `{ phase = { init = 1, step = 0 } }`     | `T`/`T *`    | one number for init, another afterwards   |
//!
//! A scalar member mapped to a `variable` carries the value: inputs, parameters and tunables
//! are written before each call, outputs read after it. A pointer member may point at a
//! scalar variable of any causality; one the library writes back (a counter in an input
//! struct) is mapped to an output variable, whose buffer the engine never overwrites. A pointer
//! member carrying a `dim`, `const` or `phase` points at engine-owned storage. Unmapped pointer
//! members are `NULL`, unmapped scalars zero. Admitted C types are `double`, `float`, `bool`,
//! `char` and the fixed-width and plain integer types at their 64-bit Linux widths, each
//! optionally `const` and optionally followed by one `*`.
//!
//! A `reported` length is one the library knows and the engine does not have to tell it: it is
//! zeroed before init, then compared with the bound length after init and after every step; a
//! difference fails the call with both values. The check runs after the call returned, so a
//! library loaded with the wrong sizes may already have written at its own size. Every
//! dimension a member reports therefore needs a `max`, and the engine allocates each buffer
//! shaped by it at that `max` while exchanging only the bound length; text and byte buffers
//! keep their literal capacity.
//!
//! Text travels as bytes: a `char *` member maps to a `u8` variable with a literal shape, its
//! capacity with the terminating NUL included (`shape = [32]`). The caller supplies text through
//! a parameter or input, the library writes it into an output. A `uint8_t` or `uint8_t *`
//! member may also map to a `bool` variable: the engine keeps the bytes and reads any non-zero
//! byte as true.
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
//! # Single entry point
//!
//! A common legacy style exports one function for every call, taking an integer id and two
//! opaque pointers the library casts to its input and output structs. Which call runs is a
//! flag the caller sets, and the library reports its own sizes at init:
//!
//! ```c
//! typedef struct { int32_t *first; int32_t variant; double *k; double *u; int32_t *count; } io_in;
//! typedef struct { double *y; char *label; int32_t *n_u; int32_t *n_y; } io_out;
//! int model_call(int id, char *in, char *out);  /* 0 on success */
//! ```
//!
//! ```toml
//! name = "single-entry"
//!
//! [[dimensions]]
//! name = "nu"
//! max = 16
//!
//! [[dimensions]]
//! name = "ny"
//! max = 16
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
//! shape = ["nu"]
//!
//! [[variables]]
//! name = "count"
//! causality = "output"
//! type = "i32"
//!
//! [[variables]]
//! name = "y"
//! causality = "output"
//! type = "f64"
//! shape = ["ny"]
//!
//! [[variables]]
//! name = "label"
//! causality = "output"
//! type = "u8"
//! shape = [32]
//!
//! [abi]
//! confirmed = true
//!
//! [abi.init]
//! symbol = "model_call"
//! args = [{ const = 7, type = "int" }, { struct = "io_in" }, { struct = "io_out" }]
//!
//! [abi.step]
//! symbol = "model_call"
//! args = [{ const = 7, type = "int" }, { struct = "io_in" }, { struct = "io_out" }]
//!
//! [abi.structs.io_in]
//! members = [
//!   { name = "first", type = "int32_t *", phase = { init = 1, step = 0 } },
//!   { name = "variant", type = "int32_t", const = 1 },
//!   { name = "k", type = "double *", variable = "k" },
//!   { name = "u", type = "double *", variable = "u" },
//!   { name = "count", type = "int32_t *", variable = "count" },
//! ]
//!
//! [abi.structs.io_out]
//! members = [
//!   { name = "y", type = "double *", variable = "y" },
//!   { name = "label", type = "char *", variable = "label" },
//!   { name = "n_u", type = "int32_t *", dim = "nu", reported = true },
//!   { name = "n_y", type = "int32_t *", dim = "ny", reported = true },
//! ]
//! ```
//!
//! # Header import
//!
//! [`import_header`] parses a C header (flat `typedef struct` blocks and function prototypes
//! over the admitted types). A header in the [recommended shape](shape) is read completely and
//! comes out with `confirmed = true`. Any other gets a proposal with `confirmed = false`:
//! function roles, dimension members and pointer→length relations are guessed by name and
//! listed in the proposal's notes, after every deviation from the shape. The developer edits
//! the proposal and sets `confirmed = true`. [`ImportOptions::require_shape`] makes a deviation
//! an error instead.
//!
//! [`import_header_with`] takes what the header cannot say: [`ImportOptions::entry`] names a
//! single entry point and [`ImportOptions::arg_structs`] which struct each opaque `char *` or
//! `void *` parameter carries. The proposal then also guesses a flag-like integer as a
//! `phase`, size-like integer pointers in an output struct as `reported` lengths, and a leading
//! integer argument as a `const` whose value is left to the developer.
//!
//! # Wrapping as an FMU
//!
//! [`fmu::generate`] turns a confirmed descriptor into an FMI 3 co-simulation wrapper (C source
//! and `modelDescription.xml`), [`fmu::build::build`] compiles and packs it with the library;
//! `taktwerk fmu-wrap` is the command.
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
pub mod fmu;
pub mod import;
pub mod layout;
pub mod shape;

mod ffi;

pub use adapter::{RawModel, arch_dir};
pub use descriptor::{DESCRIPTOR_FILE, Descriptor, DescriptorError, Plan};
pub use import::{
    ImportError, ImportOptions, Proposal, import_header, import_header_with, parse_header, propose,
    propose_with,
};

#[cfg(test)]
mod doc_example {
    use super::*;

    /// The `n`th TOML example in the crate docs.
    fn example(n: usize) -> Descriptor {
        let source = include_str!("lib.rs");
        let mut rest = source;
        for _ in 0..=n {
            let start = rest.find("//! ```toml").unwrap();
            rest = &rest[start + "//! ```toml".len()..];
        }
        let end = rest.find("//! ```").unwrap();
        let toml: String = rest[..end]
            .lines()
            .map(|l| l.trim_start_matches("//!").trim_start_matches(' '))
            .collect::<Vec<_>>()
            .join("\n");
        Descriptor::parse(&toml).unwrap()
    }

    /// The examples in the crate docs stay valid, confirmed descriptors.
    #[test]
    fn the_documented_examples_validate() {
        let plan = example(0).validate().unwrap();
        assert_eq!(plan.structs.len(), 2);
        assert_eq!(plan.structs[1].layout.size(), 40, "ss_params");
        assert_eq!(plan.structs[0].layout.size(), 24, "ss_io");

        let single = example(1);
        assert_eq!(single.abi.init, single.abi.step);
        let plan = single.validate().unwrap();
        assert_eq!(plan.cells, 3, "first, n_u, n_y");
    }
}
