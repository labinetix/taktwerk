//! `taktwerk new`: a starting project file for one model.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use taktwerk_core::model::{Causality, ModelInterface};

use crate::inspect::symbolic_shape;
use crate::summary::scalar_type;

/// What the scaffold is built from.
pub struct Scaffold<'a> {
    /// Model id and instance id; a TOML bare key.
    pub id: String,
    /// The model's `kind`.
    pub kind: &'a str,
    /// The model's `path` as the project file will hold it.
    pub path: String,
    /// The model's interface.
    pub interface: &'a ModelInterface,
    /// Base tick, milliseconds.
    pub tick_ms: f64,
    /// Port of the own OPC UA server.
    pub port: u16,
}

/// A bare TOML key and instance id from a model name: ASCII alphanumerics, `_` and `-`.
pub fn id_from(name: &str) -> String {
    let id: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let id = id.trim_matches('_').to_owned();
    if id.is_empty() {
        "model".to_owned()
    } else {
        id
    }
}

/// `target` relative to the directory `base`; both absolute or both relative to one directory.
pub fn relative(target: &Path, base: &Path) -> PathBuf {
    let t: Vec<Component<'_>> = target.components().collect();
    let b: Vec<Component<'_>> = base
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    let t_clean: Vec<Component<'_>> = t
        .iter()
        .copied()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    let common = t_clean.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if b[common..]
        .iter()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return target.to_path_buf();
    }
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &t_clean[common..] {
        out.push(c.as_os_str());
    }
    out
}

/// A TOML basic string.
fn quoted(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

/// A TOML key: bare when possible.
fn key(s: &str) -> String {
    let bare = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare { s.to_owned() } else { quoted(s) }
}

/// The project file text.
pub fn project(s: &Scaffold<'_>) -> String {
    let iface = s.interface;
    let id = &s.id;
    let mut out = String::new();
    let _ = writeln!(out, "# taktwerk project for model `{}`.", iface.name);
    out.push_str("# Check with `taktwerk check <file>`, run with `taktwerk run <file>`.\n\n");
    out.push_str("[engine]\n");
    let _ = writeln!(out, "tick_ms = {:?}", s.tick_ms);
    out.push_str("# input_max_age_ms = 100.0   # inputs older than this fault the tick\n");
    out.push_str("# [engine.realtime]\n# policy = \"fifo\"\n# priority = 80\n# cpu = 3\n");
    out.push_str("# lock_memory = true\n\n");

    let _ = writeln!(out, "[models.{id}]");
    let _ = writeln!(out, "kind = {}", quoted(s.kind));
    let _ = writeln!(out, "path = {}\n", quoted(&s.path));

    out.push_str("[[instance]]\n");
    let _ = writeln!(out, "id = {}", quoted(id));
    let _ = writeln!(out, "model = {}", quoted(id));
    out.push_str("every = 1   # period in ticks\n");

    if !iface.dimensions.is_empty() {
        out.push_str("\n[instance.dims]\n");
        for d in &iface.dimensions {
            let range = match (d.min, d.max) {
                (None, None) => String::new(),
                (lo, hi) => format!(
                    "; {}..={}",
                    lo.map_or_else(String::new, |n| n.to_string()),
                    hi.map_or_else(String::new, |n| n.to_string())
                ),
            };
            match d.default {
                Some(n) => {
                    let _ = writeln!(out, "{} = {n}   # model default{range}", key(&d.name));
                }
                None => {
                    let _ = writeln!(
                        out,
                        "# {} =    # no default: bind a length or \"from-server\"{range}",
                        key(&d.name)
                    );
                }
            }
        }
    }

    let settable: Vec<_> = iface
        .variables
        .iter()
        .filter(|v| matches!(v.causality, Causality::Parameter | Causality::Tunable))
        .collect();
    if !settable.is_empty() {
        out.push_str("\n[instance.parameters]   # start values; unset ones keep the model's\n");
        for v in settable {
            let _ = writeln!(
                out,
                "# {} =    # {} {} {}",
                key(&v.name),
                crate::inspect::causality(v.causality),
                scalar_type(v.ty),
                symbolic_shape(&v.shape)
            );
        }
    }

    for (causality, table) in [
        (Causality::Input, "inputs"),
        (Causality::Output, "outputs"),
        (Causality::Tunable, "tunables"),
    ] {
        let vars: Vec<_> = iface
            .variables
            .iter()
            .filter(|v| v.causality == causality)
            .collect();
        if vars.is_empty() {
            continue;
        }
        let _ = writeln!(out, "\n[instance.{table}]   # variable = signal");
        for v in vars {
            let _ = writeln!(
                out,
                "{} = {}",
                key(&v.name),
                quoted(&format!("{id}.{}", v.name))
            );
        }
    }

    out.push_str("\n[[connector]]\nid = \"ua\"\nkind = \"opcua-server\"\n");
    let _ = writeln!(out, "endpoint = \"opc.tcp://127.0.0.1:{}\"", s.port);
    out.push_str("# security = [\"none\"]\n# anonymous = true\n");
    out.push_str(
        "\n# A PLC's OPC UA server: map image signals to its nodes.\n\
         # [[connector]]\n\
         # id = \"plc\"\n\
         # kind = \"opcua-client\"\n\
         # endpoint = \"opc.tcp://192.168.0.10:4840\"\n\
         # sync_period_ms = 100\n\
         # [connector.map]\n",
    );
    for v in iface
        .variables
        .iter()
        .filter(|v| matches!(v.causality, Causality::Input | Causality::Output))
    {
        let _ = writeln!(
            out,
            "# {} = \"ns=4;s=Plant.{}\"",
            quoted(&format!("{id}.{}", v.name)),
            v.name
        );
    }
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
    use std::collections::BTreeMap;

    use taktwerk_core::model::{Dimension, Instances, Variable};
    use taktwerk_core::plan::Plan;
    use taktwerk_core::project::Project;
    use taktwerk_core::value::{Dim, Layout, ScalarType};

    use super::*;

    fn var(name: &str, causality: Causality, shape: Vec<Dim>) -> Variable {
        Variable {
            name: name.into(),
            causality,
            ty: ScalarType::F64,
            shape,
            layout: Layout::RowMajor,
            unit: None,
            description: None,
        }
    }

    fn iface(default_n: Option<usize>) -> ModelInterface {
        ModelInterface {
            name: "State Space".into(),
            dimensions: vec![Dimension {
                name: "n".into(),
                min: Some(1),
                max: Some(8),
                default: default_n,
            }],
            variables: vec![
                var("u", Causality::Input, vec![Dim::Symbol("n".into())]),
                var("y", Causality::Output, vec![Dim::Symbol("n".into())]),
                var("k", Causality::Tunable, vec![]),
                var("x0", Causality::Parameter, vec![Dim::Symbol("n".into())]),
            ],
            instances: Instances::Multiple,
        }
    }

    fn scaffold(iface: &ModelInterface) -> String {
        project(&Scaffold {
            id: id_from(&iface.name),
            kind: "fmi",
            path: "models/ss.fmu".into(),
            interface: iface,
            tick_ms: 10.0,
            port: 4840,
        })
    }

    #[test]
    fn ids_are_bare_keys() {
        assert_eq!(id_from("State Space"), "state_space");
        assert_eq!(id_from("{8c4e810f}"), "8c4e810f");
        assert_eq!(id_from("..."), "model");
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative(Path::new("/a/b/m.fmu"), Path::new("/a/c")),
            Path::new("../b/m.fmu")
        );
        assert_eq!(
            relative(Path::new("m.fmu"), Path::new("")),
            Path::new("m.fmu")
        );
        assert_eq!(
            relative(Path::new("./models/m.fmu"), Path::new(".")),
            Path::new("models/m.fmu")
        );
        assert_eq!(relative(Path::new("m"), Path::new("../x")), Path::new("m"));
    }

    #[tokio::test]
    async fn a_scaffold_with_defaults_resolves() {
        let iface = iface(Some(3));
        let text = scaffold(&iface);
        assert!(text.contains("n = 3   # model default; 1..=8\n"), "{text}");
        assert!(text.contains("u = \"state_space.u\"\n"), "{text}");
        assert!(text.contains("# x0 =    # parameter f64 [n]\n"), "{text}");
        let project: Project = text.parse().unwrap();
        let interfaces = BTreeMap::from([("state_space".to_owned(), iface)]);
        let plan = Plan::resolve(&project, &interfaces, &mut []).await.unwrap();
        assert_eq!(plan.instances[0].spec.dims["n"], 3);
        assert_eq!(project.connectors[0].kind, "opcua-server");
    }

    #[tokio::test]
    async fn an_unbound_dimension_is_left_for_the_user() {
        let iface = iface(None);
        let text = scaffold(&iface);
        assert!(
            text.contains("# n =    # no default: bind a length or \"from-server\""),
            "{text}"
        );
        let project: Project = text.parse().unwrap();
        let interfaces = BTreeMap::from([("state_space".to_owned(), iface)]);
        let err = Plan::resolve(&project, &interfaces, &mut [])
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("`n`"), "{err}");
    }
}
