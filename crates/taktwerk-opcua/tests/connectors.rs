//! Both connectors against real OPC UA peers on localhost.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    missing_docs,
    reason = "tests"
)]

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use opcua::client::{Client, ClientBuilder, DataChangeCallback, IdentityToken, Session};
use opcua::crypto::SecurityPolicy;
use opcua::nodes::{AccessLevel, VariableBuilder};
use opcua::server::diagnostics::NamespaceMetadata;
use opcua::server::node_manager::memory::{SimpleNodeManager, simple_node_manager};
use opcua::server::{ANONYMOUS_USER_TOKEN_ID, ServerBuilder, ServerEndpoint, ServerHandle};
use opcua::types::{
    Array, AttributeId, DataTypeId, DataValue, MessageSecurityMode, MonitoredItemCreateRequest,
    NodeId, ObjectId, QualifiedName, ReadValueId, StatusCode, TimestampsToReturn, Variant,
    VariantScalarTypeId, WriteValue,
};
use taktwerk_core::connector::{Connector, ConnectorError};
use taktwerk_core::image::{CycleImage, Direction, ImageHandle, ImageLayout, SignalSpec, image};
use taktwerk_core::project::ConnectorConfig;
use taktwerk_core::value::{Buffer, Layout, ScalarType};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(5);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn scratch_dir(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "taktwerk-opcua-test-{}-{name}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn connector_config(text: &str) -> ConnectorConfig {
    toml::from_str(text).unwrap()
}

fn signal(name: &str, ty: ScalarType, shape: &[usize], direction: Direction) -> SignalSpec {
    SignalSpec {
        name: name.to_owned(),
        ty,
        shape: shape.to_vec(),
        layout: Layout::RowMajor,
        direction,
        max_age: None,
    }
}

fn f64s(v: &[f64]) -> Variant {
    let values: Vec<Variant> = v.iter().copied().map(Variant::Double).collect();
    Variant::Array(Box::new(
        Array::new(VariantScalarTypeId::Double, values).unwrap(),
    ))
}

/// Poll `f` until it yields `Some`, or fail after [`WAIT`].
async fn eventually<T, F, Fut>(what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A plain test client session.
struct Peer {
    _client: Client,
    session: Arc<Session>,
    events: JoinHandle<StatusCode>,
}

impl Peer {
    async fn connect(url: &str) -> Self {
        let pki = scratch_dir("peer-pki");
        let mut client = ClientBuilder::new()
            .application_name("test-peer")
            .application_uri("urn:test-peer")
            .pki_dir(&pki)
            .create_sample_keypair(false)
            .trust_server_certs(true)
            .session_retry_limit(0)
            .client()
            .unwrap();
        let _ = std::fs::remove_dir_all(pki);
        let (session, event_loop) = client
            .connect_to_matching_endpoint(
                (
                    url,
                    SecurityPolicy::None.to_str(),
                    MessageSecurityMode::None,
                ),
                IdentityToken::Anonymous,
            )
            .await
            .unwrap();
        let events = event_loop.spawn();
        tokio::time::timeout(WAIT, session.wait_for_connection())
            .await
            .unwrap();
        Self {
            _client: client,
            session,
            events,
        }
    }

    async fn read(&self, node: &NodeId, attr: AttributeId) -> DataValue {
        self.session
            .read(
                &[ReadValueId::new(node.clone(), attr)],
                TimestampsToReturn::Both,
                0.0,
            )
            .await
            .unwrap()
            .remove(0)
    }

    async fn write(&self, node: &NodeId, value: Variant) -> StatusCode {
        self.session
            .write(&[WriteValue::value_attr(node.clone(), value)])
            .await
            .unwrap()[0]
    }

    async fn close(self) {
        let _ = self.session.disconnect().await;
        self.events.abort();
    }
}

// ---------------------------------------------------------------------------------------------
// Server connector
// ---------------------------------------------------------------------------------------------

struct Engine {
    cycle: CycleImage,
    handle: ImageHandle,
    stop: watch::Sender<bool>,
    task: JoinHandle<Result<(), ConnectorError>>,
}

impl Engine {
    fn store(&self, name: &str, value: Buffer) {
        let id = self.cycle.layout().id(name).unwrap();
        self.cycle.store(id, &value, Instant::now()).unwrap();
    }

    fn read(&self, name: &str) -> (Buffer, Option<Instant>) {
        let id = self.handle.layout().id(name).unwrap();
        let spec = self.handle.layout().spec(id).unwrap();
        let mut out = Buffer::zeroed(spec.ty, spec.len());
        let stamp = self.handle.read(id, &mut out).unwrap();
        (out, stamp)
    }

    async fn stop(self) -> Result<(), ConnectorError> {
        self.stop.send_replace(true);
        tokio::time::timeout(WAIT, self.task)
            .await
            .unwrap()
            .unwrap()
    }
}

async fn start(mut connector: Box<dyn Connector>, layout: ImageLayout) -> Engine {
    connector.bind(&layout).await.unwrap();
    let (cycle, handle) = image(layout);
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(connector.run(handle.clone(), shutdown));
    Engine {
        cycle,
        handle,
        stop,
        task,
    }
}

fn server_layout() -> ImageLayout {
    let mut matrix = signal("plant.A", ScalarType::F64, &[2, 3], Direction::Output);
    matrix.layout = Layout::ColumnMajor;
    ImageLayout::new(vec![
        signal("plant.u", ScalarType::F64, &[], Direction::Output),
        signal("plant.y", ScalarType::F64, &[3], Direction::Input),
        signal("loop.kp", ScalarType::F32, &[], Direction::Tunable),
        signal("plant.on", ScalarType::Bool, &[], Direction::Input),
        signal(
            "taktwerk.heartbeat",
            ScalarType::U64,
            &[],
            Direction::System,
        ),
        matrix,
    ])
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_exposes_reads_writes_and_notifies() {
    let port = free_port();
    let url = format!("opc.tcp://127.0.0.1:{port}");
    let pki = scratch_dir("server-pki");
    let config = connector_config(&format!(
        "id = \"ua\"\nkind = \"opcua-server\"\nendpoint = \"{url}\"\npki_dir = \"{}\"\n",
        pki.display()
    ));
    let engine = start(taktwerk_opcua::build(&config).unwrap(), server_layout()).await;
    engine.store("plant.u", Buffer::F64(vec![1.5]));
    engine.store(
        "plant.A",
        Buffer::F64(vec![0.0, 10.0, 1.0, 11.0, 2.0, 12.0]),
    );
    engine.store("taktwerk.heartbeat", Buffer::U64(vec![7]));
    engine.cycle.publish(1);

    let peer = Peer::connect(&url).await;
    let ns = peer
        .session
        .get_namespace_index("urn:taktwerk")
        .await
        .unwrap();
    let node = |name: &str| NodeId::new(ns, name.to_owned());

    // Outputs and system signals follow the publish.
    let u = node("plant.u");
    eventually("published output", || async {
        (peer.read(&u, AttributeId::Value).await.value == Some(Variant::Double(1.5))).then_some(())
    })
    .await;
    let hb = peer
        .read(&node("taktwerk.heartbeat"), AttributeId::Value)
        .await;
    assert_eq!(hb.value, Some(Variant::UInt64(7)));

    // A column-major matrix reads row-major with its dimensions.
    let a = peer.read(&node("plant.A"), AttributeId::Value).await;
    let Some(Variant::Array(a)) = a.value else {
        panic!("matrix is not an array");
    };
    let row: Vec<f64> = a.values.iter().filter_map(Variant::as_f64).collect();
    assert_eq!(row, vec![0.0, 1.0, 2.0, 10.0, 11.0, 12.0]);
    assert_eq!(a.dimensions, Some(vec![2, 3]));
    let rank = peer.read(&node("plant.A"), AttributeId::ValueRank).await;
    assert_eq!(rank.value, Some(Variant::Int32(2)));
    let dt = peer.read(&node("plant.y"), AttributeId::DataType).await;
    assert_eq!(
        dt.value,
        Some(Variant::NodeId(Box::new(DataTypeId::Double.into())))
    );

    // The browse path follows the dots.
    let folder = peer
        .read(&NodeId::new(ns, "plant/"), AttributeId::BrowseName)
        .await;
    assert_eq!(
        folder.value,
        Some(Variant::QualifiedName(Box::new(QualifiedName::new(
            ns, "plant"
        ))))
    );

    // Inputs and tunables are writable and land in the image.
    let y = node("plant.y");
    assert_eq!(
        peer.write(&y, f64s(&[1.0, 2.0, 3.0])).await,
        StatusCode::Good
    );
    assert_eq!(engine.read("plant.y").0, Buffer::F64(vec![1.0, 2.0, 3.0]));
    assert!(engine.read("plant.y").1.is_some());
    assert_eq!(
        peer.write(&node("loop.kp"), Variant::Float(0.5)).await,
        StatusCode::Good
    );
    assert_eq!(engine.read("loop.kp").0, Buffer::F32(vec![0.5]));
    assert_eq!(
        peer.write(&node("plant.on"), Variant::Boolean(true)).await,
        StatusCode::Good
    );
    assert_eq!(engine.read("plant.on").0, Buffer::Bool(vec![true]));
    eventually("written input readable", || async {
        let v = peer.read(&y, AttributeId::Value).await.value;
        (v == Some(f64s(&[1.0, 2.0, 3.0]))).then_some(())
    })
    .await;

    // Bad writes are refused and leave the image alone.
    let ints = Variant::Array(Box::new(
        Array::new(VariantScalarTypeId::Int32, vec![Variant::Int32(1); 3]).unwrap(),
    ));
    assert_eq!(peer.write(&y, ints).await, StatusCode::BadTypeMismatch);
    assert_eq!(
        peer.write(&y, f64s(&[1.0, 2.0])).await,
        StatusCode::BadTypeMismatch
    );
    assert_eq!(
        peer.write(&y, Variant::Double(4.0)).await,
        StatusCode::BadTypeMismatch
    );
    assert_eq!(engine.read("plant.y").0, Buffer::F64(vec![1.0, 2.0, 3.0]));
    let refused = peer.write(&u, Variant::Double(9.0)).await;
    assert!(refused.is_bad(), "output write accepted: {refused}");

    // A subscription sees the next publish.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sub = peer
        .session
        .create_subscription(
            Duration::from_millis(50),
            100,
            10,
            0,
            0,
            true,
            DataChangeCallback::new(move |dv, _| {
                let _ = tx.send(dv);
            }),
        )
        .await
        .unwrap();
    let mut item: MonitoredItemCreateRequest = u.clone().into();
    item.requested_parameters.sampling_interval = 0.0;
    let created = peer
        .session
        .create_monitored_items(sub, TimestampsToReturn::Both, vec![item])
        .await
        .unwrap();
    assert!(created[0].result.status_code.is_good());
    engine.store("plant.u", Buffer::F64(vec![2.5]));
    engine.cycle.publish(2);
    let seen = tokio::time::timeout(WAIT, async {
        while let Some(dv) = rx.recv().await {
            if dv.value == Some(Variant::Double(2.5)) {
                return true;
            }
        }
        false
    })
    .await;
    assert_eq!(seen, Ok(true));

    peer.close().await;
    engine.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(pki);
}

#[tokio::test]
async fn server_refuses_bad_settings() {
    for text in [
        "id = \"ua\"\nkind = \"opcua-server\"\nendpoint = \"http://x:1\"\n",
        "id = \"ua\"\nkind = \"opcua-server\"\nsecurity = []\n",
        "id = \"ua\"\nkind = \"opcua-server\"\nanonymous = false\n",
        "id = \"ua\"\nkind = \"opcua-server\"\nbogus = 1\n",
        "id = \"ua\"\nkind = \"opcua-server\"\nusers = [{ user = \"op\" }]\n",
    ] {
        let Err(err) = taktwerk_opcua::build(&connector_config(text)) else {
            panic!("{text}: accepted");
        };
        assert!(matches!(err, ConnectorError::Config(_)), "{text}: {err}");
    }
}

// ---------------------------------------------------------------------------------------------
// Client connector
// ---------------------------------------------------------------------------------------------

const PLC_NS: &str = "urn:test-plc";

/// A plain OPC UA server standing in for a PLC.
struct Plc {
    handle: ServerHandle,
    manager: Arc<SimpleNodeManager>,
    ns: u16,
    task: JoinHandle<Result<(), String>>,
}

impl Plc {
    async fn start(port: u16, y: [f64; 3]) -> Self {
        let pki = scratch_dir("plc-pki");
        let (server, handle) = ServerBuilder::new()
            .application_name("test-plc")
            .application_uri("urn:test-plc-app")
            .product_uri("urn:test-plc-app")
            .host("127.0.0.1")
            .port(port)
            .pki_dir(&pki)
            .create_sample_keypair(false)
            .discovery_urls(vec![format!("opc.tcp://127.0.0.1:{port}")])
            .add_endpoint(
                "none",
                ServerEndpoint::new_none("/", &[ANONYMOUS_USER_TOKEN_ID.to_owned()]),
            )
            .with_node_manager(simple_node_manager(
                NamespaceMetadata {
                    namespace_uri: PLC_NS.to_owned(),
                    ..Default::default()
                },
                "plc",
            ))
            .build()
            .unwrap();
        let _ = std::fs::remove_dir_all(pki);
        let manager = handle
            .node_managers()
            .get_of_type::<SimpleNodeManager>()
            .unwrap();
        let ns = handle.get_namespace_index(PLC_NS).unwrap();
        {
            let mut space = manager.address_space().write();
            let mut add = |name: &str, ty: DataTypeId, value: Variant, writable: bool| {
                let id = NodeId::new(ns, name.to_owned());
                let mut b = VariableBuilder::new(&id, QualifiedName::new(ns, name), name)
                    .value(value.clone())
                    .data_type(ty)
                    .organized_by(NodeId::from(ObjectId::ObjectsFolder));
                b = match &value {
                    Variant::Array(a) => b
                        .value_rank(1)
                        .array_dimensions(&[u32::try_from(a.values.len()).unwrap()]),
                    _ => b.value_rank(-1),
                };
                b = if writable {
                    b.writable()
                } else {
                    b.access_level(AccessLevel::CURRENT_READ)
                        .user_access_level(AccessLevel::CURRENT_READ)
                };
                assert!(b.insert(&mut *space));
            };
            add("y", DataTypeId::Double, f64s(&y), true);
            add("sp", DataTypeId::Double, Variant::Double(4.0), true);
            add("u", DataTypeId::Double, Variant::Double(0.0), true);
            add("hb", DataTypeId::UInt64, Variant::UInt64(0), true);
            add("ro", DataTypeId::Double, Variant::Double(0.0), false);
            add("i", DataTypeId::Int32, Variant::Int32(0), true);
            add("y5", DataTypeId::Double, f64s(&[0.0; 5]), true);
        }
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        let task = tokio::spawn(server.run_with(listener));
        Self {
            handle,
            manager,
            ns,
            task,
        }
    }

    fn node(&self, name: &str) -> NodeId {
        NodeId::new(self.ns, name.to_owned())
    }

    fn set(&self, name: &str, value: Variant) {
        self.manager
            .set_value(
                self.handle.subscriptions(),
                &self.node(name),
                None,
                DataValue::value_only(value),
            )
            .unwrap();
    }

    async fn stop(self) {
        self.handle.cancel();
        let _ = tokio::time::timeout(WAIT, self.task).await;
    }
}

fn client_layout() -> ImageLayout {
    ImageLayout::new(vec![
        signal("plant.y", ScalarType::F64, &[3], Direction::Input),
        signal("plant.sp", ScalarType::F64, &[], Direction::Tunable),
        signal("plant.u", ScalarType::F64, &[], Direction::Output),
        signal(
            "taktwerk.heartbeat",
            ScalarType::U64,
            &[],
            Direction::System,
        ),
    ])
    .unwrap()
}

fn client_config(port: u16, map: &[(&str, &str)]) -> ConnectorConfig {
    let mut text = format!(
        "id = \"plc\"\nkind = \"opcua-client\"\nendpoint = \"opc.tcp://127.0.0.1:{port}\"\n\
         sync_period_ms = 20\nrequest_timeout_ms = 500\nreconnect_backoff_ms = 50\n[map]\n"
    );
    for (signal, node) in map {
        text.push_str(&format!("\"{signal}\" = \"{node}\"\n"));
    }
    connector_config(&text)
}

fn good_map(ns: u16) -> Vec<(&'static str, String)> {
    vec![
        ("plant.y", format!("nsu={PLC_NS};s=y")),
        ("plant.sp", format!("ns={ns};s=sp")),
        ("plant.u", format!("ns={ns};s=u")),
        ("taktwerk.heartbeat", format!("nsu={PLC_NS};s=hb")),
    ]
}

fn as_refs<'a>(map: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    map.iter().map(|(s, n)| (*s, n.as_str())).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_discovers_binds_and_syncs() {
    let port = free_port();
    let plc = Plc::start(port, [1.0, 2.0, 3.0]).await;
    let map = good_map(plc.ns);
    let mut client = taktwerk_opcua::build(&client_config(port, &as_refs(&map))).unwrap();

    assert_eq!(client.discover_len("plant.y").await.unwrap(), Some(3));
    assert_eq!(client.discover_len("plant.sp").await.unwrap(), Some(1));
    assert_eq!(client.discover_len("unmapped").await.unwrap(), None);

    let engine = start(client, client_layout()).await;

    // Inputs and tunables are read into the image.
    eventually("inputs read", || async {
        let (y, stamp) = engine.read("plant.y");
        (y == Buffer::F64(vec![1.0, 2.0, 3.0]) && stamp.is_some()).then_some(())
    })
    .await;
    assert_eq!(engine.read("plant.sp").0, Buffer::F64(vec![4.0]));
    plc.set("sp", Variant::Double(5.5));
    eventually("changed input read", || async {
        (engine.read("plant.sp").0 == Buffer::F64(vec![5.5])).then_some(())
    })
    .await;
    let fresh = engine.read("plant.y").1.unwrap();
    assert!(fresh.elapsed() < Duration::from_secs(1));

    // Outputs are written after a publish.
    engine.store("plant.u", Buffer::F64(vec![0.25]));
    engine.store("taktwerk.heartbeat", Buffer::U64(vec![42]));
    engine.cycle.publish(1);
    let peer = Peer::connect(&format!("opc.tcp://127.0.0.1:{port}")).await;
    eventually("outputs written", || async {
        let u = peer.read(&plc.node("u"), AttributeId::Value).await.value;
        let hb = peer.read(&plc.node("hb"), AttributeId::Value).await.value;
        (u == Some(Variant::Double(0.25)) && hb == Some(Variant::UInt64(42))).then_some(())
    })
    .await;
    peer.close().await;

    engine.stop().await.unwrap();
    plc.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_bind_lists_every_problem() {
    let port = free_port();
    let plc = Plc::start(port, [0.0; 3]).await;
    let ns = plc.ns;
    let layout = ImageLayout::new(vec![
        signal("plant.y", ScalarType::F64, &[3], Direction::Input),
        signal("a", ScalarType::F64, &[], Direction::Output),
        signal("b", ScalarType::F64, &[], Direction::Input),
        signal("c", ScalarType::F64, &[3], Direction::Input),
        signal("d", ScalarType::F64, &[], Direction::Input),
        signal("e", ScalarType::F64, &[], Direction::Input),
    ])
    .unwrap();
    let map = [
        ("plant.y", format!("ns={ns};s=y")),
        ("a", format!("ns={ns};s=ro")),
        ("b", format!("ns={ns};s=i")),
        ("c", format!("ns={ns};s=y5")),
        ("d", format!("ns={ns};s=missing")),
        ("e", "nsu=urn:nowhere;s=x".to_owned()),
        ("ghost", format!("ns={ns};s=sp")),
    ];
    let mut client = taktwerk_opcua::build(&client_config(port, &as_refs(&map))).unwrap();
    let err = client.bind(&layout).await.unwrap_err();
    let ConnectorError::Bind(text) = err else {
        panic!("not a bind refusal: {err}");
    };
    assert!(text.starts_with("6 mapped node(s)"), "{text}");
    for needle in [
        "a (ns=",
        "not writable",
        "b (ns=",
        "DataType is Int32, expected Double scalar",
        "c (ns=",
        "holds 5 element(s), expected Double [3]",
        "d (ns=",
        "does not exist",
        "e (nsu=urn:nowhere",
        "namespace `urn:nowhere`",
        "ghost",
        "no such signal",
    ] {
        assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
    }
    assert!(!text.contains("plant.y"), "{text}");
    plc.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_reconnects_after_server_restart() {
    let port = free_port();
    let plc = Plc::start(port, [1.0, 1.0, 1.0]).await;
    let map = good_map(plc.ns);
    let client = taktwerk_opcua::build(&client_config(port, &as_refs(&map))).unwrap();
    let engine = start(client, client_layout()).await;
    eventually("first read", || async {
        (engine.read("plant.y").0 == Buffer::F64(vec![1.0, 1.0, 1.0])).then_some(())
    })
    .await;

    plc.stop().await;
    let lost = Instant::now();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (_, stamp) = engine.read("plant.y");
    assert!(
        stamp.unwrap() < lost + Duration::from_millis(100),
        "input kept fresh while down"
    );

    let plc = Plc::start(port, [7.0, 8.0, 9.0]).await;
    eventually("read after restart", || async {
        (engine.read("plant.y").0 == Buffer::F64(vec![7.0, 8.0, 9.0])).then_some(())
    })
    .await;
    engine.store("plant.u", Buffer::F64(vec![3.0]));
    engine.cycle.publish(5);
    let peer = Peer::connect(&format!("opc.tcp://127.0.0.1:{port}")).await;
    eventually("write after restart", || async {
        let u = peer.read(&plc.node("u"), AttributeId::Value).await.value;
        (u == Some(Variant::Double(3.0))).then_some(())
    })
    .await;
    peer.close().await;
    engine.stop().await.unwrap();
    plc.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secure_server_generates_its_certificate() {
    let port = free_port();
    let pki = scratch_dir("secure-pki");
    let config = connector_config(&format!(
        "id = \"ua\"\nkind = \"opcua-server\"\nendpoint = \"opc.tcp://127.0.0.1:{port}\"\n\
         pki_dir = \"{}\"\nsecurity = [\"sign\", \"sign-encrypt\"]\nanonymous = false\n\
         users = [{{ user = \"op\", password = \"pw\" }}]\n",
        pki.display()
    ));
    let mut server = taktwerk_opcua::build(&config).unwrap();
    server.bind(&server_layout()).await.unwrap();
    assert!(pki.join("own/cert.der").exists());
    assert!(pki.join("private/private.pem").exists());
    drop(server);
    let _ = std::fs::remove_dir_all(pki);
}
