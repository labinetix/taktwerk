# Connectors

A connector syncs part of the process image with the outside. At init the engine builds each
connector from its `[[connector]]` table, asks it for `"from-server"` lengths, builds the image
and lets each connector verify its mapping (fail-closed). While running, connectors work on
their own I/O threads; the cycle thread never waits for them.

Two kinds exist: the engine's own OPC UA server and an OPC UA client.

## OPC UA server

`kind = "opcua-server"` exposes every signal of the image, the engine's own signals included.

```toml
[[connector]]
id = "ua"
kind = "opcua-server"
endpoint = "opc.tcp://0.0.0.0:4840"
namespace = "urn:taktwerk"
security = ["none"]
anonymous = true
users = [{ user = "op", password_env = "UA_PASSWORD" }]
pki_dir = "./pki"
trust_client_certs = false
```

| key                  | default                    | meaning                                                         |
|----------------------|----------------------------|-----------------------------------------------------------------|
| `endpoint`           | `"opc.tcp://0.0.0.0:4840"` | listen URL, `opc.tcp://host:port`                               |
| `namespace`          | `"urn:taktwerk"`           | namespace URI of the signal nodes                               |
| `security`           | `["none"]`                 | offered message security, one endpoint each: `"none"`, `"sign"`, `"sign-encrypt"` |
| `anonymous`          | `true`                     | accept anonymous sessions                                       |
| `users`              | `[]`                       | accepted users: `{ user, password }` or `{ user, password_env }` |
| `pki_dir`            | `"./pki"`                  | server certificate and key, trusted and rejected client certificates |
| `trust_client_certs` | `false`                    | trust every client certificate, not only those in `<pki_dir>/trusted` |

`"sign"` and `"sign-encrypt"` use the Basic256Sha256 policy. The server certificate is generated
in `pki_dir` when a secure endpoint needs one. A user's password comes from exactly one of
`password` (inline) and `password_env` (the name of an environment variable); prefer
`password_env` so no secret sits in the project file.

**The server is open by default:** anonymous, no encryption, all interfaces. On any network
other than a closed machine network, bind it to one interface, turn off `anonymous`, add users
and offer only secure endpoints.

### Nodes

- Every signal is a variable node with NodeId `ns=<index of namespace>;s=<signal name>`, for
  example `ns=2;s=plant.y` (the index is assigned by the server; read it from the server's
  namespace array, or address the node by URI as `nsu=urn:taktwerk;s=plant.y`).
- The browse path splits the name on `.` into folders under `Objects`: `plant.y` is
  `Objects/plant/y`, `taktwerk.heartbeat` is `Objects/taktwerk/heartbeat`.
- Data types: `f64` Double, `f32` Float, `i64` Int64, `i32` Int32, `i16` Int16, `i8` SByte,
  `u64` UInt64, `u32` UInt32, `u16` UInt16, `u8` Byte, `bool` Boolean.
- A scalar has ValueRank −1. An array has its rank and `ArrayDimensions`; matrix elements are
  sent in row-major order whatever the model's storage layout.
- Outputs and system signals are read-only. Inputs and tunables are writable; a write goes to
  the image and is picked up at the next tick.
- Every node follows each published tick, with the time the value was written into the image as
  its source timestamp. A value never written has status `UncertainInitialValue`.

## OPC UA client

`kind = "opcua-client"` syncs mapped signals with another OPC UA server, typically a PLC's.

```toml
[[connector]]
id = "plc"
kind = "opcua-client"
endpoint = "opc.tcp://192.168.0.10:4840"
security = "none"
credentials = { user = "op", password_env = "PLC_PASSWORD" }
request_timeout_ms = 1000
reconnect_backoff_ms = 1000
sync_period_ms = 100
stamp = "receive"

[connector.map]
"plant.u" = "ns=4;s=Plant.u"                       # input: read from the PLC
"plant.y" = "nsu=urn:example:plc;s=Plant.y"        # output: written to the PLC
"taktwerk.heartbeat" = "ns=4;s=Plant.heartbeat"    # the PLC watches it
"taktwerk.status" = "ns=4;s=Plant.status"
```

| key                    | default            | meaning                                                         |
|------------------------|--------------------|-----------------------------------------------------------------|
| `endpoint`             | required           | server URL, `opc.tcp://host:port[/path]`                        |
| `security`             | `"none"`           | `"none"`, `"sign"` or `"sign-encrypt"` (Basic256Sha256)         |
| `credentials`          | anonymous          | `{ user, password }` or `{ user, password_env }`                |
| `request_timeout_ms`   | `1000`             | bound on every request                                          |
| `reconnect_backoff_ms` | `1000`             | first wait before a reconnect; doubles per failure up to 16 times this |
| `sync_period_ms`       | `100`              | period of the input read                                        |
| `stamp`                | `"receive"`        | time an input read is stamped with: `"receive"` or `"source"`   |
| `pki_dir`              | `"./pki-client"`   | client certificate and trusted server certificates (secure sessions only) |
| `trust_server_certs`   | `false`            | trust every server certificate, not only those in `<pki_dir>/trusted` |
| `map`                  | `{}`               | signal name → NodeId                                            |

### Map

Each key is a signal of the image; each value a NodeId: `ns=<index>;s=<id>`, `nsu=<uri>;s=<id>`
(the namespace is resolved by URI on every session), or the `i=`, `g=`, `b=` forms. Unmapped
signals are not synced.

- Inputs and tunables are **read** from their nodes, one bulk Read every `sync_period_ms`.
- Outputs and system signals are **written** to theirs, one bulk Write per published tick.

### Verification

Every session is verified before it syncs: the first at init, every reconnect again. For each
mapped node the client reads DataType, ValueRank, ArrayDimensions, AccessLevel and the value,
and refuses the session listing every mismatch:

- the node exists and its DataType is the signal's type;
- a scalar signal maps to a scalar node, an array signal to an array node with the same element
  count (from `ArrayDimensions`, or the current value when the server reports none);
- the node is readable, and writable when the engine writes it.

Array elements travel in the signal's storage order. At init a refusal, or an unreachable
server, stops the engine before the first tick. After a reconnect a refusal is logged and
retried with backoff.

### Reconnect and staleness

A lost session is closed and reconnected with exponential backoff. While disconnected, inputs
are not written, so they age; with a `max_age_ms` the engine turns them stale and skips ticks
until reads resume. An input read with a bad status, or a value that does not fit the signal,
leaves that signal aging as well; the problem is logged once until it clears.

### Stamp

`stamp = "receive"` stamps an input with the time the read response arrived: an input is as
fresh as the last successful read. `stamp = "source"` uses the server's source timestamp (or
the receive time when it sends none). Servers that stamp a value only when it changes make a
constant input look old under `"source"`.

### `from-server`

For an instance dimension bound `"from-server"`, the client answers with the length of the node
mapped to the dimension's signal (its `ArrayDimensions`, else its current value's length).
