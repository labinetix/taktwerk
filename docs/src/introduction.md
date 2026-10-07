# taktwerk

taktwerk runs compiled models at a fixed step on Linux, beside or on a PLC, and exchanges their
signals over OPC UA.

- **Models** are FMI 3 or FMI 2 co-simulation FMUs, or plain C libraries described by a small
  descriptor. A model is size-generic; an instance binds its sizes.
- **The engine owns a process image.** A real-time thread steps every model on absolute
  deadlines and reads a snapshot of the image at each tick; connectors sync the image with the
  outside in the background.
- **An own OPC UA server** exposes every signal, the engine status and a heartbeat, so any
  OPC UA client doubles as a UI. `taktwerk tui` is one.
- **One TOML project file** per engine describes models, instances, wiring and connectors.

<svg viewBox="0 0 720 250" role="img" aria-label="Process image diagram" style="max-width:100%;height:auto;font-family:sans-serif;font-size:14px">
  <defs>
    <marker id="a" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
      <path d="M0,0 L10,5 L0,10 z" fill="currentColor"/>
    </marker>
  </defs>
  <g fill="none" stroke="currentColor" stroke-width="1.5">
    <rect x="200" y="10" width="330" height="230" rx="8" stroke-dasharray="6 4"/>
    <rect x="20" y="40" width="140" height="50" rx="6"/>
    <rect x="230" y="40" width="140" height="50" rx="6"/>
    <rect x="230" y="160" width="140" height="50" rx="6"/>
    <rect x="400" y="40" width="110" height="50" rx="6"/>
    <rect x="400" y="160" width="110" height="50" rx="6"/>
    <rect x="570" y="40" width="130" height="50" rx="6"/>
    <rect x="570" y="160" width="130" height="50" rx="6"/>
    <line x1="160" y1="65" x2="228" y2="65" marker-end="url(#a)" marker-start="url(#a)"/>
    <line x1="300" y1="90" x2="300" y2="158" marker-end="url(#a)" marker-start="url(#a)"/>
    <line x1="370" y1="175" x2="398" y2="80" marker-end="url(#a)" marker-start="url(#a)"/>
    <line x1="370" y1="185" x2="398" y2="185" marker-end="url(#a)" marker-start="url(#a)"/>
    <line x1="510" y1="65" x2="568" y2="65" marker-end="url(#a)" marker-start="url(#a)"/>
    <line x1="510" y1="185" x2="568" y2="185" marker-end="url(#a)" marker-start="url(#a)"/>
  </g>
  <g fill="currentColor" text-anchor="middle">
    <text x="365" y="30">taktwerk</text>
    <text x="90" y="62">FMU or</text>
    <text x="90" y="80">C library</text>
    <text x="300" y="62">step thread</text>
    <text x="300" y="80">fixed period</text>
    <text x="300" y="190">process image</text>
    <text x="455" y="62">OPC UA</text>
    <text x="455" y="80">server</text>
    <text x="455" y="182">OPC UA</text>
    <text x="455" y="200">client</text>
    <text x="635" y="62">taktwerk tui,</text>
    <text x="635" y="80">any OPC UA client</text>
    <text x="635" y="190">PLC</text>
  </g>
</svg>

## What it is not

- Not hard real time: a fixed step on stock Linux, with optional `SCHED_FIFO`, CPU pinning and
  locked memory. Jitter is measured on the target, never assumed.
- Not a safety controller: models run in the engine's process. Whoever consumes the outputs,
  typically the PLC, owns the safe state once the heartbeat stops (see
  [Concepts](concepts.md#heartbeat-status-and-the-safety-contract)).

## Where to go next

- [Getting started](getting-started.md): build, scaffold, check and run a project.
- [Concepts](concepts.md): process image, ticks, instances, wiring, staleness, fail-stop.
- [Making a model taktwerk-compatible](models/README.md): for model developers.

taktwerk is licensed under MIT or Apache-2.0. Source:
[github.com/labinetix/taktwerk](https://github.com/labinetix/taktwerk).
