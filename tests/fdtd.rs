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
use cadlab::electrical::dispersion::Dielectric as Dispersive;
use cadlab::electrical::fieldsolver::{self, Accuracy, LossTangents};
use cadlab::electrical::loss::{self, COPPER_CONDUCTIVITY, Conductor, Roughness};
use cadlab::fdtd::engine::{C0, EPS0, Gaussian, Hooks, LumpedPort, Material, Probe, Settings, Simulation, Structure};
use cadlab::fdtd::fit;
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
    Settings {
        pml,
        courant: 0.95,
        max_steps,
        end_db: 40.0,
        excitation: Gaussian { f0, fc },
        freqs,
        threads: 0,
        surface: None,
    }
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
// Conductor loss (surface impedance) and dispersive dielectrics (Djordjevic–Sarkar).

/// A microstrip with copper of thickness `t` on the strip and the ground plane (0: sheets),
/// the plane over a thin air gap above the bottom wall: dielectric `er` / `tan`
/// (Djordjevic–Sarkar through them at `ref_freq`, else at the pulse's center), copper of
/// conductivity `sigma` (surface impedance) or perfect.
#[derive(Clone)]
struct Lossy {
    er: f64,
    tan: f64,
    ref_freq: Option<f64>,
    h: i64,
    w: i64,
    t: i64,
    across: i64,
    in_sub: i64,
    dx: i64,
    length: i64,
    f_max: f64,
    sigma: Option<f64>,
    rough: Roughness,
}

impl Default for Lossy {
    fn default() -> Self {
        Lossy {
            er: 4.0,
            tan: 0.0,
            ref_freq: None,
            h: 500 * UM,
            w: 1000 * UM,
            t: 35 * UM,
            across: 8,
            in_sub: 8,
            dx: 200 * UM,
            length: 30 * MM,
            f_max: 10e9,
            sigma: Some(COPPER_CONDUCTIVITY),
            rough: Roughness::Smooth,
        }
    }
}

/// A material for `er` / `tan` (Djordjevic–Sarkar at `ref_freq` or `anchor`) fitted over
/// `lo .. hi`, as `sim.sparams` builds it.
fn debye(st: &mut Structure, er: f64, tan: f64, ref_freq: Option<f64>, anchor: f64, lo: f64, hi: f64) -> u16 {
    if tan == 0.0 {
        return st.material(Material { eps_r: er, sigma: 0.0 });
    }
    let taus = fit::relaxation_times(lo, hi);
    st.set_relaxation(taus.clone());
    let d = Dispersive { er, tan_d: tan, ref_freq };
    let f = fit::debye_fit(&d.model(anchor), &taus, lo, hi);
    st.debye_material(Material { eps_r: f.eps_inf, sigma: f.sigma }, f.delta)
}

fn lossy_line(s: &Lossy, threads: usize, max_steps: u64, end_db: f64) -> Line {
    let (h, w, t) = (s.h, s.w, s.t);
    let fine = w / s.across;
    let y = 10 * h + w / 2;
    let (a, b) = (w / 2 - fine / 3, w / 2 + 2 * fine / 3);
    let ys = mesh::smooth(&[-y, -b, -a, a, b, y], &|lo, hi| if lo >= -b && hi <= b { fine } else { h / 2 }, 1.3);
    let gap = 100 * UM;
    let top = h + t;
    let mut zf = vec![-t - gap, -t, 0, h, top, top + 10 * h];
    zf.dedup();
    let zs = mesh::smooth(&zf, &|lo, hi| if lo >= 0 && hi <= top + h { h / s.in_sub } else { h }, 1.3);
    let xs = uniform(s.length / s.dx, s.dx);
    let mut st = Structure::new([xs, ys, zs]).unwrap();
    let (f0, lo) = (s.f_max / 2.0, s.f_max / 100.0);
    let m = debye(&mut st, s.er, s.tan, s.ref_freq, f0, lo, s.f_max);
    st.fill(m, [0, -y, 0], [s.length, y, h], &|_, _| true);
    // The ground plane down to the wall (no field below it), and the strip.
    st.pec([0, -y, -t - gap], [s.length, y, 0], &|_, _| true);
    st.pec([0, -y, h], [s.length, y, top], &|_, yy| (-w / 2..=w / 2).contains(&yy));
    let n = st.n();
    let feed = 12;
    let (j0, j1) = (st.node(1, -w / 2), st.node(1, w / 2));
    let (kg, kh, kt) = (st.node(2, 0), st.node(2, h), st.node(2, top));
    let port = LumpedPort { axis: 2, lo: [feed, j0, kg], hi: [feed, j1, kh], positive_high: true, r: 50.0 };
    let center = (n[0] + feed) / 2;
    let dn = (n[0] - 10 - center).min(center - feed - 2) / 2;
    let jm = st.node(1, 0);
    let v = |i: usize| Probe::Voltage { axis: 2, from: [i, jm, kh], to: kg };
    let probes = vec![
        v(center - dn),
        v(center),
        v(center + dn),
        Probe::Current { axis: 0, at: center - 1, lo: [j0, kh], hi: [j1, kt] },
        Probe::Current { axis: 0, at: center, lo: [j0, kh], hi: [j1, kt] },
    ];
    let freqs: Vec<f64> = (1..=20).map(|k| k as f64 * s.f_max / 20.0).collect();
    let surface = s.sigma.map(|sg| fit::surface_fit(sg, &s.rough, lo, s.f_max));
    let set = Settings { threads, surface, end_db, ..settings([6, 6, 6, 6, 0, 6], f0, f0, freqs.clone(), max_steps) };
    let sim = Simulation::new(&st, &[port], probes, set).unwrap();
    Line { sim, freqs, d: (dn as i64 * s.dx) as f64 * 1e-9 }
}

/// The line's matrices from the field solver (D52), with the same dielectric model.
fn d52_matrices(s: &Lossy) -> cadlab::electrical::fieldsolver::LineMatrices {
    let st = fieldsolver::Structure {
        model: Model::Microstrip,
        h: s.h as f64 / 1e6,
        er: s.er,
        h2: 0.0,
        er2: s.er,
        t: s.t as f64 / 1e6,
        etch: 0.0,
        broadside: None,
        coplanar_gap: None,
        mask: None,
    };
    let tans = LossTangents { below: s.tan, ref_freq: [s.ref_freq, None, None], ..LossTangents::default() };
    st.line_matrices(s.w as f64 / 1e6, None, false, &tans, Accuracy::default()).unwrap()
}

/// A parallel-plate cavity: `a × b` mm, 0.5 mm of a material between two copper plates (each a
/// slab from the domain's wall), meshed in 40 × 24 × `nz` cells, a high-impedance source near a
/// corner and a voltage probe elsewhere. Its TM_mn0 modes have E along z only.
fn plate_cavity(a: i64, b: i64, nz: i64, mat: &dyn Fn(&mut Structure) -> u16, s: Settings) -> Simulation {
    let (h, gap) = (500 * UM, 250 * UM);
    let mut zs = vec![-gap];
    zs.extend((0..=nz).map(|k| k * h / nz));
    zs.push(h + gap);
    let mut st = Structure::new([uniform(40, a / 40), uniform(24, b / 24), zs]).unwrap();
    let m = mat(&mut st);
    st.fill(m, [0, 0, 0], [a, b, h], &|_, _| true);
    st.pec([0, 0, -gap], [a, b, 0], &|_, _| true);
    st.pec([0, 0, h], [a, b, h + gap], &|_, _| true);
    let port = LumpedPort { axis: 2, lo: [5, 4, 1], hi: [5, 4, 1 + nz as usize], positive_high: true, r: 1e9 };
    Simulation::new(&st, &[port], vec![Probe::Voltage { axis: 2, from: [27, 15, 1], to: 1 + nz as usize }], s).unwrap()
}

/// TM_mn0 of an `a × b` mm box in vacuum.
fn tm_vac(m: f64, n: f64, a: i64, b: i64) -> f64 {
    C0 / 2.0 * ((m / (a as f64 * 1e-9)).powi(2) + (n / (b as f64 * 1e-9)).powi(2)).sqrt()
}

/// Q of the ringing mode from the decay of the field energy after a narrow pulse at `f`.
fn ringing_q(sim: &mut Simulation, f: f64, periods: f64) -> f64 {
    let g = Gaussian { f0: f, fc: f / 5.0 };
    let dt = sim.dt();
    let mut n = 0u64;
    while (n as f64) * dt < g.end() {
        sim.step(Some((0, g.at((n as f64 + 0.5) * dt))));
        n += 1;
    }
    // Least-squares slope of ln W (smooths the E/H ripple of staggered samples).
    let (mut sx, mut sy, mut sxx, mut sxy, mut m) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for k in 0..(periods / f / dt) as u64 {
        sim.step(None);
        let (x, y) = (k as f64 * dt, sim.energy().ln());
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
        m += 1.0;
    }
    let slope = (m * sxy - sx * sy) / (m * sxx - sx * sx);
    2.0 * PI * f / -slope
}

/// The cavity's Q with copper plates of conductivity σ (surface impedance, fitted over 0.1–20 GHz)
/// is h/δ: the two plates carry the same current, the side walls are lossless; a rough surface
/// lowers it by its factor K.
fn check_surface_q(a: i64, b: i64, rough: Roughness) {
    let er = 4.4;
    let lossless = |st: &mut Structure| st.material(Material { eps_r: er, sigma: 0.0 });
    let f = tm_vac(1.0, 1.0, a, b) / er.sqrt();
    let surface = Some(fit::surface_fit(COPPER_CONDUCTIVITY, &rough, 1e8, 2e10));
    let s = Settings { surface, ..settings([0; 6], f, f / 5.0, vec![], 0) };
    let mut sim = plate_cavity(a, b, 1, &lossless, s);
    let q = ringing_q(&mut sim, f, 15.0);
    let delta = loss::skin_depth(f, COPPER_CONDUCTIVITY);
    let want = 500e-6 / delta / rough.factor(delta);
    println!("{rough:?} {f:.4e} Hz: Q {q:.1} vs {want:.1}");
    assert!((q - want).abs() < 0.01 * want, "{rough:?} {f} Hz: Q {q} vs {want}");
}

/// Surface impedance: the cavity's Q at 6.9 GHz, smooth and with Hammerstad–Jensen roughness.
#[test]
fn surface_impedance_cavity_q() {
    check_surface_q(20 * MM, 12 * MM, Roughness::Smooth);
    check_surface_q(20 * MM, 12 * MM, Roughness::HammerstadJensen { rms: 1e-6 });
}

/// The same at 1.4 and 14 GHz, and with Huray's roughness.
#[test]
#[ignore]
fn surface_impedance_cavity_q_wideband() {
    check_surface_q(100 * MM, 60 * MM, Roughness::Smooth);
    check_surface_q(10 * MM, 6 * MM, Roughness::Smooth);
    check_surface_q(20 * MM, 12 * MM, Roughness::Huray { radius: 0.5e-6, ratio: 2.0 });
}

/// A Djordjevic–Sarkar filling (εr 4, tan δ 0.02 at 1 GHz, Debye terms fitted over `band`): the
/// mode resonates where ω √εr(ω) is the vacuum mode, with Q = 1 / tan δ(f).
fn check_dispersive_cavity(band: (f64, f64), a: i64, b: i64) {
    let d = Dispersive { er: 4.0, tan_d: 0.02, ref_freq: Some(1e9) };
    let fill = |st: &mut Structure| debye(st, d.er, d.tan_d, d.ref_freq, 0.0, band.0, band.1);
    let fv = tm_vac(1.0, 1.0, a, b);
    // f = f_vac / √εr(f), by fixed point.
    let mut f = fv / 2.0;
    for _ in 0..20 {
        f = fv / d.at(f).0.sqrt();
    }
    let freqs: Vec<f64> = (-40..=40).map(|k| f * (1.0 + k as f64 * 1e-3)).collect();
    let s = settings([0; 6], f, f / 5.0, freqs.clone(), 20_000);
    let mut sim = plate_cavity(a, b, 1, &fill, s.clone());
    let run = sim.run(Some(0), &Hooks::none()).unwrap();
    let fr = peak(&freqs, &run.probes[0]);
    println!("{band:?} {f:.4e} Hz: resonance {:+.2} %", 100.0 * (fr / f - 1.0));
    // Numerical dispersion at 40 × 24 cells lowers it by about 0.1 %.
    assert!((fr - f).abs() < 0.003 * f, "{a}: resonance {fr} vs {f}");
    let mut sim = plate_cavity(a, b, 1, &fill, Settings { freqs: vec![], ..s });
    let q = ringing_q(&mut sim, f, 10.0);
    let want = 1.0 / d.at(f).1;
    println!("{band:?} {f:.4e} Hz: Q {q:.2} vs {want:.2}");
    assert!((q - want).abs() < 0.01 * want, "{a}: Q {q} vs {want}");
}

/// Debye terms fitted over 0.1–10 GHz, as `sim.sparams` does for `f_max` 10 GHz: the mode at
/// 2.4 GHz.
#[test]
fn dispersive_cavity_modes() {
    check_dispersive_cavity((1e8, 1e10), 60 * MM, 36 * MM);
}

/// The same at 9 GHz, and with a fit over 0.5–20 GHz at 2.4 and 15 GHz.
#[test]
#[ignore]
fn dispersive_cavity_modes_wideband() {
    check_dispersive_cavity((1e8, 1e10), 16 * MM, 10 * MM);
    check_dispersive_cavity((5e8, 2e10), 60 * MM, 36 * MM);
    check_dispersive_cavity((5e8, 2e10), 10 * MM, 6 * MM);
}

/// The surface impedance on a microstrip with 35 µm copper (strip and plane) against the D52
/// conductor loss of the field solver: the attenuation the conductivity adds (the same line
/// with perfect conductors subtracted, which removes the probes' systematic error), 3 to
/// 8 GHz, at 4 and 8 cells across the strip. A low conductivity (580 kS/m) makes the loss large
/// enough to measure on 6 mm of line; Rs scales out of the comparison.
#[test]
#[ignore]
fn conductor_loss_matches_d52() {
    for across in [8, 4] {
        let spec = Lossy { sigma: Some(5.8e5), across, in_sub: across, ..Lossy::default() };
        let r = extract(&mut lossy_line(&spec, 0, 100_000, 40.0));
        let r0 = extract(&mut lossy_line(&Lossy { sigma: None, ..spec.clone() }, 0, 100_000, 40.0));
        let m = d52_matrices(&spec);
        let cond = Conductor { sigma: 5.8e5, roughness: Roughness::Smooth, r_dc: 0.0 };
        let (mut sum, mut count) = (0.0, 0.0);
        for (&(f, g, _), &(_, g0, _)) in r.iter().zip(&r0).filter(|x| (3e9..=8e9).contains(&x.0.0)) {
            let want = loss::at(&m, f, &cond).alpha;
            let got = g.re - g0.re;
            println!("{across} across, {f:.3e} Hz: α {got:.4} vs {want:.4} ({:+.1} %)", 100.0 * (got / want - 1.0));
            assert!((got - want).abs() < 0.08 * want, "{across} across, {f} Hz: α {got} vs {want}");
            sum += got / want;
            count += 1.0;
        }
        assert!((sum / count - 1.0).abs() < 0.05, "{across} across: mean ratio {}", sum / count);
    }
}

/// A Djordjevic–Sarkar laminate (εr 4, tan δ 0.02 at 1 GHz) under a microstrip with perfect
/// conductors, against the field solver with the same model (D52): εeff(f) from 1 to 5 GHz (it
/// falls with εr(f)), and the attenuation from 3 to 6 GHz on average (the probes' own error is
/// about ±0.1 Np/m, the size of the attenuation below 3 GHz).
#[test]
#[ignore]
fn dispersive_microstrip_matches_d52() {
    let spec = Lossy { tan: 0.02, ref_freq: Some(1e9), sigma: None, ..Lossy::default() };
    let r = extract(&mut lossy_line(&spec, 0, 100_000, 60.0));
    let m = d52_matrices(&spec);
    let cond = Conductor { sigma: f64::INFINITY, roughness: Roughness::Smooth, r_dc: 0.0 };
    let (mut sum, mut count) = (0.0, 0.0);
    for &(f, g, _) in r.iter().filter(|x| (1e9..=6e9).contains(&x.0)) {
        let p = loss::at(&m, f, &cond);
        let ee = eps_eff(g.im, f);
        let want_ee = (p.beta / (2.0 * PI * f) * C0).powi(2);
        println!("{f:.3e} Hz: εeff {ee:.4} vs {want_ee:.4}, α {:.4} vs {:.4}", g.re, p.alpha);
        if f <= 5e9 {
            assert!((ee - want_ee).abs() < 0.012 * want_ee, "{f}: εeff {ee} vs {want_ee}");
        }
        if f >= 3e9 {
            sum += g.re / p.alpha;
            count += 1.0;
        }
    }
    assert!((sum / count - 1.0).abs() < 0.05, "mean α ratio {}", sum / count);
}

/// With the surface impedance and Debye terms (their own parallel passes), one thread and
/// three give bit-identical spectra.
#[test]
fn threads_do_not_change_lossy_results() {
    let spec = Lossy {
        tan: 0.02,
        ref_freq: Some(1e9),
        across: 2,
        in_sub: 2,
        dx: 500 * UM,
        length: 20 * MM,
        rough: Roughness::HammerstadJensen { rms: 1e-6 },
        ..Lossy::default()
    };
    let ra = lossy_line(&spec, 1, 200, 40.0).sim.run(Some(0), &Hooks::none()).unwrap();
    let rb = lossy_line(&spec, 3, 200, 40.0).sim.run(Some(0), &Hooks::none()).unwrap();
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
    let m = d52_matrices(&Lossy { tan: 0.02, ref_freq: Some(1e9), ..Lossy::default() });
    let cond = Conductor {
        sigma: COPPER_CONDUCTIVITY,
        roughness: Roughness::Smooth,
        r_dc: 1.0 / (COPPER_CONDUCTIVITY * 1e-3 * 35e-6),
    };
    // The reference planes are 2 mm inside each end.
    let l = 0.060;
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
            assert!((z0.re - p.z0.re).abs() < 0.03 * p.z0.re, "{f}: Z0 {z0:?} vs {:?}", p.z0);
            assert!((ee - want_ee).abs() < 0.015 * want_ee, "{f}: εeff {ee} vs {want_ee}");
        }
        if (0.5e9..=9.5e9).contains(f) {
            assert!(c64(&sm[0]).abs() < 0.04, "{f}: |S11| {}", c64(&sm[0]).abs());
            let s21_db = 20.0 * c64(&sm[2]).abs().log10();
            let want_db = -20.0 * std::f64::consts::LOG10_E * p.alpha * l;
            // −9 % to +6 %: the residual of this open-region measurement (the same line with a
            // constant conductivity, whose loss is flat, measures 0.30–0.39 Np/m over 0.6–4 GHz).
            let tol = if *f < 2.5e9 { 0.10 } else { 0.06 };
            println!("{f}: |S21| {s21_db:.4} vs {want_db:.4} dB ({:+.1} %)", 100.0 * (s21_db / want_db - 1.0));
            assert!((s21_db - want_db).abs() < tol * want_db.abs(), "{f}: |S21| {s21_db} vs {want_db} dB");
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
