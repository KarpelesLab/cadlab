//! `sim.sparams --solver ems` (DECISIONS D86): the board region run by the ems crate's FDTD, against
//! the built-in solver and the references its own tests use. The `#[ignore]` tests are the
//! measurements behind the table in docs/ELECTRICAL.md ("ems as a second solver"); run them in
//! release, one at a time: `cargo test --release --test fdtd_ems -- --ignored --nocapture <name>`.
#![cfg(feature = "ems")]

use std::f64::consts::PI;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::electrical::Model;
use cadlab::electrical::complex::C64;
use cadlab::electrical::fieldsolver::{self, Accuracy};
use cadlab::fdtd::engine::C0;
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

fn fail(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> String {
    r.execute(s, cmd, args, RunOptions::default()).expect_err(cmd).error.diagnostic.code.to_string()
}

fn codes_of(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

/// A two-layer board: a straight microstrip on F.Cu (`w` wide, `len_mm` long, from (5, 12) mm)
/// over a GND pour on B.Cu, one dielectric `h` thick (as `tests/fdtd.rs`).
fn board_line(r: &Registry, h: &str, er: &str, tan: &str, w: &str, len_mm: i64) -> (tempfile::TempDir, Session, u64) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    let s_ = &mut s;
    exec(r, s_, "project.new", json!({"path": dir.path().join("line"), "name": "line"}));
    exec(r, s_, "board.setup", json!({"layers": 2}));
    exec(r, s_, "board.dielectric", json!({"thickness": h, "er": er, "loss_tangent": tan}));
    exec(r, s_, "board.outline", json!({"width": format!("{}mm", len_mm + 10), "height": "30mm"}));
    exec(
        r,
        s_,
        "footprint.create",
        json!({"name": "TP2", "pads": [
            {"number": "1", "at": ["0mm", "0mm"], "size": ["1mm", "1mm"], "shape": "rect"},
            {"number": "2", "at": ["2mm", "0mm"], "size": ["1mm", "1mm"], "shape": "rect"}]}),
    );
    exec(
        r,
        s_,
        "part.create",
        json!({"id": "TP", "category": "connector", "footprint": "TP2",
            "pins": [{"number": "1", "name": "SIG"}, {"number": "2", "name": "GND"}]}),
    );
    exec(r, s_, "circuit.add", json!({"part": "TP", "count": 2}));
    exec(r, s_, "net.connect", json!({"net": "RF", "pins": ["J1.1", "J2.1"]}));
    exec(r, s_, "net.connect", json!({"net": "GND", "pins": ["J1.2", "J2.2"]}));
    exec(r, s_, "place.set", json!({"refdes": "J1", "at": ["2mm", "27mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J2", "at": [format!("{}mm", len_mm + 5), "27mm"]}));
    let o = exec(
        r,
        s_,
        "track.add",
        json!({"layer": "F.Cu", "points": [["5mm", "12mm"], [format!("{}mm", len_mm + 5), "12mm"]], "width": w, "net": "RF"}),
    );
    let id = o["output"]["tracks"][0]["id"].as_u64().unwrap();
    exec(r, s_, "via.add", json!({"at": ["2mm", "22mm"], "net": "GND", "drill": "0.3mm", "diameter": "0.6mm"}));
    exec(r, s_, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["B.Cu"], "outline": "board"}));
    (dir, s, id)
}

fn c64(v: &cadlab::touchstone::Complex) -> C64 {
    C64::new(v.re, v.im)
}

fn db(v: &cadlab::touchstone::Complex) -> f64 {
    20.0 * c64(v).abs().log10()
}

/// ABCD of a 2-port from its S-parameters (reference `zr`), then the line's Z₀ = √(B/C) and
/// γl = acosh(A) (as `tests/fdtd.rs`).
fn line_from_s(s: &[cadlab::touchstone::Complex], zr: f64) -> (C64, C64) {
    let (s11, s12, s21, s22) = (c64(&s[0]), c64(&s[1]), c64(&s[2]), c64(&s[3]));
    let (one, two, z) = (c(1.0), c(2.0), c(zr));
    let a = ((one + s11) * (one - s22) + s12 * s21) / (two * s21);
    let b = z * ((one + s11) * (one + s22) - s12 * s21) / (two * s21);
    let cc = ((one - s11) * (one - s22) - s12 * s21) / (two * s21 * z);
    let mut z0 = (b / cc).sqrt();
    if z0.re < 0.0 {
        z0 = c(0.0) - z0;
    }
    let mut gl = (a + (a * a - one).sqrt()).ln();
    if gl.im < 0.0 {
        gl = c(0.0) - gl;
    }
    (z0, gl)
}

fn eps_eff(beta: f64, f: f64) -> f64 {
    (beta * C0 / (2.0 * PI * f)).powi(2)
}

fn msl_args(dir: &std::path::Path, id: u64, extra: Value) -> Value {
    let a = json!({"dir": dir, "name": "line", "around": [format!("track#{id}")], "copper": "sheet",
        "ports": [{"at": format!("track#{id}"), "kind": "msl", "length": "4mm"},
                  {"at": format!("track#{id}"), "end": "end", "kind": "msl", "length": "4mm"}]});
    with(a, extra)
}

fn with(mut a: Value, extra: Value) -> Value {
    for (k, v) in extra.as_object().unwrap() {
        a[k] = v.clone();
    }
    a
}

/// Resident memory of this process (kB).
fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()))
        .map_or(0, |pages| pages * 4)
}

/// One solver's run of a case: its S-parameters and what it cost.
struct Ran {
    ts: cadlab::touchstone::Touchstone,
    out: Value,
    seconds: f64,
    /// Peak resident memory above the level before the run (MB), sampled every 20 ms.
    mem_mb: f64,
}

impl Ran {
    fn steps(&self) -> Vec<u64> {
        self.out["output"]["runs"].as_array().unwrap().iter().map(|r| r["steps"].as_u64().unwrap()).collect()
    }
    fn cells(&self) -> u64 {
        self.out["output"]["mesh"]["cells"].as_u64().unwrap()
    }
    fn dt_ps(&self) -> f64 {
        self.out["output"]["timestep"].as_f64().unwrap() * 1e12
    }
    /// Million cell updates per second, all runs (setup time included).
    fn rate(&self) -> f64 {
        self.cells() as f64 * self.steps().iter().sum::<u64>() as f64 / self.seconds / 1e6
    }
}

/// Runs `sim.sparams` with `args` (which name the output file) on `solver`.
fn run(r: &Registry, s: &mut Session, args: &Value, solver: &str) -> Ran {
    let name = format!("{}_{solver}", args["name"].as_str().unwrap());
    let a = with(args.clone(), json!({"solver": solver, "name": name}));
    let base = rss_kb();
    let peak = Arc::new(AtomicU64::new(base));
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let (peak, stop) = (peak.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                peak.fetch_max(rss_kb(), Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })
    };
    let t = Instant::now();
    let out = exec(r, s, "sim.sparams", a);
    let seconds = t.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    sampler.join().unwrap();
    let mem_mb = (peak.load(Ordering::Relaxed) - base) as f64 / 1024.0;
    let file = out["output"]["touchstone"].as_str().unwrap().to_string();
    let n = out["output"]["ports"].as_array().unwrap().len();
    let ts = cadlab::touchstone::parse(&std::fs::read_to_string(file).unwrap(), Some(n)).unwrap();
    Ran { ts, out, seconds, mem_mb }
}

/// Both solvers on one case, with the cost line of the table and the largest differences of
/// |S11| and |S21| over `band` (Hz).
fn both(case: &str, r: &Registry, s: &mut Session, args: &Value, band: (f64, f64)) -> (Ran, Ran) {
    let b = run(r, s, args, "builtin");
    let e = run(r, s, args, "ems");
    let np = b.ts.ports;
    let (mut d11, mut d21, mut d21_db) = (0.0f64, 0.0f64, 0.0f64);
    for ((f, x), y) in b.ts.freqs.iter().zip(&b.ts.data).zip(&e.ts.data) {
        if *f < band.0 || *f > band.1 {
            continue;
        }
        d11 = d11.max((c64(&x[0]).abs() - c64(&y[0]).abs()).abs());
        if np > 1 {
            d21 = d21.max((c64(&x[np]).abs() - c64(&y[np]).abs()).abs());
            d21_db = d21_db.max((db(&x[np]) - db(&y[np])).abs());
        }
    }
    println!(
        "## {case}: {} cells; builtin dt {:.4} ps, steps {:?}, {:.1} s ({:.0} Mcells/s), {:.0} MB; ems dt {:.4} ps, \
         steps {:?}, {:.1} s ({:.0} Mcells/s), {:.0} MB; {:.2}-{:.2} GHz: max Δ|S11| {d11:.4}, Δ|S21| {d21:.4} ({d21_db:.3} dB)",
        b.cells(),
        b.dt_ps(),
        b.steps(),
        b.seconds,
        b.rate(),
        b.mem_mb,
        e.dt_ps(),
        e.steps(),
        e.seconds,
        e.rate(),
        e.mem_mb,
        band.0 / 1e9,
        band.1 / 1e9,
    );
    let notes: Vec<String> = e.out["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| format!("{}: {}", d["code"].as_str().unwrap(), d["message"].as_str().unwrap()))
        .collect();
    println!("ems diagnostics:\n  {}", notes.join("\n  "));
    (b, e)
}

fn field_solver(er: f64, h: f64, w: f64) -> fieldsolver::FieldLine {
    let s = fieldsolver::Structure {
        model: Model::Microstrip,
        h,
        er,
        h2: 0.0,
        er2: er,
        t: 0.0,
        etch: 0.0,
        broadside: None,
        coplanar_gap: None,
        mask: None,
    };
    s.line(w, Accuracy::default()).unwrap()
}

/// The coarse line of `tests/fdtd.rs`'s `sim_sparams_command` on ems: it passes, is reciprocal,
/// agrees with the built-in solver, and gives the same data again on any thread count; plan, lumped
/// ports, lossy copper with its notes, the cell guard.
#[test]
fn ems_runs_the_region() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = board_line(&r, "1mm", "4", "0.02", "2mm", 8);
    let out = dir.path().join("out");
    let args = msl_args(
        &out,
        id,
        json!({"f_max": "5GHz", "points": 11, "margin": "4mm", "air": "3mm", "max_cell": "1mm", "pml_cells": 4,
            "end_criteria": "30dB"}),
    );
    let e = run(&r, &mut s, &args, "ems");
    let b = run(&r, &mut s, &args, "builtin");
    assert!(e.out["output"]["runs"].as_array().unwrap().iter().all(|r| r["converged"] == true), "{}", e.out);
    assert_eq!(e.out["output"]["mesh"], b.out["output"]["mesh"]);
    assert_eq!(e.out["output"]["ports"], b.out["output"]["ports"]);
    for ((f, m), n) in e.ts.freqs.iter().zip(&e.ts.data).zip(&b.ts.data) {
        let (s11, s12, s21) = (c64(&m[0]), c64(&m[1]), c64(&m[2]));
        assert!(s11.abs() < 0.2, "{f}: |S11| {}", s11.abs());
        assert!(s21.abs() > 0.9 && s21.abs() < 1.0, "{f}: |S21| {}", s21.abs());
        assert!((s12 - s21).abs() < 0.01, "{f}: reciprocity {s12:?} {s21:?}");
        // Measured: |S21| within 0.0024, |S11| within 0.0043 of the built-in solver.
        assert!((s21.abs() - c64(&n[2]).abs()).abs() < 0.01, "{f}: |S21| {} vs {}", s21.abs(), c64(&n[2]).abs());
        assert!((s11.abs() - c64(&n[0]).abs()).abs() < 0.015, "{f}: |S11| {} vs {}", s11.abs(), c64(&n[0]).abs());
    }
    // The same run again, on one and on three threads: the same data.
    for threads in [1, 3] {
        let t = run(&r, &mut s, &with(args.clone(), json!({"threads": threads})), "ems");
        assert_eq!(t.ts.data, e.ts.data, "{threads} thread(s)");
    }
    // The result names its solver, in the output and in the file.
    assert_eq!(e.out["output"]["solver"], "ems");
    assert!(b.out["output"].get("solver").is_none());
    let text = std::fs::read_to_string(e.out["output"]["touchstone"].as_str().unwrap()).unwrap();
    assert!(text.starts_with("! cadlab sim.sparams (solver: ems): board region"), "{text}");

    // A plan: nothing runs, the timestep is the solver's.
    let p = exec(&r, &mut s, "sim.sparams", with(args.clone(), json!({"solver": "ems", "plan": true})));
    assert_eq!(p["output"]["planned"], true);
    assert_eq!(p["output"]["timestep"], e.out["output"]["timestep"]);
    assert!(p["output"]["estimate"]["pulse_steps"].as_u64().unwrap() > 100);

    // Lumped ports at the track's ends.
    let lumped = with(
        args.clone(),
        json!({"name": "lumped", "f_max": "10GHz",
            "ports": [{"at": format!("track#{id}")}, {"at": format!("track#{id}"), "end": "end"}]}),
    );
    let (le, lb) = (run(&r, &mut s, &lumped, "ems"), run(&r, &mut s, &lumped, "builtin"));
    for ((f, m), n) in le.ts.freqs.iter().zip(&le.ts.data).zip(&lb.ts.data) {
        let (s12, s21) = (c64(&m[1]), c64(&m[2]));
        assert!(s21.abs() > 0.7 && s21.abs() < 1.002, "{f}: |S21| {}", s21.abs());
        assert!((s12 - s21).abs() < 0.01, "{f}: reciprocity {s12:?} {s21:?}");
        // Measured: within 0.008 to 6 GHz, 0.030 at 9 GHz on these 1 mm cells (the two solvers read a
        // lumped port's voltage and current differently, and this copper is lossy).
        assert!((s21.abs() - c64(&n[2]).abs()).abs() < 0.05, "{f}: |S21| {} vs {}", s21.abs(), c64(&n[2]).abs());
    }

    // What this path does not model is said: rough copper is at the pulse's center only.
    let rough = with(args.clone(), json!({"solver": "ems", "plan": true, "roughness": "2um"}));
    let o = exec(&r, &mut s, "sim.sparams", rough);
    assert!(codes_of(&o).contains(&"sim.ems_roughness_at_f0".to_string()), "{:?}", codes_of(&o));

    // The guards are the built-in solver's.
    let big = with(args.clone(), json!({"solver": "ems", "max_cells": 1000}));
    assert_eq!(fail(&r, &mut s, "sim.sparams", big), "sim.too_many_cells");
    let none = with(args, json!({"solver": "ems", "ports": []}));
    assert_eq!(fail(&r, &mut s, "sim.sparams", none), "sim.no_ports");
}

/// Lossless microstrip, W = 1 mm on 0.5 mm of εr 4, sheet copper, 24 mm between microstrip ports
/// (20 mm between the reference planes): Z₀ and εeff from the S-parameters against the 2D field
/// solver, both solvers.
#[test]
#[ignore]
fn measure_microstrip() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = board_line(&r, "0.5mm", "4", "0", "1mm", 24);
    let out = dir.path().join("out");
    let args = msl_args(
        &out,
        id,
        json!({"f_max": "10GHz", "points": 21, "margin": "5mm", "air": "5mm", "max_cell": "0.5mm", "pml_cells": 6,
            "perfect_copper": true}),
    );
    let (b, e) = both("lossless microstrip", &r, &mut s, &args, (0.5e9, 9.5e9));
    let fs = field_solver(4.0, 0.5, 1.0);
    for (name, ran) in [("builtin", &b), ("ems", &e)] {
        for (f, sm) in ran.ts.freqs.iter().zip(&ran.ts.data) {
            if (1e9..=3.6e9).contains(f) {
                let (z0, gl) = line_from_s(sm, 50.0);
                let ee = eps_eff(gl.im / 0.020, *f);
                println!(
                    "{name} {:.2} GHz: Z0 {:.2} ({:+.2} %), εeff {ee:.4} ({:+.2} %), |S11| {:.4}, |S21| {:.4}, |S11|²+|S21|² {:.5}",
                    f / 1e9,
                    z0.re,
                    100.0 * (z0.re / fs.z0 - 1.0),
                    100.0 * (ee / fs.er_eff - 1.0),
                    c64(&sm[0]).abs(),
                    c64(&sm[2]).abs(),
                    c64(&sm[0]).abs().powi(2) + c64(&sm[2]).abs().powi(2)
                );
            }
        }
    }
    println!("field solver: Z0 {:.2}, εeff {:.4}", fs.z0, fs.er_eff);
}

/// The lossy microstrip of the openEMS oracle (`tests/openems.rs`, `lossy_microstrip_oracle`:
/// 24 mm of 1 mm strip on 0.5 mm of Djordjevic–Sarkar εr 4 / tan δ 0.02 at 1 GHz, 58 MS/m copper,
/// 6 GHz, thirds rule), both variants, both solvers; with perfect copper too, for the conductor
/// part.
#[test]
#[ignore]
fn measure_lossy_microstrip() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = board_line(&r, "0.5mm", "4", "0.02", "1mm", 24);
    exec(&r, &mut s, "board.dielectric", json!({"ref_freq": "1GHz"}));
    let out = dir.path().join("out");
    let track = format!("track#{id}");
    let cases = [
        (
            "msl",
            json!([{"at": track, "kind": "msl", "length": "4mm"}, {"at": track, "end": "end", "kind": "msl", "length": "4mm"}]),
            "sheet",
        ),
        ("lumped", json!([{"at": track}, {"at": track, "end": "end"}]), "solid"),
    ];
    for (name, ports, copper) in cases {
        let common = json!({"dir": out, "name": name, "around": [track], "copper": copper, "thirds": true,
            "ports": ports, "pml_outside": false, "f_max": "6GHz", "points": 13, "margin": "5mm", "air": "5mm",
            "max_cell": "0.5mm", "pml_cells": 6, "end_criteria": "50dB"});
        let (b, e) =
            both(&format!("lossy microstrip, {copper} copper, {name} ports"), &r, &mut s, &common, (0.5e9, 6e9));
        let perfect = with(common.clone(), json!({"name": format!("{name}_pec"), "perfect_copper": true}));
        let (bp, ep) = both(&format!("the same, perfect copper ({name})"), &r, &mut s, &perfect, (0.5e9, 6e9));
        for ((((f, x), y), xp), yp) in
            b.ts.freqs.iter().zip(&b.ts.data).zip(&e.ts.data).zip(&bp.ts.data).zip(&ep.ts.data)
        {
            let ((zb, _), (ze, _)) = (line_from_s(x, 50.0), line_from_s(y, 50.0));
            println!(
                "{name} {:.2} GHz: |S21| builtin {:.4} dB, ems {:.4} dB; conductor part builtin {:.4}, ems {:.4} dB; \
                 Z0 builtin {:.2}, ems {:.2}; |S11| builtin {:.4}, ems {:.4}",
                f / 1e9,
                db(&x[2]),
                db(&y[2]),
                db(&xp[2]) - db(&x[2]),
                db(&yp[2]) - db(&y[2]),
                zb.re,
                ze.re,
                c64(&x[0]).abs(),
                c64(&y[0]).abs()
            );
        }
    }
}

/// A 4-layer board: a 50 Ω microstrip on F.Cu from J1 to a via, continuing on B.Cu to J2, over GND
/// planes on both inner layers, with return vias beside the signal via (as `tests/openems.rs`).
fn via_transition(r: &Registry, s: &mut Session) {
    exec(r, s, "board.setup", json!({"layers": 4, "thickness": "1.6mm"}));
    exec(
        r,
        s,
        "board.dielectric",
        json!({"thickness": "0.2mm", "er": "4.1", "loss_tangent": "0.02", "material": "prepreg"}),
    );
    exec(r, s, "board.dielectric", json!({"gap": 2, "thickness": "1.065mm", "er": "4.5", "material": "core"}));
    exec(r, s, "board.outline", json!({"width": "16mm", "height": "8mm"}));
    exec(
        r,
        s,
        "footprint.create",
        json!({"name": "RF_EDGE", "pads": [
            {"number": "1", "at": ["0mm", "0mm"], "size": ["1mm", "0.6mm"], "shape": "rect"},
            {"number": "2", "at": ["0mm", "1.5mm"], "size": ["1mm", "1mm"], "shape": "rect"},
            {"number": "3", "at": ["0mm", "-1.5mm"], "size": ["1mm", "1mm"], "shape": "rect"}]}),
    );
    exec(
        r,
        s,
        "part.create",
        json!({"id": "RF_CONN", "category": "connector", "footprint": "RF_EDGE",
            "pins": [{"number": "1", "name": "SIG"}, {"number": "2", "name": "GND"}], "pin_map": {"2": ["2", "3"]}}),
    );
    exec(r, s, "circuit.add", json!({"part": "RF_CONN", "count": 2}));
    exec(r, s, "net.connect", json!({"net": "RF", "pins": ["J1.1", "J2.1"]}));
    exec(r, s, "net.connect", json!({"net": "GND", "pins": ["J1.2", "J2.2"]}));
    exec(r, s, "place.set", json!({"refdes": "J1", "at": ["2mm", "4mm"]}));
    exec(r, s, "place.set", json!({"refdes": "J2", "at": ["14mm", "4mm"], "side": "bottom"}));
    for (layer, a, b) in [("F.Cu", "2mm", "8mm"), ("B.Cu", "8mm", "14mm")] {
        exec(
            r,
            s,
            "track.add",
            json!({"layer": layer, "points": [[a, "4mm"], [b, "4mm"]], "width": "0.36mm", "net": "RF"}),
        );
    }
    exec(r, s, "via.add", json!({"at": ["8mm", "4mm"], "net": "RF", "drill": "0.3mm", "diameter": "0.6mm"}));
    for at in
        [["8mm", "5.2mm"], ["8mm", "2.8mm"], ["2mm", "5.5mm"], ["2mm", "2.5mm"], ["14mm", "5.5mm"], ["14mm", "2.5mm"]]
    {
        exec(r, s, "via.add", json!({"at": at, "net": "GND", "drill": "0.3mm", "diameter": "0.6mm"}));
    }
    exec(r, s, "zone.add", json!({"name": "GND_in", "net": "GND", "layers": ["In1.Cu", "In2.Cu"], "outline": "board"}));
}

/// The via transition of the openEMS oracle (`via_transition_export`: the 4-layer board at 4 GHz,
/// lumped ports, the absorbing cells beyond the region), both solvers.
#[test]
#[ignore]
fn measure_via() {
    let r = Registry::with_builtins();
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("via"), "name": "via"}));
    via_transition(&r, &mut s);
    let out = dir.path().join("out");
    let args = json!({"dir": out, "name": "via", "f_max": "4GHz", "points": 9, "nets": ["RF"], "margin": "4mm",
        "pml_outside": true, "end_criteria": "40dB", "ports": [{"at": "J1.1"}, {"at": "J2.1", "layer": "B.Cu"}]});
    let (b, e) = both("via transition", &r, &mut s, &args, (0.5e9, 4e9));
    for ((f, x), y) in b.ts.freqs.iter().zip(&b.ts.data).zip(&e.ts.data) {
        println!(
            "{:.2} GHz: |S21| builtin {:.3} dB, ems {:.3} dB; |S11| builtin {:.2} dB, ems {:.2} dB",
            f / 1e9,
            db(&x[2]),
            db(&y[2]),
            db(&x[0]),
            db(&y[0])
        );
    }
}

/// A 0603 part in a 50 Ω microstrip (as `tests/openems.rs`, `part_board`): two tracks of `len` mm
/// ending on its pads.
fn part_board(r: &Registry, category: &str, params: Value, len: i64) -> (tempfile::TempDir, Session, [u64; 2]) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    let s_ = &mut s;
    exec(r, s_, "project.new", json!({"path": dir.path().join("parts"), "name": "parts"}));
    let mm = |v: f64| format!("{v}mm");
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
    let x = (len + 6) as f64;
    exec(r, s_, "place.set", json!({"refdes": "U1", "at": [mm(x), "10mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J1", "at": ["2mm", "18mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J2", "at": [mm(width as f64 - 2.0), "18mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J3", "at": ["2mm", "2mm"]}));
    let mut track = |a: f64, b: f64, net: &str| {
        let o = exec(
            r,
            s_,
            "track.add",
            json!({"layer": "F.Cu", "points": [[mm(a), "10mm"], [mm(b), "10mm"]], "width": "1mm", "net": net}),
        );
        o["output"]["tracks"][0]["id"].as_u64().unwrap()
    };
    let (t1, t2) = (track(x - len as f64 - 1.0, x - 0.8, "A"), track(x + 0.8, x + len as f64 + 1.0, "B"));
    exec(r, &mut s, "via.add", json!({"at": ["2mm", "2mm"], "net": "GND", "drill": "0.3mm", "diameter": "0.6mm"}));
    exec(r, &mut s, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["B.Cu"], "outline": "board"}));
    (dir, s, [t1, t2])
}

fn part_args(dir: &std::path::Path, t: [u64; 2]) -> Value {
    json!({"dir": dir, "name": "dut", "around": [format!("track#{}", t[0]), format!("track#{}", t[1])],
        "copper": "sheet", "perfect_copper": true, "f_max": "6GHz", "points": 13, "margin": "4mm", "air": "4mm",
        "max_cell": "0.5mm", "pml_cells": 6, "thirds": true, "end_criteria": "50dB", "pml_outside": true,
        "ports": [{"at": format!("track#{}", t[0]), "kind": "msl", "length": "3mm"},
                  {"at": format!("track#{}", t[1]), "end": "end", "kind": "msl", "length": "3mm"}]})
}

type Impedance = Box<dyn Fn(f64) -> C64>;

/// Board parts (the openEMS oracle's `lumped_parts_oracle` setup): a series 50 Ω resistor, a
/// series 1 pF capacitor and the capacitor with 1 nH ESL and 1 Ω ESR (resonant at 5.03 GHz) on a
/// microstrip, both solvers against S21 = 2 Z₀ / (2 Z₀ + Z).
#[test]
#[ignore]
fn measure_parts() {
    let r = Registry::with_builtins();
    let cases: [(&str, &str, Value, Impedance); 3] = [
        ("series R", "resistor", json!({"resistance": "50"}), Box::new(|_| C64::new(50.0, 0.0))),
        (
            "series C",
            "capacitor",
            json!({"capacitance": "1pF"}),
            Box::new(|f| C64::new(0.0, -1.0 / (2.0 * PI * f * 1e-12))),
        ),
        (
            "series RLC",
            "capacitor",
            json!({"capacitance": "1pF", "esl": "1nH", "esr": "1"}),
            Box::new(|f| C64::new(1.0, 2.0 * PI * f * 1e-9 - 1.0 / (2.0 * PI * f * 1e-12))),
        ),
    ];
    for (name, category, params, z) in cases {
        let (dir, mut s, t) = part_board(&r, category, params, 6);
        let out = dir.path().join("out");
        let (b, e) = both(name, &r, &mut s, &part_args(&out, t), (0.5e9, 4.5e9));
        let (mut wb, mut we) = (0.0f64, 0.0f64);
        for ((f, x), y) in b.ts.freqs.iter().zip(&b.ts.data).zip(&e.ts.data) {
            let ideal = (c(100.0) / (c(100.0) + z(*f))).abs();
            let (sb, se) = (c64(&x[2]).abs(), c64(&y[2]).abs());
            println!("{name} {:.2} GHz: |S21| builtin {sb:.4}, ems {se:.4}, theory {ideal:.4}", f / 1e9);
            if (0.5e9..=4.5e9).contains(f) {
                wb = wb.max((sb - ideal).abs());
                we = we.max((se - ideal).abs());
            }
        }
        println!("{name}: 0.5-4.5 GHz against theory: builtin {wb:.4}, ems {we:.4}");
    }
}

/// The 45° line of D73 (`tests/fdtd_conformal.rs`, `routed_45_degree_line`): 8 mm, a 45° run of
/// 14.1 mm, 8 mm, sheet copper, W = 1 mm on 0.5 mm of εr 4; arg S21 at 10 GHz and |S11| against
/// the straight line of that length, both solvers, at the default mesh and at 0.25 mm.
#[test]
#[ignore]
fn measure_diagonal() {
    let r = Registry::with_builtins();
    let run_mm = 10.0;
    for (diag, cell) in [(false, None), (true, None), (true, Some("0.25mm"))] {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new();
        let s_ = &mut s;
        exec(&r, s_, "project.new", json!({"path": dir.path().join("line"), "name": "line"}));
        exec(&r, s_, "board.setup", json!({"layers": 2}));
        exec(&r, s_, "board.dielectric", json!({"thickness": "0.5mm", "er": "4", "loss_tangent": "0"}));
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
                json!([mm(16.0 + run_mm), mm(10.0 + run_mm)]),
                json!([mm(24.0 + run_mm), mm(10.0 + run_mm)]),
            ]
        } else {
            let l = run_mm * std::f64::consts::SQRT_2;
            vec![json!(["8mm", "10mm"]), json!([mm(16.0 + l), "10mm"]), json!([mm(24.0 + l), "10mm"])]
        };
        let o = exec(&r, s_, "track.add", json!({"layer": "F.Cu", "points": pts, "width": "1mm", "net": "RF"}));
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
        if let Some(cell) = cell {
            args["max_cell"] = json!(cell);
        }
        let (bi, em) = both(&format!("45° line (diagonal {diag}, cell {cell:?})"), &r, &mut s, &args, (0.5e9, 10e9));
        for (name, ran) in [("builtin", &bi), ("ems", &em)] {
            for fi in [12, 20, 40] {
                let m = &ran.ts.data[fi];
                let s21 = c64(&m[2]);
                println!(
                    "{name} diagonal {diag} cell {cell:?} {:.1} GHz: S11 {:.1} dB, S21 {:.2} dB, {:.1}°",
                    ran.ts.freqs[fi] / 1e9,
                    db(&m[0]),
                    db(&m[2]),
                    s21.im.atan2(s21.re).to_degrees()
                );
            }
        }
        if diag && cell.is_some() {
            // Solid perfect copper: where ems's conformal cells apply.
            let solid = with(args.clone(), json!({"copper": "solid", "name": "solid", "max_cells": 8_000_000}));
            let e = run(&r, &mut s, &solid, "ems");
            let b = run(&r, &mut s, &solid, "builtin");
            for (name, ran) in [("builtin solid", &b), ("ems solid", &e)] {
                let m = &ran.ts.data[40];
                let s21 = c64(&m[2]);
                println!(
                    "{name}: {} cells, {:.0} s, 10 GHz: S11 {:.1} dB, S21 {:.2} dB, {:.1}°; {:?}",
                    ran.cells(),
                    ran.seconds,
                    db(&m[0]),
                    db(&m[2]),
                    s21.im.atan2(s21.re).to_degrees(),
                    codes_of(&ran.out)
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// ems alone, on the engine-level fixtures of `tests/fdtd_parts.rs` and `tests/fdtd.rs` rebuilt as
// csx models: what ems does with the physics `sim.sparams` hands it.

mod direct {
    use std::f64::consts::PI;

    use cadlab::electrical::complex::C64;
    use cadlab::electrical::dispersion::Dielectric as Dispersive;
    use cadlab::electrical::loss::{self, COPPER_CONDUCTIVITY};
    use cadlab::fdtd::engine::{C0, EPS0};
    use cadlab::fdtd::fit;
    use ems::csx::primitives::BoxShape;
    use ems::csx::properties::{DebyeMaterial, DebyePole, LumpedElement, LumpedElementType, ProbeBox, ProbeType};
    use ems::csx::{self, ContinuousStructure, Primitive, Property};
    use ems::port::LumpedPort;
    use ems::{Axis, EndCriteria, Fdtd, Simulation, Waveform};

    fn c(re: f64) -> C64 {
        C64::new(re, 0.0)
    }

    fn grid(lines: [Vec<f64>; 3]) -> ContinuousStructure {
        let mut m = ContinuousStructure::new();
        m.grid.delta_unit = 1e-3;
        for (a, l) in lines.into_iter().enumerate() {
            m.grid.set_lines(a, l).unwrap();
        }
        m
    }

    fn uniform(cells: i64, d: f64) -> Vec<f64> {
        (0..=cells).map(|i| i as f64 * d).collect()
    }

    pub(super) enum Place {
        Series,
        Shunt,
    }

    pub(super) const FREQS: [f64; 8] = [0.5e9, 1e9, 1.5e9, 2e9, 2.5e9, 3e9, 3.5e9, 4e9];

    /// `tests/fdtd_parts.rs`'s fixture: a strip 0.4 mm wide, 0.2 mm over the bottom wall of a closed
    /// 3 × 1 × 0.5 mm box (0.1 mm cells), between 50 Ω lumped ports at x = 0.5 and 2.5 mm, with `el`
    /// (R, L, C in series with each other) placed in series or shunt. (S11, S21) per frequency.
    pub(super) fn fixture(place: Place, el: Option<(f64, f64, Option<f64>)>) -> Vec<(f64, C64, C64)> {
        let d = 0.1;
        let mut m = grid([uniform(30, d), uniform(10, d), uniform(5, d)]);
        let strip = m.add_property(Property::metal("strip"));
        let (y0, y1, z) = (3.0 * d, 7.0 * d, 2.0 * d);
        let mut plate = |x0: f64, x1: f64| {
            m.add_primitive(strip, Primitive::new(BoxShape::new([x0 * d, y0, z], [x1 * d, y1, z])).with_priority(10))
                .unwrap();
        };
        match (&place, el.is_some()) {
            (Place::Series, true) => {
                plate(5.0, 14.0);
                plate(16.0, 25.0);
            }
            _ => plate(5.0, 25.0),
        }
        if let Some((r, l, cap)) = el {
            let (axis, lo, hi) = match place {
                Place::Series => (csx::Axis::X, [14.0 * d, y0, z], [16.0 * d, y1, z]),
                Place::Shunt => (csx::Axis::Z, [15.0 * d, y0, 0.0], [15.0 * d, y1, z]),
            };
            let mut e = LumpedElement { direction: axis, ..LumpedElement::default() };
            if r > 0.0 {
                e.r = r.into();
            }
            if l > 0.0 {
                e.l = l.into();
            }
            if let Some(cap) = cap {
                e.c = cap.into();
            }
            if l > 0.0 || cap.is_some() {
                e.le_type = Some(LumpedElementType::Series);
            }
            let id = m.add_property(Property::lumped_element("dut", e));
            m.add_primitive(id, Primitive::new(BoxShape::new(lo, hi)).with_priority(9)).unwrap();
        }
        let port = |n: u32, x: f64| LumpedPort::new(n, 50.0, [x * d, y0, z], [x * d, y1, 0.0], Axis::Z).priority(30);
        let p1 = port(1, 5.0).excite(1.0).add_to(&mut m).unwrap();
        let p2 = port(2, 25.0).add_to(&mut m).unwrap();
        let fdtd = Fdtd::new()
            .waveform(Waveform::gaussian(2.5e9, 2.5e9))
            .end_criteria(EndCriteria::energy_decay_db(-60.0))
            .max_timesteps(200_000)
            .timestep_factor(0.95)
            .threads(1);
        let mut sim = Simulation::new(&m, &fdtd).unwrap();
        let res = sim.run().unwrap();
        let (r1, r2) = (p1.analyze(&res, &FREQS).unwrap(), p2.analyze(&res, &FREQS).unwrap());
        let (a1, b1, b2) = (r1.a(), r1.b(), r2.b());
        let cc = |x: ems::Complex64| C64::new(x.re, x.im);
        FREQS.iter().enumerate().map(|(k, &f)| (f, cc(b1[k]) / cc(a1[k]), cc(b2[k]) / cc(a1[k]))).collect()
    }

    pub(super) fn z_series(f: f64, r: f64, l: f64, cap: Option<f64>) -> C64 {
        let w = 2.0 * PI * f;
        C64::new(r, w * l - cap.map_or(0.0, |cc| 1.0 / (w * cc)))
    }

    /// The largest | |S21| − theory | (and | |S11| − theory |) over the fixture's frequencies.
    pub(super) fn worst(place: Place, el: (f64, f64, Option<f64>), label: &str) -> (f64, f64) {
        let shunt = matches!(place, Place::Shunt);
        let (mut w21, mut w11) = (0.0f64, 0.0f64);
        for (f, s11, s21) in fixture(place, Some(el)) {
            let z = z_series(f, el.0, el.1, el.2);
            let (t21, t11) = if shunt {
                (c(2.0) * z / (c(2.0) * z + c(50.0)), c(-50.0) / (c(2.0) * z + c(50.0)))
            } else {
                (c(100.0) / (c(100.0) + z), z / (c(100.0) + z))
            };
            println!(
                "{label} {:.1} GHz: |S21| {:.4} vs {:.4}, |S11| {:.4} vs {:.4}",
                f / 1e9,
                s21.abs(),
                t21.abs(),
                s11.abs(),
                t11.abs()
            );
            w21 = w21.max((s21.abs() - t21.abs()).abs());
            w11 = w11.max((s11.abs() - t11.abs()).abs());
        }
        println!("{label}: worst |S21| {w21:.4}, |S11| {w11:.4}");
        (w21, w11)
    }

    /// The frequency of the largest |x|, refined by a parabola through the three samples around it.
    fn peak(freqs: &[f64], x: &[ems::Complex64]) -> f64 {
        let k = (1..x.len() - 1).max_by(|&a, &b| x[a].norm().total_cmp(&x[b].norm())).unwrap();
        let (a, b, cc) = (x[k - 1].norm(), x[k].norm(), x[k + 1].norm());
        let shift = 0.5 * (a - cc) / (a - 2.0 * b + cc);
        freqs[k] + shift * (freqs[k + 1] - freqs[k])
    }

    /// A cavity `a × b` mm between plates 0.5 mm apart in 40 × 24 × 1 cells, filled with `fill`; with
    /// `sheets`, the plates are conducting sheets of that conductivity (35 µm) in front of vacuum
    /// gaps, else the domain's PEC walls. A 1 GΩ source near a corner, a voltage probe elsewhere.
    fn cavity(a: f64, b: f64, fill: Property, sheets: Option<f64>) -> (ContinuousStructure, LumpedPort) {
        let (h, gap) = (0.5, 0.25);
        let zs = if sheets.is_some() { vec![-gap, 0.0, h, h + gap] } else { vec![0.0, h] };
        let mut m = grid([uniform(40, a / 40.0), uniform(24, b / 24.0), zs]);
        let id = m.add_property(fill);
        m.add_primitive(id, Primitive::new(BoxShape::new([0.0, 0.0, 0.0], [a, b, h]))).unwrap();
        if let Some(sigma) = sheets {
            let id = m.add_property(Property::conducting_sheet("plates", sigma, 35e-6));
            for z in [0.0, h] {
                m.add_primitive(id, Primitive::new(BoxShape::new([0.0, 0.0, z], [a, b, z])).with_priority(10)).unwrap();
            }
        }
        let (dx, dy) = (a / 40.0, b / 24.0);
        let port = LumpedPort::new(1, 1e9, [5.0 * dx, 4.0 * dy, h], [5.0 * dx, 4.0 * dy, 0.0], Axis::Z)
            .priority(30)
            .excite(1.0)
            .add_to(&mut m)
            .unwrap();
        let probe = m.add_property(Property::probe("v", ProbeBox::new(ProbeType::Voltage)));
        m.add_primitive(
            probe,
            Primitive::new(BoxShape::new([27.0 * dx, 15.0 * dy, h], [27.0 * dx, 15.0 * dy, 0.0])).with_priority(30),
        )
        .unwrap();
        (m, port)
    }

    /// Resonance near `f` from the probe's spectrum after `steps` steps of a pulse around it.
    fn resonance(m: &ContinuousStructure, f: f64, fc: f64, steps: u64) -> f64 {
        let fdtd =
            Fdtd::new().waveform(Waveform::gaussian(f, fc)).max_timesteps(steps).timestep_factor(0.95).threads(1);
        let mut sim = Simulation::new(m, &fdtd).unwrap();
        let res = sim.run().unwrap();
        let freqs: Vec<f64> = (-40..=40).map(|k| f * (1.0 + k as f64 * 1e-3)).collect();
        peak(&freqs, &res.probe("v").unwrap().dft(&freqs).unwrap())
    }

    /// Q of the ringing mode from the decay of the energy after a narrow pulse at `f`.
    fn ringing_q(m: &ContinuousStructure, f: f64, periods: f64) -> f64 {
        let w = Waveform::gaussian(f, f / 5.0);
        let fdtd = Fdtd::new().waveform(w.clone()).timestep_factor(0.95).threads(1);
        let mut sim = Simulation::new(m, &fdtd).unwrap();
        let dt = sim.engine().dt();
        sim.step((w.duration().unwrap() / dt).ceil() as u64);
        let (mut sx, mut sy, mut sxx, mut sxy, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for k in 0..(periods / f / dt) as u64 {
            sim.step(1);
            let (x, y) = (k as f64 * dt, sim.engine().energy().ln());
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
            n += 1.0;
        }
        let slope = (n * sxy - sx * sy) / (n * sxx - sx * sx);
        2.0 * PI * f / -slope
    }

    fn tm_vac(mm: f64, nn: f64, a: f64, b: f64) -> f64 {
        C0 / 2.0 * ((mm / (a * 1e-3)).powi(2) + (nn / (b * 1e-3)).powi(2)).sqrt()
    }

    /// PEC cavity resonances, Q from a conductivity, from conducting-sheet plates and from a
    /// Djordjevic–Sarkar filling: (label, measured, reference) rows.
    pub(super) fn cavities() -> Vec<(String, f64, f64)> {
        let mut rows = Vec::new();
        let er = 4.4;
        let plain = |sigma: f64| Property::material("fill", csx::Material::new(er, sigma));
        // 20 × 12 mm, εr 4.4, 0.5 mm cells: TM110 and TM210.
        let (m, _) = cavity(20.0, 12.0, plain(0.0), None);
        for (mm, nn) in [(1.0, 1.0), (2.0, 1.0)] {
            let want = tm_vac(mm, nn, 20.0, 12.0) / er.sqrt();
            rows.push((format!("PEC cavity TM{mm}{nn}0 (Hz)"), resonance(&m, want, 3e9, 30_000), want));
        }
        // σ for tan δ 0.01 at TM110: Q = 100.
        let f = tm_vac(1.0, 1.0, 20.0, 12.0) / er.sqrt();
        let (m, _) = cavity(20.0, 12.0, plain(2.0 * PI * f * EPS0 * er * 0.01), None);
        rows.push(("cavity Q, tan δ 0.01 as σ".into(), ringing_q(&m, f, 15.0), 100.0));
        // Copper plates as conducting sheets: Q = h / δ.
        for (a, b) in [(100.0, 60.0), (20.0, 12.0), (10.0, 6.0)] {
            let f = tm_vac(1.0, 1.0, a, b) / er.sqrt();
            let (m, _) = cavity(a, b, plain(0.0), Some(COPPER_CONDUCTIVITY));
            let want = 500e-6 / loss::skin_depth(f, COPPER_CONDUCTIVITY);
            rows.push((format!("plate cavity Q, 58 MS/m sheets, {:.1} GHz", f / 1e9), ringing_q(&m, f, 15.0), want));
        }
        // Djordjevic–Sarkar εr 4, tan δ 0.02 at 1 GHz as Debye terms: resonance and Q = 1 / tan δ(f).
        let d = Dispersive { er: 4.0, tan_d: 0.02, ref_freq: Some(1e9) };
        for (band, a, b) in [((1e8, 1e10), 60.0, 36.0), ((1e8, 1e10), 16.0, 10.0), ((5e8, 2e10), 10.0, 6.0)] {
            let taus = fit::relaxation_times(band.0, band.1);
            let fitted = fit::debye_fit(&d.model(0.0), &taus, band.0, band.1);
            let fill = Property::debye_material(
                "fill",
                DebyeMaterial {
                    material: csx::Material::new(fitted.eps_inf, fitted.sigma),
                    poles: fitted
                        .delta
                        .iter()
                        .zip(&taus)
                        .filter(|(x, _)| **x > 0.0)
                        .map(|(x, t)| DebyePole::new(*x, *t))
                        .collect(),
                },
            );
            let fv = tm_vac(1.0, 1.0, a, b);
            let mut f = fv / 2.0;
            for _ in 0..20 {
                f = fv / d.at(f).0.sqrt();
            }
            let (m, _) = cavity(a, b, fill, None);
            rows.push((
                format!("Debye cavity resonance, {:.1} GHz (Hz)", f / 1e9),
                resonance(&m, f, f / 5.0, 20_000),
                f,
            ));
            rows.push((format!("Debye cavity Q, {:.1} GHz", f / 1e9), ringing_q(&m, f, 10.0), 1.0 / d.at(f).1));
        }
        rows
    }
}

/// ems on the lumped-element fixture of `tests/fdtd_parts.rs`: series R 10 / 50 / 200 Ω, series
/// 1 pF, 5 nH and 10 Ω + 4 nH + 1 pF, shunt 2 pF and a shunt 5 Ω + 2 nH + 2 pF notch, against
/// circuit theory.
#[test]
#[ignore]
fn measure_lumped_fixture() {
    use direct::{Place, worst};
    for (f, s11, s21) in direct::fixture(Place::Series, None) {
        println!("plain strip {:.1} GHz: |S21| {:.4}, |S11| {:.4}", f / 1e9, s21.abs(), s11.abs());
    }
    for r in [10.0, 50.0, 200.0] {
        worst(Place::Series, (r, 0.0, None), &format!("series R {r}"));
    }
    worst(Place::Series, (0.0, 0.0, Some(1e-12)), "series 1 pF");
    worst(Place::Series, (0.0, 5e-9, None), "series 5 nH");
    worst(Place::Series, (10.0, 4e-9, Some(1e-12)), "series 10 Ω + 4 nH + 1 pF");
    worst(Place::Shunt, (0.0, 0.0, Some(2e-12)), "shunt 2 pF");
    worst(Place::Shunt, (5.0, 2e-9, Some(2e-12)), "shunt 5 Ω + 2 nH + 2 pF");
    worst(Place::Shunt, (50.0, 0.0, None), "shunt 50 Ω");
}

/// ems on the cavities of `tests/fdtd.rs`: resonances of a PEC box, Q from a conductivity, from
/// copper plates (conducting sheets) and from a Djordjevic–Sarkar filling as Debye terms.
#[test]
#[ignore]
fn measure_cavities() {
    for (label, got, want) in direct::cavities() {
        println!("{label}: {got:.5e} vs {want:.5e} ({:+.2} %)", 100.0 * (got / want - 1.0));
    }
}

/// The dielectric loss alone: the oracle's microstrip with perfect solid copper (35 µm) on the
/// Djordjevic–Sarkar laminate, |S21| over the 20 mm between the reference planes against the
/// field solver with the same model (D52), both solvers.
#[test]
#[ignore]
fn measure_dielectric_loss() {
    use cadlab::electrical::fieldsolver::LossTangents;
    use cadlab::electrical::loss::{self, Conductor, Roughness};
    let r = Registry::with_builtins();
    let (dir, mut s, id) = board_line(&r, "0.5mm", "4", "0.02", "1mm", 24);
    exec(&r, &mut s, "board.dielectric", json!({"ref_freq": "1GHz"}));
    let out = dir.path().join("out");
    let st = fieldsolver::Structure {
        model: Model::Microstrip,
        h: 0.5,
        er: 4.0,
        h2: 0.0,
        er2: 4.0,
        t: 0.035,
        etch: 0.0,
        broadside: None,
        coplanar_gap: None,
        mask: None,
    };
    let tans = LossTangents { below: 0.02, ref_freq: [Some(1e9), None, None], ..LossTangents::default() };
    let m = st.line_matrices(1.0, None, false, &tans, Accuracy::default()).unwrap();
    let perfect = Conductor { sigma: 1e30, roughness: Roughness::Smooth, r_dc: 0.0 };
    for cell in ["0.5mm"] {
        let args = msl_args(
            &out,
            id,
            json!({"name": format!("diel_{cell}"), "thirds": true, "f_max": "6GHz", "points": 13, "margin": "5mm",
                "air": "5mm", "max_cell": cell, "pml_cells": 6, "end_criteria": "50dB", "perfect_copper": true, "copper": "solid"}),
        );
        let (b, e) = both(&format!("dielectric loss, cell {cell}"), &r, &mut s, &args, (0.5e9, 6e9));
        for ((f, x), y) in b.ts.freqs.iter().zip(&b.ts.data).zip(&e.ts.data) {
            let p = loss::at(&m, *f, &perfect);
            let want = -20.0 * std::f64::consts::LOG10_E * p.alpha * 0.020;
            // The loss of the line itself: what the two ports do not reflect.
            let avail = |v: &[cadlab::touchstone::Complex]| {
                10.0 * (c64(&v[2]).abs().powi(2) / (1.0 - c64(&v[0]).abs().powi(2))).log10()
            };
            println!(
                "cell {cell} {:.2} GHz: D52 {want:.4} dB; builtin {:.4} ({:+.1} %), ems {:.4} ({:+.1} %)",
                f / 1e9,
                avail(x),
                100.0 * (avail(x) / want - 1.0),
                avail(y),
                100.0 * (avail(y) / want - 1.0)
            );
        }
    }
}

/// The same region at 1 and 4 worker threads, both solvers: identical Touchstone data, and the
/// time each takes. The lossy model (Debye laminate, conducting sheets / surface impedance).
#[test]
#[ignore]
fn measure_threads() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = board_line(&r, "0.5mm", "4", "0.02", "1mm", 24);
    exec(&r, &mut s, "board.dielectric", json!({"ref_freq": "1GHz"}));
    let out = dir.path().join("out");
    for (label, extra) in [("lossless", json!({"perfect_copper": true})), ("lossy", json!({}))] {
        if label == "lossless" {
            exec(&r, &mut s, "board.dielectric", json!({"loss_tangent": "0"}));
        } else {
            exec(&r, &mut s, "board.dielectric", json!({"loss_tangent": "0.02", "ref_freq": "1GHz"}));
        }
        let args = with(
            msl_args(
                &out,
                id,
                json!({"f_max": "6GHz", "points": 13, "margin": "5mm", "air": "5mm", "max_cell": "0.5mm",
                    "pml_cells": 6, "end_criteria": "50dB"}),
            ),
            extra,
        );
        let t = Instant::now();
        exec(&r, &mut s, "sim.sparams", with(args.clone(), json!({"solver": "ems", "plan": true})));
        println!("{label}: ems plan (model compiled once) {:.2} s", t.elapsed().as_secs_f64());
        for solver in ["builtin", "ems"] {
            let one =
                run(&r, &mut s, &with(args.clone(), json!({"threads": 1, "name": format!("{label}_t1")})), solver);
            let four =
                run(&r, &mut s, &with(args.clone(), json!({"threads": 4, "name": format!("{label}_t4")})), solver);
            println!(
                "{label} {solver}: {} cells, steps {:?}; 1 thread {:.1} s ({:.0} Mcells/s, {:.0} MB), 4 threads {:.1} s \
                 ({:.0} Mcells/s); identical {}",
                one.cells(),
                one.steps(),
                one.seconds,
                one.rate(),
                one.mem_mb,
                four.seconds,
                four.rate(),
                one.ts.data == four.ts.data
            );
            assert!(one.ts.data == four.ts.data, "{label} {solver}: the thread count changed the result");
        }
    }
}

/// A symmetric stripline: a 0.25 mm strip on In1.Cu between GND planes on F.Cu and In2.Cu, 0.3 mm
/// of εr 4 on either side (lossless, sheet copper), 24 mm between microstrip-kind ports that feed
/// against the lower plane, the planes stitched by vias beside each port. Z₀ and εeff from the
/// S-parameters against the 2D field solver (εeff = εr exactly), both solvers.
#[test]
#[ignore]
fn measure_stripline() {
    let r = Registry::with_builtins();
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    let s_ = &mut s;
    exec(&r, s_, "project.new", json!({"path": dir.path().join("strip"), "name": "strip"}));
    exec(&r, s_, "board.setup", json!({"layers": 4}));
    for gap in 1..=3 {
        exec(&r, s_, "board.dielectric", json!({"gap": gap, "thickness": "0.3mm", "er": "4", "loss_tangent": "0"}));
    }
    exec(&r, s_, "board.outline", json!({"width": "34mm", "height": "20mm"}));
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
    exec(&r, s_, "place.set", json!({"refdes": "J1", "at": ["2mm", "18mm"], "side": "bottom"}));
    let o = exec(
        &r,
        s_,
        "track.add",
        json!({"layer": "In1.Cu", "points": [["5mm", "8mm"], ["29mm", "8mm"]], "width": "0.25mm", "net": "RF"}),
    );
    let id = o["output"]["tracks"][0]["id"].as_u64().unwrap();
    for x in ["5.5mm", "7mm", "27mm", "28.5mm"] {
        for y in ["6.8mm", "9.2mm"] {
            exec(&r, s_, "via.add", json!({"at": [x, y], "net": "GND", "drill": "0.3mm", "diameter": "0.5mm"}));
        }
    }
    exec(&r, s_, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["F.Cu", "In2.Cu"], "outline": "board"}));
    let out = dir.path().join("out");
    let args = json!({"dir": out, "name": "strip", "around": [format!("track#{id}")], "copper": "sheet",
        "perfect_copper": true, "f_max": "10GHz", "points": 21, "margin": "3mm", "air": "3mm", "max_cell": "0.3mm",
        "pml_cells": 6,
        "ports": [{"at": format!("track#{id}"), "kind": "msl", "length": "4mm", "reference": "In2.Cu"},
                  {"at": format!("track#{id}"), "end": "end", "kind": "msl", "length": "4mm", "reference": "In2.Cu"}]});
    let (b, e) = both("stripline", &r, &mut s, &args, (0.5e9, 9.5e9));
    let st = fieldsolver::Structure {
        model: Model::Stripline,
        h: 0.3,
        er: 4.0,
        h2: 0.3,
        er2: 4.0,
        t: 0.0,
        etch: 0.0,
        broadside: None,
        coplanar_gap: None,
        mask: None,
    };
    let fs = st.line(0.25, Accuracy::default()).unwrap();
    for (name, ran) in [("builtin", &b), ("ems", &e)] {
        for (f, sm) in ran.ts.freqs.iter().zip(&ran.ts.data) {
            if (1e9..=3.1e9).contains(f) {
                let (z0, gl) = line_from_s(sm, 50.0);
                let ee = eps_eff(gl.im / 0.020, *f);
                println!(
                    "{name} {:.2} GHz: Z0 {:.2} ({:+.2} %), εeff {ee:.4} ({:+.2} %), |S11| {:.4}, |S21| {:.4}",
                    f / 1e9,
                    z0.re,
                    100.0 * (z0.re / fs.z0 - 1.0),
                    100.0 * (ee / fs.er_eff - 1.0),
                    c64(&sm[0]).abs(),
                    c64(&sm[2]).abs(),
                );
            }
        }
    }
    println!("field solver: Z0 {:.2}, εeff {:.4}", fs.z0, fs.er_eff);
}
