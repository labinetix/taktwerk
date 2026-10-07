# Concepts

## Process image

The engine owns one value per signal, the process image. Every value lives in a typed buffer
sized at init and never resized. Connectors write inputs and tunables and read outputs from
their own threads; the cycle thread copies what it needs at its tick and stores outputs back.
Each signal is locked only for one copy.

A signal has a direction:

| direction | written by                       | read by                      |
|-----------|----------------------------------|------------------------------|
| input     | a connector                      | the instances that consume it |
| output    | the instance that produces it    | connectors, other instances  |
| tunable   | a connector, while running       | one instance, before its step |
| system    | the engine                       | connectors                   |

Element types are `f64`, `f32`, `i64`, `i32`, `i16`, `i8`, `u64`, `u32`, `u16`, `u8` and `bool`.
A signal is a scalar or an array of any rank; matrices are stored row-major or column-major as
the model declares.

## Ticks and periods

One real-time thread wakes on an absolute deadline grid (`clock_nanosleep` with
`TIMER_ABSTIME`) every `engine.tick_ms`. Each instance has a period of `every` ticks and is due
at every tick whose number is a multiple of `every`, so all instances are due at tick 0.

Per tick:

1. The external inputs and tunables of every due instance are copied from the image.
2. If any of those inputs is stale, the tick is skipped (see [stale inputs](#stale-inputs)).
3. Otherwise the due instances step in declared order. Each first copies its wired inputs, so it
   sees outputs stored earlier in the same tick.
4. Outputs are stored, the heartbeat advances and the tick is published: connectors now see a
   complete, consistent set of outputs.

Model time is `start_time + tick × tick_ms`; an instance advances by its step size
`every × tick_ms` per step. A tick that ends after the next deadline skips the missed slots
and counts each in `overruns`; the tick counter stays contiguous, so after an overrun model time
lags wall time by the skipped slots. Nothing on the tick path allocates.

## Instances and dimensions

A model is size-generic: its variables have symbolic shapes (`u: f64[n]`, `A: f64[nx, nx]`)
over named dimensions with an optional `min`, `max` and `default`. An instance references a
model and binds every dimension, from the project file, from the model's default, or
`"from-server"` (the length a connector finds outside). The bound sizes are structural: they
are fixed for the run, and a change needs a restart.

One model serves any number of instances. Whether one loaded library can host several is the
model's `instances` capability: `multiple` libraries keep their state per instance; for a
`single` library (state in globals), each further instance loads a private copy of the file.

Parameters are set once before init. Tunables are set before init and change while running;
a changed value is delivered before the instance's next step. A tunable that no connector has
written yet shows the model's own start value after init, so writing one tunable never zeroes
another.

## Signals and wiring

An unmapped input, output or tunable `v` of instance `i` is the signal `i.v`. Mapping a
variable in the project file names its signal:

- An input mapped to a signal that some instance outputs is **wired** to it. It reads what the
  producer stored last: the same tick when the producer runs earlier in declared order, else the
  previous one. A wired input never goes stale.
- An input nobody outputs is an **external** input, written by a connector.
- Several instances may consume one signal.

Errors at init: two outputs on one signal, a tunable on an input or output signal, one signal
used with two types, shapes or layouts, or a mapping onto one of the engine's own signals.

## Stale inputs

Each external input may carry a maximum age, `max_age_ms` on the binding or
`engine.input_max_age_ms` for all. Without one, an input never goes stale. With one, an input
is stale when its last write is older than that at the tick, or when it was never written.

A stale input skips the whole tick: no instance steps, outputs and heartbeat are held, `status`
becomes `faulted` and `stale` counts the tick. The fault is published at once. When inputs are
fresh again the next tick runs normally and `status` returns to `running`.

An OPC UA client connector that loses its server stops writing its inputs, so they age and the
engine turns them stale.

## Heartbeat, status and the safety contract

The engine publishes its own signals under `engine.system_prefix` (default `taktwerk`):

| signal               | type  | meaning                                             |
|----------------------|-------|-----------------------------------------------------|
| `taktwerk.heartbeat` | `u64` | advances once per tick whose outputs were published |
| `taktwerk.status`    | `i32` | `0` init, `1` running, `2` faulted, `3` stopped     |
| `taktwerk.cycle`     | `u64` | every tick, skipped ones included                   |
| `taktwerk.overruns`  | `u64` | deadline slots missed                               |
| `taktwerk.stale`     | `u64` | ticks skipped for a stale input                     |

Models run inside the engine's process; there is no process isolation. **Whoever consumes the
outputs, typically the PLC, owns the safe state.** It watches the heartbeat (map it to a PLC
node with the [OPC UA client](connectors.md#opc-ua-client)) and falls back to its safe state
when the heartbeat stops advancing or `status` is not `1`. The heartbeat advances only on a tick
whose outputs were published, so a held heartbeat means the outputs are not current.

## Fail-stop

A model error (a non-success return code, or a failed init) terminates every instance and ends
the run with that error; `taktwerk run` exits non-zero. A connector that fails beyond its own
reconnect logic stops the run the same way. The engine does not retry: restart is the service
manager's job (see [Running in production](production.md)).
