//! Element types, shapes and the typed buffers every signal and model variable lives in.

use serde::{Deserialize, Serialize};

/// The element type of a signal or model variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScalarType {
    /// IEEE 754 binary64.
    F64,
    /// IEEE 754 binary32.
    F32,
    /// Signed 64-bit integer.
    I64,
    /// Signed 32-bit integer.
    I32,
    /// Signed 16-bit integer.
    I16,
    /// Signed 8-bit integer.
    I8,
    /// Unsigned 64-bit integer.
    U64,
    /// Unsigned 32-bit integer.
    U32,
    /// Unsigned 16-bit integer.
    U16,
    /// Unsigned 8-bit integer.
    U8,
    /// Boolean, one byte, `0` or `1`.
    Bool,
}

impl ScalarType {
    /// Size of one element in bytes.
    #[must_use]
    pub const fn size(self) -> usize {
        match self {
            Self::F64 | Self::I64 | Self::U64 => 8,
            Self::F32 | Self::I32 | Self::U32 => 4,
            Self::I16 | Self::U16 => 2,
            Self::I8 | Self::U8 | Self::Bool => 1,
        }
    }
}

/// One dimension of a declared shape: a literal length or a symbol the instance binds.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Dim {
    /// A length fixed by the model.
    Literal(usize),
    /// A named dimension (`nx`) bound per instance.
    Symbol(String),
}

/// Storage order of a value with more than one dimension.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// C order: the last index varies fastest.
    #[default]
    RowMajor,
    /// Fortran/MATLAB order: the first index varies fastest.
    ColumnMajor,
}

/// A typed, fixed-length value buffer.
///
/// Allocated once at init with the length its bound shape gives; never resized afterwards.
/// [`Buffer::copy_from`] is the only way values move between buffers on the cycle path.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs, reason = "one variant per ScalarType, named alike")]
pub enum Buffer {
    F64(Vec<f64>),
    F32(Vec<f32>),
    I64(Vec<i64>),
    I32(Vec<i32>),
    I16(Vec<i16>),
    I8(Vec<i8>),
    U64(Vec<u64>),
    U32(Vec<u32>),
    U16(Vec<u16>),
    U8(Vec<u8>),
    Bool(Vec<bool>),
}

impl Buffer {
    /// A zeroed buffer of `len` elements of `ty`. Allocates; call at init only.
    #[must_use]
    pub fn zeroed(ty: ScalarType, len: usize) -> Self {
        match ty {
            ScalarType::F64 => Self::F64(vec![0.0; len]),
            ScalarType::F32 => Self::F32(vec![0.0; len]),
            ScalarType::I64 => Self::I64(vec![0; len]),
            ScalarType::I32 => Self::I32(vec![0; len]),
            ScalarType::I16 => Self::I16(vec![0; len]),
            ScalarType::I8 => Self::I8(vec![0; len]),
            ScalarType::U64 => Self::U64(vec![0; len]),
            ScalarType::U32 => Self::U32(vec![0; len]),
            ScalarType::U16 => Self::U16(vec![0; len]),
            ScalarType::U8 => Self::U8(vec![0; len]),
            ScalarType::Bool => Self::Bool(vec![false; len]),
        }
    }

    /// The element type.
    #[must_use]
    pub const fn ty(&self) -> ScalarType {
        match self {
            Self::F64(_) => ScalarType::F64,
            Self::F32(_) => ScalarType::F32,
            Self::I64(_) => ScalarType::I64,
            Self::I32(_) => ScalarType::I32,
            Self::I16(_) => ScalarType::I16,
            Self::I8(_) => ScalarType::I8,
            Self::U64(_) => ScalarType::U64,
            Self::U32(_) => ScalarType::U32,
            Self::U16(_) => ScalarType::U16,
            Self::U8(_) => ScalarType::U8,
            Self::Bool(_) => ScalarType::Bool,
        }
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::F64(v) => v.len(),
            Self::F32(v) => v.len(),
            Self::I64(v) => v.len(),
            Self::I32(v) => v.len(),
            Self::I16(v) => v.len(),
            Self::I8(v) => v.len(),
            Self::U64(v) => v.len(),
            Self::U32(v) => v.len(),
            Self::U16(v) => v.len(),
            Self::U8(v) => v.len(),
            Self::Bool(v) => v.len(),
        }
    }

    /// Whether the buffer holds no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copy `src` into `self` without allocating.
    ///
    /// # Errors
    /// [`Mismatch`] when type or length differ; `self` is then unchanged.
    pub fn copy_from(&mut self, src: &Self) -> Result<(), Mismatch> {
        match (self, src) {
            (Self::F64(d), Self::F64(s)) if d.len() == s.len() => d.copy_from_slice(s),
            (Self::F32(d), Self::F32(s)) if d.len() == s.len() => d.copy_from_slice(s),
            (Self::I64(d), Self::I64(s)) if d.len() == s.len() => d.copy_from_slice(s),
            (Self::I32(d), Self::I32(s)) if d.len() == s.len() => d.copy_from_slice(s),
            (Self::I16(d), Self::I16(s)) if d.len() == s.len() => d.copy_from_slice(s),
            (Self::I8(d), Self::I8(s)) if d.len() == s.len() => d.copy_from_slice(s),
            (Self::U64(d), Self::U64(s)) if d.len() == s.len() => d.copy_from_slice(s),
            (Self::U32(d), Self::U32(s)) if d.len() == s.len() => d.copy_from_slice(s),
            (Self::U16(d), Self::U16(s)) if d.len() == s.len() => d.copy_from_slice(s),
            (Self::U8(d), Self::U8(s)) if d.len() == s.len() => d.copy_from_slice(s),
            (Self::Bool(d), Self::Bool(s)) if d.len() == s.len() => d.copy_from_slice(s),
            _ => return Err(Mismatch),
        }
        Ok(())
    }
}

/// Two buffers differ in element type or length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("buffer type or length mismatch")]
pub struct Mismatch;
