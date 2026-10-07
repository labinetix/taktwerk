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

/// The FMI example with its model path made absolute and its server on `port`.
fn example_project(dir: &Path, port: u16) -> PathBuf {
    let text =
        std::fs::read_to_string(repo().join("examples/fmi-state-space/project.toml")).unwrap();
    let mut doc: toml::Table = text.parse().unwrap();
    let model = doc["models"]["lag"].as_table_mut().unwrap();
    model.insert(
        "path".into(),
        fmus().join("fmi3/StateSpace").display().to_string().into(),
    );
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
    let file = example_project(&dir, free_port());
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
    assert!(stdout.contains("ok: 1 instance(s), 12 signal(s), 1 connector(s) bound"));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_serves_steps_and_stops_on_sigint() {
    let dir = scratch("run");
    let port = free_port();
    let file = example_project(&dir, port);
    let mut engine = Engine(Some(
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

    // Running, and the heartbeat advances.
    let start = Instant::now();
    let first = loop {
        let status = peer.read(ns, "taktwerk.status").await.value;
        if status == Some(Variant::Int32(1)) {
            break peer.u64(ns, "taktwerk.heartbeat").await;
        }
        assert!(start.elapsed() < WAIT, "never Running: {status:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let later = peer.u64(ns, "taktwerk.heartbeat").await;
    assert!(later >= first + 10, "heartbeat {first} -> {later}");

    // Tunables show the project's start values.
    assert_eq!(peer.f64s(ns, "plant.A").await, [-1.0]);

    // A step on the input: y follows the first-order lag towards 2.
    assert_eq!(peer.f64s(ns, "plant.y").await, [0.0]);
    let u = Variant::Array(Box::new(
        Array::new(VariantScalarTypeId::Double, vec![Variant::Double(2.0)]).unwrap(),
    ));
    let status = peer
        .session
        .write(&[WriteValue::value_attr(NodeId::new(ns, "plant.u"), u)])
        .await
        .unwrap();
    assert_eq!(status, [StatusCode::Good]);
    let start = Instant::now();
    let y = loop {
        let y = peer.f64s(ns, "plant.y").await[0];
        if y > 1.0 {
            break y;
        }
        assert!(start.elapsed() < WAIT, "y stuck at {y}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(y < 2.0, "{y}");

    let _ = peer.session.disconnect().await;
    let out = engine.interrupt();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{:?}\n{stdout}\n{stderr}", out.status);
    assert!(stdout.starts_with("cycles "), "{stdout}");
    assert!(stderr.contains("SIGINT received"), "{stderr}");
}
