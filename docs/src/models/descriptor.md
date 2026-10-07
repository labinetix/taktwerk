# Other C interfaces: the descriptor

An existing C library that does not follow [the recommended shape](c.md) is hosted as it is,
through a descriptor that says how to call it. The engine owns every buffer, lays structs out by
C's rules for the running target, fills pointers once per call and copies inputs in and outputs
out around each step.

## Workflow

1. `taktwerk import-header model.h -o taktwerk-model.toml` proposes a descriptor. The importer
   reads flat `typedef struct { … } Name;` blocks and function prototypes over the admitted
   types; it is not a C parser (no macro expansion) and skips, with a note, anything it does not
   cover.
2. Read the notes at the top of the proposal. Function roles (init, step, terminate), dimension
   members and pointer→length relations are guessed by name; every guess is listed. Pointers are
   inputs when `const` and outputs otherwise; scalars by value are parameters unless the struct
   name hints otherwise.
3. Fix causalities (which values are parameters, which tunables), shapes, matrix layouts (the
   importer proposes one dimension per pointer), dimension bounds and defaults.
4. Set `abi.confirmed = true`. An unconfirmed descriptor is refused at load.
5. Put the library at `lib/<arch>/` next to the descriptor and run `taktwerk inspect <dir> --kind
   raw`.

## Package

```text
<package>/
  taktwerk-model.toml      the descriptor
  lib/aarch64/<name>.so    one library per architecture; the running one is picked
  lib/x86_64/<name>.so
```

## Calls

A library is called through at most three functions, or one function serving as both init and
step (see [single entry point](#single-entry-point)):

- `[abi.init]` once per instance, after the engine has allocated and filled the buffers
  (parameters, first inputs, dimension lengths);
- `[abi.step]` once per step, after inputs and changed tunables were copied in; outputs are
  copied out afterwards;
- `[abi.terminate]` (optional) once when the instance is released, also after an error.

Each function returns `int` (`returns = "int"`, the default), where any value in `ok_codes`
(default `[0]`) is success and anything else fails the call, or `void` (`returns = "void"`).
Arguments are listed in C order; each is exactly one of:

| argument                      | C parameter | filled with                                     |
|-------------------------------|-------------|-------------------------------------------------|
| `{ struct = "s" }`            | `s *`       | the address of the engine's image of `s`        |
| `{ array = "v" }`             | `T *`       | the address of variable `v`'s buffer            |
| `{ value = "v" }`             | `T`         | scalar variable `v` (input, parameter, tunable) |
| `{ dim = "n", type = "int" }` | integer     | the bound length of dimension `n`               |
| `{ builtin = "step_size" }`   | `double`    | the instance's step, seconds                    |
| `{ builtin = "time" }`        | `double`    | the time the call advances from                 |
| `{ handle = "out" }`          | `void **`   | init only: the library stores its handle        |
| `{ handle = "in" }`           | `void *`    | the stored handle                               |
| `{ const = 7, type = "int" }` | `T`         | the same number on every call                   |
| `{ phase = { init = 1, step = 0 }, type = "int" }` | `T` | one number for init, another for step and terminate |

`type` (default `int`) applies to `dim`, `const` and `phase`. Written as a pointer (`int *`),
the argument is the address of engine-owned storage holding the value. A `const` or `phase`
number is an integer literal (any type that holds it) or a float literal (`double`, `float`
only). Handles need
`instances = "multiple"`; init has at most one `handle = "out"`. A `value` argument cannot be an
output or a shaped variable. Up to eight pointer/integer and eight floating-point arguments are
passed per call; larger interfaces go through a struct.

## Structs

A struct in `[abi.structs.<s>]` lists its members with their C type in declaration order;
each member maps to at most one of:

| member (`name`, `type` omitted)          | C member      | holds                                     |
|------------------------------------------|---------------|-------------------------------------------|
| `{ variable = "v" }`                     | `T *`         | the address of `v`'s buffer               |
| `{ variable = "v" }`                     | `T`           | scalar `v` (see below)                    |
| `{ dim = "n" }`                          | `int`/`int *` | the bound length of `n`                   |
| `{ dim = "n", reported = true }`         | `int`/`int *` | a length the library writes (see below)   |
| `{ builtin = "step_size" }`              | `double`      | the step size or the time                 |
| `{ const = 9 }`                          | `T`/`T *`     | the same number on every call             |
| `{ phase = { init = 1, step = 0 } }`     | `T`/`T *`     | one number for init, another afterwards   |

- A scalar member mapped to a `variable` carries the value: inputs, parameters and tunables are
  written before each call, outputs read after it. A shaped variable needs a pointer member.
- A pointer member may point at a scalar variable of any causality. One the library writes back
  (a counter in an input struct) is mapped to an output variable, whose buffer the engine never
  overwrites.
- A pointer member carrying a `dim`, `const` or `phase` points at engine-owned storage.
- Unmapped pointer members are `NULL`, unmapped scalars zero.

A variable member's C type must match the variable's type, with one exception: a `uint8_t` or
`uint8_t *` member may carry a `bool` variable; the engine keeps the bytes and reads any non-zero
byte as true.

**Reported lengths.** A `dim` member with `reported = true` is a length the library knows and
writes itself. It is zeroed before init, then compared with the bound length after init and
after every step; a difference fails the call with both values. The check runs after the call
returned, so it catches a library loaded with the wrong sizes but cannot undo a write it already
made beyond a buffer bound too small. The project file stays the size authority.

**Text** travels as bytes: a `char *` member maps to a `u8` variable with a literal shape, its
capacity with the terminating NUL included (`shape = [32]`). The caller supplies text through a
parameter or input; the library writes it into an output. Each instance owns one image per struct, the same in every call,
so a library may keep a struct pointer it got at init.

## C types

Admitted are `double`, `float`, `bool`, `char` and the fixed-width and plain integer types at
their 64-bit Linux widths (`int` is `i32`, `long` and `size_t` are 64-bit, `char` is a byte),
each optionally `const` and optionally followed by one `*`. Pointers to pointers are refused,
except the `void **` of `handle = "out"`.

## Keys

| key                       | meaning                                                             |
|---------------------------|---------------------------------------------------------------------|
| `name`                    | model name, informational                                           |
| `instances`               | `"single"` (default; state in globals) or `"multiple"`              |
| `[[dimensions]]`          | `name`, optional `min`, `max`, `default`                            |
| `[[variables]]`           | `name`, `causality` (`input`, `output`, `parameter`, `tunable`), `type`, `shape` (dimension names or literal lengths; empty for a scalar), `layout` (`row-major` default, or `column-major`), optional `unit`, `description` |
| `abi.confirmed`           | `true` once the developer checked every pointer→length relation     |
| `abi.library`             | file name in `lib/<arch>/`; default: the only `.so` there            |
| `abi.ok_codes`            | success return values of `int` calls; default `[0]`                 |
| `abi.init`, `abi.step`, `abi.terminate` | `symbol`, `returns` (`int` or `void`), `args`; init and step may share a symbol |
| `abi.structs.<s>.members` | `name`, `type` (C spelling), at most one of `variable`, `dim` (with optional `reported`), `builtin`, `const`, `phase` |

A `single` library keeps its state in globals, so each further instance loads a private copy of
the file; a `multiple` library carries its state behind a handle or inside a struct the engine
owns per instance.

## Example

A discrete state-space model `x' = A x + b u`, `y = k cᵀx`, size-generic in `nx`, with this C
interface:

```c
typedef struct { int nx; const double *A; const double *b; const double *c; double k; } ss_params;
typedef struct { double u; double *x; double y; } ss_io;
int  ss_init(const ss_params *p, double dt);
int  ss_step(const ss_params *p, ss_io *io, double t);
void ss_terminate(void);
```

```toml
name = "state-space"
instances = "single"

[[dimensions]]
name = "nx"
min = 1
max = 64

[[variables]]
name = "A"
causality = "parameter"
type = "f64"
shape = ["nx", "nx"]
layout = "row-major"

[[variables]]
name = "b"
causality = "parameter"
type = "f64"
shape = ["nx"]

[[variables]]
name = "c"
causality = "parameter"
type = "f64"
shape = ["nx"]

[[variables]]
name = "k"
causality = "tunable"
type = "f64"

[[variables]]
name = "u"
causality = "input"
type = "f64"

[[variables]]
name = "x"
causality = "output"
type = "f64"
shape = ["nx"]

[[variables]]
name = "y"
causality = "output"
type = "f64"

[abi]
confirmed = true
ok_codes = [0]

[abi.init]
symbol = "ss_init"
args = [{ struct = "ss_params" }, { builtin = "step_size" }]

[abi.step]
symbol = "ss_step"
args = [{ struct = "ss_params" }, { struct = "ss_io" }, { builtin = "time" }]

[abi.terminate]
symbol = "ss_terminate"
returns = "void"

[abi.structs.ss_params]
members = [
  { name = "nx", type = "int", dim = "nx" },
  { name = "A", type = "const double *", variable = "A" },
  { name = "b", type = "const double *", variable = "b" },
  { name = "c", type = "const double *", variable = "c" },
  { name = "k", type = "double", variable = "k" },
]

[abi.structs.ss_io]
members = [
  { name = "u", type = "double", variable = "u" },
  { name = "x", type = "double *", variable = "x" },
  { name = "y", type = "double", variable = "y" },
]
```

A library with a handle instead declares `instances = "multiple"` and, for example,
`init.args = [{ handle = "out" }, { dim = "n" }, { builtin = "step_size" }]` with
`step.args = [{ handle = "in" }, { array = "sp" }, { array = "pv" }, { array = "out" }]`. The
repository's
[`examples/raw-pi`](https://github.com/labinetix/taktwerk/tree/main/examples/raw-pi) is such a
library, with its confirmed descriptor.

## Single entry point

A common legacy style exports one function for every call, taking an integer id and two opaque
pointers the library casts to its input and output structs. Which call runs is a flag the caller
sets, and the library reports its own sizes at init:

```c
typedef struct { int32_t *first; int32_t variant; double *k; double *u; int32_t *count; } io_in;
typedef struct { double *y; char *label; int32_t *n_u; int32_t *n_y; } io_out;
int model_call(int id, char *in, char *out);  /* 0 on success */
```

```toml
name = "single-entry"

[[dimensions]]
name = "nu"

[[dimensions]]
name = "ny"

[[variables]]
name = "k"
causality = "tunable"
type = "f64"

[[variables]]
name = "u"
causality = "input"
type = "f64"
shape = ["nu"]

[[variables]]
name = "count"
causality = "output"
type = "i32"

[[variables]]
name = "y"
causality = "output"
type = "f64"
shape = ["ny"]

[[variables]]
name = "label"
causality = "output"
type = "u8"
shape = [32]

[abi]
confirmed = true

[abi.init]
symbol = "model_call"
args = [{ const = 7, type = "int" }, { struct = "io_in" }, { struct = "io_out" }]

[abi.step]
symbol = "model_call"
args = [{ const = 7, type = "int" }, { struct = "io_in" }, { struct = "io_out" }]

[abi.structs.io_in]
members = [
  { name = "first", type = "int32_t *", phase = { init = 1, step = 0 } },
  { name = "variant", type = "int32_t", const = 1 },
  { name = "k", type = "double *", variable = "k" },
  { name = "u", type = "double *", variable = "u" },
  { name = "count", type = "int32_t *", variable = "count" },
]

[abi.structs.io_out]
members = [
  { name = "y", type = "double *", variable = "y" },
  { name = "label", type = "char *", variable = "label" },
  { name = "n_u", type = "int32_t *", dim = "nu", reported = true },
  { name = "n_y", type = "int32_t *", dim = "ny", reported = true },
]
```

`init` and `step` name the same symbol; the `phase` member `first` tells the two calls apart,
`const` fills the id and a fixed `variant`, the library writes the step counter back through
`count` (an output), and `n_u`, `n_y` are reported lengths checked against the instance's bound
`nu` and `ny`.

The `taktwerk-raw` crate's `import_header_with` imports such a header when given the entry point
and the struct behind each opaque parameter; it then also guesses a flag-like integer as a `phase`, size-like integer
pointers in an output struct as `reported` lengths, and a leading integer argument as a `const`
whose value is left to the developer. `taktwerk import-header` does not take these options yet:
start from the example above, or from the plain proposal, and fill in the roles by hand.

## Calling convention

Supported targets are aarch64 and x86_64 Linux, with the platform's standard C calling
convention.
