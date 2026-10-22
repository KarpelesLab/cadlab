//! Lumped R, L, C elements in the FDTD solver (`cadlab::fdtd`, D65) against circuit theory: a
//! series element between two 50 Ω ports has S21 = 2 Z₀ / (2 Z₀ + Z), a shunt one
//! S21 = 2 / (2 + Y Z₀); and board parts through `sim.sparams`.
//!
//! The engine fixtures are a short, wide strip 0.2 mm over the ground of a closed box (a few
//! thousand cells), small against the wavelength, so its own reactances stay small at a few GHz.
//! The board-level validations are `#[ignore]`: `cargo test --release --test fdtd_parts --
//! --ignored`.

use std::f64::consts::PI;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::electrical::complex::C64;
use cadlab::fdtd::engine::{Gaussian, Hooks, LumpedElement, LumpedPort, Settings, Simulation, Structure};
use serde_json::{Value, json};

const UM: i64 = 1_000;

fn c(re: f64) -> C64 {
    C64::new(re, 0.0)
}

fn uniform(cells: i64, d: i64) -> Vec<i64> {
    (0..=cells).map(|i| i * d).collect()
}

/// Where the fixture's element goes.
enum Place {
    /// Across a two-cell gap in the strip.
    Series,
    /// From the strip's middle down to the ground.
    Shunt,
}

const FREQS: [f64; 8] = [0.5e9, 1e9, 1.5e9, 2e9, 2.5e9, 3e9, 3.5e9, 4e9];

/// A strip 0.4 mm wide, 0.2 mm over the bottom wall of a closed 3 × 1 × 0.5 mm box (0.1 mm
/// cells), between 50 Ω lumped ports at x = 0.5 and 2.5 mm, with `el` (R, L, C) placed in
/// series or shunt; `None`: the plain strip. Returns (S11, S21) per frequency of [`FREQS`].
fn fixture(place: Place, el: Option<(f64, f64, Option<f64>)>) -> Vec<(f64, C64, C64)> {
    let d = 100 * UM;
    let mut st = Structure::new([uniform(30, d), uniform(10, d), uniform(5, d)]).unwrap();
    let (y0, y1, z) = (3 * d, 7 * d, 2 * d);
    let all = |_: i64, _: i64| true;
    match (&place, el.is_some()) {
        (Place::Series, true) => {
            st.pec([5 * d, y0, z], [14 * d, y1, z], &all);
            st.pec([16 * d, y0, z], [25 * d, y1, z], &all);
        }
        _ => {
            st.pec([5 * d, y0, z], [25 * d, y1, z], &all);
        }
    }
    if let Some((r, l, cap)) = el {
        let (lo, hi) = match place {
            Place::Series => ([14, 3, 2], [16, 7, 2]),
            Place::Shunt => ([15, 3, 0], [15, 7, 2]),
        };
        let axis = match place {
            Place::Series => 0,
            Place::Shunt => 2,
        };
        st.element(LumpedElement { axis, lo, hi, r, l, c: cap });
    }
    let port = |i: usize| LumpedPort { axis: 2, lo: [i, 3, 0], hi: [i, 7, 2], positive_high: true, r: 50.0 };
    let settings = Settings {
        pml: [0; 6],
        courant: 0.95,
        max_steps: 200_000,
        end_db: 60.0,
        excitation: Gaussian { f0: 2.5e9, fc: 2.5e9 },
        freqs: FREQS.to_vec(),
        threads: 0,
        surface: None,
    };
    let mut sim = Simulation::new(&st, &[port(5), port(25)], Vec::new(), settings).unwrap();
    let run = sim.run(Some(0), &Hooks::none()).unwrap();
    assert!(run.converged, "{} steps, {} dB", run.steps, run.energy_db);
    let k = 1.0 / (2.0 * 50f64.sqrt());
    let wave = |p: usize, f: usize, sign: f64| (run.port_v[p][f] + run.port_i[p][f] * c(50.0 * sign)) * c(k);
    FREQS
        .iter()
        .enumerate()
        .map(|(f, &hz)| {
            let a1 = wave(0, f, 1.0);
            (hz, wave(0, f, -1.0) / a1, wave(1, f, -1.0) / a1)
        })
        .collect()
}

fn z_series(f: f64, r: f64, l: f64, cap: Option<f64>) -> C64 {
    let w = 2.0 * PI * f;
    C64::new(r, w * l - cap.map_or(0.0, |cc| 1.0 / (w * cc)))
}

/// The plain strip is nearly transparent: its own reactances are what the element tests
/// tolerate.
#[test]
fn fixture_is_transparent() {
    for (f, s11, s21) in fixture(Place::Series, None) {
        assert!(s11.abs() < 0.12, "{f}: |S11| {}", s11.abs());
        assert!((s21.abs() - 1.0).abs() < 0.01, "{f}: |S21| {}", s21.abs());
    }
}

/// A series resistor: S21 = 2 Z₀ / (2 Z₀ + R), S11 = R / (2 Z₀ + R), real and flat.
#[test]
fn series_resistor() {
    for r in [10.0, 50.0, 200.0] {
        for (f, s11, s21) in fixture(Place::Series, Some((r, 0.0, None))) {
            let (w21, w11) = (100.0 / (100.0 + r), r / (100.0 + r));
            println!("R {r}: {f}: S21 {s21:?} vs {w21:.4}, S11 {s11:?} vs {w11:.4}");
            // Magnitudes: the strip's own delay turns the phase.
            assert!((s21.abs() - w21).abs() < 0.003, "R {r}, {f}: S21 {s21:?} vs {w21}");
            assert!((s11.abs() - w11).abs() < 0.015, "R {r}, {f}: S11 {s11:?} vs {w11}");
        }
    }
}

/// A series capacitor (DC block, a high-pass) and a series inductor (a low-pass), and series
/// R–L–C (a band-pass at 1/(2π√(LC))): S21 = 2 Z₀ / (2 Z₀ + Z).
#[test]
fn series_reactive() {
    let cases = [(0.0, 0.0, Some(1e-12)), (0.0, 5e-9, None), (10.0, 4e-9, Some(1e-12))];
    for (r, l, cap) in cases {
        for (f, _, s21) in fixture(Place::Series, Some((r, l, cap))) {
            let want = c(100.0) / (c(100.0) + z_series(f, r, l, cap));
            println!("R {r} L {l} C {cap:?}: {f}: |S21| {:.4} vs {:.4}", s21.abs(), want.abs());
            assert!((s21.abs() - want.abs()).abs() < 0.02, "{f}: |S21| {} vs {}", s21.abs(), want.abs());
        }
    }
}

/// A shunt capacitor: the low-pass S21 = 2 / (2 + jωC Z₀); and a shunt series R–L–C, a notch
/// down to 2R / (2R + Z₀) at resonance.
#[test]
fn shunt_elements() {
    for (f, _, s21) in fixture(Place::Shunt, Some((0.0, 0.0, Some(2e-12)))) {
        let want = c(2.0) / (c(2.0) + C64::new(0.0, 2.0 * PI * f * 2e-12 * 50.0));
        println!("shunt C: {f}: |S21| {:.4} vs {:.4}", s21.abs(), want.abs());
        assert!((s21.abs() - want.abs()).abs() < 0.02, "{f}: |S21| {} vs {}", s21.abs(), want.abs());
    }
    // 2 nH and 2 pF: 2.52 GHz.
    let (r, l, cap) = (5.0, 2e-9, Some(2e-12));
    for (f, _, s21) in fixture(Place::Shunt, Some((r, l, cap))) {
        let z = z_series(f, r, l, cap);
        let want = c(2.0) * z / (c(2.0) * z + c(50.0));
        println!("shunt RLC: {f}: |S21| {:.4} vs {:.4}", s21.abs(), want.abs());
        assert!((s21.abs() - want.abs()).abs() < 0.03, "{f}: |S21| {} vs {}", s21.abs(), want.abs());
    }
}

/// A 0 Ω element is a short: perfect-conductor edges, no element state.
#[test]
fn zero_ohm_is_a_short() {
    let mut st = Structure::new([uniform(4, 1000), uniform(4, 1000), uniform(4, 1000)]).unwrap();
    st.element(LumpedElement { axis: 0, lo: [1, 2, 2], hi: [3, 2, 2], r: 0.0, l: 0.0, c: None });
    assert!(st.elements().is_empty());
    assert!(st.is_pec(0, [1, 2, 2]) && st.is_pec(0, [2, 2, 2]) && !st.is_pec(0, [3, 2, 2]));
    let mut st = Structure::new([uniform(4, 1000), uniform(4, 1000), uniform(4, 1000)]).unwrap();
    st.element(LumpedElement { axis: 0, lo: [1, 2, 2], hi: [3, 2, 2], r: -1.0, l: 0.0, c: None });
    let err = Simulation::new(&st, &[], Vec::new(), Settings { pml: [0; 6], ..Settings::default() }).err().unwrap();
    assert!(err.contains("non-negative"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// Board level: `sim.sparams` with parts on the line.

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn codes_of(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

fn c64(v: &cadlab::touchstone::Complex) -> C64 {
    C64::new(v.re, v.im)
}

/// A two-layer board, 0.5 mm of εr 4 (lossless) over a ground pour: a 1 mm (50 Ω) microstrip
/// on F.Cu from x = 5 mm, `len` mm long on each side of a 0603 part `U1` (pads 0.9 × 1 mm at
/// ±0.8 mm) of category `category` with `params`. Returns the project directory, the session
/// and the two tracks' IDs.
fn board(r: &Registry, category: &str, params: Value, len: i64) -> (tempfile::TempDir, Session, [u64; 2]) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    let s_ = &mut s;
    let mm = |v: f64| format!("{v}mm");
    exec(r, s_, "project.new", json!({"path": dir.path().join("p"), "name": "p"}));
    exec(r, s_, "board.setup", json!({"layers": 2}));
    exec(r, s_, "board.dielectric", json!({"thickness": "0.5mm", "er": "4", "loss_tangent": "0"}));
    let width = 2 * len + 12;
    exec(r, s_, "board.outline", json!({"width": mm(width as f64), "height": "20mm"}));
    exec(
        r,
        s_,
        "footprint.create",
        json!({"name": "P0603", "pads": [
            {"number": "1", "at": ["-0.8mm", "0mm"], "size": ["0.9mm", "1mm"], "shape": "rect"},
            {"number": "2", "at": ["0.8mm", "0mm"], "size": ["0.9mm", "1mm"], "shape": "rect"}]}),
    );
    exec(
        r,
        s_,
        "footprint.create",
        json!({"name": "TP1", "pads": [
        {"number": "1", "at": ["0mm", "0mm"], "size": ["1mm", "1mm"], "shape": "rect"}]}),
    );
    exec(
        r,
        s_,
        "part.create",
        json!({"id": "DUT", "category": category, "footprint": "P0603", "params": params,
            "pins": [{"number": "1", "name": "1"}, {"number": "2", "name": "2"}]}),
    );
    exec(
        r,
        s_,
        "part.create",
        json!({"id": "TP", "category": "connector", "footprint": "TP1",
        "pins": [{"number": "1", "name": "1"}]}),
    );
    exec(r, s_, "circuit.add", json!({"part": "DUT", "refdes": "U1"}));
    exec(r, s_, "circuit.add", json!({"part": "TP", "count": 3}));
    exec(r, s_, "net.connect", json!({"net": "A", "pins": ["U1.1", "J1.1"]}));
    exec(r, s_, "net.connect", json!({"net": "B", "pins": ["U1.2", "J2.1"]}));
    exec(r, s_, "net.connect", json!({"net": "GND", "pins": ["J3.1"]}));
    let xc = 5 + len + 1; // the part's center (mm)
    exec(r, s_, "place.set", json!({"refdes": "U1", "at": [mm(xc as f64), "10mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J1", "at": ["2mm", "18mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J2", "at": [mm(width as f64 - 2.0), "18mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J3", "at": ["2mm", "2mm"]}));
    let track = |s_: &mut Session, a: f64, b: f64, net: &str| {
        let o = exec(
            r,
            s_,
            "track.add",
            json!({"layer": "F.Cu", "points": [[mm(a), "10mm"], [mm(b), "10mm"]], "width": "1mm", "net": net}),
        );
        o["output"]["tracks"][0]["id"].as_u64().unwrap()
    };
    let x = xc as f64;
    let t1 = track(s_, 5.0, x - 0.8, "A");
    let t2 = track(s_, x + 0.8, x + 2.0 + len as f64 - 1.0, "B");
    exec(r, s_, "via.add", json!({"at": ["2mm", "2mm"], "net": "GND", "drill": "0.3mm", "diameter": "0.6mm"}));
    exec(r, s_, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["B.Cu"], "outline": "board"}));
    (dir, s, [t1, t2])
}

fn args(dir: &std::path::Path, t: [u64; 2], extra: Value) -> Value {
    let mut a = json!({"dir": dir, "name": "dut", "around": [format!("track#{}", t[0]), format!("track#{}", t[1])],
        "copper": "sheet", "perfect_copper": true,
        "ports": [{"at": format!("track#{}", t[0]), "kind": "msl", "length": "3mm"},
                  {"at": format!("track#{}", t[1]), "end": "end", "kind": "msl", "length": "3mm"}]});
    for (k, v) in extra.as_object().unwrap() {
        a[k] = v.clone();
    }
    a
}

/// A coarse run: the resistor on the line is found and modeled (|S21| near 2 Z₀ / (2 Z₀ + R)
/// up to 4 GHz); `parts: false` leaves it out, and a part without a value is reported (those
/// two runs stop after a few timesteps: only their outputs are checked).
#[test]
fn sim_sparams_models_parts() {
    let r = Registry::with_builtins();
    let (dir, mut s, t) = board(&r, "resistor", json!({"resistance": "50"}), 4);
    let out = dir.path().join("out");
    let coarse = json!({"f_max": "8GHz", "points": 5, "margin": "2mm", "air": "2mm", "max_cell": "1mm",
        "pml_cells": 4, "end_criteria": "30dB"});
    let o = exec(&r, &mut s, "sim.sparams", args(&out, t, coarse.clone()));
    let x = &o["output"];
    assert_eq!(x["parts"], json!([{"refdes": "U1", "pads": ["U1.1", "U1.2"], "resistance": "50Ω"}]), "{x}");
    assert!(codes_of(&o).is_empty(), "{}", o["diagnostics"]);
    let ts = cadlab::touchstone::parse(&std::fs::read_to_string(out.join("dut.s2p")).unwrap(), Some(2)).unwrap();
    for (f, m) in ts.freqs.iter().zip(&ts.data) {
        let (s11, s21) = (c64(&m[0]).abs(), c64(&m[2]).abs());
        println!("{f}: |S21| {s21:.4}, |S11| {s11:.4} (2/3, 1/3 ideal)");
        if *f <= 4.1e9 {
            assert!((s21 - 2.0 / 3.0).abs() < 0.03, "{f}: |S21| {s21}");
            assert!((s11 - 1.0 / 3.0).abs() < 0.05, "{f}: |S11| {s11}");
        }
    }
    let short = |extra: Value| {
        let mut a = args(&out, t, coarse.clone());
        a["timesteps"] = json!(4);
        a["name"] = json!("short");
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        a
    };
    let o = exec(&r, &mut s, "sim.sparams", short(json!({"parts": false})));
    assert!(o["output"].get("parts").is_none(), "{}", o["output"]);
    assert!(!codes_of(&o).contains(&"sim.part_not_modeled".to_string()));
    exec(&r, &mut s, "part.set", json!({"id": "DUT", "params": {"resistance": null}}));
    let o = exec(&r, &mut s, "sim.sparams", short(json!({})));
    assert!(o["output"].get("parts").is_none(), "{}", o["output"]);
    assert!(codes_of(&o).contains(&"sim.part_not_modeled".to_string()), "{}", o["diagnostics"]);
}

/// Validation in release: a series resistor (S21 = 2 Z₀ / (2 Z₀ + R)), a series 1 pF DC block
/// (high-pass) and the same capacitor with an ESL (resonant at 1/(2π√(LC))) on the board's
/// microstrip, finer mesh, between the ports' reference planes. The pads' own reactance
/// (a 0603's gap, pad steps) is in the result.
#[test]
#[ignore]
fn sim_sparams_parts_validation() {
    let r = Registry::with_builtins();
    for (params, cat, zf) in [
        (json!({"resistance": "50"}), "resistor", Box::new(|_: f64| C64::new(50.0, 0.0)) as Box<dyn Fn(f64) -> C64>),
        (json!({"capacitance": "1pF"}), "capacitor", Box::new(|f: f64| C64::new(0.0, -1.0 / (2.0 * PI * f * 1e-12)))),
        (
            json!({"capacitance": "1pF", "esl": "1nH", "esr": "1"}),
            "capacitor",
            Box::new(|f: f64| C64::new(1.0, 2.0 * PI * f * 1e-9 - 1.0 / (2.0 * PI * f * 1e-12))),
        ),
    ] {
        let (dir, mut s, t) = board(&r, cat, params.clone(), 6);
        let out = dir.path().join("out");
        let fine = json!({"f_max": "6GHz", "f_min": "0.5GHz", "points": 12, "margin": "3mm", "air": "3mm",
            "max_cell": "0.5mm", "pml_cells": 6});
        let o = exec(&r, &mut s, "sim.sparams", args(&out, t, fine));
        assert!(o["output"]["runs"].as_array().unwrap().iter().all(|r| r["converged"] == true));
        let ts = cadlab::touchstone::parse(&std::fs::read_to_string(out.join("dut.s2p")).unwrap(), Some(2)).unwrap();
        for (f, m) in ts.freqs.iter().zip(&ts.data) {
            let s21 = c64(&m[2]).abs();
            let want = (c(100.0) / (c(100.0) + zf(*f))).abs();
            println!("{params}: {f}: |S21| {s21:.4} vs {want:.4}");
            let tol = 0.02;
            assert!((s21 - want).abs() < tol, "{params}: {f}: |S21| {s21} vs {want}");
        }
    }
}
