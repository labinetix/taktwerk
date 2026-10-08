# Changelog

## v0.1.0

First release.

- Engine: process image, one real-time cycle thread on absolute deadlines, several models per
  engine with periods in base ticks, stale-input faulting, heartbeat and status, fail-stop.
- Models: FMI 3 and FMI 2 co-simulation FMUs (FMI 3 arrays with structural parameters, String
  variables as byte buffers); raw C libraries through a descriptor with symbolic dimensions,
  including single-entry interfaces, constants, phase values, library-reported sizes and text.
- `import-header` reads the recommended C shape into a ready descriptor and proposes one for
  any other header; `fmu-wrap` builds an FMI 3 FMU from a raw package.
- Connectors: an own OPC UA server exposing every signal, and an OPC UA client with fail-closed
  node verification and reconnect.
- CLI: `new`, `check`, `run`, `inspect`, `import-header`, `fmu-wrap`, `tui`.
- Guide at https://labinetix.github.io/taktwerk/.
