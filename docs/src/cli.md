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
  import-header  Propose a raw model descriptor from a C header; prints it unless asked to write
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
Propose a raw model descriptor from a C header; prints it unless asked to write

Usage: taktwerk import-header [OPTIONS] <HEADER>

Arguments:
  <HEADER>  C header declaring the model's functions and structs

Options:
  -o, --output <OUTPUT>  Write the descriptor to this file instead of printing it
      --force            Overwrite an existing file
  -h, --help             Print help
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
