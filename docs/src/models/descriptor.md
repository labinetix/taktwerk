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

A library is called through at most three functions:

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

`type` on a `dim` argument is any admitted integer type (default `int`). Handles need
`instances = "multiple"`; init has at most one `handle = "out"`. A `value` argument cannot be an
output or a shaped variable. Up to eight pointer/integer and eight floating-point arguments are
passed per call; larger interfaces go through a struct.

## Structs

A struct in `[abi.structs.<s>]` lists its members with their C type in declaration order.

- A pointer member mapped to a `variable` gets the buffer's address.
- A scalar member mapped to a `variable` carries the value: inputs, parameters and tunables are
  written before each call, outputs read after it. A shaped variable needs a pointer member.
- A scalar integer member mapped to a `dim` carries the bound length.
- A `double` member mapped to a `builtin` carries the step size or the time.
- Unmapped pointer members are `NULL`, unmapped scalars zero.

Each member maps at most one of `variable`, `dim` and `builtin`, and a variable member's C type
must match the variable's type. Each instance owns one image per struct, the same in every call,
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
| `abi.init`, `abi.step`, `abi.terminate` | `symbol`, `returns` (`int` or `void`), `args`         |
| `abi.structs.<s>.members` | `name`, `type` (C spelling), one of `variable`, `dim`, `builtin`    |

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

## Calling convention

Supported targets are aarch64 and x86_64 Linux, with the platform's standard C calling
convention.
