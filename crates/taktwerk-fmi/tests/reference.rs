//! Runs the Modelica Association Reference FMUs, built for this host by `reference-fmus.sh`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail loudly"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use taktwerk_core::model::{
    BoundDims, Causality, InstanceSpec, Instances, ModelAdapter, ModelInterface, ParamValues,
    StepIo,
};
use taktwerk_core::value::{Buffer, Dim};
use taktwerk_fmi::{FmiVersion, FmuAdapter};

/// Directory of the built FMUs; builds them on first use (needs git, network and cc).
fn fmus() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("reference-fmus");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reference-fmus.sh");
        let status = Command::new("bash").arg(script).arg(&dir).status().unwrap();
        assert!(status.success(), "building the Reference FMUs failed");
        dir
    })
}

fn load(version: u8, model: &str) -> FmuAdapter {
    FmuAdapter::load(fmus().join(format!("fmi{version}")).join(model)).unwrap()
}

/// Buffers for every input, output and tunable at the bound sizes.
fn io_for(iface: &ModelInterface, dims: &BoundDims) -> StepIo {
    let mut io = StepIo::default();
    for v in &iface.variables {
        let len = v
            .shape
            .iter()
            .map(|d| match d {
                Dim::Literal(n) => *n,
                Dim::Symbol(s) => dims[s],
            })
            .product();
        let buf = Buffer::zeroed(v.ty, len);
        match v.causality {
            Causality::Input => io.inputs.push(buf),
            Causality::Output => io.outputs.push(buf),
            Causality::Tunable => io.tunables.push(buf),
            Causality::Parameter => {}
        }
    }
    io
}

fn spec(id: &str, dims: BoundDims, params: ParamValues, step_size: f64) -> InstanceSpec {
    InstanceSpec {
        id: id.into(),
        dims,
        params,
        step_size,
    }
}

fn f64s(b: &Buffer) -> &[f64] {
    match b {
        Buffer::F64(v) => v,
        other => panic!("not f64: {other:?}"),
    }
}

/// Run `steps` steps from 0 and return the scalar f64 outputs after init and every step.
fn trajectory(adapter: &FmuAdapter, params: ParamValues, h: f64, steps: usize) -> Vec<Vec<f64>> {
    let mut inst = adapter
        .instantiate(&spec("a", BoundDims::new(), params, h))
        .unwrap();
    let mut io = io_for(adapter.interface(), &BoundDims::new());
    inst.init(0.0, &mut io).unwrap();
    let row = |io: &StepIo| io.outputs.iter().map(|b| f64s(b)[0]).collect::<Vec<_>>();
    let mut rows = vec![row(&io)];
    for k in 0..steps {
        inst.step(k as f64 * h, &mut io).unwrap();
        rows.push(row(&io));
    }
    inst.terminate();
    rows
}

/// The upstream reference result `<Model>_out.csv`, without the time column.
fn reference_csv(model: &str) -> Vec<Vec<f64>> {
    let src = fmus().join("src");
    fs::read_to_string(src.join(model).join(format!("{model}_out.csv")))
        .unwrap()
        .lines()
        .skip(1)
        .map(|l| l.split(',').skip(1).map(|x| x.parse().unwrap()).collect())
        .collect()
}

fn assert_close(got: &[Vec<f64>], want: &[Vec<f64>], tol: f64) {
    assert_eq!(got.len(), want.len());
    for (k, (g, w)) in got.iter().zip(want).enumerate() {
        for (a, b) in g.iter().zip(w) {
            assert!(
                (a - b).abs() <= tol * (1.0 + b.abs()),
                "row {k}: {g:?} vs {w:?}"
            );
        }
    }
}

fn dahlquist(version: u8) {
    let adapter = load(version, "Dahlquist");
    let iface = adapter.interface();
    assert_eq!(iface.instances, Instances::Multiple);
    assert_eq!(iface.variables.len(), 2); // x, k
    // Forward Euler at the communication step: x_n = (1 - k h)^n.
    let rows = trajectory(&adapter, ParamValues::new(), 0.1, 10);
    for (n, r) in rows.iter().enumerate() {
        assert!((r[0] - 0.9f64.powi(n as i32)).abs() < 1e-12, "{n}: {r:?}");
    }
    let k2 = ParamValues::from([("k".to_owned(), Buffer::F64(vec![2.0]))]);
    let rows = trajectory(&adapter, k2, 0.1, 10);
    assert!((rows[10][0] - 0.8f64.powi(10)).abs() < 1e-12);
}

#[test]
fn dahlquist_fmi3() {
    assert_eq!(load(3, "Dahlquist").fmi_version(), FmiVersion::V3);
    dahlquist(3);
}

#[test]
fn dahlquist_fmi2() {
    assert_eq!(load(2, "Dahlquist").fmi_version(), FmiVersion::V2);
    dahlquist(2);
}

fn against_reference(version: u8, model: &str, steps: usize) {
    let adapter = load(version, model);
    let rows = trajectory(&adapter, ParamValues::new(), 0.01, steps);
    assert_close(&rows, &reference_csv(model), 1e-9);
}

#[test]
fn bouncing_ball_fmi3() {
    against_reference(3, "BouncingBall", 300);
}

#[test]
fn bouncing_ball_fmi2() {
    against_reference(2, "BouncingBall", 300);
}

#[test]
fn van_der_pol_fmi3() {
    against_reference(3, "VanDerPol", 2000);
}

#[test]
fn van_der_pol_fmi2() {
    against_reference(2, "VanDerPol", 2000);
}

#[test]
fn tunable_reads_back_and_changes_live() {
    let adapter = load(3, "BouncingBall");
    let mut inst = adapter
        .instantiate(&spec("b", BoundDims::new(), ParamValues::new(), 0.01))
        .unwrap();
    let mut io = io_for(adapter.interface(), &BoundDims::new());
    inst.init(0.0, &mut io).unwrap();
    assert_eq!(f64s(&io.tunables[0]), [0.7]);
    io.tunables[0] = Buffer::F64(vec![0.9]);
    io.tunables_changed = true;
    inst.step(0.0, &mut io).unwrap();
    inst.terminate();
    assert!(inst.step(0.01, &mut io).is_err());
}

/// `StateSpace` with A = B = C = D = I and x0 = 0: x' = x + u, y = x + u = u e^t.
fn state_space(adapter: &FmuAdapter, n: usize) -> Vec<f64> {
    let dims = BoundDims::from([
        ("m".to_owned(), n),
        ("n".to_owned(), n),
        ("r".to_owned(), n),
    ]);
    let h = 0.1;
    let mut inst = adapter
        .instantiate(&spec("ss", dims.clone(), ParamValues::new(), h))
        .unwrap();
    let mut io = io_for(adapter.interface(), &dims);
    let u: Vec<f64> = (1..=n).map(|i| i as f64).collect();
    io.inputs[0] = Buffer::F64(u);
    inst.init(0.0, &mut io).unwrap();
    for k in 0..10 {
        inst.step(f64::from(k) * h, &mut io).unwrap();
    }
    inst.terminate();
    f64s(&io.outputs[0]).to_vec()
}

#[test]
fn state_space_structural_parameters_at_two_sizes() {
    let adapter = load(3, "StateSpace");
    let iface = adapter.interface();
    let dims: Vec<_> = iface.dimensions.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(dims, ["m", "n", "r"]);
    assert_eq!(iface.dimensions[1].default, Some(3));
    let a = iface.variables.iter().find(|v| v.name == "A").unwrap();
    assert_eq!(a.shape, [Dim::Symbol("n".into()), Dim::Symbol("n".into())]);
    assert_eq!(a.causality, Causality::Tunable);

    for n in [2, 4] {
        let y = state_space(&adapter, n);
        assert_eq!(y.len(), n);
        for (i, yi) in y.iter().enumerate() {
            let want = (i + 1) as f64 * 1f64.exp();
            assert!((yi - want).abs() / want < 1e-3, "n={n}: {y:?}");
        }
    }

    let too_big = BoundDims::from([("n".to_owned(), 6)]);
    assert!(
        adapter
            .instantiate(&spec("x", too_big, ParamValues::new(), 0.1))
            .is_err()
    );
}

#[test]
fn refuses_binary_variables() {
    let err = FmuAdapter::load(fmus().join("fmi3/Feedthrough")).unwrap_err();
    assert!(err.to_string().contains("Binary"), "{err}");
}

/// Feedthrough without its Binary variables: the String input echoes to the String output.
#[test]
fn strings_travel_as_byte_buffers() {
    let tmp = tempfile::tempdir().unwrap();
    copy_fmu(&fmus().join("fmi3/Feedthrough"), tmp.path(), false);
    let md = tmp.path().join("modelDescription.xml");
    let xml = fs::read_to_string(&md).unwrap();
    let start = xml.find("<Binary name=\"Binary_input\"").unwrap();
    let end = xml[start..].find("causality=\"output\"/>").unwrap() + start;
    let end = end + xml[end..].find("/>").unwrap() + 2;
    fs::write(&md, format!("{}{}", &xml[..start], &xml[end..])).unwrap();
    let adapter = FmuAdapter::load(tmp.path()).unwrap();
    let iface = adapter.interface();
    let input = iface
        .variables
        .iter()
        .find(|v| v.name == "String_input")
        .unwrap();
    assert_eq!(input.ty, taktwerk_core::value::ScalarType::U8);
    assert_eq!(
        input.shape,
        [Dim::Literal(taktwerk_fmi::DEFAULT_TEXT_CAPACITY)]
    );
    let mut inst = adapter
        .instantiate(&spec("s", BoundDims::new(), ParamValues::new(), 0.1))
        .unwrap();
    let mut io = io_for(iface, &BoundDims::new());
    let input_index = iface
        .variables
        .iter()
        .filter(|v| v.causality == Causality::Input)
        .position(|v| v.name == "String_input")
        .unwrap();
    let output_index = iface
        .variables
        .iter()
        .filter(|v| v.causality == Causality::Output)
        .position(|v| v.name == "String_output")
        .unwrap();
    // The enumeration input accepts only its literals; the others start at zero.
    let enumeration = iface
        .variables
        .iter()
        .filter(|v| v.causality == Causality::Input)
        .position(|v| v.name == "Enumeration_input")
        .unwrap();
    io.inputs[enumeration] = Buffer::I64(vec![1]);
    let mut text = b"hello fmu".to_vec();
    text.resize(taktwerk_fmi::DEFAULT_TEXT_CAPACITY, 0);
    io.inputs[input_index] = Buffer::U8(text.clone());
    inst.init(0.0, &mut io).unwrap();
    inst.step(0.0, &mut io).unwrap();
    assert_eq!(io.outputs[output_index], Buffer::U8(text));
    inst.terminate();
}

#[test]
fn refuses_wrong_parameter() {
    let adapter = load(3, "Dahlquist");
    let wrong = ParamValues::from([("k".to_owned(), Buffer::F64(vec![1.0, 2.0]))]);
    assert!(
        adapter
            .instantiate(&spec("a", BoundDims::new(), wrong, 0.1))
            .is_err()
    );
    let unknown = ParamValues::from([("x".to_owned(), Buffer::F64(vec![1.0]))]);
    assert!(
        adapter
            .instantiate(&spec("a", BoundDims::new(), unknown, 0.1))
            .is_err()
    );
}

/// Copy an extracted FMU, optionally marking it single-instance.
fn copy_fmu(from: &Path, to: &Path, single: bool) {
    for entry in walk(from) {
        let rel = entry.strip_prefix(from).unwrap();
        let dest = to.join(rel);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(&entry, &dest).unwrap();
    }
    if single {
        let md = to.join("modelDescription.xml");
        let xml = fs::read_to_string(&md).unwrap().replacen(
            "<CoSimulation",
            "<CoSimulation canBeInstantiatedOnlyOncePerProcess=\"true\"",
            1,
        );
        fs::write(md, xml).unwrap();
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            files.extend(walk(&p));
        } else {
            files.push(p);
        }
    }
    files
}

#[test]
fn single_instance_fmu_loads_private_copies() {
    let tmp = tempfile::tempdir().unwrap();
    copy_fmu(&fmus().join("fmi3/Dahlquist"), tmp.path(), true);
    let adapter = FmuAdapter::load(tmp.path()).unwrap();
    assert_eq!(adapter.interface().instances, Instances::Single);

    let make = |k: f64| {
        let params = ParamValues::from([("k".to_owned(), Buffer::F64(vec![k]))]);
        adapter
            .instantiate(&spec("s", BoundDims::new(), params, 0.1))
            .unwrap()
    };
    let (mut a, mut b) = (make(1.0), make(2.0));
    let mut ia = io_for(adapter.interface(), &BoundDims::new());
    let mut ib = io_for(adapter.interface(), &BoundDims::new());
    a.init(0.0, &mut ia).unwrap();
    b.init(0.0, &mut ib).unwrap();
    for k in 0..5 {
        a.step(f64::from(k) * 0.1, &mut ia).unwrap();
        b.step(f64::from(k) * 0.1, &mut ib).unwrap();
    }
    assert!((f64s(&ia.outputs[0])[0] - 0.9f64.powi(5)).abs() < 1e-12);
    assert!((f64s(&ib.outputs[0])[0] - 0.8f64.powi(5)).abs() < 1e-12);
    a.terminate();
    b.terminate();
}

#[test]
fn loads_fmu_archive() {
    let from = fmus().join("fmi2/Dahlquist");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("Dahlquist.fmu");
    let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for file in walk(&from) {
        let name = file
            .strip_prefix(&from)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        zip.start_file(name, opts).unwrap();
        std::io::Write::write_all(&mut zip, &fs::read(&file).unwrap()).unwrap();
    }
    zip.finish().unwrap();

    let adapter = FmuAdapter::load(&path).unwrap();
    assert!(!adapter.binary().starts_with(&from));
    let rows = trajectory(&adapter, ParamValues::new(), 0.1, 3);
    assert!((rows[3][0] - 0.729).abs() < 1e-12);
}
