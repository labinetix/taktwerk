# Design

taktwerk runs compiled models at a fixed step on Linux, beside or on a PLC, and exchanges their
signals over OPC UA. Nothing is implemented yet; this file records the decisions the code follows.

## Scope

- **Linux only:** aarch64 and x86_64, cross-built against a low glibc floor. The same binary runs
  on a PC, a single-board computer or a Linux-based PLC; a container app with a web UI for PLC app
  platforms is a later packaging step.
- **Soft real time:** a fixed step on stock Linux. Jitter is published as measured, never claimed.

## Execution

- **The engine owns a process image.** Connectors sync it in the background; the step reads a
  snapshot taken at its tick, and its outputs are published after the step. Each input carries a
  maximum age, and a stale input faults the cycle.
- **One real-time thread on absolute deadlines** (`clock_nanosleep`, `TIMER_ABSTIME`); optional
  `SCHED_FIFO`, priority, affinity and `mlockall` apply to that thread only. No allocation on the
  cycle path.
- **Several models per engine.** Each period is an integer multiple of the base tick; models run
  in declared order and exchange data only through the image. An overrun delays the models after
  it in that tick and is counted.

## Models and instances

- **Two adapters, no own ABI:** FMI 3 and FMI 2 co-simulation FMUs, and raw C libraries described
  by a descriptor imported from their header.
- **A model is size-generic and plant-agnostic.** A model package (one library per architecture
  plus a descriptor, versioned and hashed as one) declares signals and parameters with symbolic
  dimensions (`y: f64[ny]`, `A: f64[nx, nx]`), optional constraints and defaults, and how the
  library learns each size. This is FMI 3's structural parameters, used for both adapters. A
  fixed-size model is one whose dimensions are literals.
- **An instance is plant-specific.** It references a model, binds every dimension, sets parameters,
  maps signals to the image and picks its period. One model serves many instances.
- **The project file is the size authority.** A connected server must match the bound sizes or init
  fails; an instance may opt into `"from-server"` discovery for a dimension. Sizes are structural:
  a change needs a restart.
- **The engine owns the memory.** Buffers are sized at init from the bound dimensions and passed as
  pointers that stay valid for the instance's lifetime. A library may allocate at init, never per
  step.
- **Length relations are declared, not inferred.** The header importer proposes which pointer
  takes which dimension; the model developer confirms it once, in the descriptor.
- **A raw descriptor declares `instances = "single" | "multiple"`.** A second instance of a
  single-instance library loads a private copy of it, since one path shares its globals.
- **Matrices declare their layout,** row- or column-major.
- **The engine knows no producer.** A toolchain that wants its models run (labinetix among them)
  emits a model package or an FMU.

## Safety

- **Models run in-process.** taktwerk publishes a heartbeat and a status; whoever consumes the
  outputs, typically the PLC, owns the safe state once the heartbeat stops. No process isolation.

## I/O

- **An own OPC UA server** exposes every signal, the status and the heartbeat, so any OPC UA client
  doubles as a UI.
- **The server is open by default** (anonymous, no encryption, all interfaces); security is
  configured per project.
- **Connectors:** the OPC UA client (to a PLC's server) and the own server. Others (Modbus TCP,
  MQTT) come later behind the same connector trait.

## Configuration and surfaces

- **One TOML project file per engine** is the source of truth: models, schedule, signals,
  connectors. Structural changes need a restart; parameters marked tunable change live through the
  OPC server.
- **CLI and TUI first;** a web UI follows, served by the engine over a small HTTP API.

## Repository

- Public, crates on crates.io as `taktwerk-*`, licensed MIT OR Apache-2.0.
- No company, vendor or customer names anywhere: code, docs, tests, fixtures, commit messages.
- The code starts from the labinetix OPC engine, copied with fresh history. labinetix keeps its
  engine until taktwerk runs its plants on hardware, then deletes it.

## Open

- The crate split.
- Whether labinetix plants reach taktwerk as a descriptor or as an FMU; parity with today's
  enveloped run is proven once on hardware.
- The web UI stack.
