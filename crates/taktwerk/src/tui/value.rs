//! Signal values as text, and text back into a value of the same type and length.

use opcua::types::{Array, Variant};

/// One element as text; `None` for a type a taktwerk signal never has.
fn element(v: &Variant) -> Option<String> {
    Some(match v {
        Variant::Boolean(b) => b.to_string(),
        Variant::SByte(n) => n.to_string(),
        Variant::Byte(n) => n.to_string(),
        Variant::Int16(n) => n.to_string(),
        Variant::UInt16(n) => n.to_string(),
        Variant::Int32(n) => n.to_string(),
        Variant::UInt32(n) => n.to_string(),
        Variant::Int64(n) => n.to_string(),
        Variant::UInt64(n) => n.to_string(),
        Variant::Float(x) => format_float(f64::from(*x)),
        Variant::Double(x) => format_float(*x),
        _ => return None,
    })
}

/// Shortest exact form up to six significant digits, else exponent notation.
fn format_float(x: f64) -> String {
    if x == 0.0 || (1e-4..1e7).contains(&x.abs()) {
        let s = format!("{x:.6}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s == "-0" {
            "0".to_owned()
        } else {
            s.to_owned()
        }
    } else {
        format!("{x:.4e}")
    }
}

/// A value as the table shows it: a scalar, or `[a, b, …]`.
pub fn format(v: &Variant) -> String {
    match v {
        Variant::Empty => "-".to_owned(),
        Variant::Array(a) => {
            let parts: Vec<String> = a
                .values
                .iter()
                .map(|e| element(e).unwrap_or_else(|| "?".to_owned()))
                .collect();
            format!("[{}]", parts.join(", "))
        }
        scalar => element(scalar).unwrap_or_else(|| "?".to_owned()),
    }
}

/// Parse `text` as an element of the same type as `like`.
fn parse_element(like: &Variant, text: &str) -> Result<Variant, String> {
    let t = text.trim();
    let bad = |what: &str| format!("`{t}` is not {what}");
    Ok(match like {
        Variant::Boolean(_) => Variant::Boolean(match t {
            "true" | "1" => true,
            "false" | "0" => false,
            _ => return Err(bad("a boolean")),
        }),
        Variant::SByte(_) => Variant::SByte(t.parse().map_err(|_| bad("an i8"))?),
        Variant::Byte(_) => Variant::Byte(t.parse().map_err(|_| bad("a u8"))?),
        Variant::Int16(_) => Variant::Int16(t.parse().map_err(|_| bad("an i16"))?),
        Variant::UInt16(_) => Variant::UInt16(t.parse().map_err(|_| bad("a u16"))?),
        Variant::Int32(_) => Variant::Int32(t.parse().map_err(|_| bad("an i32"))?),
        Variant::UInt32(_) => Variant::UInt32(t.parse().map_err(|_| bad("a u32"))?),
        Variant::Int64(_) => Variant::Int64(t.parse().map_err(|_| bad("an i64"))?),
        Variant::UInt64(_) => Variant::UInt64(t.parse().map_err(|_| bad("a u64"))?),
        Variant::Float(_) => Variant::Float(t.parse().map_err(|_| bad("a number"))?),
        Variant::Double(_) => Variant::Double(t.parse().map_err(|_| bad("a number"))?),
        _ => return Err("this value type cannot be edited".to_owned()),
    })
}

/// Parse `text` into a value shaped like `current`: a scalar, or an array of the same length
/// written `[a, b, …]`, `a, b, …` or `a b …`.
///
/// # Errors
/// What does not fit, as text.
pub fn parse(current: &Variant, text: &str) -> Result<Variant, String> {
    match current {
        Variant::Array(a) => {
            let like = a
                .values
                .first()
                .ok_or_else(|| "an empty array cannot be edited".to_owned())?;
            let inner = text.trim().trim_start_matches('[').trim_end_matches(']');
            let values = inner
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .map(|s| parse_element(like, s))
                .collect::<Result<Vec<_>, _>>()?;
            if values.len() != a.values.len() {
                return Err(format!(
                    "expected {} values, got {}",
                    a.values.len(),
                    values.len()
                ));
            }
            let array = match &a.dimensions {
                Some(dims) => Array::new_multi(a.value_type, values, dims.clone()),
                None => Array::new(a.value_type, values),
            }
            .map_err(|e| format!("{e:?}"))?;
            Ok(Variant::Array(Box::new(array)))
        }
        Variant::Empty => Err("no value read yet".to_owned()),
        scalar => parse_element(scalar, text),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use opcua::types::VariantScalarTypeId;

    use super::*;

    fn doubles(v: &[f64]) -> Variant {
        let values: Vec<Variant> = v.iter().copied().map(Variant::Double).collect();
        Variant::Array(Box::new(
            Array::new(VariantScalarTypeId::Double, values).unwrap(),
        ))
    }

    #[test]
    fn formats_scalars_and_arrays() {
        assert_eq!(format(&Variant::Double(2.5)), "2.5");
        assert_eq!(format(&Variant::Double(1.0 / 3.0)), "0.333333");
        assert_eq!(format(&Variant::Double(-0.0)), "0");
        assert_eq!(format(&Variant::Double(1.5e9)), "1.5000e9");
        assert_eq!(format(&Variant::UInt64(42)), "42");
        assert_eq!(format(&Variant::Boolean(true)), "true");
        assert_eq!(format(&doubles(&[1.0, 2.0])), "[1, 2]");
        assert_eq!(format(&Variant::Empty), "-");
    }

    #[test]
    fn parses_like_the_current_value() {
        assert_eq!(
            parse(&Variant::Double(0.0), " 3.5 ").unwrap(),
            Variant::Double(3.5)
        );
        assert_eq!(parse(&Variant::Int32(0), "-7").unwrap(), Variant::Int32(-7));
        assert_eq!(
            parse(&Variant::Boolean(false), "1").unwrap(),
            Variant::Boolean(true)
        );
        assert!(parse(&Variant::Int32(0), "1.5").is_err());
        let a = doubles(&[0.0, 0.0, 0.0]);
        assert_eq!(parse(&a, "[1, 2, 3]").unwrap(), doubles(&[1.0, 2.0, 3.0]));
        assert_eq!(parse(&a, "1 2 3").unwrap(), doubles(&[1.0, 2.0, 3.0]));
        let err = parse(&a, "1, 2").unwrap_err();
        assert_eq!(err, "expected 3 values, got 2");
        assert!(parse(&Variant::Empty, "1").is_err());
    }
}
