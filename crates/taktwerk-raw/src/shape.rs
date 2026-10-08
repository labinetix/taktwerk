//! The recommended C shape: a header the importer reads without guessing.
//!
//! For a model `<m>`:
//!
//! ```c
//! typedef struct { int nx; /* 1..64 = 4 */ int nu; } m_dims;
//! typedef struct { const double *A; /* [nx][nx] */ double k; } m_params;
//! typedef struct { const double *u; /* [nu] m/s: inflow */ } m_inputs;
//! typedef struct { double gain; } m_tunables;
//! typedef struct { double *x; /* [nx] */ double y; } m_outputs;
//!
//! int m_init(void **h, const m_dims *dims, const m_params *params, double step_size);
//! int m_step(void *h, double time, const m_inputs *in, const m_tunables *tun, m_outputs *out);
//! int m_terminate(void *h);  /* or void */
//! ```
//!
//! - Struct roles come from the type name suffix; every member takes its struct's causality.
//!   `<m>_params` and `<m>_tunables` may be absent, and with them their argument. Struct
//!   arguments are matched by type, not position; the handle, `time` and `step_size` are fixed.
//! - `<m>_dims` holds one integer per dimension, named like it. An optional trailing comment
//!   bounds it: `/* 1..64 */`, `/* 1.. */`, `/* ..64 = 4 */` (`= default`).
//! - Every pointer member carries its shape in a trailing comment, in dimension names or
//!   literal lengths: `/* [n] */`, `/* [nx][nu] */`, `/* [3] */`; matrices are row-major.
//!   Scalars are members by value.
//! - After the shape, an optional `unit: description`; text without a colon is a unit when it
//!   is one word and a description otherwise.
//!
//! The importer returns the confirmed descriptor or every deviation from the shape.

use std::collections::{BTreeMap, BTreeSet};

use taktwerk_core::model::{Causality, Dimension, Instances, ModelInterface, Variable};
use taktwerk_core::value::{Dim, Layout, ScalarType};

use crate::descriptor::{
    Abi, Arg, Builtin, CType, Call, Descriptor, Handle, Member, Returns, StructSpec,
};
use crate::import::{Function, Header, ParsedType, Struct};

/// A struct's role, by its name suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    Dims,
    Params,
    Inputs,
    Tunables,
    Outputs,
}

impl Role {
    const ALL: [Self; 5] = [
        Self::Dims,
        Self::Params,
        Self::Inputs,
        Self::Tunables,
        Self::Outputs,
    ];

    const fn suffix(self) -> &'static str {
        match self {
            Self::Dims => "dims",
            Self::Params => "params",
            Self::Inputs => "inputs",
            Self::Tunables => "tunables",
            Self::Outputs => "outputs",
        }
    }

    const fn causality(self) -> Causality {
        match self {
            Self::Dims | Self::Params => Causality::Parameter,
            Self::Inputs => Causality::Input,
            Self::Tunables => Causality::Tunable,
            Self::Outputs => Causality::Output,
        }
    }
}

/// A fixed argument of a call.
#[derive(Debug, Clone, Copy)]
enum Fixed {
    HandleOut,
    HandleIn,
    Time,
    StepSize,
}

impl Fixed {
    fn matches(self, ty: &ParsedType) -> bool {
        let double = ParsedType::Scalar(CType {
            base: Some(ScalarType::F64),
            pointer: false,
        });
        match self {
            Self::HandleOut => *ty == ParsedType::VoidPtrPtr,
            Self::HandleIn => {
                *ty == ParsedType::Scalar(CType {
                    base: None,
                    pointer: true,
                })
            }
            Self::Time | Self::StepSize => *ty == double,
        }
    }

    const fn spelled(self) -> &'static str {
        match self {
            Self::HandleOut => "void **h",
            Self::HandleIn => "void *h",
            Self::Time => "double time",
            Self::StepSize => "double step_size",
        }
    }

    fn arg(self) -> Arg {
        match self {
            Self::HandleOut => Arg {
                handle: Some(Handle::Out),
                ..Arg::default()
            },
            Self::HandleIn => Arg {
                handle: Some(Handle::In),
                ..Arg::default()
            },
            Self::Time => Arg {
                builtin: Some(Builtin::Time),
                ..Arg::default()
            },
            Self::StepSize => Arg {
                builtin: Some(Builtin::StepSize),
                ..Arg::default()
            },
        }
    }
}

/// Read `header` as the recommended shape: the confirmed descriptor named `name`, or every
/// deviation from the shape.
pub(crate) fn read(header: &Header, name: &str) -> Result<Descriptor, Vec<String>> {
    let steps: Vec<&str> = header
        .functions
        .iter()
        .filter_map(|f| f.name.strip_suffix("_step"))
        .collect();
    let m = match steps.as_slice() {
        [m] if !m.is_empty() => (*m).to_owned(),
        [] | [_] => return Err(vec!["no `<m>_step` function".to_owned()]),
        many => {
            return Err(vec![format!(
                "more than one `<m>_step` function: {}",
                many.iter()
                    .map(|m| format!("{m}_step"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )]);
        }
    };
    let role_of = |s: &Struct| {
        Role::ALL
            .into_iter()
            .find(|r| s.name == format!("{m}_{}", r.suffix()))
    };
    if !header.structs.iter().any(|s| role_of(s).is_some()) {
        return Err(vec![format!(
            "no struct named `{m}_dims`, `{m}_params`, `{m}_inputs`, `{m}_tunables` or \
             `{m}_outputs`"
        )]);
    }

    let mut r = Reader {
        m: m.clone(),
        deviations: Vec::new(),
        roles: BTreeMap::new(),
        dims: Vec::new(),
        variables: Vec::new(),
        structs: BTreeMap::new(),
    };
    for s in &header.structs {
        match role_of(s) {
            Some(role) => {
                r.roles.insert(role, s);
            }
            None => r.deviate(format!(
                "struct {}: not one of {m}_dims, {m}_params, {m}_inputs, {m}_tunables, \
                 {m}_outputs",
                s.name
            )),
        }
    }
    for role in [Role::Dims, Role::Inputs, Role::Outputs] {
        if !r.roles.contains_key(&role) {
            r.deviate(format!("no struct {m}_{}", role.suffix()));
        }
    }
    if let Some(dims) = r.roles.get(&Role::Dims).copied() {
        r.read_dims(dims);
    }
    for role in [Role::Params, Role::Inputs, Role::Tunables, Role::Outputs] {
        if let Some(s) = r.roles.get(&role).copied() {
            r.read_struct(s, role);
        }
    }

    let function = |suffix: &str| {
        header
            .functions
            .iter()
            .find(|f| f.name == format!("{m}_{suffix}"))
    };
    let init = function("init").map(|f| {
        r.call(
            f,
            &[Fixed::HandleOut],
            &[Fixed::StepSize],
            &[Role::Dims, Role::Params],
        )
    });
    let step = function("step").map(|f| {
        r.call(
            f,
            &[Fixed::HandleIn, Fixed::Time],
            &[],
            &[Role::Inputs, Role::Tunables, Role::Outputs],
        )
    });
    let terminate = function("terminate").map(|f| r.terminate(f));
    if init.is_none() {
        r.deviate(format!("no function {m}_init"));
    }
    if terminate.is_none() {
        r.deviate(format!("no function {m}_terminate"));
    }
    let (Some(init), Some(step), Some(terminate)) = (init, step, terminate) else {
        return Err(r.deviations);
    };
    if !r.deviations.is_empty() {
        return Err(r.deviations);
    }
    let descriptor = Descriptor {
        interface: ModelInterface {
            name: name.to_owned(),
            dimensions: r.dims,
            variables: r.variables,
            instances: Instances::Multiple,
        },
        abi: Abi {
            confirmed: true,
            library: None,
            ok_codes: vec![0],
            init,
            step,
            terminate: Some(terminate),
            structs: r.structs,
        },
    };
    match descriptor.validate() {
        Ok(_) => Ok(descriptor),
        Err(e) => Err(vec![format!("the descriptor read from it is invalid: {e}")]),
    }
}

/// Accumulates the descriptor while the shape is checked.
struct Reader<'h> {
    m: String,
    deviations: Vec<String>,
    roles: BTreeMap<Role, &'h Struct>,
    dims: Vec<Dimension>,
    variables: Vec<Variable>,
    structs: BTreeMap<String, StructSpec>,
}

fn member(name: &str, ty: CType) -> Member {
    Member {
        name: name.to_owned(),
        ty,
        variable: None,
        dim: None,
        builtin: None,
        const_: None,
        phase: None,
        reported: false,
    }
}

impl Reader<'_> {
    fn deviate(&mut self, text: String) {
        self.deviations.push(text);
    }

    /// `<m>_dims`: one integer per dimension, optional bounds in its comment.
    fn read_dims(&mut self, s: &Struct) {
        let mut members = Vec::with_capacity(s.members.len());
        for slot in &s.members {
            let name = slot.name.clone().unwrap_or_default();
            let ty = match &slot.ty {
                ParsedType::Scalar(t) if !t.pointer && t.is_integer() => *t,
                _ => {
                    self.deviate(format!(
                        "{}: {name} is not an integer by value (one `int` per dimension)",
                        s.name
                    ));
                    continue;
                }
            };
            let (min, max, default) = match slot.comment.as_deref().map(bounds) {
                None => (None, None, None),
                Some(Ok(b)) => b,
                Some(Err(e)) => {
                    self.deviate(format!("{}: {name}: {e}", s.name));
                    (None, None, None)
                }
            };
            self.dims.push(Dimension {
                name: name.clone(),
                min,
                max,
                default,
            });
            members.push(Member {
                dim: Some(name.clone()),
                ..member(&name, ty)
            });
        }
        self.structs.insert(s.name.clone(), StructSpec { members });
    }

    /// A struct of variables, each of the role's causality.
    fn read_struct(&mut self, s: &Struct, role: Role) {
        let dims: BTreeSet<String> = self.dims.iter().map(|d| d.name.clone()).collect();
        let mut members = Vec::with_capacity(s.members.len());
        for slot in &s.members {
            let name = slot.name.clone().unwrap_or_default();
            let ty = match &slot.ty {
                ParsedType::Scalar(t @ CType { base: Some(_), .. }) => *t,
                _ => {
                    self.deviate(format!(
                        "{}: {name} is not an admitted scalar or a pointer to one",
                        s.name
                    ));
                    continue;
                }
            };
            let comment = slot.comment.as_deref().unwrap_or("");
            let (shape, rest) = match shape(comment, &dims) {
                Ok(parsed) => parsed,
                Err(e) => {
                    self.deviate(format!("{}: {name}: {e}", s.name));
                    continue;
                }
            };
            if ty.pointer && shape.is_empty() {
                self.deviate(format!(
                    "{}: {name} is a pointer without a shape comment such as `/* [n] */`",
                    s.name
                ));
                continue;
            }
            if !ty.pointer && !shape.is_empty() {
                self.deviate(format!(
                    "{}: {name} has a shape but is not a pointer",
                    s.name
                ));
                continue;
            }
            if let Some(other) = self.variables.iter().find(|v| v.name == name) {
                let causality = other.causality;
                self.deviate(format!(
                    "{}: {name} is also a {causality:?} variable; member names must be unique",
                    s.name
                ));
                continue;
            }
            let (unit, description) = unit_description(rest);
            self.variables.push(Variable {
                name: name.clone(),
                causality: role.causality(),
                ty: ty.base.unwrap_or(ScalarType::F64),
                shape,
                layout: Layout::RowMajor,
                unit,
                description,
            });
            members.push(Member {
                variable: Some(name.clone()),
                ..member(&name, ty)
            });
        }
        self.structs.insert(s.name.clone(), StructSpec { members });
    }

    /// `int f(<head>, <struct pointers of `roles`, any order>, <tail>)`.
    fn call(&mut self, f: &Function, head: &[Fixed], tail: &[Fixed], roles: &[Role]) -> Call {
        let m = self.m.clone();
        let expected = |r: &Self| {
            let structs = roles
                .iter()
                .filter(|role| r.roles.contains_key(role))
                .map(|role| {
                    let constness = if *role == Role::Outputs { "" } else { "const " };
                    format!("{constness}{m}_{} *", role.suffix())
                });
            let params: Vec<String> = head
                .iter()
                .map(|x| x.spelled().to_owned())
                .chain(structs)
                .chain(tail.iter().map(|x| x.spelled().to_owned()))
                .collect();
            format!("int {}({})", f.name, params.join(", "))
        };
        if !is_int(&f.returns) {
            self.deviate(format!("{} does not return int", f.name));
        }
        let n = f.params.len();
        let fixed = head.len() + tail.len();
        let fits = n >= fixed
            && head.iter().zip(&f.params).all(|(x, p)| x.matches(&p.ty))
            && tail
                .iter()
                .zip(&f.params[n.saturating_sub(tail.len())..])
                .all(|(x, p)| x.matches(&p.ty));
        if !fits {
            let want = expected(self);
            self.deviate(format!("{}: expected {want}", f.name));
            return Call {
                symbol: f.name.clone(),
                returns: Returns::Int,
                args: Vec::new(),
            };
        }
        let mut args: Vec<Arg> = head.iter().map(|x| x.arg()).collect();
        let mut seen = BTreeSet::new();
        for p in &f.params[head.len()..n - tail.len()] {
            let role = match &p.ty {
                ParsedType::Struct {
                    name,
                    pointer: true,
                } => self
                    .roles
                    .iter()
                    .find(|(role, s)| s.name == *name && roles.contains(role))
                    .map(|(role, _)| *role),
                _ => None,
            };
            match role {
                Some(role) if seen.insert(role) => args.push(Arg {
                    struct_: Some(format!("{m}_{}", role.suffix())),
                    ..Arg::default()
                }),
                _ => {
                    let want = expected(self);
                    self.deviate(format!(
                        "{}: parameter {} does not fit; expected {want}",
                        f.name,
                        p.name.as_deref().unwrap_or("?")
                    ));
                }
            }
        }
        for role in roles {
            if self.roles.contains_key(role) && !seen.contains(role) {
                self.deviate(format!("{} does not take {m}_{}", f.name, role.suffix()));
            }
        }
        args.extend(tail.iter().map(|x| x.arg()));
        Call {
            symbol: f.name.clone(),
            returns: Returns::Int,
            args,
        }
    }

    /// `void|int <m>_terminate(void *h)`.
    fn terminate(&mut self, f: &Function) -> Call {
        let returns = if f.returns == ParsedType::Void {
            Returns::Void
        } else {
            if !is_int(&f.returns) {
                self.deviate(format!("{} returns neither int nor void", f.name));
            }
            Returns::Int
        };
        if f.params.len() != 1 || !Fixed::HandleIn.matches(&f.params[0].ty) {
            self.deviate(format!("{}: expected {}(void *h)", f.name, f.name));
        }
        Call {
            symbol: f.name.clone(),
            returns,
            args: vec![Fixed::HandleIn.arg()],
        }
    }
}

fn is_int(ty: &ParsedType) -> bool {
    *ty == ParsedType::Scalar(CType {
        base: Some(ScalarType::I32),
        pointer: false,
    })
}

/// A leading shape `[a][b]…` in dimension names or literal lengths, and the text after it.
fn shape<'t>(comment: &'t str, dims: &BTreeSet<String>) -> Result<(Vec<Dim>, &'t str), String> {
    let mut rest = comment.trim_start();
    let mut out = Vec::new();
    while let Some(open) = rest.strip_prefix('[') {
        let Some((inner, after)) = open.split_once(']') else {
            return Err(format!("unclosed `[` in `{comment}`"));
        };
        let inner = inner.trim();
        match inner.parse::<usize>() {
            Ok(0) => return Err(format!("zero length in `{comment}`")),
            Ok(n) => out.push(Dim::Literal(n)),
            Err(_) if dims.contains(inner) => out.push(Dim::Symbol(inner.to_owned())),
            Err(_) => {
                return Err(format!(
                    "`[{inner}]` is neither a member of the dims struct nor a length"
                ));
            }
        }
        rest = after.trim_start();
    }
    Ok((out, rest))
}

/// `unit: description`; without a colon one word is a unit, more words a description.
fn unit_description(text: &str) -> (Option<String>, Option<String>) {
    let some = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
    match text.split_once(':') {
        Some((unit, description)) => (some(unit), some(description)),
        None if text.trim().contains(char::is_whitespace) => (None, some(text)),
        None => (some(text), None),
    }
}

type Bounds = (Option<usize>, Option<usize>, Option<usize>);

/// `[min]..[max] [= default] [: text]` on a dims member; a comment of words only is a
/// description and bounds nothing.
fn bounds(comment: &str) -> Result<Bounds, String> {
    let head = comment.split_once(':').map_or(comment, |(h, _)| h).trim();
    if !head.contains(|c: char| c.is_ascii_digit() || c == '.' || c == '=') {
        return Ok((None, None, None));
    }
    let bad = || format!("`{comment}` is not `min..max = default`");
    let number = |s: &str| -> Result<Option<usize>, String> {
        let s = s.trim();
        if s.is_empty() {
            Ok(None)
        } else {
            s.parse().map(Some).map_err(|_| bad())
        }
    };
    let (range, default) = match head.split_once('=') {
        Some((range, default)) => (range.trim(), number(default)?),
        None => (head, None),
    };
    if default.is_none() && head.contains('=') {
        return Err(bad());
    }
    let (min, max) = if range.is_empty() {
        (None, None)
    } else {
        let (min, max) = range.split_once("..").ok_or_else(bad)?;
        (number(min)?, number(max)?)
    };
    Ok((min, max, default))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{ImportOptions, parse_header, propose_with};

    const SHAPE: &str = r"
#include <stdint.h>
typedef struct {
    int nx; /* 1..64 = 4 */
    int nu; // 1..
} ss_dims;

typedef struct {
    const double *A;   /* [nx][nx] */
    const double *B;   /* [nx][nu] */
    const double *c;   /* [nx] */
    const double *w;   /* [3] */
    char *title;       /* [16] */
} ss_params;

typedef struct {
    const double *u;   /* [nu] m/s: inflow */
} ss_inputs;

typedef struct {
    double k;          /* gain on the output */
    double offset;     /* m */
} ss_tunables;

typedef struct {
    double *x;         /* [nx] */
    double y;
} ss_outputs;

int ss_init(void **h, const ss_dims *dims, const ss_params *params, double step_size);
int ss_step(void *h, double time, const ss_inputs *in, const ss_tunables *tun,
            ss_outputs *out);
void ss_terminate(void *h);
";

    fn import(text: &str) -> crate::Proposal {
        let header = parse_header(text, "ss.h").unwrap();
        propose_with(&header, "ss", &ImportOptions::default()).unwrap()
    }

    fn var<'d>(d: &'d Descriptor, name: &str) -> &'d Variable {
        d.interface
            .variables
            .iter()
            .find(|v| v.name == name)
            .unwrap()
    }

    #[test]
    fn the_recommended_shape_is_read_confirmed() {
        let p = import(SHAPE);
        assert!(p.from_shape, "{:?}", p.notes);
        let d = &p.descriptor;
        assert!(d.abi.confirmed);
        assert_eq!(d.interface.instances, Instances::Multiple);
        assert_eq!(
            d.interface.dimensions,
            vec![
                Dimension {
                    name: "nx".to_owned(),
                    min: Some(1),
                    max: Some(64),
                    default: Some(4),
                },
                Dimension {
                    name: "nu".to_owned(),
                    min: Some(1),
                    max: None,
                    default: None,
                },
            ]
        );
        let sym = |s: &str| Dim::Symbol(s.to_owned());
        assert_eq!(var(d, "A").shape, vec![sym("nx"), sym("nx")]);
        assert_eq!(var(d, "B").shape, vec![sym("nx"), sym("nu")]);
        assert_eq!(var(d, "w").shape, vec![Dim::Literal(3)]);
        assert_eq!(var(d, "title").ty, ScalarType::U8);
        assert_eq!(var(d, "A").causality, Causality::Parameter);
        assert_eq!(var(d, "u").causality, Causality::Input);
        assert_eq!(var(d, "u").unit.as_deref(), Some("m/s"));
        assert_eq!(var(d, "u").description.as_deref(), Some("inflow"));
        assert_eq!(var(d, "k").causality, Causality::Tunable);
        assert_eq!(
            var(d, "k").description.as_deref(),
            Some("gain on the output")
        );
        assert_eq!(var(d, "offset").unit.as_deref(), Some("m"));
        assert_eq!(var(d, "x").causality, Causality::Output);
        assert_eq!(var(d, "y").causality, Causality::Output);
        assert!(var(d, "y").shape.is_empty());
        assert_eq!(d.abi.init.args.len(), 4);
        assert_eq!(d.abi.step.args.len(), 5);
        assert_eq!(d.abi.terminate.as_ref().unwrap().returns, Returns::Void);
        assert_eq!(
            d.abi.structs["ss_dims"].members[0].dim.as_deref(),
            Some("nx")
        );

        let text = p.to_toml().unwrap();
        assert!(text.starts_with("# Read by `taktwerk import-header` from the recommended shape"));
        let back = Descriptor::parse(&text).unwrap();
        assert_eq!(&back, d);
        back.validate().unwrap();
    }

    #[test]
    fn params_and_tunables_may_be_absent_and_structs_come_in_any_order() {
        let p = import(
            "typedef struct { int n; } f_dims;
             typedef struct { const double *u; /* [n] */ } f_inputs;
             typedef struct { double *y; /* [n] */ } f_outputs;
             int f_init(void **h, const f_dims *d, double step_size);
             int f_step(void *h, double t, f_outputs *out, const f_inputs *in);
             int f_terminate(void *h);",
        );
        assert!(p.from_shape, "{:?}", p.notes);
        let step = &p.descriptor.abi.step;
        assert_eq!(step.args[2].struct_.as_deref(), Some("f_outputs"));
        assert_eq!(step.args[3].struct_.as_deref(), Some("f_inputs"));
    }

    /// The deviation notes of `text`, imported in auto mode.
    fn deviations(text: &str) -> Vec<String> {
        let p = import(text);
        assert!(!p.from_shape);
        assert!(!p.descriptor.abi.confirmed);
        p.notes
            .into_iter()
            .filter_map(|n| {
                n.strip_prefix("not the recommended shape: ")
                    .map(str::to_owned)
            })
            .collect()
    }

    #[test]
    fn a_pointer_without_a_shape_comment_deviates() {
        let text = SHAPE.replace("const double *c;   /* [nx] */", "const double *c;");
        assert_eq!(
            deviations(&text),
            vec![
                "ss_params: c is a pointer without a shape comment such as `/* [n] */`".to_owned()
            ]
        );
    }

    #[test]
    fn an_unknown_dimension_in_a_shape_deviates() {
        let text = SHAPE.replace("/* [nx][nu] */", "/* [nx][nz] */");
        assert_eq!(
            deviations(&text),
            vec![
                "ss_params: B: `[nz]` is neither a member of the dims struct nor a length"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn a_pointer_in_dims_deviates() {
        let text = SHAPE.replace("int nu; // 1..", "int *nu;");
        let got = deviations(&text);
        assert_eq!(
            got[0],
            "ss_dims: nu is not an integer by value (one `int` per dimension)"
        );
        // Shapes naming the missing dimension deviate too.
        assert!(got.iter().any(|d| d.contains("`[nu]`")), "{got:?}");
    }

    #[test]
    fn unknown_suffixes_signatures_and_bounds_deviate() {
        let text = SHAPE
            .replace("} ss_tunables;", "} ss_gains;")
            .replace("const ss_tunables *tun", "const ss_gains *tun")
            .replace("int nx; /* 1..64 = 4 */", "int nx; /* 1-64 */")
            .replace("void ss_terminate(void *h);", "void ss_terminate(void);");
        let got = deviations(&text);
        assert_eq!(
            got,
            vec![
                "struct ss_gains: not one of ss_dims, ss_params, ss_inputs, ss_tunables, \
                 ss_outputs",
                "ss_dims: nx: `1-64` is not `min..max = default`",
                "ss_step: parameter tun does not fit; expected int ss_step(void *h, \
                 double time, const ss_inputs *, ss_outputs *)",
                "ss_terminate: expected ss_terminate(void *h)",
            ]
        );
    }

    #[test]
    fn a_header_of_another_interface_gets_one_note() {
        let got = deviations(
            "typedef struct { double kp; } pi_gains;
             int pi_init(void **h, int n, double dt);
             int pi_step(void *h, const pi_gains *g, const double *sp, double *u);",
        );
        assert_eq!(
            got,
            vec![
                "no struct named `pi_dims`, `pi_params`, `pi_inputs`, `pi_tunables` or \
                 `pi_outputs`"
            ]
        );
    }

    #[test]
    fn requiring_the_shape_fails_with_the_deviations() {
        let text = SHAPE.replace("const double *c;   /* [nx] */", "const double *c;");
        let header = parse_header(&text, "ss.h").unwrap();
        let options = ImportOptions {
            require_shape: true,
            ..ImportOptions::default()
        };
        let err = propose_with(&header, "ss", &options).unwrap_err();
        assert_eq!(
            err.0,
            "not the recommended shape:\n  - ss_params: c is a pointer without a shape comment \
             such as `/* [n] */`"
        );
        let header = parse_header(SHAPE, "ss.h").unwrap();
        assert!(propose_with(&header, "ss", &options).unwrap().from_shape);
    }

    #[test]
    fn bounds_comments() {
        assert_eq!(bounds("1..64"), Ok((Some(1), Some(64), None)));
        assert_eq!(
            bounds("1..64 = 4: channels"),
            Ok((Some(1), Some(64), Some(4)))
        );
        assert_eq!(bounds("..8"), Ok((None, Some(8), None)));
        assert_eq!(bounds("= 2"), Ok((None, None, Some(2))));
        assert_eq!(bounds("number of channels"), Ok((None, None, None)));
        assert!(bounds("1..x").is_err());
        assert!(bounds("1..4 =").is_err());
    }
}
