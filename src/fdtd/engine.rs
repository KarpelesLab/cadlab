//! The Yee/FDTD time stepping on a graded rectilinear grid: field arrays, material
//! coefficients, CPML, lumped elements, probes and the running DFT.
//!
//! Written from the textbook formulations: K. S. Yee, IEEE Trans. AP-14 (1966); A. Taflove and
//! S. C. Hagness, *Computational Electrodynamics*, 3rd ed. (2005), chapters 3 (Yee algorithm), 7
//! (CPML, after J. A. Roden and S. D. Gedney, Microwave Opt. Technol. Lett. 27, 2000) and 15
//! (lumped elements, after M. Piket-May, A. Taflove and J. Baron, IEEE Trans. MTT-42, 1994).
//! The graded grid uses the integral (finite integration) form: curls are circulations divided by
//! the face areas of the primal or dual grid.
//!
//! Layout: nodes are the mesh lines' crossings, `idx = i + nx (j + ny k)`. `E_c(i, j, k)` lies on
//! the primal edge from node `(i, j, k)` one step along `c`; `H_c(i, j, k)` on the primal face
//! normal to `c` whose lowest corner is that node (half a cell along the two other axes). Every
//! field array has one entry per node; entries past the last edge stay zero. Tangential E on the
//! domain's six faces is never updated (a perfect conductor behind the absorbing layers).
//!
//! Determinism: every update writes each element from the previous values of the other field
//! only, so splitting the planes between threads gives bit-identical results; reductions (energy)
//! are summed per plane and then in plane order.

use crate::electrical::complex::C64;

/// Speed of light in vacuum (m/s).
pub const C0: f64 = 299_792_458.0;
/// Vacuum permittivity (F/m), CODATA 2018.
pub const EPS0: f64 = 8.854_187_812_8e-12;
/// Vacuum permeability (H/m), consistent with [`C0`] and [`EPS0`].
pub const MU0: f64 = 1.0 / (EPS0 * C0 * C0);
/// Free-space wave impedance (Ω).
pub const ETA0: f64 = MU0 * C0;

/// Grid positions are integer nanometers; the solver works in meters.
fn meters(nm: i64) -> f64 {
    nm as f64 * 1e-9
}

/// A linear isotropic material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    /// Relative permittivity.
    pub eps_r: f64,
    /// Conductivity (S/m).
    pub sigma: f64,
}

impl Material {
    /// Vacuum (and air).
    pub const VACUUM: Material = Material { eps_r: 1.0, sigma: 0.0 };
}

/// The voxelized structure: mesh lines, a material per cell and perfect-conductor edges.
#[derive(Clone, Debug)]
pub struct Structure {
    nm: [Vec<i64>; 3],
    pos: [Vec<f64>; 3],
    materials: Vec<Material>,
    cell: Vec<u16>,
    pec: [Vec<bool>; 3],
}

/// Number of entries of a node-indexed array.
fn nodes(n: [usize; 3]) -> usize {
    n[0] * n[1] * n[2]
}

impl Structure {
    /// A vacuum-filled structure on these mesh lines (nm, strictly increasing, at least two per
    /// axis).
    pub fn new(lines: [Vec<i64>; 3]) -> Result<Structure, String> {
        for (a, l) in lines.iter().enumerate() {
            if l.len() < 2 {
                return Err(format!("axis {a}: at least two mesh lines are needed"));
            }
            if l.windows(2).any(|w| w[1] <= w[0]) {
                return Err(format!("axis {a}: mesh lines must increase strictly"));
            }
        }
        let n = [lines[0].len(), lines[1].len(), lines[2].len()];
        let cells = (n[0] - 1) * (n[1] - 1) * (n[2] - 1);
        let pos = [0, 1, 2].map(|a| lines[a].iter().map(|&v| meters(v)).collect());
        Ok(Structure {
            pos,
            nm: lines,
            materials: vec![Material::VACUUM],
            cell: vec![0; cells],
            pec: [vec![false; nodes(n)], vec![false; nodes(n)], vec![false; nodes(n)]],
        })
    }

    /// Number of mesh lines per axis.
    pub fn n(&self) -> [usize; 3] {
        [self.nm[0].len(), self.nm[1].len(), self.nm[2].len()]
    }

    /// Mesh lines (nm).
    pub fn lines(&self) -> &[Vec<i64>; 3] {
        &self.nm
    }

    /// Number of cells.
    pub fn cells(&self) -> u64 {
        let n = self.n();
        ((n[0] - 1) * (n[1] - 1) * (n[2] - 1)) as u64
    }

    /// Registers a material, returning its index (0 is vacuum).
    pub fn material(&mut self, m: Material) -> u16 {
        if let Some(i) = self.materials.iter().position(|x| *x == m) {
            return i as u16;
        }
        self.materials.push(m);
        (self.materials.len() - 1) as u16
    }

    /// Index of the mesh line nearest to `at` (nm) on `axis` (the lower one on a tie).
    pub fn node(&self, axis: usize, at: i64) -> usize {
        let l = &self.nm[axis];
        match l.binary_search(&at) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) if i >= l.len() => l.len() - 1,
            Err(i) => {
                if at - l[i - 1] <= l[i] - at {
                    i - 1
                } else {
                    i
                }
            }
        }
    }

    /// Index range of the lines within `[lo, hi]` on an axis.
    fn range(&self, axis: usize, lo: i64, hi: i64) -> std::ops::Range<usize> {
        let l = &self.nm[axis];
        l.partition_point(|&v| v < lo)..l.partition_point(|&v| v <= hi)
    }

    /// Sets the material of the cells whose center lies in the box `[lo, hi]` (nm, inclusive) and
    /// for which `inside(x, y)` holds at the center.
    pub fn fill(&mut self, mat: u16, lo: [i64; 3], hi: [i64; 3], inside: &dyn Fn(i64, i64) -> bool) {
        let n = self.n();
        let c = |a: usize, i: usize| self.nm[a][i] + (self.nm[a][i + 1] - self.nm[a][i]) / 2;
        let mut set = Vec::new();
        for j in 0..n[1] - 1 {
            let y = c(1, j);
            if y < lo[1] || y > hi[1] {
                continue;
            }
            for i in 0..n[0] - 1 {
                let x = c(0, i);
                if x < lo[0] || x > hi[0] || !inside(x, y) {
                    continue;
                }
                for k in 0..n[2] - 1 {
                    let z = c(2, k);
                    if z >= lo[2] && z <= hi[2] {
                        set.push(i + (n[0] - 1) * (j + (n[1] - 1) * k));
                    }
                }
            }
        }
        for s in set {
            self.cell[s] = mat;
        }
    }

    /// Makes perfect conductors of the edges lying in the closed box `[lo, hi]` (nm) whose position
    /// passes `inside(x, y)`: x and y edges by their midpoint, z edges by their node. A box of zero
    /// height is a sheet (no z edges). Returns the number of edges set.
    pub fn pec(&mut self, lo: [i64; 3], hi: [i64; 3], inside: &dyn Fn(i64, i64) -> bool) -> usize {
        let n = self.n();
        let (rx, ry, rz) = (self.range(0, lo[0], hi[0]), self.range(1, lo[1], hi[1]), self.range(2, lo[2], hi[2]));
        let l = &self.nm;
        let mut set: [Vec<usize>; 3] = Default::default();
        let id = |i: usize, j: usize, k: usize| i + n[0] * (j + n[1] * k);
        for k in rz.clone() {
            for j in ry.clone() {
                // x edges between consecutive lines inside the box.
                for i in rx.start..rx.end.saturating_sub(1) {
                    let xm = l[0][i] + (l[0][i + 1] - l[0][i]) / 2;
                    if inside(xm, l[1][j]) {
                        set[0].push(id(i, j, k));
                    }
                }
            }
            for j in ry.start..ry.end.saturating_sub(1) {
                let ym = l[1][j] + (l[1][j + 1] - l[1][j]) / 2;
                for i in rx.clone() {
                    if inside(l[0][i], ym) {
                        set[1].push(id(i, j, k));
                    }
                }
            }
        }
        for k in rz.start..rz.end.saturating_sub(1) {
            for j in ry.clone() {
                for i in rx.clone() {
                    if inside(l[0][i], l[1][j]) {
                        set[2].push(id(i, j, k));
                    }
                }
            }
        }
        let mut count = 0;
        for (a, s) in set.iter().enumerate() {
            for &e in s {
                self.pec[a][e] = true;
            }
            count += s.len();
        }
        count
    }

    /// Whether the edge along `axis` from node `at` is a perfect conductor.
    pub fn is_pec(&self, axis: usize, at: [usize; 3]) -> bool {
        let n = self.n();
        self.pec[axis][at[0] + n[0] * (at[1] + n[1] * at[2])]
    }
}

/// A Gaussian-modulated pulse covering `f0 − fc .. f0 + fc` at −20 dB (a plain Gaussian, from
/// DC, when `f0` is 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gaussian {
    /// Center frequency (Hz).
    pub f0: f64,
    /// 20 dB half-bandwidth (Hz).
    pub fc: f64,
}

impl Gaussian {
    /// Width τ of `exp(−((t − t0)/τ)²)`: its spectrum `exp(−(π f τ)²)` is 20 dB down at `fc`.
    pub fn tau(&self) -> f64 {
        std::f64::consts::LN_10.sqrt() / (std::f64::consts::PI * self.fc)
    }
    /// Delay of the peak (the pulse starts 106 dB below it).
    pub fn t0(&self) -> f64 {
        3.5 * self.tau()
    }
    /// When the pulse is over (symmetric around `t0`).
    pub fn end(&self) -> f64 {
        2.0 * self.t0()
    }
    /// Value at `t` (peak 1).
    pub fn at(&self, t: f64) -> f64 {
        let u = (t - self.t0()) / self.tau();
        (-u * u).exp() * (2.0 * std::f64::consts::PI * self.f0 * (t - self.t0())).cos()
    }
}

/// A lumped port: a resistive voltage source along `axis` (Piket-May): columns of edges at every
/// node of the transverse ranges, from `lo[axis]` to `hi[axis]`, the total resistance `r` in
/// series with the source. `positive_high`: the positive terminal (the signal) is at `hi[axis]`.
#[derive(Clone, Debug, PartialEq)]
pub struct LumpedPort {
    /// Direction of the element (0 x, 1 y, 2 z).
    pub axis: usize,
    /// Lowest node (inclusive box).
    pub lo: [usize; 3],
    /// Highest node.
    pub hi: [usize; 3],
    /// The positive terminal is at `hi[axis]`.
    pub positive_high: bool,
    /// Resistance (Ω).
    pub r: f64,
}

/// A probe, sampled every timestep and transformed to the frequencies of the run.
#[derive(Clone, Debug, PartialEq)]
pub enum Probe {
    /// Voltage `φ(from) − φ(to)`, the line integral of E from node `from` to the node at index
    /// `to` along `axis`. Sampled with E.
    Voltage {
        /// Axis of the path.
        axis: usize,
        /// Start node.
        from: [usize; 3],
        /// End index along `axis`.
        to: usize,
    },
    /// Current along `+axis` through the dual plane between nodes `at` and `at + 1`: the
    /// circulation of H around the nodes `lo ..= hi` of the two other axes (taken in cyclic order
    /// `axis + 1`, `axis + 2`). Sampled with H (half a step before E).
    Current {
        /// Normal of the plane.
        axis: usize,
        /// Plane position: between nodes `at` and `at + 1`.
        at: usize,
        /// Lowest enclosed node (axes `axis + 1`, `axis + 2`).
        lo: [usize; 2],
        /// Highest enclosed node.
        hi: [usize; 2],
    },
}

/// Run settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// CPML thickness in cells on the faces x−, x+, y−, y+, z−, z+ (0: a perfect conductor wall).
    pub pml: [usize; 6],
    /// Timestep as a fraction of the Courant limit of the smallest cells.
    pub courant: f64,
    /// Most timesteps per run.
    pub max_steps: u64,
    /// Stop when the field energy has decayed this many dB below its peak (after the pulse).
    pub end_db: f64,
    /// Excitation of the driven port.
    pub excitation: Gaussian,
    /// Frequencies of the running DFT (Hz).
    pub freqs: Vec<f64>,
    /// Worker threads (0: from the grid size, one per 131072 nodes up to the global pool's
    /// size; 1: none). Results are identical for any count.
    pub threads: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            pml: [8; 6],
            courant: 0.95,
            max_steps: 100_000,
            end_db: 40.0,
            excitation: Gaussian { f0: 5e9, fc: 5e9 },
            freqs: Vec::new(),
            threads: 0,
        }
    }
}

/// Progress and cancellation callbacks.
pub struct Hooks<'a> {
    /// `(done, total, message)`.
    pub progress: &'a dyn Fn(u64, Option<u64>, &str),
    /// Polled regularly; `true` stops the run.
    pub cancelled: &'a dyn Fn() -> bool,
}

impl Hooks<'_> {
    /// No progress, never cancelled.
    pub fn none() -> Hooks<'static> {
        Hooks { progress: &|_, _, _| {}, cancelled: &|| false }
    }
}

/// The run was cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

/// Result of one run: spectra of the port and probe quantities.
#[derive(Clone, Debug)]
pub struct Run {
    /// Timesteps taken.
    pub steps: u64,
    /// Whether the energy criterion ended the run (else `max_steps` did).
    pub converged: bool,
    /// Final field energy relative to its peak (dB).
    pub energy_db: f64,
    /// Per port: voltage (positive terminal minus the other) per frequency.
    pub port_v: Vec<Vec<C64>>,
    /// Per port: current out of the element into the positive terminal per frequency.
    pub port_i: Vec<Vec<C64>>,
    /// Per probe, per frequency.
    pub probes: Vec<Vec<C64>>,
    /// The source voltage of the driven port per frequency.
    pub source: Vec<C64>,
}

/// One port's precomputed edges.
struct PortEdges {
    axis: usize,
    /// Field index, length, source coefficient (`E −= k · V_s`).
    edges: Vec<(usize, f32, f32)>,
    /// Sign: −1 when the positive terminal is at `hi` (V = −Σ E l).
    sign: f64,
    columns: usize,
    r: f64,
}

/// A CPML slab: one field component's correction for its derivative along one axis.
struct Slab {
    /// Updated component, 0..3 E, 3..6 H.
    comp: usize,
    /// Differentiated component (the other field, 0..3).
    src: usize,
    /// Derivative axis.
    w: usize,
    /// Sign in the curl.
    sign: f32,
    /// Box in node indices.
    lo: [usize; 3],
    dims: [usize; 3],
    /// Profiles along `w` (index − lo[w]): b, c, 1/κ − 1.
    b: Vec<f32>,
    c: Vec<f32>,
    kinv1: Vec<f32>,
    psi: Vec<f32>,
}

/// The simulation state.
pub struct Simulation {
    n: [usize; 3],
    dt: f64,
    settings: Settings,
    /// Primal cell sizes and their inverses, dual sizes and inverses per axis.
    d: [Vec<f64>; 3],
    inv_d: [Vec<f32>; 3],
    dd: [Vec<f64>; 3],
    inv_dd: [Vec<f32>; 3],
    /// E then H components.
    f: [Vec<f32>; 6],
    /// Coefficient table index per E edge, table of (ca, cb, ε).
    tab: [Vec<u32>; 3],
    coef: Vec<(f32, f32, f32)>,
    /// dt / μ0.
    ch: f32,
    slabs: Vec<Slab>,
    ports: Vec<PortEdges>,
    probes: Vec<Probe>,
    pool: Option<Pool>,
}

fn dual(d: &[f64]) -> Vec<f64> {
    let n = d.len() + 1;
    (0..n)
        .map(|i| {
            let a = if i > 0 { d[i - 1] } else { 0.0 };
            let b = if i < d.len() { d[i] } else { 0.0 };
            (a + b) / 2.0
        })
        .collect()
}

fn inv(v: &[f64]) -> Vec<f32> {
    v.iter().map(|&x| if x > 0.0 { (1.0 / x) as f32 } else { 0.0 }).collect()
}

/// Nodes per parallel task: enough work to outweigh the scheduling.
#[cfg(feature = "parallel")]
const TASK: usize = 32_768;

/// The solver's own thread pool, sized to the grid (waking many threads for a small grid costs
/// more than the update).
#[cfg(feature = "parallel")]
type Pool = rayon::ThreadPool;
#[cfg(not(feature = "parallel"))]
type Pool = ();

/// Nodes per thread.
#[cfg(feature = "parallel")]
const PER_THREAD: usize = 131_072;

#[cfg(feature = "parallel")]
fn pool(nodes: usize, threads: usize) -> Option<Pool> {
    let t = if threads > 0 { threads } else { rayon::current_num_threads().min(nodes / PER_THREAD) };
    if t < 2 {
        return None;
    }
    rayon::ThreadPoolBuilder::new().num_threads(t).build().ok()
}

#[cfg(not(feature = "parallel"))]
fn pool(_: usize, _: usize) -> Option<Pool> {
    None
}

/// Runs `f(k, plane)` on the planes of `data` (in parallel with the `parallel` feature; results
/// do not depend on it).
fn planes<T: Send>(data: &mut [T], plane: usize, pool: Option<&Pool>, f: impl Fn(usize, &mut [T]) + Sync + Send) {
    #[cfg(feature = "parallel")]
    if let Some(pool) = pool {
        use rayon::prelude::*;
        let g = TASK.div_ceil(plane);
        pool.install(|| {
            data.par_chunks_mut(plane * g).enumerate().for_each(|(t, c)| {
                for (k, p) in c.chunks_mut(plane).enumerate() {
                    f(t * g + k, p);
                }
            })
        });
        return;
    }
    let _ = pool;
    for (k, c) in data.chunks_mut(plane).enumerate() {
        f(k, c);
    }
}

impl Simulation {
    /// Prepares a simulation of the structure. Errors: ports or probes outside the grid or
    /// degenerate, absorbing layers thicker than the grid.
    pub fn new(
        s: &Structure,
        ports: &[LumpedPort],
        probes: Vec<Probe>,
        settings: Settings,
    ) -> Result<Simulation, String> {
        let n = s.n();
        let d: [Vec<f64>; 3] = [0, 1, 2].map(|a| s.pos[a].windows(2).map(|w| w[1] - w[0]).collect::<Vec<f64>>());
        let dd = [dual(&d[0]), dual(&d[1]), dual(&d[2])];
        for (f, &p) in settings.pml.iter().enumerate() {
            if p > 0 && 2 * p + 1 > n[f / 2] - 1 {
                return Err(format!("{p} absorbing cells do not fit in the {} cells of axis {}", n[f / 2] - 1, f / 2));
            }
        }
        // Courant limit of the smallest cells, in vacuum.
        let min = [0, 1, 2].map(|a| d[a].iter().copied().fold(f64::INFINITY, f64::min));
        let dt = settings.courant
            / (C0 * (1.0 / (min[0] * min[0]) + 1.0 / (min[1] * min[1]) + 1.0 / (min[2] * min[2])).sqrt());
        let size = nodes(n);
        let mut sim = Simulation {
            n,
            dt,
            inv_d: [inv(&d[0]), inv(&d[1]), inv(&d[2])],
            inv_dd: [inv(&dd[0]), inv(&dd[1]), inv(&dd[2])],
            d,
            dd,
            f: std::array::from_fn(|_| vec![0.0; size]),
            tab: [vec![0; size], vec![0; size], vec![0; size]],
            coef: Vec::new(),
            ch: (dt / MU0) as f32,
            pool: pool(size, settings.threads),
            slabs: Vec::new(),
            ports: Vec::new(),
            probes: Vec::new(),
            settings,
        };
        sim.coefficients(s, ports)?;
        sim.cpml();
        for p in &probes {
            sim.check_probe(p)?;
        }
        sim.probes = probes;
        Ok(sim)
    }

    /// The timestep (s).
    pub fn dt(&self) -> f64 {
        self.dt
    }

    /// Number of nodes per axis.
    pub fn n(&self) -> [usize; 3] {
        self.n
    }

    fn id(&self, p: [usize; 3]) -> usize {
        p[0] + self.n[0] * (p[1] + self.n[1] * p[2])
    }

    /// The four cells around an edge along `a` at node `p`, with their quarter dual areas.
    fn around(&self, a: usize, p: [usize; 3]) -> Vec<([usize; 3], f64)> {
        let (b, c) = ((a + 1) % 3, (a + 2) % 3);
        let mut out = Vec::with_capacity(4);
        for ob in [0usize, 1] {
            for oc in [0usize, 1] {
                let (Some(ib), Some(ic)) = ((p[b] + ob).checked_sub(1), (p[c] + oc).checked_sub(1)) else { continue };
                if ib >= self.n[b] - 1 || ic >= self.n[c] - 1 {
                    continue;
                }
                let mut q = p;
                q[b] = ib;
                q[c] = ic;
                out.push((q, self.d[b][ib] * self.d[c][ic] / 4.0));
            }
        }
        out
    }

    /// Edge coefficients from the averaged materials, perfect conductors and port resistances.
    fn coefficients(&mut self, s: &Structure, ports: &[LumpedPort]) -> Result<(), String> {
        let n = self.n;
        let cell_id = |q: [usize; 3]| q[0] + (n[0] - 1) * (q[1] + (n[1] - 1) * q[2]);
        // Port edges: extra conductivity and source coefficient.
        let mut extra: [std::collections::BTreeMap<usize, f64>; 3] = Default::default();
        let mut port_edges = Vec::new();
        for (pi, p) in ports.iter().enumerate() {
            let a = p.axis;
            if a > 2 || (0..3).any(|x| p.lo[x] > p.hi[x] || p.hi[x] >= n[x]) || p.lo[a] == p.hi[a] {
                return Err(format!("port {}: its box is empty or outside the grid", pi + 1));
            }
            if p.r <= 0.0 || !p.r.is_finite() {
                return Err(format!("port {}: the resistance must be positive", pi + 1));
            }
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            let columns = (p.hi[b] - p.lo[b] + 1) * (p.hi[c] - p.lo[c] + 1);
            let per_edge = (p.hi[a] - p.lo[a]) as f64;
            let r_e = p.r * columns as f64 / per_edge;
            let mut edges = Vec::new();
            for ic in p.lo[c]..=p.hi[c] {
                for ib in p.lo[b]..=p.hi[b] {
                    for ia in p.lo[a]..p.hi[a] {
                        let mut q = [0; 3];
                        q[a] = ia;
                        q[b] = ib;
                        q[c] = ic;
                        let area: f64 = self.around(a, q).iter().map(|x| x.1).sum();
                        let l = self.d[a][ia];
                        *extra[a].entry(self.id(q)).or_default() += l / (r_e * area);
                        edges.push((self.id(q), l, area));
                    }
                }
            }
            let sign = if p.positive_high { -1.0 } else { 1.0 };
            port_edges.push((a, edges, sign, columns, per_edge, r_e, p.r));
        }
        let mut table: std::collections::BTreeMap<(u32, u32, u32), u32> = std::collections::BTreeMap::new();
        let mut coef = vec![(0.0f32, 0.0f32, 1.0f32)];
        table.insert((0, 0, 1.0f32.to_bits()), 0);
        let dt = self.dt;
        for a in 0..3 {
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            for k in 0..n[2] {
                for j in 0..n[1] {
                    for i in 0..n[0] {
                        let p = [i, j, k];
                        if p[a] + 1 >= n[a] {
                            continue;
                        }
                        let e = self.id(p);
                        // Tangential edges on the walls stay zero.
                        let wall = p[b] == 0 || p[b] + 1 == n[b] || p[c] == 0 || p[c] + 1 == n[c];
                        if wall || s.pec[a][e] && !extra[a].contains_key(&e) {
                            self.tab[a][e] = 0;
                            continue;
                        }
                        let (mut eps, mut sig, mut w) = (0.0, 0.0, 0.0);
                        for (q, area) in self.around(a, p) {
                            let m = s.materials[s.cell[cell_id(q)] as usize];
                            eps += m.eps_r * area;
                            sig += m.sigma * area;
                            w += area;
                        }
                        let (eps, mut sig) = (EPS0 * eps / w, sig / w);
                        sig += extra[a].get(&e).copied().unwrap_or(0.0);
                        let h = sig * dt / (2.0 * eps);
                        let ca = ((1.0 - h) / (1.0 + h)) as f32;
                        let cb = (dt / eps / (1.0 + h)) as f32;
                        let key = (ca.to_bits(), cb.to_bits(), (eps as f32).to_bits());
                        let next = coef.len() as u32;
                        let t = *table.entry(key).or_insert(next);
                        if t == next {
                            coef.push((ca, cb, eps as f32));
                        }
                        self.tab[a][e] = t;
                    }
                }
            }
        }
        self.coef = coef;
        for (a, edges, sign, columns, per_edge, r_e, r) in port_edges {
            let edges = edges
                .into_iter()
                .map(|(e, l, area)| {
                    let cb = self.coef[self.tab[a][e] as usize].1 as f64;
                    // J = σ_p E + (−sign) V_s / (N R_e A): E −= cb · J_src.
                    (e, l as f32, (cb * -sign / (per_edge * r_e * area)) as f32)
                })
                .collect();
            self.ports.push(PortEdges { axis: a, edges, sign, columns, r });
        }
        Ok(())
    }

    /// CPML slabs on every face with absorbing cells.
    fn cpml(&mut self) {
        const M: i32 = 3;
        let n = self.n;
        let dt = self.dt;
        let alpha_max =
            2.0 * std::f64::consts::PI * EPS0 * (self.settings.excitation.f0.max(self.settings.excitation.fc) / 10.0);
        for face in 0..6 {
            let cells = self.settings.pml[face];
            if cells == 0 {
                continue;
            }
            let w = face / 2;
            let high = face % 2 == 1;
            let x = |i: usize| self.d[w][..i].iter().sum::<f64>();
            let (inner, outer) = if high { (x(n[w] - 1 - cells), x(n[w] - 1)) } else { (x(cells), 0.0) };
            let thick = (inner - outer).abs();
            let sigma_max = 0.8 * (M + 1) as f64 / (ETA0 * thick / cells as f64);
            let profile = |pos: f64| {
                let rho = ((pos - inner) / (outer - inner)).clamp(0.0, 1.0);
                let sigma = sigma_max * rho.powi(M);
                let kappa = 1.0;
                let alpha = alpha_max * (1.0 - rho);
                let b = (-(sigma / kappa + alpha) * dt / EPS0).exp();
                let c = if sigma > 0.0 { sigma / (sigma * kappa + kappa * kappa * alpha) * (b - 1.0) } else { 0.0 };
                (b as f32, c as f32, (1.0 / kappa - 1.0) as f32)
            };
            // E components at nodes along w, H components at cell centers.
            for comp in 0..6 {
                let c = comp % 3;
                if c == w {
                    continue;
                }
                let is_e = comp < 3;
                // The other field's component differentiated along w: the third axis.
                let src = 3 - c - w;
                // E_c gains +∂H_(c+2)/∂x_(c+1) − ∂H_(c+1)/∂x_(c+2); H_c loses the same curl of E
                // (the multiplier carries the minus).
                let sign = if w == (c + 1) % 3 { 1.0 } else { -1.0 };
                // The component's update box.
                let mut lo = [0; 3];
                let mut hi = [0; 3];
                for ax in 0..3 {
                    (lo[ax], hi[ax]) = match (is_e, ax == c) {
                        (true, true) => (0, n[ax] - 1),
                        (true, false) => (1, n[ax] - 1),
                        (false, true) => (0, n[ax]),
                        (false, false) => (0, n[ax] - 1),
                    };
                }
                // Along w: the PML part.
                let (a, b) = match (is_e, high) {
                    (true, false) => (1, cells + 1),
                    (true, true) => (n[w] - 1 - cells, n[w] - 1),
                    (false, false) => (0, cells),
                    (false, true) => (n[w] - 1 - cells, n[w] - 1),
                };
                lo[w] = a.max(lo[w]);
                hi[w] = b.min(hi[w]);
                if hi[w] <= lo[w] {
                    continue;
                }
                let dims = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
                let (mut pb, mut pc, mut pk) = (Vec::new(), Vec::new(), Vec::new());
                for i in lo[w]..hi[w] {
                    let pos = if is_e { x(i) } else { x(i) + self.d[w][i] / 2.0 };
                    let (b, c, k) = profile(pos);
                    pb.push(b);
                    pc.push(c);
                    pk.push(k);
                }
                self.slabs.push(Slab {
                    comp,
                    src,
                    w,
                    sign,
                    lo,
                    dims,
                    b: pb,
                    c: pc,
                    kinv1: pk,
                    psi: vec![0.0; dims[0] * dims[1] * dims[2]],
                });
            }
        }
    }

    fn check_probe(&self, p: &Probe) -> Result<(), String> {
        let n = self.n;
        match p {
            Probe::Voltage { axis, from, to } => {
                if *axis > 2 || (0..3).any(|a| from[a] >= n[a]) || *to >= n[*axis] {
                    return Err("a voltage probe is outside the grid".into());
                }
            }
            Probe::Current { axis, at, lo, hi } => {
                let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
                if *axis > 2
                    || *at + 1 >= n[*axis]
                    || lo[0] == 0
                    || lo[1] == 0
                    || hi[0] + 1 >= n[b]
                    || hi[1] + 1 >= n[c]
                    || lo[0] > hi[0]
                    || lo[1] > hi[1]
                {
                    return Err("a current probe's loop is outside the grid".into());
                }
            }
        }
        Ok(())
    }

    /// Zeroes the fields and the CPML memory.
    pub fn reset(&mut self) {
        for f in &mut self.f {
            f.fill(0.0);
        }
        for s in &mut self.slabs {
            s.psi.fill(0.0);
        }
    }

    #[allow(clippy::needless_range_loop)]
    fn update_h(&mut self) {
        let n = self.n;
        let plane = n[0] * n[1];
        let pool = self.pool.as_ref();
        let ch = self.ch;
        let [ex, ey, ez, hx, hy, hz] = &mut self.f;
        let (ex, ey, ez) = (&*ex, &*ey, &*ez);
        let [idx, idy, idz] = &self.inv_d;
        let nx = n[0];
        // Hx −= ch (∂Ez/∂y − ∂Ey/∂z), on faces j < ny − 1, k < nz − 1.
        planes(hx, plane, pool, |k, h| {
            if k + 1 >= n[2] {
                return;
            }
            let base = k * plane;
            let dz = idz[k];
            for j in 0..n[1] - 1 {
                let (o, dy) = (nx * j, idy[j]);
                let g = base + o;
                let row = h[o..o + nx].iter_mut();
                let a = ez[g + nx..g + 2 * nx].iter().zip(&ez[g..g + nx]);
                let b = ey[g + plane..g + plane + nx].iter().zip(&ey[g..g + nx]);
                for (hv, ((e1, e0), (f1, f0))) in row.zip(a.zip(b)) {
                    *hv -= ch * ((e1 - e0) * dy - (f1 - f0) * dz);
                }
            }
        });
        // Hy −= ch (∂Ex/∂z − ∂Ez/∂x), i < nx − 1.
        planes(hy, plane, pool, |k, h| {
            if k + 1 >= n[2] {
                return;
            }
            let base = k * plane;
            let dz = idz[k];
            let len = nx - 1;
            for j in 0..n[1] {
                let o = nx * j;
                let g = base + o;
                let row = h[o..o + len].iter_mut();
                let a = ex[g + plane..g + plane + len].iter().zip(&ex[g..g + len]);
                let b = ez[g + 1..g + 1 + len].iter().zip(&ez[g..g + len]).zip(&idx[..len]);
                for (hv, ((e1, e0), ((f1, f0), dx))) in row.zip(a.zip(b)) {
                    *hv -= ch * ((e1 - e0) * dz - (f1 - f0) * dx);
                }
            }
        });
        // Hz −= ch (∂Ey/∂x − ∂Ex/∂y), i < nx − 1, j < ny − 1.
        planes(hz, plane, pool, |k, h| {
            let base = k * plane;
            let len = nx - 1;
            for j in 0..n[1] - 1 {
                let (o, dy) = (nx * j, idy[j]);
                let g = base + o;
                let row = h[o..o + len].iter_mut();
                let a = ey[g + 1..g + 1 + len].iter().zip(&ey[g..g + len]).zip(&idx[..len]);
                let b = ex[g + nx..g + nx + len].iter().zip(&ex[g..g + len]);
                for (hv, (((e1, e0), dx), (f1, f0))) in row.zip(a.zip(b)) {
                    *hv -= ch * ((e1 - e0) * dx - (f1 - f0) * dy);
                }
            }
        });
    }

    #[allow(clippy::needless_range_loop)]
    fn update_e(&mut self) {
        let n = self.n;
        let plane = n[0] * n[1];
        let pool = self.pool.as_ref();
        let [ex, ey, ez, hx, hy, hz] = &mut self.f;
        let (hx, hy, hz) = (&*hx, &*hy, &*hz);
        let [tx, ty, tz] = &self.tab;
        let coef = &self.coef;
        let [idx, idy, idz] = &self.inv_dd;
        let nx = n[0];
        // Ex: ∂Hz/∂y − ∂Hy/∂z, edges i < nx − 1, 0 < j < ny − 1, 0 < k < nz − 1.
        planes(ex, plane, pool, |k, e| {
            if k == 0 || k + 1 >= n[2] {
                return;
            }
            let base = k * plane;
            let dz = idz[k];
            let len = nx - 1;
            for j in 1..n[1] - 1 {
                let (o, dy) = (nx * j, idy[j]);
                let g = base + o;
                let row = e[o..o + len].iter_mut().zip(&tx[g..g + len]);
                let a = hz[g..g + len].iter().zip(&hz[g - nx..g - nx + len]);
                let b = hy[g..g + len].iter().zip(&hy[g - plane..g - plane + len]);
                for ((ev, t), ((h1, h0), (k1, k0))) in row.zip(a.zip(b)) {
                    let (ca, cb, _) = coef[*t as usize];
                    *ev = ca * *ev + cb * ((h1 - h0) * dy - (k1 - k0) * dz);
                }
            }
        });
        // Ey: ∂Hx/∂z − ∂Hz/∂x, 0 < i < nx − 1, j < ny − 1.
        planes(ey, plane, pool, |k, e| {
            if k == 0 || k + 1 >= n[2] {
                return;
            }
            let base = k * plane;
            let dz = idz[k];
            let len = nx - 2;
            for j in 0..n[1] - 1 {
                let o = nx * j + 1;
                let g = base + o;
                let row = e[o..o + len].iter_mut().zip(&ty[g..g + len]);
                let a = hx[g..g + len].iter().zip(&hx[g - plane..g - plane + len]);
                let b = hz[g..g + len].iter().zip(&hz[g - 1..g - 1 + len]).zip(&idx[1..1 + len]);
                for ((ev, t), ((h1, h0), ((k1, k0), dx))) in row.zip(a.zip(b)) {
                    let (ca, cb, _) = coef[*t as usize];
                    *ev = ca * *ev + cb * ((h1 - h0) * dz - (k1 - k0) * dx);
                }
            }
        });
        // Ez: ∂Hy/∂x − ∂Hx/∂y, 0 < i < nx − 1, 0 < j < ny − 1, k < nz − 1.
        planes(ez, plane, pool, |k, e| {
            if k + 1 >= n[2] {
                return;
            }
            let base = k * plane;
            let len = nx - 2;
            for j in 1..n[1] - 1 {
                let (o, dy) = (nx * j + 1, idy[j]);
                let g = base + o;
                let row = e[o..o + len].iter_mut().zip(&tz[g..g + len]);
                let a = hy[g..g + len].iter().zip(&hy[g - 1..g - 1 + len]).zip(&idx[1..1 + len]);
                let b = hx[g..g + len].iter().zip(&hx[g - nx..g - nx + len]);
                for ((ev, t), (((h1, h0), dx), (k1, k0))) in row.zip(a.zip(b)) {
                    let (ca, cb, _) = coef[*t as usize];
                    *ev = ca * *ev + cb * ((h1 - h0) * dx - (k1 - k0) * dy);
                }
            }
        });
    }

    /// CPML corrections of E (`e_field`) or H.
    fn update_cpml(&mut self, e_field: bool) {
        let n = self.n;
        let plane = n[0] * n[1];
        let stride = [1, n[0], plane];
        let ch = self.ch;
        let Simulation { f, slabs, tab, coef, inv_d, inv_dd, pool, .. } = self;
        let pool = pool.as_ref();
        for s in slabs.iter_mut().filter(|s| (s.comp < 3) == e_field) {
            let (dst, src) = if e_field {
                let (lo, hi) = f.split_at_mut(3);
                (&mut lo[s.comp], &hi[s.src])
            } else {
                let (lo, hi) = f.split_at_mut(3);
                (&mut hi[s.comp - 3], &lo[s.src])
            };
            let src: &[f32] = src;
            let st = stride[s.w];
            let inv: &[f32] = if e_field { &inv_dd[s.w] } else { &inv_d[s.w] };
            let tabc: &[u32] = if e_field { &tab[s.comp] } else { &[] };
            let coef: &[(f32, f32, f32)] = coef;
            let Slab { lo, dims, b, c, kinv1, psi, w, sign, .. } = s;
            let (lo, dims, w, sign) = (*lo, *dims, *w, *sign);
            let (b, c, kinv1) = (&*b, &*c, &*kinv1);
            let pplane = dims[0] * dims[1];
            let rows = &mut dst[lo[2] * plane..(lo[2] + dims[2]) * plane];
            let work = |kk: usize, psi_k: &mut [f32], row: &mut [f32]| {
                let k = lo[2] + kk;
                for jj in 0..dims[1] {
                    let j = lo[1] + jj;
                    // Profile index and position along w when constant over the row.
                    let (pw_row, at_row) = match w {
                        1 => (jj, j),
                        _ => (kk, k),
                    };
                    let g0 = lo[0] + n[0] * j + plane * k;
                    let prow = &mut psi_k[dims[0] * jj..dims[0] * (jj + 1)];
                    let frow = &mut row[lo[0] + n[0] * j..lo[0] + n[0] * j + dims[0]];
                    for (ii, (p, fv)) in prow.iter_mut().zip(frow.iter_mut()).enumerate() {
                        let g = g0 + ii;
                        let (pw, at) = if w == 0 { (ii, lo[0] + ii) } else { (pw_row, at_row) };
                        // E: (H[at] − H[at − 1]) / dual; H: (E[at + 1] − E[at]) / primal.
                        let d =
                            if e_field { (src[g] - src[g - st]) * inv[at] } else { (src[g + st] - src[g]) * inv[at] };
                        *p = b[pw] * *p + c[pw] * d;
                        let m = if e_field { coef[tabc[g] as usize].1 } else { -ch };
                        *fv += m * sign * (kinv1[pw] * d + *p);
                    }
                }
            };
            #[cfg(feature = "parallel")]
            if let Some(pool) = pool {
                use rayon::prelude::*;
                let g = TASK.div_ceil(plane);
                pool.install(|| {
                    psi.par_chunks_mut(pplane * g).zip(rows.par_chunks_mut(plane * g)).enumerate().for_each(
                        |(t, (pk, row))| {
                            for (kk, (pk, row)) in pk.chunks_mut(pplane).zip(row.chunks_mut(plane)).enumerate() {
                                work(t * g + kk, pk, row);
                            }
                        },
                    )
                });
                continue;
            }
            let _ = pool;
            for (kk, (pk, row)) in psi.chunks_mut(pplane).zip(rows.chunks_mut(plane)).enumerate() {
                work(kk, pk, row);
            }
        }
    }

    /// Column voltages of a port: the mean over columns of (positive terminal − other).
    fn port_voltage(&self, p: &PortEdges) -> f64 {
        let e = &self.f[p.axis];
        let s: f64 = p.edges.iter().map(|&(g, l, _)| e[g] as f64 * l as f64).sum();
        p.sign * s / p.columns as f64
    }

    fn probe_value(&self, p: &Probe) -> f64 {
        match *p {
            Probe::Voltage { axis, from, to } => {
                let e = &self.f[axis];
                let (a, b, sign) = if to >= from[axis] { (from[axis], to, 1.0) } else { (to, from[axis], -1.0) };
                let mut q = from;
                let mut s = 0.0;
                for i in a..b {
                    q[axis] = i;
                    s += e[self.id(q)] as f64 * self.d[axis][i];
                }
                sign * s
            }
            Probe::Current { axis, at, lo, hi } => {
                let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
                let (hb, hc) = (&self.f[3 + b], &self.f[3 + c]);
                let pt = |vb: usize, vc: usize| {
                    let mut q = [0; 3];
                    q[axis] = at;
                    q[b] = vb;
                    q[c] = vc;
                    self.id(q)
                };
                let mut s = 0.0;
                for j in lo[0]..=hi[0] {
                    s += (hb[pt(j, lo[1] - 1)] as f64 - hb[pt(j, hi[1])] as f64) * self.dd[b][j];
                }
                for k in lo[1]..=hi[1] {
                    s += (hc[pt(hi[0], k)] as f64 - hc[pt(lo[0] - 1, k)] as f64) * self.dd[c][k];
                }
                s
            }
        }
    }

    /// Total electromagnetic energy (J).
    pub fn energy(&self) -> f64 {
        let n = self.n;
        let plane = n[0] * n[1];
        let per_plane = |k: usize| -> f64 {
            let mut w = 0.0;
            for comp in 0..6 {
                let a = comp % 3;
                let (b, c) = ((a + 1) % 3, (a + 2) % 3);
                let f = &self.f[comp];
                for j in 0..n[1] {
                    for i in 0..n[0] {
                        let p = [i, j, k];
                        let g = i + n[0] * j + plane * k;
                        let v = f[g] as f64;
                        if v == 0.0 {
                            continue;
                        }
                        if comp < 3 {
                            if p[a] + 1 >= n[a] {
                                continue;
                            }
                            let eps = self.coef[self.tab[a][g] as usize].2 as f64;
                            w += 0.5 * eps * v * v * self.d[a][p[a]] * self.dd[b][p[b]] * self.dd[c][p[c]];
                        } else {
                            if p[b] + 1 >= n[b] || p[c] + 1 >= n[c] {
                                continue;
                            }
                            w += 0.5 * MU0 * v * v * self.dd[a][p[a]] * self.d[b][p[b]] * self.d[c][p[c]];
                        }
                    }
                }
            }
            w
        };
        #[cfg(feature = "parallel")]
        if let Some(pool) = &self.pool {
            use rayon::prelude::*;
            let sums: Vec<f64> = pool.install(|| (0..n[2]).into_par_iter().map(per_plane).collect());
            return sums.iter().sum();
        }
        let sums: Vec<f64> = (0..n[2]).map(per_plane).collect();
        sums.iter().sum()
    }

    /// One timestep: H from E, then E from H with the port sources at `t + dt/2`. `drive` gives
    /// the driven port and its source voltage. Returns the ports' (V, I) at the half step.
    pub fn step(&mut self, drive: Option<(usize, f64)>) -> Vec<(f64, f64)> {
        self.update_h();
        self.update_cpml(false);
        let before: Vec<f64> = self.ports.iter().map(|p| self.port_voltage(p)).collect();
        self.update_e();
        self.update_cpml(true);
        if let Some((k, vs)) = drive
            && let Some(p) = self.ports.get(k)
        {
            let e = &mut self.f[p.axis];
            for &(g, _, kc) in &p.edges {
                e[g] -= kc * vs as f32;
            }
        }
        self.ports
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let v = (before[i] + self.port_voltage(p)) / 2.0;
                let vs = match drive {
                    Some((k, vs)) if k == i => vs,
                    _ => 0.0,
                };
                (v, (vs - v) / p.r)
            })
            .collect()
    }

    /// Current value of a probe (E-sampled probes after the last step, H-sampled ones half a step
    /// earlier).
    pub fn probe(&self, i: usize) -> f64 {
        self.probe_value(&self.probes[i])
    }

    /// Runs from rest with port `drive` excited by the Gaussian pulse (none: no source, e.g. to
    /// test stability), until the energy criterion or `max_steps`.
    pub fn run(&mut self, drive: Option<usize>, hooks: &Hooks<'_>) -> Result<Run, Cancelled> {
        self.reset();
        let freqs = self.settings.freqs.clone();
        let nf = freqs.len();
        let g = self.settings.excitation;
        let dt = self.dt;
        let np = self.ports.len();
        let mut port_v = vec![vec![C64::new(0.0, 0.0); nf]; np];
        let mut port_i = port_v.clone();
        let mut probes = vec![vec![C64::new(0.0, 0.0); nf]; self.probes.len()];
        let mut source = vec![C64::new(0.0, 0.0); nf];
        let omega: Vec<f64> = freqs.iter().map(|f| 2.0 * std::f64::consts::PI * f).collect();
        let (mut wmax, mut w) = (0.0f64, 0.0f64);
        let check = 32u64;
        let max = self.settings.max_steps;
        let decay = 10f64.powf(-self.settings.end_db / 10.0);
        let mut steps = 0;
        let mut converged = false;
        let mut half = vec![C64::new(0.0, 0.0); nf];
        let mut full = vec![C64::new(0.0, 0.0); nf];
        while steps < max {
            let th = (steps as f64 + 0.5) * dt;
            let tf = (steps as f64 + 1.0) * dt;
            let vs = drive.map(|k| (k, g.at(th)));
            let pv = self.step(vs);
            for (fi, om) in omega.iter().enumerate() {
                let (s, c) = (om * th).sin_cos();
                half[fi] = C64::new(c * dt, -s * dt);
                let (s, c) = (om * tf).sin_cos();
                full[fi] = C64::new(c * dt, -s * dt);
            }
            let acc = |a: &mut [C64], x: f64, ph: &[C64]| {
                if x != 0.0 {
                    for (o, p) in a.iter_mut().zip(ph) {
                        o.re += x * p.re;
                        o.im += x * p.im;
                    }
                }
            };
            for (i, (v, cur)) in pv.into_iter().enumerate() {
                acc(&mut port_v[i], v, &half);
                acc(&mut port_i[i], cur, &half);
            }
            if let Some((_, v)) = vs {
                acc(&mut source, v, &half);
            }
            for (i, p) in self.probes.iter().enumerate() {
                let x = self.probe_value(p);
                // Current loops read H, half a step before E.
                let ph = if matches!(p, Probe::Current { .. }) { &half } else { &full };
                acc(&mut probes[i], x, ph);
            }
            steps += 1;
            if steps % check == 0 {
                if (hooks.cancelled)() {
                    return Err(Cancelled);
                }
                w = self.energy();
                wmax = wmax.max(w);
                let db = if wmax > 0.0 { 10.0 * (w / wmax).log10() } else { 0.0 };
                (hooks.progress)(steps, Some(max), &format!("timestep {steps}, energy {db:.1} dB"));
                if !w.is_finite() {
                    break;
                }
                if tf > g.end() && wmax > 0.0 && w <= wmax * decay {
                    converged = true;
                    break;
                }
            }
        }
        let energy_db = if wmax > 0.0 && w > 0.0 { 10.0 * (w / wmax).log10() } else { f64::NEG_INFINITY };
        Ok(Run { steps, converged, energy_db, port_v, port_i, probes, source })
    }
}
