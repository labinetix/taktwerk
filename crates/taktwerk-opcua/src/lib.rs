//! OPC UA server and client connectors for taktwerk.
//!
//! [`OpcUaServer`] (`kind = "opcua-server"`) exposes every image signal as a node;
//! [`OpcUaClient`] (`kind = "opcua-client"`) syncs mapped signals with another server.

pub mod client;
mod convert;
pub mod server;
pub mod settings;

pub use client::OpcUaClient;
pub use server::OpcUaServer;

use taktwerk_core::connector::{Connector, ConnectorError};
use taktwerk_core::project::ConnectorConfig;

/// Whether `kind` is one of this crate's connectors.
#[must_use]
pub fn handles(kind: &str) -> bool {
    kind == server::KIND || kind == client::KIND
}

/// Build the connector of `config.kind`.
///
/// # Errors
/// [`ConnectorError::Config`] for another kind or invalid settings.
pub fn build(config: &ConnectorConfig) -> Result<Box<dyn Connector>, ConnectorError> {
    match config.kind.as_str() {
        server::KIND => Ok(Box::new(OpcUaServer::from_config(config)?)),
        client::KIND => Ok(Box::new(OpcUaClient::from_config(config)?)),
        other => Err(ConnectorError::Config(format!(
            "connector `{}`: kind `{other}` is not an OPC UA connector",
            config.id
        ))),
    }
}
