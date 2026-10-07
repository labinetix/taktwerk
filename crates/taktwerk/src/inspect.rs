//! `taktwerk inspect`: a model's declared interface as text.

use taktwerk_core::model::{Causality, Instances, ModelInterface};
use taktwerk_core::value::{Dim, Layout};

use crate::summary::{scalar_type, table};

/// Lower-case name of a causality.
pub const fn causality(c: Causality) -> &'static str {
    match c {
        Causality::Input => "input",
        Causality::Output => "output",
        Causality::Parameter => "parameter",
        Causality::Tunable => "tunable",
    }
}

/// `[nx, 3]`, or `scalar`.
pub fn symbolic_shape(shape: &[Dim]) -> String {
    if shape.is_empty() {
        return "scalar".to_owned();
    }
    let parts: Vec<String> = shape
        .iter()
        .map(|d| match d {
            Dim::Literal(n) => n.to_string(),
            Dim::Symbol(s) => s.clone(),
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

/// The interface as `inspect` prints it.
pub fn interface(kind: &str, iface: &ModelInterface) -> String {
    let instances = match iface.instances {
        Instances::Single => "single",
        Instances::Multiple => "multiple",
    };
    let mut out = format!(
        "model {} ({kind})\ninstances {instances} per process\n\n",
        iface.name
    );
    if iface.dimensions.is_empty() {
        out.push_str("dimensions: none (fixed size)\n\n");
    } else {
        let mut rows = vec![vec![
            "dimension".to_owned(),
            "min".to_owned(),
            "max".to_owned(),
            "default".to_owned(),
        ]];
        let opt = |v: Option<usize>| v.map_or_else(|| "-".to_owned(), |n| n.to_string());
        for d in &iface.dimensions {
            rows.push(vec![d.name.clone(), opt(d.min), opt(d.max), opt(d.default)]);
        }
        out.push_str(&table(&rows));
        out.push('\n');
    }
    let mut rows = vec![vec![
        "variable".to_owned(),
        "causality".to_owned(),
        "type".to_owned(),
        "shape".to_owned(),
        "unit".to_owned(),
        "description".to_owned(),
    ]];
    for v in &iface.variables {
        let mut shape = symbolic_shape(&v.shape);
        if v.shape.len() > 1 && v.layout == Layout::ColumnMajor {
            shape.push_str(" col-major");
        }
        rows.push(vec![
            v.name.clone(),
            causality(v.causality).to_owned(),
            scalar_type(v.ty),
            shape,
            v.unit.clone().unwrap_or_else(|| "-".to_owned()),
            v.description.clone().unwrap_or_default(),
        ]);
    }
    out.push_str(&table(&rows));
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use taktwerk_core::model::{Dimension, Variable};
    use taktwerk_core::value::ScalarType;

    use super::*;

    #[test]
    fn prints_dimensions_and_symbolic_shapes() {
        let iface = ModelInterface {
            name: "ss".into(),
            dimensions: vec![Dimension {
                name: "nx".into(),
                min: Some(1),
                max: None,
                default: Some(2),
            }],
            variables: vec![Variable {
                name: "A".into(),
                causality: Causality::Tunable,
                ty: ScalarType::F64,
                shape: vec![Dim::Symbol("nx".into()), Dim::Symbol("nx".into())],
                layout: Layout::ColumnMajor,
                unit: None,
                description: Some("system matrix".into()),
            }],
            instances: Instances::Multiple,
        };
        let text = interface("fmi", &iface);
        assert!(
            text.starts_with("model ss (fmi)\ninstances multiple per process\n"),
            "{text}"
        );
        assert!(text.contains("  nx         1    -    2\n"), "{text}");
        assert!(
            text.contains("  A         tunable    f64   [nx, nx] col-major  -     system matrix\n"),
            "{text}"
        );
    }
}
