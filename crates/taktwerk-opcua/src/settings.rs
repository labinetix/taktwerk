//! Settings shared by both connectors.

use opcua::crypto::SecurityPolicy;
use opcua::types::MessageSecurityMode;
use serde::Deserialize;
use taktwerk_core::connector::ConnectorError;
use taktwerk_core::project::ConnectorConfig;

/// Message security of an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Security {
    /// No signing, no encryption (SecurityPolicy None).
    None,
    /// Signed with Basic256Sha256.
    Sign,
    /// Signed and encrypted with Basic256Sha256.
    SignEncrypt,
}

impl Security {
    pub(crate) fn policy(self) -> SecurityPolicy {
        match self {
            Self::None => SecurityPolicy::None,
            Self::Sign | Self::SignEncrypt => SecurityPolicy::Basic256Sha256,
        }
    }

    pub(crate) fn mode(self) -> MessageSecurityMode {
        match self {
            Self::None => MessageSecurityMode::None,
            Self::Sign => MessageSecurityMode::Sign,
            Self::SignEncrypt => MessageSecurityMode::SignAndEncrypt,
        }
    }
}

/// A user name with its password, given inline or by environment variable.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    /// User name.
    pub user: String,
    /// Password, inline.
    #[serde(default)]
    pub password: Option<String>,
    /// Name of the environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
}

impl Credentials {
    /// The password, from the setting or the environment.
    pub(crate) fn resolve_password(&self) -> Result<String, ConnectorError> {
        match (&self.password, &self.password_env) {
            (Some(p), None) => Ok(p.clone()),
            (None, Some(var)) => std::env::var(var).map_err(|_| {
                ConnectorError::Config(format!(
                    "user `{}`: environment variable `{var}` is not set",
                    self.user
                ))
            }),
            _ => Err(ConnectorError::Config(format!(
                "user `{}`: set exactly one of `password` and `password_env`",
                self.user
            ))),
        }
    }
}

/// Deserialize a connector's kind-specific keys into `T`.
pub(crate) fn parse<T: for<'de> Deserialize<'de>>(
    config: &ConnectorConfig,
) -> Result<T, ConnectorError> {
    toml::Value::Table(config.settings.clone())
        .try_into()
        .map_err(|e| ConnectorError::Config(format!("connector `{}`: {e}", config.id)))
}

/// Host and port of an `opc.tcp://host:port[/path]` URL.
pub(crate) fn host_port(url: &str) -> Result<(String, u16), ConnectorError> {
    let bad = || ConnectorError::Config(format!("endpoint `{url}`: expected opc.tcp://host:port"));
    let rest = url.strip_prefix("opc.tcp://").ok_or_else(bad)?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = authority.rsplit_once(':').ok_or_else(bad)?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(bad());
    }
    let port = port.parse().map_err(|_| bad())?;
    Ok((host.to_owned(), port))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn parses_endpoints() {
        assert_eq!(
            host_port("opc.tcp://0.0.0.0:4840").ok(),
            Some(("0.0.0.0".to_owned(), 4840))
        );
        assert_eq!(
            host_port("opc.tcp://plc.local:4841/path").ok(),
            Some(("plc.local".to_owned(), 4841))
        );
        assert!(host_port("http://x:1").is_err());
        assert!(host_port("opc.tcp://x").is_err());
    }
}
