//! Model adapters and connectors by their project `kind`.

use std::path::Path;
use std::sync::Arc;

use clap::ValueEnum;
use taktwerk_core::connector::Connector;
use taktwerk_core::model::ModelAdapter;
use taktwerk_core::project::ConnectorConfig;

/// A model adapter kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ModelKind {
    /// FMI 2 or FMI 3 co-simulation FMU (`.fmu` or an extracted directory).
    Fmi,
    /// Raw C library package (`taktwerk-model.toml` and libraries).
    Raw,
}

impl ModelKind {
    /// The `kind` string of the project file.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fmi => "fmi",
            Self::Raw => "raw",
        }
    }

    /// Parse a project `kind`.
    ///
    /// # Errors
    /// An unknown kind, with the known ones listed.
    pub fn parse(kind: &str) -> Result<Self, String> {
        match kind {
            "fmi" => Ok(Self::Fmi),
            "raw" => Ok(Self::Raw),
            other => Err(format!(
                "unknown model kind `{other}`; known kinds: fmi, raw"
            )),
        }
    }

    /// Load the model at `path`.
    ///
    /// # Errors
    /// The adapter's load error, as text.
    pub fn load(self, path: &Path) -> Result<Arc<dyn ModelAdapter>, String> {
        match self {
            Self::Fmi => taktwerk_fmi::FmuAdapter::load(path)
                .map(|a| Arc::new(a) as Arc<dyn ModelAdapter>)
                .map_err(|e| e.to_string()),
            Self::Raw => taktwerk_raw::RawModel::load(path)
                .map(|a| Arc::new(a) as Arc<dyn ModelAdapter>)
                .map_err(|e| e.to_string()),
        }
    }
}

/// Connector kinds this binary builds.
pub const CONNECTOR_KINDS: &[&str] = &[taktwerk_opcua::server::KIND, taktwerk_opcua::client::KIND];

/// Build the connector `config` describes.
///
/// # Errors
/// An unknown kind, with the known ones listed, or the connector's own config error.
pub fn connector(config: &ConnectorConfig) -> Result<Box<dyn Connector>, String> {
    if taktwerk_opcua::handles(&config.kind) {
        return taktwerk_opcua::build(config).map_err(|e| e.to_string());
    }
    Err(format!(
        "connector `{}`: unknown kind `{}`; known kinds: {}",
        config.id,
        config.kind,
        CONNECTOR_KINDS.join(", ")
    ))
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
    fn unknown_kinds_list_the_known_ones() {
        let err = ModelKind::parse("onnx").unwrap_err();
        assert!(err.contains("fmi, raw"), "{err}");
        let config: ConnectorConfig = toml::from_str("id = \"bus\"\nkind = \"modbus\"\n").unwrap();
        let err = connector(&config).err().unwrap();
        assert!(err.contains("opcua-server, opcua-client"), "{err}");
        assert!(err.contains("`bus`"), "{err}");
    }

    #[test]
    fn builds_the_own_server() {
        let config: ConnectorConfig = toml::from_str(
            "id = \"ua\"\nkind = \"opcua-server\"\nendpoint = \"opc.tcp://127.0.0.1:4840\"\n",
        )
        .unwrap();
        assert_eq!(connector(&config).unwrap().id(), "ua");
    }
}
