//! Conformal copper through `sim.sparams` (DECISIONS D73, D87, D89): the option and what it
//! reports, the mesh refined for a narrow diagonal track, and a routed 45° line with the straight
//! line of the same length and the staircase on the same mesh alongside.
//!
//! The validation that prints the table of docs/ELECTRICAL.md is `#[ignore]`:
//! `cargo test --release --test fdtd_conformal -- --ignored --nocapture`.

use std::f64::consts::SQRT_2;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::electrical::complex::C64;
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

/// A routed line: 8 mm, `run` in x and y at 45° (or the same length straight), 8 mm.
fn board(w: &str, h: &str, er: &str, run: f64, diag: bool, extra: Value) -> (cadlab::touchstone::Touchstone, Value) {
    let r = Registry::with_builtins();
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    let s_ = &mut s;
    exec(&r, s_, "project.new", json!({"path": dir.path().join("line"), "name": "line"}));
    exec(&r, s_, "board.setup", json!({"layers": 2}));
    exec(&r, s_, "board.dielectric", json!({"thickness": h, "er": er, "loss_tangent": "0"}));
    exec(&r, s_, "board.outline", json!({"width": "60mm", "height": "40mm"}));
    exec(
        &r,
        s_,
        "footprint.create",
        json!({"name": "TP2", "pads": [
            {"number": "1", "at": ["0mm", "0mm"], "size": ["1mm", "1mm"], "shape": "rect"},
            {"number": "2", "at": ["2mm", "0mm"], "size": ["1mm", "1mm"], "shape": "rect"}]}),
    );
    exec(
        &r,
        s_,
        "part.create",
        json!({"id": "TP", "category": "connector", "footprint": "TP2",
            "pins": [{"number": "1", "name": "SIG"}, {"number": "2", "name": "GND"}]}),
    );
    exec(&r, s_, "circuit.add", json!({"part": "TP", "count": 1}));
    exec(&r, s_, "net.connect", json!({"net": "RF", "pins": ["J1.1"]}));
    exec(&r, s_, "net.connect", json!({"net": "GND", "pins": ["J1.2"]}));
    exec(&r, s_, "place.set", json!({"refdes": "J1", "at": ["2mm", "37mm"]}));
    let mm = |v: f64| format!("{v:.3}mm");
    let pts: Vec<Value> = if diag {
        vec![
            json!(["8mm", "10mm"]),
            json!(["16mm", "10mm"]),
            json!([mm(16.0 + run), mm(10.0 + run)]),
            json!([mm(24.0 + run), mm(10.0 + run)]),
        ]
    } else {
        let l = run * SQRT_2;
        vec![json!(["8mm", "10mm"]), json!([mm(16.0 + l), "10mm"]), json!([mm(24.0 + l), "10mm"])]
    };
    let o = exec(&r, s_, "track.add", json!({"layer": "F.Cu", "points": pts, "width": w, "net": "RF"}));
    let tr = o["output"]["tracks"].as_array().unwrap();
    let (a, b) = (tr[0]["id"].as_u64().unwrap(), tr[tr.len() - 1]["id"].as_u64().unwrap());
    exec(&r, s_, "via.add", json!({"at": ["2mm", "30mm"], "net": "GND", "drill": "0.3mm", "diameter": "0.6mm"}));
    exec(&r, s_, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["B.Cu"], "outline": "board"}));
    let out = dir.path().join("out");
    let mut args = json!({"dir": out, "name": "line", "around": [format!("track#{a}"), format!("track#{b}")],
        "copper": "sheet", "f_max": "10GHz", "perfect_copper": true, "points": 41, "margin": "4mm", "air": "5mm",
        "pml_cells": 6,
        "ports": [{"at": format!("track#{a}"), "kind": "msl", "length": "4mm"},
                  {"at": format!("track#{b}"), "end": "end", "kind": "msl", "length": "4mm"}]});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    let o = exec(&r, s_, "sim.sparams", args);
    let ts = cadlab::touchstone::parse(&std::fs::read_to_string(out.join("line.s2p")).unwrap(), Some(2)).unwrap();
    (ts, o)
}

fn codes(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

/// The option and what it reports (few timesteps: this checks the plumbing, not the result).
#[test]
fn sim_sparams_conformal_option() {
    let few = |cut: bool| json!({"conformal": cut, "timesteps": 40, "max_cell": "0.5mm", "points": 5, "margin": "2mm", "air": "2mm"});
    let (_, on) = board("1mm", "0.5mm", "4", 2.0, true, few(true));
    let (_, off) = board("1mm", "0.5mm", "4", 2.0, true, few(false));
    assert!(on["output"]["conformal_edges"].as_u64().unwrap() > 0, "{}", on["output"]);
    assert!(off["output"].get("conformal_edges").is_none());
}

/// A trace narrower than the cells is an open circuit on any scheme: the mesh is refined for it
/// unless `max_cell` is given, which warns instead.
#[test]
fn narrow_diagonal_track_refines_the_mesh() {
    let few = json!({"timesteps": 10, "points": 3, "margin": "1mm", "air": "1mm"});
    let (_, o) = board("0.3mm", "0.17mm", "4.3", 2.0, true, few);
    assert!(codes(&o).iter().any(|c| c == "sim.mesh_refined"), "{:?}", codes(&o));
    let cell: cadlab::units::Nm = serde_json::from_value(o["output"]["mesh"]["max_cell"].clone()).unwrap();
    assert!(cell.0 <= 160_000, "{}", cell.0);
    let fixed = json!({"timesteps": 10, "points": 3, "margin": "1mm", "air": "1mm", "max_cell": "0.7mm"});
    let (_, o) = board("0.3mm", "0.17mm", "4.3", 2.0, true, fixed);
    assert!(codes(&o).iter().any(|c| c == "sim.slanted_copper_unresolved"), "{:?}", codes(&o));
}

/// Board level, through the command, the table of docs/ELECTRICAL.md: |S11| and the phase of S21
/// at the default mesh and finer, staircase and conformal, the straight line of the same length
/// alongside (the bends' own effect aside).
#[test]
#[ignore]
fn routed_45_degree_line() {
    let cases = [
        ("1mm", "0.5mm", "4", None, false, false),
        ("1mm", "0.5mm", "4", None, true, false),
        ("1mm", "0.5mm", "4", None, true, true),
        ("1mm", "0.5mm", "4", Some("0.25mm"), true, false),
        ("1mm", "0.5mm", "4", Some("0.25mm"), true, true),
        ("0.3mm", "0.17mm", "4.3", None, true, true),
    ];
    for (w, h, er, cell, diag, conf) in cases {
        let extra = match cell {
            Some(c) => json!({"max_cell": c, "conformal": conf}),
            None => json!({"conformal": conf}),
        };
        let t = std::time::Instant::now();
        let (ts, o) = board(w, h, er, 10.0, diag, extra);
        print!("W {w} diag {diag} conformal {conf} cell {cell:?}: {} cells:", o["output"]["mesh"]["cells"]);
        for fi in [12, 20, 40] {
            let s = &ts.data[fi];
            let (s11, s21) = (C64::new(s[0].re, s[0].im), C64::new(s[2].re, s[2].im));
            print!(
                "  {:.1}G S11 {:.1} dB S21 {:.2} dB {:.1}°",
                ts.freqs[fi] / 1e9,
                20.0 * s11.abs().log10(),
                20.0 * s21.abs().log10(),
                s21.im.atan2(s21.re).to_degrees()
            );
        }
        println!(" [{:.0} s]", t.elapsed().as_secs_f64());
        if diag && conf {
            let s21 = &ts.data[20][2];
            assert!(C64::new(s21.re, s21.im).abs() > 0.9, "{w} {cell:?}: S21 {s21:?}");
        }
    }
}
