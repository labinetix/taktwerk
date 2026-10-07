//! Text summaries of a plan and of a run.

use std::fmt::Write as _;
use std::time::Duration;

use taktwerk_core::image::Direction;
use taktwerk_core::plan::Plan;
use taktwerk_core::schedule::RunSummary;
use taktwerk_core::value::ScalarType;

/// Milliseconds, trimmed: `10`, `2.5`, `0.125`.
pub fn ms(d: Duration) -> String {
    let v = d.as_secs_f64() * 1e3;
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// Lower-case name of a direction.
pub const fn direction(d: Direction) -> &'static str {
    match d {
        Direction::Input => "input",
        Direction::Output => "output",
        Direction::Tunable => "tunable",
        Direction::System => "system",
    }
}

/// Lower-case name of a type.
pub fn scalar_type(t: ScalarType) -> String {
    format!("{t:?}").to_lowercase()
}

/// `[3, 2]`, or `scalar`.
pub fn shape(dims: &[usize]) -> String {
    if dims.is_empty() {
        return "scalar".to_owned();
    }
    let parts: Vec<String> = dims.iter().map(ToString::to_string).collect();
    format!("[{}]", parts.join(", "))
}

/// Pad `rows` into aligned columns, two spaces apart, the first row a header.
pub fn table(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in rows {
        let mut line = String::new();
        for (c, cell) in row.iter().enumerate() {
            if c + 1 == row.len() {
                line.push_str(cell);
            } else {
                let w = widths.get(c).copied().unwrap_or(0);
                let _ = write!(line, "{cell:<w$}  ");
            }
        }
        out.push_str("  ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The plan as `check` prints it: tick, instances, signals.
pub fn plan(plan: &Plan) -> String {
    let mut out = format!("tick {} ms\n\n", ms(plan.tick));
    let mut rows = vec![vec![
        "instance".to_owned(),
        "model".to_owned(),
        "period".to_owned(),
        "dims".to_owned(),
    ]];
    for inst in &plan.instances {
        let dims: Vec<String> = inst
            .spec
            .dims
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        rows.push(vec![
            inst.id.clone(),
            inst.model.clone(),
            format!("{} ms", ms(plan.tick.saturating_mul(inst.every))),
            if dims.is_empty() {
                "-".to_owned()
            } else {
                dims.join(" ")
            },
        ]);
    }
    out.push_str(&table(&rows));
    out.push('\n');
    let mut rows = vec![vec![
        "signal".to_owned(),
        "direction".to_owned(),
        "type".to_owned(),
        "shape".to_owned(),
        "max age".to_owned(),
    ]];
    for (_, spec) in plan.layout.iter() {
        rows.push(vec![
            spec.name.clone(),
            direction(spec.direction).to_owned(),
            scalar_type(spec.ty),
            shape(&spec.shape),
            spec.max_age
                .map_or_else(|| "-".to_owned(), |d| format!("{} ms", ms(d))),
        ]);
    }
    out.push_str(&table(&rows));
    out
}

/// The run summary `run` prints on exit.
pub fn run(s: &RunSummary) -> String {
    format!(
        "cycles {}  published {}  overruns {}  stale {}  cycle mean {} ms  max {} ms",
        s.cycles,
        s.published,
        s.overruns,
        s.stale,
        ms(s.mean_cycle),
        ms(s.max_cycle)
    )
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

    use taktwerk_core::model::{Causality, Dimension, Instances, ModelInterface, Variable};
    use taktwerk_core::project::Project;
    use taktwerk_core::value::{Dim, Layout};

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

    #[test]
    fn formats_durations() {
        assert_eq!(ms(Duration::from_millis(10)), "10");
        assert_eq!(ms(Duration::from_micros(2500)), "2.5");
        assert_eq!(ms(Duration::from_micros(125)), "0.125");
        assert_eq!(ms(Duration::ZERO), "0");
    }

    #[tokio::test]
    async fn prints_instances_and_signals() {
        let iface = ModelInterface {
            name: "gain".into(),
            dimensions: vec![Dimension {
                name: "n".into(),
                min: None,
                max: None,
                default: Some(2),
            }],
            variables: vec![
                var("u", Causality::Input, vec![Dim::Symbol("n".into())]),
                var("k", Causality::Tunable, vec![]),
                var("y", Causality::Output, vec![Dim::Symbol("n".into())]),
            ],
            instances: Instances::Multiple,
        };
        let project: Project = r#"
[engine]
tick_ms = 5.0
input_max_age_ms = 50.0

[models.gain]
kind = "fmi"
path = "gain.fmu"

[[instance]]
id = "g"
model = "gain"
every = 2
dims = { n = 3 }
"#
        .parse()
        .unwrap();
        let interfaces = BTreeMap::from([("gain".to_owned(), iface)]);
        let plan = Plan::resolve(&project, &interfaces, &mut []).await.unwrap();
        let expected = "tick 5 ms

  instance  model  period  dims
  g         gain   10 ms   n=3

  signal              direction  type  shape   max age
  taktwerk.heartbeat  system     u64   scalar  -
  taktwerk.status     system     i32   scalar  -
  taktwerk.cycle      system     u64   scalar  -
  taktwerk.overruns   system     u64   scalar  -
  taktwerk.stale      system     u64   scalar  -
  g.y                 output     f64   [3]     -
  g.u                 input      f64   [3]     50 ms
  g.k                 tunable    f64   scalar  -
";
        assert_eq!(super::plan(&plan), expected);
    }
}
