# Project file

One TOML file per engine is the source of truth. Unknown keys are errors. Relative paths are
resolved against the project file's directory.

```toml
[engine]
tick_ms = 10.0
input_max_age_ms = 100.0

[engine.realtime]
policy = "fifo"
priority = 80
cpu = 3
lock_memory = true

[models.pid]
kind = "raw"            # or "fmi"
path = "models/pid"     # package directory, .fmu, or extracted FMU directory

[[instance]]            # instances run in declared order
id = "loop1"
model = "pid"
every = 2               # period = every × tick_ms
dims = { n = 3, m = "from-server" }
parameters = { kp = 1.5, ki = [0.1, 0.2, 0.3] }
inputs = { y = { signal = "plant.y", max_age_ms = 50.0 }, sp = "loop1.sp" }
outputs = { u = "plant.u" }
tunables = { kp = "tuning.kp" }

[[connector]]
id = "ua"
kind = "opcua-server"
```

## `[engine]`

| key                | type   | default      | meaning                                                         |
|--------------------|--------|--------------|-----------------------------------------------------------------|
| `tick_ms`          | float  | required     | base tick, milliseconds, > 0; every period is a multiple of it  |
| `start_time`       | float  | `0.0`        | model time of the first tick, seconds                           |
| `input_max_age_ms` | float  | none         | default age limit of external inputs; none never goes stale     |
| `system_prefix`    | string | `"taktwerk"` | prefix of the engine's own signals; non-empty                   |
| `realtime`         | table  | none         | real-time settings of the cycle thread, below                   |

### `[engine.realtime]`

Applies to the cycle thread only; the I/O threads keep the default scheduling.

| key           | type    | default  | meaning                                            |
|---------------|---------|----------|----------------------------------------------------|
| `policy`      | string  | required | `"fifo"` (`SCHED_FIFO`) or `"rr"` (`SCHED_RR`)     |
| `priority`    | integer | required | 1 to 99                                            |
| `cpu`         | integer | none     | CPU the cycle thread is pinned to                  |
| `lock_memory` | bool    | `false`  | `mlockall` before the first tick                   |

A refused setting (missing privilege, CPU out of range) stops the engine at start. See
[Running in production](production.md#real-time) for the limits it needs.

## `[models.<id>]`

| key    | type   | meaning                                                              |
|--------|--------|----------------------------------------------------------------------|
| `kind` | string | `"fmi"` (FMI 2 or 3 co-simulation) or `"raw"` (C library package)    |
| `path` | string | an `.fmu`, an extracted FMU directory, or a raw package directory    |

## `[[instance]]`

| key          | type    | default | meaning                                                      |
|--------------|---------|---------|--------------------------------------------------------------|
| `id`         | string  | required | unique, non-empty, no `.`                                    |
| `model`      | string  | required | a key of `[models]`                                          |
| `every`      | integer | `1`     | period in ticks, ≥ 1                                         |
| `dims`       | table   | `{}`    | dimension → length or `"from-server"`                        |
| `parameters` | table   | `{}`    | parameter or tunable → start value                           |
| `inputs`     | table   | `{}`    | input → signal, or `{ signal, max_age_ms }`                  |
| `outputs`    | table   | `{}`    | output → signal                                              |
| `tunables`   | table   | `{}`    | tunable → signal                                             |

Every key in `dims`, `parameters`, `inputs`, `outputs` and `tunables` must name a dimension or a
variable of that causality in the model.

### `dims`

- `n = 3`: a fixed length, checked against the dimension's `min` and `max`.
- `n = "from-server"`: the length is discovered at init. The model needs a one-dimensional
  variable of shape `[n]`; the engine asks each connector for the length of the external item
  mapped to that variable's signal. Today the [OPC UA client](connectors.md#opc-ua-client)
  answers for nodes in its `map`.
- Unbound: the model's `default`; without one, init fails.

### `parameters`

Start values of parameters and tunables. Unset ones keep the model's value: the FMU's start
value, or zero for a raw library.

- A scalar takes a number or a boolean. Integers convert to floats; floats never convert to
  integers.
- An array takes nested arrays, one level per dimension, first index outermost:
  `A = [[1.0, 0.0], [0.0, 1.0]]`. The engine stores it in the variable's layout.
- An array also takes one flat array of the full length, taken as already stored in the
  variable's layout: `A = [1.0, 0.0, 0.0, 1.0]`.

### `inputs`, `outputs`, `tunables`

Each maps a variable to a signal name. Unmapped variables get `<instance>.<variable>`. An input
binding is either a signal name or a table:

```toml
inputs = { y = "plant.y" }                                   # signal only
inputs = { y = { signal = "plant.y", max_age_ms = 50.0 } }   # with an age limit
```

`max_age_ms` overrides `engine.input_max_age_ms` for that input. It is refused on a wired input
(one some instance outputs). When several instances consume one external signal, the explicit
limits they set must agree; one explicit limit wins over the default. See
[Concepts](concepts.md#signals-and-wiring) for wiring rules.

## `[[connector]]`

| key  | type   | meaning                                              |
|------|--------|------------------------------------------------------|
| `id`   | string | unique                                               |
| `kind` | string | `"opcua-server"` or `"opcua-client"`                 |

All other keys belong to the connector kind; see [Connectors](connectors.md). A project may have
several connectors of one kind, for example one client per PLC.

## Changes

Everything in the project file is read at start. Structural changes (models, dimensions, wiring,
connectors) need a restart. While running, only tunables change, through a connector.
