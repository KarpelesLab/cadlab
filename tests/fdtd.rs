//! `sim.sparams` (`cadlab::fdtd`, on the ems solver) on a routed microstrip, against analytic
//! results: a matched line through the command with its guards and errors, the full lossy model
//! against the 2D field solver with the perturbation losses of D52 (Z₀, εeff, |S21|), a lossless
//! line's passivity near the absorber, and the solder mask against the field solver's.
//!
//! The coarse run is in debug; the validations are `#[ignore]`: run them with
//! `cargo test --release --test fdtd -- --ignored`.

use std::f64::consts::PI;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::electrical::Model;
use cadlab::electrical::complex::C64;
use cadlab::electrical::fieldsolver::{self, Accuracy, LossTangents};
use cadlab::electrical::loss::{self, COPPER_CONDUCTIVITY, Conductor, Roughness};
use cadlab::openems::C0;
use serde_json::{Value, json};

fn c(re: f64) -> C64 {
    C64::new(re, 0.0)
}

fn eps_eff(beta: f64, f: f64) -> f64 {
    (beta * C0 / (2.0 * PI * f)).powi(2)
}

/// The matrices of the validation's line from the field solver (D52): W = 1 mm of 35 µm copper on
/// 0.5 mm of εr 4 with `tan` at `ref_freq` (Djordjevic–Sarkar).
fn d52_matrices(tan: f64, ref_freq: Option<f64>) -> fieldsolver::LineMatrices {
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
    let tans = LossTangents { below: tan, ref_freq: [ref_freq, None, None], ..LossTangents::default() };
    st.line_matrices(1.0, None, false, &tans, Accuracy::default()).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Board level: `sim.sparams` on a routed microstrip.

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn codes_of(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

fn fail(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> String {
    r.execute(s, cmd, args, RunOptions::default()).expect_err(cmd).error.diagnostic.code.to_string()
}

/// A two-layer board: a straight microstrip on F.Cu (`w` wide, `len_mm` long, from (5, 12) mm)
/// over a GND pour on B.Cu, one dielectric `h` thick. The nets come from two connectors placed
/// far from the line. Returns the project directory, the session and the track's ID.
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
    // A ground via keeps the pour from being removed as an island.
    exec(r, s_, "via.add", json!({"at": ["2mm", "22mm"], "net": "GND", "drill": "0.3mm", "diameter": "0.6mm"}));
    exec(r, s_, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["B.Cu"], "outline": "board"}));
    (dir, s, id)
}

fn c64(v: &cadlab::touchstone::Complex) -> C64 {
    C64::new(v.re, v.im)
}

/// ABCD of a 2-port from its S-parameters (reference `zr`), then the line's Z₀ = √(B/C) and
/// γl = acosh(A) (|Re| is the attenuation whatever the branch).
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

fn sim_args(dir: &std::path::Path, id: u64, extra: Value) -> Value {
    let mut a = json!({"dir": dir, "name": "line", "around": [format!("track#{id}")], "copper": "sheet",
        "ports": [{"at": format!("track#{id}"), "kind": "msl", "length": "4mm"},
                  {"at": format!("track#{id}"), "end": "end", "kind": "msl", "length": "4mm"}]});
    for (k, v) in extra.as_object().unwrap() {
        a[k] = v.clone();
    }
    a
}

/// A coarse run through the command: a matched line passes, the result is reciprocal and
/// deterministic; guards and errors.
#[test]
fn sim_sparams_command() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = board_line(&r, "1mm", "4", "0.02", "2mm", 8);
    let out = dir.path().join("out");
    let args = sim_args(
        &out,
        id,
        json!({"f_max": "5GHz", "points": 11, "margin": "4mm", "air": "3mm", "max_cell": "1mm", "pml_cells": 4,
            "end_criteria": "30dB"}),
    );
    let base = exec(&r, &mut s, "sim.sparams", args.clone());
    let o = base.clone();
    let x = &o["output"];
    assert!(o["diagnostics"].as_array().unwrap().is_empty(), "{}", o["diagnostics"]);
    // The result names the solver and its version; so does a plan.
    assert_eq!(x["solver"], cadlab::fdtd::SOLVER);
    let mut plan = args.clone();
    plan["plan"] = json!(true);
    assert_eq!(exec(&r, &mut s, "sim.sparams", plan)["output"]["solver"], cadlab::fdtd::SOLVER);
    // There is one solver and nothing to choose (DECISIONS D100): the parameter is gone, and a
    // request that still names it is refused as any unknown argument is.
    for old in ["ems", "builtin"] {
        let mut named = args.clone();
        named["solver"] = json!(old);
        let e = r.execute(&mut s, "sim.sparams", named, RunOptions::default()).expect_err(old).error;
        assert_eq!(e.diagnostic.code, "args.invalid", "{e}");
        assert!(e.to_string().contains("solver"), "{e}");
    }
    assert_eq!(x["runs"].as_array().unwrap().len(), 2);
    assert!(x["runs"].as_array().unwrap().iter().all(|r| r["converged"] == true), "{}", x["runs"]);
    assert_eq!(x["ports"][0]["reference_plane"], json!(["7mm", "12mm"]));
    let text = std::fs::read_to_string(out.join("line.s2p")).unwrap();
    let ts = cadlab::touchstone::parse(&text, Some(2)).unwrap();
    assert_eq!(ts.freqs.len(), 11);
    for (f, m) in ts.freqs.iter().zip(&ts.data) {
        let (s11, s12, s21) = (c64(&m[0]), c64(&m[1]), c64(&m[2]));
        assert!(s11.abs() < 0.2, "{f}: |S11| {}", s11.abs());
        assert!(s21.abs() > 0.9 && s21.abs() < 1.0, "{f}: |S21| {}", s21.abs());
        assert!((s12 - s21).abs() < 0.01, "{f}: reciprocity {s12:?} {s21:?}");
    }
    let names: Vec<&str> = x["params"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["S11", "S12", "S21", "S22"]);

    // Lumped ports at the track's ends: the launch adds its own reactance, the line still passes.
    let mut lumped = args.clone();
    lumped["name"] = json!("lumped");
    lumped["f_max"] = json!("10GHz");
    lumped["ports"] = json!([{"at": format!("track#{id}")}, {"at": format!("track#{id}"), "end": "end"}]);
    let o = exec(&r, &mut s, "sim.sparams", lumped);
    assert_eq!(o["output"]["ports"][1]["kind"], "lumped");
    let ts = cadlab::touchstone::parse(&std::fs::read_to_string(out.join("lumped.s2p")).unwrap(), Some(2)).unwrap();
    for (f, m) in ts.freqs.iter().zip(&ts.data) {
        let (s12, s21) = (c64(&m[1]), c64(&m[2]));
        // Passive within the truncation of a 30 dB end criterion.
        assert!(s21.abs() > 0.7 && s21.abs() < 1.002, "{f}: |S21| {}", s21.abs());
        assert!((s12 - s21).abs() < 0.01, "{f}: reciprocity {s12:?} {s21:?}");
    }

    // Solder mask (assumed εr and tan δ noted), rough copper: a lossier line, still matched.
    let mut masked = args.clone();
    masked["name"] = json!("masked");
    masked["mask"] = json!("50um");
    masked["roughness"] = json!("2um");
    let o = exec(&r, &mut s, "sim.sparams", masked);
    let mut notes = codes_of(&o);
    notes.sort();
    assert_eq!(notes, ["openems.mask_er_assumed", "openems.mask_loss_tangent_assumed"]);
    let mts = cadlab::touchstone::parse(&std::fs::read_to_string(out.join("masked.s2p")).unwrap(), Some(2)).unwrap();
    for (f, m) in mts.freqs.iter().zip(&mts.data) {
        let s21 = c64(&m[2]).abs();
        assert!(c64(&m[0]).abs() < 0.2 && s21 > 0.9 && s21 < 1.002, "{f}: |S21| {s21}");
    }
    // Mesh lines at both surfaces of the mask on both sides.
    let zl = |o: &Value| o["output"]["mesh"]["lines"][2].as_u64().unwrap();
    assert!(zl(&o) >= zl(&base) + 2, "{} vs {}", zl(&o), zl(&base));

    // Guards and errors.
    let mut copper = args.clone();
    copper["perfect_copper"] = json!(true);
    copper["conductivity"] = json!("58M");
    assert_eq!(fail(&r, &mut s, "sim.sparams", copper), "sim.perfect_copper");
    let mut big = args.clone();
    big["max_cells"] = json!(1000);
    assert_eq!(fail(&r, &mut s, "sim.sparams", big), "sim.too_many_cells");
    let mut none = args.clone();
    none["ports"] = json!([]);
    assert_eq!(fail(&r, &mut s, "sim.sparams", none), "sim.no_ports");
    let mut bad = args;
    bad["end_criteria"] = json!("3V");
    assert_eq!(fail(&r, &mut s, "sim.sparams", bad), "value.wrong_unit");
}

/// The full model through `sim.sparams`: a 64 mm microstrip, 1 mm wide on 0.5 mm of a
/// Djordjevic–Sarkar laminate (εr 4, tan δ 0.02 at 1 GHz), 35 µm copper (solid) at 58 MS/m,
/// between microstrip ports. Against D52 with the same models (field solver with the copper
/// thickness, Djordjevic–Sarkar, skin effect): Z₀ and εeff from the S-parameters (ABCD of the
/// 60 mm between the reference planes), a matched line's |S11|, and |S21| from 0.5 to 9.5 GHz.
#[test]
#[ignore]
fn sim_sparams_microstrip_validation() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = board_line(&r, "0.5mm", "4", "0.02", "1mm", 64);
    exec(&r, &mut s, "board.dielectric", json!({"ref_freq": "1GHz"}));
    let out = dir.path().join("out");
    let o = exec(
        &r,
        &mut s,
        "sim.sparams",
        sim_args(
            &out,
            id,
            json!({"f_max": "10GHz", "points": 21, "margin": "5mm", "air": "5mm", "max_cell": "0.5mm", "pml_cells": 6,
                "copper": "solid"}),
        ),
    );
    assert!(o["output"]["runs"].as_array().unwrap().iter().all(|r| r["converged"] == true));
    let ts = cadlab::touchstone::parse(&std::fs::read_to_string(out.join("line.s2p")).unwrap(), Some(2)).unwrap();
    let m = d52_matrices(0.02, Some(1e9));
    let cond = Conductor {
        sigma: COPPER_CONDUCTIVITY,
        roughness: Roughness::Smooth,
        r_dc: 1.0 / (COPPER_CONDUCTIVITY * 1e-3 * 35e-6),
    };
    // The reference planes are 2 mm inside each end.
    let l = 0.060;
    // Every frequency is printed before anything fails.
    let mut fails = Vec::new();
    let (mut worst, mut sq, mut n) = (0.0f64, 0.0, 0);
    for (f, sm) in ts.freqs.iter().zip(&ts.data) {
        let (z0, gl) = line_from_s(sm, 50.0);
        let p = loss::at(&m, *f, &cond);
        if (1e9..=3.5e9).contains(f) {
            // βl beyond π: the branch (±θ + 2πm) nearest the model's.
            let bl = [gl.im, -gl.im]
                .map(|th| th + 2.0 * PI * ((p.beta * l - th) / (2.0 * PI)).round())
                .into_iter()
                .min_by(|a, b| (a - p.beta * l).abs().total_cmp(&(b - p.beta * l).abs()))
                .unwrap();
            let ee = eps_eff(bl / l, *f);
            let want_ee = (p.beta / (2.0 * PI * f) * C0).powi(2);
            println!("{f}: Z0 {:.2} vs {:.2}, εeff {ee:.4} vs {want_ee:.4}", z0.re, p.z0.re);
            if (z0.re - p.z0.re).abs() >= 0.03 * p.z0.re {
                fails.push(format!("{f}: Z0 {z0:?} vs {:?}", p.z0));
            }
            if (ee - want_ee).abs() >= 0.015 * want_ee {
                fails.push(format!("{f}: εeff {ee} vs {want_ee}"));
            }
        }
        if (0.5e9..=9.5e9).contains(f) {
            if c64(&sm[0]).abs() >= 0.04 {
                fails.push(format!("{f}: |S11| {}", c64(&sm[0]).abs()));
            }
            let s21_db = 20.0 * c64(&sm[2]).abs().log10();
            let want_db = -20.0 * std::f64::consts::LOG10_E * p.alpha * l;
            // −1.3 % to +5.3 % (ems 0.0.9 on; with openEMS's rule for a laminate's poles at its
            // surface +7.2 %, D89, D90). The tolerance is the one the removed built-in solver was
            // held to (−2.3 % to +5.0 %, D68).
            let tol = if *f < 2.5e9 { 0.03 } else { 0.06 };
            println!(
                "{f}: |S21| {s21_db:.4} vs {want_db:.4} dB ({:+.1} %), |S11| {:.4}",
                100.0 * (s21_db / want_db - 1.0),
                c64(&sm[0]).abs()
            );
            if (s21_db - want_db).abs() >= tol * want_db.abs() {
                fails.push(format!("{f}: |S21| {s21_db} vs {want_db} dB"));
            }
            worst = worst.max((s21_db - want_db).abs());
            sq += (s21_db / want_db - 1.0).powi(2);
            n += 1;
        }
    }
    println!(
        "worst |S21| {worst:.4} dB from D52, RMS {:.2} %, timestep {} s, {} steps",
        100.0 * (sq / n as f64).sqrt(),
        o["output"]["timestep"],
        o["output"]["runs"][0]["steps"]
    );
    assert!(fails.is_empty(), "{fails:#?}");
}

/// The same 60 mm line lossless (εr 4, perfect copper) with the side PML 4 h from the strip:
/// passive, |S21|² + |S11|² ≤ 1 within 0.25 % from 0.5 to 4 GHz (0.1 % measured). With a CPML's
/// frequency shift α at a tenth of f₀ (D59) the line's near field drew energy from the PML: up to
/// 0.7 % gain (−0.06 Np/m) around 1.8 GHz, the low reading of D63's lossy line (DECISIONS D68);
/// the shift is at f₀.
#[test]
#[ignore]
fn lossless_line_near_the_pml_is_passive() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = board_line(&r, "0.5mm", "4", "0", "1mm", 64);
    let out = dir.path().join("out");
    exec(
        &r,
        &mut s,
        "sim.sparams",
        sim_args(
            &out,
            id,
            json!({"f_max": "10GHz", "points": 41, "margin": "5mm", "air": "5mm", "max_cell": "0.5mm",
                "pml_cells": 6, "copper": "solid", "perfect_copper": true}),
        ),
    );
    let ts = cadlab::touchstone::parse(&std::fs::read_to_string(out.join("line.s2p")).unwrap(), Some(2)).unwrap();
    for (f, sm) in ts.freqs.iter().zip(&ts.data) {
        if (0.5e9..=4e9).contains(f) {
            let p = c64(&sm[0]).abs().powi(2) + c64(&sm[2]).abs().powi(2);
            println!("{f}: |S11|² + |S21|² = {p:.5}");
            assert!(p < 1.0025, "{f}: |S11|² + |S21|² = {p}");
        }
    }
}

/// Solder mask in `sim.sparams`: the 24 mm microstrip (sheet copper, lossless) under 25 µm of
/// εr 3.8, against the field solver's conformal mask (zero thickness: a flat layer): Z₀ within
/// 1.5 %, εeff within 0.5 %, and the change the mask makes to εeff within 30 % of the solver's.
#[test]
#[ignore]
fn sim_sparams_mask_validation() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = board_line(&r, "0.5mm", "4", "0", "1mm", 24);
    let out = dir.path().join("out");
    let base = json!({"f_max": "10GHz", "points": 21, "margin": "5mm", "air": "5mm", "max_cell": "0.5mm",
        "pml_cells": 6, "perfect_copper": true});
    let mut masked = base.clone();
    masked["mask"] = json!("25um");
    masked["mask_er"] = json!("3.8");
    masked["mask_loss_tangent"] = json!("0");
    masked["name"] = json!("masked");
    let solve = |mask: Option<fieldsolver::Mask>| {
        let st = fieldsolver::Structure {
            model: Model::Microstrip,
            h: 0.5,
            er: 4.0,
            h2: 0.0,
            er2: 4.0,
            t: 0.0,
            etch: 0.0,
            broadside: None,
            coplanar_gap: None,
            mask,
        };
        st.line(1.0, Accuracy::default()).unwrap()
    };
    let bare = solve(None);
    let fs = solve(Some(fieldsolver::Mask { thickness: 0.025, over_copper: 0.025, er: 3.8 }));
    let mut ee_runs = Vec::new();
    for (name, args) in [("line", base), ("masked", masked)] {
        exec(&r, &mut s, "sim.sparams", sim_args(&out, id, args));
        let ts = cadlab::touchstone::parse(&std::fs::read_to_string(out.join(format!("{name}.s2p"))).unwrap(), Some(2))
            .unwrap();
        let (mut z, mut ee, mut n) = (0.0, 0.0, 0.0);
        for (f, sm) in ts.freqs.iter().zip(&ts.data) {
            if (1e9..=3.5e9).contains(f) {
                let (z0, gl) = line_from_s(sm, 50.0);
                z += z0.re;
                ee += eps_eff(gl.im / 0.020, *f);
                n += 1.0;
            }
        }
        ee_runs.push((z / n, ee / n));
    }
    let ((_, ee0), (z1, ee1)) = (ee_runs[0], ee_runs[1]);
    println!(
        "masked: Z0 {z1:.2} vs {:.2}, εeff {ee1:.4} vs {:.4}; bare εeff {ee0:.4} vs {:.4}",
        fs.z0, fs.er_eff, bare.er_eff
    );
    assert!((z1 - fs.z0).abs() < 0.015 * fs.z0, "Z0 {z1} vs {}", fs.z0);
    assert!((ee1 - fs.er_eff).abs() < 0.005 * fs.er_eff, "εeff {ee1} vs {}", fs.er_eff);
    // The mask's own effect (3 % of εeff) against the solver's: the bare line's +0.4 % bias
    // (D59) is a sixth of it.
    let (d_fdtd, d_fs) = (ee1 - ee0, fs.er_eff - bare.er_eff);
    assert!(d_fs > 0.0 && (d_fdtd - d_fs).abs() < 0.3 * d_fs, "Δεeff {d_fdtd} vs {d_fs}");
}
