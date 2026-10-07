//! `fmu-wrap`: each fixture package wrapped as an FMI 3 FMU, loaded with `taktwerk-fmi`, and
//! driven through the same steps as the raw adapter; the outputs must be identical.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail loudly"
)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use common::*;
use taktwerk_core::model::{InstanceSpec, ModelAdapter, ModelError, StepIo};
use taktwerk_core::value::Buffer;
use taktwerk_fmi::FmuAdapter;
use taktwerk_raw::RawModel;
use taktwerk_raw::fmu::build::{BuildOptions, Target, build, plan};

/// Wrap `name` once per test binary. The FMU's log messages reach stderr under `RUST_LOG`.
fn fmu(name: &str) -> PathBuf {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
    static DONE: Mutex<BTreeMap<String, PathBuf>> = Mutex::new(BTreeMap::new());
    let mut done = DONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(path) = done.get(name) {
        return path.clone();
    }
    let output = built().root.join(format!("{name}.fmu"));
    let report = build(
        &package(name),
        &BuildOptions {
            targets: vec![Target::host().unwrap()],
            bundle: Vec::new(),
            output: output.clone(),
        },
    )
    .unwrap();
    assert_eq!(report.compiled.len(), 1);
    done.insert(name.to_owned(), output.clone());
    output
}

/// One step of a script: the inputs, and new tunables if they change.
struct Step {
    inputs: Vec<Buffer>,
    tunables: Option<Vec<Buffer>>,
}

/// Instantiate every `(spec, io)` on `adapter`, init all, then step all through `script`
/// interleaved; the outputs after init and after every step, per instance.
fn drive(
    adapter: &dyn ModelAdapter,
    runs: &[(InstanceSpec, StepIo, Vec<Step>)],
    step: f64,
) -> Vec<Vec<Vec<Buffer>>> {
    let mut instances: Vec<_> = runs
        .iter()
        .map(|(spec, io, _)| (adapter.instantiate(spec).unwrap(), io.clone()))
        .collect();
    let mut trace: Vec<Vec<Vec<Buffer>>> = Vec::new();
    for (inst, io) in &mut instances {
        inst.init(0.0, io).unwrap();
        trace.push(vec![io.outputs.clone()]);
    }
    let steps = runs.iter().map(|r| r.2.len()).max().unwrap_or(0);
    for k in 0..steps {
        for (i, (inst, io)) in instances.iter_mut().enumerate() {
            let Some(s) = runs[i].2.get(k) else { continue };
            io.inputs.clone_from(&s.inputs);
            if let Some(t) = &s.tunables {
                io.tunables.clone_from(t);
                io.tunables_changed = true;
            }
            inst.step(k as f64 * step, io).unwrap();
            io.tunables_changed = false;
            trace[i].push(io.outputs.clone());
        }
    }
    for (inst, _) in &mut instances {
        inst.terminate();
    }
    trace
}

fn assert_same(raw: &[Vec<Vec<Buffer>>], fmu: &[Vec<Vec<Buffer>>]) {
    assert_eq!(raw.len(), fmu.len());
    for (i, (r, f)) in raw.iter().zip(fmu).enumerate() {
        assert_eq!(r.len(), f.len(), "instance {i}");
        for (k, (ro, fo)) in r.iter().zip(f).enumerate() {
            assert_eq!(ro, fo, "instance {i}, step {k}");
        }
    }
}

#[test]
fn the_dry_run_lists_variables_and_structural_parameters() {
    let (d, wrapper) = plan(
        &package("blob_model"),
        &BuildOptions {
            targets: vec![Target::host().unwrap()],
            bundle: Vec::new(),
            output: PathBuf::from("unused.fmu"),
        },
    )
    .unwrap();
    assert_eq!(d.interface.name, "blob");
    assert_eq!(wrapper.model_identifier, "blob");
    let names: Vec<&str> = wrapper.structural.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["nu", "ny", "np"]);
    assert_eq!(wrapper.structural[0].start, 1, "no default: min");
    assert_eq!(wrapper.structural[0].max, Some(8));
    let title = wrapper
        .variables
        .iter()
        .find(|v| v.name == "title")
        .unwrap();
    assert_eq!(title.fmi_type, "String");
    assert_eq!(title.causality, "parameter (fixed)");
    let gain = wrapper.variables.iter().find(|v| v.name == "gain").unwrap();
    assert_eq!(gain.shape, "[ny, nu]");
    assert!(wrapper.variable_step, "blob is not told its step");
    assert!(!wrapper.resettable, "no terminate call");
    assert!(
        wrapper
            .model_description
            .contains("canBeInstantiatedOnlyOncePerProcess=\"true\"")
    );
    assert!(
        wrapper
            .model_description
            .contains(r#"<text capacity="16"/>"#)
    );
    assert!(
        wrapper
            .source
            .contains("_Static_assert(offsetof(blob_output, dim_nu)")
    );
}

#[test]
fn the_state_space_fmu_matches_the_raw_adapter_at_two_sizes() {
    let raw = RawModel::load(&package("ss")).unwrap();
    let fmu = FmuAdapter::load(fmu("ss")).unwrap();
    assert_eq!(fmu.interface().name, raw.interface().name);
    assert_eq!(fmu.interface().dimensions.len(), 1);
    assert!(!fmu.interface().variables.is_empty());
    assert_eq!(
        fmu.interface().variables.len(),
        raw.interface().variables.len()
    );
    let script = |sign: f64, k_new: f64| -> Vec<Step> {
        (0..12)
            .map(|k| Step {
                inputs: vec![f64s(&[sign * (k as f64 * 0.3).sin()])],
                tunables: (k == 8).then(|| vec![f64s(&[k_new])]),
            })
            .collect()
    };
    let (ref3, ref5) = (Ss::new(3, 2.0), Ss::new(5, 0.5));
    let runs = vec![
        (ref3.spec("three"), ref3.io(), script(1.0, 4.0)),
        (ref5.spec("five"), ref5.io(), script(-1.0, 0.25)),
    ];
    let from_raw = drive(&raw, &runs, 0.01);
    let from_fmu = drive(&fmu, &runs, 0.01);
    assert_same(&from_raw, &from_fmu);
    // And the values are the model's, not zeros.
    let mut reference = Ss::new(3, 2.0);
    for (k, outputs) in from_fmu[0].iter().skip(1).enumerate() {
        if k == 8 {
            reference.k = 4.0;
        }
        let y = reference.step((k as f64 * 0.3).sin());
        assert!((as_f64s(&outputs[1])[0] - y).abs() < 1e-12, "step {k}");
    }
}

#[test]
fn the_pi_fmu_matches_the_raw_adapter_with_handles() {
    let raw = RawModel::load(&package("pi")).unwrap();
    let fmu = FmuAdapter::load(fmu("pi")).unwrap();
    assert!(
        fmu.interface().variables.len() == raw.interface().variables.len(),
        "{:?}",
        fmu.interface()
    );
    let (spec2, _, io2) = pi_spec("two", 2, 0.3);
    let (spec4, _, io4) = pi_spec("four", 4, 0.7);
    let script2: Vec<Step> = (0..10)
        .map(|k| Step {
            inputs: vec![f64s(&[1.0, -1.0]), f64s(&[0.1 * k as f64, 0.05 * k as f64])],
            tunables: None,
        })
        .collect();
    let script4: Vec<Step> = (0..11)
        .map(|k| Step {
            inputs: vec![
                f64s(&[2.0, 1.0, 0.0, -1.0]),
                f64s(&[0.0, 0.2 * k as f64, 0.3, 0.1 * k as f64]),
            ],
            tunables: (k == 10).then(|| vec![f64s(&[0.0])]),
        })
        .collect();
    let runs = vec![(spec2, io2, script2), (spec4, io4, script4)];
    assert_same(&drive(&raw, &runs, 0.1), &drive(&fmu, &runs, 0.1));
}

#[test]
fn the_single_entry_fmu_matches_the_raw_adapter_with_text() {
    let raw = RawModel::load(&package("blob_model")).unwrap();
    let fmu = FmuAdapter::load(fmu("blob_model")).unwrap();
    let title = fmu
        .interface()
        .variables
        .iter()
        .find(|v| v.name == "title")
        .unwrap();
    assert_eq!(
        title.shape,
        vec![taktwerk_core::value::Dim::Literal(16)],
        "the capacity annotation"
    );
    let (spec, io) = blob_spec("blob", 2, "tank 3");
    let script: Vec<Step> = (1..=5)
        .map(|k| Step {
            inputs: vec![f64s(&[0.5 * k as f64, -1.0])],
            tunables: (k == 4).then(|| vec![f64s(&[-0.5])]),
        })
        .collect();
    let runs = vec![(spec, io, script)];
    let from_raw = drive(&raw, &runs, 0.1);
    let from_fmu = drive(&fmu, &runs, 0.1);
    assert_same(&from_raw, &from_fmu);
    // Text travelled both ways: the parameter in, the label and names out.
    assert_eq!(text(&from_fmu[0][0][1]), "tank 3");
    assert_eq!(text(&from_fmu[0][0][5]), "k;u;y;p");
    assert_eq!(from_fmu[0][5][0], Buffer::I32(vec![5]));
    assert_eq!(from_fmu[0][5][6], Buffer::U8(vec![2]));
}

#[test]
fn the_fmu_checks_reported_sizes_like_the_adapter() {
    let fmu = FmuAdapter::load(fmu("blob_model")).unwrap();
    let (spec, mut io) = blob_spec("wide", 3, "x");
    let mut inst = fmu.instantiate(&spec).unwrap();
    let err = inst.init(0.0, &mut io).unwrap_err();
    assert!(
        matches!(err, ModelError::Call { .. }),
        "the wrapper fails ExitInitializationMode: {err}"
    );
}

/// The wrapped blob FMU driven by the Modelica Association Reference-FMUs import library
/// (`FMI.c`, `FMI3.c` of the checkout the FMI tests build), an importer taktwerk did not write.
/// Needs git, network and cc on first use.
#[test]
fn an_independent_importer_runs_the_wrapped_fmu() {
    let tmp = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let reference = tmp.join("reference-fmus");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../taktwerk-fmi/tests/reference-fmus.sh");
    let status = std::process::Command::new("bash")
        .arg(&script)
        .arg(&reference)
        .status()
        .unwrap();
    assert!(status.success(), "fetching the Reference-FMUs failed");
    let src = reference.join("src");
    let driver = tmp.join("fmi_import_driver");
    let status = std::process::Command::new("cc")
        .args(["-O1", "-o"])
        .arg(&driver)
        .arg(format!("-I{}", src.join("include").display()))
        .arg(src.join("src/FMI.c"))
        .arg(src.join("src/FMI3.c"))
        .arg(format!("{FIXTURES}/fmi_import_driver.c"))
        .args(["-ldl", "-lm"])
        .status()
        .unwrap();
    assert!(status.success(), "building the import driver failed");

    // Extract the FMU; the importer takes the platform binary and the resources directory.
    let unpacked = tmp.join("blob-fmu");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked).unwrap();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(fmu("blob_model")).unwrap()).unwrap();
    zip.extract(&unpacked).unwrap();
    let xml = std::fs::read_to_string(unpacked.join("modelDescription.xml")).unwrap();
    let token = xml
        .split("instantiationToken=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap();
    let binary = unpacked
        .join("binaries")
        .join(format!("{}-linux", std::env::consts::ARCH))
        .join("blob.so");
    let output = std::process::Command::new(&driver)
        .arg(&binary)
        .arg(token)
        .arg(format!("{}/", unpacked.join("resources").display()))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "driver: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let values = |key: &str| -> Vec<String> {
        stdout
            .lines()
            .filter_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_owned())
            .collect()
    };

    // The same script through the raw adapter.
    let raw = RawModel::load(&package("blob_model")).unwrap();
    let (spec, io) = blob_spec("blob", 2, "tank 3");
    let script: Vec<Step> = (1..=5)
        .map(|k| Step {
            inputs: vec![f64s(&[0.5 * k as f64, -1.0])],
            tunables: (k == 4).then(|| vec![f64s(&[-0.5])]),
        })
        .collect();
    let from_raw = drive(&raw, &[(spec, io, script)], 0.1);
    assert_eq!(values("label "), [text(&from_raw[0][0][1])]);
    assert_eq!(values("names "), [text(&from_raw[0][0][5])]);
    assert_eq!(values("ready "), ["2"]);
    let gain: Vec<f64> = values("gain ").iter().map(|v| v.parse().unwrap()).collect();
    assert_close(&gain, &as_f64s(&from_raw[0][0][3]));
    let y: Vec<f64> = values("y ").iter().map(|v| v.parse().unwrap()).collect();
    let want: Vec<f64> = from_raw[0][1..]
        .iter()
        .flat_map(|outputs| as_f64s(&outputs[2]))
        .collect();
    assert_close(&y, &want);
    assert_eq!(values("counter "), ["1", "2", "3", "4", "5"]);
}
