//! The model contract: a size-generic interface, and adapters that instantiate it with bound sizes.
//!
//! A model package declares variables with symbolic dimensions; an instance binds every symbol to
//! a length. Adapters (FMI, raw C) translate this contract onto their library; the scheduler only
//! ever sees [`ModelInstance`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::value::{Buffer, Dim, Layout, ScalarType};

/// How a variable takes part in a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Causality {
    /// Read by the model every step it runs.
    Input,
    /// Written by the model every step it runs.
    Output,
    /// Set once before init; fixed for the instance's lifetime.
    Parameter,
    /// Set before init and changeable while running; delivered before the next step.
    Tunable,
}

/// One declared variable of a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variable {
    /// Name, unique within the model.
    pub name: String,
    /// Role in a step.
    pub causality: Causality,
    /// Element type.
    #[serde(rename = "type")]
    pub ty: ScalarType,
    /// Shape; empty for a scalar.
    #[serde(default)]
    pub shape: Vec<Dim>,
    /// Storage order when `shape` has more than one dimension.
    #[serde(default)]
    pub layout: Layout,
    /// Unit, free text, informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Description, informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A named dimension the instance binds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dimension {
    /// Symbol used in variable shapes.
    pub name: String,
    /// Smallest admissible length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<usize>,
    /// Largest admissible length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
    /// Length used when the instance binds none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<usize>,
}

/// Whether one loaded library can host several instances.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Instances {
    /// State lives in globals; a second instance needs a private copy of the library.
    #[default]
    Single,
    /// Every instance carries its own state.
    Multiple,
}

/// The size-generic interface of a model, as its package declares it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelInterface {
    /// Model name, informational.
    pub name: String,
    /// Declared dimensions.
    #[serde(default)]
    pub dimensions: Vec<Dimension>,
    /// Declared variables; inputs, outputs and tunables keep this order in [`StepIo`].
    #[serde(default)]
    pub variables: Vec<Variable>,
    /// Instance capability of the library.
    #[serde(default)]
    pub instances: Instances,
}

/// Dimension symbol → bound length, for one instance.
pub type BoundDims = BTreeMap<String, usize>;

/// Parameter and tunable name → start value, for one instance.
pub type ParamValues = BTreeMap<String, Buffer>;

/// Everything an adapter needs to create one instance.
#[derive(Debug, Clone)]
pub struct InstanceSpec {
    /// Instance id from the project file.
    pub id: String,
    /// Every dimension of the interface, bound.
    pub dims: BoundDims,
    /// Values for parameters and tunables; unlisted ones keep the model's default.
    pub params: ParamValues,
    /// Communication step of this instance, seconds.
    pub step_size: f64,
}

/// The buffers one instance exchanges per step, allocated at init in interface order.
#[derive(Debug, Clone, Default)]
pub struct StepIo {
    /// One buffer per `Input` variable.
    pub inputs: Vec<Buffer>,
    /// One buffer per `Output` variable.
    pub outputs: Vec<Buffer>,
    /// One buffer per `Tunable` variable.
    pub tunables: Vec<Buffer>,
    /// Set by the scheduler when any tunable changed since the last step; cleared after it.
    pub tunables_changed: bool,
}

/// A model library the engine can instantiate.
pub trait ModelAdapter: Send + Sync {
    /// The declared, size-generic interface.
    fn interface(&self) -> &ModelInterface;

    /// Create one instance with every dimension bound. Runs at init, may allocate.
    ///
    /// # Errors
    /// The library refuses the sizes or parameters, or cannot be loaded.
    fn instantiate(&self, spec: &InstanceSpec) -> Result<Box<dyn ModelInstance>, ModelError>;
}

/// One running instance of a model, driven by the cycle thread.
pub trait ModelInstance: Send {
    /// Enter the running state with the first inputs present in `io`. May allocate.
    ///
    /// # Errors
    /// The library reports a failure.
    fn init(&mut self, start_time: f64, io: &mut StepIo) -> Result<(), ModelError>;

    /// Advance from `time` by the instance's step size. No allocation.
    ///
    /// # Errors
    /// The library reports a failure; the engine then stops.
    fn step(&mut self, time: f64, io: &mut StepIo) -> Result<(), ModelError>;

    /// Release the library's state. Called once, also after an error.
    fn terminate(&mut self);
}

/// A model adapter or library failure.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// The package or library could not be loaded or understood.
    #[error("load: {0}")]
    Load(String),
    /// The bound sizes or parameters are not admissible.
    #[error("instantiate: {0}")]
    Instantiate(String),
    /// The library returned an error code.
    #[error("{call} returned {code}: {detail}")]
    Call {
        /// Library function that failed.
        call: &'static str,
        /// Its return code.
        code: i64,
        /// Context.
        detail: String,
    },
}
