# FMI 3 (recommended)

taktwerk runs FMI 3 and FMI 2 **co-simulation** FMUs. Export from any tool that writes FMI 3
co-simulation for Linux; the FMU then runs unchanged in taktwerk and in every other FMI
importer. A `kind = "fmi"` model `path` is an `.fmu` archive (extracted to a private temporary
directory at load) or an already extracted FMU directory.

## Sizes: structural parameters

A size-generic FMI 3 model declares each size as a **structural parameter** and sizes its arrays
with `<Dimension valueReference="…"/>`. Each structural parameter becomes a taktwerk dimension:
its name is the dimension's name, `start` its default, `min` and `max` its bounds.

```xml
<ModelVariables>
  <UInt64 name="n" valueReference="1" causality="structuralParameter"
          variability="fixed" start="1" min="1" max="64"/>
  <Float64 name="tau" valueReference="2" causality="parameter" variability="fixed"
           start="1.0" unit="s">
    <Dimension valueReference="1"/>
  </Float64>
  <Float64 name="gain" valueReference="3" causality="parameter" variability="tunable"
           start="1.0"/>
  <Float64 name="u" valueReference="4" causality="input" start="0">
    <Dimension valueReference="1"/>
  </Float64>
  <Float64 name="y" valueReference="5" causality="output">
    <Dimension valueReference="1"/>
  </Float64>
</ModelVariables>
```

```text
$ taktwerk inspect filter.fmu --kind fmi
  dimension  min  max  default
  n          1    64   1

  variable  causality  type  shape   unit  description
  tau       parameter  f64   [n]     s
  gain      tunable    f64   scalar  -
  u         input      f64   [n]     -
  y         output     f64   [n]     -
```

- Structural parameters must have an integer type.
- A `<Dimension>` takes `start="4"` (a literal length), or a `valueReference` to a structural
  parameter or to a constant integer variable with a `start`.
- At instantiation taktwerk enters configuration mode, sets every structural parameter to the
  instance's bound length, and leaves it. Then it sets the instance's parameters and tunables.
- Arrays are row-major, as FMI 3 defines them.

## Variables and types

| FMI causality / variability          | taktwerk causality |
|--------------------------------------|--------------------|
| `input`                              | input              |
| `output`                             | output             |
| `parameter`, `fixed`                 | parameter          |
| `parameter`, `tunable`               | tunable            |
| `structuralParameter`                | a dimension        |
| `local`, `independent`, `calculatedParameter` | not exposed |

| FMI 3 type                                          | taktwerk type              |
|-----------------------------------------------------|----------------------------|
| `Float64`, `Float32`                                | `f64`, `f32`               |
| `Int64`, `Int32`, `Int16`, `Int8`                   | `i64`, `i32`, `i16`, `i8`  |
| `UInt64`, `UInt32`, `UInt16`, `UInt8`               | `u64`, `u32`, `u16`, `u8`  |
| `Boolean`                                           | `bool`                     |
| `Enumeration`                                       | `i64`                      |
| `String`, `Binary`, `Clock`                         | refused when exposed       |

Units come from `unit` or the declared type's unit; `description` is kept. Both show in
`inspect` and are informational.

FMI 2 FMUs are scalar only: `Real` is `f64`, `Integer` and `Enumeration` are `i32`, `Boolean`
is `bool`; other types are refused.

## Calling sequence

Per instance: instantiate (co-simulation, no event mode, the FMU's `resources/` directory as
resource path), configuration mode for structural parameters, parameters set, initialization
mode with the first inputs at `engine.start_time` (no tolerance, no stop time), then one
`DoStep` per period of `every × tick_ms`. Inputs are set before every step, changed tunables
before the next step; outputs are read after it. `OK` and `Warning` are success; any other
status, or a step that requests termination, stops the engine. Logging goes to the engine log
at debug level.

## Single vs multiple instances

`canBeInstantiatedOnlyOncePerProcess="true"` marks a single-instance FMU. taktwerk still runs
several instances of it: each further instance loads a private copy of the shared library, since
one loaded file shares its globals. Without the flag, all instances share one loaded library.
Prefer FMUs that keep all state in the instance.

## Platform binaries

taktwerk loads `binaries/<platform>/<modelIdentifier>.so` for the running architecture:

| FMI | x86_64                       | aarch64                                            |
|-----|------------------------------|----------------------------------------------------|
| 3   | `x86_64-linux`               | `aarch64-linux`                                    |
| 2   | `linux64`, then `x86_64-linux` | `aarch64-linux`, `linuxaarch64`, `linux-aarch64` |

An FMU without a binary for the target is refused at load, naming the platforms it has. Build
the binary against a glibc no newer than the target's (see
[cross-building](../production.md#cross-building)).

## Checklist

- [ ] FMI 3 (or FMI 2), with a `CoSimulation` element; model exchange alone is refused.
- [ ] Every size is a structural parameter with an integer type, a `start`, and `min`/`max`
      where the model has limits.
- [ ] Exposed variables use numeric or Boolean types only.
- [ ] Parameters that may change while running are `variability="tunable"`.
- [ ] A Linux binary for every target architecture, in the platform folder above.
- [ ] No allocation and no blocking I/O in `DoStep`.
- [ ] `taktwerk inspect model.fmu --kind fmi` shows the dimensions and variables you expect.
