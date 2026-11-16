//! Board parts as lumped R, L, C elements through `sim.sparams` (`cadlab::fdtd`, D65), against
//! circuit theory: a series element between two 50 Ω ports has S21 = 2 Z₀ / (2 Z₀ + Z). (The
//! solver's elements themselves, series and shunt on a strip in a closed box, are
//! `lumped_fixture_matches_circuit_theory` in `tests/fdtd_ems.rs`.)
//!
//! The validations on a finer mesh are `#[ignore]`: `cargo test --release --test fdtd_parts --
//! --ignored`.

use std::f64::consts::PI;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::electrical::complex::C64;
use serde_json::{Value, json};

fn c(re: f64) -> C64 {
    C64::new(re, 0.0)
}

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
    assert_eq!(
        x["parts"],
        json!([{"refdes": "U1", "pads": ["U1.1", "U1.2"], "resistance": "50Ω", "columns": 3}]),
        "{x}"
    );
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

/// A track's round end passes the pad's inner edge by 50 µm here, so cut cells sit next to the gap.
/// The element keeps the columns it has without them: with only those at the end's tip, the
/// current crowding into them made a 1 pF part's |S21| 0.02 too high (DECISIONS D73).
#[test]
fn cut_cells_do_not_change_a_parts_columns() {
    let r = Registry::with_builtins();
    let columns = |cut: bool, cell: &str| {
        let (dir, mut s, t) = board(&r, "capacitor", json!({"capacitance": "1pF"}), 4);
        let out = dir.path().join("out");
        let a = json!({"f_max": "6GHz", "f_min": "0.5GHz", "points": 3, "margin": "3mm", "air": "3mm",
            "max_cell": cell, "pml_cells": 6, "conformal": cut, "timesteps": 2});
        let o = exec(&r, &mut s, "sim.sparams", args(&out, t, a));
        assert_eq!(o["output"]["conformal_edges"].as_u64().is_some(), cut, "{}", o["output"]);
        o["output"]["parts"][0]["columns"].as_u64().unwrap()
    };
    for (cell, want) in [("0.5mm", 3), ("0.25mm", 5)] {
        assert_eq!(columns(false, cell), want, "{cell}");
        assert_eq!(columns(true, cell), want, "{cell}");
    }
}
