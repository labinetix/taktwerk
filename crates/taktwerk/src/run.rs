//! `taktwerk check` and `taktwerk run`.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use taktwerk_core::connector::ConnectorError;
use taktwerk_core::image::{ImageError, ImageHandle, image};
use taktwerk_core::plan::Plan;
use taktwerk_core::schedule::{Engine, EngineHandle, instantiate};
use tokio::runtime::Runtime;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

use crate::setup::{self, Ready};
use crate::summary;

/// How long connectors get to stop after shutdown is signalled.
const CONNECTOR_STOP: Duration = Duration::from_secs(5);

fn runtime() -> anyhow::Result<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("taktwerk-io")
        .enable_all()
        .build()
        .context("cannot start the I/O runtime")
}

/// Load, resolve and bind `project_file`, print the plan; no cycle is started.
pub fn check(project_file: &Path) -> anyhow::Result<ExitCode> {
    let rt = runtime()?;
    match rt.block_on(setup::prepare(project_file)) {
        Ok(ready) => {
            print!("{}", summary::plan(&ready.plan));
            println!(
                "\nok: {} instance(s), {} signal(s), {} connector(s) bound",
                ready.plan.instances.len(),
                ready.plan.layout.len(),
                ready.connectors.len()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(problems) => {
            eprintln!("{}: {problems}", project_file.display());
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Write every tunable's start value into the image, so connectors show it from the start.
fn write_start_values(plan: &Plan, image: &ImageHandle) -> Result<(), ImageError> {
    for inst in &plan.instances {
        for port in &inst.tunables {
            if let Some(value) = inst.spec.params.get(&port.name) {
                image.write(port.signal, value)?;
            }
        }
    }
    Ok(())
}

/// Why the run ends.
#[derive(Debug)]
enum End {
    Signal(&'static str),
    EngineStopped,
    Connector(String, Result<(), ConnectorError>),
}

type Tasks = JoinSet<(String, Result<(), ConnectorError>)>;

/// Run `project_file` until SIGINT or SIGTERM, a model error or a connector failure.
pub fn run(project_file: &Path) -> anyhow::Result<ExitCode> {
    let rt = runtime()?;
    let ready = match rt.block_on(setup::prepare(project_file)) {
        Ok(ready) => ready,
        Err(problems) => {
            eprintln!("{}: {problems}", project_file.display());
            return Ok(ExitCode::FAILURE);
        }
    };
    let Ready {
        project,
        adapters,
        connectors,
        plan,
        ..
    } = ready;
    info!(
        instances = plan.instances.len(),
        signals = plan.layout.len(),
        tick_ms = summary::ms(plan.tick),
        "plan resolved"
    );

    let (mut sigint, mut sigterm) = rt.block_on(async {
        Ok::<_, std::io::Error>((
            signal(SignalKind::interrupt())?,
            signal(SignalKind::terminate())?,
        ))
    })?;

    let (cycle, handle) = image(plan.layout.clone());
    write_start_values(&plan, &handle).context("tunable start values")?;
    let (shutdown, shutdown_rx) = watch::channel(false);
    let mut tasks = Tasks::new();
    for c in connectors {
        let id = c.id().to_owned();
        let fut = c.run(handle.clone(), shutdown_rx.clone());
        tasks.spawn_on(async move { (id, fut.await) }, rt.handle());
    }

    let started = instantiate(&plan, &adapters)
        .and_then(|i| Engine::start(plan, i, cycle, project.engine.realtime.clone()));
    let engine = match started {
        Ok(engine) => engine,
        Err(e) => {
            shutdown.send_replace(true);
            let _ = rt.block_on(stop_connectors(&mut tasks));
            return Err(anyhow::Error::new(e).context("the engine did not start"));
        }
    };
    info!("running; SIGINT or SIGTERM stops");

    let end = rt.block_on(wait(&engine, &mut tasks, &mut sigint, &mut sigterm));
    match &end {
        End::Signal(name) => info!("{name} received, stopping"),
        End::EngineStopped => {}
        End::Connector(id, result) => match result {
            Ok(()) => warn!(connector = %id, "connector ended, stopping"),
            Err(e) => error!(connector = %id, "connector failed, stopping: {e}"),
        },
    }
    engine.stop();
    let result = engine.join();
    shutdown.send_replace(true);
    let mut failed = rt.block_on(stop_connectors(&mut tasks));
    if let End::Connector(id, Err(e)) = end {
        failed.push(format!("connector `{id}`: {e}"));
    }

    let mut code = ExitCode::SUCCESS;
    match result {
        Ok(s) => println!("{}", summary::run(&s)),
        Err(e) => {
            eprintln!("error: {e}");
            code = ExitCode::FAILURE;
        }
    }
    for f in failed {
        eprintln!("error: {f}");
        code = ExitCode::FAILURE;
    }
    Ok(code)
}

async fn wait(
    engine: &EngineHandle,
    tasks: &mut Tasks,
    sigint: &mut tokio::signal::unix::Signal,
    sigterm: &mut tokio::signal::unix::Signal,
) -> End {
    let mut finished = engine.finished();
    let connector = async {
        match tasks.join_next().await {
            Some(Ok((id, result))) => End::Connector(id, result),
            Some(Err(e)) => End::Connector("?".into(), Err(ConnectorError::Io(e.to_string()))),
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = sigint.recv() => End::Signal("SIGINT"),
        _ = sigterm.recv() => End::Signal("SIGTERM"),
        _ = finished.wait_for(|f| *f) => End::EngineStopped,
        end = connector => end,
    }
}

/// Wait for every connector after shutdown; the failures, as text.
async fn stop_connectors(tasks: &mut Tasks) -> Vec<String> {
    let mut failed = Vec::new();
    let all = async {
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok((_, Ok(()))) => {}
                Ok((id, Err(e))) => failed.push(format!("connector `{id}`: {e}")),
                Err(e) => failed.push(format!("connector task: {e}")),
            }
        }
    };
    if tokio::time::timeout(CONNECTOR_STOP, all).await.is_err() {
        tasks.abort_all();
        warn!("connectors did not stop within {CONNECTOR_STOP:?}; aborted");
    }
    failed
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

    use taktwerk_core::model::{Causality, Instances, ModelInterface, Variable};
    use taktwerk_core::project::Project;
    use taktwerk_core::value::{Buffer, Layout, ScalarType};

    use super::*;

    #[tokio::test]
    async fn tunable_start_values_reach_the_image() {
        let iface = ModelInterface {
            name: "g".into(),
            dimensions: vec![],
            variables: vec![Variable {
                name: "k".into(),
                causality: Causality::Tunable,
                ty: ScalarType::F64,
                shape: vec![],
                layout: Layout::RowMajor,
                unit: None,
                description: None,
            }],
            instances: Instances::Multiple,
        };
        let project: Project =
            "[engine]\ntick_ms = 1.0\n[models.g]\nkind = \"fmi\"\npath = \"g\"\n\
             [[instance]]\nid = \"a\"\nmodel = \"g\"\nparameters = { k = 2.5 }\n"
                .parse()
                .unwrap();
        let interfaces = BTreeMap::from([("g".to_owned(), iface)]);
        let plan = Plan::resolve(&project, &interfaces, &mut []).await.unwrap();
        let (_cycle, handle) = image(plan.layout.clone());
        write_start_values(&plan, &handle).unwrap();
        let id = plan.layout.id("a.k").unwrap();
        let mut out = Buffer::zeroed(ScalarType::F64, 1);
        assert!(handle.read(id, &mut out).unwrap().is_some());
        assert_eq!(out, Buffer::F64(vec![2.5]));
    }
}
