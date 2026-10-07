# Getting started

## Install

taktwerk is not released yet; build it from a checkout. It needs a Rust toolchain (1.85 or newer)
and a C compiler for the examples.

```sh
git clone https://github.com/labinetix/taktwerk
cd taktwerk
cargo install --path crates/taktwerk     # installs `taktwerk` into ~/.cargo/bin
```

For a target other than the build host, see [cross-building](production.md#cross-building).

## A project in four commands

```sh
taktwerk inspect plant.fmu --kind fmi           # dimensions, variables, shapes
taktwerk new plant.fmu --kind fmi -o plant.toml # scaffold a project file
taktwerk check plant.toml                       # load, resolve, bind; print the plan
taktwerk run plant.toml                         # until SIGINT or SIGTERM
```

`new` writes a project with one instance, every dimension at the model's default, every input,
output and tunable mapped to its default signal name, commented-out parameters, and an OPC UA
server on `opc.tcp://127.0.0.1:4840`. Edit it, then `check` it: `check` loads every model,
binds every dimension, resolves every signal and connects every connector, so it fails on
anything `run` would fail on at init. It prints the plan:

```text
tick 10 ms

  instance  model    period  dims
  filt      lowpass  10 ms   n=3

  signal              direction  type  shape   max age
  taktwerk.heartbeat  system     u64   scalar  -
  taktwerk.status     system     i32   scalar  -
  taktwerk.cycle      system     u64   scalar  -
  taktwerk.overruns   system     u64   scalar  -
  taktwerk.stale      system     u64   scalar  -
  filt.y              output     f64   [3]     -
  filt.u              input      f64   [3]     -
  filt.gain           tunable    f64   scalar  -

ok: 1 instance(s), 8 signal(s), 1 connector(s) bound
```

`run` logs to stderr (`RUST_LOG` filters, default `info`), stops on SIGINT or SIGTERM, prints a
run summary and exits non-zero when a model or connector failed.

## Watch and edit with the TUI

```sh
taktwerk tui opc.tcp://127.0.0.1:4840
```

The TUI is an OPC UA client of the engine's own server. It shows status, heartbeat and counters,
and every signal with its value and age. Keys: `↑`/`↓` or `j`/`k` select, `PgUp`/`PgDn`,
`Home`/`End` (`g`/`G`) jump, `Enter` edits the selected input or tunable and `Enter` again
writes it, `Esc` cancels, `q` quits. If the project changes `namespace` or `system_prefix`, pass
`--namespace` and `--prefix`.

## The examples

The repository's [`examples/`](https://github.com/labinetix/taktwerk/tree/main/examples) run
from the repository root:

| example             | what it shows                                                        |
|---------------------|----------------------------------------------------------------------|
| `fmi-state-space`   | an FMI 3 FMU with structural parameters bound to one state           |
| `raw-pi`            | a PI controller from a plain C library, multiple instances by handle |
| `closed-loop`       | both in one engine, wired through the process image                  |
| `taktwerk.service`  | a systemd unit (see [Running in production](production.md))          |

```sh
# The FMU is the Reference FMU `StateSpace`, built for this host (needs git, network and cc).
crates/taktwerk-fmi/tests/reference-fmus.sh target/tmp/reference-fmus
taktwerk run examples/fmi-state-space/project.toml

# The PI controller is built from its C source.
examples/raw-pi/build.sh
taktwerk run examples/raw-pi/project.toml

# Both: the controller closes the loop around the plant.
taktwerk run examples/closed-loop/project.toml
taktwerk tui opc.tcp://127.0.0.1:4840            # edit ctrl.sp, watch plant.y follow
```
