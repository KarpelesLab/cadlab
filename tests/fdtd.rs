//! The built-in FDTD solver (`cadlab::fdtd`, `sim.sparams`) against analytic results: a
//! microstrip's Z₀ and εeff against the 2D field solver (D49), its dielectric loss against the
//! perturbation model (D52), cavity resonances and Q, energy conservation, the Courant limit and
//! thread-count independence.
//!
//! Small meshes run in debug; the finer validations are `#[ignore]`: run them with
//! `cargo test --release --test fdtd -- --ignored`.

use std::f64::consts::PI;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::electrical::Model;
use cadlab::electrical::complex::C64;
use cadlab::electrical::fieldsolver::{self, Accuracy};
use cadlab::fdtd::engine::{C0, EPS0, Gaussian, Hooks, LumpedPort, Material, Probe, Settings, Simulation, Structure};
use cadlab::openems::mesh;
use serde_json::{Value, json};

const UM: i64 = 1_000;
const MM: i64 = 1_000_000;

fn c(re: f64) -> C64 {
    C64::new(re, 0.0)
}

fn uniform(cells: i64, d: i64) -> Vec<i64> {
    (0..=cells).map(|i| i * d).collect()
}

fn settings(pml: [usize; 6], f0: f64, fc: f64, freqs: Vec<f64>, max_steps: u64) -> Settings {
    Settings { pml, courant: 0.95, max_steps, end_db: 40.0, excitation: Gaussian { f0, fc }, freqs, threads: 0 }
}

// ---------------------------------------------------------------------------------------------
// Microstrip from probes along the line.

/// A microstrip along x over a ground plane (the z = 0 wall), running through the absorbing
/// layers at both ends, excited by a 50 Ω lumped source near the start; voltage probes at
/// `center − d`, `center`, `center + d` and current loops on both sides of `center`.
struct Line {
    sim: Simulation,
    freqs: Vec<f64>,
    d: f64,
}

struct Spec {
    er: f64,
    tan: f64,
    h: i64,
    w: i64,
    /// Cells across the strip, in the substrate; cell size along the line.
    across: i64,
    in_sub: i64,
    dx: i64,
    length: i64,
    f_max: f64,
}

fn microstrip(s: &Spec) -> Line {
    microstrip_on(s, 0, 40_000)
}

fn microstrip_on(s: &Spec, threads: usize, max_steps: u64) -> Line {
    let (h, w) = (s.h, s.w);
    let fine = w / s.across;
    let y = 10 * h + w / 2;
    // The thirds rule at the strip's edges (as `sim.sparams` meshes).
    let (a, b) = (w / 2 - fine / 3, w / 2 + 2 * fine / 3);
    let ys = mesh::smooth(&[-y, -b, -a, a, b, y], &|lo, hi| if lo >= -b && hi <= b { fine } else { h }, 1.3);
    let zs = mesh::smooth(&[0, h, 10 * h], &|lo, _| if lo < h { h / s.in_sub } else { h }, 1.3);
    let xs = uniform(s.length / s.dx, s.dx);
    let mut st = Structure::new([xs, ys, zs]).unwrap();
    let f0 = s.f_max / 2.0;
    let sub = st.material(Material { eps_r: s.er, sigma: 2.0 * PI * f0 * EPS0 * s.er * s.tan });
    st.fill(sub, [0, -y, 0], [s.length, y, h], &|_, _| true);
    st.pec([0, -y, h], [s.length, y, h], &|_, yy| (-w / 2..=w / 2).contains(&yy));
    let n = st.n();
    let feed = 12;
    let (j0, j1) = (st.node(1, -w / 2), st.node(1, w / 2));
    let kh = st.node(2, h);
    let port = LumpedPort { axis: 2, lo: [feed, j0, 0], hi: [feed, j1, kh], positive_high: true, r: 50.0 };
    let center = (n[0] + feed) / 2;
    let dn = (n[0] - 10 - center).min(center - feed - 2) / 2;
    let jm = st.node(1, 0);
    let v = |i: usize| Probe::Voltage { axis: 2, from: [i, jm, kh], to: 0 };
    let probes = vec![
        v(center - dn),
        v(center),
        v(center + dn),
        Probe::Current { axis: 0, at: center - 1, lo: [j0, kh], hi: [j1, kh] },
        Probe::Current { axis: 0, at: center, lo: [j0, kh], hi: [j1, kh] },
    ];
    let freqs: Vec<f64> = (1..=10).map(|k| k as f64 * s.f_max / 10.0).collect();
    let set = Settings { threads, ..settings([6, 6, 6, 6, 0, 6], f0, f0, freqs.clone(), max_steps) };
    let sim = Simulation::new(&st, &[port], probes, set).unwrap();
    Line { sim, freqs, d: (dn as i64 * s.dx) as f64 * 1e-9 }
}

/// Propagation constant and characteristic impedance per frequency from the probes:
/// V(−d) + V(d) = 2 cosh(γd) V(0), and V⁺ − V⁻ = (V(−d) − V(d)) / (2 sinh γd) = Z₀ I(0).
fn extract(line: &mut Line) -> Vec<(f64, C64, C64)> {
    let run = line.sim.run(Some(0), &Hooks::none()).unwrap();
    assert!(run.converged, "{} steps, {} dB", run.steps, run.energy_db);
    let p = &run.probes;
    (0..line.freqs.len())
        .map(|f| {
            let (vm, v0, vp) = (p[0][f], p[1][f], p[2][f]);
            let i0 = (p[3][f] + p[4][f]) * c(0.5);
            let ch = (vm + vp) / (v0 * c(2.0));
            let mut gd = (ch + (ch * ch - c(1.0)).sqrt()).ln();
            // The forward wave e^(−γx): β > 0.
            if gd.im < 0.0 {
                gd = c(0.0) - gd;
            }
            let sh = (gd.exp() - (c(0.0) - gd).exp()) * c(0.5);
            let z0 = (vm - vp) / (sh * c(2.0)) / i0;
            (line.freqs[f], gd * c(1.0 / line.d), z0)
        })
        .collect()
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

/// Dielectric attenuation (Np/m) of a zero-thickness microstrip by the perturbation method of
/// D52: α = G Z₀ / 2, G = ω tan δ · εr ∂C/∂εr (the derivative by a central difference).
fn alpha_d(er: f64, tan: f64, h: f64, w: f64, f: f64) -> f64 {
    let e = 1e-3;
    let dc = (field_solver(er * (1.0 + e), h, w).c - field_solver(er * (1.0 - e), h, w).c) / (2.0 * e);
    let line = field_solver(er, h, w);
    2.0 * PI * f * tan * dc * line.z0 / 2.0
}

fn eps_eff(beta: f64, f: f64) -> f64 {
    (beta * C0 / (2.0 * PI * f)).powi(2)
}

fn check_line(spec: &Spec, z_tol: f64, e_tol: f64) {
    let mut line = microstrip(spec);
    let r = extract(&mut line);
    let (h, w) = (spec.h as f64 / 1e6, spec.w as f64 / 1e6);
    let fs = field_solver(spec.er, h, w);
    // Quasi-static range: up to 30 % of f_max (dispersion raises εeff above it). Lossless only:
    // a constant σ makes Z₀ complex at low frequencies.
    for &(f, g, z) in r.iter().filter(|x| x.0 <= 0.3 * spec.f_max && spec.tan == 0.0) {
        let ee = eps_eff(g.im, f);
        assert!((z.re - fs.z0).abs() < z_tol * fs.z0, "{f} Hz: Z0 {z:?} vs {}", fs.z0);
        assert!((ee - fs.er_eff).abs() < e_tol * fs.er_eff, "{f} Hz: εeff {ee} vs {}", fs.er_eff);
    }
    if spec.tan > 0.0 {
        // σ is exact at f0: the attenuation there.
        let f0 = spec.f_max / 2.0;
        let (_, g, _) = r.iter().find(|x| (x.0 - f0).abs() < 1.0).unwrap();
        let ad = alpha_d(spec.er, spec.tan, h, w, f0);
        assert!((g.re - ad).abs() < 0.1 * ad, "α {} vs {ad} Np/m", g.re);
    }
}

/// Zero-thickness microstrip, W/H 2 in εr 4, four cells across: Z₀ and εeff from the probes
/// against the 2D field solver.
#[test]
fn microstrip_matches_field_solver() {
    let spec = Spec {
        er: 4.0,
        tan: 0.0,
        h: 500 * UM,
        w: 1000 * UM,
        across: 4,
        in_sub: 4,
        dx: 500 * UM,
        length: 22 * MM,
        f_max: 10e9,
    };
    check_line(&spec, 0.02, 0.015);
}

/// Eight cells across the strip and in the substrate, and a lossy laminate (tan δ 0.02): Z₀
/// within 1 %, εeff within 1 %, the attenuation within 10 % of the perturbation model.
#[test]
#[ignore]
fn microstrip_fine_with_loss() {
    let spec = Spec {
        er: 4.0,
        tan: 0.0,
        h: 500 * UM,
        w: 1000 * UM,
        across: 8,
        in_sub: 8,
        dx: 100 * UM,
        length: 30 * MM,
        f_max: 10e9,
    };
    check_line(&spec, 0.01, 0.01);
    check_line(&Spec { tan: 0.02, ..spec }, 0.01, 0.01);
}

// ---------------------------------------------------------------------------------------------
// Cavities: resonances, Q, energy, stability.

/// A 20 × 12 × 0.5 mm box of perfect conductor filled with `er` (σ `sigma`), meshed in 0.5 mm
/// cells (one in z): its TM_mn0 modes have E along z only. A high-impedance source near a corner
/// and a voltage probe elsewhere.
fn cavity(er: f64, sigma: f64, s: Settings) -> Simulation {
    let mut st = Structure::new([uniform(40, 500 * UM), uniform(24, 500 * UM), uniform(1, 500 * UM)]).unwrap();
    let m = st.material(Material { eps_r: er, sigma });
    st.fill(m, [0, 0, 0], [20 * MM, 12 * MM, 500 * UM], &|_, _| true);
    let port = LumpedPort { axis: 2, lo: [5, 4, 0], hi: [5, 4, 1], positive_high: true, r: 1e9 };
    Simulation::new(&st, &[port], vec![Probe::Voltage { axis: 2, from: [27, 15, 1], to: 0 }], s).unwrap()
}

/// f_mn of the box: c / (2√εr) √((m/a)² + (n/b)²).
fn tm(m: f64, n: f64, er: f64) -> f64 {
    C0 / (2.0 * er.sqrt()) * ((m / 0.020).powi(2) + (n / 0.012).powi(2)).sqrt()
}

/// The frequency of the largest |x| near `guess`, refined by a parabola through the three
/// samples around it.
fn peak(freqs: &[f64], x: &[C64]) -> f64 {
    let k = (1..x.len() - 1).max_by(|&a, &b| x[a].abs().total_cmp(&x[b].abs())).unwrap();
    let (a, b, cc) = (x[k - 1].abs(), x[k].abs(), x[k + 1].abs());
    let shift = 0.5 * (a - cc) / (a - 2.0 * b + cc);
    freqs[k] + shift * (freqs[k + 1] - freqs[k])
}

#[test]
fn cavity_resonances() {
    let er = 4.4;
    let modes = [(1.0, 1.0), (2.0, 1.0)];
    let mut freqs = Vec::new();
    for &(m, n) in &modes {
        let f = tm(m, n, er);
        freqs.extend((-40..=40).map(|k| f * (1.0 + k as f64 * 1e-3)));
    }
    let mut sim = cavity(er, 0.0, settings([0; 6], 8e9, 3e9, freqs.clone(), 30_000));
    let run = sim.run(Some(0), &Hooks::none()).unwrap();
    assert!(!run.converged, "a lossless cavity rings");
    for (i, &(m, n)) in modes.iter().enumerate() {
        let span = i * 81..(i + 1) * 81;
        let f = peak(&freqs[span.clone()], &run.probes[0][span]);
        let want = tm(m, n, er);
        // Numerical dispersion at 31 cells per wavelength lowers it by about 0.2 %.
        assert!((f - want).abs() < 0.004 * want, "TM{m}{n}: {f} vs {want}");
    }
}

/// A lossy filling (σ = ω₀ ε tan δ at the mode) gives Q = 1 / tan δ: the energy of the ringing
/// mode decays as e^(−ω₀t/Q).
#[test]
fn cavity_q_from_loss_tangent() {
    let (er, tan) = (4.4, 0.01);
    let f = tm(1.0, 1.0, er);
    let sigma = 2.0 * PI * f * EPS0 * er * tan;
    let s = settings([0; 6], f, 1e9, vec![], 0);
    let g = s.excitation;
    let mut sim = cavity(er, sigma, s);
    let dt = sim.dt();
    let mut n = 0u64;
    while (n as f64) * dt < g.end() {
        sim.step(Some((0, g.at((n as f64 + 0.5) * dt))));
        n += 1;
    }
    // Least-squares slope of ln W over 3 ns (smooths the E/H ripple of staggered samples).
    let (mut sx, mut sy, mut sxx, mut sxy, mut m) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let steps = (3e-9 / dt) as u64;
    for k in 0..steps {
        sim.step(None);
        let (x, y) = (k as f64 * dt, sim.energy().ln());
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
        m += 1.0;
    }
    let slope = (m * sxy - sx * sy) / (m * sxx - sx * sx);
    let q = 2.0 * PI * f / -slope;
    assert!((q - 1.0 / tan).abs() < 0.03 / tan, "Q {q}");
}

/// Lossless, on a graded mesh with two dielectrics: after the pulse the energy stays constant.
#[test]
fn energy_is_conserved() {
    let xs = mesh::smooth(&[0, 3 * MM, 4 * MM, 12 * MM], &|a, _| if a == 3 * MM { 200 * UM } else { MM }, 1.4);
    let ys = mesh::smooth(&[0, 5 * MM, 8 * MM], &|a, _| if a == 0 { 400 * UM } else { MM }, 1.4);
    let zs = mesh::smooth(&[0, MM, 3 * MM], &|a, _| if a == 0 { 250 * UM } else { 700 * UM }, 1.4);
    let mut st = Structure::new([xs, ys, zs]).unwrap();
    let m = st.material(Material { eps_r: 4.0, sigma: 0.0 });
    st.fill(m, [0, 0, 0], [12 * MM, 8 * MM, MM], &|_, _| true);
    st.pec([2 * MM, 2 * MM, MM], [9 * MM, 4 * MM, MM], &|_, _| true);
    let n = st.n();
    let port = LumpedPort { axis: 2, lo: [3, 3, 0], hi: [3, 3, st.node(2, MM)], positive_high: true, r: 1e9 };
    let s = settings([0; 6], 10e9, 8e9, vec![], 0);
    let g = s.excitation;
    let mut sim = Simulation::new(&st, &[port], vec![], s).unwrap();
    assert!(n.iter().all(|&k| k > 4));
    let dt = sim.dt();
    let mut k = 0u64;
    while (k as f64) * dt < g.end() {
        sim.step(Some((0, g.at((k as f64 + 0.5) * dt))));
        k += 1;
    }
    let w0 = sim.energy();
    assert!(w0 > 0.0);
    let (mut lo, mut hi) = (w0, w0);
    for k in 0..6_000 {
        sim.step(None);
        if k % 10 != 0 {
            continue;
        }
        let w = sim.energy();
        lo = lo.min(w);
        hi = hi.max(w);
    }
    assert!(hi < 1.01 * w0 && lo > 0.99 * w0, "energy {lo} .. {hi} from {w0}");
}

/// Above the Courant limit the update diverges; below it, it does not.
#[test]
fn courant_limit() {
    let run = |courant: f64| {
        let st = Structure::new([uniform(12, MM), uniform(12, MM), uniform(12, MM)]).unwrap();
        let port = LumpedPort { axis: 2, lo: [4, 5, 3], hi: [4, 5, 6], positive_high: true, r: 1e9 };
        let s = Settings { courant, ..settings([0; 6], 20e9, 15e9, vec![], 0) };
        let g = s.excitation;
        let mut sim = Simulation::new(&st, &[port], vec![], s).unwrap();
        let dt = sim.dt();
        let mut peak = 0.0f64;
        for k in 0..3000u64 {
            let t = (k as f64 + 0.5) * dt;
            sim.step(Some((0, if t < g.end() { g.at(t) } else { 0.0 })));
            if k % 50 == 0 {
                peak = peak.max(sim.energy());
            }
        }
        let w = sim.energy();
        (w, peak)
    };
    let (w, peak) = run(0.99);
    assert!(w.is_finite() && w <= peak * 1.01, "stable at 0.99: {w} vs peak {peak}");
    let (w, peak) = run(1.03);
    assert!(!w.is_finite() || w > 1e6 * peak, "diverges at 1.03: {w} vs {peak}");
}

/// The same run on one thread and on three gives bit-identical spectra.
#[test]
fn threads_do_not_change_results() {
    let spec = Spec {
        er: 3.0,
        tan: 0.01,
        h: 500 * UM,
        w: 1000 * UM,
        across: 2,
        in_sub: 4,
        dx: 500 * UM,
        length: 20 * MM,
        f_max: 8e9,
    };
    let ra = microstrip_on(&spec, 1, 200).sim.run(Some(0), &Hooks::none()).unwrap();
    let rb = microstrip_on(&spec, 3, 200).sim.run(Some(0), &Hooks::none()).unwrap();
    assert_eq!(ra.steps, rb.steps);
    for (x, y) in ra.probes.iter().flatten().zip(rb.probes.iter().flatten()) {
        assert_eq!((x.re.to_bits(), x.im.to_bits()), (y.re.to_bits(), y.im.to_bits()));
    }
}

// ---------------------------------------------------------------------------------------------
// Board level: `sim.sparams` on a routed microstrip.

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
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
    let o = exec(&r, &mut s, "sim.sparams", args.clone());
    let x = &o["output"];
    assert!(o["diagnostics"].as_array().unwrap().is_empty(), "{}", o["diagnostics"]);
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
        assert!(s21.abs() > 0.7 && s21.abs() < 1.0, "{f}: |S21| {}", s21.abs());
        assert!((s12 - s21).abs() < 0.01, "{f}: reciprocity {s12:?} {s21:?}");
    }

    // Guards and errors.
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

/// A 24 mm microstrip, 1 mm wide on 0.5 mm of εr 4 with tan δ 0.02, between microstrip ports:
/// Z₀ and εeff from the S-parameters against the field solver, a matched line's |S11| and its
/// attenuation against the perturbation model of D52.
#[test]
#[ignore]
fn sim_sparams_microstrip_validation() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = board_line(&r, "0.5mm", "4", "0.02", "1mm", 24);
    let out = dir.path().join("out");
    let o = exec(
        &r,
        &mut s,
        "sim.sparams",
        sim_args(
            &out,
            id,
            json!({"f_max": "10GHz", "points": 21, "margin": "5mm", "air": "5mm", "max_cell": "0.5mm", "pml_cells": 6}),
        ),
    );
    assert!(o["output"]["runs"].as_array().unwrap().iter().all(|r| r["converged"] == true));
    let ts = cadlab::touchstone::parse(&std::fs::read_to_string(out.join("line.s2p")).unwrap(), Some(2)).unwrap();
    let fs = field_solver(4.0, 0.5, 1.0);
    // The reference planes are 2 mm inside each end.
    let l = 0.020;
    let f0 = 5e9;
    let ad = alpha_d(4.0, 0.02, 0.5, 1.0, f0);
    let (mut alpha, mut s21_db, mut count) = (0.0, 0.0, 0.0);
    for (f, m) in ts.freqs.iter().zip(&ts.data) {
        let (z0, gl) = line_from_s(m, 50.0);
        if (1e9..=3.5e9).contains(f) {
            let ee = eps_eff(gl.im / l, *f);
            assert!((z0.re - fs.z0).abs() < 0.02 * fs.z0, "{f}: Z0 {z0:?} vs {}", fs.z0);
            assert!((ee - fs.er_eff).abs() < 0.02 * fs.er_eff, "{f}: εeff {ee} vs {}", fs.er_eff);
        }
        if (0.5e9..=9.5e9).contains(f) {
            assert!(c64(&m[0]).abs() < 0.04, "{f}: |S11| {}", c64(&m[0]).abs());
        }
        // A constant σ keeps G, so the dielectric attenuation, constant over frequency: the
        // band's mean against the model at f0.
        if (1e9..=9e9).contains(f) {
            alpha += gl.re.abs() / l;
            s21_db += 20.0 * c64(&m[2]).abs().log10();
            count += 1.0;
        }
    }
    let (alpha, s21_db) = (alpha / count, s21_db / count);
    assert!((alpha - ad).abs() < 0.12 * ad, "α {alpha} vs {ad} Np/m");
    let want = -20.0 * std::f64::consts::LOG10_E * ad * l;
    assert!((s21_db - want).abs() < 0.05, "|S21| {s21_db} dB vs {want} dB");
}
