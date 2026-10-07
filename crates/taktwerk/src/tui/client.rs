//! The monitor's OPC UA side: connect, find the signal nodes, poll them, write edits.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use opcua::client::{Client, ClientBuilder, IdentityToken, Session};
use opcua::crypto::SecurityPolicy;
use opcua::types::{
    AttributeId, BrowseDescription, BrowseDirection, BrowseResultMask, DataValue, DateTime,
    Identifier, MessageSecurityMode, NodeClass, NodeClassMask, NodeId, ObjectId, ReadValueId,
    ReferenceTypeId, StatusCode, TimestampsToReturn, Variant, WriteValue,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::app::Row;

/// How often every signal is read.
const POLL: Duration = Duration::from_millis(200);
/// Pause before reconnecting.
const RETRY: Duration = Duration::from_secs(1);
/// `AccessLevel` bit `CurrentWrite`.
const CURRENT_WRITE: u8 = 2;
/// 100 ns ticks from 1601-01-01 to 1970-01-01.
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

/// A signal node.
#[derive(Debug, Clone)]
struct Signal {
    name: String,
    node: NodeId,
    writable: bool,
}

/// An OPC UA source timestamp as wall time.
pub fn system_time(t: &DateTime) -> Option<SystemTime> {
    let ticks = u64::try_from(t.ticks().checked_sub(UNIX_EPOCH_TICKS)?).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(ticks.saturating_mul(100)))
}

/// A connected session.
struct Conn {
    _client: Client,
    session: Arc<Session>,
    events: JoinHandle<StatusCode>,
}

impl Conn {
    async fn open(endpoint: &str) -> Result<Self, String> {
        static N: AtomicU32 = AtomicU32::new(0);
        let pki = std::env::temp_dir().join(format!(
            "taktwerk-tui-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let client = ClientBuilder::new()
            .application_name("taktwerk-tui")
            .application_uri("urn:taktwerk:tui")
            .pki_dir(&pki)
            .create_sample_keypair(false)
            .trust_server_certs(true)
            .session_retry_limit(0)
            .client();
        let _ = std::fs::remove_dir_all(&pki);
        let mut client = client.map_err(|e| format!("client: {e:?}"))?;
        let (session, event_loop) = client
            .connect_to_matching_endpoint(
                (
                    endpoint,
                    SecurityPolicy::None.to_str(),
                    MessageSecurityMode::None,
                ),
                IdentityToken::Anonymous,
            )
            .await
            .map_err(|e| format!("connect {endpoint}: {e}"))?;
        let events = event_loop.spawn();
        tokio::time::timeout(Duration::from_secs(5), session.wait_for_connection())
            .await
            .map_err(|_| format!("connect {endpoint}: timed out"))?;
        Ok(Self {
            _client: client,
            session,
            events,
        })
    }

    async fn close(self) {
        let _ = self.session.disconnect().await;
        self.events.abort();
    }

    /// Every variable in namespace `ns` below `Objects`, with its write access.
    async fn signals(&self, ns: u16) -> Result<Vec<Signal>, String> {
        let mut found = Vec::new();
        let mut pending: Vec<NodeId> = vec![ObjectId::ObjectsFolder.into()];
        while let Some(parent) = pending.pop() {
            let description = BrowseDescription {
                node_id: parent,
                browse_direction: BrowseDirection::Forward,
                reference_type_id: ReferenceTypeId::HierarchicalReferences.into(),
                include_subtypes: true,
                node_class_mask: (NodeClassMask::OBJECT | NodeClassMask::VARIABLE).bits(),
                result_mask: BrowseResultMask::All as u32,
            };
            let mut results = self
                .session
                .browse(&[description], 0, None)
                .await
                .map_err(|e| format!("browse: {e}"))?;
            while let Some(result) = results.pop() {
                for r in result.references.unwrap_or_default() {
                    let node = r.node_id.node_id;
                    if node.namespace != ns {
                        continue;
                    }
                    match r.node_class {
                        NodeClass::Object => pending.push(node),
                        NodeClass::Variable => {
                            if let Identifier::String(s) = &node.identifier {
                                found.push(Signal {
                                    name: s.as_ref().to_owned(),
                                    node,
                                    writable: false,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                if result.continuation_point.is_null() {
                    break;
                }
                results = self
                    .session
                    .browse_next(false, &[result.continuation_point])
                    .await
                    .map_err(|e| format!("browse: {e}"))?;
            }
        }
        let reads: Vec<ReadValueId> = found
            .iter()
            .map(|s| ReadValueId::new(s.node.clone(), AttributeId::UserAccessLevel))
            .collect();
        let levels = self.read(&reads).await?;
        for (s, level) in found.iter_mut().zip(levels) {
            s.writable = matches!(level.value, Some(Variant::Byte(b)) if b & CURRENT_WRITE != 0);
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }

    async fn read(&self, reads: &[ReadValueId]) -> Result<Vec<DataValue>, String> {
        let mut out = Vec::with_capacity(reads.len());
        // Servers cap the nodes per request; stay well below common limits.
        for chunk in reads.chunks(500) {
            out.extend(
                self.session
                    .read(chunk, TimestampsToReturn::Source, 0.0)
                    .await
                    .map_err(|e| format!("read: {e}"))?,
            );
        }
        Ok(out)
    }

    async fn poll(&self, signals: &[Signal]) -> Result<Vec<Row>, String> {
        let reads: Vec<ReadValueId> = signals
            .iter()
            .map(|s| ReadValueId::new(s.node.clone(), AttributeId::Value))
            .collect();
        let values = self.read(&reads).await?;
        Ok(signals
            .iter()
            .zip(values)
            .map(|(s, dv)| Row {
                name: s.name.clone(),
                writable: s.writable,
                // A never-written signal carries the read time; it has no age.
                stamp: if dv.status == Some(StatusCode::UncertainInitialValue) {
                    None
                } else {
                    dv.source_timestamp.as_ref().and_then(system_time)
                },
                value: dv.value.unwrap_or(Variant::Empty),
            })
            .collect())
    }

    async fn write(&self, node: &NodeId, value: Variant) -> Result<(), String> {
        let status = self
            .session
            .write(&[WriteValue::value_attr(node.clone(), value)])
            .await
            .map_err(|e| format!("write: {e}"))?;
        match status.first() {
            Some(s) if s.is_good() => Ok(()),
            Some(s) => Err(format!("write refused: {s}")),
            None => Err("write: no result".to_owned()),
        }
    }
}

/// What the network side reports to the screen.
pub struct Feed {
    /// Every signal, refreshed each poll.
    pub rows: watch::Sender<Vec<Row>>,
    /// Connection state and write outcomes.
    pub messages: std::sync::mpsc::Sender<String>,
}

/// Keep a session to `endpoint`, poll every signal of `namespace`, apply `writes`; reconnects
/// until the receiving side is gone.
pub async fn serve(
    endpoint: String,
    namespace: String,
    feed: Feed,
    mut writes: mpsc::UnboundedReceiver<(String, Variant)>,
) {
    loop {
        let ended = session(&endpoint, &namespace, &feed, &mut writes).await;
        match ended {
            Ok(()) => return,
            Err(e) => {
                if feed.messages.send(format!("{e}; retrying")).is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(RETRY).await;
    }
}

async fn session(
    endpoint: &str,
    namespace: &str,
    feed: &Feed,
    writes: &mut mpsc::UnboundedReceiver<(String, Variant)>,
) -> Result<(), String> {
    let conn = Conn::open(endpoint).await?;
    let result = async {
        let ns = conn
            .session
            .get_namespace_index(namespace)
            .await
            .map_err(|e| format!("namespace `{namespace}`: {e}"))?;
        let signals = conn.signals(ns).await?;
        let _ = feed
            .messages
            .send(format!("connected, {} signals", signals.len()));
        let mut tick = tokio::time::interval(POLL);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let rows = conn.poll(&signals).await?;
                    if feed.rows.send(rows).is_err() {
                        return Ok(());
                    }
                }
                request = writes.recv() => {
                    let Some((name, value)) = request else { return Ok(()) };
                    let message = match signals.iter().find(|s| s.name == name) {
                        Some(s) => match conn.write(&s.node, value).await {
                            Ok(()) => format!("wrote {name}"),
                            Err(e) => format!("{name}: {e}"),
                        },
                        None => format!("{name}: no such node"),
                    };
                    let _ = feed.messages.send(message);
                }
            }
        }
    }
    .await;
    conn.close().await;
    result
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    #[test]
    fn source_timestamps_convert_to_wall_time() {
        let t = DateTime::from(UNIX_EPOCH_TICKS + 15_000_000);
        assert_eq!(
            system_time(&t),
            Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1500))
        );
        assert_eq!(system_time(&DateTime::from(0)), None);
    }
}
