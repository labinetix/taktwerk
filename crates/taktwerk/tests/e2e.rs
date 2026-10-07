//! The `taktwerk` binary end to end: scaffold, check and run the FMI example on a free localhost
//! port, drive it over OPC UA, stop it with SIGINT.
//!
//! Network: the first run builds the Reference FMUs with `reference-fmus.sh` (git fetch from
//! GitHub, then cc); later runs reuse them from the target directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "tests"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use opcua::client::{Client, ClientBuilder, IdentityToken, Session};
use opcua::crypto::SecurityPolicy;
use opcua::types::{
    Array, AttributeId, DataValue, MessageSecurityMode, NodeId, ReadValueId, StatusCode,
    TimestampsToReturn, Variant, VariantScalarTypeId, WriteValue,
};

const WAIT: Duration = Duration::from_secs(10);

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_taktwerk")
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The Reference FMUs for this host; built on first use (needs git, network and cc).
fn fmus() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("reference-fmus");
        let script = repo().join("crates/taktwerk-fmi/tests/reference-fmus.sh");
        let status = Command::new("bash").arg(script).arg(&dir).status().unwrap();
        assert!(status.success(), "building the Reference FMUs failed");
        dir
    })
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("e2e-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The raw PI package, copied out of `examples/raw-pi` and built with its `build.sh` (needs cc).
fn raw_pi() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = scratch("raw-pi-package");
        for f in ["pi.c", "pi.h", "taktwerk-model.toml", "build.sh"] {
            std::fs::copy(repo().join("examples/raw-pi").join(f), dir.join(f)).unwrap();
        }
        let status = Command::new("sh")
            .arg(dir.join("build.sh"))
            .status()
            .unwrap();
        assert!(status.success(), "building the PI library failed");
        dir
    })
}

/// Example `name` with absolute model paths (FMI models: the Reference `StateSpace`) and its
/// server on `port`, written to `dir`.
fn example_project(name: &str, dir: &Path, port: u16) -> PathBuf {
    let text =
        std::fs::read_to_string(repo().join("examples").join(name).join("project.toml")).unwrap();
    let mut doc: toml::Table = text.parse().unwrap();
    for (_, model) in doc["models"].as_table_mut().unwrap().iter_mut() {
        let model = model.as_table_mut().unwrap();
        let path = match model["kind"].as_str().unwrap() {
            "fmi" => fmus().join("fmi3/StateSpace"),
            _ => raw_pi().to_path_buf(),
        };
        model.insert("path".into(), path.display().to_string().into());
    }
    let ua = doc["connector"].as_array_mut().unwrap()[0]
        .as_table_mut()
        .unwrap();
    ua.insert(
        "endpoint".into(),
        format!("opc.tcp://127.0.0.1:{port}").into(),
    );
    ua.insert(
        "pki_dir".into(),
        dir.join("pki").display().to_string().into(),
    );
    let file = dir.join("project.toml");
    std::fs::write(&file, toml::to_string(&doc).unwrap()).unwrap();
    file
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

#[test]
fn check_prints_the_plan() {
    let dir = scratch("check");
    let file = example_project("fmi-state-space", &dir, free_port());
    let out = run(&["check", file.to_str().unwrap()], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("plant     lag    10 ms   m=1 n=1 r=1"),
        "{stdout}"
    );
    assert!(
        stdout.contains("plant.u             input      f64   [1]"),
        "{stdout}"
    );
    assert!(
        stdout.contains("ok: 1 instance(s), 12 signal(s), 1 connector(s) verified"),
        "{stdout}"
    );
}

#[test]
fn check_reports_every_problem() {
    let dir = scratch("problems");
    let file = dir.join("bad.toml");
    std::fs::write(
        &file,
        "[engine]\ntick_ms = 10.0\n[models.a]\nkind = \"fmi\"\npath = \"nope.fmu\"\n\
         [[connector]]\nid = \"bus\"\nkind = \"modbus\"\n",
    )
    .unwrap();
    let out = run(&["check", "bad.toml"], &dir);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("2 problems"), "{stderr}");
    assert!(
        stderr.contains("known kinds: opcua-server, opcua-client"),
        "{stderr}"
    );
}

#[test]
fn new_scaffolds_a_project_that_checks() {
    let dir = scratch("new");
    let model = fmus().join("fmi3/StateSpace");
    let port = free_port().to_string();
    let args = [
        "new",
        model.to_str().unwrap(),
        "--kind",
        "fmi",
        "-o",
        "p.toml",
        "--port",
        &port,
    ];
    let out = run(&args, &dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // A second write is refused without --force.
    let again = run(&args, &dir);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("--force"));
    let out = run(&["check", "p.toml"], &dir);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn inspect_prints_the_interface() {
    let model = fmus().join("fmi3/StateSpace");
    let out = run(
        &["inspect", model.to_str().unwrap(), "--kind", "fmi"],
        &repo(),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        stdout.contains("  A         tunable    f64   [n, n]"),
        "{stdout}"
    );
}

#[test]
fn import_header_proposes_an_unconfirmed_descriptor() {
    let dir = scratch("import");
    let header = repo().join("examples/raw-pi/pi.h");
    let out = run(&["import-header", header.to_str().unwrap()], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("confirmed = false"), "{stdout}");
    assert!(stdout.contains("symbol = \"pi_step\""), "{stdout}");
    let args = ["import-header", header.to_str().unwrap(), "-o", "pi.toml"];
    assert!(run(&args, &dir).status.success());
    assert!(!run(&args, &dir).status.success());
    assert!(std::fs::read_to_string(dir.join("pi.toml")).unwrap() == stdout);

    // The single-entry options reach the importer.
    let header = repo().join("crates/taktwerk-raw/tests/fixtures/blob_model.h");
    let out = run(
        &[
            "import-header",
            header.to_str().unwrap(),
            "--entry",
            "blob_call",
            "--arg-struct",
            "in=blob_input",
            "--arg-struct",
            "out=blob_output",
        ],
        &dir,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("reported = true"), "{stdout}");
    assert!(stdout.contains("abi.structs.blob_output"), "{stdout}");
    let out = run(
        &[
            "import-header",
            header.to_str().unwrap(),
            "--arg-struct",
            "nonsense",
        ],
        &dir,
    );
    assert!(!out.status.success());
}

/// The engine process; killed if the test fails before it is stopped.
struct Engine(Option<Child>);

impl Engine {
    fn interrupt(&mut self) -> Output {
        let child = self.0.take().unwrap();
        let status = Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        child.wait_with_output().unwrap()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Peer {
    _client: Client,
    session: Arc<Session>,
}

impl Peer {
    async fn connect(url: &str, pki: &Path) -> Self {
        let start = Instant::now();
        loop {
            let mut client = ClientBuilder::new()
                .application_name("e2e")
                .application_uri("urn:e2e")
                .pki_dir(pki)
                .create_sample_keypair(false)
                .trust_server_certs(true)
                .session_retry_limit(0)
                .client()
                .unwrap();
            let connected = client
                .connect_to_matching_endpoint(
                    (
                        url,
                        SecurityPolicy::None.to_str(),
                        MessageSecurityMode::None,
                    ),
                    IdentityToken::Anonymous,
                )
                .await;
            if let Ok((session, event_loop)) = connected {
                event_loop.spawn();
                tokio::time::timeout(WAIT, session.wait_for_connection())
                    .await
                    .unwrap();
                return Self {
                    _client: client,
                    session,
                };
            }
            assert!(start.elapsed() < WAIT, "server never came up");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn read(&self, ns: u16, name: &str) -> DataValue {
        self.session
            .read(
                &[ReadValueId::new(NodeId::new(ns, name), AttributeId::Value)],
                TimestampsToReturn::Both,
                0.0,
            )
            .await
            .unwrap()
            .remove(0)
    }

    async fn u64(&self, ns: u16, name: &str) -> u64 {
        match self.read(ns, name).await.value {
            Some(Variant::UInt64(n)) => n,
            other => panic!("{name}: {other:?}"),
        }
    }

    async fn f64s(&self, ns: u16, name: &str) -> Vec<f64> {
        match self.read(ns, name).await.value {
            Some(Variant::Array(a)) => a
                .values
                .iter()
                .map(|v| match v {
                    Variant::Double(x) => *x,
                    other => panic!("{name}: {other:?}"),
                })
                .collect(),
            other => panic!("{name}: {other:?}"),
        }
    }
}

/// `taktwerk run` on example `name`, with a client session once it is Running.
async fn start(name: &str) -> (Engine, Peer, u16) {
    let dir = scratch(&format!("run-{name}"));
    let port = free_port();
    let file = example_project(name, &dir, port);
    let engine = Engine(Some(
        Command::new(bin())
            .args(["run", file.to_str().unwrap()])
            .current_dir(&dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let peer = Peer::connect(
        &format!("opc.tcp://127.0.0.1:{port}"),
        &dir.join("peer-pki"),
    )
    .await;
    let ns = peer
        .session
        .get_namespace_index("urn:taktwerk")
        .await
        .unwrap();
    let start = Instant::now();
    loop {
        let status = peer.read(ns, "taktwerk.status").await.value;
        if status == Some(Variant::Int32(1)) {
            break;
        }
        assert!(start.elapsed() < WAIT, "never Running: {status:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (engine, peer, ns)
}

fn f64s(v: &[f64]) -> Variant {
    let values: Vec<Variant> = v.iter().copied().map(Variant::Double).collect();
    Variant::Array(Box::new(
        Array::new(VariantScalarTypeId::Double, values).unwrap(),
    ))
}

/// Poll scalar element 0 of `name` until `done` holds; returns it.
async fn until(peer: &Peer, ns: u16, name: &str, done: impl Fn(f64) -> bool) -> f64 {
    let start = Instant::now();
    loop {
        let v = peer.f64s(ns, name).await[0];
        if done(v) {
            return v;
        }
        assert!(start.elapsed() < WAIT, "{name} stuck at {v}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// SIGINT: a clean exit 0 with the run summary on stdout.
async fn stop(mut engine: Engine, peer: Peer) {
    let _ = peer.session.disconnect().await;
    let out = engine.interrupt();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{:?}\n{stdout}\n{stderr}", out.status);
    assert!(stdout.starts_with("cycles "), "{stdout}");
    assert!(stdout.contains(" overruns "), "{stdout}");
    assert!(stderr.contains("SIGINT received"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_serves_steps_and_stops_on_sigint() {
    let (engine, peer, ns) = start("fmi-state-space").await;

    // The heartbeat advances.
    let first = peer.u64(ns, "taktwerk.heartbeat").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let later = peer.u64(ns, "taktwerk.heartbeat").await;
    assert!(later >= first + 10, "heartbeat {first} -> {later}");

    // Tunables show the project's start values.
    assert_eq!(peer.f64s(ns, "plant.A").await, [-1.0]);

    // A step on the input: y follows the first-order lag towards 2.
    assert_eq!(peer.f64s(ns, "plant.y").await, [0.0]);
    let status = peer
        .session
        .write(&[WriteValue::value_attr(
            NodeId::new(ns, "plant.u"),
            f64s(&[2.0]),
        )])
        .await
        .unwrap();
    assert_eq!(status, [StatusCode::Good]);
    let y = until(&peer, ns, "plant.y", |y| y > 1.0).await;
    assert!(y < 2.0, "{y}");

    stop(engine, peer).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_raw_controller_closes_the_loop_around_an_fmu() {
    let (engine, peer, ns) = start("closed-loop").await;
    assert_eq!(peer.f64s(ns, "plant.y").await, [0.0]);
    let status = peer
        .session
        .write(&[WriteValue::value_attr(
            NodeId::new(ns, "ctrl.sp"),
            f64s(&[1.0]),
        )])
        .await
        .unwrap();
    assert_eq!(status, [StatusCode::Good]);
    // The PI's integral removes the offset: y settles at the setpoint.
    until(&peer, ns, "plant.y", |y| (y - 1.0).abs() < 0.02).await;
    let u = peer.f64s(ns, "ctrl.u").await[0];
    assert!((u - 1.0).abs() < 0.1, "u = {u}");
    stop(engine, peer).await;
}
