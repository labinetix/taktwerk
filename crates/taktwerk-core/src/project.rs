//! The project file: one TOML document per engine, the single source of truth.
//!
//! ```toml
//! [engine]
//! tick_ms = 10.0
//!
//! [models.pid]
//! kind = "raw"            # or "fmi"
//! path = "models/pid"     # package directory, or an .fmu
//!
//! [[instance]]            # runs in declared order
//! id = "loop1"
//! model = "pid"
//! every = 2               # period = every * tick
//! dims = { n = 3, m = "from-server" }
//! parameters = { kp = 1.5 }
//! inputs = { y = { signal = "plant.y", max_age_ms = 50.0 } }
//! outputs = { u = "plant.u" }
//!
//! [[connector]]
//! id = "plc"
//! kind = "opcua-client"
//! # kind-specific keys follow, parsed by the connector
//! ```
//!
//! An unmapped input, output or tunable `v` of instance `i` gets the signal `i.v`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A whole project file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// Engine-wide settings.
    pub engine: EngineConfig,
    /// Model packages by id.
    #[serde(default)]
    pub models: BTreeMap<String, ModelRef>,
    /// Instances, in execution order.
    #[serde(default, rename = "instance")]
    pub instances: Vec<InstanceConfig>,
    /// Connectors.
    #[serde(default, rename = "connector")]
    pub connectors: Vec<ConnectorConfig>,
}

/// `[engine]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    /// Base tick, milliseconds; every instance period is a multiple of it.
    pub tick_ms: f64,
    /// Simulation time of the first tick, seconds.
    #[serde(default)]
    pub start_time: f64,
    /// Default `max_age_ms` of external inputs; `None` never goes stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_max_age_ms: Option<f64>,
    /// Prefix of the engine's own signals (heartbeat, status, counters).
    #[serde(default = "default_system_prefix")]
    pub system_prefix: String,
    /// Real-time settings of the cycle thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realtime: Option<RealtimeConfig>,
}

fn default_system_prefix() -> String {
    "taktwerk".to_owned()
}

/// `[engine.realtime]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeConfig {
    /// Scheduling class of the cycle thread.
    pub policy: SchedPolicy,
    /// Priority within the class, 1–99.
    pub priority: u8,
    /// CPU the cycle thread is pinned to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<usize>,
    /// `mlockall` before the first tick.
    #[serde(default)]
    pub lock_memory: bool,
}

/// A real-time scheduling class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SchedPolicy {
    /// `SCHED_FIFO`.
    Fifo,
    /// `SCHED_RR`.
    Rr,
}

/// `[models.<id>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    /// Adapter: `"fmi"` or `"raw"`.
    pub kind: String,
    /// Package path, relative to the project file.
    pub path: PathBuf,
}

/// `[[instance]]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceConfig {
    /// Unique id.
    pub id: String,
    /// Key into `models`.
    pub model: String,
    /// Period in base ticks.
    #[serde(default = "one")]
    pub every: u32,
    /// Dimension bindings; unbound dimensions take the model's default.
    #[serde(default)]
    pub dims: BTreeMap<String, DimBinding>,
    /// Parameter and tunable start values.
    #[serde(default)]
    pub parameters: BTreeMap<String, toml::Value>,
    /// Input variable → signal.
    #[serde(default)]
    pub inputs: BTreeMap<String, InputBinding>,
    /// Output variable → signal.
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
    /// Tunable variable → signal.
    #[serde(default)]
    pub tunables: BTreeMap<String, String>,
}

fn one() -> u32 {
    1
}

/// A dimension bound to a length, or discovered from a connector at init.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DimBinding {
    /// A fixed length.
    Len(usize),
    /// `"from-server"`: the length of the external item a connector maps to a signal of this
    /// dimension.
    Discover(FromServer),
}

/// The literal `"from-server"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromServer {
    /// `"from-server"`.
    #[serde(rename = "from-server")]
    FromServer,
}

/// An input's signal, with an optional age limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputBinding {
    /// Signal name only.
    Signal(String),
    /// Signal name and age limit.
    Detailed {
        /// Signal name.
        signal: String,
        /// Overrides `engine.input_max_age_ms`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_age_ms: Option<f64>,
    },
}

/// `[[connector]]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectorConfig {
    /// Unique id.
    pub id: String,
    /// Connector kind, e.g. `"opcua-server"`, `"opcua-client"`.
    pub kind: String,
    /// Kind-specific keys, parsed by the connector.
    #[serde(flatten)]
    pub settings: toml::Table,
}

impl Project {
    /// Parse and structurally validate a project file.
    ///
    /// # Errors
    /// The file cannot be read or parsed, or [`Self::validate`] fails.
    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ProjectError::Read(path.to_path_buf(), e.to_string()))?;
        text.parse()
    }

    /// The checks that need no model interface: ids unique, model refs known, periods positive.
    ///
    /// # Errors
    /// The first violation found.
    pub fn validate(&self) -> Result<(), ProjectError> {
        let tick_ok = self.engine.tick_ms.is_finite() && self.engine.tick_ms > 0.0;
        if !tick_ok {
            return Err(ProjectError::Invalid(format!(
                "engine.tick_ms must be a positive number, got {}",
                self.engine.tick_ms
            )));
        }
        if self.engine.system_prefix.is_empty() {
            return Err(ProjectError::Invalid(
                "engine.system_prefix is empty".into(),
            ));
        }
        if let Some(rt) = &self.engine.realtime {
            if !(1..=99).contains(&rt.priority) {
                return Err(ProjectError::Invalid(format!(
                    "engine.realtime.priority must be 1..=99, got {}",
                    rt.priority
                )));
            }
        }
        let mut ids = BTreeSet::new();
        for inst in &self.instances {
            if inst.id.is_empty() || inst.id.contains('.') {
                return Err(ProjectError::Invalid(format!(
                    "instance id `{}` must be non-empty and contain no `.`",
                    inst.id
                )));
            }
            if !ids.insert(inst.id.as_str()) {
                return Err(ProjectError::Invalid(format!(
                    "duplicate instance id `{}`",
                    inst.id
                )));
            }
            if !self.models.contains_key(&inst.model) {
                return Err(ProjectError::Invalid(format!(
                    "instance `{}` references unknown model `{}`",
                    inst.id, inst.model
                )));
            }
            if inst.every == 0 {
                return Err(ProjectError::Invalid(format!(
                    "instance `{}`: every must be >= 1",
                    inst.id
                )));
            }
        }
        let mut ids = BTreeSet::new();
        for c in &self.connectors {
            if !ids.insert(c.id.as_str()) {
                return Err(ProjectError::Invalid(format!(
                    "duplicate connector id `{}`",
                    c.id
                )));
            }
        }
        Ok(())
    }
}

impl FromStr for Project {
    type Err = ProjectError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let project: Self = toml::from_str(text).map_err(|e| ProjectError::Parse(e.to_string()))?;
        project.validate()?;
        Ok(project)
    }
}

impl InputBinding {
    /// The signal name.
    #[must_use]
    pub fn signal(&self) -> &str {
        match self {
            Self::Signal(s) | Self::Detailed { signal: s, .. } => s,
        }
    }

    /// The age limit, when the binding sets one.
    #[must_use]
    pub fn max_age_ms(&self) -> Option<f64> {
        match self {
            Self::Signal(_) => None,
            Self::Detailed { max_age_ms, .. } => *max_age_ms,
        }
    }
}

/// A project file could not be read, parsed or validated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectError {
    /// The file could not be read.
    #[error("read {0}: {1}")]
    Read(PathBuf, String),
    /// The TOML does not match the schema.
    #[error("parse: {0}")]
    Parse(String),
    /// The document is well-formed but inconsistent.
    #[error("{0}")]
    Invalid(String),
}
