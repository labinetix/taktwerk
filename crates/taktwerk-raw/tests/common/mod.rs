//! Fixtures shared by the raw adapter and the FMU wrapper tests: three C models compiled at
//! test time with the system `cc`, their confirmed descriptors, and reference computations.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; each test binary uses a subset"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use taktwerk_core::model::{InstanceSpec, ModelAdapter, ModelError, StepIo};
use taktwerk_core::value::Buffer;
use taktwerk_raw::RawModel;
use taktwerk_raw::arch_dir;
use taktwerk_raw::descriptor::DESCRIPTOR_FILE;

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// The descriptor documented in the crate (`ss.h`), confirmed.
pub const SS_DESCRIPTOR: &str = r#"
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
pub const PI_DESCRIPTOR: &str = r#"
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
pub const BLOB_DESCRIPTOR: &str = r#"
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
pub struct Built {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

pub fn built() -> &'static Built {
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

pub fn cc(args: &[&str]) {
    let status = Command::new("cc")
        .args(args)
        .current_dir(FIXTURES)
        .status()
        .expect("cc on PATH");
    assert!(status.success(), "cc {args:?}");
}

pub fn package(name: &str) -> PathBuf {
    built().root.join(name)
}

/// A package directory with a different descriptor but the same library.
pub fn variant(name: &str, descriptor: &str) -> PathBuf {
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

pub fn f64s(values: &[f64]) -> Buffer {
    Buffer::F64(values.to_vec())
}

/// The error of a refused instantiation.
pub fn refused(model: &RawModel, spec: &InstanceSpec) -> ModelError {
    match model.instantiate(spec) {
        Ok(_) => panic!("{}: instantiated", spec.id),
        Err(e) => e,
    }
}

/// Equal up to the rounding the C compiler's fused multiply-adds introduce.
pub fn assert_close(got: &[f64], want: &[f64]) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 1e-12, "{got:?} vs {want:?}");
    }
}

pub fn as_f64s(buffer: &Buffer) -> Vec<f64> {
    match buffer {
        Buffer::F64(v) => v.clone(),
        other => panic!("not f64: {other:?}"),
    }
}

/// Plant parameters for size `nx`: a shift register with feedback, so every state moves.
pub struct Ss {
    pub nx: usize,
    pub a: Vec<f64>,
    pub b: Vec<f64>,
    pub c: Vec<f64>,
    pub k: f64,
    pub x: Vec<f64>,
}

impl Ss {
    pub fn new(nx: usize, k: f64) -> Self {
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

    pub fn step(&mut self, u: f64) -> f64 {
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

    pub fn spec(&self, id: &str) -> InstanceSpec {
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

    pub fn io(&self) -> StepIo {
        StepIo {
            inputs: vec![f64s(&[0.0])],
            outputs: vec![f64s(&vec![0.0; self.nx]), f64s(&[0.0])],
            tunables: vec![f64s(&[self.k])],
            tunables_changed: false,
        }
    }
}

pub struct Pi {
    pub n: usize,
    pub kp: Vec<f64>,
    pub ki: f64,
    pub dt: f64,
    pub integral: Vec<f64>,
}

impl Pi {
    pub fn step(&mut self, sp: &[f64], pv: &[f64]) -> Vec<f64> {
        (0..self.n)
            .map(|i| {
                let e = sp[i] - pv[i];
                self.integral[i] += self.ki * e * self.dt;
                self.kp[i] * e + self.integral[i]
            })
            .collect()
    }
}

pub fn pi_spec(id: &str, n: usize, ki: f64) -> (InstanceSpec, Pi, StepIo) {
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

pub fn bytes(text: &str, capacity: usize) -> Buffer {
    let mut v = text.as_bytes().to_vec();
    v.resize(capacity, 0);
    Buffer::U8(v)
}

/// The text in a NUL-terminated byte buffer.
pub fn text(buffer: &Buffer) -> String {
    match buffer {
        Buffer::U8(v) => {
            let end = v.iter().position(|b| *b == 0).unwrap_or(v.len());
            String::from_utf8(v[..end].to_vec()).unwrap()
        }
        other => panic!("not u8: {other:?}"),
    }
}

/// Variant 1 of the fixture reports nu = 2, ny = 3, np = 4.
pub fn blob_spec(id: &str, nu: usize, title: &str) -> (InstanceSpec, StepIo) {
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
