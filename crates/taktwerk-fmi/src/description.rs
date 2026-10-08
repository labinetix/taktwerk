//! `modelDescription.xml` of FMI 2 and FMI 3, reduced to what a co-simulation instance needs.

use std::collections::BTreeMap;

use roxmltree::{Document, Node};
use taktwerk_core::model::{Causality, Dimension, Instances, ModelInterface, Variable};
use taktwerk_core::value::{Dim, Layout, ScalarType};

/// The FMI version an FMU implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmiVersion {
    /// FMI 2.0.
    V2,
    /// FMI 3.0.
    V3,
}

/// One variable the interface exposes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FmiVariable {
    pub name: String,
    pub vr: u32,
    pub causality: Causality,
    pub ty: ScalarType,
    pub shape: Vec<Dim>,
    pub unit: Option<String>,
    pub description: Option<String>,
    /// An FMI 3 `String`: a `u8` buffer of literal capacity, exchanged NUL-terminated.
    pub text: bool,
}

/// Capacity of a `String` variable without a `taktwerk` annotation, bytes (NUL included).
pub const DEFAULT_TEXT_CAPACITY: usize = 256;

/// A structural parameter; it becomes a dimension of the interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StructuralParameter {
    pub name: String,
    pub vr: u32,
    pub ty: ScalarType,
    pub start: Option<usize>,
    pub min: Option<usize>,
    pub max: Option<usize>,
}

/// The parsed parts of `modelDescription.xml`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelDescription {
    pub version: FmiVersion,
    pub model_name: String,
    /// `instantiationToken` (FMI 3) or `guid` (FMI 2).
    pub token: String,
    /// `modelIdentifier` of the co-simulation interface.
    pub model_identifier: String,
    pub single_instance: bool,
    pub variables: Vec<FmiVariable>,
    pub structural: Vec<StructuralParameter>,
}

impl ModelDescription {
    /// The size-generic interface: structural parameters as dimensions, exposed variables in
    /// declaration order.
    pub fn interface(&self) -> ModelInterface {
        ModelInterface {
            name: self.model_name.clone(),
            dimensions: self
                .structural
                .iter()
                .map(|s| Dimension {
                    name: s.name.clone(),
                    min: s.min,
                    max: s.max,
                    default: s.start,
                })
                .collect(),
            variables: self
                .variables
                .iter()
                .map(|v| Variable {
                    name: v.name.clone(),
                    causality: v.causality,
                    ty: v.ty,
                    shape: v.shape.clone(),
                    layout: Layout::RowMajor,
                    unit: v.unit.clone(),
                    description: v.description.clone(),
                })
                .collect(),
            instances: if self.single_instance {
                Instances::Single
            } else {
                Instances::Multiple
            },
        }
    }
}

/// Parse `modelDescription.xml`.
///
/// # Errors
/// Malformed XML, no co-simulation interface, or a variable type the adapter does not support.
pub(crate) fn parse(xml: &str) -> Result<ModelDescription, String> {
    let doc = Document::parse(xml).map_err(|e| format!("modelDescription.xml: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "fmiModelDescription" {
        return Err("modelDescription.xml: root is not fmiModelDescription".into());
    }
    let fmi_version = root.attribute("fmiVersion").unwrap_or_default();
    let version = if fmi_version.starts_with("3.") {
        FmiVersion::V3
    } else if fmi_version.starts_with("2.") {
        FmiVersion::V2
    } else {
        return Err(format!("unsupported fmiVersion {fmi_version:?}"));
    };
    let model_name = root.attribute("modelName").unwrap_or_default().to_owned();
    let token = match version {
        FmiVersion::V3 => root.attribute("instantiationToken"),
        FmiVersion::V2 => root.attribute("guid"),
    }
    .ok_or("modelDescription.xml: no instantiation token")?
    .to_owned();

    let Some(cs) = child(root, "CoSimulation") else {
        return Err(if child(root, "ModelExchange").is_some() {
            format!("{model_name}: model exchange only; taktwerk runs co-simulation FMUs")
        } else {
            format!("{model_name}: no co-simulation interface")
        });
    };
    let model_identifier = cs
        .attribute("modelIdentifier")
        .ok_or("CoSimulation: no modelIdentifier")?
        .to_owned();
    let single_instance = cs.attribute("canBeInstantiatedOnlyOncePerProcess") == Some("true");

    let units = type_units(root, version);
    let vars = child(root, "ModelVariables")
        .map(|n| n.children().filter(Node::is_element).collect::<Vec<_>>())
        .unwrap_or_default();
    let (variables, structural) = match version {
        FmiVersion::V3 => parse_v3(&vars, &units)?,
        FmiVersion::V2 => (parse_v2(&vars, &units)?, Vec::new()),
    };
    Ok(ModelDescription {
        version,
        model_name,
        token,
        model_identifier,
        single_instance,
        variables,
        structural,
    })
}

fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|c| c.has_tag_name(name))
}

/// Declared type name → unit, from `TypeDefinitions`.
fn type_units(root: Node<'_, '_>, version: FmiVersion) -> BTreeMap<String, String> {
    let mut units = BTreeMap::new();
    let Some(defs) = child(root, "TypeDefinitions") else {
        return units;
    };
    for def in defs.children().filter(Node::is_element) {
        let (name, unit) = match version {
            FmiVersion::V3 => (def.attribute("name"), def.attribute("unit")),
            FmiVersion::V2 => (
                def.attribute("name"),
                child(def, "Real").and_then(|r| r.attribute("unit")),
            ),
        };
        if let (Some(name), Some(unit)) = (name, unit) {
            units.insert(name.to_owned(), unit.to_owned());
        }
    }
    units
}

fn unit_of(node: Node<'_, '_>, units: &BTreeMap<String, String>) -> Option<String> {
    node.attribute("unit").map(str::to_owned).or_else(|| {
        node.attribute("declaredType")
            .and_then(|t| units.get(t).cloned())
    })
}

fn value_reference(node: Node<'_, '_>, name: &str) -> Result<u32, String> {
    node.attribute("valueReference")
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| format!("{name}: missing or invalid valueReference"))
}

fn parse_usize(node: Node<'_, '_>, attr: &str) -> Option<usize> {
    node.attribute(attr).and_then(|v| v.trim().parse().ok())
}

/// Causality of an exposed variable; `None` for variables the interface leaves out.
fn exposed(causality: &str, variability: Option<&str>) -> Option<Causality> {
    match causality {
        "input" => Some(Causality::Input),
        "output" => Some(Causality::Output),
        "parameter" if variability == Some("tunable") => Some(Causality::Tunable),
        "parameter" => Some(Causality::Parameter),
        _ => None,
    }
}

fn v3_type(tag: &str) -> Result<Option<ScalarType>, ()> {
    Ok(Some(match tag {
        "Float64" => ScalarType::F64,
        "Float32" => ScalarType::F32,
        "Int64" | "Enumeration" => ScalarType::I64,
        "Int32" => ScalarType::I32,
        "Int16" => ScalarType::I16,
        "Int8" => ScalarType::I8,
        "UInt64" => ScalarType::U64,
        "UInt32" => ScalarType::U32,
        "UInt16" => ScalarType::U16,
        "UInt8" => ScalarType::U8,
        "Boolean" => ScalarType::Bool,
        "Binary" | "Clock" => return Err(()),
        "String" => return Ok(None),
        _ => return Ok(None),
    }))
}

const fn is_integer(ty: ScalarType) -> bool {
    !matches!(ty, ScalarType::F64 | ScalarType::F32 | ScalarType::Bool)
}

type V3Parts = (Vec<FmiVariable>, Vec<StructuralParameter>);

fn parse_v3(vars: &[Node<'_, '_>], units: &BTreeMap<String, String>) -> Result<V3Parts, String> {
    // Value reference → what a `<Dimension valueReference>` resolves to.
    let mut dim_refs: BTreeMap<u32, Dim> = BTreeMap::new();
    let mut structural = Vec::new();
    for var in vars {
        let name = var.attribute("name").unwrap_or_default();
        let causality = var.attribute("causality").unwrap_or("local");
        let variability = var.attribute("variability");
        let Ok(Some(ty)) = v3_type(var.tag_name().name()) else {
            continue;
        };
        if !is_integer(ty) {
            if causality == "structuralParameter" {
                return Err(format!(
                    "{name}: structural parameter of type {} is not supported",
                    var.tag_name().name()
                ));
            }
            continue;
        }
        let vr = value_reference(*var, name)?;
        if causality == "structuralParameter" {
            dim_refs.insert(vr, Dim::Symbol(name.to_owned()));
            structural.push(StructuralParameter {
                name: name.to_owned(),
                vr,
                ty,
                start: parse_usize(*var, "start"),
                min: parse_usize(*var, "min"),
                max: parse_usize(*var, "max"),
            });
        } else if variability == Some("constant")
            && let Some(start) = parse_usize(*var, "start")
        {
            dim_refs.insert(vr, Dim::Literal(start));
        }
    }

    let mut variables = Vec::new();
    for var in vars {
        let tag = var.tag_name().name();
        let name = var.attribute("name").unwrap_or_default();
        let causality = var.attribute("causality").unwrap_or("local");
        let Some(causality) = exposed(causality, var.attribute("variability")) else {
            continue;
        };
        let text = tag == "String";
        let ty = match v3_type(tag) {
            Ok(Some(ty)) => ty,
            Ok(None) if text => ScalarType::U8,
            Ok(None) => continue,
            Err(()) => {
                return Err(format!(
                    "{name}: FMI 3 type {tag} is not supported (only numeric, Boolean and String)"
                ));
            }
        };
        if text {
            variables.push(FmiVariable {
                name: name.to_owned(),
                vr: value_reference(*var, name)?,
                causality,
                ty,
                shape: vec![Dim::Literal(text_capacity(*var))],
                unit: None,
                description: var.attribute("description").map(str::to_owned),
                text: true,
            });
            continue;
        }
        let mut shape = Vec::new();
        for dim in var.children().filter(|c| c.has_tag_name("Dimension")) {
            if let Some(start) = parse_usize(dim, "start") {
                shape.push(Dim::Literal(start));
            } else {
                let vr = value_reference(dim, name)?;
                let resolved = dim_refs.get(&vr).cloned().ok_or_else(|| {
                    format!("{name}: Dimension valueReference {vr} is no structural parameter")
                })?;
                shape.push(resolved);
            }
        }
        variables.push(FmiVariable {
            name: name.to_owned(),
            vr: value_reference(*var, name)?,
            causality,
            ty,
            shape,
            unit: unit_of(*var, units),
            description: var.attribute("description").map(str::to_owned),
            text: false,
        });
    }
    Ok((variables, structural))
}

/// `<Annotations><Annotation type="taktwerk"><text capacity="N"/></Annotation></Annotations>`,
/// else the default.
fn text_capacity(var: Node<'_, '_>) -> usize {
    child(var, "Annotations")
        .into_iter()
        .flat_map(|a| a.children().filter(|n| n.has_tag_name("Annotation")))
        .filter(|a| a.attribute("type") == Some("taktwerk"))
        .filter_map(|a| child(a, "text"))
        .find_map(|t| parse_usize(t, "capacity"))
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_TEXT_CAPACITY)
}

fn parse_v2(
    vars: &[Node<'_, '_>],
    units: &BTreeMap<String, String>,
) -> Result<Vec<FmiVariable>, String> {
    let mut variables = Vec::new();
    for var in vars.iter().filter(|v| v.has_tag_name("ScalarVariable")) {
        let name = var.attribute("name").unwrap_or_default();
        let causality = var.attribute("causality").unwrap_or("local");
        let Some(causality) = exposed(causality, var.attribute("variability")) else {
            continue;
        };
        let Some(typed) = var.children().find(Node::is_element) else {
            return Err(format!("{name}: ScalarVariable without type element"));
        };
        let ty = match typed.tag_name().name() {
            "Real" => ScalarType::F64,
            "Integer" | "Enumeration" => ScalarType::I32,
            "Boolean" => ScalarType::Bool,
            other => {
                return Err(format!(
                    "{name}: FMI 2 type {other} is not supported (only Real, Integer, \
                     Enumeration and Boolean)"
                ));
            }
        };
        variables.push(FmiVariable {
            name: name.to_owned(),
            vr: value_reference(*var, name)?,
            causality,
            ty,
            shape: Vec::new(),
            unit: unit_of(typed, units),
            description: var.attribute("description").map(str::to_owned),
            text: false,
        });
    }
    Ok(variables)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use super::*;

    const V3: &str = r#"<?xml version="1.0"?>
<fmiModelDescription fmiVersion="3.0" modelName="M" instantiationToken="{t}">
  <CoSimulation modelIdentifier="m" canBeInstantiatedOnlyOncePerProcess="true"/>
  <TypeDefinitions><Float64Type name="Len" unit="m"/></TypeDefinitions>
  <ModelVariables>
    <Float64 name="time" valueReference="0" causality="independent"/>
    <UInt64 name="n" valueReference="1" causality="structuralParameter" variability="fixed" start="3" min="1" max="8"/>
    <UInt64 name="k" valueReference="2" variability="constant" start="2"/>
    <Float64 name="u" valueReference="3" causality="input" declaredType="Len"><Dimension valueReference="1"/></Float64>
    <Float64 name="A" valueReference="4" causality="parameter" variability="tunable"><Dimension valueReference="1"/><Dimension start="4"/></Float64>
    <Int32 name="c" valueReference="5" causality="parameter" variability="fixed"><Dimension valueReference="2"/></Int32>
    <Boolean name="y" valueReference="6" causality="output"/>
    <String name="s" valueReference="7" causality="local"/>
  </ModelVariables>
</fmiModelDescription>"#;

    #[test]
    fn fmi3_maps_structural_parameters_to_dimensions() {
        let md = parse(V3).unwrap();
        assert_eq!(md.version, FmiVersion::V3);
        assert!(md.single_instance);
        let i = md.interface();
        assert_eq!(i.instances, Instances::Single);
        assert_eq!(i.dimensions.len(), 1);
        assert_eq!(i.dimensions[0].name, "n");
        assert_eq!(i.dimensions[0].default, Some(3));
        assert_eq!(i.dimensions[0].max, Some(8));
        let names: Vec<_> = i.variables.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["u", "A", "c", "y"]);
        assert_eq!(i.variables[0].shape, [Dim::Symbol("n".into())]);
        assert_eq!(i.variables[0].unit.as_deref(), Some("m"));
        assert_eq!(i.variables[1].causality, Causality::Tunable);
        assert_eq!(
            i.variables[1].shape,
            [Dim::Symbol("n".into()), Dim::Literal(4)]
        );
        assert_eq!(i.variables[2].shape, [Dim::Literal(2)]);
        assert_eq!(i.variables[3].ty, ScalarType::Bool);
    }

    #[test]
    fn refuses_model_exchange_only() {
        let xml = r#"<fmiModelDescription fmiVersion="3.0" modelName="M" instantiationToken="t">
            <ModelExchange modelIdentifier="m"/><ModelVariables/></fmiModelDescription>"#;
        assert!(parse(xml).unwrap_err().contains("model exchange only"));
    }

    #[test]
    fn strings_are_byte_buffers_with_a_capacity() {
        let xml = r#"<fmiModelDescription fmiVersion="3.0" modelName="M" instantiationToken="t">
            <CoSimulation modelIdentifier="m"/><ModelVariables>
            <String name="s" valueReference="1" causality="input"/>
            <String name="t" valueReference="2" causality="parameter" variability="fixed">
              <Start value=""/>
              <Annotations><Annotation type="taktwerk"><text capacity="16"/></Annotation></Annotations>
            </String>
            <Binary name="b" valueReference="3" causality="output"/>
            </ModelVariables></fmiModelDescription>"#;
        assert!(parse(xml).unwrap_err().contains("Binary"));
        let md = parse(&xml.replace(
            r#"<Binary name="b" valueReference="3" causality="output"/>"#,
            "",
        ))
        .unwrap();
        let i = md.interface();
        assert_eq!(i.variables[0].ty, ScalarType::U8);
        assert_eq!(i.variables[0].shape, [Dim::Literal(DEFAULT_TEXT_CAPACITY)]);
        assert_eq!(i.variables[1].shape, [Dim::Literal(16)]);
        assert_eq!(i.variables[1].causality, Causality::Parameter);
        assert!(md.variables[1].text);
    }

    #[test]
    fn fmi2_is_scalar() {
        let xml = r#"<fmiModelDescription fmiVersion="2.0" modelName="M" guid="g">
            <CoSimulation modelIdentifier="m"/>
            <TypeDefinitions><SimpleType name="P"><Real unit="Pa"/></SimpleType></TypeDefinitions>
            <ModelVariables>
            <ScalarVariable name="x" valueReference="1" causality="output"><Real declaredType="P"/></ScalarVariable>
            <ScalarVariable name="b" valueReference="2" causality="input"><Boolean/></ScalarVariable>
            <ScalarVariable name="e" valueReference="3" causality="parameter" variability="tunable"><Real/></ScalarVariable>
            </ModelVariables></fmiModelDescription>"#;
        let md = parse(xml).unwrap();
        assert_eq!(md.version, FmiVersion::V2);
        assert_eq!(md.token, "g");
        let i = md.interface();
        assert_eq!(i.instances, Instances::Multiple);
        assert_eq!(i.variables[0].unit.as_deref(), Some("Pa"));
        assert_eq!(i.variables[1].ty, ScalarType::Bool);
        assert_eq!(i.variables[2].causality, Causality::Tunable);
    }
}
