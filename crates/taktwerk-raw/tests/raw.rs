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
use taktwerk_raw::{
    Descriptor, ImportOptions, RawModel, arch_dir, import_header, import_header_with,
};

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

/// `blob_model.h`: one entry point for init and step, an id and two opaque struct pointers.
const BLOB_DESCRIPTOR: &str = r#"
name = "blob"

[[dimensions]]
name = "nu"
min = 1
max = 8

[[dimensions]]
name = "ny"
min = 1
max = 8

[[dimensions]]
name = "np"
min = 1
max = 8

[[variables]]
name = "k"
causality = "tunable"
type = "f64"

[[variables]]
name = "u"
causality = "input"
type = "f64"
shape = ["nu"]

[[variables]]
name = "counter"
causality = "output"
type = "i32"

[[variables]]
name = "title"
causality = "parameter"
type = "u8"
shape = [16]

[[variables]]
name = "label"
causality = "output"
type = "u8"
shape = [32]

[[variables]]
name = "y"
causality = "output"
type = "f64"
shape = ["ny"]

[[variables]]
name = "gain"
causality = "output"
type = "f64"
shape = ["ny", "nu"]

[[variables]]
name = "p"
causality = "output"
type = "f64"
shape = ["np"]

[[variables]]
name = "names"
causality = "output"
type = "u8"
shape = [24]

[[variables]]
name = "ready"
causality = "output"
type = "u8"

[abi]
confirmed = true

[abi.init]
symbol = "blob_call"
args = [{ const = 7, type = "int" }, { struct = "blob_input" }, { struct = "blob_output" }]

[abi.step]
symbol = "blob_call"
args = [{ const = 7, type = "int" }, { struct = "blob_input" }, { struct = "blob_output" }]

[abi.structs.blob_input]
members = [
  { name = "init_flag", type = "int32_t *", phase = { init = 1, step = 0 } },
  { name = "variant", type = "int32_t", const = 1 },
  { name = "k", type = "double *", variable = "k" },
  { name = "u", type = "double *", variable = "u" },
  { name = "counter", type = "int32_t *", variable = "counter" },
  { name = "title", type = "char *", variable = "title" },
]

[abi.structs.blob_output]
members = [
  { name = "label", type = "char *", variable = "label" },
  { name = "y", type = "double *", variable = "y" },
  { name = "gain", type = "double *", variable = "gain" },
  { name = "p", type = "double *", variable = "p" },
  { name = "names", type = "char *", variable = "names" },
  { name = "dim_nu", type = "int32_t *", dim = "nu", reported = true },
  { name = "dim_ny", type = "int32_t *", dim = "ny", reported = true },
  { name = "dim_np", type = "int32_t *", dim = "np", reported = true },
  { name = "ready", type = "uint8_t *", variable = "ready" },
]
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
        for (name, descriptor) in [
            ("ss", SS_DESCRIPTOR),
            ("pi", PI_DESCRIPTOR),
            ("blob_model", BLOB_DESCRIPTOR),
        ] {
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
        cc(&[
            "-o",
            root.join("blob_offsets").to_str().unwrap(),
            &format!("{FIXTURES}/blob_offsets.c"),
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
    for (oracle, descriptor) in [
        ("ss_offsets", SS_DESCRIPTOR),
        ("blob_offsets", BLOB_DESCRIPTOR),
    ] {
        check_layout(oracle, descriptor);
    }
}

fn check_layout(oracle: &str, descriptor: &str) {
    let output = Command::new(built().root.join(oracle)).output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.is_empty(), "{oracle} printed nothing");
    let plan = Descriptor::parse(descriptor).unwrap().validate().unwrap();
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

// ==========================================================================
// The blob fixture: a single entry point for init and step.
// ==========================================================================

fn bytes(text: &str, capacity: usize) -> Buffer {
    let mut v = text.as_bytes().to_vec();
    v.resize(capacity, 0);
    Buffer::U8(v)
}

/// The text in a NUL-terminated byte buffer.
fn text(buffer: &Buffer) -> String {
    match buffer {
        Buffer::U8(v) => {
            let end = v.iter().position(|b| *b == 0).unwrap_or(v.len());
            String::from_utf8(v[..end].to_vec()).unwrap()
        }
        other => panic!("not u8: {other:?}"),
    }
}

/// Variant 1 of the fixture reports nu = 2, ny = 3, np = 4.
fn blob_spec(id: &str, nu: usize, title: &str) -> (InstanceSpec, StepIo) {
    let (ny, np) = (3, 4);
    let spec = InstanceSpec {
        id: id.to_owned(),
        dims: BTreeMap::from([
            ("nu".to_owned(), nu),
            ("ny".to_owned(), ny),
            ("np".to_owned(), np),
        ]),
        params: BTreeMap::from([
            ("k".to_owned(), f64s(&[2.0])),
            ("title".to_owned(), bytes(title, 16)),
        ]),
        step_size: 0.1,
    };
    // Outputs in interface order: counter, label, y, gain, p, names, ready.
    let io = StepIo {
        inputs: vec![f64s(&vec![0.0; nu])],
        outputs: vec![
            Buffer::I32(vec![0]),
            Buffer::U8(vec![0; 32]),
            f64s(&vec![0.0; ny]),
            f64s(&vec![0.0; ny * nu]),
            f64s(&vec![0.0; np]),
            Buffer::U8(vec![0; 24]),
            Buffer::U8(vec![0]),
        ],
        tunables: vec![f64s(&[2.0])],
        tunables_changed: false,
    };
    (spec, io)
}

#[test]
fn a_single_entry_point_runs_init_and_steps() {
    let model = RawModel::load(&package("blob_model")).unwrap();
    let (spec, mut io) = blob_spec("blob", 2, "tank 3");
    let mut inst = model.instantiate(&spec).unwrap();
    inst.init(0.0, &mut io).unwrap();
    // Init: the phase member said 1, the library reported its sizes and filled the matrix.
    let gain = as_f64s(&io.outputs[3]);
    assert_close(&gain, &[1.0, 1.1, 2.0, 2.1, 3.0, 3.1]);
    assert_close(&as_f64s(&io.outputs[4]), &[0.0, 0.5, 1.0, 1.5]);
    assert_eq!(io.outputs[0], Buffer::I32(vec![0]));
    assert_eq!(
        text(&io.outputs[1]),
        "tank 3",
        "the caller's text echoed back"
    );
    assert_eq!(text(&io.outputs[5]), "k;u;y;p");
    assert_eq!(
        io.outputs[6],
        Buffer::U8(vec![2]),
        "a true byte that is not 1"
    );
    // Steps: the phase member says 0; the library re-reads the sizes and counts in the input
    // struct's write-back member.
    let mut k = 2.0;
    for step in 1..=5 {
        let u = [0.5 * step as f64, -1.0];
        if step == 4 {
            k = -0.5;
            io.tunables[0] = f64s(&[k]);
            io.tunables_changed = true;
        }
        io.inputs[0] = f64s(&u);
        inst.step(step as f64 * 0.1, &mut io).unwrap();
        io.tunables_changed = false;
        let want: Vec<f64> = (0..3)
            .map(|i| k * (gain[i * 2] * u[0] + gain[i * 2 + 1] * u[1]))
            .collect();
        assert_close(&as_f64s(&io.outputs[2]), &want);
        assert_eq!(io.outputs[0], Buffer::I32(vec![step]));
        assert_eq!(io.outputs[6], Buffer::U8(vec![2]));
    }
    inst.terminate();
}

#[test]
fn an_empty_title_gets_the_library_name() {
    let model = RawModel::load(&package("blob_model")).unwrap();
    let (spec, mut io) = blob_spec("untitled", 2, "");
    let mut inst = model.instantiate(&spec).unwrap();
    inst.init(0.0, &mut io).unwrap();
    assert_eq!(text(&io.outputs[1]), "blob");
}

#[test]
fn a_true_byte_mapped_to_bool_is_normalised() {
    let descriptor = BLOB_DESCRIPTOR.replace(
        "name = \"ready\"\ncausality = \"output\"\ntype = \"u8\"",
        "name = \"ready\"\ncausality = \"output\"\ntype = \"bool\"",
    );
    assert_ne!(descriptor, BLOB_DESCRIPTOR);
    let model = RawModel::load(&variant("blob_model", &descriptor)).unwrap();
    let (spec, mut io) = blob_spec("flagged", 2, "x");
    io.outputs[6] = Buffer::Bool(vec![false]);
    let mut inst = model.instantiate(&spec).unwrap();
    inst.init(0.0, &mut io).unwrap();
    assert_eq!(io.outputs[6], Buffer::Bool(vec![true]));
    inst.step(0.1, &mut io).unwrap();
    assert_eq!(io.outputs[6], Buffer::Bool(vec![true]));
}

#[test]
fn a_reported_size_other_than_the_bound_one_fails_init() {
    let model = RawModel::load(&package("blob_model")).unwrap();
    let (spec, mut io) = blob_spec("wide", 3, "x");
    let mut inst = model.instantiate(&spec).unwrap();
    let err = inst.init(0.0, &mut io).unwrap_err();
    match err {
        ModelError::Instantiate(ref m) => {
            assert!(
                m.contains("dim_nu = 2") && m.contains("bound to 3") && m.contains("nu"),
                "{m}"
            );
        }
        other => panic!("{other}"),
    }
}

/// The library reports nu = 2 while 1 is bound: it writes 2 x 3 gains before the check runs.
/// The buffers are allocated at the dimension's max, so that write stays inside them; the
/// mismatch then fails init. (Checked by construction and under debug assertions; no sanitizer
/// run.)
#[test]
fn a_reported_size_larger_than_bound_fails_init_inside_the_max_sized_buffers() {
    let model = RawModel::load(&package("blob_model")).unwrap();
    let (spec, mut io) = blob_spec("narrow", 1, "x");
    let mut inst = model.instantiate(&spec).unwrap();
    let err = inst.init(0.0, &mut io).unwrap_err();
    match err {
        ModelError::Instantiate(ref m) => {
            assert!(m.contains("dim_nu = 2") && m.contains("bound to 1"), "{m}");
        }
        other => panic!("{other}"),
    }
    // The heap is intact: a further instance at the reported size runs.
    let (spec, mut io) = blob_spec("right", 2, "x");
    let mut inst = model.instantiate(&spec).unwrap();
    inst.init(0.0, &mut io).unwrap();
    assert_close(&as_f64s(&io.outputs[3]), &[1.0, 1.1, 2.0, 2.1, 3.0, 3.1]);

    // A reported dimension without a max is refused at load.
    let dir = variant(
        "blob_model",
        &BLOB_DESCRIPTOR.replace("name = \"nu\"\nmin = 1\nmax = 8", "name = \"nu\"\nmin = 1"),
    );
    let err = RawModel::load(&dir).unwrap_err();
    assert!(
        matches!(err, ModelError::Load(ref m) if m.contains("needs `max`") && m.contains("dim_nu")),
        "{err}"
    );
}

#[test]
fn an_unknown_id_fails_the_call_with_its_code() {
    let model = RawModel::load(&variant(
        "blob_model",
        &BLOB_DESCRIPTOR.replace("const = 7", "const = 8"),
    ))
    .unwrap();
    let (spec, mut io) = blob_spec("stranger", 2, "x");
    let mut inst = model.instantiate(&spec).unwrap();
    match inst.init(0.0, &mut io).unwrap_err() {
        ModelError::Call { call, code, .. } => {
            assert_eq!(call, "init");
            assert_eq!(code, -3);
        }
        other => panic!("{other}"),
    }
}

#[test]
fn a_step_value_on_init_is_refused_by_the_library() {
    // Without the phase the library is never initialised: its sizes stay zero.
    let model = RawModel::load(&variant(
        "blob_model",
        &BLOB_DESCRIPTOR.replace("phase = { init = 1, step = 0 }", "const = 0"),
    ))
    .unwrap();
    let (spec, mut io) = blob_spec("never", 2, "x");
    let mut inst = model.instantiate(&spec).unwrap();
    match inst.init(0.0, &mut io).unwrap_err() {
        ModelError::Call { code, .. } => assert_eq!(code, -4),
        other => panic!("{other}"),
    }
}

#[test]
fn the_header_import_proposes_the_single_entry_descriptor() {
    let options = ImportOptions {
        entry: Some("blob_call".to_owned()),
        arg_structs: vec![
            ("in".to_owned(), "blob_input".to_owned()),
            ("out".to_owned(), "blob_output".to_owned()),
        ],
    };
    let proposal =
        import_header_with(Path::new(&format!("{FIXTURES}/blob_model.h")), &options).unwrap();
    let d = &proposal.descriptor;
    assert!(!d.abi.confirmed);
    assert_eq!(d.abi.init, d.abi.step);
    assert_eq!(d.abi.step.symbol, "blob_call");
    assert!(d.abi.terminate.is_none());
    let args = &d.abi.step.args;
    assert!(args[0].const_.is_some(), "{args:?}");
    assert_eq!(args[1].struct_.as_deref(), Some("blob_input"));
    assert_eq!(args[2].struct_.as_deref(), Some("blob_output"));
    let input = &d.abi.structs["blob_input"].members;
    assert!(input[0].phase.is_some(), "init_flag: {:?}", input[0]);
    let output = &d.abi.structs["blob_output"].members;
    for (member, dim) in [("dim_nu", "nu"), ("dim_ny", "ny"), ("dim_np", "np")] {
        let m = output.iter().find(|m| m.name == member).unwrap();
        assert!(m.reported, "{member}");
        assert_eq!(m.dim.as_deref(), Some(dim));
    }
    let dims: Vec<&str> = d
        .interface
        .dimensions
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(dims, vec!["np", "nu", "ny"]);
    let u = d
        .interface
        .variables
        .iter()
        .find(|v| v.name == "u")
        .unwrap();
    assert_eq!(
        u.shape,
        vec![taktwerk_core::value::Dim::Symbol("nu".to_owned())]
    );
    assert!(
        proposal.notes.iter().any(|n| n.contains("TODO")),
        "{:?}",
        proposal.notes
    );
    // The proposal round-trips, stays refused until confirmed, and validates once confirmed.
    let text = proposal.to_toml().unwrap();
    let mut back = Descriptor::parse(&text).unwrap();
    assert_eq!(&back, d);
    assert!(back.validate().is_err());
    back.abi.confirmed = true;
    let err = back.validate().unwrap_err();
    assert!(
        err.0.contains("needs `max`"),
        "reported dimensions need a max: {err}"
    );
    assert!(
        proposal.notes.iter().any(|n| n.contains("max")),
        "{:?}",
        proposal.notes
    );
    for d in &mut back.interface.dimensions {
        d.max = Some(8);
    }
    back.validate().unwrap();

    // Options naming what the header lacks are refused.
    let bad = ImportOptions {
        entry: Some("blob_call".to_owned()),
        arg_structs: vec![("in".to_owned(), "no_such".to_owned())],
    };
    assert!(import_header_with(Path::new(&format!("{FIXTURES}/blob_model.h")), &bad).is_err());
}
