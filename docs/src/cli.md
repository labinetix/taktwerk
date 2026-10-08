# CLI reference

`taktwerk` logs to stderr. `RUST_LOG` sets the filter; the default is `info` for `run` and
`warn` otherwise. Exit status is non-zero when a command fails, when `check` finds a problem,
and when `run` ends on a model or connector failure.

The sections below are the program's own `--help` output.

## `taktwerk`

```text
Fixed-step model execution engine for Linux with its own OPC UA server

Usage: taktwerk <COMMAND>

Commands:
  check          Load a project, its models and connectors, resolve and bind it, print the plan
  run            Run a project until SIGINT or SIGTERM
  inspect        Print a model's interface: dimensions, variables, shapes
  new            Scaffold a project file for one model; prints it unless asked to write
  import-header  Read a raw model descriptor from a C header, or propose one; prints it unless asked to write
  fmu-wrap       Wrap a confirmed raw model package as an FMI 3 co-simulation FMU; a dry run unless asked to write
  tui            Monitor a running engine over OPC UA and edit its inputs and tunables
  help           Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

## `taktwerk check`

```text
Load a project, its models and connectors, resolve and bind it, print the plan

Usage: taktwerk check <PROJECT>

Arguments:
  <PROJECT>  Project file

Options:
  -h, --help  Print help
```

## `taktwerk run`

```text
Run a project until SIGINT or SIGTERM

Usage: taktwerk run <PROJECT>

Arguments:
  <PROJECT>  Project file

Options:
  -h, --help  Print help
```

## `taktwerk inspect`

```text
Print a model's interface: dimensions, variables, shapes

Usage: taktwerk inspect --kind <KIND> <MODEL>

Arguments:
  <MODEL>
          Model path: an .fmu, an extracted FMU or a raw package directory

Options:
      --kind <KIND>
          Model kind

          Possible values:
          - fmi: FMI 2 or FMI 3 co-simulation FMU (`.fmu` or an extracted directory)
          - raw: Raw C library package (`taktwerk-model.toml` and libraries)

  -h, --help
          Print help (see a summary with '-h')
```

## `taktwerk new`

```text
Scaffold a project file for one model; prints it unless asked to write

Usage: taktwerk new [OPTIONS] --kind <KIND> <MODEL>

Arguments:
  <MODEL>
          Model path

Options:
      --kind <KIND>
          Model kind

          Possible values:
          - fmi: FMI 2 or FMI 3 co-simulation FMU (`.fmu` or an extracted directory)
          - raw: Raw C library package (`taktwerk-model.toml` and libraries)

  -o, --output <OUTPUT>
          Write the project to this file instead of printing it

      --write
          Write the project to `taktwerk.toml` (or `--output`)

      --force
          Overwrite an existing file

      --tick-ms <TICK_MS>
          Base tick, milliseconds

          [default: 10]

      --port <PORT>
          Port of the own OPC UA server

          [default: 4840]

  -h, --help
          Print help (see a summary with '-h')
```

## `taktwerk import-header`

```text
Read a raw model descriptor from a C header, or propose one; prints it unless asked to write

Usage: taktwerk import-header [OPTIONS] <HEADER>

Arguments:
  <HEADER>  C header declaring the model's functions and structs

Options:
  -o, --output <OUTPUT>            Write the descriptor to this file instead of printing it
      --force                      Overwrite an existing file
      --entry <ENTRY>              The one function that serves as init and step (single entry point)
      --arg-struct <PARAM=STRUCT>  Which struct an opaque `char *`/`void *` parameter of the entry carries, as `<param>=<struct>`; repeatable
      --shape                      Require the recommended shape: fail with every deviation instead of proposing an unconfirmed descriptor
  -h, --help                       Print help
```

## `taktwerk fmu-wrap`

```text
Wrap a confirmed raw model package as an FMI 3 co-simulation FMU; a dry run unless asked to write

Usage: taktwerk fmu-wrap [OPTIONS] <PACKAGE>

Arguments:
  <PACKAGE>
          The raw model package (directory with `taktwerk-model.toml` and `lib/<arch>/`)

Options:
  -o, --output <OUTPUT>
          The `.fmu` to write; default `<modelIdentifier>.fmu` in the working directory

      --target <ARCH>
          Target architecture (`aarch64`, `x86_64`), repeatable; default the host.

          Another target needs `zig` on PATH (`zig cc -target <arch>-linux-gnu.2.25`) and the package's library for it.

      --bundle <LIB>
          A further library to ship beside the model's, repeatable.

          A bare file name is taken from the package's `lib/<arch>/` per target, a path is copied as is; both land in `binaries/<arch>-linux/`.

      --write
          Build and write the FMU instead of printing what it would contain

  -h, --help
          Print help (see a summary with '-h')
```

## `taktwerk tui`

```text
Monitor a running engine over OPC UA and edit its inputs and tunables

Usage: taktwerk tui [OPTIONS] <ENDPOINT>

Arguments:
  <ENDPOINT>  The engine's server, e.g. `opc.tcp://127.0.0.1:4840`

Options:
      --namespace <NAMESPACE>  Namespace URI of the signal nodes [default: urn:taktwerk]
      --prefix <PREFIX>        The engine's `system_prefix` [default: taktwerk]
  -h, --help                   Print help
```
