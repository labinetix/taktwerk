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

use std::collections::BTreeMap;
use std::path::PathBuf;

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
