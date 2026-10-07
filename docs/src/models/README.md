# Making a model taktwerk-compatible

taktwerk has no ABI of its own. It hosts two kinds of model:

1. **FMI co-simulation FMUs** (FMI 3, also FMI 2). [FMI 3 is the recommended format](fmi.md):
   any tool that exports FMI 3 co-simulation works, and structural parameters carry the sizes.
2. **Plain C shared libraries**, described by a descriptor (`taktwerk-model.toml`) next to the
   library. New C models follow [the recommended shape](c.md); existing
   [other C interfaces](descriptor.md) are hosted through a hand-confirmed descriptor.

## The model convention

Whatever the format, a taktwerk model is:

- **Size-generic and plant-agnostic.** Variables have symbolic shapes over named dimensions
  (`y: f64[ny]`, `A: f64[nx, nx]`) with optional `min`, `max` and `default`. The model learns
  each size at init. A fixed-size model is one whose dimensions are literals.
- **Explicit about roles.** Each variable is an `input` (read every step), an `output` (written
  every step), a `parameter` (set once before init) or a `tunable` (set before init, changeable
  while running, delivered before the next step).
- **Allocation-free in its step.** The engine owns every buffer the model exchanges: it sizes
  them at init and passes pointers that stay valid for the instance's lifetime. A library may
  allocate its own state at init, never per step.
- **Honest about instances.** A library that keeps state per instance (behind a handle, or in
  structs the engine owns per instance) declares `multiple`. A library with state in globals
  declares `single`; taktwerk then loads a private copy of the file for each further instance.
- **Explicit about matrix layout:** row-major (C order) or column-major.
- **Fail-loud.** A step that cannot continue returns an error code; the engine stops
  (fail-stop) and the service manager restarts it. Never return success with garbage outputs.

The project file then binds every dimension, sets parameters and maps signals per instance; one
model serves many instances. See [Concepts](../concepts.md#instances-and-dimensions).

## Checking a model

```sh
taktwerk inspect model.fmu --kind fmi      # or: taktwerk inspect path/to/package --kind raw
taktwerk new model.fmu --kind fmi -o try.toml
taktwerk check try.toml
taktwerk run try.toml
```

`inspect` prints dimensions and variables exactly as the engine sees them; it is the quickest
way to confirm sizes, causalities, types and units came across.

## Wrapping a raw model as an FMU

A confirmed package can also leave taktwerk as a standard FMI 3 co-simulation FMU, for any
importer:

```sh
taktwerk fmu-wrap path/to/package                       # dry run: variables, structural parameters, targets
taktwerk fmu-wrap path/to/package --write -o model.fmu  # build for the host
taktwerk fmu-wrap path/to/package --target aarch64 --target x86_64 --write   # cross-build with zig
```

The generated C wrapper does what the raw adapter does, in C: it allocates the buffers from the
bound structural parameters (one `UInt64` per dimension, with the descriptor's `min`, `max` and
`default` as `start`), fills the struct members per call, loads the library itself (a `single`
library as a private copy per instance), checks `ok_codes` and reported lengths. The structs are
emitted for the target compiler and checked against taktwerk's layout at compile time. Text
buffers appear as `String` variables; a library told its step size (`builtin = "step_size"`) is
initialised on the first `fmi3DoStep`, which fixes the communication step. The FMU carries the
library and any `--bundle`d dependency under `binaries/<arch>-linux/` and the wrapper source
under `sources/`.
