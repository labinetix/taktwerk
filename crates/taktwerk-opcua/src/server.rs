//! The engine's own OPC UA server (`kind = "opcua-server"`).
//!
//! Every image signal is a variable node: NodeId `ns=<namespace>;s=<signal name>`, browse path
//! the name split on `.` into folders under `Objects`. Every node follows each publish; outputs
//! and system signals are read-only, inputs and tunables are writable and a write goes to the
//! image.
//!
//! ```toml
//! [[connector]]
//! id = "ua"
//! kind = "opcua-server"
//! endpoint = "opc.tcp://0.0.0.0:4840"    # default
//! namespace = "urn:taktwerk"             # default
//! security = ["none"]                    # default; also "sign", "sign-encrypt" (Basic256Sha256)
//! anonymous = true                       # default
//! users = [{ user = "op", password_env = "UA_PASSWORD" }]
//! pki_dir = "./pki"                      # default
//! trust_client_certs = false             # default
//! ```

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use opcua::nodes::{AccessLevel, ObjectBuilder, VariableBuilder};
use opcua::server::diagnostics::NamespaceMetadata;
use opcua::server::node_manager::memory::{SimpleNodeManager, simple_node_manager};
use opcua::server::{
    ANONYMOUS_USER_TOKEN_ID, Server, ServerBuilder, ServerEndpoint, ServerHandle, ServerUserToken,
};
use opcua::types::{
    DataValue, DateTime, NodeId, NumericRange, ObjectId, QualifiedName, StatusCode, Variant,
};
use serde::Deserialize;
use taktwerk_core::connector::{BoxFuture, Connector, ConnectorError, Shutdown};
use taktwerk_core::image::{Direction, ImageError, ImageHandle, ImageLayout, SignalId};
use taktwerk_core::project::ConnectorConfig;
use taktwerk_core::value::Buffer;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::convert::{self, Shape};
use crate::settings::{self, Credentials, Security};

/// The project `kind` of the server connector.
pub const KIND: &str = "opcua-server";

const USER_TOKEN_PREFIX: &str = "user:";
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// Kind-specific keys of an `opcua-server` connector.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSettings {
    /// Listen URL, `opc.tcp://host:port`.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// Namespace URI of the signal nodes.
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// Offered message security; one endpoint each.
    #[serde(default = "default_security")]
    pub security: Vec<Security>,
    /// Accept anonymous sessions.
    #[serde(default = "yes")]
    pub anonymous: bool,
    /// Accepted user names and passwords.
    #[serde(default)]
    pub users: Vec<Credentials>,
    /// Directory of the server certificate, its key and the trusted and rejected client
    /// certificates; the certificate is generated there when a secure endpoint needs one.
    #[serde(default = "default_pki_dir")]
    pub pki_dir: PathBuf,
    /// Trust every client certificate instead of only those in `<pki_dir>/trusted`.
    #[serde(default)]
    pub trust_client_certs: bool,
}

fn default_endpoint() -> String {
    "opc.tcp://0.0.0.0:4840".to_owned()
}

fn default_namespace() -> String {
    "urn:taktwerk".to_owned()
}

fn default_security() -> Vec<Security> {
    vec![Security::None]
}

fn default_pki_dir() -> PathBuf {
    PathBuf::from("./pki")
}

fn yes() -> bool {
    true
}

/// One signal as exposed on the server.
#[derive(Debug, Clone)]
struct Exposed {
    id: SignalId,
    node: NodeId,
    shape: Shape,
    scratch: Buffer,
}

/// What `bind` prepared for `run`.
struct Prepared {
    server: Server,
    handle: ServerHandle,
    manager: Arc<SimpleNodeManager>,
    listener: TcpListener,
    readable: Vec<Exposed>,
    writable: Vec<Exposed>,
}

/// The OPC UA server connector.
pub struct OpcUaServer {
    id: String,
    settings: ServerSettings,
    prepared: Option<Prepared>,
}

impl std::fmt::Debug for OpcUaServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpcUaServer")
            .field("id", &self.id)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl OpcUaServer {
    /// Build from a `[[connector]]` table of kind `opcua-server`.
    ///
    /// # Errors
    /// [`ConnectorError::Config`] on an unknown key or an invalid value.
    pub fn from_config(config: &ConnectorConfig) -> Result<Self, ConnectorError> {
        let settings: ServerSettings = settings::parse(config)?;
        settings::host_port(&settings.endpoint)?;
        if settings.security.is_empty() {
            return Err(ConnectorError::Config(format!(
                "connector `{}`: `security` lists no endpoint",
                config.id
            )));
        }
        if !settings.anonymous && settings.users.is_empty() {
            return Err(ConnectorError::Config(format!(
                "connector `{}`: no identity accepted; set `anonymous = true` or add `users`",
                config.id
            )));
        }
        for user in &settings.users {
            user.resolve_password()?;
        }
        Ok(Self {
            id: config.id.clone(),
            settings,
            prepared: None,
        })
    }

    /// The server's endpoint URL.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.settings.endpoint
    }

    fn builder(&self) -> Result<ServerBuilder, ConnectorError> {
        let s = &self.settings;
        let (host, port) = settings::host_port(&s.endpoint)?;
        let secure = s.security.iter().any(|m| *m != Security::None);
        let mut token_ids = Vec::new();
        if s.anonymous {
            token_ids.push(ANONYMOUS_USER_TOKEN_ID.to_owned());
        }
        let mut builder = ServerBuilder::new()
            .application_name("taktwerk")
            .application_uri(format!("urn:taktwerk:{}", self.id))
            .product_uri("urn:taktwerk")
            .host(host)
            .port(port)
            .pki_dir(&s.pki_dir)
            .create_sample_keypair(secure)
            .trust_client_certs(s.trust_client_certs)
            .discovery_urls(vec![s.endpoint.clone()])
            .max_array_length(1 << 20)
            .max_message_size(64 << 20)
            .max_chunk_count(1024)
            .diagnostics_enabled(false);
        for user in &s.users {
            let key = format!("{USER_TOKEN_PREFIX}{}", user.user);
            builder = builder.add_user_token(
                key.clone(),
                ServerUserToken::user_pass(user.user.clone(), user.resolve_password()?),
            );
            token_ids.push(key);
        }
        for mode in &s.security {
            let id = match mode {
                Security::None => "none",
                Security::Sign => "sign",
                Security::SignEncrypt => "sign-encrypt",
            };
            builder = builder.add_endpoint(
                id,
                ServerEndpoint::new("/", mode.policy(), mode.mode(), &token_ids),
            );
        }
        Ok(builder)
    }

    async fn prepare(&self, layout: &ImageLayout) -> Result<Prepared, ConnectorError> {
        let namespace = NamespaceMetadata {
            namespace_uri: self.settings.namespace.clone(),
            ..Default::default()
        };
        let (server, handle) = self
            .builder()?
            .with_node_manager(simple_node_manager(namespace, "taktwerk"))
            .build()
            .map_err(|e| ConnectorError::Config(format!("connector `{}`: {e}", self.id)))?;
        let manager = handle
            .node_managers()
            .get_of_type::<SimpleNodeManager>()
            .ok_or_else(|| ConnectorError::Io("server has no signal node manager".to_owned()))?;
        let ns = handle
            .get_namespace_index(&self.settings.namespace)
            .ok_or_else(|| ConnectorError::Io("signal namespace not registered".to_owned()))?;
        let (readable, writable) = populate(&manager, ns, layout)?;

        let (host, port) = settings::host_port(&self.settings.endpoint)?;
        let listener = TcpListener::bind((host.as_str(), port))
            .await
            .map_err(|e| {
                ConnectorError::Io(format!("listen on {}: {e}", self.settings.endpoint))
            })?;
        Ok(Prepared {
            server,
            handle,
            manager,
            listener,
            readable,
            writable,
        })
    }
}

/// Add a folder per name prefix and a variable per signal.
fn populate(
    manager: &SimpleNodeManager,
    ns: u16,
    layout: &ImageLayout,
) -> Result<(Vec<Exposed>, Vec<Exposed>), ConnectorError> {
    let mut space = manager.address_space().write();
    let mut folders = BTreeSet::new();
    let mut readable = Vec::new();
    let mut writable = Vec::new();
    for (id, spec) in layout.iter() {
        let parts: Vec<&str> = spec.name.split('.').collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(ConnectorError::Config(format!(
                "signal `{}`: empty name segment",
                spec.name
            )));
        }
        let mut parent: NodeId = ObjectId::ObjectsFolder.into();
        let (leaf, dirs) = parts.split_last().unwrap_or((&"", &[]));
        for depth in 1..=dirs.len() {
            let path = parts.get(..depth).unwrap_or_default().join(".");
            let folder = NodeId::new(ns, format!("{path}/"));
            if folders.insert(path) {
                let name = dirs.get(depth - 1).copied().unwrap_or_default();
                let builder = ObjectBuilder::new(&folder, QualifiedName::new(ns, name), name)
                    .is_folder()
                    .organized_by(parent.clone());
                if builder.is_valid() {
                    builder.insert(&mut *space);
                }
            }
            parent = folder;
        }

        let node = NodeId::new(ns, spec.name.clone());
        let shape = Shape::of_signal(&spec.shape, spec.layout);
        let scratch = Buffer::zeroed(spec.ty, spec.len());
        let mut builder = VariableBuilder::new(&node, QualifiedName::new(ns, *leaf), *leaf)
            .value(convert::to_variant(&scratch, &shape))
            .data_type(convert::data_type_node(spec.ty))
            .organized_by(parent);
        builder = if shape.scalar {
            builder.value_rank(-1)
        } else {
            builder
                .value_rank(i32::try_from(shape.dims.len()).unwrap_or(1))
                .array_dimensions(&shape.dims)
        };
        let input = matches!(spec.direction, Direction::Input | Direction::Tunable);
        builder = if input {
            builder.writable()
        } else {
            builder
                .access_level(AccessLevel::CURRENT_READ)
                .user_access_level(AccessLevel::CURRENT_READ)
        };
        if !builder.is_valid() {
            return Err(ConnectorError::Config(format!(
                "signal `{}`: cannot be a node",
                spec.name
            )));
        }
        builder.insert(&mut *space);
        let exposed = Exposed {
            id,
            node,
            shape,
            scratch,
        };
        if input {
            writable.push(exposed);
        } else {
            readable.push(exposed);
        }
    }
    Ok((readable, writable))
}

/// The node value of `e` from the image.
fn current(image: &ImageHandle, e: &mut Exposed) -> Option<DataValue> {
    let stamp = image.read(e.id, &mut e.scratch).ok()?;
    let now = DateTime::now();
    let (status, source) = match stamp {
        Some(stamp) => (StatusCode::Good, convert::wall_time(stamp)),
        None => (StatusCode::UncertainInitialValue, now),
    };
    Some(DataValue {
        value: Some(convert::to_variant(&e.scratch, &e.shape)),
        status: Some(status),
        source_timestamp: Some(source),
        server_timestamp: Some(now),
        ..Default::default()
    })
}

/// Copy `items` from the image into their nodes, notifying subscriptions.
fn refresh<'a>(
    handle: &ServerHandle,
    manager: &SimpleNodeManager,
    image: &ImageHandle,
    items: impl Iterator<Item = &'a mut Exposed>,
) {
    let mut ids = Vec::new();
    let mut values = Vec::new();
    for e in items {
        if let Some(dv) = current(image, e) {
            ids.push(e.node.clone());
            values.push(dv);
        }
    }
    if ids.is_empty() {
        return;
    }
    let result = manager.set_values(
        handle.subscriptions(),
        ids.iter().zip(values).map(|(id, dv)| (id, None, dv)),
    );
    if let Err(status) = result {
        tracing::warn!(%status, "server node update failed");
    }
}

/// The write callback of one writable signal.
fn on_write(
    image: ImageHandle,
    exposed: &Exposed,
    changed: mpsc::UnboundedSender<usize>,
    index: usize,
) -> impl Fn(DataValue, &NumericRange) -> StatusCode + Send + Sync + 'static {
    let id = exposed.id;
    let shape = exposed.shape.clone();
    let template = exposed.scratch.clone();
    move |value: DataValue, range: &NumericRange| {
        if !matches!(range, NumericRange::None) {
            return StatusCode::BadWriteNotSupported;
        }
        let Some(variant) = value.value.as_ref() else {
            return StatusCode::BadTypeMismatch;
        };
        if matches!(variant, Variant::Empty) {
            return StatusCode::BadTypeMismatch;
        }
        let mut buffer = template.clone();
        if let Err(status) = convert::from_variant(variant, &mut buffer, &shape) {
            return status;
        }
        match image.write(id, &buffer) {
            Ok(()) => {
                let _ = changed.send(index);
                StatusCode::Good
            }
            Err(ImageError::NotWritable(_)) => StatusCode::BadNotWritable,
            Err(ImageError::Mismatch(_)) => StatusCode::BadTypeMismatch,
            Err(_) => StatusCode::BadInternalError,
        }
    }
}

impl Connector for OpcUaServer {
    fn id(&self) -> &str {
        &self.id
    }

    fn discover_len<'a>(
        &'a mut self,
        _signal: &'a str,
    ) -> BoxFuture<'a, Result<Option<usize>, ConnectorError>> {
        Box::pin(async { Ok(None) })
    }

    fn bind<'a>(
        &'a mut self,
        layout: &'a ImageLayout,
    ) -> BoxFuture<'a, Result<(), ConnectorError>> {
        Box::pin(async move {
            self.prepared = Some(self.prepare(layout).await?);
            Ok(())
        })
    }

    fn run(
        self: Box<Self>,
        image: ImageHandle,
        mut shutdown: Shutdown,
    ) -> BoxFuture<'static, Result<(), ConnectorError>> {
        Box::pin(async move {
            let this = *self;
            let mut p = match this.prepared {
                Some(p) => p,
                None => this.prepare(image.layout()).await?,
            };
            let (tx, mut rx) = mpsc::unbounded_channel();
            for (index, exposed) in p.writable.iter().enumerate() {
                p.manager.inner().add_write_callback(
                    exposed.node.clone(),
                    on_write(image.clone(), exposed, tx.clone(), index),
                );
            }
            drop(tx);

            refresh(
                &p.handle,
                &p.manager,
                &image,
                p.readable.iter_mut().chain(p.writable.iter_mut()),
            );

            let mut server = tokio::spawn(p.server.run_with(p.listener));
            tracing::info!(connector = %this.id, endpoint = %this.settings.endpoint, "OPC UA server up");
            let mut published = image.published();
            let result = loop {
                if *shutdown.borrow() {
                    break Ok(());
                }
                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() {
                            break Ok(());
                        }
                    }
                    changed = published.changed() => {
                        if changed.is_err() {
                            break Ok(());
                        }
                        // Inputs and tunables too: other connectors and the engine write them.
                        refresh(
                            &p.handle,
                            &p.manager,
                            &image,
                            p.readable.iter_mut().chain(p.writable.iter_mut()),
                        );
                    }
                    Some(first) = rx.recv() => {
                        let mut dirty = BTreeSet::from([first]);
                        while let Ok(more) = rx.try_recv() {
                            dirty.insert(more);
                        }
                        let items = p
                            .writable
                            .iter_mut()
                            .enumerate()
                            .filter(|(i, _)| dirty.contains(i))
                            .map(|(_, e)| e);
                        refresh(&p.handle, &p.manager, &image, items);
                    }
                    ended = &mut server => {
                        break Err(ConnectorError::Io(format!("OPC UA server stopped: {ended:?}")));
                    }
                }
            };
            p.handle.cancel();
            let _ = tokio::time::timeout(STOP_TIMEOUT, &mut server).await;
            server.abort();
            result
        })
    }
}
