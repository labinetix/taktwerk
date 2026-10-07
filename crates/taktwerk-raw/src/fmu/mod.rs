//! `taktwerk fmu-wrap`: turn a confirmed raw model package into an FMI 3 co-simulation FMU.
//!
//! [`generate`] is pure: from a validated descriptor it produces `modelDescription.xml` and a C
//! wrapper source that does what the raw adapter does, in C. Buffers and struct images are
//! allocated from the bound structural parameters (a reported dimension at its `max`), members
//! are filled per call (constants, phases, dimension lengths, builtins, the handle), the
//! library is called through function pointers it `dlopen`s itself (a `single` library as a
//! private copy per instance, like the adapter), return codes are checked against `ok_codes`
//! and reported lengths against the bound ones. The structs are emitted as C typedefs so the
//! target compiler lays them out, with a `_Static_assert` per member offset against the layout
//! taktwerk computed.
//!
//! The FMU: one `UInt64` structural parameter per dimension (`start` from `default`, else
//! `min`, else 1), every variable with its causality (`parameter` fixed or tunable, `input`,
//! `output`), arrays with `<Dimension valueReference>` to the structural parameters, text
//! buffers (`u8` with one literal dimension) as `String` variables whose capacity an annotation
//! carries. A library told its step size (`builtin = "step_size"`) is initialised on the first
//! `fmi3DoStep`, which fixes the communication step; the others at
//! `fmi3ExitInitializationMode`.
//!
//! [`build`](build::build) does the I/O: compiles the wrapper for each target, packs it with the
//! library into the `.fmu`.

pub mod build;

use std::fmt::Write as _;

use taktwerk_core::model::{Causality, Instances, Variable};
use taktwerk_core::value::{Dim, ScalarType};

use crate::descriptor::{
    ArgRole, Builtin, CValue, Descriptor, Handle, MemberRole, Phased, Plan, ResolvedCall, Returns,
};
use crate::layout::Carrier;

/// The vendored FMI 3.0.2 headers (BSD-2-Clause, Modelica Association Project "FMI").
pub const FMI3_HEADERS: &[(&str, &str)] = &[
    (
        "fmi3Functions.h",
        include_str!("../../fmi3/fmi3Functions.h"),
    ),
    (
        "fmi3FunctionTypes.h",
        include_str!("../../fmi3/fmi3FunctionTypes.h"),
    ),
    (
        "fmi3PlatformTypes.h",
        include_str!("../../fmi3/fmi3PlatformTypes.h"),
    ),
    ("LICENSE.txt", include_str!("../../fmi3/LICENSE.txt")),
];

/// Default capacity of a `String` variable without a capacity annotation, bytes.
pub const DEFAULT_TEXT_CAPACITY: usize = 256;

/// A descriptor the wrapper cannot be generated for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("fmu-wrap: {0}")]
pub struct WrapError(pub String);

/// What a variable became in the FMU, for the dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedVariable {
    /// Variable name.
    pub name: String,
    /// Value reference.
    pub vr: u32,
    /// FMI type element (`Float64`, `String`, …).
    pub fmi_type: String,
    /// `parameter (fixed)`, `parameter (tunable)`, `input`, `output`.
    pub causality: String,
    /// Shape, as the descriptor declares it.
    pub shape: String,
}

/// A structural parameter of the FMU, for the dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedStructural {
    /// Dimension name.
    pub name: String,
    /// Value reference.
    pub vr: u32,
    /// `start`.
    pub start: usize,
    /// `min`, if declared.
    pub min: Option<usize>,
    /// `max`, if declared.
    pub max: Option<usize>,
}

/// The generated FMU contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapper {
    /// `modelIdentifier`: the binary and the C source are named after it.
    pub model_identifier: String,
    /// `instantiationToken`, derived from the descriptor.
    pub token: String,
    /// `modelDescription.xml`.
    pub model_description: String,
    /// The C wrapper source.
    pub source: String,
    /// Variables as wrapped.
    pub variables: Vec<WrappedVariable>,
    /// Structural parameters.
    pub structural: Vec<WrappedStructural>,
    /// `canHandleVariableCommunicationStepSize`: false when the library is told its step.
    pub variable_step: bool,
    /// Whether `fmi3Reset` is supported (the descriptor has a terminate call).
    pub resettable: bool,
}

/// Generation options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WrapOptions {
    /// File name of the library inside `binaries/<platform>/` on the host target (a build for
    /// another target overrides it with `-DMODEL_LIBRARY`).
    pub library: String,
    /// File names of further libraries in `binaries/<platform>/` the wrapper loads first.
    pub bundled: Vec<String>,
}

/// A C identifier from a model name: alphanumerics kept, the rest `_`.
#[must_use]
pub fn model_identifier(name: &str) -> String {
    let mut id: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if id.is_empty() {
        id.push_str("model");
    }
    if id.starts_with(|c: char| c.is_ascii_digit()) {
        id.insert_str(0, "m_");
    }
    id
}

/// FNV-1a over the descriptor text: the same descriptor gives the same token.
fn token_of(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("taktwerk-{h:016x}")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// C string literal contents (ASCII kept, the rest escaped).
fn c_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            c => {
                for b in c.to_string().as_bytes() {
                    let _ = write!(out, "\\x{b:02x}");
                }
            }
        }
    }
    out
}

const fn c_type(ty: ScalarType) -> &'static str {
    match ty {
        ScalarType::F64 => "double",
        ScalarType::F32 => "float",
        ScalarType::I64 => "int64_t",
        ScalarType::I32 => "int32_t",
        ScalarType::I16 => "int16_t",
        ScalarType::I8 => "int8_t",
        ScalarType::U64 => "uint64_t",
        ScalarType::U32 => "uint32_t",
        ScalarType::U16 => "uint16_t",
        ScalarType::U8 => "uint8_t",
        ScalarType::Bool => "bool",
    }
}

/// The C type of a variable's storage: bytes for `bool`, as in the adapter.
const fn storage_type(ty: ScalarType) -> &'static str {
    match ty {
        ScalarType::Bool => "uint8_t",
        other => c_type(other),
    }
}

const fn fmi_type(ty: ScalarType) -> &'static str {
    match ty {
        ScalarType::F64 => "Float64",
        ScalarType::F32 => "Float32",
        ScalarType::I64 => "Int64",
        ScalarType::I32 => "Int32",
        ScalarType::I16 => "Int16",
        ScalarType::I8 => "Int8",
        ScalarType::U64 => "UInt64",
        ScalarType::U32 => "UInt32",
        ScalarType::U16 => "UInt16",
        ScalarType::U8 => "UInt8",
        ScalarType::Bool => "Boolean",
    }
}

/// Numeric type ids of the generated `VAR_TYPE` table, matching the Get/Set functions.
const fn type_id(ty: ScalarType) -> u8 {
    match ty {
        ScalarType::F64 => 0,
        ScalarType::F32 => 1,
        ScalarType::I64 => 2,
        ScalarType::I32 => 3,
        ScalarType::I16 => 4,
        ScalarType::I8 => 5,
        ScalarType::U64 => 6,
        ScalarType::U32 => 7,
        ScalarType::U16 => 8,
        ScalarType::U8 => 9,
        ScalarType::Bool => 10,
    }
}

/// Type id of a text variable (`String`).
const TEXT_TYPE: u8 = 11;

/// A `u8` variable with exactly one literal dimension travels as text.
fn is_text(v: &Variable) -> bool {
    v.ty == ScalarType::U8 && matches!(v.shape.as_slice(), [Dim::Literal(_)])
}

/// Capacity of a text variable.
fn text_capacity(v: &Variable) -> usize {
    match v.shape.as_slice() {
        [Dim::Literal(n)] => *n,
        _ => 0,
    }
}

/// A `const`/`phase` value as a C expression of its type.
fn literal(v: CValue) -> Result<String, WrapError> {
    let b = v.bits;
    let text = match v.ty {
        ScalarType::F64 => {
            let f = f64::from_bits(b);
            if !f.is_finite() {
                return Err(WrapError(format!("a constant of {f} cannot be emitted")));
            }
            format!("{f:?}")
        }
        ScalarType::F32 => {
            let f = f32::from_bits(b as u32);
            if !f.is_finite() {
                return Err(WrapError(format!("a constant of {f} cannot be emitted")));
            }
            format!("{f:?}f")
        }
        ScalarType::I64 => format!("INT64_C({})", b as i64),
        ScalarType::I32 => format!("{}", b as i32),
        ScalarType::I16 => format!("{}", b as i16),
        ScalarType::I8 => format!("{}", b as i8),
        ScalarType::U64 => format!("UINT64_C({b})"),
        ScalarType::U32 => format!("{}u", b as u32),
        ScalarType::U16 => format!("{}", b as u16),
        ScalarType::U8 => format!("{}", b as u8),
        ScalarType::Bool => (if b != 0 { "true" } else { "false" }).to_owned(),
    };
    Ok(format!("(({}){text})", c_type(v.ty)))
}

fn phased_literal(p: Phased, init: bool) -> Result<String, WrapError> {
    literal(if init { p.init } else { p.step })
}

/// Everything the generator looks up by index.
struct Ctx<'a> {
    d: &'a Descriptor,
    plan: &'a Plan,
    n_dims: usize,
}

impl Ctx<'_> {
    fn vars(&self) -> &[Variable] {
        &self.d.interface.variables
    }

    const fn var_vr(&self, idx: usize) -> usize {
        1 + self.n_dims + idx
    }

    fn uses_step_size(&self) -> bool {
        let in_call = |c: &ResolvedCall| {
            c.args
                .iter()
                .any(|a| matches!(a, ArgRole::Builtin(Builtin::StepSize)))
        };
        in_call(&self.plan.init)
            || in_call(&self.plan.step)
            || self.plan.terminate.as_ref().is_some_and(in_call)
            || self.plan.structs.iter().any(|s| {
                s.members
                    .iter()
                    .any(|m| matches!(m, MemberRole::Builtin(Builtin::StepSize)))
            })
    }

    /// `(bound length, allocated length)` of variable `idx` as C expressions.
    fn lengths(&self, idx: usize) -> (String, String) {
        let v = &self.vars()[idx];
        let (mut len, mut cap) = (Vec::new(), Vec::new());
        for dim in &v.shape {
            match dim {
                Dim::Literal(n) => {
                    len.push(format!("(size_t){n}"));
                    cap.push(format!("(size_t){n}"));
                }
                Dim::Symbol(s) => {
                    let i = self
                        .d
                        .interface
                        .dimensions
                        .iter()
                        .position(|d| &d.name == s)
                        .unwrap_or(0);
                    len.push(format!("(size_t)m->dims[{i}]"));
                    if self.plan.max_sized.get(i).copied().unwrap_or(false) {
                        cap.push(format!("dim_cap(m, {i})"));
                    } else {
                        cap.push(format!("(size_t)m->dims[{i}]"));
                    }
                }
            }
        }
        if len.is_empty() {
            return ("(size_t)1".to_owned(), "(size_t)1".to_owned());
        }
        (len.join(" * "), cap.join(" * "))
    }

    /// The C parameter type of an argument.
    fn arg_type(&self, a: &ArgRole) -> &'static str {
        match *a {
            ArgRole::Struct(_) | ArgRole::Array(_) | ArgRole::Handle(Handle::In) => "void *",
            ArgRole::Handle(Handle::Out) => "void **",
            ArgRole::Value(v) => c_type(self.vars()[v].ty),
            ArgRole::Dim(_, ty) => c_type(ty),
            ArgRole::Builtin(_) => "double",
            ArgRole::Fixed { cell: Some(_), .. } => "void *",
            ArgRole::Fixed { value, cell: None } => c_type(value.init.ty),
        }
    }

    /// The C expression passed for an argument in the given phase.
    fn arg_expr(&self, a: &ArgRole, init: bool) -> Result<String, WrapError> {
        Ok(match *a {
            ArgRole::Struct(i) => format!("&m->s{i}"),
            ArgRole::Array(v) => format!("m->v{v}"),
            ArgRole::Value(v) => {
                if self.vars()[v].ty == ScalarType::Bool {
                    format!("(bool)(m->v{v}[0] != 0)")
                } else {
                    format!("m->v{v}[0]")
                }
            }
            ArgRole::Dim(d, ty) => format!("({})m->dims[{d}]", c_type(ty)),
            ArgRole::Builtin(Builtin::StepSize) => "m->step_size".to_owned(),
            ArgRole::Builtin(Builtin::Time) => "time".to_owned(),
            ArgRole::Handle(Handle::Out) => "&m->handle".to_owned(),
            ArgRole::Handle(Handle::In) => "m->handle".to_owned(),
            ArgRole::Fixed { cell: Some(c), .. } => format!("&m->c{c}"),
            ArgRole::Fixed { value, cell: None } => phased_literal(value, init)?,
        })
    }
}

/// Generate the FMU for a validated descriptor.
///
/// # Errors
/// A constant that cannot be emitted, or two calls on one symbol with different prototypes.
pub fn generate(d: &Descriptor, plan: &Plan, options: &WrapOptions) -> Result<Wrapper, WrapError> {
    if d.interface.variables.is_empty() {
        return Err(WrapError("the model declares no variable".to_owned()));
    }
    let ctx = Ctx {
        d,
        plan,
        n_dims: d.interface.dimensions.len(),
    };
    let text = d.to_toml().map_err(|e| WrapError(e.to_string()))?;
    let token = token_of(&text);
    let model_identifier = model_identifier(&d.interface.name);
    let variable_step = !ctx.uses_step_size();
    let resettable = plan.terminate.is_some();

    let structural: Vec<WrappedStructural> = d
        .interface
        .dimensions
        .iter()
        .enumerate()
        .map(|(i, dim)| WrappedStructural {
            name: dim.name.clone(),
            vr: (1 + i) as u32,
            start: dim.default.or(dim.min).unwrap_or(1),
            min: dim.min,
            max: dim.max,
        })
        .collect();
    let variables: Vec<WrappedVariable> = ctx
        .vars()
        .iter()
        .enumerate()
        .map(|(i, v)| WrappedVariable {
            name: v.name.clone(),
            vr: ctx.var_vr(i) as u32,
            fmi_type: if is_text(v) {
                "String".to_owned()
            } else {
                fmi_type(v.ty).to_owned()
            },
            causality: match v.causality {
                Causality::Input => "input",
                Causality::Output => "output",
                Causality::Parameter => "parameter (fixed)",
                Causality::Tunable => "parameter (tunable)",
            }
            .to_owned(),
            shape: if v.shape.is_empty() {
                "scalar".to_owned()
            } else {
                let parts: Vec<String> = v
                    .shape
                    .iter()
                    .map(|dim| match dim {
                        Dim::Literal(n) => n.to_string(),
                        Dim::Symbol(s) => s.clone(),
                    })
                    .collect();
                format!("[{}]", parts.join(", "))
            },
        })
        .collect();

    let model_description = xml(&ctx, &model_identifier, &token, &structural, variable_step);
    let source = c_source(&ctx, &token, options, variable_step, resettable)?;
    Ok(Wrapper {
        model_identifier,
        token,
        model_description,
        source,
        variables,
        structural,
        variable_step,
        resettable,
    })
}

// ==========================================================================
// modelDescription.xml
// ==========================================================================

fn xml(
    ctx: &Ctx<'_>,
    model_identifier: &str,
    token: &str,
    structural: &[WrappedStructural],
    variable_step: bool,
) -> String {
    let d = ctx.d;
    let mut x = String::new();
    let _ = writeln!(x, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        x,
        r#"<fmiModelDescription fmiVersion="3.0" modelName="{}" instantiationToken="{}" generationTool="taktwerk fmu-wrap">"#,
        xml_escape(&d.interface.name),
        xml_escape(token)
    );
    let _ = writeln!(
        x,
        r#"  <CoSimulation modelIdentifier="{model_identifier}" canHandleVariableCommunicationStepSize="{variable_step}" canBeInstantiatedOnlyOncePerProcess="{}" canGetAndSetFMUState="false" canSerializeFMUState="false" providesIntermediateUpdate="false" hasEventMode="false"/>"#,
        d.interface.instances == Instances::Single
    );
    let _ = writeln!(x, "  <ModelVariables>");
    let _ = writeln!(
        x,
        r#"    <Float64 name="time" valueReference="0" causality="independent" variability="continuous"/>"#
    );
    for s in structural {
        let _ = write!(
            x,
            r#"    <UInt64 name="{}" valueReference="{}" causality="structuralParameter" variability="fixed" start="{}""#,
            xml_escape(&s.name),
            s.vr,
            s.start
        );
        if let Some(min) = s.min {
            let _ = write!(x, r#" min="{min}""#);
        }
        if let Some(max) = s.max {
            let _ = write!(x, r#" max="{max}""#);
        }
        let _ = writeln!(x, "/>");
    }
    let start_len = |v: &Variable| -> usize {
        v.shape
            .iter()
            .map(|dim| match dim {
                Dim::Literal(n) => *n,
                Dim::Symbol(s) => structural
                    .iter()
                    .find(|st| &st.name == s)
                    .map_or(1, |st| st.start),
            })
            .product()
    };
    for (i, v) in ctx.vars().iter().enumerate() {
        let vr = ctx.var_vr(i);
        let text = is_text(v);
        let tag = if text { "String" } else { fmi_type(v.ty) };
        let (causality, variability) = match v.causality {
            Causality::Input => ("input", None),
            Causality::Output => ("output", None),
            Causality::Parameter => ("parameter", Some("fixed")),
            Causality::Tunable => ("parameter", Some("tunable")),
        };
        let _ = write!(
            x,
            r#"    <{tag} name="{}" valueReference="{vr}" causality="{causality}""#,
            xml_escape(&v.name)
        );
        if let Some(variability) = variability {
            let _ = write!(x, r#" variability="{variability}""#);
        }
        if let Some(desc) = &v.description {
            let _ = write!(x, r#" description="{}""#, xml_escape(desc));
        }
        let needs_start = v.causality != Causality::Output;
        if needs_start && !text {
            let one = match v.ty {
                ScalarType::Bool => "false",
                ScalarType::F64 | ScalarType::F32 => "0.0",
                _ => "0",
            };
            let start: Vec<&str> = std::iter::repeat_n(one, start_len(v).max(1)).collect();
            let _ = write!(x, r#" start="{}""#, start.join(" "));
        }
        let dims: Vec<String> = if text {
            Vec::new()
        } else {
            v.shape
                .iter()
                .map(|dim| match dim {
                    Dim::Literal(n) => format!(r#"      <Dimension start="{n}"/>"#),
                    Dim::Symbol(s) => {
                        let vr = structural
                            .iter()
                            .find(|st| &st.name == s)
                            .map_or(0, |st| st.vr);
                        format!(r#"      <Dimension valueReference="{vr}"/>"#)
                    }
                })
                .collect()
        };
        if dims.is_empty() && !text {
            let _ = writeln!(x, "/>");
            continue;
        }
        let _ = writeln!(x, ">");
        // Schema order: Annotations, Dimension, Start.
        if text {
            let _ = writeln!(
                x,
                r#"      <Annotations><Annotation type="taktwerk"><text capacity="{}"/></Annotation></Annotations>"#,
                text_capacity(v)
            );
        }
        for line in dims {
            let _ = writeln!(x, "{line}");
        }
        if text && needs_start {
            let _ = writeln!(x, r#"      <Start value=""/>"#);
        }
        let _ = writeln!(x, "    </{tag}>");
    }
    let _ = writeln!(x, "  </ModelVariables>");
    let _ = writeln!(x, "  <ModelStructure>");
    for (i, v) in ctx.vars().iter().enumerate() {
        if v.causality == Causality::Output {
            let _ = writeln!(x, r#"    <Output valueReference="{}"/>"#, ctx.var_vr(i));
        }
    }
    for (i, v) in ctx.vars().iter().enumerate() {
        if v.causality == Causality::Output {
            let _ = writeln!(
                x,
                r#"    <InitialUnknown valueReference="{}"/>"#,
                ctx.var_vr(i)
            );
        }
    }
    let _ = writeln!(x, "  </ModelStructure>");
    let _ = writeln!(x, "</fmiModelDescription>");
    x
}

// ==========================================================================
// The C wrapper
// ==========================================================================

/// Every `fmi3*` function the standard declares that this wrapper does not implement, with its
/// parameter list: each is exported and returns `fmi3Error` with a log.
const STUBS: &[(&str, &str)] = &[
    ("fmi3EnterEventMode", "fmi3Instance instance"),
    (
        "fmi3GetBinary",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, size_t sizes[], fmi3Binary values[], size_t nValues",
    ),
    (
        "fmi3GetClock",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, fmi3Clock values[]",
    ),
    (
        "fmi3SetBinary",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const size_t sizes[], const fmi3Binary values[], size_t nValues",
    ),
    (
        "fmi3SetClock",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const fmi3Clock values[]",
    ),
    (
        "fmi3GetNumberOfVariableDependencies",
        "fmi3Instance instance, fmi3ValueReference vr, size_t *n",
    ),
    (
        "fmi3GetVariableDependencies",
        "fmi3Instance instance, fmi3ValueReference dependent, size_t elementIndicesOfDependent[], fmi3ValueReference independents[], size_t elementIndicesOfIndependents[], fmi3DependencyKind kinds[], size_t n",
    ),
    (
        "fmi3GetFMUState",
        "fmi3Instance instance, fmi3FMUState *state",
    ),
    (
        "fmi3SetFMUState",
        "fmi3Instance instance, fmi3FMUState state",
    ),
    (
        "fmi3FreeFMUState",
        "fmi3Instance instance, fmi3FMUState *state",
    ),
    (
        "fmi3SerializedFMUStateSize",
        "fmi3Instance instance, fmi3FMUState state, size_t *size",
    ),
    (
        "fmi3SerializeFMUState",
        "fmi3Instance instance, fmi3FMUState state, fmi3Byte bytes[], size_t size",
    ),
    (
        "fmi3DeserializeFMUState",
        "fmi3Instance instance, const fmi3Byte bytes[], size_t size, fmi3FMUState *state",
    ),
    (
        "fmi3GetDirectionalDerivative",
        "fmi3Instance instance, const fmi3ValueReference unknowns[], size_t nUnknowns, const fmi3ValueReference knowns[], size_t nKnowns, const fmi3Float64 seed[], size_t nSeed, fmi3Float64 sensitivity[], size_t nSensitivity",
    ),
    (
        "fmi3GetAdjointDerivative",
        "fmi3Instance instance, const fmi3ValueReference unknowns[], size_t nUnknowns, const fmi3ValueReference knowns[], size_t nKnowns, const fmi3Float64 seed[], size_t nSeed, fmi3Float64 sensitivity[], size_t nSensitivity",
    ),
    (
        "fmi3GetIntervalDecimal",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, fmi3Float64 intervals[], fmi3IntervalQualifier qualifiers[]",
    ),
    (
        "fmi3GetIntervalFraction",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, fmi3UInt64 counters[], fmi3UInt64 resolutions[], fmi3IntervalQualifier qualifiers[]",
    ),
    (
        "fmi3GetShiftDecimal",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, fmi3Float64 shifts[]",
    ),
    (
        "fmi3GetShiftFraction",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, fmi3UInt64 counters[], fmi3UInt64 resolutions[]",
    ),
    (
        "fmi3SetIntervalDecimal",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const fmi3Float64 intervals[]",
    ),
    (
        "fmi3SetIntervalFraction",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const fmi3UInt64 counters[], const fmi3UInt64 resolutions[]",
    ),
    (
        "fmi3SetShiftDecimal",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const fmi3Float64 shifts[]",
    ),
    (
        "fmi3SetShiftFraction",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const fmi3UInt64 counters[], const fmi3UInt64 resolutions[]",
    ),
    ("fmi3EvaluateDiscreteStates", "fmi3Instance instance"),
    (
        "fmi3UpdateDiscreteStates",
        "fmi3Instance instance, fmi3Boolean *discreteStatesNeedUpdate, fmi3Boolean *terminateSimulation, fmi3Boolean *nominalsChanged, fmi3Boolean *valuesChanged, fmi3Boolean *nextEventTimeDefined, fmi3Float64 *nextEventTime",
    ),
    ("fmi3EnterContinuousTimeMode", "fmi3Instance instance"),
    (
        "fmi3CompletedIntegratorStep",
        "fmi3Instance instance, fmi3Boolean noSetFMUStatePriorToCurrentPoint, fmi3Boolean *enterEventMode, fmi3Boolean *terminateSimulation",
    ),
    ("fmi3SetTime", "fmi3Instance instance, fmi3Float64 time"),
    (
        "fmi3SetContinuousStates",
        "fmi3Instance instance, const fmi3Float64 x[], size_t nx",
    ),
    (
        "fmi3GetContinuousStateDerivatives",
        "fmi3Instance instance, fmi3Float64 dx[], size_t nx",
    ),
    (
        "fmi3GetEventIndicators",
        "fmi3Instance instance, fmi3Float64 z[], size_t nz",
    ),
    (
        "fmi3GetContinuousStates",
        "fmi3Instance instance, fmi3Float64 x[], size_t nx",
    ),
    (
        "fmi3GetNominalsOfContinuousStates",
        "fmi3Instance instance, fmi3Float64 nominals[], size_t nx",
    ),
    (
        "fmi3GetNumberOfEventIndicators",
        "fmi3Instance instance, size_t *nz",
    ),
    (
        "fmi3GetNumberOfContinuousStates",
        "fmi3Instance instance, size_t *nx",
    ),
    ("fmi3EnterStepMode", "fmi3Instance instance"),
    (
        "fmi3GetOutputDerivatives",
        "fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const fmi3Int32 orders[], fmi3Float64 values[], size_t nValues",
    ),
    (
        "fmi3ActivateModelPartition",
        "fmi3Instance instance, fmi3ValueReference clockReference, fmi3Float64 activationTime",
    ),
];

/// Typed Get/Set functions: `(FMI name, fmi3 type, type id)`.
const TYPED: &[(&str, &str, u8)] = &[
    ("Float64", "fmi3Float64", 0),
    ("Float32", "fmi3Float32", 1),
    ("Int64", "fmi3Int64", 2),
    ("Int32", "fmi3Int32", 3),
    ("Int16", "fmi3Int16", 4),
    ("Int8", "fmi3Int8", 5),
    ("UInt32", "fmi3UInt32", 7),
    ("UInt16", "fmi3UInt16", 8),
    ("UInt8", "fmi3UInt8", 9),
];

/// The prototype typedef name of a call.
fn fn_typedef(which: &str) -> String {
    format!("{which}_fn")
}

#[allow(clippy::too_many_lines, reason = "one template, read top to bottom")]
fn c_source(
    ctx: &Ctx<'_>,
    token: &str,
    options: &WrapOptions,
    variable_step: bool,
    resettable: bool,
) -> Result<String, WrapError> {
    let d = ctx.d;
    let plan = ctx.plan;
    let vars = ctx.vars();
    let n_dims = ctx.n_dims;
    let mut c = String::new();
    macro_rules! w {
        ($($arg:tt)*) => {{ let _ = writeln!(c, $($arg)*); }};
    }

    w!(
        "/* {}: FMI 3.0 co-simulation wrapper generated by taktwerk fmu-wrap from {}. Do not edit. */",
        c_escape(&d.interface.name),
        crate::descriptor::DESCRIPTOR_FILE
    );
    w!("#define _GNU_SOURCE");
    w!("#include <dlfcn.h>");
    w!("#include <stdarg.h>");
    w!("#include <stdbool.h>");
    w!("#include <stddef.h>");
    w!("#include <stdint.h>");
    w!("#include <stdio.h>");
    w!("#include <stdlib.h>");
    w!("#include <string.h>");
    w!("#include <unistd.h>");
    w!("#include \"fmi3Functions.h\"");
    w!();
    w!("#define MODEL_NAME \"{}\"", c_escape(&d.interface.name));
    w!("#define MODEL_TOKEN \"{}\"", c_escape(token));
    w!("#ifndef MODEL_LIBRARY");
    w!("#define MODEL_LIBRARY \"{}\"", c_escape(&options.library));
    w!("#endif");
    w!(
        "#define MODEL_SINGLE {}",
        u8::from(d.interface.instances == Instances::Single)
    );
    w!("#define USES_STEP_SIZE {}", u8::from(!variable_step));
    w!("#define HAS_TERMINATE {}", u8::from(resettable));
    w!("#if defined(__aarch64__)");
    w!("#define MODEL_PLATFORM \"aarch64-linux\"");
    w!("#elif defined(__x86_64__)");
    w!("#define MODEL_PLATFORM \"x86_64-linux\"");
    w!("#else");
    w!("#error \"taktwerk wrappers run on aarch64 and x86_64 Linux\"");
    w!("#endif");
    let bundled: Vec<String> = options
        .bundled
        .iter()
        .map(|b| format!("\"{}\"", c_escape(b)))
        .collect();
    w!(
        "static const char *const BUNDLED[] = {{ {}NULL }};",
        bundled.iter().map(|b| format!("{b}, ")).collect::<String>()
    );
    let ok: Vec<String> = d.abi.ok_codes.iter().map(ToString::to_string).collect();
    w!("static const int32_t OK_CODES[] = {{ {} }};", ok.join(", "));
    w!("#define N_OK_CODES {}", d.abi.ok_codes.len());
    w!();

    // The library's structs, laid out by this compiler, checked against taktwerk's layout.
    w!(
        "/* The library's structs; every offset is checked against the layout taktwerk computed. */"
    );
    for s in &plan.structs {
        let spec = &d.abi.structs[&s.name];
        w!("typedef struct {{");
        for m in &spec.members {
            w!("    {} {};", m.ty, m.name);
        }
        w!("}} {};", s.name);
        w!(
            "_Static_assert(sizeof({}) == {}, \"{}: size differs from the layout taktwerk computed\");",
            s.name,
            s.layout.size(),
            s.name
        );
        for ml in s.layout.members() {
            w!(
                "_Static_assert(offsetof({}, {}) == {}, \"{}.{}: offset differs from the layout taktwerk computed\");",
                s.name,
                ml.name,
                ml.offset,
                s.name,
                ml.name
            );
            let expected = match ml.carrier {
                Carrier::Pointer => "sizeof(void *)".to_owned(),
                Carrier::Value(_) => ml.size.to_string(),
            };
            w!(
                "_Static_assert(sizeof((({}*)0)->{}) == {}, \"{}.{}: width\");",
                s.name,
                ml.name,
                expected,
                s.name,
                ml.name
            );
        }
        w!();
    }

    // Function pointer types, one per call; a symbol shared by two calls needs one prototype.
    w!("/* The library's functions, called through pointers resolved with dlsym. */");
    let mut protos: Vec<(String, String)> = Vec::new();
    let calls: Vec<(&str, &ResolvedCall)> = [
        Some(("init", &plan.init)),
        Some(("step", &plan.step)),
        plan.terminate.as_ref().map(|t| ("terminate", t)),
    ]
    .into_iter()
    .flatten()
    .collect();
    for (which, call) in &calls {
        let params: Vec<&str> = call.args.iter().map(|a| ctx.arg_type(a)).collect();
        let params = if params.is_empty() {
            "void".to_owned()
        } else {
            params.join(", ")
        };
        let ret = match call.returns {
            Returns::Int => "int",
            Returns::Void => "void",
        };
        let proto = format!("{ret} (*)({params})");
        if let Some((_, other)) = protos.iter().find(|(sym, _)| sym == &call.symbol) {
            if other != &proto {
                return Err(WrapError(format!(
                    "{}: init and step share the symbol but differ in prototype ({other} vs {proto})",
                    call.symbol
                )));
            }
        } else {
            protos.push((call.symbol.clone(), proto));
        }
        w!("typedef {ret} (*{})({params});", fn_typedef(which));
    }
    w!();

    // Tables.
    w!("#define N_DIMS {n_dims}");
    w!("#define N_VARS {}", vars.len());
    w!("#define VR_VAR0 (1 + N_DIMS)");
    let names = |items: Vec<String>| items.join(", ");
    if n_dims > 0 {
        w!(
            "static const char *const DIM_NAME[N_DIMS] = {{ {} }};",
            names(
                d.interface
                    .dimensions
                    .iter()
                    .map(|x| format!("\"{}\"", c_escape(&x.name)))
                    .collect()
            )
        );
        for (name, pick) in [
            ("DIM_START", 0_u8),
            ("DIM_MIN", 1),
            ("DIM_MAX", 2),
            ("DIM_HAS_MIN", 3),
            ("DIM_HAS_MAX", 4),
        ] {
            let values: Vec<String> = d
                .interface
                .dimensions
                .iter()
                .map(|x| match pick {
                    0 => format!("UINT64_C({})", x.default.or(x.min).unwrap_or(1)),
                    1 => format!("UINT64_C({})", x.min.unwrap_or(0)),
                    2 => format!("UINT64_C({})", x.max.unwrap_or(0)),
                    3 => x.min.is_some().to_string(),
                    _ => x.max.is_some().to_string(),
                })
                .collect();
            let ty = if pick >= 3 { "bool" } else { "uint64_t" };
            w!(
                "static const {ty} {name}[N_DIMS] = {{ {} }};",
                values.join(", ")
            );
        }
    }
    if !vars.is_empty() {
        w!(
            "static const char *const VAR_NAME[N_VARS] = {{ {} }};",
            names(
                vars.iter()
                    .map(|v| format!("\"{}\"", c_escape(&v.name)))
                    .collect()
            )
        );
        w!("enum {{ CAUS_INPUT, CAUS_OUTPUT, CAUS_PARAMETER, CAUS_TUNABLE }};");
        w!(
            "static const uint8_t VAR_CAUS[N_VARS] = {{ {} }};",
            names(
                vars.iter()
                    .map(|v| match v.causality {
                        Causality::Input => "CAUS_INPUT",
                        Causality::Output => "CAUS_OUTPUT",
                        Causality::Parameter => "CAUS_PARAMETER",
                        Causality::Tunable => "CAUS_TUNABLE",
                    }
                    .to_owned())
                    .collect()
            )
        );
        w!(
            "static const uint8_t VAR_TYPE[N_VARS] = {{ {} }};",
            names(
                vars.iter()
                    .map(|v| if is_text(v) {
                        TEXT_TYPE.to_string()
                    } else {
                        type_id(v.ty).to_string()
                    })
                    .collect()
            )
        );
    }
    w!("#define TYPE_UINT64 6");
    w!("#define TYPE_BOOLEAN 10");
    w!("#define TYPE_STRING {TEXT_TYPE}");
    w!();

    // The instance.
    w!(
        "enum State {{ ST_INSTANTIATED, ST_CONFIGURATION, ST_INITIALIZATION, ST_STEP, ST_TERMINATED }};"
    );
    w!();
    w!("typedef struct {{");
    w!("    char *name;");
    w!("    fmi3InstanceEnvironment env;");
    w!("    fmi3LogMessageCallback log;");
    w!("    bool logging;");
    w!("    enum State state;");
    w!("    void *lib;");
    w!("    void *bundled[{}];", options.bundled.len().max(1));
    for (which, _) in &calls {
        w!("    {} {which};", fn_typedef(which));
    }
    w!("    uint64_t dims[N_DIMS > 0 ? N_DIMS : 1];");
    for (i, v) in vars.iter().enumerate() {
        w!(
            "    {} *v{i}; size_t n{i}; size_t cap{i}; /* {} */",
            storage_type(v.ty),
            c_escape(&v.name)
        );
    }
    for (si, s) in plan.structs.iter().enumerate() {
        w!("    {} s{si};", s.name);
    }
    // Cells, typed by their role.
    let mut cell_types: Vec<Option<ScalarType>> = vec![None; plan.cells];
    let mut note_cell = |cell: usize, ty: ScalarType| {
        if let Some(slot) = cell_types.get_mut(cell) {
            *slot = Some(ty);
        }
    };
    for s in &plan.structs {
        for role in &s.members {
            match *role {
                MemberRole::Fixed {
                    value,
                    cell: Some(cell),
                } => note_cell(cell, value.init.ty),
                MemberRole::DimPointer { ty, cell, .. } => note_cell(cell, ty),
                MemberRole::Reported {
                    ty,
                    cell: Some(cell),
                    ..
                } => note_cell(cell, ty),
                _ => {}
            }
        }
    }
    for (_, call) in &calls {
        for a in &call.args {
            if let ArgRole::Fixed {
                value,
                cell: Some(cell),
            } = *a
            {
                note_cell(cell, value.init.ty);
            }
        }
    }
    for (i, ty) in cell_types.iter().enumerate() {
        w!("    {} c{i};", c_type(ty.unwrap_or(ScalarType::I64)));
    }
    w!("    void *handle;");
    w!("    bool initialised;");
    w!("    double start_time;");
    w!("    double time;");
    w!("    double step_size;");
    w!("    bool step_known;");
    w!("}} Model;");
    w!();

    // Logging.
    w!("static void vsay(Model *m, fmi3Status status, const char *fmt, va_list ap) {{");
    w!("    char buf[1024];");
    w!("    vsnprintf(buf, sizeof buf, fmt, ap);");
    w!("    if (m->log && (status != fmi3OK || m->logging))");
    w!("        m->log(m->env, status, status == fmi3OK ? \"logAll\" : \"logStatusError\", buf);");
    w!("}}");
    w!();
    w!("static fmi3Status fail(Model *m, const char *fmt, ...) {{");
    w!("    va_list ap;");
    w!("    va_start(ap, fmt);");
    w!("    vsay(m, fmi3Error, fmt, ap);");
    w!("    va_end(ap);");
    w!("    return fmi3Error;");
    w!("}}");
    w!();
    w!("static bool ok_code(int rc) {{");
    w!("    for (size_t i = 0; i < N_OK_CODES; i++) if (OK_CODES[i] == rc) return true;");
    w!("    return false;");
    w!("}}");
    w!();
    w!("#if N_DIMS > 0");
    w!("/* Allocation length of a dimension the library reports: its max. */");
    w!("static inline size_t dim_cap(const Model *m, size_t d) {{");
    w!("    uint64_t n = m->dims[d];");
    w!("    if (DIM_HAS_MAX[d] && DIM_MAX[d] > n) n = DIM_MAX[d];");
    w!("    return (size_t)n;");
    w!("}}");
    w!("#endif");
    w!();

    // Buffers.
    w!("static void free_buffers(Model *m) {{");
    for i in 0..vars.len() {
        w!("    free(m->v{i}); m->v{i} = NULL;");
    }
    w!("    (void)m;");
    w!("}}");
    w!();
    w!("/* Every buffer at the bound size (text at its capacity plus a guard NUL), zeroed. */");
    w!("static bool alloc_buffers(Model *m) {{");
    w!("    free_buffers(m);");
    for (i, v) in vars.iter().enumerate() {
        let (len, cap) = ctx.lengths(i);
        let guard = if is_text(v) { " + 1" } else { "" };
        w!("    m->n{i} = {len};");
        w!("    m->cap{i} = {cap};");
        w!("    m->v{i} = calloc(m->cap{i}{guard} ? m->cap{i}{guard} : 1, sizeof *m->v{i});");
        w!("    if (!m->v{i}) return false;");
    }
    for si in 0..plan.structs.len() {
        w!("    memset(&m->s{si}, 0, sizeof m->s{si});");
    }
    w!("    m->handle = NULL;");
    w!("    return true;");
    w!("}}");
    w!();

    // Fill before a call.
    w!("/* Write every struct image and storage cell for one call. */");
    w!("static void fill(Model *m, bool init, double time) {{");
    w!("    (void)m; (void)init; (void)time;");
    for (si, s) in plan.structs.iter().enumerate() {
        let spec = &d.abi.structs[&s.name];
        for (m, role) in spec.members.iter().zip(&s.members) {
            let lhs = format!("m->s{si}.{}", m.name);
            match *role {
                MemberRole::Pointer(Some(v)) => w!("    {lhs} = ({})m->v{v};", m.ty),
                MemberRole::Pointer(None) => w!("    {lhs} = NULL;"),
                MemberRole::Value(v) => {
                    if !plan.outputs.contains(&v) {
                        if m.ty.base == Some(ScalarType::Bool) {
                            w!("    {lhs} = (bool)(m->v{v}[0] != 0);");
                        } else {
                            w!("    {lhs} = ({})m->v{v}[0];", m.ty);
                        }
                    }
                }
                MemberRole::Dim(dim, ty) => w!("    {lhs} = ({})m->dims[{dim}];", c_type(ty)),
                MemberRole::Builtin(Builtin::StepSize) => w!("    {lhs} = m->step_size;"),
                MemberRole::Builtin(Builtin::Time) => w!("    {lhs} = time;"),
                MemberRole::Scratch => {}
                MemberRole::Fixed { value, cell: None } => w!(
                    "    {lhs} = init ? {} : {};",
                    phased_literal(value, true)?,
                    phased_literal(value, false)?
                ),
                MemberRole::Fixed {
                    value,
                    cell: Some(cell),
                } => {
                    w!(
                        "    m->c{cell} = init ? {} : {};",
                        phased_literal(value, true)?,
                        phased_literal(value, false)?
                    );
                    w!("    {lhs} = ({})&m->c{cell};", m.ty);
                }
                MemberRole::DimPointer { dim, ty, cell } => {
                    w!("    m->c{cell} = ({})m->dims[{dim}];", c_type(ty));
                    w!("    {lhs} = ({})&m->c{cell};", m.ty);
                }
                MemberRole::Reported { cell: None, .. } => {
                    w!("    if (init) {lhs} = 0;");
                }
                MemberRole::Reported {
                    cell: Some(cell), ..
                } => {
                    w!("    if (init) m->c{cell} = 0;");
                    w!("    {lhs} = ({})&m->c{cell};", m.ty);
                }
            }
        }
    }
    w!("}}");
    w!();

    // Reported lengths and output members after a call.
    w!("/* Compare every reported length with the bound one; read by-value outputs back. */");
    w!("static fmi3Status after_call(Model *m, const char *call) {{");
    w!("    (void)m; (void)call;");
    for (si, s) in plan.structs.iter().enumerate() {
        let spec = &d.abi.structs[&s.name];
        for (m, role) in spec.members.iter().zip(&s.members) {
            match *role {
                MemberRole::Reported { dim, cell, .. } => {
                    let got = match cell {
                        Some(c) => format!("m->c{c}"),
                        None => format!("m->s{si}.{}", m.name),
                    };
                    w!("    if ((long long){got} != (long long)m->dims[{dim}])");
                    w!(
                        "        return fail(m, \"%s: the library reports {}.{} = %lld but dimension {} is bound to %llu\", call, (long long){got}, (unsigned long long)m->dims[{dim}]);",
                        c_escape(&s.name),
                        c_escape(&m.name),
                        c_escape(&d.interface.dimensions[dim].name)
                    );
                }
                MemberRole::Value(v) if plan.outputs.contains(&v) => {
                    w!(
                        "    m->v{v}[0] = ({})m->s{si}.{};",
                        storage_type(vars[v].ty),
                        m.name
                    );
                }
                _ => {}
            }
        }
    }
    w!("    return fmi3OK;");
    w!("}}");
    w!();

    // The calls.
    for (which, call) in &calls {
        let init = *which == "init";
        let args: Vec<String> = call
            .args
            .iter()
            .map(|a| ctx.arg_expr(a, init))
            .collect::<Result<_, _>>()?;
        let args = args.join(", ");
        if *which == "terminate" {
            w!("static void call_terminate(Model *m) {{");
            w!("    fill(m, false, m->time);");
            w!("    double time = m->time; (void)time;");
            match call.returns {
                Returns::Int => w!("    (void)m->terminate({args});"),
                Returns::Void => w!("    m->terminate({args});"),
            }
            w!("}}");
        } else {
            w!("static fmi3Status call_{which}(Model *m, double time) {{");
            w!("    fill(m, {init}, time);");
            match call.returns {
                Returns::Int => {
                    w!("    int rc = m->{which}({args});");
                    w!(
                        "    if (!ok_code(rc)) return fail(m, \"{} returned %d\", rc);",
                        c_escape(&call.symbol)
                    );
                }
                Returns::Void => w!("    m->{which}({args});"),
            }
            w!("    return after_call(m, \"{which}\");");
            w!("}}");
        }
        w!();
    }

    // Loading the library.
    w!("static inline bool copy_file(const char *from, char *to_template) {{");
    w!("    FILE *in = fopen(from, \"rb\");");
    w!("    if (!in) return false;");
    w!("    int fd = mkstemp(to_template);");
    w!("    if (fd < 0) {{ fclose(in); return false; }}");
    w!("    FILE *out = fdopen(fd, \"wb\");");
    w!("    if (!out) {{ close(fd); fclose(in); return false; }}");
    w!("    char buf[65536];");
    w!("    size_t n;");
    w!("    bool good = true;");
    w!(
        "    while ((n = fread(buf, 1, sizeof buf, in)) > 0) if (fwrite(buf, 1, n, out) != n) good = false;"
    );
    w!("    if (ferror(in)) good = false;");
    w!("    fclose(in);");
    w!("    if (fclose(out) != 0) good = false;");
    w!("    return good;");
    w!("}}");
    w!();
    w!("/* The library lives in binaries/<platform>/ beside resources/; a `single` library is");
    w!("   loaded as a private copy per instance, since one path shares its globals. */");
    w!("static bool load_library(Model *m, const char *resource_path) {{");
    w!("    char dir[4096];");
    w!("    if (resource_path && *resource_path) {{");
    w!(
        "        /* <fmu>/resources/ -> <fmu>/binaries/<platform>/, without needing resources/ to exist. */"
    );
    w!("        snprintf(dir, sizeof dir, \"%s\", resource_path);");
    w!("        size_t n = strlen(dir);");
    w!("        while (n > 1 && dir[n - 1] == '/') dir[--n] = '\\0';");
    w!("        char *slash = strrchr(dir, '/');");
    w!("        if (slash) slash[1] = '\\0';");
    w!("        size_t used = strlen(dir);");
    w!("        snprintf(dir + used, sizeof dir - used, \"binaries/\" MODEL_PLATFORM \"/\");");
    w!("    }} else {{");
    w!("        Dl_info info;");
    w!(
        "        if (!dladdr((void *)&fmi3GetVersion, &info) || !info.dli_fname) {{ fail(m, \"cannot locate the FMU binaries\"); return false; }}"
    );
    w!("        snprintf(dir, sizeof dir, \"%s\", info.dli_fname);");
    w!("        char *slash = strrchr(dir, '/');");
    w!("        if (slash) slash[1] = '\\0'; else snprintf(dir, sizeof dir, \"./\");");
    w!("    }}");
    w!("    char path[4096];");
    w!("    for (size_t i = 0; BUNDLED[i]; i++) {{");
    w!("        snprintf(path, sizeof path, \"%s%s\", dir, BUNDLED[i]);");
    w!("        m->bundled[i] = dlopen(path, RTLD_NOW | RTLD_GLOBAL);");
    w!("        if (!m->bundled[i]) {{ fail(m, \"%s: %s\", path, dlerror()); return false; }}");
    w!("    }}");
    w!("    snprintf(path, sizeof path, \"%s%s\", dir, MODEL_LIBRARY);");
    w!("#if MODEL_SINGLE");
    w!("    const char *tmp = getenv(\"TMPDIR\");");
    w!("    char copy[4096];");
    w!(
        "    snprintf(copy, sizeof copy, \"%s/taktwerk-fmu-XXXXXX\", tmp && *tmp ? tmp : \"/tmp\");"
    );
    w!(
        "    if (!copy_file(path, copy)) {{ fail(m, \"%s: cannot make a private copy\", path); return false; }}"
    );
    w!("    m->lib = dlopen(copy, RTLD_NOW | RTLD_LOCAL);");
    w!("    unlink(copy);");
    w!("#else");
    w!("    m->lib = dlopen(path, RTLD_NOW | RTLD_LOCAL);");
    w!("#endif");
    w!("    if (!m->lib) {{ fail(m, \"%s: %s\", path, dlerror()); return false; }}");
    for (which, call) in &calls {
        w!(
            "    m->{which} = ({})dlsym(m->lib, \"{}\");",
            fn_typedef(which),
            c_escape(&call.symbol)
        );
        w!(
            "    if (!m->{which}) {{ fail(m, \"%s: no symbol {}\", path); return false; }}",
            c_escape(&call.symbol)
        );
    }
    w!("    return true;");
    w!("}}");
    w!();
    w!("static void free_model(Model *m) {{");
    w!("    if (m->initialised && m->state != ST_TERMINATED) {{");
    w!("#if HAS_TERMINATE");
    w!("        call_terminate(m);");
    w!("#endif");
    w!("    }}");
    w!("    if (m->lib) dlclose(m->lib);");
    w!("    free_buffers(m);");
    w!("    free(m->name);");
    w!("    free(m);");
    w!("}}");
    w!();

    // Variable lookup.
    w!("/* Variable index of a value reference, or -1. */");
    w!("static int var_of(fmi3ValueReference vr) {{");
    w!("    if (vr < VR_VAR0 || vr >= VR_VAR0 + N_VARS) return -1;");
    w!("    return (int)(vr - VR_VAR0);");
    w!("}}");
    w!();
    w!("static void *var_ptr(Model *m, int v) {{");
    w!("    switch (v) {{");
    for i in 0..vars.len() {
        w!("    case {i}: return m->v{i};");
    }
    w!("    default: return NULL;");
    w!("    }}");
    w!("}}");
    w!();
    w!("static size_t var_len(const Model *m, int v) {{");
    w!("    switch (v) {{");
    for i in 0..vars.len() {
        w!("    case {i}: return m->n{i};");
    }
    w!("    default: return 0;");
    w!("    }}");
    w!("}}");
    w!();
    w!("/* Whether variable `v` may be set in the instance's state. */");
    w!("static fmi3Status settable(Model *m, int v) {{");
    w!(
        "    if (m->state == ST_TERMINATED) return fail(m, \"%s: set after terminate\", VAR_NAME[v]);"
    );
    w!("    if (VAR_CAUS[v] == CAUS_OUTPUT) return fail(m, \"%s is an output\", VAR_NAME[v]);");
    w!(
        "    if (VAR_CAUS[v] == CAUS_PARAMETER && m->state == ST_STEP) return fail(m, \"%s: a fixed parameter cannot change after initialization\", VAR_NAME[v]);"
    );
    w!("    return fmi3OK;");
    w!("}}");
    w!();

    // Typed Get/Set.
    w!("#define DEFINE_GET(NAME, CTYPE, TYPEID) \\");
    w!(
        "fmi3Status NAME(fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, CTYPE values[], size_t nValues) {{ \\"
    );
    w!("    Model *m = (Model *)instance; \\");
    w!("    size_t k = 0; \\");
    w!("    if (!m) return fmi3Error; \\");
    w!("    for (size_t i = 0; i < nvr; i++) {{ \\");
    w!("        int v = var_of(vr[i]); \\");
    w!(
        "        if (v < 0 || VAR_TYPE[v] != TYPEID) return fail(m, #NAME \": value reference %u is not a \" #CTYPE, (unsigned)vr[i]); \\"
    );
    w!("        size_t n = var_len(m, v); \\");
    w!(
        "        if (k + n > nValues) return fail(m, #NAME \": %zu values given, more needed\", nValues); \\"
    );
    w!("        memcpy(values + k, var_ptr(m, v), n * sizeof(CTYPE)); \\");
    w!("        k += n; \\");
    w!("    }} \\");
    w!("    return fmi3OK; \\");
    w!("}}");
    w!();
    w!("#define DEFINE_SET(NAME, CTYPE, TYPEID) \\");
    w!(
        "fmi3Status NAME(fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const CTYPE values[], size_t nValues) {{ \\"
    );
    w!("    Model *m = (Model *)instance; \\");
    w!("    size_t k = 0; \\");
    w!("    if (!m) return fmi3Error; \\");
    w!("    for (size_t i = 0; i < nvr; i++) {{ \\");
    w!("        int v = var_of(vr[i]); \\");
    w!(
        "        if (v < 0 || VAR_TYPE[v] != TYPEID) return fail(m, #NAME \": value reference %u is not a \" #CTYPE, (unsigned)vr[i]); \\"
    );
    w!("        fmi3Status s = settable(m, v); \\");
    w!("        if (s != fmi3OK) return s; \\");
    w!("        size_t n = var_len(m, v); \\");
    w!(
        "        if (k + n > nValues) return fail(m, #NAME \": %zu values given, more needed\", nValues); \\"
    );
    w!("        memcpy(var_ptr(m, v), values + k, n * sizeof(CTYPE)); \\");
    w!("        k += n; \\");
    w!("    }} \\");
    w!("    return fmi3OK; \\");
    w!("}}");
    w!();
    for (name, ty, id) in TYPED {
        w!("DEFINE_GET(fmi3Get{name}, {ty}, {id})");
        w!("DEFINE_SET(fmi3Set{name}, {ty}, {id})");
    }
    w!();

    // UInt64: structural parameters and plain u64 variables.
    w!(
        "fmi3Status fmi3GetUInt64(fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, fmi3UInt64 values[], size_t nValues) {{"
    );
    w!("    Model *m = (Model *)instance;");
    w!("    size_t k = 0;");
    w!("    if (!m) return fmi3Error;");
    w!("    for (size_t i = 0; i < nvr; i++) {{");
    w!("        if (vr[i] >= 1 && vr[i] < VR_VAR0) {{");
    w!(
        "            if (k + 1 > nValues) return fail(m, \"fmi3GetUInt64: %zu values given, more needed\", nValues);"
    );
    w!("            values[k++] = m->dims[vr[i] - 1];");
    w!("            continue;");
    w!("        }}");
    w!("        int v = var_of(vr[i]);");
    w!(
        "        if (v < 0 || VAR_TYPE[v] != TYPE_UINT64) return fail(m, \"fmi3GetUInt64: value reference %u is not a UInt64\", (unsigned)vr[i]);"
    );
    w!("        size_t n = var_len(m, v);");
    w!(
        "        if (k + n > nValues) return fail(m, \"fmi3GetUInt64: %zu values given, more needed\", nValues);"
    );
    w!("        memcpy(values + k, var_ptr(m, v), n * sizeof(fmi3UInt64));");
    w!("        k += n;");
    w!("    }}");
    w!("    return fmi3OK;");
    w!("}}");
    w!();
    w!(
        "fmi3Status fmi3SetUInt64(fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const fmi3UInt64 values[], size_t nValues) {{"
    );
    w!("    Model *m = (Model *)instance;");
    w!("    size_t k = 0;");
    w!("    if (!m) return fmi3Error;");
    w!("    for (size_t i = 0; i < nvr; i++) {{");
    w!("        if (vr[i] >= 1 && vr[i] < VR_VAR0) {{");
    w!("#if N_DIMS > 0");
    w!("            size_t d = vr[i] - 1;");
    w!(
        "            if (m->state != ST_CONFIGURATION) return fail(m, \"%s: a structural parameter is set in configuration mode\", DIM_NAME[d]);"
    );
    w!(
        "            if (k + 1 > nValues) return fail(m, \"fmi3SetUInt64: %zu values given, more needed\", nValues);"
    );
    w!("            uint64_t n = values[k++];");
    w!("            if ((DIM_HAS_MIN[d] && n < DIM_MIN[d]) || (DIM_HAS_MAX[d] && n > DIM_MAX[d]))");
    w!(
        "                return fail(m, \"%s = %llu is outside [%llu, %llu]\", DIM_NAME[d], (unsigned long long)n, (unsigned long long)DIM_MIN[d], (unsigned long long)DIM_MAX[d]);"
    );
    w!("            m->dims[d] = n;");
    w!("#endif");
    w!("            continue;");
    w!("        }}");
    w!("        int v = var_of(vr[i]);");
    w!(
        "        if (v < 0 || VAR_TYPE[v] != TYPE_UINT64) return fail(m, \"fmi3SetUInt64: value reference %u is not a UInt64\", (unsigned)vr[i]);"
    );
    w!("        fmi3Status s = settable(m, v);");
    w!("        if (s != fmi3OK) return s;");
    w!("        size_t n = var_len(m, v);");
    w!(
        "        if (k + n > nValues) return fail(m, \"fmi3SetUInt64: %zu values given, more needed\", nValues);"
    );
    w!("        memcpy(var_ptr(m, v), values + k, n * sizeof(fmi3UInt64));");
    w!("        k += n;");
    w!("    }}");
    w!("    return fmi3OK;");
    w!("}}");
    w!();

    // Boolean: bytes in storage, any non-zero byte is true.
    w!(
        "fmi3Status fmi3GetBoolean(fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, fmi3Boolean values[], size_t nValues) {{"
    );
    w!("    Model *m = (Model *)instance;");
    w!("    size_t k = 0;");
    w!("    if (!m) return fmi3Error;");
    w!("    for (size_t i = 0; i < nvr; i++) {{");
    w!("        int v = var_of(vr[i]);");
    w!(
        "        if (v < 0 || VAR_TYPE[v] != TYPE_BOOLEAN) return fail(m, \"fmi3GetBoolean: value reference %u is not a Boolean\", (unsigned)vr[i]);"
    );
    w!("        size_t n = var_len(m, v);");
    w!(
        "        if (k + n > nValues) return fail(m, \"fmi3GetBoolean: %zu values given, more needed\", nValues);"
    );
    w!("        const uint8_t *p = var_ptr(m, v);");
    w!("        for (size_t j = 0; j < n; j++) values[k + j] = p[j] != 0;");
    w!("        k += n;");
    w!("    }}");
    w!("    return fmi3OK;");
    w!("}}");
    w!();
    w!(
        "fmi3Status fmi3SetBoolean(fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const fmi3Boolean values[], size_t nValues) {{"
    );
    w!("    Model *m = (Model *)instance;");
    w!("    size_t k = 0;");
    w!("    if (!m) return fmi3Error;");
    w!("    for (size_t i = 0; i < nvr; i++) {{");
    w!("        int v = var_of(vr[i]);");
    w!(
        "        if (v < 0 || VAR_TYPE[v] != TYPE_BOOLEAN) return fail(m, \"fmi3SetBoolean: value reference %u is not a Boolean\", (unsigned)vr[i]);"
    );
    w!("        fmi3Status s = settable(m, v);");
    w!("        if (s != fmi3OK) return s;");
    w!("        size_t n = var_len(m, v);");
    w!(
        "        if (k + n > nValues) return fail(m, \"fmi3SetBoolean: %zu values given, more needed\", nValues);"
    );
    w!("        uint8_t *p = var_ptr(m, v);");
    w!("        for (size_t j = 0; j < n; j++) p[j] = values[k + j] ? 1 : 0;");
    w!("        k += n;");
    w!("    }}");
    w!("    return fmi3OK;");
    w!("}}");
    w!();

    // Strings: one value per text variable, copied NUL-terminated and truncated to capacity.
    w!(
        "fmi3Status fmi3GetString(fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, fmi3String values[], size_t nValues) {{"
    );
    w!("    Model *m = (Model *)instance;");
    w!("    if (!m) return fmi3Error;");
    w!(
        "    if (nValues < nvr) return fail(m, \"fmi3GetString: %zu values given, %zu needed\", nValues, nvr);"
    );
    w!("    for (size_t i = 0; i < nvr; i++) {{");
    w!("        int v = var_of(vr[i]);");
    w!(
        "        if (v < 0 || VAR_TYPE[v] != TYPE_STRING) return fail(m, \"fmi3GetString: value reference %u is not a String\", (unsigned)vr[i]);"
    );
    w!("        char *p = var_ptr(m, v);");
    w!("        p[var_len(m, v)] = '\\0'; /* the guard byte beyond the capacity */");
    w!("        values[i] = p;");
    w!("    }}");
    w!("    return fmi3OK;");
    w!("}}");
    w!();
    w!(
        "fmi3Status fmi3SetString(fmi3Instance instance, const fmi3ValueReference vr[], size_t nvr, const fmi3String values[], size_t nValues) {{"
    );
    w!("    Model *m = (Model *)instance;");
    w!("    if (!m) return fmi3Error;");
    w!(
        "    if (nValues < nvr) return fail(m, \"fmi3SetString: %zu values given, %zu needed\", nValues, nvr);"
    );
    w!("    for (size_t i = 0; i < nvr; i++) {{");
    w!("        int v = var_of(vr[i]);");
    w!(
        "        if (v < 0 || VAR_TYPE[v] != TYPE_STRING) return fail(m, \"fmi3SetString: value reference %u is not a String\", (unsigned)vr[i]);"
    );
    w!("        fmi3Status s = settable(m, v);");
    w!("        if (s != fmi3OK) return s;");
    w!("        char *p = var_ptr(m, v);");
    w!("        size_t cap = var_len(m, v);");
    w!("        memset(p, 0, cap + 1);");
    w!("        if (cap > 0 && values[i]) strncpy(p, values[i], cap - 1);");
    w!("    }}");
    w!("    return fmi3OK;");
    w!("}}");
    w!();

    // Lifecycle.
    w!("const char *fmi3GetVersion(void) {{ return fmi3Version; }}");
    w!();
    w!(
        "fmi3Status fmi3SetDebugLogging(fmi3Instance instance, fmi3Boolean loggingOn, size_t nCategories, const fmi3String categories[]) {{"
    );
    w!("    Model *m = (Model *)instance;");
    w!("    (void)nCategories; (void)categories;");
    w!("    if (!m) return fmi3Error;");
    w!("    m->logging = loggingOn;");
    w!("    return fmi3OK;");
    w!("}}");
    w!();
    w!(
        "static void refuse(fmi3InstanceEnvironment env, fmi3LogMessageCallback log, const char *what) {{"
    );
    w!("    if (log) log(env, fmi3Error, \"logStatusError\", what);");
    w!("}}");
    w!();
    w!(
        "fmi3Instance fmi3InstantiateModelExchange(fmi3String instanceName, fmi3String instantiationToken, fmi3String resourcePath, fmi3Boolean visible, fmi3Boolean loggingOn, fmi3InstanceEnvironment instanceEnvironment, fmi3LogMessageCallback logMessage) {{"
    );
    w!(
        "    (void)instanceName; (void)instantiationToken; (void)resourcePath; (void)visible; (void)loggingOn;"
    );
    w!("    refuse(instanceEnvironment, logMessage, MODEL_NAME \" is a co-simulation FMU\");");
    w!("    return NULL;");
    w!("}}");
    w!();
    w!(
        "fmi3Instance fmi3InstantiateScheduledExecution(fmi3String instanceName, fmi3String instantiationToken, fmi3String resourcePath, fmi3Boolean visible, fmi3Boolean loggingOn, fmi3InstanceEnvironment instanceEnvironment, fmi3LogMessageCallback logMessage, fmi3ClockUpdateCallback clockUpdate, fmi3LockPreemptionCallback lockPreemption, fmi3UnlockPreemptionCallback unlockPreemption) {{"
    );
    w!(
        "    (void)instanceName; (void)instantiationToken; (void)resourcePath; (void)visible; (void)loggingOn; (void)clockUpdate; (void)lockPreemption; (void)unlockPreemption;"
    );
    w!("    refuse(instanceEnvironment, logMessage, MODEL_NAME \" is a co-simulation FMU\");");
    w!("    return NULL;");
    w!("}}");
    w!();
    w!(
        "fmi3Instance fmi3InstantiateCoSimulation(fmi3String instanceName, fmi3String instantiationToken, fmi3String resourcePath, fmi3Boolean visible, fmi3Boolean loggingOn, fmi3Boolean eventModeUsed, fmi3Boolean earlyReturnAllowed, const fmi3ValueReference requiredIntermediateVariables[], size_t nRequiredIntermediateVariables, fmi3InstanceEnvironment instanceEnvironment, fmi3LogMessageCallback logMessage, fmi3IntermediateUpdateCallback intermediateUpdate) {{"
    );
    w!(
        "    (void)visible; (void)eventModeUsed; (void)earlyReturnAllowed; (void)requiredIntermediateVariables; (void)nRequiredIntermediateVariables; (void)intermediateUpdate;"
    );
    w!(
        "    if (!instanceName || !instantiationToken) {{ refuse(instanceEnvironment, logMessage, \"instance name and token are required\"); return NULL; }}"
    );
    w!(
        "    if (strcmp(instantiationToken, MODEL_TOKEN) != 0) {{ refuse(instanceEnvironment, logMessage, \"instantiation token does not match \" MODEL_TOKEN); return NULL; }}"
    );
    w!("    Model *m = calloc(1, sizeof *m);");
    w!("    if (!m) return NULL;");
    w!("    m->name = strdup(instanceName);");
    w!("    m->env = instanceEnvironment;");
    w!("    m->log = logMessage;");
    w!("    m->logging = loggingOn;");
    w!("    m->state = ST_INSTANTIATED;");
    w!("#if N_DIMS > 0");
    w!("    for (size_t d = 0; d < N_DIMS; d++) m->dims[d] = DIM_START[d];");
    w!("#endif");
    w!("    if (!alloc_buffers(m)) {{ fail(m, \"out of memory\"); free_model(m); return NULL; }}");
    w!("    if (!load_library(m, resourcePath)) {{ free_model(m); return NULL; }}");
    w!("    return m;");
    w!("}}");
    w!();
    w!("void fmi3FreeInstance(fmi3Instance instance) {{");
    w!("    if (instance) free_model((Model *)instance);");
    w!("}}");
    w!();
    w!("fmi3Status fmi3EnterConfigurationMode(fmi3Instance instance) {{");
    w!("    Model *m = (Model *)instance;");
    w!("    if (!m) return fmi3Error;");
    w!(
        "    if (m->state != ST_INSTANTIATED) return fail(m, \"configuration mode is entered before initialization\");"
    );
    w!("    m->state = ST_CONFIGURATION;");
    w!("    return fmi3OK;");
    w!("}}");
    w!();
    w!("fmi3Status fmi3ExitConfigurationMode(fmi3Instance instance) {{");
    w!("    Model *m = (Model *)instance;");
    w!("    if (!m) return fmi3Error;");
    w!("    if (m->state != ST_CONFIGURATION) return fail(m, \"not in configuration mode\");");
    w!("    if (!alloc_buffers(m)) return fail(m, \"out of memory\");");
    w!("    m->state = ST_INSTANTIATED;");
    w!("    return fmi3OK;");
    w!("}}");
    w!();
    w!(
        "fmi3Status fmi3EnterInitializationMode(fmi3Instance instance, fmi3Boolean toleranceDefined, fmi3Float64 tolerance, fmi3Float64 startTime, fmi3Boolean stopTimeDefined, fmi3Float64 stopTime) {{"
    );
    w!("    Model *m = (Model *)instance;");
    w!("    (void)toleranceDefined; (void)tolerance; (void)stopTimeDefined; (void)stopTime;");
    w!("    if (!m) return fmi3Error;");
    w!(
        "    if (m->state != ST_INSTANTIATED) return fail(m, \"initialization mode is entered once, after instantiation\");"
    );
    w!("    m->start_time = startTime;");
    w!("    m->time = startTime;");
    w!("    m->state = ST_INITIALIZATION;");
    w!("    return fmi3OK;");
    w!("}}");
    w!();
    w!("fmi3Status fmi3ExitInitializationMode(fmi3Instance instance) {{");
    w!("    Model *m = (Model *)instance;");
    w!("    if (!m) return fmi3Error;");
    w!("    if (m->state != ST_INITIALIZATION) return fail(m, \"not in initialization mode\");");
    w!("    m->state = ST_STEP;");
    w!("#if !USES_STEP_SIZE");
    w!("    /* The library is not told its step: init runs now, as the adapter does. */");
    w!("    fmi3Status s = call_init(m, m->start_time);");
    w!("    if (s != fmi3OK) return s;");
    w!("    m->initialised = true;");
    w!("#endif");
    w!("    return fmi3OK;");
    w!("}}");
    w!();
    w!(
        "fmi3Status fmi3DoStep(fmi3Instance instance, fmi3Float64 currentCommunicationPoint, fmi3Float64 communicationStepSize, fmi3Boolean noSetFMUStatePriorToCurrentPoint, fmi3Boolean *eventHandlingNeeded, fmi3Boolean *terminateSimulation, fmi3Boolean *earlyReturn, fmi3Float64 *lastSuccessfulTime) {{"
    );
    w!("    Model *m = (Model *)instance;");
    w!("    (void)noSetFMUStatePriorToCurrentPoint;");
    w!("    if (!m) return fmi3Error;");
    w!("    if (m->state != ST_STEP) return fail(m, \"fmi3DoStep outside step mode\");");
    w!("    if (eventHandlingNeeded) *eventHandlingNeeded = false;");
    w!("    if (terminateSimulation) *terminateSimulation = false;");
    w!("    if (earlyReturn) *earlyReturn = false;");
    w!("    if (lastSuccessfulTime) *lastSuccessfulTime = currentCommunicationPoint;");
    w!("    if (!m->step_known) {{ m->step_size = communicationStepSize; m->step_known = true; }}");
    w!("#if USES_STEP_SIZE");
    w!("    if (communicationStepSize != m->step_size)");
    w!(
        "        return fail(m, \"communication step %g differs from the step size %g the library was initialised with\", communicationStepSize, m->step_size);"
    );
    w!("    if (!m->initialised) {{");
    w!("        /* The library is told its step at init: the first step fixes it. */");
    w!("        fmi3Status s = call_init(m, m->start_time);");
    w!("        if (s != fmi3OK) return s;");
    w!("        m->initialised = true;");
    w!("    }}");
    w!("#endif");
    w!("    fmi3Status s = call_step(m, currentCommunicationPoint);");
    w!("    if (s != fmi3OK) return s;");
    w!("    m->time = currentCommunicationPoint + communicationStepSize;");
    w!("    if (lastSuccessfulTime) *lastSuccessfulTime = m->time;");
    w!("    return fmi3OK;");
    w!("}}");
    w!();
    w!("fmi3Status fmi3Terminate(fmi3Instance instance) {{");
    w!("    Model *m = (Model *)instance;");
    w!("    if (!m) return fmi3Error;");
    w!("    if (m->state == ST_TERMINATED) return fail(m, \"terminated twice\");");
    w!("#if HAS_TERMINATE");
    w!("    if (m->initialised) call_terminate(m);");
    w!("#endif");
    w!("    m->state = ST_TERMINATED;");
    w!("    return fmi3OK;");
    w!("}}");
    w!();
    w!("fmi3Status fmi3Reset(fmi3Instance instance) {{");
    w!("    Model *m = (Model *)instance;");
    w!("    if (!m) return fmi3Error;");
    w!("#if HAS_TERMINATE");
    w!("    if (m->initialised && m->state != ST_TERMINATED) call_terminate(m);");
    w!("    if (!alloc_buffers(m)) return fail(m, \"out of memory\");");
    w!("    m->initialised = false;");
    w!("    m->step_known = false;");
    w!("    m->state = ST_INSTANTIATED;");
    w!("    return fmi3OK;");
    w!("#else");
    w!("    return fail(m, \"reset is not supported: the library declares no terminate call\");");
    w!("#endif");
    w!("}}");
    w!();

    // Stubs.
    w!("/* Functions the standard declares that this wrapper does not provide. */");
    w!("static fmi3Status unsupported(fmi3Instance instance, const char *what) {{");
    w!("    Model *m = (Model *)instance;");
    w!("    if (!m) return fmi3Error;");
    w!("    return fail(m, \"%s is not supported by this FMU\", what);");
    w!("}}");
    w!();
    for (name, params) in STUBS {
        let unused: Vec<String> = params
            .split(',')
            .filter_map(|p| p.trim().split([' ', '*']).next_back())
            .map(|p| p.trim_end_matches("[]").to_owned())
            .filter(|p| p != "instance" && !p.is_empty())
            .map(|p| format!("(void){p};"))
            .collect();
        w!("fmi3Status {name}({params}) {{");
        if !unused.is_empty() {
            w!("    {}", unused.join(" "));
        }
        w!("    return unsupported(instance, \"{name}\");");
        w!("}}");
        w!();
    }

    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_and_tokens_are_stable() {
        assert_eq!(model_identifier("state-space"), "state_space");
        assert_eq!(model_identifier("3d model"), "m_3d_model");
        assert_eq!(model_identifier(""), "model");
        assert_eq!(token_of("a"), token_of("a"));
        assert_ne!(token_of("a"), token_of("b"));
        assert!(token_of("x").starts_with("taktwerk-"));
    }

    #[test]
    fn literals_carry_their_type() {
        let v = CValue::new(ScalarType::I16, crate::descriptor::Number::Int(-3)).unwrap();
        assert_eq!(literal(v).unwrap(), "((int16_t)-3)");
        let v = CValue::new(ScalarType::F64, crate::descriptor::Number::Float(0.5)).unwrap();
        assert_eq!(literal(v).unwrap(), "((double)0.5)");
        let v = CValue::new(ScalarType::Bool, crate::descriptor::Number::Int(1)).unwrap();
        assert_eq!(literal(v).unwrap(), "((bool)true)");
    }

    #[test]
    fn stubs_cover_the_standard() {
        // Every function fmi3Functions.h exports is implemented or stubbed exactly once.
        let header = FMI3_HEADERS[0].1;
        let declared: Vec<&str> = header
            .lines()
            .filter(|l| l.starts_with("FMI3_Export"))
            .filter_map(|l| l.split_whitespace().last())
            .map(|n| n.trim_end_matches(';'))
            .collect();
        let implemented: Vec<String> = TYPED
            .iter()
            .flat_map(|(n, _, _)| [format!("fmi3Get{n}"), format!("fmi3Set{n}")])
            .chain(
                [
                    "fmi3GetVersion",
                    "fmi3SetDebugLogging",
                    "fmi3InstantiateModelExchange",
                    "fmi3InstantiateCoSimulation",
                    "fmi3InstantiateScheduledExecution",
                    "fmi3FreeInstance",
                    "fmi3EnterInitializationMode",
                    "fmi3ExitInitializationMode",
                    "fmi3Terminate",
                    "fmi3Reset",
                    "fmi3GetUInt64",
                    "fmi3SetUInt64",
                    "fmi3GetBoolean",
                    "fmi3SetBoolean",
                    "fmi3GetString",
                    "fmi3SetString",
                    "fmi3EnterConfigurationMode",
                    "fmi3ExitConfigurationMode",
                    "fmi3DoStep",
                ]
                .map(str::to_owned),
            )
            .chain(STUBS.iter().map(|(n, _)| (*n).to_owned()))
            .collect();
        for name in &declared {
            assert_eq!(
                implemented.iter().filter(|i| i == name).count(),
                1,
                "{name}"
            );
        }
        assert_eq!(declared.len(), implemented.len());
    }
}
