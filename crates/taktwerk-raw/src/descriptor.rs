//! The package descriptor (`taktwerk-model.toml`): the interface plus the `[abi]` section, and
//! its validation into an index-resolved [`Plan`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use taktwerk_core::model::{Causality, Instances, ModelInterface};
use taktwerk_core::value::{Dim, ScalarType};

use crate::layout::{Carrier, StructLayout};

/// File name of the descriptor inside a package directory.
pub const DESCRIPTOR_FILE: &str = "taktwerk-model.toml";

/// Register-class limits of the call shim (`ffi`): pointer and integer arguments, and
/// floating-point arguments, each counted separately.
pub const MAX_INT_ARGS: usize = 8;
/// See [`MAX_INT_ARGS`].
pub const MAX_FLOAT_ARGS: usize = 8;

/// A whole descriptor file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Descriptor {
    /// The size-generic interface (`name`, `dimensions`, `variables`, `instances`).
    #[serde(flatten)]
    pub interface: ModelInterface,
    /// How the library is called.
    pub abi: Abi,
}

/// `[abi]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Abi {
    /// Set by the model developer after checking every pointer→length relation. An unconfirmed
    /// descriptor is refused at load.
    #[serde(default)]
    pub confirmed: bool,
    /// Library file name inside `lib/<arch>/`; defaults to the only `.so` there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
    /// Return values that mean success for calls returning `int`.
    #[serde(default = "default_ok_codes")]
    pub ok_codes: Vec<i32>,
    /// Called once per instance before the first step.
    pub init: Call,
    /// Called once per step.
    pub step: Call,
    /// Called once when the instance is released.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<Call>,
    /// Structs passed by pointer, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub structs: BTreeMap<String, StructSpec>,
}

fn default_ok_codes() -> Vec<i32> {
    vec![0]
}

/// One C function and how its arguments are filled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    /// Exported symbol name.
    pub symbol: String,
    /// Return type.
    #[serde(default)]
    pub returns: Returns,
    /// Arguments in C order.
    #[serde(default)]
    pub args: Vec<Arg>,
}

/// Return type of a call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Returns {
    /// `int`; a value outside `ok_codes` is an error.
    #[default]
    Int,
    /// `void`; never fails.
    Void,
}

/// One argument. Exactly one of `struct`, `array`, `value`, `dim`, `builtin`, `handle`,
/// `const`, `phase` is set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arg {
    /// Pointer to the named struct (`[abi.structs.<name>]`).
    #[serde(rename = "struct", default, skip_serializing_if = "Option::is_none")]
    pub struct_: Option<String>,
    /// Pointer to the variable's buffer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array: Option<String>,
    /// A scalar variable by value (input, parameter or tunable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// A dimension's bound length by value; `type` gives the C integer type (default `int`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim: Option<String>,
    /// A value the engine supplies, `double` by value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<Builtin>,
    /// The instance handle of a `multiple` library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<Handle>,
    /// The same number on every call; `type` gives the C type (default `int`).
    #[serde(rename = "const", default, skip_serializing_if = "Option::is_none")]
    pub const_: Option<Number>,
    /// One number for init, another for step (and terminate); `type` as for `const`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<PhaseValues>,
    /// C type of a `dim`, `const` or `phase` argument. A pointer type (`int *`) passes the
    /// address of engine-owned storage holding the value.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub ty: Option<CType>,
}

/// A number in the descriptor, converted to the C type it is written as.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Number {
    /// An integer literal; admitted for every type that can hold it.
    Int(i64),
    /// A floating-point literal; admitted for `double` and `float` only.
    Float(f64),
}

/// `phase = { init = …, step = … }`: the value of the init call and of every later call.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseValues {
    /// Value on the init call.
    pub init: Number,
    /// Value on step and terminate calls.
    pub step: Number,
}

/// A value the engine supplies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Builtin {
    /// The instance's communication step, seconds.
    StepSize,
    /// The time the call advances from, seconds.
    Time,
}

/// Direction of a handle argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Handle {
    /// `void **`: the library stores its instance handle through it (init only).
    Out,
    /// `void *`: the stored handle (step and terminate).
    In,
}

/// `[abi.structs.<name>]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructSpec {
    /// Members in C declaration order.
    pub members: Vec<Member>,
}

/// One struct member. At most one of `variable`, `dim`, `builtin`, `const`, `phase` is set; an
/// unmapped pointer is `NULL`, an unmapped scalar stays zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    /// Member name.
    pub name: String,
    /// C type, e.g. `"double *"`, `"int"`.
    #[serde(rename = "type")]
    pub ty: CType,
    /// Variable: a pointer member gets the buffer's address, a scalar member the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
    /// A dimension's bound length (scalar integer member).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim: Option<String>,
    /// A value the engine supplies (`double` member).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<Builtin>,
    /// The same number on every call. A pointer member points at engine-owned storage.
    #[serde(rename = "const", default, skip_serializing_if = "Option::is_none")]
    pub const_: Option<Number>,
    /// One number for init, another for step and terminate. A pointer member points at
    /// engine-owned storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<PhaseValues>,
    /// With `dim`: the library writes the length (zeroed before init, compared with the bound
    /// length after init and after every step) instead of reading it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reported: bool,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if signature"
)]
const fn is_false(b: &bool) -> bool {
    !*b
}

/// A number resolved to its C type: the value's bits, two's complement for integers, IEEE for
/// floats (`float` in the low 32 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CValue {
    /// C type.
    pub ty: ScalarType,
    /// The value's bits.
    pub bits: u64,
}

impl CValue {
    /// Convert `n` to `ty`.
    ///
    /// # Errors
    /// A value out of range for an integer type, a fraction for an integer type, or a `bool`
    /// other than 0 or 1.
    pub fn new(ty: ScalarType, n: Number) -> Result<Self, String> {
        let int_range = |lo: i128, hi: i128, v: i64| -> Result<u64, String> {
            if (lo..=hi).contains(&i128::from(v)) {
                Ok(v as u64)
            } else {
                Err(format!("{v} does not fit a {ty:?}"))
            }
        };
        let bits = match (ty, n) {
            (ScalarType::F64, Number::Float(f)) => f.to_bits(),
            (ScalarType::F64, Number::Int(i)) => (i as f64).to_bits(),
            (ScalarType::F32, Number::Float(f)) => u64::from((f as f32).to_bits()),
            (ScalarType::F32, Number::Int(i)) => u64::from((i as f32).to_bits()),
            (_, Number::Float(f)) => return Err(format!("{f} is not an integer, as {ty:?} needs")),
            (ScalarType::I64, Number::Int(i)) => i as u64,
            (ScalarType::U64, Number::Int(i)) => int_range(0, i128::from(u64::MAX), i)?,
            (ScalarType::I32, Number::Int(i)) => int_range(i32::MIN.into(), i32::MAX.into(), i)?,
            (ScalarType::U32, Number::Int(i)) => int_range(0, u32::MAX.into(), i)?,
            (ScalarType::I16, Number::Int(i)) => int_range(i16::MIN.into(), i16::MAX.into(), i)?,
            (ScalarType::U16, Number::Int(i)) => int_range(0, u16::MAX.into(), i)?,
            (ScalarType::I8, Number::Int(i)) => int_range(i8::MIN.into(), i8::MAX.into(), i)?,
            (ScalarType::U8, Number::Int(i)) => int_range(0, u8::MAX.into(), i)?,
            (ScalarType::Bool, Number::Int(i)) => int_range(0, 1, i)?,
        };
        Ok(Self { ty, bits })
    }

    /// Write the value's native bytes into `slot`; `false` when `slot` is not exactly as wide.
    #[must_use]
    pub fn write(self, slot: &mut [u8]) -> bool {
        macro_rules! put {
            ($v:expr) => {{
                let bytes = $v.to_ne_bytes();
                if bytes.len() != slot.len() {
                    return false;
                }
                slot.copy_from_slice(&bytes);
                true
            }};
        }
        let b = self.bits;
        match self.ty {
            ScalarType::F64 | ScalarType::I64 | ScalarType::U64 => put!(b),
            ScalarType::F32 | ScalarType::I32 | ScalarType::U32 => put!(b as u32),
            ScalarType::I16 | ScalarType::U16 => put!(b as u16),
            ScalarType::I8 | ScalarType::U8 | ScalarType::Bool => put!(b as u8),
        }
    }

    /// Register image when passed by value: `(is_float, bits)`. Narrow signed integers are
    /// sign-extended to 32 bits, as C requires.
    #[must_use]
    pub const fn reg(self) -> (bool, u64) {
        let b = self.bits;
        match self.ty {
            ScalarType::F64 | ScalarType::F32 => (true, b),
            ScalarType::I32 | ScalarType::I16 | ScalarType::I8 => {
                (false, (b as i64 as i32) as u32 as u64)
            }
            _ => (false, b),
        }
    }
}

/// A value that may differ between the init call and the calls after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phased {
    /// On the init call.
    pub init: CValue,
    /// On step and terminate calls.
    pub step: CValue,
}

/// A C type the descriptor admits: an optional scalar base (`None` is `void`) and at most one
/// level of pointer. Serialized as its C spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CType {
    /// Scalar base; `None` for `void`.
    pub base: Option<ScalarType>,
    /// `T *` rather than `T`.
    pub pointer: bool,
}

/// C spellings (after dropping `const`, whitespace-normalized) and their scalar type.
/// Widths are those of 64-bit Linux (LP64); `char` is treated as a byte.
const C_SPELLINGS: &[(&str, ScalarType)] = &[
    ("double", ScalarType::F64),
    ("float", ScalarType::F32),
    ("int", ScalarType::I32),
    ("signed int", ScalarType::I32),
    ("int32_t", ScalarType::I32),
    ("unsigned", ScalarType::U32),
    ("unsigned int", ScalarType::U32),
    ("uint32_t", ScalarType::U32),
    ("long", ScalarType::I64),
    ("long int", ScalarType::I64),
    ("long long", ScalarType::I64),
    ("int64_t", ScalarType::I64),
    ("ssize_t", ScalarType::I64),
    ("unsigned long", ScalarType::U64),
    ("unsigned long long", ScalarType::U64),
    ("uint64_t", ScalarType::U64),
    ("size_t", ScalarType::U64),
    ("short", ScalarType::I16),
    ("short int", ScalarType::I16),
    ("int16_t", ScalarType::I16),
    ("unsigned short", ScalarType::U16),
    ("uint16_t", ScalarType::U16),
    ("int8_t", ScalarType::I8),
    ("signed char", ScalarType::I8),
    ("uint8_t", ScalarType::U8),
    ("unsigned char", ScalarType::U8),
    ("char", ScalarType::U8),
    ("bool", ScalarType::Bool),
    ("_Bool", ScalarType::Bool),
];

impl CType {
    /// Parse a C spelling such as `const double *` or `int32_t`.
    ///
    /// # Errors
    /// An unknown base type, more than one `*`, or `void` by value.
    pub fn parse(text: &str) -> Result<Self, String> {
        let stars = text.chars().filter(|c| *c == '*').count();
        if stars > 1 {
            return Err(format!("`{text}`: a pointer to a pointer"));
        }
        let words: Vec<&str> = text
            .split(|c: char| c.is_whitespace() || c == '*')
            .filter(|w| !w.is_empty() && *w != "const" && *w != "volatile")
            .collect();
        let base = words.join(" ");
        if base == "void" {
            if stars == 0 {
                return Err("`void` by value".to_owned());
            }
            return Ok(Self {
                base: None,
                pointer: true,
            });
        }
        let scalar = C_SPELLINGS
            .iter()
            .find(|(spelling, _)| *spelling == base)
            .map(|(_, ty)| *ty)
            .ok_or_else(|| format!("`{text}`: unknown C type"))?;
        Ok(Self {
            base: Some(scalar),
            pointer: stars == 1,
        })
    }

    /// Whether the base is an integer or bool type (usable for a dimension length).
    #[must_use]
    pub const fn is_integer(self) -> bool {
        matches!(
            self.base,
            Some(
                ScalarType::I64
                    | ScalarType::I32
                    | ScalarType::I16
                    | ScalarType::I8
                    | ScalarType::U64
                    | ScalarType::U32
                    | ScalarType::U16
                    | ScalarType::U8
            )
        )
    }
}

impl fmt::Display for CType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = match self.base {
            None => "void",
            Some(ScalarType::F64) => "double",
            Some(ScalarType::F32) => "float",
            Some(ScalarType::I64) => "int64_t",
            Some(ScalarType::I32) => "int32_t",
            Some(ScalarType::I16) => "int16_t",
            Some(ScalarType::I8) => "int8_t",
            Some(ScalarType::U64) => "uint64_t",
            Some(ScalarType::U32) => "uint32_t",
            Some(ScalarType::U16) => "uint16_t",
            Some(ScalarType::U8) => "uint8_t",
            Some(ScalarType::Bool) => "bool",
        };
        if self.pointer {
            write!(f, "{base} *")
        } else {
            f.write_str(base)
        }
    }
}

impl Serialize for CType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// A descriptor that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("descriptor: {0}")]
pub struct DescriptorError(pub String);

// ==========================================================================
// Validation: names to indices, structs to layouts.
// ==========================================================================

/// A validated descriptor, every name resolved to an index into the interface.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Structs in `[abi.structs]` key order (the order [`StructRef`] indexes).
    pub structs: Vec<ResolvedStruct>,
    /// The init call.
    pub init: ResolvedCall,
    /// The step call.
    pub step: ResolvedCall,
    /// The terminate call, if declared.
    pub terminate: Option<ResolvedCall>,
    /// Indices of `Input` variables, interface order.
    pub inputs: Vec<usize>,
    /// Indices of `Output` variables, interface order.
    pub outputs: Vec<usize>,
    /// Indices of `Tunable` variables, interface order.
    pub tunables: Vec<usize>,
    /// Engine-owned storage cells (one 8-byte scalar each) that pointer members and arguments
    /// carrying a `const`, `phase` or `dim` point at.
    pub cells: usize,
}

/// A struct with its layout and the meaning of every member.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedStruct {
    /// Struct name.
    pub name: String,
    /// Offsets and widths.
    pub layout: StructLayout,
    /// One role per member, layout order.
    pub members: Vec<MemberRole>,
}

/// What a struct member holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRole {
    /// Address of the variable's buffer, or `NULL`.
    Pointer(Option<usize>),
    /// The scalar variable's value, synced before (non-output) or after (output) each call.
    Value(usize),
    /// A dimension's bound length.
    Dim(usize, ScalarType),
    /// An engine-supplied `double`.
    Builtin(Builtin),
    /// Unmapped scalar, zero.
    Scratch,
    /// A `const` or `phase` value, in the struct (`cell = None`) or in storage cell `cell`
    /// the member points at.
    Fixed {
        /// The value per phase.
        value: Phased,
        /// Storage cell for a pointer member.
        cell: Option<usize>,
    },
    /// A pointer to storage cell `cell` holding dimension `dim`'s bound length as `ty`.
    DimPointer {
        /// Dimension index.
        dim: usize,
        /// C integer type.
        ty: ScalarType,
        /// Storage cell.
        cell: usize,
    },
    /// A length the library writes: in the struct (`cell = None`) or in storage cell `cell`.
    /// Zeroed before init, compared with dimension `dim`'s bound length after each call.
    Reported {
        /// Dimension index.
        dim: usize,
        /// C integer type.
        ty: ScalarType,
        /// Storage cell for a pointer member.
        cell: Option<usize>,
    },
}

/// A call with its arguments resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCall {
    /// Exported symbol name.
    pub symbol: String,
    /// Return type.
    pub returns: Returns,
    /// Arguments in C order.
    pub args: Vec<ArgRole>,
}

/// What an argument carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgRole {
    /// Pointer to struct image `i`.
    Struct(usize),
    /// Pointer to variable `v`'s buffer.
    Array(usize),
    /// Scalar variable `v` by value.
    Value(usize),
    /// Dimension `d`'s bound length as the given integer type.
    Dim(usize, ScalarType),
    /// An engine-supplied `double`.
    Builtin(Builtin),
    /// The instance handle.
    Handle(Handle),
    /// A `const` or `phase` value by value (`cell = None`), or the address of storage cell
    /// `cell` holding it.
    Fixed {
        /// The value per phase.
        value: Phased,
        /// Storage cell for a pointer argument.
        cell: Option<usize>,
    },
}

/// `int`, the default type of `dim`, `const` and `phase` arguments.
const C_INT: CType = CType {
    base: Some(ScalarType::I32),
    pointer: false,
};

/// Whether a C scalar `c` carries a variable of type `var`: the same type, or a byte for a
/// `bool` (kept as bytes and normalised, any non-zero byte is true) and the reverse.
fn carries(c: ScalarType, var: ScalarType) -> bool {
    c == var
        || matches!(
            (c, var),
            (ScalarType::U8, ScalarType::Bool) | (ScalarType::Bool, ScalarType::U8)
        )
}

/// Hand out the next storage cell.
const fn next_cell(cells: &mut usize) -> usize {
    let cell = *cells;
    *cells += 1;
    cell
}

/// A `const` or `phase` resolved against the C type, or `None` when neither is set.
fn phased(
    constant: Option<Number>,
    phase: Option<PhaseValues>,
    ty: CType,
) -> Result<Option<Phased>, String> {
    let (init, step) = match (constant, phase) {
        (Some(c), None) => (c, c),
        (None, Some(p)) => (p.init, p.step),
        (None, None) => return Ok(None),
        (Some(_), Some(_)) => return Err("both const and phase".to_owned()),
    };
    let Some(base) = ty.base else {
        return Err("a value needs a scalar type, not void *".to_owned());
    };
    Ok(Some(Phased {
        init: CValue::new(base, init)?,
        step: CValue::new(base, step)?,
    }))
}

/// Index helper for `[abi.structs]`.
pub type StructRef = usize;

impl Descriptor {
    /// Parse a descriptor from TOML text (no validation).
    ///
    /// # Errors
    /// Malformed TOML or unknown keys.
    pub fn parse(text: &str) -> Result<Self, DescriptorError> {
        toml::from_str(text).map_err(|e| DescriptorError(e.to_string()))
    }

    /// Serialize to TOML text.
    ///
    /// # Errors
    /// A value TOML cannot carry (does not happen for a parsed descriptor).
    pub fn to_toml(&self) -> Result<String, DescriptorError> {
        toml::to_string_pretty(self).map_err(|e| DescriptorError(e.to_string()))
    }

    /// Check the descriptor and resolve every name.
    ///
    /// # Errors
    /// Unconfirmed, unknown names, type mismatches, outputs by value, handle misuse, or more
    /// arguments than the call shim passes.
    pub fn validate(&self) -> Result<Plan, DescriptorError> {
        let fail = |s: String| Err(DescriptorError(s));
        if !self.abi.confirmed {
            return fail(
                "abi.confirmed is false: check every pointer→length relation and set it to true"
                    .to_owned(),
            );
        }
        let iface = &self.interface;
        let dims: BTreeMap<&str, usize> = iface
            .dimensions
            .iter()
            .enumerate()
            .map(|(i, d)| (d.name.as_str(), i))
            .collect();
        if dims.len() != iface.dimensions.len() {
            return fail("a dimension is declared twice".to_owned());
        }
        for d in &iface.dimensions {
            if d.name.is_empty() {
                return fail("a dimension has an empty name".to_owned());
            }
            if let (Some(min), Some(max)) = (d.min, d.max) {
                if min > max {
                    return fail(format!("dimension {}: min {min} > max {max}", d.name));
                }
            }
            if let Some(default) = d.default {
                if d.min.is_some_and(|m| default < m) || d.max.is_some_and(|m| default > m) {
                    return fail(format!(
                        "dimension {}: default {default} out of range",
                        d.name
                    ));
                }
            }
        }
        let vars: BTreeMap<&str, usize> = iface
            .variables
            .iter()
            .enumerate()
            .map(|(i, v)| (v.name.as_str(), i))
            .collect();
        if vars.len() != iface.variables.len() {
            return fail("a variable is declared twice".to_owned());
        }
        for v in &iface.variables {
            if v.name.is_empty() {
                return fail("a variable has an empty name".to_owned());
            }
            for dim in &v.shape {
                if let Dim::Symbol(s) = dim {
                    if !dims.contains_key(s.as_str()) {
                        return fail(format!("variable {}: unknown dimension {s}", v.name));
                    }
                }
            }
        }

        let struct_index: BTreeMap<&str, usize> = self
            .abi
            .structs
            .keys()
            .enumerate()
            .map(|(i, k)| (k.as_str(), i))
            .collect();
        let mut cells = 0_usize;
        let mut structs = Vec::with_capacity(self.abi.structs.len());
        for (name, spec) in &self.abi.structs {
            structs.push(self.resolve_struct(name, spec, &vars, &dims, &mut cells)?);
        }

        let init = self.resolve_call(
            "init",
            &self.abi.init,
            &struct_index,
            &vars,
            &dims,
            &mut cells,
        )?;
        let step = self.resolve_call(
            "step",
            &self.abi.step,
            &struct_index,
            &vars,
            &dims,
            &mut cells,
        )?;
        let terminate = self
            .abi
            .terminate
            .as_ref()
            .map(|c| self.resolve_call("terminate", c, &struct_index, &vars, &dims, &mut cells))
            .transpose()?;

        let handle_out = |c: &ResolvedCall| {
            c.args
                .iter()
                .filter(|a| **a == ArgRole::Handle(Handle::Out))
                .count()
        };
        let handle_in = |c: &ResolvedCall| c.args.contains(&ArgRole::Handle(Handle::In));
        let has_out = handle_out(&init);
        if has_out > 1 {
            return fail("init: more than one `handle = \"out\"` argument".to_owned());
        }
        if handle_out(&step) > 0 || terminate.as_ref().is_some_and(|t| handle_out(t) > 0) {
            return fail("`handle = \"out\"` belongs to init only".to_owned());
        }
        if handle_in(&init) {
            return fail("init: `handle = \"in\"` before any handle exists".to_owned());
        }
        if has_out == 0 && (handle_in(&step) || terminate.as_ref().is_some_and(handle_in)) {
            return fail("`handle = \"in\"` without a `handle = \"out\"` in init".to_owned());
        }
        if has_out == 1 && iface.instances != Instances::Multiple {
            return fail("a handle argument needs `instances = \"multiple\"`".to_owned());
        }

        let by_causality = |c: Causality| -> Vec<usize> {
            iface
                .variables
                .iter()
                .enumerate()
                .filter(|(_, v)| v.causality == c)
                .map(|(i, _)| i)
                .collect()
        };
        Ok(Plan {
            structs,
            init,
            step,
            terminate,
            inputs: by_causality(Causality::Input),
            outputs: by_causality(Causality::Output),
            tunables: by_causality(Causality::Tunable),
            cells,
        })
    }

    fn resolve_struct(
        &self,
        name: &str,
        spec: &StructSpec,
        vars: &BTreeMap<&str, usize>,
        dims: &BTreeMap<&str, usize>,
        cells: &mut usize,
    ) -> Result<ResolvedStruct, DescriptorError> {
        let fail = |s: String| DescriptorError(format!("struct {name}: {s}"));
        let mut seen = BTreeSet::new();
        let mut roles = Vec::with_capacity(spec.members.len());
        let mut carriers = Vec::with_capacity(spec.members.len());
        for m in &spec.members {
            if !seen.insert(m.name.as_str()) {
                return Err(fail(format!("member {} declared twice", m.name)));
            }
            let mapped = usize::from(m.variable.is_some())
                + usize::from(m.dim.is_some())
                + usize::from(m.builtin.is_some())
                + usize::from(m.const_.is_some())
                + usize::from(m.phase.is_some());
            if mapped > 1 {
                return Err(fail(format!(
                    "member {}: at most one of variable, dim, builtin, const, phase",
                    m.name
                )));
            }
            if m.reported && m.dim.is_none() {
                return Err(fail(format!(
                    "member {}: `reported` applies to `dim` only",
                    m.name
                )));
            }
            let carrier = match (m.ty.pointer, m.ty.base) {
                (true, _) => Carrier::Pointer,
                (false, Some(ty)) => Carrier::Value(ty),
                (false, None) => return Err(fail(format!("member {}: void by value", m.name))),
            };
            let role = if let Some(var) = &m.variable {
                let idx = *vars
                    .get(var.as_str())
                    .ok_or_else(|| fail(format!("member {}: unknown variable {var}", m.name)))?;
                let v = &self.interface.variables[idx];
                if !m.ty.base.is_some_and(|b| carries(b, v.ty)) {
                    return Err(fail(format!(
                        "member {}: C type {} does not match variable {} of type {:?}",
                        m.name, m.ty, var, v.ty
                    )));
                }
                if m.ty.pointer {
                    MemberRole::Pointer(Some(idx))
                } else {
                    if !v.shape.is_empty() {
                        return Err(fail(format!(
                            "member {}: variable {var} has a shape and cannot be carried by value",
                            m.name
                        )));
                    }
                    MemberRole::Value(idx)
                }
            } else if let Some(dim) = &m.dim {
                let idx = *dims
                    .get(dim.as_str())
                    .ok_or_else(|| fail(format!("member {}: unknown dimension {dim}", m.name)))?;
                if !m.ty.is_integer() {
                    return Err(fail(format!(
                        "member {}: a dimension length needs an integer type, not {}",
                        m.name, m.ty
                    )));
                }
                let Some(ty) = m.ty.base else {
                    return Err(fail(format!("member {}: void", m.name)));
                };
                match (m.reported, m.ty.pointer) {
                    (false, false) => MemberRole::Dim(idx, ty),
                    (false, true) => MemberRole::DimPointer {
                        dim: idx,
                        ty,
                        cell: next_cell(cells),
                    },
                    (true, pointer) => MemberRole::Reported {
                        dim: idx,
                        ty,
                        cell: pointer.then(|| next_cell(cells)),
                    },
                }
            } else if let Some(builtin) = m.builtin {
                if m.ty.pointer || m.ty.base != Some(ScalarType::F64) {
                    return Err(fail(format!(
                        "member {}: a builtin is a `double` by value, not {}",
                        m.name, m.ty
                    )));
                }
                MemberRole::Builtin(builtin)
            } else if let Some(value) = phased(m.const_, m.phase, m.ty)
                .map_err(|e| fail(format!("member {}: {e}", m.name)))?
            {
                MemberRole::Fixed {
                    value,
                    cell: m.ty.pointer.then(|| next_cell(cells)),
                }
            } else if m.ty.pointer {
                MemberRole::Pointer(None)
            } else {
                MemberRole::Scratch
            };
            roles.push(role);
            carriers.push((m.name.as_str(), carrier));
        }
        let layout =
            StructLayout::place(name, &carriers).map_err(|e| DescriptorError(e.to_string()))?;
        Ok(ResolvedStruct {
            name: name.to_owned(),
            layout,
            members: roles,
        })
    }

    fn resolve_call(
        &self,
        which: &str,
        call: &Call,
        structs: &BTreeMap<&str, usize>,
        vars: &BTreeMap<&str, usize>,
        dims: &BTreeMap<&str, usize>,
        cells: &mut usize,
    ) -> Result<ResolvedCall, DescriptorError> {
        let fail = |s: String| DescriptorError(format!("{which} ({}): {s}", call.symbol));
        if call.symbol.is_empty() {
            return Err(fail("empty symbol".to_owned()));
        }
        let mut args = Vec::with_capacity(call.args.len());
        let (mut ints, mut floats) = (0_usize, 0_usize);
        for (i, a) in call.args.iter().enumerate() {
            let set = usize::from(a.struct_.is_some())
                + usize::from(a.array.is_some())
                + usize::from(a.value.is_some())
                + usize::from(a.dim.is_some())
                + usize::from(a.builtin.is_some())
                + usize::from(a.handle.is_some())
                + usize::from(a.const_.is_some())
                + usize::from(a.phase.is_some());
            if set != 1 {
                return Err(fail(format!(
                    "argument {i}: exactly one of struct, array, value, dim, builtin, handle, \
                     const, phase"
                )));
            }
            if a.ty.is_some() && a.dim.is_none() && a.const_.is_none() && a.phase.is_none() {
                return Err(fail(format!(
                    "argument {i}: `type` applies to `dim`, `const` and `phase` only"
                )));
            }
            let role = if let Some(s) = &a.struct_ {
                let idx = *structs
                    .get(s.as_str())
                    .ok_or_else(|| fail(format!("argument {i}: unknown struct {s}")))?;
                ints += 1;
                ArgRole::Struct(idx)
            } else if let Some(v) = &a.array {
                let idx = *vars
                    .get(v.as_str())
                    .ok_or_else(|| fail(format!("argument {i}: unknown variable {v}")))?;
                ints += 1;
                ArgRole::Array(idx)
            } else if let Some(v) = &a.value {
                let idx = *vars
                    .get(v.as_str())
                    .ok_or_else(|| fail(format!("argument {i}: unknown variable {v}")))?;
                let var = &self.interface.variables[idx];
                if !var.shape.is_empty() {
                    return Err(fail(format!(
                        "argument {i}: variable {v} has a shape and cannot be passed by value"
                    )));
                }
                if var.causality == Causality::Output {
                    return Err(fail(format!(
                        "argument {i}: output {v} cannot be passed by value"
                    )));
                }
                if matches!(var.ty, ScalarType::F64 | ScalarType::F32) {
                    floats += 1;
                } else {
                    ints += 1;
                }
                ArgRole::Value(idx)
            } else if let Some(d) = &a.dim {
                let idx = *dims
                    .get(d.as_str())
                    .ok_or_else(|| fail(format!("argument {i}: unknown dimension {d}")))?;
                let ty = a.ty.unwrap_or(C_INT);
                if ty.pointer || !ty.is_integer() {
                    return Err(fail(format!(
                        "argument {i}: a dimension length needs a scalar integer type, not {ty}"
                    )));
                }
                let Some(base) = ty.base else {
                    return Err(fail(format!("argument {i}: void")));
                };
                ints += 1;
                ArgRole::Dim(idx, base)
            } else if let Some(b) = a.builtin {
                floats += 1;
                ArgRole::Builtin(b)
            } else if let Some(h) = a.handle {
                ints += 1;
                ArgRole::Handle(h)
            } else if let Some(value) = phased(a.const_, a.phase, a.ty.unwrap_or(C_INT))
                .map_err(|e| fail(format!("argument {i}: {e}")))?
            {
                let pointer = a.ty.is_some_and(|t| t.pointer);
                if !pointer && matches!(value.init.ty, ScalarType::F64 | ScalarType::F32) {
                    floats += 1;
                } else {
                    ints += 1;
                }
                ArgRole::Fixed {
                    value,
                    cell: pointer.then(|| next_cell(cells)),
                }
            } else {
                return Err(fail(format!("argument {i}: empty")));
            };
            args.push(role);
        }
        if ints > MAX_INT_ARGS || floats > MAX_FLOAT_ARGS {
            return Err(fail(format!(
                "at most {MAX_INT_ARGS} pointer/integer and {MAX_FLOAT_ARGS} floating-point \
                 arguments are supported; pass the rest through a struct"
            )));
        }
        Ok(ResolvedCall {
            symbol: call.symbol.clone(),
            returns: call.returns,
            args,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_types_parse_and_print() {
        let t = CType::parse("const double *").unwrap();
        assert_eq!(t.base, Some(ScalarType::F64));
        assert!(t.pointer);
        assert_eq!(t.to_string(), "double *");
        assert_eq!(
            CType::parse("unsigned long").unwrap().base,
            Some(ScalarType::U64)
        );
        assert!(CType::parse("void").is_err());
        assert!(CType::parse("double **").is_err());
        assert!(CType::parse("struct foo").is_err());
        assert_eq!(CType::parse("void*").unwrap().base, None);
    }

    const MINIMAL: &str = r#"
name = "gain"
[[variables]]
name = "u"
causality = "input"
type = "f64"
[[variables]]
name = "y"
causality = "output"
type = "f64"
[abi]
confirmed = true
[abi.init]
symbol = "gain_init"
[abi.step]
symbol = "gain_step"
args = [{ value = "u" }, { array = "y" }]
"#;

    #[test]
    fn a_minimal_descriptor_validates() {
        let d = Descriptor::parse(MINIMAL).unwrap();
        let plan = d.validate().unwrap();
        assert_eq!(plan.step.args, vec![ArgRole::Value(0), ArgRole::Array(1)]);
        assert_eq!(plan.inputs, vec![0]);
        assert_eq!(plan.outputs, vec![1]);
        let back = Descriptor::parse(&d.to_toml().unwrap()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn an_unconfirmed_descriptor_is_refused() {
        let d =
            Descriptor::parse(&MINIMAL.replace("confirmed = true", "confirmed = false")).unwrap();
        let err = d.validate().unwrap_err();
        assert!(err.0.contains("confirmed"), "{err}");
    }

    #[test]
    fn an_output_by_value_is_refused() {
        let d =
            Descriptor::parse(&MINIMAL.replace("{ array = \"y\" }", "{ value = \"y\" }")).unwrap();
        assert!(d.validate().unwrap_err().0.contains("by value"));
    }

    #[test]
    fn a_handle_needs_multiple_instances() {
        let text = MINIMAL.replace(
            "symbol = \"gain_init\"",
            "symbol = \"gain_init\"\nargs = [{ handle = \"out\" }]",
        );
        let d = Descriptor::parse(&text).unwrap();
        assert!(d.validate().unwrap_err().0.contains("multiple"));
        let d = Descriptor::parse(&format!("instances = \"multiple\"\n{text}")).unwrap();
        d.validate().unwrap();
    }

    #[test]
    fn const_and_phase_values_resolve_to_their_c_type() {
        let text = MINIMAL.replace(
            "symbol = \"gain_init\"",
            "symbol = \"gain_init\"\nargs = [{ const = -3, type = \"int16_t\" }, \
             { phase = { init = 1, step = 0 }, type = \"int *\" }, { const = 0.5, type = \"double\" }]",
        );
        let plan = Descriptor::parse(&text).unwrap().validate().unwrap();
        let ArgRole::Fixed { value, cell: None } = plan.init.args[0] else {
            panic!("{:?}", plan.init.args[0]);
        };
        assert_eq!(value.init.reg(), (false, u64::from((-3_i32) as u32)));
        let mut slot = [0_u8; 2];
        assert!(value.init.write(&mut slot));
        assert_eq!(i16::from_ne_bytes(slot), -3);
        assert!(!value.init.write(&mut [0_u8; 4]), "exact width only");
        let ArgRole::Fixed {
            value,
            cell: Some(0),
        } = plan.init.args[1]
        else {
            panic!("{:?}", plan.init.args[1]);
        };
        assert_eq!((value.init.bits, value.step.bits), (1, 0));
        let ArgRole::Fixed { value, cell: None } = plan.init.args[2] else {
            panic!("{:?}", plan.init.args[2]);
        };
        assert_eq!(value.init.reg(), (true, 0.5_f64.to_bits()));
        assert_eq!(plan.cells, 1);
        let back = Descriptor::parse(&text).unwrap();
        assert_eq!(Descriptor::parse(&back.to_toml().unwrap()).unwrap(), back);

        for (bad, why) in [
            ("{ const = 300, type = \"uint8_t\" }", "fit"),
            ("{ const = 1.5, type = \"int\" }", "integer"),
            ("{ const = 2, type = \"bool\" }", "fit"),
            ("{ const = 1, type = \"void *\" }", "void"),
            (
                "{ const = 1, phase = { init = 1, step = 0 } }",
                "exactly one",
            ),
            ("{ value = \"u\", type = \"int\" }", "`type` applies"),
        ] {
            let text = MINIMAL.replace(
                "symbol = \"gain_init\"",
                &format!("symbol = \"gain_init\"\nargs = [{bad}]"),
            );
            let err = Descriptor::parse(&text).unwrap().validate().unwrap_err();
            assert!(err.0.contains(why), "{bad}: {err}");
        }
    }

    #[test]
    fn members_carry_constants_phases_and_reported_lengths() {
        let text = MINIMAL
            .replace(
                "name = \"gain\"\n",
                "name = \"gain\"\n[[dimensions]]\nname = \"n\"\n",
            )
            .replace(
                "args = [{ value = \"u\" }, { array = \"y\" }]",
                "args = [{ struct = \"s\" }]\n[abi.structs.s]\nmembers = [\
                 { name = \"id\", type = \"int\", const = 4 },\
                 { name = \"flag\", type = \"int *\", phase = { init = 1, step = 0 } },\
                 { name = \"n\", type = \"int *\", dim = \"n\" },\
                 { name = \"n_out\", type = \"int *\", dim = \"n\", reported = true },\
                 { name = \"n_val\", type = \"long\", dim = \"n\", reported = true },\
                 { name = \"ok\", type = \"uint8_t *\", variable = \"y\" }]",
            );
        // `y` is f64 in MINIMAL; a byte pointer to it is refused, to a bool variable admitted.
        let err = Descriptor::parse(&text).unwrap().validate().unwrap_err();
        assert!(err.0.contains("does not match"), "{err}");
        let text = text.replace(
            "name = \"y\"\ncausality = \"output\"\ntype = \"f64\"",
            "name = \"y\"\ncausality = \"output\"\ntype = \"bool\"",
        );
        let plan = Descriptor::parse(&text).unwrap().validate().unwrap();
        let roles = &plan.structs[0].members;
        assert!(matches!(roles[0], MemberRole::Fixed { cell: None, .. }));
        assert!(matches!(roles[1], MemberRole::Fixed { cell: Some(0), .. }));
        assert!(matches!(roles[2], MemberRole::DimPointer { cell: 1, .. }));
        assert!(matches!(
            roles[3],
            MemberRole::Reported {
                cell: Some(2),
                ty: ScalarType::I32,
                ..
            }
        ));
        assert!(matches!(
            roles[4],
            MemberRole::Reported {
                cell: None,
                ty: ScalarType::I64,
                ..
            }
        ));
        assert!(matches!(roles[5], MemberRole::Pointer(Some(1))));
        assert_eq!(plan.cells, 3);

        let refused = text.replace(
            "{ name = \"id\", type = \"int\", const = 4 }",
            "{ name = \"id\", type = \"int\", const = 4, reported = true }",
        );
        let err = Descriptor::parse(&refused).unwrap().validate().unwrap_err();
        assert!(err.0.contains("`reported` applies"), "{err}");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        assert!(
            Descriptor::parse(&MINIMAL.replace("confirmed = true", "confirmed = true\nbogus = 1"))
                .is_err()
        );
    }
}
