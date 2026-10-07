//! The OPC UA client connector (`kind = "opcua-client"`): syncs mapped signals with another
//! server, typically a PLC's.
//!
//! Image inputs and tunables are read from their nodes, one bulk Read per sync period; image
//! outputs and system signals are written to theirs, one bulk Write per published cycle. Every
//! session is verified fail-closed before it syncs, the first at `bind` and each reconnect's
//! again; while disconnected, inputs age and the engine turns them stale.
//!
//! ```toml
//! [[connector]]
//! id = "plc"
//! kind = "opcua-client"
//! endpoint = "opc.tcp://192.168.0.10:4840"
//! security = "none"                      # default; or "sign", "sign-encrypt"
//! credentials = { user = "op", password_env = "PLC_PASSWORD" }   # default anonymous
//! request_timeout_ms = 1000              # default
//! reconnect_backoff_ms = 1000            # default
//! sync_period_ms = 100                   # default
//! stamp = "receive"                      # default; or "source"
//! [connector.map]
//! "plant.y" = "ns=4;s=Plant.y"
//! "plant.u" = "nsu=urn:example:plc;s=Plant.u"
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use opcua::client::{Client, ClientBuilder, IdentityToken, Session};
use opcua::nodes::AccessLevel;
use opcua::types::{
    AttributeId, DataValue, NodeId, ReadValueId, StatusCode, TimestampsToReturn, VariableId,
    Variant, WriteValue,
};
use serde::Deserialize;
use taktwerk_core::connector::{BoxFuture, Connector, ConnectorError, Shutdown};
use taktwerk_core::image::{Direction, ImageHandle, ImageLayout, SignalId, SignalSpec};
use taktwerk_core::project::ConnectorConfig;
use taktwerk_core::value::Buffer;
use tokio::task::JoinHandle;

use crate::convert::{self, Shape};
use crate::settings::{self, Credentials, Security};

/// The project `kind` of the client connector.
pub const KIND: &str = "opcua-client";

const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
const BACKOFF_FACTOR_MAX: u32 = 16;

/// Which time an input read is stamped with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stamp {
    /// When the response arrived.
    Receive,
    /// The server's source timestamp, or the receive time when it sends none. Servers that
    /// stamp a value only when it changes make a constant input look old.
    Source,
}

/// Kind-specific keys of an `opcua-client` connector.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientSettings {
    /// Server URL, `opc.tcp://host:port[/path]`.
    pub endpoint: String,
    /// Message security of the session.
    #[serde(default = "security_none")]
    pub security: Security,
    /// User name and password; anonymous when absent.
    #[serde(default)]
    pub credentials: Option<Credentials>,
    /// Bound on every request, milliseconds.
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    /// First wait before a reconnect, milliseconds; doubles per failure up to 16 times this.
    #[serde(default = "default_reconnect_backoff_ms")]
    pub reconnect_backoff_ms: u64,
    /// Period of the input Read, milliseconds.
    #[serde(default = "default_sync_period_ms")]
    pub sync_period_ms: u64,
    /// Which time stamps an input.
    #[serde(default = "stamp_receive")]
    pub stamp: Stamp,
    /// Directory of the client certificate and trusted server certificates; used only with
    /// message security.
    #[serde(default = "default_pki_dir")]
    pub pki_dir: PathBuf,
    /// Trust every server certificate instead of only those in `<pki_dir>/trusted`.
    #[serde(default)]
    pub trust_server_certs: bool,
    /// Signal name → NodeId (`ns=<i>;s=<id>`, `nsu=<uri>;s=<id>`, or `i=`/`g=`/`b=` forms).
    #[serde(default)]
    pub map: BTreeMap<String, String>,
}

fn security_none() -> Security {
    Security::None
}

fn default_request_timeout_ms() -> u64 {
    1000
}

fn default_reconnect_backoff_ms() -> u64 {
    1000
}

fn default_sync_period_ms() -> u64 {
    100
}

fn stamp_receive() -> Stamp {
    Stamp::Receive
}

fn default_pki_dir() -> PathBuf {
    PathBuf::from("./pki-client")
}

/// A mapped node as configured.
#[derive(Debug, Clone)]
enum NodeRef {
    /// A NodeId with a namespace index.
    Index(NodeId),
    /// A NodeId whose namespace is named by URI, resolved per session.
    Uri(String, NodeId),
}

impl NodeRef {
    fn parse(text: &str) -> Result<Self, String> {
        if let Some(rest) = text.strip_prefix("nsu=") {
            let (uri, id) = rest
                .split_once(';')
                .ok_or_else(|| format!("`{text}`: expected nsu=<uri>;<id>"))?;
            let id = NodeId::from_str(id).map_err(|_| format!("`{text}`: invalid NodeId"))?;
            return Ok(Self::Uri(uri.to_owned(), id));
        }
        NodeId::from_str(text)
            .map(Self::Index)
            .map_err(|_| format!("`{text}`: invalid NodeId"))
    }

    fn resolve(&self, namespaces: &[String]) -> Result<NodeId, String> {
        match self {
            Self::Index(id) => Ok(id.clone()),
            Self::Uri(uri, id) => {
                let index = namespaces
                    .iter()
                    .position(|n| n == uri)
                    .and_then(|i| u16::try_from(i).ok())
                    .ok_or_else(|| format!("namespace `{uri}` is not on the server"))?;
                let mut id = id.clone();
                id.namespace = index;
                Ok(id)
            }
        }
    }
}

/// One mapped signal on a verified session.
#[derive(Debug)]
struct Bound {
    id: SignalId,
    node: NodeId,
    label: String,
    shape: Shape,
    scratch: Buffer,
    /// Last non-good outcome logged, so a persistent fault logs once.
    reported: Option<StatusCode>,
}

impl Bound {
    fn report(&mut self, status: StatusCode, what: &str) {
        if self.reported != Some(status) {
            tracing::warn!(node = %self.label, %status, "{what}");
            self.reported = Some(status);
        }
    }
}

/// The read and write sets of a verified session.
#[derive(Debug, Default)]
struct Plan {
    reads: Vec<Bound>,
    read_ids: Vec<ReadValueId>,
    writes: Vec<Bound>,
}

/// An open session.
struct Conn {
    _client: Client,
    session: Arc<Session>,
    events: JoinHandle<StatusCode>,
}

impl Conn {
    async fn close(self) {
        self.session.disable_reconnects();
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, self.session.disconnect()).await;
        self.events.abort();
    }
}

/// The OPC UA client connector.
pub struct OpcUaClient {
    id: String,
    settings: ClientSettings,
    map: Vec<(String, NodeRef)>,
    conn: Option<Conn>,
    plan: Option<Plan>,
}

impl std::fmt::Debug for OpcUaClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpcUaClient")
            .field("id", &self.id)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl OpcUaClient {
    /// Build from a `[[connector]]` table of kind `opcua-client`.
    ///
    /// # Errors
    /// [`ConnectorError::Config`] on an unknown key, an invalid value or NodeId.
    pub fn from_config(config: &ConnectorConfig) -> Result<Self, ConnectorError> {
        let settings: ClientSettings = settings::parse(config)?;
        settings::host_port(&settings.endpoint)?;
        if let Some(c) = &settings.credentials {
            c.resolve_password()?;
        }
        if settings.sync_period_ms == 0 || settings.request_timeout_ms == 0 {
            return Err(ConnectorError::Config(format!(
                "connector `{}`: `sync_period_ms` and `request_timeout_ms` must be positive",
                config.id
            )));
        }
        let mut map = Vec::with_capacity(settings.map.len());
        let mut bad = Vec::new();
        for (signal, text) in &settings.map {
            match NodeRef::parse(text) {
                Ok(node) => map.push((signal.clone(), node)),
                Err(e) => bad.push(format!("{signal}: {e}")),
            }
        }
        if !bad.is_empty() {
            return Err(ConnectorError::Config(format!(
                "connector `{}`: {}",
                config.id,
                bad.join("; ")
            )));
        }
        Ok(Self {
            id: config.id.clone(),
            settings,
            map,
            conn: None,
            plan: None,
        })
    }

    fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.settings.request_timeout_ms)
    }

    fn label(&self, signal: &str) -> String {
        let node = self.settings.map.get(signal).map_or("?", String::as_str);
        format!("{signal} ({node})")
    }

    fn build_client(&self) -> Result<Client, ConnectorError> {
        let secure = self.settings.security != Security::None;
        let pki_dir = if secure {
            self.settings.pki_dir.clone()
        } else {
            // A None channel reads no certificate; keep the library's directory scaffold out
            // of the working directory.
            std::env::temp_dir().join(format!("taktwerk-opcua-{}-{}", std::process::id(), self.id))
        };
        let client = ClientBuilder::new()
            .application_name("taktwerk")
            .application_uri(format!("urn:taktwerk:client:{}", self.id))
            .product_uri("urn:taktwerk")
            .pki_dir(&pki_dir)
            .create_sample_keypair(secure)
            .trust_server_certs(self.settings.trust_server_certs)
            .session_retry_limit(0)
            .request_timeout(self.request_timeout())
            .max_array_length(1 << 20)
            .max_message_size(64 << 20)
            .max_chunk_count(1024)
            .client()
            .map_err(|errs| ConnectorError::Config(errs.join("; ")));
        if !secure {
            let _ = std::fs::remove_dir_all(&pki_dir);
        }
        client
    }

    async fn connect(&self) -> Result<Conn, ConnectorError> {
        let mut client = self.build_client()?;
        let identity = match &self.settings.credentials {
            Some(c) => IdentityToken::new_user_name(c.user.clone(), c.resolve_password()?),
            None => IdentityToken::Anonymous,
        };
        let endpoint = (
            self.settings.endpoint.as_str(),
            self.settings.security.policy().to_str(),
            self.settings.security.mode(),
        );
        let io = |e: &dyn std::fmt::Display| {
            ConnectorError::Io(format!("connect {}: {e}", self.settings.endpoint))
        };
        let (session, event_loop) = tokio::time::timeout(
            self.request_timeout() * 2,
            client.connect_to_matching_endpoint(endpoint, identity),
        )
        .await
        .map_err(|_| io(&"timed out"))?
        .map_err(|e| io(&e))?;
        session.disable_reconnects();
        let mut events = event_loop.spawn();
        let connected = tokio::select! {
            ended = &mut events => Err(io(&format!("session ended: {ended:?}"))),
            ok = tokio::time::timeout(self.request_timeout() * 2, session.wait_for_connection()) => {
                match ok {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(io(&"session did not connect")),
                    Err(_) => Err(io(&"timed out")),
                }
            }
        };
        let conn = Conn {
            _client: client,
            session,
            events,
        };
        match connected {
            Ok(()) => {
                tracing::info!(connector = %self.id, endpoint = %self.settings.endpoint, "OPC UA session up");
                Ok(conn)
            }
            Err(e) => {
                conn.close().await;
                Err(e)
            }
        }
    }

    async fn ensure_conn(&mut self) -> Result<&Conn, ConnectorError> {
        if self.conn.as_ref().is_some_and(|c| c.events.is_finished()) {
            if let Some(old) = self.conn.take() {
                old.close().await;
            }
        }
        if self.conn.is_none() {
            self.conn = Some(self.connect().await?);
        }
        self.conn
            .as_ref()
            .ok_or_else(|| ConnectorError::Io("no session".to_owned()))
    }

    /// Verify every mapping against the server; the plan on success, else every problem.
    async fn verify(
        &self,
        session: &Session,
        layout: &ImageLayout,
    ) -> Result<Plan, ConnectorError> {
        let mut problems = Vec::new();
        let namespaces = if self.map.iter().any(|(_, n)| matches!(n, NodeRef::Uri(..))) {
            read_namespaces(session).await?
        } else {
            Vec::new()
        };
        let mut targets: Vec<(&SignalSpec, SignalId, NodeId, String)> = Vec::new();
        for (signal, node) in &self.map {
            let label = self.label(signal);
            let Some(id) = layout.id(signal) else {
                problems.push(format!("{label}: no such signal in the image"));
                continue;
            };
            let Some(spec) = layout.spec(id) else {
                problems.push(format!("{label}: no such signal in the image"));
                continue;
            };
            match node.resolve(&namespaces) {
                Ok(node) => targets.push((spec, id, node, label)),
                Err(e) => problems.push(format!("{label}: {e}")),
            }
        }

        const ATTRS: [AttributeId; 5] = [
            AttributeId::DataType,
            AttributeId::ValueRank,
            AttributeId::ArrayDimensions,
            AttributeId::AccessLevel,
            AttributeId::Value,
        ];
        let ids: Vec<ReadValueId> = targets
            .iter()
            .flat_map(|(_, _, node, _)| ATTRS.map(|a| ReadValueId::new(node.clone(), a)))
            .collect();
        let results = if ids.is_empty() {
            Vec::new()
        } else {
            read_exact(session, &ids, "verification").await?
        };

        let mut plan = Plan::default();
        for ((spec, id, node, label), attrs) in targets.into_iter().zip(results.chunks_exact(5)) {
            let [dt, rank, dims, access, value] = attrs else {
                continue;
            };
            match check_one(&label, spec, dt, rank, dims, access, value) {
                Err(problem) => problems.push(problem),
                Ok(scalar) => {
                    let bound = Bound {
                        id,
                        node: node.clone(),
                        label,
                        shape: Shape::flat(scalar),
                        scratch: Buffer::zeroed(spec.ty, spec.len()),
                        reported: None,
                    };
                    if is_read(spec.direction) {
                        plan.read_ids.push(ReadValueId::new_value(node));
                        plan.reads.push(bound);
                    } else {
                        plan.writes.push(bound);
                    }
                }
            }
        }
        if !problems.is_empty() {
            return Err(refusal(&self.settings.endpoint, &problems));
        }
        tracing::info!(
            connector = %self.id,
            reads = plan.reads.len(),
            writes = plan.writes.len(),
            "all mapped nodes verified"
        );
        Ok(plan)
    }
}

fn is_read(direction: Direction) -> bool {
    matches!(direction, Direction::Input | Direction::Tunable)
}

/// The refusal listing every problem.
fn refusal(endpoint: &str, problems: &[String]) -> ConnectorError {
    let mut text = format!(
        "{} mapped node(s) on {endpoint} failed verification:",
        problems.len()
    );
    for p in problems {
        text.push_str("\n  - ");
        text.push_str(p);
    }
    ConnectorError::Bind(text)
}

async fn read_exact(
    session: &Session,
    ids: &[ReadValueId],
    what: &str,
) -> Result<Vec<DataValue>, ConnectorError> {
    let results = session
        .read(ids, TimestampsToReturn::Neither, 0.0)
        .await
        .map_err(|e| ConnectorError::Io(format!("{what} read: {e}")))?;
    if results.len() != ids.len() {
        return Err(ConnectorError::Io(format!(
            "{what} read: {} results for {} items",
            results.len(),
            ids.len()
        )));
    }
    Ok(results)
}

async fn read_namespaces(session: &Session) -> Result<Vec<String>, ConnectorError> {
    let id = ReadValueId::new_value(VariableId::Server_NamespaceArray.into());
    let results = read_exact(session, &[id], "NamespaceArray").await?;
    let value = results.first().and_then(|dv| good(dv).ok());
    match value {
        Some(Variant::Array(a)) => Ok(a
            .values
            .iter()
            .filter_map(|v| match v {
                Variant::String(s) => Some(s.as_ref().to_owned()),
                _ => None,
            })
            .collect()),
        _ => Err(ConnectorError::Io("NamespaceArray unreadable".to_owned())),
    }
}

/// The value of a good `dv`, else why not.
fn good(dv: &DataValue) -> Result<&Variant, String> {
    let status = dv.status();
    if !status.is_good() {
        return Err(status.to_string());
    }
    dv.value.as_ref().ok_or_else(|| "no value".to_owned())
}

/// The length a node declares: its `ArrayDimensions` when known, else its value's.
fn node_len(dims: &DataValue, value: &DataValue) -> Option<usize> {
    if let Ok(Variant::Array(a)) = good(dims) {
        let axes: Vec<usize> = a
            .values
            .iter()
            .filter_map(|v| match v {
                Variant::UInt32(n) => usize::try_from(*n).ok(),
                _ => None,
            })
            .collect();
        if !axes.is_empty() && axes.len() == a.values.len() && axes.iter().all(|n| *n > 0) {
            return Some(axes.iter().product());
        }
    }
    good(value).ok().and_then(convert::element_count)
}

/// Check one node against its signal; `Ok(true)` when the node is a scalar.
fn check_one(
    label: &str,
    spec: &SignalSpec,
    dt: &DataValue,
    rank: &DataValue,
    dims: &DataValue,
    access: &DataValue,
    value: &DataValue,
) -> Result<bool, String> {
    let expected = convert::data_type_node(spec.ty);
    let want = format!(
        "{} {}",
        convert::type_label(spec.ty),
        if spec.shape.is_empty() {
            "scalar".to_owned()
        } else {
            format!("[{}]", spec.len())
        }
    );
    let found = match good(dt) {
        Ok(Variant::NodeId(id)) => id.as_ref().clone(),
        Ok(other) => return Err(format!("{label}: DataType is {other:?}, expected {want}")),
        Err(e) => {
            return Err(format!(
                "{label}: does not exist or is unreadable ({e}); expected {want}"
            ));
        }
    };
    if found != expected {
        return Err(format!(
            "{label}: DataType is {}, expected {want}",
            convert::data_type_label(&found)
        ));
    }

    let rank = match good(rank) {
        Ok(Variant::Int32(r)) => *r,
        Ok(other) => return Err(format!("{label}: ValueRank is {other:?}, expected Int32")),
        Err(e) => return Err(format!("{label}: ValueRank unreadable ({e})")),
    };
    let scalar = match rank {
        -1 => true,
        r if r >= 0 => false,
        // Any or ScalarOrOneDimension: the current value decides.
        _ => !matches!(good(value), Ok(Variant::Array(_))),
    };
    if scalar {
        if spec.len() != 1 {
            return Err(format!("{label}: node is a scalar, expected {want}"));
        }
    } else if spec.shape.is_empty() {
        return Err(format!(
            "{label}: node is an array (ValueRank {rank}), expected {want}"
        ));
    } else {
        match node_len(dims, value) {
            Some(n) if n == spec.len() => {}
            Some(n) => {
                return Err(format!(
                    "{label}: node holds {n} element(s), expected {want}"
                ));
            }
            None => {
                return Err(format!(
                    "{label}: array length unreadable (no ArrayDimensions, no value); expected {want}"
                ));
            }
        }
    }

    let level = match good(access) {
        Ok(Variant::Byte(b)) => AccessLevel::from_bits_truncate(*b),
        Ok(other) => return Err(format!("{label}: AccessLevel is {other:?}, expected Byte")),
        Err(e) => return Err(format!("{label}: AccessLevel unreadable ({e})")),
    };
    if !level.contains(AccessLevel::CURRENT_READ) {
        return Err(format!(
            "{label}: not readable (AccessLevel {})",
            level.bits()
        ));
    }
    if !is_read(spec.direction) && !level.contains(AccessLevel::CURRENT_WRITE) {
        return Err(format!(
            "{label}: not writable (AccessLevel {}), but the engine writes this signal",
            level.bits()
        ));
    }
    Ok(scalar)
}

/// One bulk Read of the inputs into the image.
async fn read_inputs(
    session: &Session,
    plan: &mut Plan,
    image: &ImageHandle,
    stamp: Stamp,
) -> Result<(), ConnectorError> {
    let results = session
        .read(&plan.read_ids, TimestampsToReturn::Source, 0.0)
        .await
        .map_err(|e| ConnectorError::Io(format!("input read: {e}")))?;
    if results.len() != plan.reads.len() {
        return Err(ConnectorError::Io(format!(
            "input read: {} results for {} items",
            results.len(),
            plan.reads.len()
        )));
    }
    let received = Instant::now();
    for (bound, dv) in plan.reads.iter_mut().zip(&results) {
        let status = dv.status();
        if !status.is_good() {
            bound.report(status, "input read refused; the signal ages");
            continue;
        }
        let Some(value) = dv.value.as_ref() else {
            bound.report(StatusCode::BadNoData, "input read without value");
            continue;
        };
        if let Err(status) = convert::from_variant(value, &mut bound.scratch, &bound.shape) {
            bound.report(status, "input value does not match the signal");
            continue;
        }
        let at = match (stamp, &dv.source_timestamp) {
            (Stamp::Source, Some(ts)) if !ts.is_null() => convert::monotonic(ts),
            _ => received,
        };
        if let Err(e) = image.write_stamped(bound.id, &bound.scratch, at) {
            tracing::warn!(node = %bound.label, error = %e, "image write failed");
            continue;
        }
        bound.reported = None;
    }
    Ok(())
}

/// One bulk Write of the outputs from the image.
async fn write_outputs(
    session: &Session,
    plan: &mut Plan,
    image: &ImageHandle,
) -> Result<(), ConnectorError> {
    let mut values = Vec::with_capacity(plan.writes.len());
    for bound in &mut plan.writes {
        if image.read(bound.id, &mut bound.scratch).is_err() {
            continue;
        }
        values.push(WriteValue::value_attr(
            bound.node.clone(),
            convert::to_variant(&bound.scratch, &bound.shape),
        ));
    }
    if values.is_empty() {
        return Ok(());
    }
    let results = session
        .write(&values)
        .await
        .map_err(|e| ConnectorError::Io(format!("output write: {e}")))?;
    for (bound, status) in plan.writes.iter_mut().zip(results) {
        if status.is_good() {
            bound.reported = None;
        } else {
            bound.report(status, "output write refused");
        }
    }
    Ok(())
}

/// Why a sync stopped.
enum Stop {
    Shutdown,
    Lost(ConnectorError),
}

async fn sync(
    conn: &mut Conn,
    plan: &mut Plan,
    image: &ImageHandle,
    shutdown: &mut Shutdown,
    settings: &ClientSettings,
) -> Stop {
    let mut tick = tokio::time::interval(Duration::from_millis(settings.sync_period_ms));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut published = image.published();
    let reads = !plan.reads.is_empty();
    let writes = !plan.writes.is_empty();
    loop {
        if *shutdown.borrow() {
            return Stop::Shutdown;
        }
        let step = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() {
                    return Stop::Shutdown;
                }
                Ok(())
            }
            _ = tick.tick(), if reads => {
                read_inputs(&conn.session, plan, image, settings.stamp).await
            }
            changed = published.changed(), if writes => {
                if changed.is_err() {
                    return Stop::Shutdown;
                }
                write_outputs(&conn.session, plan, image).await
            }
            ended = &mut conn.events => {
                Err(ConnectorError::Io(format!("session ended: {ended:?}")))
            }
        };
        if let Err(e) = step {
            return Stop::Lost(e);
        }
    }
}

/// Sleep `wait` unless shutdown comes first; `true` on shutdown.
async fn pause(wait: Duration, shutdown: &mut Shutdown) -> bool {
    if *shutdown.borrow() {
        return true;
    }
    tokio::select! {
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
        () = tokio::time::sleep(wait) => false,
    }
}

impl Connector for OpcUaClient {
    fn id(&self) -> &str {
        &self.id
    }

    fn discover_len<'a>(
        &'a mut self,
        signal: &'a str,
    ) -> BoxFuture<'a, Result<Option<usize>, ConnectorError>> {
        Box::pin(async move {
            let Some((_, node)) = self.map.iter().find(|(s, _)| s == signal) else {
                return Ok(None);
            };
            let node = node.clone();
            let label = self.label(signal);
            let session = Arc::clone(&self.ensure_conn().await?.session);
            let namespaces = if matches!(node, NodeRef::Uri(..)) {
                read_namespaces(&session).await?
            } else {
                Vec::new()
            };
            let node = node
                .resolve(&namespaces)
                .map_err(|e| ConnectorError::Bind(format!("{label}: {e}")))?;
            let ids = [
                ReadValueId::new(node.clone(), AttributeId::ArrayDimensions),
                ReadValueId::new_value(node),
            ];
            let results = read_exact(&session, &ids, "length").await?;
            let [dims, value] = results.as_slice() else {
                return Err(ConnectorError::Io(
                    "length read: wrong result count".to_owned(),
                ));
            };
            node_len(dims, value).map(Some).ok_or_else(|| {
                ConnectorError::Bind(format!(
                    "{label}: length unreadable ({})",
                    good(value)
                        .err()
                        .unwrap_or_else(|| "empty value".to_owned())
                ))
            })
        })
    }

    fn bind<'a>(
        &'a mut self,
        layout: &'a ImageLayout,
    ) -> BoxFuture<'a, Result<(), ConnectorError>> {
        Box::pin(async move {
            let session = Arc::clone(&self.ensure_conn().await?.session);
            let plan = self.verify(&session, layout).await?;
            self.plan = Some(plan);
            Ok(())
        })
    }

    fn run(
        self: Box<Self>,
        image: ImageHandle,
        mut shutdown: Shutdown,
    ) -> BoxFuture<'static, Result<(), ConnectorError>> {
        Box::pin(async move {
            let mut this = *self;
            let base = Duration::from_millis(this.settings.reconnect_backoff_ms);
            let mut backoff = base;
            let layout = image.layout().clone();
            loop {
                if *shutdown.borrow() {
                    break;
                }
                let conn = match this.conn.take() {
                    Some(c) if !c.events.is_finished() => c,
                    stale => {
                        if let Some(c) = stale {
                            c.close().await;
                        }
                        this.plan = None;
                        let attempt = tokio::select! {
                            r = this.connect() => Some(r),
                            _ = shutdown.changed() => None,
                        };
                        match attempt {
                            None => continue,
                            Some(Ok(c)) => c,
                            Some(Err(e)) => {
                                tracing::warn!(connector = %this.id, error = %e, "connect failed");
                                if pause(backoff, &mut shutdown).await {
                                    break;
                                }
                                backoff = (backoff * 2).min(base * BACKOFF_FACTOR_MAX);
                                continue;
                            }
                        }
                    }
                };
                let mut conn = conn;
                let mut plan = match this.plan.take() {
                    Some(p) => p,
                    None => match this.verify(&conn.session, &layout).await {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(connector = %this.id, error = %e, "session refused");
                            conn.close().await;
                            if pause(backoff, &mut shutdown).await {
                                break;
                            }
                            backoff = (backoff * 2).min(base * BACKOFF_FACTOR_MAX);
                            continue;
                        }
                    },
                };
                backoff = base;
                match sync(&mut conn, &mut plan, &image, &mut shutdown, &this.settings).await {
                    Stop::Shutdown => {
                        conn.close().await;
                        return Ok(());
                    }
                    Stop::Lost(e) => {
                        tracing::warn!(connector = %this.id, error = %e, "session lost");
                        conn.close().await;
                        if pause(backoff, &mut shutdown).await {
                            break;
                        }
                    }
                }
            }
            if let Some(c) = this.conn.take() {
                c.close().await;
            }
            Ok(())
        })
    }
}
