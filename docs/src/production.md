# Running in production

## systemd

Run one engine per unit. The service manager owns restarts: taktwerk stops on a model error
(fail-stop) and exits non-zero, and `Restart=on-failure` brings it back.

```ini
[Unit]
Description=taktwerk model engine
After=network-online.target
Wants=network-online.target

[Service]
User=taktwerk
WorkingDirectory=/srv/taktwerk
ExecStart=/usr/local/bin/taktwerk run /srv/taktwerk/project.toml
Restart=on-failure
RestartSec=2
Environment=RUST_LOG=info

# Let [engine.realtime] apply SCHED_FIFO, a priority and mlockall without root.
LimitRTPRIO=90
LimitMEMLOCK=infinity
# Optional: keep the rest of the process (OPC UA I/O) off the cycle thread's CPU.
# CPUAffinity=0-2

[Install]
WantedBy=multi-user.target
```

```sh
sudo install -m 0644 taktwerk.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now taktwerk
journalctl -u taktwerk -f
```

The same unit is in the repository as
[`examples/taktwerk.service`](https://github.com/labinetix/taktwerk/blob/main/examples/taktwerk.service).
`WorkingDirectory` matters for relative paths in connector settings such as `pki_dir`; model
paths are relative to the project file. Passwords referenced by `password_env` go into the unit
with `Environment=` or an `EnvironmentFile=` readable only by root.

## Real time

`[engine.realtime]` applies to the cycle thread only:

```toml
[engine.realtime]
policy = "fifo"       # SCHED_FIFO; or "rr"
priority = 80         # 1..=99, at most the unit's LimitRTPRIO
cpu = 3               # pin the cycle thread
lock_memory = true    # mlockall before the first tick
```

What each setting needs:

| setting       | needs                                                                |
|---------------|----------------------------------------------------------------------|
| `policy`, `priority` | `LimitRTPRIO` ≥ `priority`, or `CAP_SYS_NICE`                |
| `lock_memory` | `LimitMEMLOCK=infinity`, or `CAP_IPC_LOCK`                           |
| `cpu`         | a CPU the process's cgroup allows                                    |

If a setting is refused the engine does not start. A dedicated CPU helps most when nothing else
runs on it: keep it free of other work (`isolcpus=` on the kernel command line, or a cpuset),
start the process on the other CPUs with the unit's `CPUAffinity=`, and let `cpu` pin the cycle
thread to the free one. The I/O threads stay where `CPUAffinity=` put them.

Measure on the target: `taktwerk run` prints the mean and longest tick and the overrun count when
it stops, and `taktwerk.overruns` is live on the OPC UA server.

## Cross-building

taktwerk runs on aarch64 and x86_64 Linux. Build on a development machine for the target with
[`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild), which links against an older
glibc than the host's so one binary runs on older target systems:

```sh
cargo install --locked cargo-zigbuild      # also needs `zig` on the PATH
rustup target add aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu

# The suffix after the target is the glibc version to link against: the oldest your targets run.
cargo zigbuild --release -p taktwerk --target aarch64-unknown-linux-gnu.<glibc>
cargo zigbuild --release -p taktwerk --target x86_64-unknown-linux-gnu.<glibc>
```

The binaries land in `target/<target>/release/taktwerk`. taktwerk has no native dependencies
beyond libc; its OPC UA security is pure Rust.

Model libraries are separate files and must run on the target too: build raw libraries per
architecture into `lib/aarch64/` and `lib/x86_64/` (`zig cc -target aarch64-linux-gnu.<glibc>`
works for C), and ship FMUs with a binary for the target platform
(see [FMI 3](models/fmi.md#platform-binaries)).
