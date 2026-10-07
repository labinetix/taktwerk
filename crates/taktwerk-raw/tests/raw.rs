//! The adapter against two C fixtures compiled at test time with the system `cc`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail loudly"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use taktwerk_core::model::{InstanceSpec, ModelAdapter, ModelError, StepIo};
use taktwerk_core::value::Buffer;
use taktwerk_raw::descriptor::DESCRIPTOR_FILE;
use taktwerk_raw::{Descriptor, RawModel, arch_dir, import_header};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// The descriptor documented in the crate (`ss.h`), confirmed.
const SS_DESCRIPTOR: &str = r#"
name = "state-space"
instances = "single"

[[dimensions]]
name = "nx"
min = 1
max = 64

[[variables]]
name = "A"
causality = "parameter"
type = "f64"
shape = ["nx", "nx"]

[[variables]]
name = "b"
causality = "parameter"
type = "f64"
shape = ["nx"]

[[variables]]
name = "c"
causality = "parameter"
type = "f64"
shape = ["nx"]

[[variables]]
name = "k"
causality = "tunable"
type = "f64"

[[variables]]
name = "u"
causality = "input"
type = "f64"

[[variables]]
name = "x"
causality = "output"
type = "f64"
shape = ["nx"]

[[variables]]
name = "y"
causality = "output"
type = "f64"

[abi]
confirmed = true

[abi.init]
symbol = "ss_init"
args = [{ struct = "ss_params" }, { builtin = "step_size" }]

[abi.step]
symbol = "ss_step"
args = [{ struct = "ss_params" }, { struct = "ss_io" }, { builtin = "time" }]

[abi.terminate]
symbol = "ss_terminate"
returns = "void"

[abi.structs.ss_params]
members = [
  { name = "nx", type = "int", dim = "nx" },
  { name = "A", type = "const double *", variable = "A" },
  { name = "b", type = "const double *", variable = "b" },
  { name = "c", type = "const double *", variable = "c" },
  { name = "k", type = "double", variable = "k" },
]

[abi.structs.ss_io]
members = [
  { name = "u", type = "double", variable = "u" },
  { name = "x", type = "double *", variable = "x" },
  { name = "y", type = "double", variable = "y" },
]
"#;

/// `pi.h`: a handle per instance, arrays and a by-value tunable in the step call.
const PI_DESCRIPTOR: &str = r#"
name = "pi"
instances = "multiple"

[[dimensions]]
name = "n"
min = 1

[[variables]]
name = "sp"
causality = "input"
type = "f64"
shape = ["n"]

[[variables]]
name = "pv"
causality = "input"
type = "f64"
shape = ["n"]

[[variables]]
name = "out"
causality = "output"
type = "f64"
shape = ["n"]

[[variables]]
name = "kp"
causality = "parameter"
type = "f64"
shape = ["n"]

[[variables]]
name = "ki"
causality = "tunable"
type = "f64"

[abi]
confirmed = true

[abi.init]
symbol = "pi_create"
args = [{ handle = "out" }, { dim = "n", type = "int" }, { builtin = "step_size" }]

[abi.step]
symbol = "pi_step"
args = [
  { handle = "in" }, { array = "sp" }, { array = "pv" }, { array = "out" },
  { array = "kp" }, { value = "ki" }, { dim = "n", type = "int" },
]

[abi.terminate]
symbol = "pi_destroy"
args = [{ handle = "in" }]
"#;

/// Compiled fixtures, built once per test binary.
struct Built {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn built() -> &'static Built {
    static BUILT: OnceLock<Built> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        for (name, descriptor) in [("ss", SS_DESCRIPTOR), ("pi", PI_DESCRIPTOR)] {
            let package = root.join(name);
            let lib = package.join("lib").join(arch_dir());
            std::fs::create_dir_all(&lib).unwrap();
            cc(&[
                "-shared",
                "-fPIC",
                "-O2",
                "-o",
                lib.join(format!("lib{name}.so")).to_str().unwrap(),
                &format!("{FIXTURES}/{name}.c"),
            ]);
            std::fs::write(package.join(DESCRIPTOR_FILE), descriptor).unwrap();
        }
        cc(&[
            "-o",
            root.join("ss_offsets").to_str().unwrap(),
            &format!("{FIXTURES}/ss_offsets.c"),
        ]);
        Built { _dir: dir, root }
    })
}

fn cc(args: &[&str]) {
    let status = Command::new("cc")
        .args(args)
        .current_dir(FIXTURES)
        .status()
        .expect("cc on PATH");
    assert!(status.success(), "cc {args:?}");
}

fn package(name: &str) -> PathBuf {
    built().root.join(name)
}

/// A package directory with a different descriptor but the same library.
fn variant(name: &str, descriptor: &str) -> PathBuf {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let dir = built().root.join(format!(
        "{name}-variant-{}",
        COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    let lib = dir.join("lib").join(arch_dir());
    std::fs::create_dir_all(&lib).unwrap();
    let so = format!("lib{name}.so");
    std::fs::copy(
        package(name).join("lib").join(arch_dir()).join(&so),
        lib.join(&so),
    )
    .unwrap();
    std::fs::write(dir.join(DESCRIPTOR_FILE), descriptor).unwrap();
    dir
}

fn f64s(values: &[f64]) -> Buffer {
    Buffer::F64(values.to_vec())
}

/// The error of a refused instantiation.
fn refused(model: &RawModel, spec: &InstanceSpec) -> ModelError {
    match model.instantiate(spec) {
        Ok(_) => panic!("{}: instantiated", spec.id),
        Err(e) => e,
    }
}

/// Equal up to the rounding the C compiler's fused multiply-adds introduce.
fn assert_close(got: &[f64], want: &[f64]) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 1e-12, "{got:?} vs {want:?}");
    }
}

fn as_f64s(buffer: &Buffer) -> Vec<f64> {
    match buffer {
        Buffer::F64(v) => v.clone(),
        other => panic!("not f64: {other:?}"),
    }
}

// ==========================================================================
// Header import and layout.
// ==========================================================================

#[test]
fn the_header_import_proposes_the_documented_descriptor_unconfirmed() {
    let proposal = import_header(Path::new(&format!("{FIXTURES}/ss.h"))).unwrap();
    let d = &proposal.descriptor;
    assert!(!d.abi.confirmed);
    assert_eq!(d.abi.init.symbol, "ss_init");
    assert_eq!(d.abi.step.symbol, "ss_step");
    assert_eq!(d.abi.terminate.as_ref().unwrap().symbol, "ss_terminate");
    assert_eq!(d.interface.dimensions.len(), 1);
    assert_eq!(d.interface.dimensions[0].name, "nx");
    let a = d
        .interface
        .variables
        .iter()
        .find(|v| v.name == "A")
        .unwrap();
    assert_eq!(
        a.shape.len(),
        1,
        "a matrix is proposed as a vector and noted"
    );
    assert!(d.abi.structs.contains_key("ss_params"));
    assert!(d.abi.structs.contains_key("ss_io"));
    assert_eq!(
        d.abi.init.args[1].builtin,
        Some(taktwerk_raw::descriptor::Builtin::StepSize)
    );
    assert!(
        proposal.notes.iter().any(|n| n.contains("A sized by nx")),
        "{:?}",
        proposal.notes
    );

    // The proposal is refused as a package until confirmed.
    let text = proposal.to_toml().unwrap();
    let dir = variant("ss", &text);
    let err = RawModel::load(&dir).unwrap_err();
    assert!(
        matches!(err, ModelError::Load(ref m) if m.contains("confirmed")),
        "{err}"
    );
}

#[test]
fn struct_layouts_match_the_compiler() {
    let output = Command::new(built().root.join("ss_offsets"))
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let plan = Descriptor::parse(SS_DESCRIPTOR)
        .unwrap()
        .validate()
        .unwrap();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let name = fields.next().unwrap();
        let numbers: Vec<usize> = fields.map(|n| n.parse().unwrap()).collect();
        let s = plan.structs.iter().find(|s| s.name == name).unwrap();
        assert_eq!(s.layout.size(), numbers[0], "{name} size");
        let offsets: Vec<usize> = s.layout.members().iter().map(|m| m.offset).collect();
        assert_eq!(offsets, numbers[1..], "{name} offsets");
    }
}

// ==========================================================================
// The state-space fixture: one package, two sizes, private copies.
// ==========================================================================

/// Plant parameters for size `nx`: a shift register with feedback, so every state moves.
struct Ss {
    nx: usize,
    a: Vec<f64>,
    b: Vec<f64>,
    c: Vec<f64>,
    k: f64,
    x: Vec<f64>,
}

impl Ss {
    fn new(nx: usize, k: f64) -> Self {
        let mut a = vec![0.0; nx * nx];
        for i in 0..nx {
            a[i * nx + i] = 0.5;
            if i + 1 < nx {
                a[i * nx + i + 1] = 0.25;
            }
        }
        Self {
            nx,
            a,
            b: (0..nx).map(|i| 1.0 + i as f64).collect(),
            c: (0..nx).map(|i| 0.1 * (i + 1) as f64).collect(),
            k,
            x: vec![0.0; nx],
        }
    }

    fn step(&mut self, u: f64) -> f64 {
        let xn: Vec<f64> = (0..self.nx)
            .map(|i| {
                let row = &self.a[i * self.nx..(i + 1) * self.nx];
                let acc: f64 = row.iter().zip(&self.x).map(|(a, x)| a * x).sum();
                acc + self.b[i] * u
            })
            .collect();
        self.x = xn;
        self.k * self.c.iter().zip(&self.x).map(|(c, x)| c * x).sum::<f64>()
    }

    fn spec(&self, id: &str) -> InstanceSpec {
        let mut params = BTreeMap::new();
        params.insert("A".to_owned(), f64s(&self.a));
        params.insert("b".to_owned(), f64s(&self.b));
        params.insert("c".to_owned(), f64s(&self.c));
        params.insert("k".to_owned(), f64s(&[self.k]));
        InstanceSpec {
            id: id.to_owned(),
            dims: BTreeMap::from([("nx".to_owned(), self.nx)]),
            params,
            step_size: 0.01,
        }
    }

    fn io(&self) -> StepIo {
        StepIo {
            inputs: vec![f64s(&[0.0])],
            outputs: vec![f64s(&vec![0.0; self.nx]), f64s(&[0.0])],
            tunables: vec![f64s(&[self.k])],
            tunables_changed: false,
        }
    }
}

#[test]
fn one_package_runs_at_nx_3_and_nx_5_in_isolation() {
    let model = RawModel::load(&package("ss")).unwrap();
    assert_eq!(model.interface().name, "state-space");
    let (mut ref3, mut ref5) = (Ss::new(3, 2.0), Ss::new(5, 0.5));
    let mut inst3 = model.instantiate(&ref3.spec("three")).unwrap();
    let mut inst5 = model.instantiate(&ref5.spec("five")).unwrap();
    let (mut io3, mut io5) = (ref3.io(), ref5.io());
    inst3.init(0.0, &mut io3).unwrap();
    inst5.init(0.0, &mut io5).unwrap();
    // Interleaved steps: shared globals would make one instance see the other's size and state.
    for k in 0..20 {
        let u = (k as f64 * 0.3).sin();
        io3.inputs[0] = f64s(&[u]);
        io5.inputs[0] = f64s(&[-u]);
        inst3.step(k as f64 * 0.01, &mut io3).unwrap();
        inst5.step(k as f64 * 0.01, &mut io5).unwrap();
        let y3 = ref3.step(u);
        let y5 = ref5.step(-u);
        assert!(
            (as_f64s(&io3.outputs[1])[0] - y3).abs() < 1e-12,
            "step {k} nx=3"
        );
        assert!(
            (as_f64s(&io5.outputs[1])[0] - y5).abs() < 1e-12,
            "step {k} nx=5"
        );
        assert_close(&as_f64s(&io3.outputs[0]), &ref3.x);
        assert_close(&as_f64s(&io5.outputs[0]), &ref5.x);
    }
    // A changed tunable reaches the library before the next step.
    ref3.k = 4.0;
    io3.tunables[0] = f64s(&[4.0]);
    io3.tunables_changed = true;
    io3.inputs[0] = f64s(&[0.0]);
    inst3.step(0.2, &mut io3).unwrap();
    assert!((as_f64s(&io3.outputs[1])[0] - ref3.step(0.0)).abs() < 1e-12);
    inst3.terminate();
    inst5.terminate();
}

#[test]
fn dimensions_outside_min_max_and_unknown_ones_are_refused() {
    let model = RawModel::load(&package("ss")).unwrap();
    for nx in [0_usize, 65] {
        let mut spec = Ss::new(1, 1.0).spec("bad");
        spec.dims.insert("nx".to_owned(), nx);
        let err = refused(&model, &spec);
        assert!(
            matches!(err, ModelError::Instantiate(ref m) if m.contains("outside")),
            "{err}"
        );
    }
    let mut spec = Ss::new(2, 1.0).spec("bad");
    spec.dims.insert("ny".to_owned(), 2);
    assert!(model.instantiate(&spec).is_err());
    let mut spec = Ss::new(2, 1.0).spec("bad");
    spec.dims.clear();
    assert!(
        matches!(refused(&model, &spec), ModelError::Instantiate(ref m) if m.contains("not bound"))
    );
    let mut spec = Ss::new(2, 1.0).spec("bad");
    spec.params.insert("A".to_owned(), f64s(&[1.0]));
    assert!(
        model.instantiate(&spec).is_err(),
        "a parameter of the wrong length"
    );
    spec.params.remove("A");
    spec.params.insert("u".to_owned(), f64s(&[1.0]));
    assert!(
        model.instantiate(&spec).is_err(),
        "an input is not a parameter"
    );
}

// ==========================================================================
// The PI fixture: handles, arrays, a by-value tunable, a non-zero return.
// ==========================================================================

struct Pi {
    n: usize,
    kp: Vec<f64>,
    ki: f64,
    dt: f64,
    integral: Vec<f64>,
}

impl Pi {
    fn step(&mut self, sp: &[f64], pv: &[f64]) -> Vec<f64> {
        (0..self.n)
            .map(|i| {
                let e = sp[i] - pv[i];
                self.integral[i] += self.ki * e * self.dt;
                self.kp[i] * e + self.integral[i]
            })
            .collect()
    }
}

fn pi_spec(id: &str, n: usize, ki: f64) -> (InstanceSpec, Pi, StepIo) {
    let kp: Vec<f64> = (0..n).map(|i| 1.0 + 0.5 * i as f64).collect();
    let spec = InstanceSpec {
        id: id.to_owned(),
        dims: BTreeMap::from([("n".to_owned(), n)]),
        params: BTreeMap::from([("kp".to_owned(), f64s(&kp)), ("ki".to_owned(), f64s(&[ki]))]),
        step_size: 0.1,
    };
    let reference = Pi {
        n,
        kp,
        ki,
        dt: 0.1,
        integral: vec![0.0; n],
    };
    let io = StepIo {
        inputs: vec![f64s(&vec![0.0; n]), f64s(&vec![0.0; n])],
        outputs: vec![f64s(&vec![0.0; n])],
        tunables: vec![f64s(&[ki])],
        tunables_changed: false,
    };
    (spec, reference, io)
}

#[test]
fn a_handle_library_hosts_two_instances_of_different_size() {
    let model = RawModel::load(&package("pi")).unwrap();
    let (spec2, mut ref2, mut io2) = pi_spec("two", 2, 0.3);
    let (spec4, mut ref4, mut io4) = pi_spec("four", 4, 0.7);
    let mut inst2 = model.instantiate(&spec2).unwrap();
    let mut inst4 = model.instantiate(&spec4).unwrap();
    inst2.init(0.0, &mut io2).unwrap();
    inst4.init(0.0, &mut io4).unwrap();
    for k in 0..10 {
        let sp2 = vec![1.0, -1.0];
        let pv2 = vec![0.1 * k as f64, 0.05 * k as f64];
        let sp4 = vec![2.0, 1.0, 0.0, -1.0];
        let pv4 = vec![0.0, 0.2 * k as f64, 0.3, 0.1 * k as f64];
        io2.inputs[0] = f64s(&sp2);
        io2.inputs[1] = f64s(&pv2);
        io4.inputs[0] = f64s(&sp4);
        io4.inputs[1] = f64s(&pv4);
        inst2.step(k as f64 * 0.1, &mut io2).unwrap();
        inst4.step(k as f64 * 0.1, &mut io4).unwrap();
        let (want2, want4) = (ref2.step(&sp2, &pv2), ref4.step(&sp4, &pv4));
        for (got, want) in as_f64s(&io2.outputs[0]).iter().zip(&want2) {
            assert!((got - want).abs() < 1e-12);
        }
        for (got, want) in as_f64s(&io4.outputs[0]).iter().zip(&want4) {
            assert!((got - want).abs() < 1e-12);
        }
    }
    // A changed by-value tunable reaches the next step.
    ref4.ki = 0.0;
    io4.tunables[0] = f64s(&[0.0]);
    io4.tunables_changed = true;
    let (sp, pv) = (vec![2.0, 1.0, 0.0, -1.0], vec![0.0, 0.0, 0.3, 0.0]);
    io4.inputs[0] = f64s(&sp);
    io4.inputs[1] = f64s(&pv);
    inst4.step(1.0, &mut io4).unwrap();
    assert_close(&as_f64s(&io4.outputs[0]), &ref4.step(&sp, &pv));
    inst2.terminate();
    inst4.terminate();
    inst4.terminate();
    assert!(
        inst4.step(1.2, &mut io4).is_err(),
        "no step after terminate"
    );
}

#[test]
fn a_return_code_outside_ok_codes_fails_the_call_with_its_code() {
    // min = 0 lets n = 0 through to the library, which answers -2.
    let dir = variant("pi", &PI_DESCRIPTOR.replace("min = 1", "min = 0"));
    let model = RawModel::load(&dir).unwrap();
    let (spec, _, mut io) = pi_spec("zero", 0, 0.1);
    let mut inst = model.instantiate(&spec).unwrap();
    let err = inst.init(0.0, &mut io).unwrap_err();
    match err {
        ModelError::Call { call, code, .. } => {
            assert_eq!(call, "init");
            assert_eq!(code, -2);
        }
        other => panic!("{other}"),
    }
    // Declared as acceptable, the same code passes.
    let dir = variant(
        "pi",
        &PI_DESCRIPTOR
            .replace("min = 1", "min = 0")
            .replace("confirmed = true", "confirmed = true\nok_codes = [0, -2]"),
    );
    let model = RawModel::load(&dir).unwrap();
    let (spec, _, mut io) = pi_spec("zero", 0, 0.1);
    let mut inst = model.instantiate(&spec).unwrap();
    inst.init(0.0, &mut io).unwrap();
}

#[test]
fn a_missing_symbol_or_library_is_refused_at_load() {
    let dir = variant("pi", &PI_DESCRIPTOR.replace("pi_create", "pi_make"));
    let err = RawModel::load(&dir).unwrap_err();
    assert!(
        matches!(err, ModelError::Load(ref m) if m.contains("pi_make")),
        "{err}"
    );
    let dir = variant(
        "pi",
        &PI_DESCRIPTOR.replace(
            "confirmed = true",
            "confirmed = true\nlibrary = \"libother.so\"",
        ),
    );
    assert!(RawModel::load(&dir).is_err());
}
