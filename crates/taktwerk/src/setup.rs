//! From a project file to a bound plan: load, validate, load models, build connectors, resolve,
//! bind. Shared by `check` and `run`; no cycle is started here.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use taktwerk_core::connector::Connector;
use taktwerk_core::model::ModelInterface;
use taktwerk_core::plan::Plan;
use taktwerk_core::project::Project;
use taktwerk_core::schedule::ModelAdapters;

use crate::kinds::{self, ModelKind};

/// A project with every model loaded, its plan resolved and every connector bound.
pub struct Ready {
    /// The project file as parsed.
    pub project: Project,
    /// Adapters by model id.
    pub adapters: ModelAdapters,
    /// Bound connectors, in project order.
    pub connectors: Vec<Box<dyn Connector>>,
    /// The resolved plan.
    pub plan: Plan,
}

/// Every problem found while preparing a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problems(pub Vec<String>);

impl fmt::Display for Problems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0.len();
        write!(f, "{n} problem{}", if n == 1 { "" } else { "s" })?;
        for p in &self.0 {
            write!(f, "\n  - {p}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Problems {}

impl From<String> for Problems {
    fn from(p: String) -> Self {
        Self(vec![p])
    }
}

/// A model `path` from the project, resolved against the project file's directory.
pub fn model_path(project_file: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    project_file
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(path)
}

/// Load `project_file` and take it as far as bound connectors.
///
/// Model and connector problems are collected, not stopped at; resolution and binding run once
/// everything loaded.
///
/// # Errors
/// Every problem found.
pub async fn prepare(project_file: &Path) -> Result<Ready, Problems> {
    let project = Project::load(project_file).map_err(|e| e.to_string())?;
    let mut problems = Vec::new();

    let mut adapters = ModelAdapters::new();
    for (id, model) in &project.models {
        let path = model_path(project_file, &model.path);
        let loaded = ModelKind::parse(&model.kind).and_then(|kind| kind.load(&path));
        match loaded {
            Ok(adapter) => {
                adapters.insert(id.clone(), adapter);
            }
            Err(e) => problems.push(format!("model `{id}` ({}): {e}", path.display())),
        }
    }
    let mut connectors = Vec::new();
    for config in &project.connectors {
        match kinds::connector(config) {
            Ok(c) => connectors.push(c),
            Err(e) => problems.push(e),
        }
    }
    if !problems.is_empty() {
        return Err(Problems(problems));
    }

    let interfaces: BTreeMap<String, ModelInterface> = adapters
        .iter()
        .map(|(id, a)| (id.clone(), a.interface().clone()))
        .collect();
    let plan = Plan::resolve(&project, &interfaces, &mut connectors)
        .await
        .map_err(|e| e.to_string())?;
    for c in &mut connectors {
        if let Err(e) = c.bind(&plan.layout).await {
            problems.push(format!("connector `{}`: {e}", c.id()));
        }
    }
    if !problems.is_empty() {
        return Err(Problems(problems));
    }
    Ok(Ready {
        project,
        adapters,
        connectors,
        plan,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("taktwerk-setup-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn model_paths_are_relative_to_the_project_file() {
        let file = Path::new("/srv/plant/project.toml");
        assert_eq!(
            model_path(file, Path::new("models/a.fmu")),
            Path::new("/srv/plant/models/a.fmu")
        );
        assert_eq!(
            model_path(file, Path::new("/m/a.fmu")),
            Path::new("/m/a.fmu")
        );
        assert_eq!(
            model_path(Path::new("p.toml"), Path::new("a.fmu")),
            Path::new("a.fmu")
        );
    }

    #[tokio::test]
    async fn collects_every_load_problem() {
        let dir = scratch("problems");
        let file = dir.join("p.toml");
        std::fs::write(
            &file,
            r#"
[engine]
tick_ms = 10.0

[models.a]
kind = "fmi"
path = "missing.fmu"

[models.b]
kind = "onnx"
path = "b"

[[connector]]
id = "bus"
kind = "modbus"
"#,
        )
        .unwrap();
        let err = prepare(&file).await.err().unwrap();
        assert_eq!(err.0.len(), 3, "{err}");
        let text = err.to_string();
        assert!(text.starts_with("3 problems"), "{text}");
        assert!(text.contains("model `a`"), "{text}");
        assert!(text.contains("unknown model kind `onnx`"), "{text}");
        assert!(text.contains("unknown kind `modbus`"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_unparsable_project_is_one_problem() {
        let dir = scratch("parse");
        let file = dir.join("p.toml");
        std::fs::write(&file, "[engine]\ntick_ms = -1.0\n").unwrap();
        let err = prepare(&file).await.err().unwrap();
        assert_eq!(err.0.len(), 1);
        assert!(err.0[0].contains("tick_ms"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
