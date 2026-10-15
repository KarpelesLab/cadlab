//! Built-in 3D full-wave solver (FDTD): S-parameters of a board region, `sim.sparams`. See
//! `docs/ELECTRICAL.md`, "Full-wave solver".
//!
//! The region, its geometry and its graded mesh come from the openEMS export
//! ([`crate::openems`]): the same stack, copper, holes, ports and mesh lines, so both describe
//! the same simulation. [`engine`] is the Yee time stepping (written from the textbooks, no code
//! of any other solver used); this module turns the region into voxels, places the ports and
//! probes, runs once per port and assembles the S-parameters into a Touchstone file.
//!
//! - Copper: perfect-conductor edges inside the copper volume (`solid`) or on the sheet
//!   (`sheet`); plated holes and vias are metal cylinders. With a conductivity, a surface
//!   impedance boundary condition on every copper surface gives the skin-effect loss (and
//!   roughness), fitted over the band ([`fit::surface_fit`]).
//! - Dielectrics: the Djordjevic–Sarkar model through the stackup's εr and tan δ at their
//!   reference frequency (at the pulse's center f₀ when the stackup gives none), as Debye terms
//!   on shared relaxation times ([`fit::debye_fit`]) updated by auxiliary differential
//!   equations; edges average the permittivity of the cells around them. Solder mask (an
//!   option) is a dielectric layer over the outer layers.
//! - Lumped ports are resistive voltage sources of the reference impedance, from the reference
//!   copper to the signal copper; microstrip ports feed the same source on the line and measure
//!   voltage and current on it at their reference plane.
//! - Absorbing boundaries: CPML in the outer `pml_cells` cells of every side.
//! - S = B A⁻¹ from the incident and reflected power waves of every port in every run, so
//!   terminations that are not exactly matched do not bias the result.

pub mod engine;
pub mod fit;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::diag::Diagnostic;
use crate::electrical::complex::C64;
use crate::geom::Point;
use crate::geom::poly::{self, Location};
use crate::model::Project;
use crate::openems::{self, MeshInfo, PortInfo, PortKind, Region};
use crate::refs::ObjectRef;
use crate::touchstone::{self, Touchstone};
use crate::units::Nm;

pub use engine::{
    Cancelled, Gaussian, Hooks, LumpedPort, Material, Probe, Run, Settings, Simulation, Structure, SurfaceImpedance,
};

use crate::electrical::dispersion::Dielectric as Dispersive;
use crate::electrical::loss::Roughness;

/// Simulation options: the region and mesh as for the openEMS export, plus the solver's limits.
#[derive(Clone, Debug)]
pub struct Options {
    /// Region, ports, frequencies, mesh (`boundary` is ignored: always CPML of `pml_cells`).
    pub region: openems::Options,
    /// Largest mesh accepted (cells).
    pub max_cells: u64,
    /// Timestep as a fraction of the Courant limit.
    pub courant: f64,
    /// Copper conductivity (S/m) for the surface impedance; `None`: perfect conductors.
    pub conductivity: Option<f64>,
    /// Copper surface roughness (on the surface impedance's resistance).
    pub roughness: Roughness,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            region: openems::Options::default(),
            max_cells: 4_000_000,
            courant: 0.95,
            conductivity: Some(crate::electrical::loss::COPPER_CONDUCTIVITY),
            roughness: Roughness::Smooth,
        }
    }
}

/// A failure: a stable code, a message, the objects involved and a hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Stable code (`sim.too_many_cells`, `openems.no_region`, ...).
    pub code: &'static str,
    /// What is wrong.
    pub message: String,
    /// How to fix it.
    pub hint: String,
    /// Objects involved.
    pub subjects: Vec<ObjectRef>,
    /// Close names, for unknown identifiers.
    pub suggestions: Vec<String>,
}

impl Error {
    fn new(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Error {
        Error { code, message: message.into(), hint: hint.into(), subjects: Vec::new(), suggestions: Vec::new() }
    }
}

impl From<openems::Error> for Error {
    fn from(e: openems::Error) -> Error {
        Error { code: e.code, message: e.message, hint: e.hint, subjects: e.subjects, suggestions: e.suggestions }
    }
}

/// One run (one excited port).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PortRun {
    /// The excited port.
    pub port: usize,
    /// Timesteps simulated.
    pub steps: u64,
    /// Whether the energy decayed by `end_criteria` (else the timestep limit stopped it).
    pub converged: bool,
    /// Final field energy relative to its peak (dB).
    pub energy_db: f64,
}

/// The result of a simulation.
#[derive(Clone, Debug)]
pub struct SParams {
    /// The S-parameters (reference impedances: the ports' `z0`).
    pub touchstone: Touchstone,
    /// Ports.
    pub ports: Vec<PortInfo>,
    /// Mesh summary.
    pub mesh: MeshInfo,
    /// The board region simulated (x, y).
    pub window: (Point, Point),
    /// Domain bottom and top (z, with the air).
    pub z_range: (Nm, Nm),
    /// Nets with copper in the region.
    pub nets: Vec<String>,
    /// Gaussian pulse center and 20 dB half-bandwidth (Hz).
    pub excitation: (u64, u64),
    /// Timestep (s).
    pub timestep: f64,
    /// One run per port.
    pub runs: Vec<PortRun>,
    /// Warnings and notes.
    pub diagnostics: Vec<Diagnostic>,
}

/// How a port is measured.
enum Measure {
    /// The lumped element's own voltage and current.
    Element,
    /// Voltage probe and the two current probes around the reference plane (probe indices), the
    /// weights interpolating their currents to the plane; the sign turns the probes' +axis
    /// current into the current towards the structure.
    Line { v: usize, i: [usize; 2], w: [f64; 2], sign: f64 },
}

/// The engine's view of a board region.
struct Built {
    structure: Structure,
    ports: Vec<LumpedPort>,
    measures: Vec<Measure>,
    probes: Vec<Probe>,
    z0: Vec<f64>,
}

fn pt(x: i64, y: i64) -> poly::Point {
    poly::Point::new(x, y)
}

fn port_error(code: &'static str, p: &openems::Port, message: String, hint: &str) -> Error {
    let mut e = Error::new(code, message, hint);
    e.subjects.push(ObjectRef::parse(&p.info.at).unwrap_or(ObjectRef::Name(p.info.at.clone())));
    e
}

/// A dielectric as Debye terms on the structure's relaxation times: the Djordjevic–Sarkar
/// model through its values (at `anchor` Hz when it has no reference frequency) fitted over
/// `band`; lossless dielectrics stay plain.
fn dielectric(s: &mut Structure, d: &Dispersive, anchor: f64, band: (f64, f64), taus: &[f64]) -> u16 {
    if d.tan_d <= 0.0 {
        return s.material(Material { eps_r: d.er, sigma: 0.0 });
    }
    let f = fit::debye_fit(&d.model(anchor), taus, band.0, band.1);
    s.debye_material(Material { eps_r: f.eps_inf, sigma: f.sigma }, f.delta)
}

/// Voxelizes the region: dielectrics (dispersive) inside the board outline, the solder mask,
/// non-plated holes as air, copper and plated holes as perfect conductors; ports and their
/// probes. `band`: the frequencies of the result (Hz).
fn build(r: &Region, band: (f64, f64)) -> Result<Built, Error> {
    let mut s = Structure::new(r.lines.clone())
        .map_err(|m| Error::new("sim.bad_mesh", m, "check `max_cell`, `min_cell` and the region"))?;
    let w = r.window;
    let on_board = |x: i64, y: i64| poly::locate(&r.board, pt(x, y)) != Location::Outside;
    let taus = fit::relaxation_times(band.0, band.1);
    s.set_relaxation(taus.clone());
    let f0 = r.f0 as f64;
    for d in &r.diels {
        let model = Dispersive {
            er: d.d.er.to_f64(),
            tan_d: d.d.loss_tangent.unwrap_or(crate::model::board::DEFAULT_LOSS_TANGENT).to_f64(),
            ref_freq: crate::electrical::ref_freq(&d.d),
        };
        let m = dielectric(&mut s, &model, f0, band, &taus);
        s.fill(m, [w.min.x, w.min.y, d.bottom], [w.max.x, w.max.y, d.top], &on_board);
    }
    if !r.masks.is_empty() {
        let (er, tan) = r.mask_material;
        let mask = dielectric(&mut s, &Dispersive::constant(er, tan), f0, band, &taus);
        for m in &r.masks {
            let span = |a: (i64, i64)| (a.0.min(a.1), a.0.max(a.1));
            let (a, b) = span(m.laminate);
            s.fill(mask, [w.min.x, w.min.y, a], [w.max.x, w.max.y, b], &on_board);
            let (lo, hi) = span(m.span);
            let (cu, over) = m.copper;
            let clear = if over > cu { (over, hi) } else { (lo, over) };
            for c in r.copper.iter().filter(|c| c.layer == m.layer) {
                let Some(bb) = c.shape.iter().filter_map(|q| q.bbox()).reduce(|a, b| a.union(&b)) else { continue };
                let inside = |x: i64, y: i64| poly::locate(&c.shape, pt(x, y)) != Location::Outside;
                if clear.1 > clear.0 {
                    s.fill(0, [bb.min.x, bb.min.y, clear.0], [bb.max.x, bb.max.y, clear.1], &inside);
                }
                let (a, b) = span(m.copper);
                s.fill(mask, [bb.min.x, bb.min.y, a], [bb.max.x, bb.max.y, b], &inside);
            }
            if let Some(bb) = m.openings.iter().filter_map(|q| q.bbox()).reduce(|a, b| a.union(&b)) {
                let inside = |x: i64, y: i64| poly::locate(&m.openings, pt(x, y)) != Location::Outside;
                s.fill(0, [bb.min.x, bb.min.y, lo], [bb.max.x, bb.max.y, hi], &inside);
            }
        }
    }
    let z_board = (
        r.layers.last().map_or(0, |l| l.bottom).min(r.diels.iter().map(|d| d.bottom).min().unwrap_or(0)),
        r.layers.first().map_or(0, |l| l.top),
    );
    let z_board =
        r.masks.iter().fold(z_board, |z, m| (z.0.min(m.span.0.min(m.span.1)), z.1.max(m.span.0.max(m.span.1))));
    let in_hole = |h: &openems::Hole, x: i64, y: i64| match &h.slot {
        Some(q) => poly::locate(std::slice::from_ref(q), pt(x, y)) != Location::Outside,
        None => {
            let (dx, dy) = ((x - h.center.x.0) as i128, (y - h.center.y.0) as i128);
            dx * dx + dy * dy <= (h.radius as i128) * (h.radius as i128)
        }
    };
    let hole_box = |h: &openems::Hole| match h.slot.as_ref().and_then(|q| q.bbox()) {
        Some(b) => (b.min.x, b.min.y, b.max.x, b.max.y),
        None => (h.center.x.0 - h.radius, h.center.y.0 - h.radius, h.center.x.0 + h.radius, h.center.y.0 + h.radius),
    };
    for h in r.holes.iter().filter(|h| !h.plated) {
        let (x0, y0, x1, y1) = hole_box(h);
        s.fill(0, [x0, y0, z_board.0], [x1, y1, z_board.1], &|x, y| in_hole(h, x, y));
    }
    for c in &r.copper {
        let l = &r.layers[c.layer];
        let Some(b) = c.shape.iter().filter_map(|q| q.bbox()).reduce(|a, b| a.union(&b)) else { continue };
        s.pec([b.min.x, b.min.y, l.bottom], [b.max.x, b.max.y, l.top], &|x, y| {
            poly::locate(&c.shape, pt(x, y)) != Location::Outside
        });
    }
    for h in r.holes.iter().filter(|h| h.plated) {
        let (z0, z1) = (r.layers[h.span.1].bottom, r.layers[h.span.0].top);
        let (x0, y0, x1, y1) = hole_box(h);
        s.pec([x0, y0, z0], [x1, y1, z1], &|x, y| in_hole(h, x, y));
        // A barrel thinner than the cells still connects its layers: the nearest node column.
        let (i, j) = (s.node(0, h.center.x.0), s.node(1, h.center.y.0));
        let (x, y) = (s.lines()[0][i], s.lines()[1][j]);
        s.pec([x, y, z0], [x, y, z1], &|_, _| true);
    }

    let mut ports = Vec::new();
    let mut measures = Vec::new();
    let mut probes = Vec::new();
    let mut z0 = Vec::new();
    let n = s.n();
    for p in &r.ports {
        z0.push(p.info.z0.to_f64());
        let node = |a: usize, v: i64| s.node(a, v);
        match p.info.kind {
            PortKind::Lumped => {
                let mut lo = [0; 3];
                let mut hi = [0; 3];
                for a in 0..3 {
                    let (u, v) = (node(a, p.start[a]), node(a, p.stop[a]));
                    (lo[a], hi[a]) = (u.min(v), u.max(v));
                }
                if lo[2] == hi[2] {
                    return Err(port_error(
                        "sim.port_too_thin",
                        p,
                        format!("port {} ({}) spans no mesh cell between its copper layers", p.info.number, p.info.at),
                        "use solid copper or a reference layer further away",
                    ));
                }
                ports.push(LumpedPort {
                    axis: 2,
                    lo,
                    hi,
                    positive_high: p.stop[2] > p.start[2],
                    r: p.info.z0.to_f64(),
                });
                measures.push(Measure::Element);
            }
            PortKind::Msl => {
                let a = p.prop;
                let across = 1 - a;
                let sign = (p.stop[a] - p.start[a]).signum();
                let feed = node(a, p.start[a] + sign * p.feed_shift);
                let m = node(a, p.start[a] + sign * p.meas_shift);
                let (ks, kg) = (node(2, p.start[2]), node(2, p.stop[2]));
                let (u, v) = (node(across, p.start[across]), node(across, p.stop[across]));
                let (w0, w1) = (u.min(v), u.max(v));
                if (m as i64 - feed as i64) * sign < 1 || m == 0 || m + 1 >= n[a] || ks == kg {
                    return Err(port_error(
                        "sim.port_too_short",
                        p,
                        format!(
                            "port {} ({}): its feed and measurement plane fall on the same mesh line",
                            p.info.number, p.info.at
                        ),
                        "give the microstrip port a longer `length`, or a finer mesh (`max_cell`)",
                    ));
                }
                let mut lo = [0; 3];
                let mut hi = [0; 3];
                lo[a] = feed;
                hi[a] = feed;
                lo[across] = w0;
                hi[across] = w1;
                lo[2] = ks.min(kg);
                hi[2] = ks.max(kg);
                ports.push(LumpedPort { axis: 2, lo, hi, positive_high: ks > kg, r: p.info.z0.to_f64() });
                // Voltage from the line's middle on the signal surface to the reference surface.
                let mid = node(across, (p.start[across] + p.stop[across]) / 2);
                let mut from = [0; 3];
                from[a] = m;
                from[across] = mid;
                from[2] = ks;
                let v = probes.len();
                probes.push(Probe::Voltage { axis: 2, from, to: kg });
                // Current loops around the signal copper on both sides of the plane.
                let layer = r.layers.iter().find(|l| l.name == p.info.layer);
                let (k0, k1) = layer.map_or((ks, ks), |l| (node(2, l.bottom), node(2, l.top)));
                let (k0, k1) = (k0.min(k1), k0.max(k1));
                let (lo2, hi2) = if a == 0 { ([w0, k0], [w1, k1]) } else { ([k0, w0], [k1, w1]) };
                if lo2[0] == 0 || lo2[1] == 0 || hi2[0] + 1 >= n[(a + 1) % 3] || hi2[1] + 1 >= n[(a + 2) % 3] {
                    return Err(port_error(
                        "sim.port_at_edge",
                        p,
                        format!("port {} ({}) touches the region's edge", p.info.number, p.info.at),
                        "grow the region (`margin`) around the port",
                    ));
                }
                let i = probes.len();
                probes.push(Probe::Current { axis: a, at: m - 1, lo: lo2, hi: hi2 });
                probes.push(Probe::Current { axis: a, at: m, lo: lo2, hi: hi2 });
                let (dm, dp) =
                    ((s.lines()[a][m] - s.lines()[a][m - 1]) as f64, (s.lines()[a][m + 1] - s.lines()[a][m]) as f64);
                measures.push(Measure::Line {
                    v,
                    i: [i, i + 1],
                    w: [dp / (dm + dp), dm / (dm + dp)],
                    sign: sign as f64,
                });
            }
        }
    }
    Ok(Built { structure: s, ports, measures, probes, z0 })
}

/// Solves `S A = B` for S (N×N complex, row-major), by Gaussian elimination with partial
/// pivoting on `Aᵀ Sᵀ = Bᵀ`. `None` when A is singular.
#[allow(clippy::needless_range_loop)]
fn solve_s(a: &[C64], b: &[C64], n: usize) -> Option<Vec<C64>> {
    let zero = C64::new(0.0, 0.0);
    // Augmented [Aᵀ | Bᵀ].
    let mut m: Vec<Vec<C64>> =
        (0..n).map(|r| (0..n).map(|c| a[c * n + r]).chain((0..n).map(|c| b[c * n + r])).collect()).collect();
    for col in 0..n {
        let piv = (col..n).max_by(|&x, &y| m[x][col].abs().total_cmp(&m[y][col].abs()))?;
        if m[piv][col].abs() == 0.0 || !m[piv][col].abs().is_finite() {
            return None;
        }
        m.swap(col, piv);
        let p = m[col][col];
        for row in 0..n {
            if row == col {
                continue;
            }
            let f = m[row][col] / p;
            if f == zero {
                continue;
            }
            for c in col..2 * n {
                let v = m[col][c];
                m[row][c] = m[row][c] - f * v;
            }
        }
    }
    // Row r of the solution is Sᵀ's row r: S[c][r].
    let mut s = vec![zero; n * n];
    for r in 0..n {
        for c in 0..n {
            s[c * n + r] = m[r][n + c] / m[r][r];
        }
    }
    Some(s)
}

/// Frequencies of the result: as the export's Touchstone file.
fn frequencies(o: &openems::Options) -> Vec<f64> {
    let start = if o.f_min_hz > 0 { o.f_min_hz } else { (o.f_max_hz / 100).max(1) } as f64;
    let stop = o.f_max_hz as f64;
    let n = o.points.max(2) as usize;
    (0..n).map(|i| start + (stop - start) * i as f64 / (n - 1) as f64).collect()
}

/// Simulates the region and returns its S-parameters.
pub fn sparams(p: &Project, o: &Options, hooks: &Hooks<'_>) -> Result<SParams, Error> {
    // The microstrip ports feed and measure on mesh lines.
    let region = openems::Options { port_planes: true, ..o.region.clone() };
    let r = openems::extract(p, &region)?;
    if r.ports.is_empty() {
        return Err(Error::new(
            "sim.no_ports",
            "no ports: nothing to excite",
            "add `ports` (pads like `J1.1`, or track ends like `track#3` with `end`)",
        ));
    }
    if r.mesh.cells > o.max_cells {
        return Err(Error::new(
            "sim.too_many_cells",
            format!("the mesh has {} cells, above the limit of {}", r.mesh.cells, o.max_cells),
            "shrink the region (`margin`, `window`, `air`), lower `f_max`, coarsen the mesh (`max_cell`, \
             `cells_per_wavelength`, `min_cell`), or raise `max_cells`",
        ));
    }
    let freqs = frequencies(&o.region);
    let band = (freqs[0], *freqs.last().unwrap_or(&freqs[0]));
    let b = build(&r, band)?;
    let excitation = Gaussian { f0: r.f0 as f64, fc: r.fc as f64 };
    let pml = o.region.pml_cells as usize;
    let settings = Settings {
        pml: [pml; 6],
        courant: o.courant,
        max_steps: o.region.timesteps,
        end_db: o.region.end_criteria_db as f64,
        excitation,
        freqs: freqs.clone(),
        threads: 0,
        surface: o.conductivity.map(|sigma| fit::surface_fit(sigma, &o.roughness, band.0, band.1)),
    };
    let mut sim = Simulation::new(&b.structure, &b.ports, b.probes.clone(), settings)
        .map_err(|m| Error::new("sim.bad_setup", m, "grow the region or lower `pml_cells`"))?;
    let np = b.ports.len();
    let nf = freqs.len();
    // Waves per frequency: a[f][i * np + j] = a_i in run j.
    let mut a = vec![vec![C64::new(0.0, 0.0); np * np]; nf];
    let mut bw = a.clone();
    let mut runs = Vec::new();
    let mut diags = r.diagnostics.clone();
    for j in 0..np {
        let progress = |done: u64, _total: Option<u64>, msg: &str| {
            (hooks.progress)(done, Some(o.region.timesteps), &format!("port {}/{np}: {msg}", j + 1));
        };
        let run = sim
            .run(Some(j), &Hooks { progress: &progress, cancelled: hooks.cancelled })
            .map_err(|_| Error::new("cancelled", "operation cancelled", "run it again"))?;
        if run.port_v.iter().chain(&run.probes).flatten().any(|c| !c.re.is_finite() || !c.im.is_finite()) {
            return Err(Error::new(
                "sim.unstable",
                "the simulation diverged",
                "lower `courant`; report the case if it persists",
            ));
        }
        if !run.converged {
            diags.push(
                Diagnostic::warning(
                    "sim.not_converged",
                    format!(
                        "port {}: the energy only decayed to {:.1} dB in {} timesteps: the S-parameters are truncated",
                        j + 1,
                        run.energy_db,
                        run.steps
                    ),
                )
                .with_hint("raise `timesteps`, or lower `end_criteria` (a resonant structure rings long)"),
            );
        }
        for (i, m) in b.measures.iter().enumerate() {
            let z = b.z0[i];
            let k = 1.0 / (2.0 * z.sqrt());
            for fi in 0..nf {
                let (v, cur) = match *m {
                    Measure::Element => (run.port_v[i][fi], run.port_i[i][fi]),
                    Measure::Line { v, i: [i1, i2], w: [w1, w2], sign } => {
                        let c = (run.probes[i1][fi] * C64::new(w1, 0.0) + run.probes[i2][fi] * C64::new(w2, 0.0))
                            * C64::new(sign, 0.0);
                        (run.probes[v][fi], c)
                    }
                };
                let zi = cur * C64::new(z, 0.0);
                a[fi][i * np + j] = (v + zi) * C64::new(k, 0.0);
                bw[fi][i * np + j] = (v - zi) * C64::new(k, 0.0);
            }
        }
        runs.push(PortRun {
            port: j + 1,
            steps: run.steps,
            converged: run.converged,
            energy_db: round2(run.energy_db),
        });
    }
    let mut data = Vec::with_capacity(nf);
    for fi in 0..nf {
        let s = solve_s(&a[fi], &bw[fi], np).unwrap_or_else(|| vec![C64::new(f64::NAN, f64::NAN); np * np]);
        data.push(s.into_iter().map(|c| touchstone::Complex::new(sig(c.re), sig(c.im))).collect());
    }
    let ts = Touchstone {
        version: "1.1".into(),
        ports: np,
        parameter: touchstone::Parameter::S,
        format: touchstone::Format::RI,
        reference: b.z0.clone(),
        freqs,
        data,
        noise: Vec::new(),
        warnings: Vec::new(),
    };
    let window = r.window;
    Ok(SParams {
        touchstone: ts,
        ports: r.ports.into_iter().map(|p| p.info).collect(),
        mesh: r.mesh,
        window: (Point::new(Nm(window.min.x), Nm(window.min.y)), Point::new(Nm(window.max.x), Nm(window.max.y))),
        z_range: (Nm(r.z_range.0), Nm(r.z_range.1)),
        nets: r.nets,
        excitation: (r.f0, r.fc),
        timestep: sim.dt(),
        runs,
        diagnostics: diags,
    })
}

fn round2(v: f64) -> f64 {
    if v.is_finite() { (v * 100.0).round() / 100.0 } else { v }
}

/// Rounds to 7 significant digits: beyond the solver's single-precision fields.
fn sig(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let e = v.abs().log10().floor() as i32;
    let f = 10f64.powi(6 - e);
    (v * f).round() / f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s_from_waves() {
        // A = diag(2, 3), B = [[1, 4], [2, 6]] → S = B A⁻¹ = [[0.5, 4/3], [1, 2]].
        let c = |x: f64| C64::new(x, 0.0);
        let a = [c(2.0), c(0.0), c(0.0), c(3.0)];
        let b = [c(1.0), c(4.0), c(2.0), c(6.0)];
        let s = solve_s(&a, &b, 2).unwrap();
        let want = [0.5, 4.0 / 3.0, 1.0, 2.0];
        for (x, w) in s.iter().zip(want) {
            assert!((x.re - w).abs() < 1e-12 && x.im.abs() < 1e-12, "{s:?}");
        }
        // A full A: S A = B for S = [[0.1, 0.9], [0.9, 0.2]].
        let sm = [c(0.1), c(0.9), c(0.9), c(0.2)];
        let a = [C64::new(1.0, 0.5), c(0.3), c(-0.2), C64::new(0.7, -0.1)];
        let mut b = vec![c(0.0); 4];
        for i in 0..2 {
            for j in 0..2 {
                b[i * 2 + j] = (0..2).fold(c(0.0), |acc, k| acc + sm[i * 2 + k] * a[k * 2 + j]);
            }
        }
        let s = solve_s(&a, &b, 2).unwrap();
        for (x, w) in s.iter().zip(sm) {
            assert!((*x - w).abs() < 1e-12, "{s:?}");
        }
    }
}
