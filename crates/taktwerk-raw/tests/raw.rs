//! The adapter against two C fixtures compiled at test time with the system `cc`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail loudly"
)]

mod common;

use std::path::Path;
use std::process::Command;

use common::*;
use taktwerk_core::model::{ModelAdapter, ModelError};
use taktwerk_core::value::Buffer;
use taktwerk_raw::{Descriptor, ImportOptions, RawModel, import_header, import_header_with};

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
        ..ImportOptions::default()
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
        ..ImportOptions::default()
    };
    assert!(import_header_with(Path::new(&format!("{FIXTURES}/blob_model.h")), &bad).is_err());
}

// ==========================================================================
// The recommended shape: `examples/raw-filter-bank`, imported without review.
// ==========================================================================

const FILTER_BANK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/raw-filter-bank"
);

#[test]
fn a_header_in_the_recommended_shape_imports_confirmed_loads_and_steps() {
    let proposal = import_header(Path::new(&format!("{FILTER_BANK}/lowpass.h"))).unwrap();
    assert!(proposal.from_shape, "{:?}", proposal.notes);
    assert!(proposal.descriptor.abi.confirmed);
    let text = proposal.to_toml().unwrap();
    assert_eq!(
        text,
        std::fs::read_to_string(format!("{FILTER_BANK}/taktwerk-model.toml")).unwrap(),
        "the example's descriptor is the import of its header"
    );

    // Package: the imported descriptor and the example's library, built here.
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib").join(taktwerk_raw::arch_dir());
    std::fs::create_dir_all(&lib).unwrap();
    cc(&[
        "-shared",
        "-fPIC",
        "-O2",
        "-o",
        lib.join("liblowpass.so").to_str().unwrap(),
        &format!("{FILTER_BANK}/lowpass.c"),
    ]);
    std::fs::write(
        dir.path().join(taktwerk_raw::descriptor::DESCRIPTOR_FILE),
        &text,
    )
    .unwrap();
    let model = RawModel::load(dir.path()).unwrap();

    let (n, dt) = (3_usize, 0.01);
    let tau = [0.1, 0.5, 1.0];
    let y0 = [0.5, 0.0, -1.0];
    let spec = taktwerk_core::model::InstanceSpec {
        id: "filt".to_owned(),
        dims: std::collections::BTreeMap::from([("n".to_owned(), n)]),
        params: std::collections::BTreeMap::from([
            ("tau".to_owned(), f64s(&tau)),
            ("y0".to_owned(), f64s(&y0)),
            ("gain".to_owned(), f64s(&[2.0])),
        ]),
        step_size: dt,
    };
    let mut io = taktwerk_core::model::StepIo {
        inputs: vec![f64s(&[0.0; 3])],
        outputs: vec![f64s(&[0.0; 3])],
        tunables: vec![f64s(&[2.0])],
        tunables_changed: false,
    };
    let mut inst = model.instantiate(&spec).unwrap();
    inst.init(0.0, &mut io).unwrap();
    let alpha: Vec<f64> = tau.iter().map(|t| dt / (t + dt)).collect();
    let mut x = y0.to_vec();
    let mut gain = 2.0;
    for k in 0..50 {
        if k == 25 {
            gain = -0.5;
            io.tunables[0] = f64s(&[gain]);
            io.tunables_changed = true;
        }
        let u = [1.0, (k as f64 * 0.2).sin(), -2.0];
        io.inputs[0] = f64s(&u);
        inst.step(k as f64 * dt, &mut io).unwrap();
        io.tunables_changed = false;
        for i in 0..n {
            x[i] += alpha[i] * (u[i] - x[i]);
        }
        let want: Vec<f64> = x.iter().map(|x| gain * x).collect();
        assert_close(&as_f64s(&io.outputs[0]), &want);
    }
    inst.terminate();
}
