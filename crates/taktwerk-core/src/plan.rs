//! Resolution of a project into a plan: every dimension bound, every parameter typed, every
//! signal named, wired and laid out, the engine's own signals added.
//!
//! Signal naming: an unmapped variable `v` of instance `i` is the signal `i.v`. An input mapped to
//! a signal some instance outputs is wired to it and creates no external input; an input nobody
//! outputs is an external `Input`. Two outputs on one signal, a tunable on an input or output
//! signal, or one signal used with two types, shapes or layouts is an error. A wired input reads
//! what its producer stored last: the same tick when the producer runs earlier in declared order,
//! else the previous one; it never goes stale, so it may not carry a `max_age_ms`.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::connector::{Connector, ConnectorError};
use crate::image::{Direction, ImageError, ImageLayout, SignalId, SignalSpec};
use crate::model::{BoundDims, Causality, InstanceSpec, ModelInterface, ParamValues, Variable};
use crate::project::{DimBinding, InputBinding, InstanceConfig, Project, ProjectError};
use crate::value::{Buffer, Dim, Layout, ScalarType};

/// A resolved project, ready to instantiate and run.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Base tick.
    pub tick: Duration,
    /// Simulation time of the first tick, seconds.
    pub start_time: f64,
    /// Every signal of the engine.
    pub layout: ImageLayout,
    /// Instances in execution order.
    pub instances: Vec<InstancePlan>,
    /// The engine's own signals.
    pub system: SystemSignals,
}

/// One instance, resolved.
#[derive(Debug, Clone)]
pub struct InstancePlan {
    /// Instance id.
    pub id: String,
    /// Model id, the key into the project's `models` and the adapter map.
    pub model: String,
    /// Period in base ticks.
    pub every: u32,
    /// What the adapter needs to create the instance.
    pub spec: InstanceSpec,
    /// `StepIo::inputs` slot → variable and signal, in interface order.
    pub inputs: Vec<Port>,
    /// `StepIo::outputs` slot → variable and signal, in interface order.
    pub outputs: Vec<Port>,
    /// `StepIo::tunables` slot → variable and signal, in interface order.
    pub tunables: Vec<Port>,
}

/// One `StepIo` slot of an instance and the signal it exchanges with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    /// Variable name in the model.
    pub name: String,
    /// Signal in the image.
    pub signal: SignalId,
}

/// The engine's own signals, under `engine.system_prefix`.
///
/// `heartbeat` (`u64`) advances once per published tick; `status` (`i32`, see
/// [`crate::schedule::Status`]); `cycle` (`u64`) counts every tick; `overruns` (`u64`) missed
/// deadlines; `stale` (`u64`) ticks skipped for a stale input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemSignals {
    /// `<prefix>.heartbeat`.
    pub heartbeat: SignalId,
    /// `<prefix>.status`.
    pub status: SignalId,
    /// `<prefix>.cycle`.
    pub cycle: SignalId,
    /// `<prefix>.overruns`.
    pub overruns: SignalId,
    /// `<prefix>.stale`.
    pub stale: SignalId,
}

/// Resolution failed.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// The project is inconsistent.
    #[error(transparent)]
    Project(#[from] ProjectError),
    /// No interface was supplied for a model the project references.
    #[error("no interface loaded for model `{0}`")]
    NoInterface(String),
    /// A model interface is inconsistent.
    #[error("model `{model}`: {detail}")]
    Interface {
        /// Model id.
        model: String,
        /// What is wrong.
        detail: String,
    },
    /// An instance's dimension binding is invalid.
    #[error("instance `{instance}`, dimension `{dim}`: {detail}")]
    Dimension {
        /// Instance id.
        instance: String,
        /// Dimension symbol.
        dim: String,
        /// What is wrong.
        detail: String,
    },
    /// An instance binds or sets a variable wrongly.
    #[error("instance `{instance}`, variable `{name}`: {detail}")]
    Variable {
        /// Instance id.
        instance: String,
        /// Variable name.
        name: String,
        /// What is wrong.
        detail: String,
    },
    /// Two uses of one signal disagree.
    #[error("signal `{name}`: {detail}")]
    Signal {
        /// Signal name.
        name: String,
        /// What is wrong.
        detail: String,
    },
    /// A connector failed while discovering a length.
    #[error("discover: {0}")]
    Discover(#[from] ConnectorError),
    /// The image layout could not be built.
    #[error(transparent)]
    Image(#[from] ImageError),
}

impl Plan {
    /// Resolve `project` against the interface of each model it references.
    ///
    /// Dimensions bound `"from-server"` are asked of `connectors`, in order, with the signal name
    /// of a one-dimensional variable of that dimension; the first length answered wins.
    ///
    /// # Errors
    /// Any inconsistency between project, interfaces and the outside.
    pub async fn resolve(
        project: &Project,
        interfaces: &BTreeMap<String, ModelInterface>,
        connectors: &mut [Box<dyn Connector>],
    ) -> Result<Self, PlanError> {
        project.validate()?;
        let tick = duration_ms(project.engine.tick_ms).ok_or_else(|| {
            ProjectError::Invalid(format!(
                "engine.tick_ms = {} is not a duration",
                project.engine.tick_ms
            ))
        })?;
        let default_max_age = match project.engine.input_max_age_ms {
            None => None,
            Some(ms) => Some(duration_ms(ms).ok_or_else(|| {
                ProjectError::Invalid(format!("engine.input_max_age_ms = {ms} is not a duration"))
            })?),
        };

        let mut table = SignalTable::new(&project.engine.system_prefix);
        let mut resolved = Vec::with_capacity(project.instances.len());
        for inst in &project.instances {
            let interface = interfaces
                .get(&inst.model)
                .ok_or_else(|| PlanError::NoInterface(inst.model.clone()))?;
            check_interface(&inst.model, interface)?;
            check_bindings(inst, interface)?;
            let dims = bind_dims(inst, interface, connectors).await?;
            let params = convert_params(inst, interface, &dims)?;
            resolved.push(Resolved {
                config: inst,
                interface,
                dims,
                params,
            });
        }

        // Outputs first so an input finds its producer whatever the declared order.
        for r in &resolved {
            for var in r
                .interface
                .variables
                .iter()
                .filter(|v| v.causality == Causality::Output)
            {
                let name = r
                    .config
                    .outputs
                    .get(&var.name)
                    .cloned()
                    .unwrap_or_else(|| default_name(&r.config.id, &var.name));
                table.output(&name, var, &r.dims)?;
            }
        }
        let mut instances = Vec::with_capacity(resolved.len());
        for r in &resolved {
            let mut inputs = Vec::new();
            let mut outputs = Vec::new();
            let mut tunables = Vec::new();
            for var in &r.interface.variables {
                match var.causality {
                    Causality::Input => {
                        let binding = r.config.inputs.get(&var.name);
                        let name = binding
                            .map(|b| b.signal().to_owned())
                            .unwrap_or_else(|| default_name(&r.config.id, &var.name));
                        let max_age = match binding.and_then(InputBinding::max_age_ms) {
                            None => default_max_age,
                            Some(ms) => {
                                Some(duration_ms(ms).ok_or_else(|| PlanError::Variable {
                                    instance: r.config.id.clone(),
                                    name: var.name.clone(),
                                    detail: format!("max_age_ms = {ms} is not a duration"),
                                })?)
                            }
                        };
                        let explicit_age = binding.and_then(InputBinding::max_age_ms).is_some();
                        let signal = table.input(&name, var, &r.dims, max_age, explicit_age)?;
                        inputs.push(Port {
                            name: var.name.clone(),
                            signal,
                        });
                    }
                    Causality::Output => {
                        let name = r
                            .config
                            .outputs
                            .get(&var.name)
                            .cloned()
                            .unwrap_or_else(|| default_name(&r.config.id, &var.name));
                        let signal = table.id_of(&name)?;
                        outputs.push(Port {
                            name: var.name.clone(),
                            signal,
                        });
                    }
                    Causality::Tunable => {
                        let name = r
                            .config
                            .tunables
                            .get(&var.name)
                            .cloned()
                            .unwrap_or_else(|| default_name(&r.config.id, &var.name));
                        let signal = table.tunable(&name, var, &r.dims)?;
                        tunables.push(Port {
                            name: var.name.clone(),
                            signal,
                        });
                    }
                    Causality::Parameter => {}
                }
            }
            instances.push(InstancePlan {
                id: r.config.id.clone(),
                model: r.config.model.clone(),
                every: r.config.every,
                spec: InstanceSpec {
                    id: r.config.id.clone(),
                    dims: r.dims.clone(),
                    params: r.params.clone(),
                    step_size: tick.as_secs_f64() * f64::from(r.config.every),
                },
                inputs,
                outputs,
                tunables,
            });
        }

        let system = table.system;
        let layout = ImageLayout::new(table.specs)?;
        Ok(Self {
            tick,
            start_time: project.engine.start_time,
            layout,
            instances,
            system,
        })
    }
}

/// An instance with its interface, dimensions and parameters resolved.
struct Resolved<'a> {
    config: &'a InstanceConfig,
    interface: &'a ModelInterface,
    dims: BoundDims,
    params: ParamValues,
}

/// `i.v`.
fn default_name(instance: &str, var: &str) -> String {
    format!("{instance}.{var}")
}

/// Milliseconds as a duration; `None` when negative, NaN or out of range.
fn duration_ms(ms: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(ms / 1000.0).ok()
}

/// The variables of an interface declare only known dimensions and distinct names.
fn check_interface(model: &str, interface: &ModelInterface) -> Result<(), PlanError> {
    let err = |detail: String| PlanError::Interface {
        model: model.to_owned(),
        detail,
    };
    let mut names = std::collections::BTreeSet::new();
    for d in &interface.dimensions {
        if !names.insert(d.name.as_str()) {
            return Err(err(format!("dimension `{}` declared twice", d.name)));
        }
        if d.min.zip(d.max).is_some_and(|(min, max)| min > max) {
            return Err(err(format!("dimension `{}`: min > max", d.name)));
        }
    }
    let mut names = std::collections::BTreeSet::new();
    for v in &interface.variables {
        if !names.insert(v.name.as_str()) {
            return Err(err(format!("variable `{}` declared twice", v.name)));
        }
        for dim in &v.shape {
            if let Dim::Symbol(s) = dim {
                if !interface.dimensions.iter().any(|d| &d.name == s) {
                    return Err(err(format!(
                        "variable `{}` uses undeclared dimension `{s}`",
                        v.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Every binding and parameter of the instance names a variable of the right causality.
fn check_bindings(inst: &InstanceConfig, interface: &ModelInterface) -> Result<(), PlanError> {
    let var = |name: &str| interface.variables.iter().find(|v| v.name == name);
    let err = |name: &str, detail: String| PlanError::Variable {
        instance: inst.id.clone(),
        name: name.to_owned(),
        detail,
    };
    let expect = |map: &[&String], want: Causality, what: &str| -> Result<(), PlanError> {
        for name in map {
            match var(name) {
                None => return Err(err(name, "unknown variable".into())),
                Some(v) if v.causality != want => {
                    return Err(err(
                        name,
                        format!(
                            "bound under `{what}` but its causality is {:?}",
                            v.causality
                        ),
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(())
    };
    expect(
        &inst.inputs.keys().collect::<Vec<_>>(),
        Causality::Input,
        "inputs",
    )?;
    expect(
        &inst.outputs.keys().collect::<Vec<_>>(),
        Causality::Output,
        "outputs",
    )?;
    expect(
        &inst.tunables.keys().collect::<Vec<_>>(),
        Causality::Tunable,
        "tunables",
    )?;
    for name in inst.parameters.keys() {
        match var(name) {
            None => return Err(err(name, "unknown variable".into())),
            Some(v) if !matches!(v.causality, Causality::Parameter | Causality::Tunable) => {
                return Err(err(
                    name,
                    format!(
                        "set under `parameters` but its causality is {:?}",
                        v.causality
                    ),
                ));
            }
            Some(_) => {}
        }
    }
    for name in inst.dims.keys() {
        if !interface.dimensions.iter().any(|d| &d.name == name) {
            return Err(PlanError::Dimension {
                instance: inst.id.clone(),
                dim: name.clone(),
                detail: "the model declares no such dimension".into(),
            });
        }
    }
    let mapped = inst
        .inputs
        .iter()
        .map(|(n, b)| (n, b.signal()))
        .chain(inst.outputs.iter().map(|(n, s)| (n, s.as_str())))
        .chain(inst.tunables.iter().map(|(n, s)| (n, s.as_str())));
    for (name, signal) in mapped {
        if signal.is_empty() {
            return Err(err(name, "mapped to an empty signal name".into()));
        }
    }
    Ok(())
}

/// Bind every declared dimension: literal, discovered, or the model's default.
async fn bind_dims(
    inst: &InstanceConfig,
    interface: &ModelInterface,
    connectors: &mut [Box<dyn Connector>],
) -> Result<BoundDims, PlanError> {
    let mut dims = BoundDims::new();
    for d in &interface.dimensions {
        let err = |detail: String| PlanError::Dimension {
            instance: inst.id.clone(),
            dim: d.name.clone(),
            detail,
        };
        let len = match inst.dims.get(&d.name) {
            Some(DimBinding::Len(n)) => *n,
            Some(DimBinding::Discover(_)) => {
                let candidates: Vec<String> = interface
                    .variables
                    .iter()
                    .filter(|v| v.shape.len() == 1 && v.shape[0] == Dim::Symbol(d.name.clone()))
                    .map(|v| signal_name_of(inst, v))
                    .collect();
                if candidates.is_empty() {
                    return Err(err(
                        "`from-server` needs a one-dimensional variable of this dimension".into(),
                    ));
                }
                let mut found = None;
                'search: for signal in &candidates {
                    for c in connectors.iter_mut() {
                        if let Some(n) = c.discover_len(signal).await? {
                            found = Some(n);
                            break 'search;
                        }
                    }
                }
                found.ok_or_else(|| {
                    err(format!(
                        "no connector knows a length for any of {}",
                        candidates.join(", ")
                    ))
                })?
            }
            None => d
                .default
                .ok_or_else(|| err("unbound and the model declares no default".into()))?,
        };
        if d.min.is_some_and(|min| len < min) || d.max.is_some_and(|max| len > max) {
            return Err(err(format!(
                "{len} is outside {}..={}",
                d.min.map_or("0".to_owned(), |m| m.to_string()),
                d.max.map_or("∞".to_owned(), |m| m.to_string())
            )));
        }
        dims.insert(d.name.clone(), len);
    }
    Ok(dims)
}

/// The signal a variable of `inst` maps to, or its default name.
fn signal_name_of(inst: &InstanceConfig, var: &Variable) -> String {
    match var.causality {
        Causality::Input => inst.inputs.get(&var.name).map(|b| b.signal().to_owned()),
        Causality::Output => inst.outputs.get(&var.name).cloned(),
        Causality::Tunable => inst.tunables.get(&var.name).cloned(),
        Causality::Parameter => None,
    }
    .unwrap_or_else(|| default_name(&inst.id, &var.name))
}

/// The shape of `var` with every symbol bound.
fn bound_shape(var: &Variable, dims: &BoundDims) -> Vec<usize> {
    var.shape
        .iter()
        .map(|d| match d {
            Dim::Literal(n) => *n,
            // Unknown symbols are refused by `check_interface` before this runs.
            Dim::Symbol(s) => dims.get(s).copied().unwrap_or(0),
        })
        .collect()
}

/// Type every `parameters` entry against its variable.
fn convert_params(
    inst: &InstanceConfig,
    interface: &ModelInterface,
    dims: &BoundDims,
) -> Result<ParamValues, PlanError> {
    let mut params = ParamValues::new();
    for (name, value) in &inst.parameters {
        let Some(var) = interface.variables.iter().find(|v| &v.name == name) else {
            continue; // refused by `check_bindings`
        };
        let shape = bound_shape(var, dims);
        let buffer = param_buffer(value, var.ty, &shape, var.layout).map_err(|detail| {
            PlanError::Variable {
                instance: inst.id.clone(),
                name: name.clone(),
                detail,
            }
        })?;
        params.insert(name.clone(), buffer);
    }
    Ok(params)
}

/// Convert a TOML value into a buffer of `ty` and `shape`.
///
/// A scalar takes a number or boolean. An array takes nested arrays, one level per dimension
/// with the first index outermost, stored in `layout`; or one flat array of the full length,
/// taken as already stored in `layout`. Integers convert to floats; floats never to integers.
/// A one-dimensional `u8` variable also takes a string: its bytes, NUL-padded to the length,
/// which must leave room for the terminating NUL.
///
/// # Errors
/// A description of the first element that does not fit.
pub fn param_buffer(
    value: &toml::Value,
    ty: ScalarType,
    shape: &[usize],
    layout: Layout,
) -> Result<Buffer, String> {
    if let (toml::Value::String(text), ScalarType::U8, [capacity]) = (value, ty, shape) {
        let bytes = text.as_bytes();
        if bytes.len() >= *capacity {
            return Err(format!(
                "text of {} bytes does not fit {capacity} with its terminating NUL",
                bytes.len()
            ));
        }
        let mut out = bytes.to_vec();
        out.resize(*capacity, 0);
        return Ok(Buffer::U8(out));
    }
    let mut nums = Vec::new();
    let nested = collect(value, shape, &mut nums)?;
    if nested && layout == Layout::ColumnMajor && shape.len() > 1 {
        nums = to_column_major(&nums, shape);
    }
    build(ty, &nums)
}

/// A TOML scalar.
#[derive(Debug, Clone, Copy)]
enum Num {
    I(i64),
    F(f64),
    B(bool),
}

impl Num {
    fn of(value: &toml::Value) -> Result<Self, String> {
        match value {
            toml::Value::Integer(i) => Ok(Self::I(*i)),
            toml::Value::Float(f) => Ok(Self::F(*f)),
            toml::Value::Boolean(b) => Ok(Self::B(*b)),
            other => Err(format!(
                "expected a number or boolean, got {}",
                other.type_str()
            )),
        }
    }

    fn float(self) -> Result<f64, String> {
        match self {
            Self::I(i) => Ok(i as f64),
            Self::F(f) => Ok(f),
            Self::B(_) => Err("expected a number, got a boolean".into()),
        }
    }

    fn int<T: TryFrom<i64>>(self, ty: ScalarType) -> Result<T, String> {
        match self {
            Self::I(i) => T::try_from(i).map_err(|_| format!("{i} does not fit {ty:?}")),
            Self::F(f) => Err(format!("expected an integer for {ty:?}, got {f}")),
            Self::B(_) => Err(format!("expected an integer for {ty:?}, got a boolean")),
        }
    }

    fn boolean(self) -> Result<bool, String> {
        match self {
            Self::B(b) => Ok(b),
            Self::I(i) => Err(format!("expected a boolean, got {i}")),
            Self::F(f) => Err(format!("expected a boolean, got {f}")),
        }
    }
}

/// Flatten `value` against `shape` in first-index-outermost order; `true` when it was nested.
fn collect(value: &toml::Value, shape: &[usize], out: &mut Vec<Num>) -> Result<bool, String> {
    let Some((&first, rest)) = shape.split_first() else {
        out.push(Num::of(value)?);
        return Ok(false);
    };
    let toml::Value::Array(items) = value else {
        return Err(format!(
            "expected an array of {first}, got {}",
            value.type_str()
        ));
    };
    let total: usize = shape.iter().product();
    let flat = !rest.is_empty()
        && items.len() == total
        && items.iter().all(|v| !matches!(v, toml::Value::Array(_)));
    if flat {
        for item in items {
            out.push(Num::of(item)?);
        }
        return Ok(false);
    }
    if items.len() != first {
        return Err(format!("expected {first} elements, got {}", items.len()));
    }
    for item in items {
        collect(item, rest, out)?;
    }
    Ok(true)
}

/// Reorder a row-major flattening into column-major.
fn to_column_major(nums: &[Num], shape: &[usize]) -> Vec<Num> {
    let n = nums.len();
    let mut out = Vec::with_capacity(n);
    let mut idx = vec![0usize; shape.len()];
    for _ in 0..n {
        // Row-major offset of the multi-index `idx`.
        let mut row = 0;
        for (i, &len) in idx.iter().zip(shape) {
            row = row * len + i;
        }
        out.push(nums[row]);
        // Advance `idx` in column-major order: the first index varies fastest.
        for (i, &len) in idx.iter_mut().zip(shape) {
            *i += 1;
            if *i < len {
                break;
            }
            *i = 0;
        }
    }
    out
}

fn ints<T: TryFrom<i64>>(ty: ScalarType, nums: &[Num]) -> Result<Vec<T>, String> {
    nums.iter().map(|n| n.int(ty)).collect()
}

fn build(ty: ScalarType, nums: &[Num]) -> Result<Buffer, String> {
    Ok(match ty {
        ScalarType::F64 => Buffer::F64(nums.iter().map(|n| n.float()).collect::<Result<_, _>>()?),
        ScalarType::F32 => Buffer::F32(
            nums.iter()
                .map(|n| n.float().map(|f| f as f32))
                .collect::<Result<_, _>>()?,
        ),
        ScalarType::I64 => Buffer::I64(ints(ty, nums)?),
        ScalarType::I32 => Buffer::I32(ints(ty, nums)?),
        ScalarType::I16 => Buffer::I16(ints(ty, nums)?),
        ScalarType::I8 => Buffer::I8(ints(ty, nums)?),
        ScalarType::U64 => Buffer::U64(ints(ty, nums)?),
        ScalarType::U32 => Buffer::U32(ints(ty, nums)?),
        ScalarType::U16 => Buffer::U16(ints(ty, nums)?),
        ScalarType::U8 => Buffer::U8(ints(ty, nums)?),
        ScalarType::Bool => {
            Buffer::Bool(nums.iter().map(|n| n.boolean()).collect::<Result<_, _>>()?)
        }
    })
}

/// The signals found so far, in id order, with how each is used.
struct SignalTable {
    specs: Vec<SignalSpec>,
    by_name: BTreeMap<String, usize>,
    /// Whether the external input's `max_age` was set explicitly by some consumer.
    explicit_age: Vec<bool>,
    system: SystemSignals,
}

impl SignalTable {
    fn new(prefix: &str) -> Self {
        let sys = |name: &str, ty| SignalSpec {
            name: format!("{prefix}.{name}"),
            ty,
            shape: Vec::new(),
            layout: Layout::RowMajor,
            direction: Direction::System,
            max_age: None,
        };
        let specs = vec![
            sys("heartbeat", ScalarType::U64),
            sys("status", ScalarType::I32),
            sys("cycle", ScalarType::U64),
            sys("overruns", ScalarType::U64),
            sys("stale", ScalarType::U64),
        ];
        let by_name = specs
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i))
            .collect();
        Self {
            explicit_age: vec![false; specs.len()],
            specs,
            by_name,
            system: SystemSignals {
                heartbeat: SignalId(0),
                status: SignalId(1),
                cycle: SignalId(2),
                overruns: SignalId(3),
                stale: SignalId(4),
            },
        }
    }

    fn push(&mut self, spec: SignalSpec, explicit_age: bool) -> Result<SignalId, PlanError> {
        let i = self.specs.len();
        let id = SignalId(u32::try_from(i).map_err(|_| ImageError::TooMany)?);
        self.by_name.insert(spec.name.clone(), i);
        self.specs.push(spec);
        self.explicit_age.push(explicit_age);
        Ok(id)
    }

    fn id_of(&self, name: &str) -> Result<SignalId, PlanError> {
        self.by_name
            .get(name)
            .and_then(|&i| u32::try_from(i).ok())
            .map(SignalId)
            .ok_or_else(|| PlanError::Signal {
                name: name.to_owned(),
                detail: "unknown".into(),
            })
    }

    /// Type, shape and layout of `var` match the existing signal `i`.
    fn check_same(&self, i: usize, var: &Variable, dims: &BoundDims) -> Result<(), PlanError> {
        let spec = &self.specs[i];
        let shape = bound_shape(var, dims);
        if spec.ty != var.ty
            || spec.shape != shape
            || (shape.len() > 1 && spec.layout != var.layout)
        {
            return Err(PlanError::Signal {
                name: spec.name.clone(),
                detail: format!(
                    "used as {:?}{:?} {:?} and as {:?}{:?} {:?}",
                    spec.ty, spec.shape, spec.layout, var.ty, shape, var.layout
                ),
            });
        }
        Ok(())
    }

    fn output(
        &mut self,
        name: &str,
        var: &Variable,
        dims: &BoundDims,
    ) -> Result<SignalId, PlanError> {
        if let Some(&i) = self.by_name.get(name) {
            let detail = match self.specs[i].direction {
                Direction::Output => "two outputs write it",
                Direction::System => "reserved for the engine",
                Direction::Input | Direction::Tunable => "already used otherwise",
            };
            return Err(PlanError::Signal {
                name: name.to_owned(),
                detail: detail.into(),
            });
        }
        self.push(
            SignalSpec {
                name: name.to_owned(),
                ty: var.ty,
                shape: bound_shape(var, dims),
                layout: var.layout,
                direction: Direction::Output,
                max_age: None,
            },
            false,
        )
    }

    fn input(
        &mut self,
        name: &str,
        var: &Variable,
        dims: &BoundDims,
        max_age: Option<Duration>,
        explicit_age: bool,
    ) -> Result<SignalId, PlanError> {
        if let Some(&i) = self.by_name.get(name) {
            let err = |detail: String| PlanError::Signal {
                name: name.to_owned(),
                detail,
            };
            match self.specs[i].direction {
                Direction::Output => {
                    if explicit_age {
                        return Err(err("wired to an output; a max_age_ms does not apply".into()));
                    }
                }
                Direction::Input => {
                    // Two consumers of one external input must agree on its age limit when
                    // both set one; a single explicit limit wins over the default.
                    let same = self.specs[i].max_age == max_age;
                    match (self.explicit_age[i], explicit_age) {
                        (true, true) if !same => {
                            return Err(err("consumers disagree on max_age_ms".into()));
                        }
                        (false, true) => {
                            self.specs[i].max_age = max_age;
                            self.explicit_age[i] = true;
                        }
                        _ => {}
                    }
                }
                Direction::Tunable => return Err(err("used as a tunable and as an input".into())),
                Direction::System => return Err(err("reserved for the engine".into())),
            }
            self.check_same(i, var, dims)?;
            return self.id_of(name);
        }
        self.push(
            SignalSpec {
                name: name.to_owned(),
                ty: var.ty,
                shape: bound_shape(var, dims),
                layout: var.layout,
                direction: Direction::Input,
                max_age,
            },
            explicit_age,
        )
    }

    fn tunable(
        &mut self,
        name: &str,
        var: &Variable,
        dims: &BoundDims,
    ) -> Result<SignalId, PlanError> {
        if let Some(&i) = self.by_name.get(name) {
            if self.specs[i].direction != Direction::Tunable {
                return Err(PlanError::Signal {
                    name: name.to_owned(),
                    detail: "used as a tunable and as an input or output".into(),
                });
            }
            self.check_same(i, var, dims)?;
            return self.id_of(name);
        }
        self.push(
            SignalSpec {
                name: name.to_owned(),
                ty: var.ty,
                shape: bound_shape(var, dims),
                layout: var.layout,
                direction: Direction::Tunable,
                max_age: None,
            },
            false,
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::connector::BoxFuture;
    use crate::image::ImageHandle;
    use crate::model::Dimension;

    fn var(name: &str, causality: Causality, ty: ScalarType, shape: &[Dim]) -> Variable {
        Variable {
            name: name.into(),
            causality,
            ty,
            shape: shape.to_vec(),
            layout: Layout::RowMajor,
            unit: None,
            description: None,
        }
    }

    fn sym(s: &str) -> Dim {
        Dim::Symbol(s.into())
    }

    /// `pid`: inputs y[n], outputs u[n], parameter kp, tunable sp[n]; dimension n (1..=8, default 1).
    fn pid() -> ModelInterface {
        ModelInterface {
            name: "pid".into(),
            dimensions: vec![Dimension {
                name: "n".into(),
                min: Some(1),
                max: Some(8),
                default: Some(1),
            }],
            variables: vec![
                var("y", Causality::Input, ScalarType::F64, &[sym("n")]),
                var("u", Causality::Output, ScalarType::F64, &[sym("n")]),
                var("kp", Causality::Parameter, ScalarType::F64, &[]),
                var("sp", Causality::Tunable, ScalarType::F64, &[sym("n")]),
            ],
            instances: Default::default(),
        }
    }

    fn interfaces() -> BTreeMap<String, ModelInterface> {
        let mut m = BTreeMap::new();
        m.insert("pid".to_string(), pid());
        m
    }

    fn project(toml: &str) -> Project {
        toml.parse().expect("project parses")
    }

    const BASE: &str = r#"
[engine]
tick_ms = 10.0
[models.pid]
kind = "raw"
path = "models/pid"
"#;

    async fn resolve(toml: &str) -> Result<Plan, PlanError> {
        let p = project(&format!("{BASE}{toml}"));
        Plan::resolve(&p, &interfaces(), &mut []).await
    }

    #[tokio::test]
    async fn unmapped_variables_get_instance_dot_name() {
        let plan = resolve(
            r#"
[[instance]]
id = "a"
model = "pid"
"#,
        )
        .await
        .unwrap();
        let names: Vec<_> = plan.layout.iter().map(|(_, s)| s.name.clone()).collect();
        assert_eq!(
            names,
            [
                "taktwerk.heartbeat",
                "taktwerk.status",
                "taktwerk.cycle",
                "taktwerk.overruns",
                "taktwerk.stale",
                "a.u",
                "a.y",
                "a.sp",
            ]
        );
        let y = plan.layout.spec(plan.layout.id("a.y").unwrap()).unwrap();
        assert_eq!(y.direction, Direction::Input);
        assert_eq!(y.shape, [1]);
        assert_eq!(y.max_age, None);
        let inst = &plan.instances[0];
        assert_eq!(inst.inputs[0].name, "y");
        assert_eq!(inst.inputs[0].signal, plan.layout.id("a.y").unwrap());
        assert_eq!(inst.outputs[0].signal, plan.layout.id("a.u").unwrap());
        assert_eq!(inst.tunables[0].signal, plan.layout.id("a.sp").unwrap());
        assert_eq!(inst.spec.dims["n"], 1);
        assert!((inst.spec.step_size - 0.01).abs() < 1e-12);
    }

    #[tokio::test]
    async fn an_input_on_another_instances_output_is_wired() {
        let plan = resolve(
            r#"
[[instance]]
id = "a"
model = "pid"
inputs = { y = "b.u" }
[[instance]]
id = "b"
model = "pid"
inputs = { y = { signal = "meas", max_age_ms = 50 } }
outputs = { u = "b.u" }
"#,
        )
        .await
        .unwrap();
        let id = plan.layout.id("b.u").unwrap();
        assert_eq!(plan.layout.spec(id).unwrap().direction, Direction::Output);
        assert_eq!(plan.instances[0].inputs[0].signal, id);
        assert_eq!(plan.instances[1].outputs[0].signal, id);
        assert!(plan.layout.id("a.y").is_none());
        let meas = plan.layout.spec(plan.layout.id("meas").unwrap()).unwrap();
        assert_eq!(meas.direction, Direction::Input);
        assert_eq!(meas.max_age, Some(Duration::from_millis(50)));
    }

    #[tokio::test]
    async fn two_outputs_on_one_signal_is_an_error() {
        let err = resolve(
            r#"
[[instance]]
id = "a"
model = "pid"
outputs = { u = "x" }
[[instance]]
id = "b"
model = "pid"
outputs = { u = "x" }
"#,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, PlanError::Signal { ref name, .. } if name == "x"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn shape_conflict_on_a_shared_signal_is_an_error() {
        let err = resolve(
            r#"
[[instance]]
id = "a"
model = "pid"
dims = { n = 2 }
inputs = { y = "b.u" }
[[instance]]
id = "b"
model = "pid"
dims = { n = 3 }
"#,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, PlanError::Signal { .. }), "{err}");
    }

    #[tokio::test]
    async fn validation_errors_are_named() {
        let p = project(&format!(
            "{BASE}[[instance]]\nid = \"a\"\nmodel = \"pid\"\ndims = {{ n = 9 }}\n"
        ));
        let err = Plan::resolve(&p, &interfaces(), &mut []).await.unwrap_err();
        assert!(
            matches!(err, PlanError::Dimension { ref dim, .. } if dim == "n"),
            "{err}"
        );

        let p = project(&format!(
            "{BASE}[[instance]]\nid = \"a\"\nmodel = \"pid\"\ninputs = {{ nope = \"x\" }}\n"
        ));
        let err = Plan::resolve(&p, &interfaces(), &mut []).await.unwrap_err();
        assert!(
            matches!(err, PlanError::Variable { ref name, .. } if name == "nope"),
            "{err}"
        );

        let p = project(&format!(
            "{BASE}[[instance]]\nid = \"a\"\nmodel = \"pid\"\nparameters = {{ kp = \"x\" }}\n"
        ));
        let err = Plan::resolve(&p, &interfaces(), &mut []).await.unwrap_err();
        assert!(
            matches!(err, PlanError::Variable { ref name, .. } if name == "kp"),
            "{err}"
        );

        let p = project(&format!(
            "{BASE}[[instance]]\nid = \"a\"\nmodel = \"pid\"\n"
        ));
        let err = Plan::resolve(&p, &BTreeMap::new(), &mut [])
            .await
            .unwrap_err();
        assert!(matches!(err, PlanError::NoInterface(_)), "{err}");

        for bad in [
            "[[instance]]\nid = \"a\"\nmodel = \"nope\"\n",
            "[[instance]]\nid = \"a\"\nmodel = \"pid\"\n[[instance]]\nid = \"a\"\nmodel = \"pid\"\n",
            "[[instance]]\nid = \"a\"\nmodel = \"pid\"\nevery = 0\n",
        ] {
            let err = format!("{BASE}{bad}").parse::<Project>().unwrap_err();
            assert!(matches!(err, ProjectError::Invalid(_)), "{err}");
        }
        let err = "[engine]\ntick_ms = 0\n".parse::<Project>().unwrap_err();
        assert!(matches!(err, ProjectError::Invalid(_)), "{err}");
    }

    struct Knows(&'static str, usize);

    impl Connector for Knows {
        fn id(&self) -> &str {
            "k"
        }
        fn discover_len<'a>(
            &'a mut self,
            signal: &'a str,
        ) -> BoxFuture<'a, Result<Option<usize>, ConnectorError>> {
            Box::pin(async move { Ok((signal == self.0).then_some(self.1)) })
        }
        fn bind<'a>(&'a mut self, _: &'a ImageLayout) -> BoxFuture<'a, Result<(), ConnectorError>> {
            Box::pin(async { Ok(()) })
        }
        fn run(
            self: Box<Self>,
            _: ImageHandle,
            _: crate::connector::Shutdown,
        ) -> BoxFuture<'static, Result<(), ConnectorError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn from_server_asks_connectors_for_a_signal_of_the_dimension() {
        let p = project(&format!(
            "{BASE}[[instance]]\nid = \"a\"\nmodel = \"pid\"\ndims = {{ n = \"from-server\" }}\noutputs = {{ u = \"plc.u\" }}\n"
        ));
        let mut connectors: Vec<Box<dyn Connector>> =
            vec![Box::new(Knows("nothing", 1)), Box::new(Knows("plc.u", 4))];
        let plan = Plan::resolve(&p, &interfaces(), &mut connectors)
            .await
            .unwrap();
        assert_eq!(plan.instances[0].spec.dims["n"], 4);
        assert_eq!(
            plan.layout
                .spec(plan.layout.id("a.y").unwrap())
                .unwrap()
                .shape,
            [4]
        );

        let mut none: Vec<Box<dyn Connector>> = vec![Box::new(Knows("nothing", 1))];
        let err = Plan::resolve(&p, &interfaces(), &mut none)
            .await
            .unwrap_err();
        assert!(matches!(err, PlanError::Dimension { .. }), "{err}");
    }

    #[test]
    fn parameters_convert_by_type_shape_and_layout() {
        let v: toml::Value = "x = 2".parse::<toml::Table>().unwrap().remove("x").unwrap();
        assert_eq!(
            param_buffer(&v, ScalarType::F64, &[], Layout::RowMajor).unwrap(),
            Buffer::F64(vec![2.0])
        );
        assert_eq!(
            param_buffer(&v, ScalarType::U8, &[], Layout::RowMajor).unwrap(),
            Buffer::U8(vec![2])
        );
        let v: toml::Value = "x = 2.5"
            .parse::<toml::Table>()
            .unwrap()
            .remove("x")
            .unwrap();
        assert!(param_buffer(&v, ScalarType::I32, &[], Layout::RowMajor).is_err());
        let v: toml::Value = "x = 300"
            .parse::<toml::Table>()
            .unwrap()
            .remove("x")
            .unwrap();
        assert!(param_buffer(&v, ScalarType::U8, &[], Layout::RowMajor).is_err());
        let v: toml::Value = "x = true"
            .parse::<toml::Table>()
            .unwrap()
            .remove("x")
            .unwrap();
        assert_eq!(
            param_buffer(&v, ScalarType::Bool, &[], Layout::RowMajor).unwrap(),
            Buffer::Bool(vec![true])
        );
        assert!(param_buffer(&v, ScalarType::F64, &[], Layout::RowMajor).is_err());

        let m: toml::Value = "x = [[1, 2, 3], [4, 5, 6]]"
            .parse::<toml::Table>()
            .unwrap()
            .remove("x")
            .unwrap();
        assert_eq!(
            param_buffer(&m, ScalarType::I32, &[2, 3], Layout::RowMajor).unwrap(),
            Buffer::I32(vec![1, 2, 3, 4, 5, 6])
        );
        assert_eq!(
            param_buffer(&m, ScalarType::I32, &[2, 3], Layout::ColumnMajor).unwrap(),
            Buffer::I32(vec![1, 4, 2, 5, 3, 6])
        );
        assert!(param_buffer(&m, ScalarType::I32, &[3, 2], Layout::RowMajor).is_err());
        let flat: toml::Value = "x = [1, 2, 3, 4, 5, 6]"
            .parse::<toml::Table>()
            .unwrap()
            .remove("x")
            .unwrap();
        assert_eq!(
            param_buffer(&flat, ScalarType::F32, &[2, 3], Layout::ColumnMajor).unwrap(),
            Buffer::F32(vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0])
        );
        assert!(param_buffer(&flat, ScalarType::F32, &[2], Layout::RowMajor).is_err());

        let text: toml::Value = "x = \"tank 3\""
            .parse::<toml::Table>()
            .unwrap()
            .remove("x")
            .unwrap();
        assert_eq!(
            param_buffer(&text, ScalarType::U8, &[8], Layout::RowMajor).unwrap(),
            Buffer::U8(b"tank 3\0\0".to_vec())
        );
        assert!(param_buffer(&text, ScalarType::U8, &[6], Layout::RowMajor).is_err());
        assert!(param_buffer(&text, ScalarType::U8, &[], Layout::RowMajor).is_err());
        assert!(param_buffer(&text, ScalarType::I8, &[8], Layout::RowMajor).is_err());
    }

    #[tokio::test]
    async fn parameters_land_in_the_spec_and_defaults_apply() {
        let plan = resolve(
            r#"
[engine.realtime]
policy = "fifo"
priority = 50
[[instance]]
id = "a"
model = "pid"
dims = { n = 2 }
parameters = { kp = 1.5, sp = [1, 2] }
"#,
        )
        .await;
        let plan = plan.unwrap();
        let spec = &plan.instances[0].spec;
        assert_eq!(spec.params["kp"], Buffer::F64(vec![1.5]));
        assert_eq!(spec.params["sp"], Buffer::F64(vec![1.0, 2.0]));
    }
}
