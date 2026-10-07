//! The connector contract: something that syncs part of the process image with the outside.
//!
//! Lifecycle, all at init except `run`: the engine builds each connector from its project table,
//! asks it for the lengths of dimensions bound `"from-server"`, builds the image, lets every
//! connector `bind` (verify its mapping against the outside, fail-closed), starts the cycle, then
//! drives every `run` future until shutdown.

use std::future::Future;
use std::pin::Pin;

use tokio::sync::watch;

use crate::image::{ImageHandle, ImageLayout};

/// A boxed `Send` future, so the trait stays object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Flips to `true` once when the engine shuts down.
pub type Shutdown = watch::Receiver<bool>;

/// A connector instance built from one `[[connector]]` table.
pub trait Connector: Send {
    /// The connector's id from the project file.
    fn id(&self) -> &str;

    /// The length of the external item this connector maps to `signal`, if it maps it and the
    /// outside knows a length. Used for dimensions bound `"from-server"`.
    fn discover_len<'a>(
        &'a mut self,
        signal: &'a str,
    ) -> BoxFuture<'a, Result<Option<usize>, ConnectorError>>;

    /// Check every mapping against the outside: items exist, types and lengths match `layout`.
    fn bind<'a>(&'a mut self, layout: &'a ImageLayout)
    -> BoxFuture<'a, Result<(), ConnectorError>>;

    /// Verify like [`bind`](Self::bind) without taking a resource a running engine would hold
    /// (a server's listen socket); `taktwerk check` calls this. Defaults to `bind`.
    fn check<'a>(
        &'a mut self,
        layout: &'a ImageLayout,
    ) -> BoxFuture<'a, Result<(), ConnectorError>> {
        self.bind(layout)
    }

    /// Sync the image until `shutdown`. Reconnects on its own; returns only on shutdown or a
    /// failure it cannot recover from.
    fn run(
        self: Box<Self>,
        image: ImageHandle,
        shutdown: Shutdown,
    ) -> BoxFuture<'static, Result<(), ConnectorError>>;
}

/// A connector failure.
#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    /// The connector's project table is invalid.
    #[error("config: {0}")]
    Config(String),
    /// A mapping does not match the outside.
    #[error("bind: {0}")]
    Bind(String),
    /// Transport failure.
    #[error("io: {0}")]
    Io(String),
}
