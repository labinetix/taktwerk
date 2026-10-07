//! Conversions between image values and OPC UA values, types and timestamps.

use std::time::{Duration, Instant};

use opcua::types::{Array, DataTypeId, DateTime, NodeId, StatusCode, Variant, VariantScalarTypeId};
use taktwerk_core::value::{Buffer, Layout, ScalarType};

/// The OPC UA data type of `ty`.
pub(crate) fn data_type(ty: ScalarType) -> DataTypeId {
    match ty {
        ScalarType::F64 => DataTypeId::Double,
        ScalarType::F32 => DataTypeId::Float,
        ScalarType::I64 => DataTypeId::Int64,
        ScalarType::I32 => DataTypeId::Int32,
        ScalarType::I16 => DataTypeId::Int16,
        ScalarType::I8 => DataTypeId::SByte,
        ScalarType::U64 => DataTypeId::UInt64,
        ScalarType::U32 => DataTypeId::UInt32,
        ScalarType::U16 => DataTypeId::UInt16,
        ScalarType::U8 => DataTypeId::Byte,
        ScalarType::Bool => DataTypeId::Boolean,
    }
}

/// The OPC UA data type node of `ty`.
pub(crate) fn data_type_node(ty: ScalarType) -> NodeId {
    data_type(ty).into()
}

fn scalar_type_id(ty: ScalarType) -> VariantScalarTypeId {
    match ty {
        ScalarType::F64 => VariantScalarTypeId::Double,
        ScalarType::F32 => VariantScalarTypeId::Float,
        ScalarType::I64 => VariantScalarTypeId::Int64,
        ScalarType::I32 => VariantScalarTypeId::Int32,
        ScalarType::I16 => VariantScalarTypeId::Int16,
        ScalarType::I8 => VariantScalarTypeId::SByte,
        ScalarType::U64 => VariantScalarTypeId::UInt64,
        ScalarType::U32 => VariantScalarTypeId::UInt32,
        ScalarType::U16 => VariantScalarTypeId::UInt16,
        ScalarType::U8 => VariantScalarTypeId::Byte,
        ScalarType::Bool => VariantScalarTypeId::Boolean,
    }
}

/// A human name of `ty` as OPC UA calls it.
pub(crate) fn type_label(ty: ScalarType) -> &'static str {
    match ty {
        ScalarType::F64 => "Double",
        ScalarType::F32 => "Float",
        ScalarType::I64 => "Int64",
        ScalarType::I32 => "Int32",
        ScalarType::I16 => "Int16",
        ScalarType::I8 => "SByte",
        ScalarType::U64 => "UInt64",
        ScalarType::U32 => "UInt32",
        ScalarType::U16 => "UInt16",
        ScalarType::U8 => "Byte",
        ScalarType::Bool => "Boolean",
    }
}

/// The name of a namespace-0 data type node, or the node itself.
pub(crate) fn data_type_label(id: &NodeId) -> String {
    let known = [
        ScalarType::F64,
        ScalarType::F32,
        ScalarType::I64,
        ScalarType::I32,
        ScalarType::I16,
        ScalarType::I8,
        ScalarType::U64,
        ScalarType::U32,
        ScalarType::U16,
        ScalarType::U8,
        ScalarType::Bool,
    ];
    known
        .into_iter()
        .find(|ty| data_type_node(*ty) == *id)
        .map_or_else(|| id.to_string(), |ty| type_label(ty).to_owned())
}

/// Element `i` of `buf` as a scalar variant.
fn element(buf: &Buffer, i: usize) -> Option<Variant> {
    Some(match buf {
        Buffer::F64(v) => Variant::Double(*v.get(i)?),
        Buffer::F32(v) => Variant::Float(*v.get(i)?),
        Buffer::I64(v) => Variant::Int64(*v.get(i)?),
        Buffer::I32(v) => Variant::Int32(*v.get(i)?),
        Buffer::I16(v) => Variant::Int16(*v.get(i)?),
        Buffer::I8(v) => Variant::SByte(*v.get(i)?),
        Buffer::U64(v) => Variant::UInt64(*v.get(i)?),
        Buffer::U32(v) => Variant::UInt32(*v.get(i)?),
        Buffer::U16(v) => Variant::UInt16(*v.get(i)?),
        Buffer::U8(v) => Variant::Byte(*v.get(i)?),
        Buffer::Bool(v) => Variant::Boolean(*v.get(i)?),
    })
}

/// Store scalar `value` as element `i` of `buf`; `false` on a type mismatch or out of range.
fn set_element(buf: &mut Buffer, i: usize, value: &Variant) -> bool {
    fn put<T: Copy>(v: &mut [T], i: usize, x: T) -> bool {
        v.get_mut(i).map(|slot| *slot = x).is_some()
    }
    match (buf, value) {
        (Buffer::F64(v), Variant::Double(x)) => put(v, i, *x),
        (Buffer::F32(v), Variant::Float(x)) => put(v, i, *x),
        (Buffer::I64(v), Variant::Int64(x)) => put(v, i, *x),
        (Buffer::I32(v), Variant::Int32(x)) => put(v, i, *x),
        (Buffer::I16(v), Variant::Int16(x)) => put(v, i, *x),
        (Buffer::I8(v), Variant::SByte(x)) => put(v, i, *x),
        (Buffer::U64(v), Variant::UInt64(x)) => put(v, i, *x),
        (Buffer::U32(v), Variant::UInt32(x)) => put(v, i, *x),
        (Buffer::U16(v), Variant::UInt16(x)) => put(v, i, *x),
        (Buffer::U8(v), Variant::Byte(x)) => put(v, i, *x),
        (Buffer::Bool(v), Variant::Boolean(x)) => put(v, i, *x),
        _ => false,
    }
}

/// How a signal's buffer appears on the wire.
#[derive(Debug, Clone)]
pub(crate) struct Shape {
    /// Scalar on the wire (a one-element buffer).
    pub scalar: bool,
    /// Array dimensions announced on the wire; empty for a flat array without dimensions.
    pub dims: Vec<u32>,
    /// Buffer index of each wire element, when the orders differ.
    pub perm: Option<Vec<usize>>,
}

impl Shape {
    /// A flat value of `len` elements, scalar when `scalar`.
    pub(crate) fn flat(scalar: bool) -> Self {
        Self {
            scalar,
            dims: Vec::new(),
            perm: None,
        }
    }

    /// The wire shape of a signal: scalar when `shape` is empty, else an array with these
    /// dimensions in row-major order.
    pub(crate) fn of_signal(shape: &[usize], layout: Layout) -> Self {
        if shape.is_empty() {
            return Self::flat(true);
        }
        let dims = shape
            .iter()
            .map(|d| u32::try_from(*d).unwrap_or(u32::MAX))
            .collect();
        let perm =
            (shape.len() > 1 && layout == Layout::ColumnMajor).then(|| column_major_perm(shape));
        Self {
            scalar: false,
            dims,
            perm,
        }
    }

    fn index(&self, wire: usize) -> usize {
        self.perm
            .as_ref()
            .and_then(|p| p.get(wire).copied())
            .unwrap_or(wire)
    }
}

/// For each row-major element index, its column-major index.
fn column_major_perm(shape: &[usize]) -> Vec<usize> {
    let len: usize = shape.iter().product();
    let mut perm = Vec::with_capacity(len);
    let mut idx = vec![0usize; shape.len()];
    for _ in 0..len {
        let mut col = 0;
        let mut stride = 1;
        for (i, d) in idx.iter().zip(shape) {
            col += i * stride;
            stride *= d;
        }
        perm.push(col);
        // Advance the row-major counter: the last index runs fastest.
        for (i, d) in idx.iter_mut().zip(shape).rev() {
            *i += 1;
            if *i < *d {
                break;
            }
            *i = 0;
        }
    }
    perm
}

/// `buf` as a variant shaped by `shape`.
pub(crate) fn to_variant(buf: &Buffer, shape: &Shape) -> Variant {
    if shape.scalar {
        return element(buf, 0).unwrap_or(Variant::Empty);
    }
    let values: Vec<Variant> = (0..buf.len())
        .filter_map(|w| element(buf, shape.index(w)))
        .collect();
    let ty = scalar_type_id(buf.ty());
    let array = if shape.dims.len() > 1 {
        Array::new_multi(ty, values, shape.dims.clone())
    } else {
        Array::new(ty, values)
    };
    array.map_or(Variant::Empty, |a| Variant::Array(Box::new(a)))
}

/// Decode `value` into `out`, which keeps its type and length.
///
/// # Errors
/// `BadTypeMismatch` when the element type, the length or the dimensions differ.
pub(crate) fn from_variant(
    value: &Variant,
    out: &mut Buffer,
    shape: &Shape,
) -> Result<(), StatusCode> {
    match value {
        Variant::Array(array) => {
            if array.values.len() != out.len() {
                return Err(StatusCode::BadTypeMismatch);
            }
            if let Some(dims) = &array.dimensions {
                let product: u64 = dims.iter().map(|d| u64::from(*d)).product();
                if product != out.len() as u64
                    || (shape.dims.len() > 1 && dims.as_slice() != shape.dims.as_slice())
                {
                    return Err(StatusCode::BadTypeMismatch);
                }
            }
            for (w, v) in array.values.iter().enumerate() {
                if !set_element(out, shape.index(w), v) {
                    return Err(StatusCode::BadTypeMismatch);
                }
            }
            Ok(())
        }
        Variant::Empty => Err(StatusCode::BadTypeMismatch),
        scalar => {
            if out.len() == 1 && set_element(out, 0, scalar) {
                Ok(())
            } else {
                Err(StatusCode::BadTypeMismatch)
            }
        }
    }
}

/// Number of elements of `value`; `None` for an empty value.
pub(crate) fn element_count(value: &Variant) -> Option<usize> {
    match value {
        Variant::Empty => None,
        Variant::Array(array) => Some(array.values.len()),
        _ => Some(1),
    }
}

const NANOS_PER_TICK: u128 = 100;

/// The wall-clock time of monotonic `stamp`.
pub(crate) fn wall_time(stamp: Instant) -> DateTime {
    let age = Instant::now().saturating_duration_since(stamp);
    let ticks = i64::try_from(age.as_nanos() / NANOS_PER_TICK).unwrap_or(i64::MAX);
    DateTime::from(DateTime::now().ticks().saturating_sub(ticks).max(0))
}

/// The monotonic instant of wall-clock `time`, never later than now.
pub(crate) fn monotonic(time: &DateTime) -> Instant {
    let now = Instant::now();
    let ticks = DateTime::now().ticks().saturating_sub(time.ticks());
    let Ok(ticks) = u64::try_from(ticks) else {
        return now;
    };
    now.checked_sub(Duration::from_nanos(ticks.saturating_mul(100)))
        .unwrap_or(now)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn column_major_round_trip() {
        // A 2x3 matrix stored column-major: [a00 a10 a01 a11 a02 a12].
        let buf = Buffer::F64(vec![0.0, 10.0, 1.0, 11.0, 2.0, 12.0]);
        let shape = Shape::of_signal(&[2, 3], Layout::ColumnMajor);
        let v = to_variant(&buf, &shape);
        let Variant::Array(a) = &v else {
            panic!("not an array: {v:?}");
        };
        let row: Vec<f64> = a.values.iter().filter_map(Variant::as_f64).collect();
        assert_eq!(row, vec![0.0, 1.0, 2.0, 10.0, 11.0, 12.0]);
        assert_eq!(a.dimensions, Some(vec![2, 3]));
        let mut back = Buffer::zeroed(ScalarType::F64, 6);
        from_variant(&v, &mut back, &shape).expect("decode");
        assert_eq!(back, buf);
    }

    #[test]
    fn rejects_wrong_type_and_length() {
        let shape = Shape::of_signal(&[2], Layout::RowMajor);
        let mut out = Buffer::zeroed(ScalarType::I32, 2);
        let wrong_type = to_variant(&Buffer::F64(vec![1.0, 2.0]), &shape);
        assert_eq!(
            from_variant(&wrong_type, &mut out, &shape),
            Err(StatusCode::BadTypeMismatch)
        );
        let wrong_len = to_variant(&Buffer::I32(vec![1, 2, 3]), &shape);
        assert_eq!(
            from_variant(&wrong_len, &mut out, &shape),
            Err(StatusCode::BadTypeMismatch)
        );
        assert_eq!(out, Buffer::I32(vec![0, 0]));
    }

    #[test]
    fn timestamps_round_trip() {
        let past = Instant::now() - Duration::from_millis(500);
        let back = monotonic(&wall_time(past));
        let diff = if back > past {
            back - past
        } else {
            past - back
        };
        assert!(diff < Duration::from_millis(5), "{diff:?}");
    }
}
