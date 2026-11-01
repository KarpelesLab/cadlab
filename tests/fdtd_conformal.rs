//! Conformal copper in the FDTD solver (`cadlab::fdtd::conformal`, DECISIONS D73): cells cut by
//! diagonal and curved copper against analytic resonances, the 2D field solver and D52's loss, the
//! staircase on the same mesh alongside; stability and thread-count independence.
//!
//! Small meshes run in debug; the validations that print the tables of docs/ELECTRICAL.md are
//! `#[ignore]`: `cargo test --release --test fdtd_conformal -- --ignored --nocapture
//! --test-threads=1`.

use std::f64::consts::{PI, SQRT_2};

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::electrical::Model;
use cadlab::electrical::complex::C64;
use cadlab::electrical::fieldsolver::{self, Accuracy, LossTangents};
use cadlab::electrical::loss::{self, Conductor, Roughness};
use cadlab::fdtd::conformal::{self, Prism, Stats};
use cadlab::fdtd::engine::{
    C0, Gaussian, Hooks, LumpedElement, LumpedPort, Material, Probe, Settings, Simulation, Structure,
};
use cadlab::fdtd::fit;
use cadlab::geom::poly::{self, Polygon};
use cadlab::openems::mesh;
use serde_json::{Value, json};

const UM: i64 = 1_000;
const MM: i64 = 1_000_000;

fn c(re: f64) -> C64 {
    C64::new(re, 0.0)
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

fn polygon(points: Vec<(i64, i64)>) -> Polygon {
    Polygon::new(points.into_iter().map(|(x, y)| poly::Point::new(x, y)).collect::<Vec<_>>(), vec![])
}

/// A strip of width `w` and length `2 √2 e` along y = x.
fn strip(w: i64, e: i64) -> Polygon {
    let o = (w as f64 / SQRT_2 / 2.0).round() as i64;
    polygon(vec![(-e + o, -e - o), (e + o, e - o), (e - o, e + o), (-e - o, -e + o)])
}

fn inside(shape: &Polygon) -> impl Fn(i64, i64) -> bool + '_ {
    move |x, y| poly::locate(std::slice::from_ref(shape), poly::Point::new(x, y)) != poly::Location::Outside
}

/// A regular polygon with the area of a circle of radius `r`.
fn circle(r: i64, n: usize) -> Vec<poly::Point> {
    let t = 2.0 * PI / n as f64;
    let rr = r as f64 * (t / t.sin()).sqrt();
    (0..n)
        .map(|k| {
            let a = t * (k as f64 + 0.5);
            poly::Point::new((rr * a.cos()).round() as i64, (rr * a.sin()).round() as i64)
        })
        .collect()
}

fn centered(half: i64, d: i64) -> Vec<i64> {
    let n = half / d;
    (-n..=n).map(|i| i * d).collect()
}

fn microstrip(er: f64, h: i64, t: i64) -> fieldsolver::Structure {
    fieldsolver::Structure {
        model: Model::Microstrip,
        h: h as f64 / 1e6,
        er,
        h2: 0.0,
        er2: er,
        t: t as f64 / 1e6,
        etch: 0.0,
        broadside: None,
        coplanar_gap: None,
        mask: None,
    }
}

// ---------------------------------------------------------------------------------------------
// A 45° microstrip: voltage probes along the diagonal, current through a plane x = constant (all
// of the line's current crosses it).

struct Line {
    w: i64,
    h: i64,
    t: i64,
    er: f64,
    d: i64,
    in_sub: i64,
    half: i64,
    conformal: bool,
}

/// (f, γ, Z₀) per frequency of a zero-thickness strip over the z = 0 wall, through the absorbing
/// layers at both ends.
fn diagonal_line(s: &Line, freqs: &[f64]) -> Vec<(f64, C64, C64)> {
    let (h, d, half) = (s.h, s.d, s.half);
    let fine = h / s.in_sub;
    let zs = mesh::smooth(&[0, h, 10 * h], &|lo, _| if lo < h { fine } else { h }, 1.3);
    let xs = centered(half, d);
    let mut st = Structure::new([xs.clone(), xs, zs]).unwrap();
    let sub = st.material(Material { eps_r: s.er, sigma: 0.0 });
    st.fill(sub, [-half, -half, 0], [half, half, h], &|_, _| true);
    let shape = strip(s.w, 2 * half);
    st.pec([-half, -half, h], [half, half, h], &inside(&shape));
    if s.conformal {
        conformal::apply(&mut st, &[Prism { shape: vec![shape.clone()], z: (h, h) }]);
    }
    let n = st.n();
    let c0 = n[0] / 2;
    let kh = st.node(2, h);
    let feed = 12usize.max(n[0] / 6);
    let port = LumpedPort { axis: 2, lo: [feed, feed, 0], hi: [feed, feed, kh], positive_high: true, r: 50.0 };
    let dn = ((n[0] - 10 - c0).min(c0 - feed - 2)) / 2;
    let v = |i: usize| Probe::Voltage { axis: 2, from: [i, i, kh], to: 0 };
    let span = (s.w as f64 * SQRT_2 / 2.0 / d as f64).ceil() as usize + 1;
    let probes = vec![
        v(c0 - dn),
        v(c0),
        v(c0 + dn),
        Probe::Current { axis: 0, at: c0 - 1, lo: [c0 - span, kh], hi: [c0 + span, kh] },
        Probe::Current { axis: 0, at: c0, lo: [c0 - span, kh], hi: [c0 + span, kh] },
    ];
    let set = Settings { end_db: 60.0, ..settings([6, 6, 6, 6, 0, 6], 5e9, 5e9, freqs.to_vec(), 100_000) };
    let mut sim = Simulation::new(&st, &[port], probes, set).unwrap();
    let run = sim.run(Some(0), &Hooks::none()).unwrap();
    assert!(run.converged, "{} steps {} dB", run.steps, run.energy_db);
    let dist = dn as f64 * d as f64 * 1e-9 * SQRT_2;
    let p = &run.probes;
    (0..freqs.len())
        .map(|f| {
            let (vm, v0, vp) = (p[0][f], p[1][f], p[2][f]);
            let i0 = (p[3][f] + p[4][f]) * c(0.5);
            let ch = (vm + vp) / (v0 * c(2.0));
            let mut gd = (ch + (ch * ch - c(1.0)).sqrt()).ln();
            if gd.im < 0.0 {
                gd = c(0.0) - gd;
            }
            let sh = (gd.exp() - (c(0.0) - gd).exp()) * c(0.5);
            (freqs[f], gd * c(1.0 / dist), (vm - vp) / (sh * c(2.0)) / i0)
        })
        .collect()
}

/// A 45° zero-thickness 50 Ω microstrip against the 2D field solver, 2–8 GHz. The staircase's
/// delay error converges only as the first power of the cell.
#[test]
#[ignore]
fn diagonal_microstrip_matches_field_solver() {
    let (w, h, er) = (1000 * UM, 500 * UM, 4.0);
    let l = microstrip(er, h, 0).line(w as f64 / 1e6, Accuracy::default()).unwrap();
    println!("field solver: Z0 {:.2}, εeff {:.3}", l.z0, l.er_eff);
    let freqs = [2e9, 3e9, 5e9, 8e9];
    for (d, in_sub) in [(500, 4), (250, 8), (125, 8)] {
        let mut worst = [(0.0f64, 0.0f64); 2];
        for (m, conf) in [false, true].into_iter().enumerate() {
            let s = Line { w, h, t: 0, er, d: d * UM, in_sub, half: 11 * MM, conformal: conf };
            let t = std::time::Instant::now();
            let r = diagonal_line(&s, &freqs);
            for (f, g, z) in &r {
                let e = (g.im * C0 / (2.0 * PI * f)).powi(2);
                let (dz, de) = (100.0 * (z.re / l.z0 - 1.0), 100.0 * (e / l.er_eff - 1.0));
                print!(" {:.0}G Z {dz:+.1}% e {de:+.1}%", f / 1e9);
                worst[m] = (worst[m].0.max(dz.abs()), worst[m].1.max(de.abs()));
            }
            println!(" | {d} um conformal {conf} [{:.0} s]", t.elapsed().as_secs_f64());
        }
        assert!(worst[1].1 < worst[0].1, "{d} um: εeff {worst:?}");
        if d == 125 {
            assert!(worst[1].0 < 3.0 && worst[1].1 < 3.0, "{d} um: {worst:?}");
        }
    }
}

/// The 45° line's conductor loss from Q: an open-ended λ/2 strip, ringing, perfect copper taken
/// out (radiation), against β/(2α) of D52.
#[test]
#[ignore]
fn diagonal_resonator_conductor_q() {
    let (h, t, w, er, sigma) = (500 * UM, 35 * UM, 1000 * UM, 4.0, 5.8e5);
    let tans = LossTangents::default();
    let m = microstrip(er, h, t).line_matrices(w as f64 / 1e6, None, false, &tans, Accuracy::default()).unwrap();
    let cond = Conductor { sigma, roughness: Roughness::Smooth, r_dc: 0.0 };
    let f = C0 / (2.0 * 25e-3 * 3.07f64.sqrt());
    let p = loss::at(&m, f, &cond);
    let want = p.beta / (2.0 * p.alpha);
    let mut errors = [0.0; 2];
    for (k, conf) in [false, true].into_iter().enumerate() {
        let s = Line { w, h, t, er, d: 250 * UM, in_sub: 4, half: 16 * MM, conformal: conf };
        let (q, q0) = (resonator_q(&s, 24 * MM, Some(sigma), f), resonator_q(&s, 24 * MM, None, f));
        let qc = 1.0 / (1.0 / q - 1.0 / q0);
        errors[k] = 100.0 * (qc / want - 1.0);
        println!("conformal {conf}: Q {q:.1}, Q0 {q0:.1}, Qc {qc:.1} vs {want:.1} ({:+.1} %)", errors[k]);
    }
    assert!(errors[1].abs() < 8.0 && errors[0] > 25.0, "{errors:?}");
}

fn resonator_q(s: &Line, length: i64, sigma: Option<f64>, f: f64) -> f64 {
    let (h, t, d, half) = (s.h, s.t, s.d, s.half);
    let gap = 100 * UM;
    let top = h + t;
    let fine = h / s.in_sub;
    let zs = mesh::smooth(
        &[-t - gap, -t, 0, h, top, top + 8 * h],
        &|lo, hi| if lo >= 0 && hi <= top + h { fine } else { h },
        1.3,
    );
    let xs = centered(half, d);
    let mut st = Structure::new([xs.clone(), xs, zs]).unwrap();
    let sub = st.material(Material { eps_r: s.er, sigma: 0.0 });
    st.fill(sub, [-half, -half, 0], [half, half, h], &|_, _| true);
    st.pec([-half, -half, -t - gap], [half, half, 0], &|_, _| true);
    let e = (length as f64 / 2.0 / SQRT_2) as i64;
    let shape = strip(s.w, e);
    st.pec([-half, -half, h], [half, half, top], &inside(&shape));
    if s.conformal {
        conformal::apply(&mut st, &[Prism { shape: vec![shape.clone()], z: (h, top) }]);
    }
    let (kg, kh) = (st.node(2, 0), st.node(2, h));
    let i = st.node(0, -e);
    let port = LumpedPort { axis: 2, lo: [i, i, kg], hi: [i, i, kh], positive_high: true, r: 1e8 };
    let surface = sigma.map(|sg| fit::surface_fit(sg, &Roughness::Smooth, 1e8, 10e9));
    let g = Gaussian { f0: f, fc: f / 4.0 };
    let set = Settings { surface, ..settings([6, 6, 6, 6, 0, 6], g.f0, g.fc, vec![], 0) };
    let mut sim = Simulation::new(&st, &[port], vec![], set).unwrap();
    let dt = sim.dt();
    let mut n = 0u64;
    while (n as f64) * dt < g.end() + 2.0 / f {
        let v = if (n as f64) * dt < g.end() { g.at((n as f64 + 0.5) * dt) } else { 0.0 };
        sim.step(Some((0, v)));
        n += 1;
    }
    decay_q(&mut sim, f, 6.0, 8)
}

/// Q from the least-squares decay of the energy over `periods` of `f`, sampled every `every` steps.
fn decay_q(sim: &mut Simulation, f: f64, periods: f64, every: u64) -> f64 {
    let dt = sim.dt();
    let (mut sx, mut sy, mut sxx, mut sxy, mut m) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for k in 0..(periods / f / dt) as u64 {
        sim.step(None);
        if k % every == 0 {
            let (x, y) = (k as f64 * dt, sim.energy().ln());
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
            m += 1.0;
        }
    }
    let slope = (m * sxy - sx * sy) / (m * sxx - sx * sx);
    2.0 * PI * f / -slope
}

// ---------------------------------------------------------------------------------------------
// Cylindrical cavity: TM_mn0 at x_mn c / (2π a), Q = a/δ with only the curved wall lossy.

/// A closed cylinder (the plates are the domain's walls): `a` over a cell `d`, z cells of 0.5 mm.
fn cylinder(a: i64, d: i64, cut: bool) -> Structure {
    let xs = centered(a + 3 * d, d);
    let mut st = Structure::new([xs.clone(), xs, vec![0, 500 * UM, 1000 * UM]]).unwrap();
    st.pec([-a - 3 * d, -a - 3 * d, 0], [a + 3 * d, a + 3 * d, 1000 * UM], &|x, y| {
        (x as i128).pow(2) + (y as i128).pow(2) >= (a as i128).pow(2)
    });
    if cut {
        let e = 2 * a;
        let hull =
            vec![poly::Point::new(-e, -e), poly::Point::new(e, -e), poly::Point::new(e, e), poly::Point::new(-e, e)];
        conformal::apply(
            &mut st,
            &[Prism { shape: vec![Polygon::new(hull, vec![circle(a, 720).into()])], z: (0, 1000 * UM) }],
        );
    }
    st
}

/// The frequency of the strongest peak near `guess` in a Hann-windowed spectrum.
fn tone(x: &[f64], dt: f64, guess: f64, span: f64) -> f64 {
    let n = x.len();
    let df = 0.1 / (n as f64 * dt);
    let m = (span * guess / df) as i64;
    let mag = |f: f64| {
        let (mut re, mut im) = (0.0, 0.0);
        let w = 2.0 * PI * f * dt;
        for (i, v) in x.iter().enumerate() {
            let win = (PI * i as f64 / n as f64).sin().powi(2);
            re += v * win * (w * i as f64).cos();
            im += v * win * (w * i as f64).sin();
        }
        (re * re + im * im).sqrt()
    };
    let vals: Vec<f64> = (-m..=m).map(|k| mag(guess + k as f64 * df)).collect();
    let k = (1..vals.len() - 1).max_by(|&a, &b| vals[a].total_cmp(&vals[b])).unwrap();
    let (a, b, cc) = (vals[k - 1].ln(), vals[k].ln(), vals[k + 1].ln());
    guess + (k as f64 - m as f64 + 0.5 * (a - cc) / (a - 2.0 * b + cc)) * df
}

/// The driven pulse, then the ringing at probe 0 up to `total` seconds.
fn ring(sim: &mut Simulation, g: Gaussian, total: f64) -> Vec<f64> {
    let dt = sim.dt();
    let mut n = 0u64;
    while (n as f64) * dt < g.end() {
        sim.step(Some((0, g.at((n as f64 + 0.5) * dt))));
        n += 1;
    }
    let mut x = Vec::new();
    while (n as f64) * dt < total {
        sim.step(None);
        x.push(sim.probe(0));
        n += 1;
    }
    x
}

fn cylinder_modes(a: i64, d: i64, cut: bool, total: f64, modes: &[f64]) -> Vec<f64> {
    let st = cylinder(a, d, cut);
    let (i, j) = (st.node(0, 3 * a / 10), st.node(1, a / 5));
    let port = LumpedPort { axis: 2, lo: [i, j, 0], hi: [i, j, 2], positive_high: true, r: 1e9 };
    let (pi, pj) = (st.node(0, -2 * a / 5), st.node(1, 3 * a / 10));
    let g = Gaussian { f0: 18e9, fc: 14e9 };
    let probes = vec![Probe::Voltage { axis: 2, from: [pi, pj, 0], to: 2 }];
    let mut sim = Simulation::new(&st, &[port], probes, settings([0; 6], g.f0, g.fc, vec![], 0)).unwrap();
    let x = ring(&mut sim, g, total);
    let want = |m: f64| m * C0 / (2.0 * PI * a as f64 * 1e-9);
    modes.iter().map(|&m| 100.0 * (tone(&x, sim.dt(), want(m), 0.06) / want(m) - 1.0)).collect()
}

const TM010: f64 = 2.404826;
const TM110: f64 = 3.831706;
const TM020: f64 = 5.520078;

/// Debug-sized: at ten cells per radius the staircase is 2.4 % low, conformal cells within 0.5 %.
#[test]
fn cylinder_resonance_small() {
    let stair = cylinder_modes(10 * MM, MM, false, 8e-9, &[TM010]);
    let cut = cylinder_modes(10 * MM, MM, true, 8e-9, &[TM010]);
    assert!(stair[0] < -1.5, "staircase {stair:?}");
    assert!(cut[0].abs() < 0.5, "conformal {cut:?}");
}

/// TM010, TM110, TM020 at a/10 to a/80: first order on the staircase, second on conformal cells.
#[test]
#[ignore]
fn cylinder_resonances() {
    for d in [1000, 500, 250, 125] {
        let modes = [TM010, TM110, TM020];
        let (s, k) = (
            cylinder_modes(10 * MM, d * UM, false, 40e-9, &modes),
            cylinder_modes(10 * MM, d * UM, true, 40e-9, &modes),
        );
        println!("a/d {}: staircase {s:+.2?} %, conformal {k:+.2?} %", 10_000 / d);
        for (a, b) in s.iter().zip(&k) {
            assert!(b.abs() < a.abs(), "a/d {}: {s:?} vs {k:?}", 10_000 / d);
        }
    }
}

/// Only the curved wall lossy: Q = a/δ (any TM_mn0). The staircase's surface is 4/π too long.
#[test]
#[ignore]
fn cylinder_wall_q() {
    let (a, sigma) = (10 * MM, 5.8e5);
    let f = TM010 * C0 / (2.0 * PI * a as f64 * 1e-9);
    let want = a as f64 * 1e-9 / loss::skin_depth(f, sigma);
    for d in [1000, 500, 250] {
        let mut e = [0.0; 2];
        for (k, conf) in [false, true].into_iter().enumerate() {
            let st = cylinder(a, d * UM, conf);
            let (i, j) = (st.node(0, 0), st.node(1, 0));
            let port = LumpedPort { axis: 2, lo: [i, j, 0], hi: [i, j, 2], positive_high: true, r: 1e9 };
            let surface = Some(fit::surface_fit(sigma, &Roughness::Smooth, 1e9, 3e10));
            let g = Gaussian { f0: f, fc: f / 5.0 };
            let set = Settings { surface, ..settings([0; 6], g.f0, g.fc, vec![], 0) };
            let mut sim = Simulation::new(&st, &[port], vec![], set).unwrap();
            let dt = sim.dt();
            let mut n = 0u64;
            while (n as f64) * dt < g.end() {
                sim.step(Some((0, g.at((n as f64 + 0.5) * dt))));
                n += 1;
            }
            let q = decay_q(&mut sim, f, 60.0, 1);
            e[k] = 100.0 * (q / want - 1.0);
        }
        println!("a/d {}: Q staircase {:+.1} %, conformal {:+.1} % (of {want:.0})", 10_000 / d, e[0], e[1]);
        assert!(e[1].abs() < 6.0 && e[0] < -15.0, "{e:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// A round patch, open: TM11 converges to one value with and without conformal cells.

fn patch_tm11(d: i64, cut: bool) -> f64 {
    let (a, h, er) = (5 * MM, 500 * UM, 4.0);
    let half = a + 6 * MM;
    let xs = centered(half, d);
    let in_sub = 8;
    let zs = mesh::smooth(&[0, h, 10 * h], &|lo, _| if lo < h { h / in_sub } else { h }, 1.3);
    let mut st = Structure::new([xs.clone(), xs, zs]).unwrap();
    let sub = st.material(Material { eps_r: er, sigma: 0.0 });
    st.fill(sub, [-half, -half, 0], [half, half, h], &|_, _| true);
    let disc = Polygon::new(circle(a, 360), vec![]);
    st.pec([-half, -half, h], [half, half, h], &inside(&disc));
    if cut {
        conformal::apply(&mut st, &[Prism { shape: vec![disc.clone()], z: (h, h) }]);
    }
    let kh = st.node(2, h);
    let (i, j) = (st.node(0, 3 * MM), st.node(1, MM));
    let port = LumpedPort { axis: 2, lo: [i, j, 0], hi: [i, j, kh], positive_high: true, r: 1e9 };
    let (pi, pj) = (st.node(0, -3 * MM), st.node(1, 0));
    let g = Gaussian { f0: 8.5e9, fc: 6e9 };
    let set = settings([6, 6, 6, 6, 0, 6], g.f0, g.fc, vec![], 0);
    let probes = vec![Probe::Voltage { axis: 2, from: [pi, pj, kh], to: 0 }];
    let mut sim = Simulation::new(&st, &[port], probes, set).unwrap();
    let x = ring(&mut sim, g, 5e-9);
    tone(&x, sim.dt(), 8.3e9, 0.15)
}

/// Round patch TM11, printed for docs/ELECTRICAL.md (the closed form is good to about 2.5 %).
#[test]
#[ignore]
fn round_patch_tm11() {
    // Wolff & Knoppik's effective radius (Electron. Lett. 10, 1974).
    let (a, h, er) = (5e-3f64, 5e-4f64, 4.0f64);
    let ae = a * (1.0 + 2.0 * h / (PI * a * er) * ((PI * a / (2.0 * h)).ln() + 1.7726)).sqrt();
    let wolff = 1.84118 * C0 / (2.0 * PI * ae * er.sqrt());
    println!("Wolff & Knoppik: {:.3} GHz", wolff / 1e9);
    for d in [1000, 500, 250] {
        let (s, k) = (patch_tm11(d * UM, false), patch_tm11(d * UM, true));
        println!("a/d {}: staircase {:.3} GHz, conformal {:.3} GHz", 5000 / d, s / 1e9, k / 1e9);
    }
}

// ---------------------------------------------------------------------------------------------
// Stability, thread independence, compatibility.

/// A closed box with a dielectric, a diagonal strip and a disc, cut cells everywhere.
fn cut_box(cells: i64, d: i64) -> Structure {
    let e = cells * d / 2;
    let xs = centered(e, d);
    let zs: Vec<i64> = (0..=6).map(|k| k * d / 2).collect();
    let mut st = Structure::new([xs.clone(), xs, zs]).unwrap();
    let sub = st.material(Material { eps_r: 4.3, sigma: 0.0 });
    st.fill(sub, [-e, -e, 0], [e, e, d], &|_, _| true);
    let line = strip(3 * d / 2, 2 * e);
    let pad = Polygon::new(
        circle(2 * d, 64).iter().map(|p| poly::Point::new(p.x + e / 2, p.y - e / 2)).collect::<Vec<_>>(),
        vec![],
    );
    st.pec([-e, -e, d], [e, e, d], &inside(&line));
    st.pec([-e, -e, d], [e, e, d + d / 2], &inside(&pad));
    conformal::apply(
        &mut st,
        &[Prism { shape: vec![line.clone()], z: (d, d) }, Prism { shape: vec![pad.clone()], z: (d, d + d / 2) }],
    );
    st
}

fn cut_box_energy(steps: u64) {
    let st = cut_box(30, 500 * UM);
    let port = LumpedPort { axis: 2, lo: [8, 22, 0], hi: [8, 22, 2], positive_high: true, r: 1e9 };
    let g = Gaussian { f0: 10e9, fc: 10e9 };
    let mut sim = Simulation::new(&st, &[port], vec![], settings([0; 6], g.f0, g.fc, vec![], 0)).unwrap();
    assert!(sim.conformal_counts().0 > 0);
    let radius = sim.curl_curl_radius(300);
    println!("Δt² λmax {radius:.3} (4: the leapfrog limit)");
    assert!(radius < 4.0, "Δt² λmax {radius}");
    let dt = sim.dt();
    let mut n = 0u64;
    while (n as f64) * dt < g.end() {
        sim.step(Some((0, g.at((n as f64 + 0.5) * dt))));
        n += 1;
    }
    let reference = sim.energy();
    let (mut lo, mut hi) = (f64::INFINITY, 0.0f64);
    for _ in 0..steps {
        sim.step(None);
        let w = sim.energy();
        lo = lo.min(w);
        hi = hi.max(w);
    }
    println!(
        "{steps} steps: energy {:+.3} % .. {:+.3} %",
        100.0 * (lo / reference - 1.0),
        100.0 * (hi / reference - 1.0)
    );
    assert!(lo > 0.9 * reference && hi < 1.1 * reference, "{lo} {hi} of {reference}");
}

#[test]
fn cut_cells_conserve_energy() {
    cut_box_energy(1500);
}

/// Long ringing on cut cells: no growth.
#[test]
#[ignore]
fn cut_cells_conserve_energy_long() {
    cut_box_energy(300_000);
}

/// Everything at once on a diagonal strip: CPML, ports, a Debye dielectric, surface impedance and
/// a lumped element.
fn full_model(threads: usize, steps: u64) -> cadlab::fdtd::engine::Run {
    let (d, h, e) = (250 * UM, 500 * UM, 6 * MM);
    let xs = centered(e, d);
    let zs: Vec<i64> = (-1..=15).map(|k| k * h / 2).collect();
    let mut st = Structure::new([xs.clone(), xs, zs]).unwrap();
    let taus = fit::relaxation_times(1e9, 1e10);
    st.set_relaxation(taus.clone());
    let model = cadlab::electrical::dispersion::Dielectric { er: 4.3, tan_d: 0.02, ref_freq: Some(1e9) };
    let f = fit::debye_fit(&model.model(5e9), &taus, 1e9, 1e10);
    let sub = st.debye_material(Material { eps_r: f.eps_inf, sigma: f.sigma }, f.delta);
    st.fill(sub, [-e, -e, 0], [e, e, h], &|_, _| true);
    st.pec([-e, -e, -h / 2], [e, e, 0], &|_, _| true);
    let line = strip(MM, 2 * e);
    st.pec([-e, -e, h], [e, e, h], &inside(&line));
    conformal::apply(&mut st, &[Prism { shape: vec![line.clone()], z: (h, h) }]);
    let (kg, kh) = (st.node(2, 0), st.node(2, h));
    let (a, b, m) = (st.node(0, -3 * MM), st.node(0, 3 * MM), st.node(0, MM));
    // A shunt R–L–C from the strip to the plane, on a node of the strip.
    st.element(LumpedElement { axis: 2, lo: [m, m, kg], hi: [m, m, kh], r: 10.0, l: 1e-9, c: Some(1e-12) });
    // A series resistor on an edge the strip's outline cuts (x from 500 to 750 µm at y = 0; the
    // outline crosses at 707 µm): the element takes the whole edge.
    let (i, j) = (st.node(0, 500 * UM), st.node(1, 0));
    st.element(LumpedElement { axis: 0, lo: [i, j, kh], hi: [i + 1, j, kh], r: 100.0, l: 0.0, c: None });
    let port = |i: usize| LumpedPort { axis: 2, lo: [i, i, kg], hi: [i, i, kh], positive_high: true, r: 50.0 };
    let probes = vec![Probe::Voltage { axis: 2, from: [b, b, kh], to: kg }];
    let set = Settings {
        surface: Some(fit::surface_fit(5.8e7, &Roughness::Smooth, 1e9, 1e10)),
        threads,
        end_db: 60.0,
        ..settings([6, 6, 6, 6, 0, 6], 5e9, 4e9, vec![2e9, 4e9, 6e9], steps)
    };
    let mut sim = Simulation::new(&st, &[port(a), port(b)], probes, set).unwrap();
    assert!(sim.conformal_counts().0 > 0 && sim.surface_loops().0 > 0);
    assert!(sim.dispersive_edges().iter().sum::<usize>() > 0 && sim.element_edges() > 0);
    sim.run(Some(0), &Hooks::none()).unwrap()
}

/// Bit-identical for any thread count, with everything on.
#[test]
fn threads_do_not_change_conformal_results() {
    let (a, b) = (full_model(1, 300), full_model(3, 300));
    for (x, y) in a.probes.iter().chain(&a.port_v).flatten().zip(b.probes.iter().chain(&b.port_v).flatten()) {
        assert_eq!(x.re.to_bits(), y.re.to_bits());
        assert_eq!(x.im.to_bits(), y.im.to_bits());
    }
}

/// Everything on, long enough for the pulse to leave: the energy falls and nothing grows.
#[test]
#[ignore]
fn full_model_decays() {
    let r = full_model(0, 40_000);
    println!("{} steps: {:.1} dB, converged {}", r.steps, r.energy_db, r.converged);
    assert!(r.converged, "{} dB", r.energy_db);
}

/// Axis-aligned copper keeps its staircase, untouched: same spectra to the bit.
#[test]
fn axis_aligned_copper_is_untouched() {
    let run = |cut: bool| {
        let (d, e) = (500 * UM, 5 * MM);
        let xs = centered(e, d);
        let mut st = Structure::new([xs.clone(), xs, vec![0, 500 * UM, MM, 2 * MM]]).unwrap();
        let rect = polygon(vec![(-3 * MM, -700 * UM), (3 * MM, -700 * UM), (3 * MM, 700 * UM), (-3 * MM, 700 * UM)]);
        st.pec([-e, -e, MM], [e, e, MM], &inside(&rect));
        if cut {
            let stats = conformal::apply(&mut st, &[Prism { shape: vec![rect.clone()], z: (MM, MM) }]);
            assert_eq!(stats, Stats::default());
        }
        let port = LumpedPort { axis: 2, lo: [5, 10, 0], hi: [5, 10, 2], positive_high: true, r: 50.0 };
        let probes = vec![Probe::Voltage { axis: 2, from: [14, 10, 2], to: 0 }];
        let set = settings([0; 6], 10e9, 10e9, vec![3e9, 9e9], 600);
        Simulation::new(&st, &[port], probes, set).unwrap().run(Some(0), &Hooks::none()).unwrap()
    };
    let (a, b) = (run(false), run(true));
    assert_eq!(a.probes, b.probes);
}

// ---------------------------------------------------------------------------------------------
// Through `sim.sparams`.

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
