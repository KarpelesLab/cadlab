//! 2D quasi-static field solver for transmission line cross-sections.
//!
//! Method: a boundary element method (method of moments) with piecewise-constant **total**
//! (free plus polarization) charge panels on conductor surfaces and on dielectric interfaces,
//! in a vacuum Green's function that already satisfies the ground planes: the half-space
//! Green's function (one image) below a single plane, and the closed-form Green's function of
//! a line charge between two grounded parallel plates for striplines. Conductor panels enforce
//! the conductor potential, interface panels the continuity of the normal electric flux
//! density. Textbook sources: R. F. Harrington, *Field Computation by Moment Methods* (1968),
//! ch. 2 (charged conductors, piecewise-constant pulses with point matching); C. Wei,
//! R. F. Harrington, J. R. Mautz, T. K. Sarkar, "Multiconductor Transmission Lines in
//! Multilayered Dielectric Media", IEEE MTT-32 (1984) (total charge on conductors and
//! dielectric interfaces in a free-space Green's function); the parallel-plate Green's
//! function, ln[(cosh(πΔx/b) − cos(π(y+y′)/b)) / (cosh(πΔx/b) − cos(π(y−y′)/b))] / 4π, is the
//! summed image series of a line charge between grounded plates (a standard textbook result).
//!
//! The cross-section solved is symmetric about x = 0 (every structure built here is), and the
//! solver works on the right half with mirrored kernels: even parity for symmetric
//! excitations, odd parity for antisymmetric ones (the odd mode of an edge-coupled pair). Panels
//! are graded towards conductor corners (the field singularities) with a size proportional to
//! the distance to the nearest corner. Everything is plain `f64` arithmetic in a fixed order:
//! the result is deterministic.
//!
//! From the capacitance matrix per unit length with the dielectrics (`C`) and in vacuum
//! (`C₀`): the inductance matrix is `L = μ₀ε₀ C₀⁻¹`, the characteristic impedance matrix
//! `Z = C⁻¹ (C L)^½`, a single line's `Z₀ = 1 / (c √(C C₀))` and `εeff = C / C₀`. For pairs,
//! the differential and common impedances are the diagonal of the mixed-mode transform of
//! `Z` (`Zdiff = Z₁₁ − Z₁₂ − Z₂₁ + Z₂₂`, `Zcomm = (Z₁₁ + Z₁₂ + Z₂₁ + Z₂₂) / 4`), which
//! reduce to `2 Z_odd` and `Z_even / 2` for a symmetric pair.
//!
//! Losses ([`Structure::line_matrices`]): the conductance from the partial capacitances of the
//! dielectric regions (derivatives of `C` in their permittivities), and the skin-effect
//! resistance per surface resistance from the perturbation method, ∮|Jₛ|² of the quasi-TEM
//! surface current (the vacuum surface charge, on the conductors and induced on the planes).
//!
//! All lengths are in one unit (millimeters by convention); only ratios matter, except for
//! [`LineMatrices::r_over_rs`], which takes millimeters.

use std::f64::consts::PI;

use super::complex::C64;
use super::impedance::{ETA0, Line, Model};

/// Speed of light in vacuum, m/s.
const C0: f64 = 299_792_458.0;
/// Vacuum permittivity, F/m.
const EPS0: f64 = 1.0 / (ETA0 * C0);

/// A dielectric layer of the cross-section, listed bottom to top from the ground plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layer {
    /// Thickness.
    pub thickness: f64,
    /// Relative permittivity.
    pub er: f64,
}

/// What a conductor is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Signal conductor number `n` (numbered from 0, without gaps).
    Signal(usize),
    /// Grounded (coplanar ground), at the potential of the planes.
    Ground,
}

/// A conductor: a trapezoid standing on `y`, bottom from `x0` to `x1`, `t` thick, its top edge
/// inset by `etch` on each side (a sheet when `t` is 0). `x0` may be `-∞` and `x1` `+∞` (a
/// coplanar ground plane extending sideways; the solver truncates it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Conductor {
    /// Left end of the bottom edge.
    pub x0: f64,
    /// Right end of the bottom edge.
    pub x1: f64,
    /// Height of the bottom edge above the ground plane.
    pub y: f64,
    /// Thickness (0: an infinitely thin sheet).
    pub t: f64,
    /// Inset of the top edge on each side (trapezoidal etch).
    pub etch: f64,
    /// Signal or ground.
    pub role: Role,
}

/// Solder mask over the top surface: `thickness` over the laminate, `over_copper` over the
/// copper, following the conductors' sides.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mask {
    /// Thickness over the laminate.
    pub thickness: f64,
    /// Thickness over the copper.
    pub over_copper: f64,
    /// Relative permittivity.
    pub er: f64,
}

/// A cross-section: dielectric layers on a ground plane at y = 0, an optional second plane on
/// top of the layers, conductors and an optional solder mask. Must be symmetric about x = 0.
#[derive(Clone, Debug, PartialEq)]
pub struct CrossSection {
    /// Dielectric layers, bottom to top.
    pub layers: Vec<Layer>,
    /// A ground plane on top of the last layer (stripline); otherwise air above.
    pub top_plane: bool,
    /// Conductors.
    pub conductors: Vec<Conductor>,
    /// Solder mask on the top surface (not with a top plane).
    pub mask: Option<Mask>,
}

/// Discretization controls. The defaults are within about 0.05 % of the converged values (and
/// of the exact results of the analytic test cases).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Accuracy {
    /// Panel size relative to the distance to the nearest corner.
    pub grading: f64,
    /// Smallest panel at a conductor corner, relative to the smallest conductor feature (width,
    /// thickness, gap, distance to a layer boundary).
    pub fine: f64,
}

impl Default for Accuracy {
    fn default() -> Self {
        Accuracy { grading: 0.25, fine: 0.001 }
    }
}

/// Why a cross-section cannot be solved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolveError(pub String);

impl std::fmt::Display for SolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Capacitance matrices per unit length, in units of ε₀ (multiply by ε₀ for F/m).
#[derive(Clone, Debug, PartialEq)]
pub struct Capacitance {
    /// Maxwell capacitance matrix with the dielectrics, `[signal][signal]`.
    pub c: Vec<Vec<f64>>,
    /// The same in vacuum (gives the inductance).
    pub c0: Vec<Vec<f64>>,
    /// Number of unknowns of the dielectric solve (half structure).
    pub panels: usize,
}

/// Single line from the field solver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldLine {
    /// Characteristic impedance, Ω.
    pub z0: f64,
    /// Effective permittivity.
    pub er_eff: f64,
    /// Capacitance per meter, F/m.
    pub c: f64,
    /// Inductance per meter, H/m.
    pub l: f64,
}

impl FieldLine {
    /// As a [`Line`].
    pub fn line(&self) -> Line {
        Line { z0: self.z0, er_eff: self.er_eff }
    }
}

/// Coupled pair from the field solver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldPair {
    /// Odd-mode impedance (Zdiff / 2), Ω.
    pub z_odd: f64,
    /// Even-mode impedance (2 Zcomm), Ω.
    pub z_even: f64,
    /// Differential impedance, Ω.
    pub zdiff: f64,
    /// Common-mode impedance, Ω.
    pub zcomm: f64,
    /// Effective permittivity of the odd mode (antiphase excitation).
    pub er_eff_odd: f64,
    /// Effective permittivity of the even mode (in-phase excitation).
    pub er_eff_even: f64,
    /// Impedance of trace 0 alone with trace 1 held at ground potential, √(L₁₁ / C₁₁) where
    /// L₁₁ is the diagonal of the inductance matrix (an indication, not a mode impedance).
    pub z_self: f64,
}

impl Capacitance {
    /// Single line (signal 0).
    pub fn line(&self) -> FieldLine {
        let (c, c0) = (self.c[0][0], self.c0[0][0]);
        FieldLine { z0: ETA0 / (c * c0).sqrt(), er_eff: c / c0, c: c * EPS0, l: 1.0 / (C0 * C0 * EPS0 * c0) }
    }

    /// Pair of signals 0 and 1.
    pub fn pair(&self) -> FieldPair {
        let c = [[self.c[0][0], self.c[0][1]], [self.c[1][0], self.c[1][1]]];
        let c0 = [[self.c0[0][0], self.c0[0][1]], [self.c0[1][0], self.c0[1][1]]];
        // Z = η₀ C⁻¹ (C C₀⁻¹)^½, in units where ε₀ = 1.
        let m = mul2(c, inv2(c0));
        let z = mul2(inv2(c), sqrt2(m));
        let z = [[ETA0 * z[0][0], ETA0 * z[0][1]], [ETA0 * z[1][0], ETA0 * z[1][1]]];
        let zdiff = z[0][0] - z[0][1] - z[1][0] + z[1][1];
        let zcomm = (z[0][0] + z[0][1] + z[1][0] + z[1][1]) / 4.0;
        let co = (c[0][0] + c[1][1]) / 2.0 - (c[0][1] + c[1][0]) / 2.0;
        let c0o = (c0[0][0] + c0[1][1]) / 2.0 - (c0[0][1] + c0[1][0]) / 2.0;
        let ce = (c[0][0] + c[1][1]) / 2.0 + (c[0][1] + c[1][0]) / 2.0;
        let c0e = (c0[0][0] + c0[1][1]) / 2.0 + (c0[0][1] + c0[1][0]) / 2.0;
        let l = inv2(c0);
        FieldPair {
            z_odd: zdiff / 2.0,
            z_even: 2.0 * zcomm,
            zdiff,
            zcomm,
            er_eff_odd: co / c0o,
            er_eff_even: ce / c0e,
            z_self: ETA0 * (l[0][0] / c[0][0]).sqrt(),
        }
    }
}

pub(crate) type M2 = [[f64; 2]; 2];

pub(crate) fn mul2(a: M2, b: M2) -> M2 {
    [
        [a[0][0] * b[0][0] + a[0][1] * b[1][0], a[0][0] * b[0][1] + a[0][1] * b[1][1]],
        [a[1][0] * b[0][0] + a[1][1] * b[1][0], a[1][0] * b[0][1] + a[1][1] * b[1][1]],
    ]
}

pub(crate) fn inv2(a: M2) -> M2 {
    let d = a[0][0] * a[1][1] - a[0][1] * a[1][0];
    [[a[1][1] / d, -a[0][1] / d], [-a[1][0] / d, a[0][0] / d]]
}

/// Principal square root of a 2×2 matrix with positive real eigenvalues:
/// √M = (M + s I) / √(tr M + 2 s), s = √det M.
pub(crate) fn sqrt2(m: M2) -> M2 {
    let s = (m[0][0] * m[1][1] - m[0][1] * m[1][0]).sqrt();
    let t = (m[0][0] + m[1][1] + 2.0 * s).sqrt();
    [[(m[0][0] + s) / t, m[0][1] / t], [m[1][0] / t, (m[1][1] + s) / t]]
}

// ---------------------------------------------------------------------------------------------
// Structures built from a stackup description

/// A line structure: the stackup around one trace (or a pair), from which [`CrossSection`]s
/// of any width are built.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Structure {
    /// Microstrip (air or mask above), embedded microstrip (covered), stripline (second plane).
    pub model: Model,
    /// Dielectric between the plane and the (lower) trace.
    pub h: f64,
    /// Its permittivity.
    pub er: f64,
    /// Embedded microstrip: cover above the (upper) trace's top; stripline: dielectric from the
    /// (upper) trace's top to the second plane. Unused for a surface microstrip.
    pub h2: f64,
    /// Permittivity of the dielectric around and above the (upper) trace (embedded, stripline).
    pub er2: f64,
    /// Copper thickness.
    pub t: f64,
    /// Top-edge inset per side (trapezoidal etch; the given width is the bottom width).
    pub etch: f64,
    /// Broadside-coupled pair: dielectric thickness between the two traces and its εr. The
    /// second trace stands that far above the first one's top.
    pub broadside: Option<(f64, f64)>,
    /// Coplanar ground on both sides at this gap (grounded coplanar waveguide).
    pub coplanar_gap: Option<f64>,
    /// Solder mask (surface microstrip: conformal; embedded: a flat layer on the cover).
    pub mask: Option<Mask>,
}

impl Structure {
    /// The cross-section of a single trace (`gap` None) or an edge-coupled pair of width `w`
    /// (or, with `broadside`, the broadside pair; `gap` must then be None).
    pub fn cross_section(&self, w: f64, gap: Option<f64>) -> Result<CrossSection, SolveError> {
        let t = self.t;
        let mut layers = vec![Layer { thickness: self.h, er: self.er }];
        let mut conductors = Vec::new();
        let trace =
            |x0: f64, x1: f64, y: f64, n: usize| Conductor { x0, x1, y, t, etch: self.etch, role: Role::Signal(n) };
        match (self.broadside, gap) {
            (Some(_), Some(_)) => return Err(SolveError("a pair is either edge-coupled (gap) or broadside".into())),
            (Some((s, er)), None) => {
                if self.coplanar_gap.is_some() {
                    return Err(SolveError("coplanar grounds are not supported with a broadside pair".into()));
                }
                conductors.push(trace(-w / 2.0, w / 2.0, self.h, 0));
                layers.push(Layer { thickness: t + s, er });
                conductors.push(trace(-w / 2.0, w / 2.0, self.h + t + s, 1));
            }
            (None, Some(g)) => {
                conductors.push(trace(-g / 2.0 - w, -g / 2.0, self.h, 0));
                conductors.push(trace(g / 2.0, g / 2.0 + w, self.h, 1));
            }
            (None, None) => conductors.push(trace(-w / 2.0, w / 2.0, self.h, 0)),
        }
        if let Some(cg) = self.coplanar_gap {
            let ext = gap.map_or(w / 2.0, |g| g / 2.0 + w) + cg;
            let ground = |x0, x1| Conductor { x0, x1, y: self.h, t, etch: self.etch, role: Role::Ground };
            conductors.push(ground(f64::NEG_INFINITY, -ext));
            conductors.push(ground(ext, f64::INFINITY));
        }
        let top_plane = match self.model {
            Model::Microstrip => false,
            Model::EmbeddedMicrostrip => {
                layers.push(Layer { thickness: t + self.h2, er: self.er2 });
                false
            }
            Model::Stripline => {
                layers.push(Layer { thickness: t + self.h2, er: self.er2 });
                true
            }
        };
        Ok(CrossSection { layers, top_plane, conductors, mask: self.mask })
    }

    /// Single trace of width `w` (with `broadside`, the lower trace alone, in the same layers).
    pub fn line(&self, w: f64, acc: Accuracy) -> Result<FieldLine, SolveError> {
        let mut cs = self.cross_section(w, None)?;
        cs.conductors.retain(|c| c.role != Role::Signal(1));
        Ok(solve(&cs, acc)?.line())
    }

    /// Pair of traces of width `w`: edge-coupled at `gap`, or broadside (`gap` None).
    pub fn pair(&self, w: f64, gap: Option<f64>, acc: Accuracy) -> Result<FieldPair, SolveError> {
        if gap.is_none() && self.broadside.is_none() {
            return Err(SolveError("a pair needs a gap or a broadside separation".into()));
        }
        Ok(solve(&self.cross_section(w, gap)?, acc)?.pair())
    }

    /// Whether this describes a pair (edge-coupled with `gap`, or broadside).
    pub fn is_pair(&self, gap: Option<f64>) -> bool {
        gap.is_some() || self.broadside.is_some()
    }

    /// The single trace, and the pair when there is one.
    pub fn evaluate(&self, w: f64, gap: Option<f64>, acc: Accuracy) -> Result<Evaluation, SolveError> {
        let line = self.line(w, acc)?;
        let pair = if self.is_pair(gap) { Some(self.pair(w, gap, acc)?) } else { None };
        Ok(Evaluation { line, pair })
    }

    /// The impedance a width is solved for: differential for a pair, else single-ended.
    pub fn impedance(&self, w: f64, gap: Option<f64>, acc: Accuracy) -> Result<f64, SolveError> {
        if self.is_pair(gap) { Ok(self.pair(w, gap, acc)?.zdiff) } else { Ok(self.line(w, acc)?.z0) }
    }

    /// Smallest and largest width searched by [`Structure::solve_width`].
    pub fn width_range(&self) -> (f64, f64) {
        ((self.h / 1000.0).max(2.0 * self.etch * 1.01), 100.0 * self.h)
    }

    /// The width (between [`Structure::width_range`]) giving impedance `target`, searched from
    /// `start` (e.g. the closed-form width): impedance falls monotonically with width. A first
    /// search runs at a coarser discretization, a second one refines its result at `acc`; each
    /// brackets by growing steps on a logarithmic scale, then applies regula falsi (Illinois
    /// variant). `None` when the target is out of reach.
    pub fn solve_width(
        &self,
        target: f64,
        gap: Option<f64>,
        start: f64,
        acc: Accuracy,
    ) -> Result<Option<f64>, SolveError> {
        let coarse = Accuracy { grading: (2.0 * acc.grading).min(0.6), fine: 10.0 * acc.fine };
        let Some(w) = self.search(target, gap, start, coarse, 0.05, 5e-4)? else { return Ok(None) };
        self.search(target, gap, w, acc, 0.004, 2e-5)
    }

    fn search(
        &self,
        target: f64,
        gap: Option<f64>,
        start: f64,
        acc: Accuracy,
        step: f64,
        tol: f64,
    ) -> Result<Option<f64>, SolveError> {
        let (wmin, wmax) = self.width_range();
        let (lmin, lmax) = (wmin.ln(), wmax.ln());
        let f = |lw: f64| -> Result<f64, SolveError> { Ok(self.impedance(lw.exp(), gap, acc)? - target) };
        let done = |fx: f64| fx.abs() <= tol * target;
        let mut a = start.clamp(wmin, wmax).ln();
        let mut fa = f(a)?;
        if done(fa) {
            return Ok(Some(a.exp()));
        }
        // Too high an impedance: widen; too low: narrow.
        let dir = if fa > 0.0 { 1.0 } else { -1.0 };
        let mut step = step;
        let (mut b, mut fb);
        loop {
            b = (a + dir * step).clamp(lmin, lmax);
            fb = f(b)?;
            if done(fb) {
                return Ok(Some(b.exp()));
            }
            if (fb > 0.0) != (fa > 0.0) {
                break;
            }
            if b == lmin || b == lmax {
                return Ok(None);
            }
            a = b;
            fa = fb;
            step *= 2.0;
        }
        let mut side = 0;
        for _ in 0..60 {
            let c = (a * fb - b * fa) / (fb - fa);
            let fc = f(c)?;
            if done(fc) || (b - a).abs() < 1e-6 {
                return Ok(Some(c.exp()));
            }
            if (fc > 0.0) == (fb > 0.0) {
                b = c;
                fb = fc;
                if side == -1 {
                    fa /= 2.0;
                }
                side = -1;
            } else {
                a = c;
                fa = fc;
                if side == 1 {
                    fb /= 2.0;
                }
                side = 1;
            }
        }
        Ok(Some((0.5 * (a + b)).exp()))
    }
}

/// A trace, and its pair when there is one, from the field solver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Evaluation {
    /// The single trace (alone; with a broadside pair, the lower trace alone).
    pub line: FieldLine,
    /// The pair.
    pub pair: Option<FieldPair>,
}

// ---------------------------------------------------------------------------------------------
// Geometry preparation

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// Surface of a thick conductor (index into the half-structure conductors), medium outside.
    Conductor { c: usize, eps: f64 },
    /// Zero-thickness conductor: media on the negative and positive (left-normal) side.
    Sheet { c: usize, neg: f64, pos: f64 },
    /// Dielectric interface.
    Interface { neg: f64, pos: f64 },
}

#[derive(Clone, Copy, Debug)]
struct Panel {
    a: [f64; 2],
    b: [f64; 2],
    m: [f64; 2],
    n: [f64; 2],
    len: f64,
    kind: Kind,
}

#[derive(Clone, Copy, Debug)]
struct Seg {
    a: [f64; 2],
    b: [f64; 2],
    /// Conductor index in the half structure, or None for an interface.
    cond: Option<usize>,
    sheet: bool,
}

/// How a conductor of the full structure maps onto the half structure.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Sym {
    /// Straddles x = 0: its own mirror image.
    Centered,
    /// In the right half; its mirror partner is the given full-structure index.
    Right(usize),
    /// In the left half (mirror of the given conductor).
    Left(usize),
}

struct Prepared {
    segs: Vec<Seg>,
    /// Corners the panels grade towards, with the smallest panel each needs.
    corners: Vec<([f64; 2], f64)>,
    /// Full-structure conductor index of each half-structure conductor.
    half: Vec<usize>,
    sym: Vec<Sym>,

    plane_b: Option<f64>,
    envelope: Option<Vec<[f64; 2]>>,
    fine: f64,
    hmax: f64,
    /// Where interfaces and coplanar grounds are truncated.
    xmax: f64,
}

fn finite_or(x: f64, clip: f64) -> f64 {
    if x.is_finite() { x } else { clip.copysign(x) }
}

fn conductor_vertices(c: &Conductor, xmax: f64) -> [[f64; 2]; 4] {
    let (x0, x1) = (finite_or(c.x0, xmax), finite_or(c.x1, xmax));
    let e0 = if c.x0.is_finite() { c.etch } else { 0.0 };
    let e1 = if c.x1.is_finite() { c.etch } else { 0.0 };
    [[x0, c.y], [x1, c.y], [x1 - e1, c.y + c.t], [x0 + e0, c.y + c.t]]
}

/// Clips segment a→b to 0 ≤ x ≤ xmax; drops pieces lying on x = 0 or x = xmax.
fn clip(a: [f64; 2], b: [f64; 2], xmax: f64) -> Option<([f64; 2], [f64; 2])> {
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    let dx = b[0] - a[0];
    for (p, q) in [(-dx, a[0]), (dx, xmax - a[0])] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    if t1 - t0 <= 1e-12 {
        return None;
    }
    let at = |t: f64| [a[0] + dx * t, a[1] + (b[1] - a[1]) * t];
    let (p, q) = (at(t0), at(t1));
    let tol = 1e-12 * xmax;
    if (p[0].abs() <= tol && q[0].abs() <= tol) || ((p[0] - xmax).abs() <= tol && (q[0] - xmax).abs() <= tol) {
        return None;
    }
    Some((p, q))
}

fn check(cs: &CrossSection) -> Result<(), SolveError> {
    let bad = |m: &str| Err(SolveError(m.to_string()));
    if cs.layers.is_empty() {
        return bad("no dielectric layer");
    }
    for l in &cs.layers {
        if !(l.thickness > 0.0 && l.thickness.is_finite()) || !(l.er >= 1.0 && l.er.is_finite()) {
            return bad("dielectric layers need a positive thickness and εr ≥ 1");
        }
    }
    let stack: f64 = cs.layers.iter().map(|l| l.thickness).sum();
    let mut signals = Vec::new();
    for c in &cs.conductors {
        if c.x1.is_nan()
            || c.x0.is_nan()
            || c.x1 <= c.x0
            || c.t.is_nan()
            || c.t < 0.0
            || c.etch.is_nan()
            || c.etch < 0.0
            || c.y.is_nan()
            || c.y <= 0.0
        {
            return bad(
                "conductors need a positive width, a non-negative thickness and etch, and to stand above the plane",
            );
        }
        if c.t > 0.0 && c.x0.is_finite() && c.x1.is_finite() && c.x1 - c.x0 <= 2.0 * c.etch {
            return bad("the etch leaves no top surface (the top width must stay positive)");
        }
        if cs.top_plane && c.y + c.t >= stack {
            return bad("a conductor reaches the top plane");
        }
        if let Role::Signal(n) = c.role {
            if !c.x0.is_finite() || !c.x1.is_finite() {
                return bad("signal conductors must be finite");
            }
            signals.push(n);
        }
    }
    signals.sort_unstable();
    if signals.is_empty() || signals.iter().enumerate().any(|(i, &n)| i != n) {
        return bad("signal conductors must be numbered 0, 1, ... once each");
    }
    if let Some(m) = &cs.mask {
        if cs.top_plane {
            return bad("a solder mask needs an open top surface");
        }
        if !(m.thickness > 0.0 && m.over_copper > 0.0 && m.er >= 1.0) {
            return bad("the solder mask needs positive thicknesses and εr ≥ 1");
        }
    }
    Ok(())
}

fn symmetry(cs: &CrossSection) -> Result<Vec<Sym>, SolveError> {
    let tol = |a: f64, b: f64| {
        if a.is_infinite() || b.is_infinite() { a == b } else { (a - b).abs() <= 1e-9 * (1.0 + a.abs().max(b.abs())) }
    };
    let mut out = Vec::new();
    for c in &cs.conductors {
        if tol(c.x0, -c.x1) {
            out.push(Sym::Centered);
            continue;
        }
        let partner = cs.conductors.iter().position(|d| {
            tol(d.x0, -c.x1) && tol(d.x1, -c.x0) && tol(d.y, c.y) && tol(d.t, c.t) && tol(d.etch, c.etch)
        });
        let Some(p) = partner else {
            return Err(SolveError("the cross-section must be symmetric about x = 0".into()));
        };
        if (c.role == Role::Ground) != (cs.conductors[p].role == Role::Ground) {
            return Err(SolveError("mirror conductors must both be grounds or both signals".into()));
        }
        out.push(if c.x0 + c.x1 > 0.0 { Sym::Right(p) } else { Sym::Left(p) });
    }
    Ok(out)
}

/// Upper envelope of piecewise-linear functions over [x_lo, x_hi]; each function is a
/// polyline with increasing x, `base` outside its range.
fn envelope(funcs: &[Vec<[f64; 2]>], base: f64, x_lo: f64, x_hi: f64) -> Vec<[f64; 2]> {
    let eval = |f: &Vec<[f64; 2]>, x: f64| -> f64 {
        if x <= f[0][0] || x >= f[f.len() - 1][0] {
            return base;
        }
        for w in f.windows(2) {
            if x <= w[1][0] {
                let t = (x - w[0][0]) / (w[1][0] - w[0][0]);
                return w[0][1] + t * (w[1][1] - w[0][1]);
            }
        }
        base
    };
    let mut xs = vec![x_lo, x_hi];
    for f in funcs {
        xs.extend(f.iter().map(|p| p[0]).filter(|&x| x > x_lo && x < x_hi));
    }
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, b| (*a - *b).abs() <= 1e-15 * (1.0 + b.abs()));
    let value = |x: f64| funcs.iter().map(|f| eval(f, x)).fold(base, f64::max);
    let mut pts: Vec<[f64; 2]> = vec![[xs[0], value(xs[0])]];
    for w in xs.windows(2) {
        let (xa, xb) = (w[0], w[1]);
        // Lines of every function on this interval (base included).
        let mut lines: Vec<(f64, f64)> = vec![(base, base)];
        for f in funcs {
            lines.push((eval(f, xa + 1e-12 * (xb - xa)), eval(f, xb - 1e-12 * (xb - xa))));
        }
        // Each line: value at xa and xb (evaluated just inside, exact for linear pieces).
        let line_at = |l: (f64, f64), x: f64| l.0 + (l.1 - l.0) * (x - xa) / (xb - xa);
        let mut x = xa;
        let mut cur = (0..lines.len())
            .max_by(|&i, &j| {
                let (vi, vj) = (line_at(lines[i], xa), line_at(lines[j], xa));
                vi.total_cmp(&vj).then((lines[i].1 - lines[i].0).total_cmp(&(lines[j].1 - lines[j].0))).then(j.cmp(&i))
            })
            .expect("non-empty");
        loop {
            let sc = lines[cur].1 - lines[cur].0;
            let mut best: Option<(f64, usize)> = None;
            for (k, &l) in lines.iter().enumerate() {
                let sk = l.1 - l.0;
                if sk <= sc {
                    continue;
                }
                // Intersection: l.0 + sk u = cur.0 + sc u.
                let u = (lines[cur].0 - l.0) / (sk - sc);
                let xi = xa + u * (xb - xa);
                if xi > x + 1e-15 * (xb - xa) && xi < xb && best.is_none_or(|(bx, _)| xi < bx) {
                    best = Some((xi, k));
                }
            }
            match best {
                Some((xi, k)) => {
                    pts.push([xi, line_at(lines[cur], xi)]);
                    x = xi;
                    cur = k;
                }
                None => break,
            }
        }
        pts.push([xb, value(xb).max(line_at(lines[cur], xb))]);
    }
    // Drop collinear middle points.
    let mut out: Vec<[f64; 2]> = Vec::with_capacity(pts.len());
    for p in pts {
        if let Some(last) = out.last()
            && (p[0] - last[0]).abs() <= 1e-15 * (1.0 + p[0].abs())
        {
            continue;
        }
        while out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let cross = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
            if cross.abs() <= 1e-12 * ((p[0] - a[0]).abs() + (p[1] - a[1]).abs()).powi(2) {
                out.pop();
            } else {
                break;
            }
        }
        out.push(p);
    }
    out
}

fn prepare(cs: &CrossSection, acc: Accuracy) -> Result<Prepared, SolveError> {
    check(cs)?;
    let sym = symmetry(cs)?;
    let stack: f64 = cs.layers.iter().map(|l| l.thickness).sum();
    let plane_b = cs.top_plane.then_some(stack);
    let mut mask = cs.mask;
    let mut layers = cs.layers.clone();
    let on_top = |c: &Conductor| (c.y - stack).abs() <= 1e-9 * stack;
    if let Some(m) = mask
        && !cs.conductors.iter().any(on_top)
    {
        layers.push(Layer { thickness: m.thickness, er: m.er });
        mask = None;
    }
    let total: f64 = layers.iter().map(|l| l.thickness).sum::<f64>()
        + mask.map_or(0.0, |m| m.thickness.max(m.over_copper + cs.conductors.iter().map(|c| c.t).fold(0.0, f64::max)));

    // Extent and smallest feature.
    let mut ext = 0.0f64;
    let mut feat = f64::INFINITY;
    for l in &layers {
        feat = feat.min(l.thickness);
    }
    for c in &cs.conductors {
        for x in [c.x0, c.x1] {
            if x.is_finite() {
                ext = ext.max(x.abs());
            }
        }
        if c.x0.is_finite() && c.x1.is_finite() {
            feat = feat.min(c.x1 - c.x0);
        }
        if c.t > 0.0 {
            feat = feat.min(c.t);
        }
        if c.etch > 0.0 {
            feat = feat.min(c.etch);
        }
    }
    for (i, a) in cs.conductors.iter().enumerate() {
        for b in &cs.conductors[i + 1..] {
            if (a.y - b.y).abs() < 1e-12 {
                let g = if a.x1 <= b.x0 { b.x0 - a.x1 } else { a.x0 - b.x1 };
                if g > 0.0 {
                    feat = feat.min(g);
                }
            } else {
                let g = if a.y < b.y { b.y - (a.y + a.t) } else { a.y - (b.y + b.t) };
                if g > 0.0 {
                    feat = feat.min(g);
                }
            }
        }
        // Distance to the layer boundaries above and below the conductor.
        let mut acc_y = 0.0;
        for l in &layers {
            acc_y += l.thickness;
            for d in [acc_y - a.y, acc_y - (a.y + a.t)] {
                if d.abs() > 1e-12 * stack {
                    feat = feat.min(d.abs());
                }
            }
        }
    }
    // The mask surface's corners are dielectric corners (weak singularities): panels there
    // need not go below a tenth of the mask thickness.
    let weak = mask.map_or(0.0, |m| 0.1 * m.thickness.min(m.over_copper));
    let xmax = match plane_b {
        Some(b) => ext + 8.0 * b,
        None => ext + 60.0 * total,
    };
    let fine = acc.fine * feat;
    let hmax = match plane_b {
        Some(b) => 0.25 * b,
        None => xmax / 12.0,
    };

    // Half-structure conductors.
    let mut half = Vec::new();
    let mut map = vec![usize::MAX; cs.conductors.len()];
    for (i, s) in sym.iter().enumerate() {
        if !matches!(s, Sym::Left(_)) {
            map[i] = half.len();
            half.push(i);
        }
    }
    let mut segs = Vec::new();
    let mut corners = Vec::new();
    for (i, c) in cs.conductors.iter().enumerate() {
        let v = conductor_vertices(c, xmax);
        for (k, p) in v.iter().enumerate() {
            let far = (k == 0 || k == 3) && !c.x0.is_finite() || (k == 1 || k == 2) && !c.x1.is_finite();
            if !far && (c.t > 0.0 || k < 2) {
                corners.push((*p, 0.0));
            }
        }
        if map[i] == usize::MAX {
            continue;
        }
        let edges: Vec<([f64; 2], [f64; 2])> =
            if c.t > 0.0 { (0..4).map(|k| (v[k], v[(k + 1) % 4])).collect() } else { vec![(v[0], v[1])] };
        for (a, b) in edges {
            if let Some((a, b)) = clip(a, b, xmax) {
                segs.push(Seg { a, b, cond: Some(map[i]), sheet: c.t == 0.0 });
            }
        }
    }
    // Horizontal layer boundaries (not the planes), minus what conductors cover.
    let mut boundaries = Vec::new();
    let mut acc_y = 0.0;
    for (k, l) in layers.iter().enumerate() {
        acc_y += l.thickness;
        if k + 1 < layers.len() || plane_b.is_none() {
            boundaries.push(acc_y);
        }
    }
    for y in boundaries {
        let mut covered: Vec<(f64, f64)> = Vec::new();
        for c in &cs.conductors {
            let tol = 1e-12 * stack.max(1.0);
            if y < c.y - tol || y > c.y + c.t + tol {
                continue;
            }
            let v = conductor_vertices(c, xmax);
            let f = if c.t > 0.0 { ((y - c.y) / c.t).clamp(0.0, 1.0) } else { 0.0 };
            let xl = v[0][0] + (v[3][0] - v[0][0]) * f;
            let xr = v[1][0] + (v[2][0] - v[1][0]) * f;
            covered.push((xl, xr));
        }
        covered.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut x = 0.0f64;
        for (l, r) in covered.iter().copied().chain(std::iter::once((xmax, xmax))) {
            if l > x {
                segs.push(Seg { a: [x, y], b: [l.min(xmax), y], cond: None, sheet: false });
            }
            x = x.max(r);
            if x >= xmax {
                break;
            }
        }
    }
    // Solder mask surface over conductors on the top surface.
    let mut env_pts = None;
    if let Some(m) = mask {
        let base = stack + m.thickness;
        let mut funcs = Vec::new();
        for c in cs.conductors.iter().filter(|c| on_top(c)) {
            let top = stack + c.t + m.over_copper;
            let rise = top - base;
            if rise <= 0.0 {
                continue;
            }
            let v = conductor_vertices(c, 2.0 * xmax);
            let run_l = if c.t > 0.0 { (c.etch * rise / c.t).max(0.5 * rise) } else { 0.5 * rise };
            let (pl, pr) = (v[3][0] - m.over_copper, v[2][0] + m.over_copper);
            funcs.push(vec![[pl - run_l, base], [pl, top], [pr, top], [pr + run_l, base]]);
        }
        let e = envelope(&funcs, base, 0.0, xmax);
        for w in e.windows(2) {
            segs.push(Seg { a: w[0], b: w[1], cond: None, sheet: false });
        }
        for p in &e[1..e.len() - 1] {
            corners.push((*p, weak));
            corners.push(([-p[0], p[1]], weak));
        }
        env_pts = Some(e);
    }
    Ok(Prepared { segs, corners, half, sym, plane_b, envelope: env_pts, fine, hmax, xmax })
}

impl Prepared {
    fn eps_at(&self, cs_layers: &[Layer], mask: Option<Mask>, p: [f64; 2]) -> f64 {
        let mut acc = 0.0;
        for l in cs_layers {
            acc += l.thickness;
            if p[1] < acc {
                return l.er;
            }
        }
        if let (Some(env), Some(m)) = (&self.envelope, mask) {
            let x = p[0].abs();
            for w in env.windows(2) {
                if x <= w[1][0] {
                    let t = (x - w[0][0]) / (w[1][0] - w[0][0]);
                    if p[1] < w[0][1] + t * (w[1][1] - w[0][1]) {
                        return m.er;
                    }
                    break;
                }
            }
        }
        1.0
    }

    fn size(&self, p: [f64; 2], grading: f64) -> f64 {
        let s = self
            .corners
            .iter()
            .map(|(c, min)| (grading * ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt()).max(*min))
            .fold(f64::INFINITY, f64::min);
        s.clamp(self.fine, self.hmax.max(self.fine))
    }

    /// Breakpoints (fractions of the segment) so that each panel is about `size` long.
    fn split(&self, s: &Seg, grading: f64) -> Vec<f64> {
        let len = ((s.b[0] - s.a[0]).powi(2) + (s.b[1] - s.a[1]).powi(2)).sqrt();
        let at = |u: f64| [s.a[0] + (s.b[0] - s.a[0]) * u / len, s.a[1] + (s.b[1] - s.a[1]) * u / len];
        let mut table = vec![(0.0f64, 0.0f64)];
        let (mut u, mut f) = (0.0f64, 0.0f64);
        let mut h0 = self.size(at(0.0), grading);
        while u < len {
            let du = (0.2 * h0).min(len - u);
            let h1 = self.size(at(u + du), grading);
            f += du * 0.5 * (1.0 / h0 + 1.0 / h1);
            u += du;
            h0 = h1;
            table.push((u, f));
        }
        let n = (f.ceil() as usize).max(1);
        let mut out = vec![0.0];
        let mut k = 0;
        for i in 1..n {
            let target = f * i as f64 / n as f64;
            while table[k + 1].1 < target {
                k += 1;
            }
            let (u0, f0) = table[k];
            let (u1, f1) = table[k + 1];
            out.push((u0 + (u1 - u0) * (target - f0) / (f1 - f0)) / len);
        }
        out.push(1.0);
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Kernels

/// ∫ ln|r − p| ds over the segment a→b and its gradient with respect to r. With `own`, r is
/// the segment's midpoint and the normal derivative is the principal value (0).
fn seg_log(r: [f64; 2], a: [f64; 2], b: [f64; 2], own: bool) -> (f64, [f64; 2]) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt();
    let t = [dx / len, dy / len];
    let n = [-t[1], t[0]];
    let (rx, ry) = (r[0] - a[0], r[1] - a[1]);
    let xi = rx * t[0] + ry * t[1];
    let eta = rx * n[0] + ry * n[1];
    let r1 = (xi * xi + eta * eta).sqrt();
    let r2 = ((xi - len) * (xi - len) + eta * eta).sqrt();
    let l1 = if r1 > 0.0 { r1.ln() } else { 0.0 };
    let l2 = if r2 > 0.0 { r2.ln() } else { 0.0 };
    let theta = if own {
        0.0
    } else {
        // Angle subtended by the segment, signed with η.
        let (v1x, v1y) = (a[0] - r[0], a[1] - r[1]);
        let (v2x, v2y) = (b[0] - r[0], b[1] - r[1]);
        (v1x * v2y - v1y * v2x).atan2(v1x * v2x + v1y * v2y)
    };
    let i = xi * l1 - (xi - len) * l2 - len + eta * theta;
    let dxi = l1 - l2;
    (i, [t[0] * dxi + n[0] * theta, t[1] * dxi + n[1] * theta])
}

const GAUSS4: [(f64, f64); 4] = [
    (-0.861_136_311_594_052_6, 0.347_854_845_137_453_9),
    (-0.339_981_043_584_856_3, 0.652_145_154_862_546_1),
    (0.339_981_043_584_856_3, 0.652_145_154_862_546_1),
    (0.861_136_311_594_052_6, 0.347_854_845_137_453_9),
];

/// Smooth remainder of the parallel-plate Green's function (plates at y = 0 and y = b) after
/// removing the direct term and the two nearest images, and its gradient in r.
fn pp_remainder(r: [f64; 2], s: [f64; 2], b: f64) -> (f64, [f64; 2]) {
    let k = PI / b;
    let dx = r[0] - s[0];
    let u = k * dx;
    let vm = k * (r[1] - s[1]);
    let vp = k * (r[1] + s[1]);
    let sh = (0.5 * u).sinh();
    let d = 2.0 * (sh * sh + (0.5 * vm).sin().powi(2));
    let nn = 2.0 * (sh * sh + (0.5 * vp).sin().powi(2));
    let ya = -s[1];
    let yb = 2.0 * b - s[1];
    let rd2 = dx * dx + (r[1] - s[1]).powi(2);
    let ra2 = dx * dx + (r[1] - ya).powi(2);
    let rb2 = dx * dx + (r[1] - yb).powi(2);
    let q = 1.0 / (4.0 * PI);
    let val = q * (nn.ln() - d.ln() + rd2.ln() - ra2.ln() - rb2.ln());
    let su = u.sinh();
    let gx = q * k * su * (1.0 / nn - 1.0 / d);
    let gy = q * k * (vp.sin() / nn - vm.sin() / d);
    // Gradient of the removed singular part: −(1/2π) ln r_d + (1/2π) ln r_a + (1/2π) ln r_b.
    let h = 1.0 / (2.0 * PI);
    let sx = -h * dx / rd2 + h * dx / ra2 + h * dx / rb2;
    let sy = -h * (r[1] - s[1]) / rd2 + h * (r[1] - ya) / ra2 + h * (r[1] - yb) / rb2;
    (val, [gx - sx, gy - sy])
}

/// Panels farther than this many panel lengths use the two-point quadrature.
const FAR: f64 = 6.0;
const FRAC_1_SQRT_3: f64 = 0.577_350_269_189_625_8;

/// Green's function (potential of a unit line charge, ε₀ = 1) at r of a source at s, with
/// the plane at y = 0 (one image) or both planes (closed form), and its gradient in r.
fn green(r: [f64; 2], s: [f64; 2], plane_b: Option<f64>) -> (f64, [f64; 2]) {
    let dx = r[0] - s[0];
    match plane_b {
        None => {
            let h = 1.0 / (2.0 * PI);
            let rd2 = dx * dx + (r[1] - s[1]).powi(2);
            let ra2 = dx * dx + (r[1] + s[1]).powi(2);
            (
                0.5 * h * (ra2.ln() - rd2.ln()),
                [-h * dx / rd2 + h * dx / ra2, -h * (r[1] - s[1]) / rd2 + h * (r[1] + s[1]) / ra2],
            )
        }
        Some(b) => {
            let k = PI / b;
            let u = k * dx;
            let vm = k * (r[1] - s[1]);
            let vp = k * (r[1] + s[1]);
            let sh = (0.5 * u).sinh();
            let d = 2.0 * (sh * sh + (0.5 * vm).sin().powi(2));
            let nn = 2.0 * (sh * sh + (0.5 * vp).sin().powi(2));
            let q = 1.0 / (4.0 * PI);
            let su = u.sinh();
            (q * (nn / d).ln(), [q * k * su * (1.0 / nn - 1.0 / d), q * k * (vp.sin() / nn - vm.sin() / d)])
        }
    }
}

/// Potential and its gradient at r of a unit (σ/ε₀ = 1) charge density on segment a→b, with
/// the ground plane(s) through images / the parallel-plate Green's function.
fn kernel(r: [f64; 2], a: [f64; 2], b: [f64; 2], own: bool, plane_b: Option<f64>) -> (f64, [f64; 2]) {
    let h = 1.0 / (2.0 * PI);
    let len2 = (b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2);
    let m = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    if !own && (r[0] - m[0]).powi(2) + (r[1] - m[1]).powi(2) > FAR * FAR * len2 {
        // Far from the panel: two-point Gauss quadrature of the full Green's function (its
        // images are farther still), accurate to about (len / distance)⁴.
        let len = len2.sqrt();
        let (mut pot, mut grad) = (0.0, [0.0, 0.0]);
        for x in [-FRAC_1_SQRT_3, FRAC_1_SQRT_3] {
            let u = 0.5 * (1.0 + x);
            let s = [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u];
            let (v, g) = green(r, s, plane_b);
            pot += 0.5 * len * v;
            grad[0] += 0.5 * len * g[0];
            grad[1] += 0.5 * len * g[1];
        }
        return (pot, grad);
    }
    let (i, g) = seg_log(r, a, b, own);
    let (ia, ga) = seg_log(r, [a[0], -a[1]], [b[0], -b[1]], false);
    let mut pot = -h * i + h * ia;
    let mut grad = [-h * g[0] + h * ga[0], -h * g[1] + h * ga[1]];
    if let Some(bb) = plane_b {
        let (ib, gb) = seg_log(r, [a[0], 2.0 * bb - a[1]], [b[0], 2.0 * bb - b[1]], false);
        pot += h * ib;
        grad[0] += h * gb[0];
        grad[1] += h * gb[1];
        let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        for (x, w) in GAUSS4 {
            let u = 0.5 * (1.0 + x);
            let s = [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u];
            let (v, gr) = pp_remainder(r, s, bb);
            let f = 0.5 * w * len;
            pot += f * v;
            grad[0] += f * gr[0];
            grad[1] += f * gr[1];
        }
    }
    (pot, grad)
}

// ---------------------------------------------------------------------------------------------
// Solve

/// LU decomposition with partial pivoting (row-major, in place); solves for each right-hand
/// side.
fn lu_solve(a: &mut [f64], n: usize, rhs: &mut [Vec<f64>]) -> Result<(), SolveError> {
    let mut piv: Vec<usize> = (0..n).collect();
    for k in 0..n {
        let mut p = k;
        let mut best = a[k * n + k].abs();
        for i in k + 1..n {
            let v = a[i * n + k].abs();
            if v > best {
                best = v;
                p = i;
            }
        }
        if best == 0.0 || !best.is_finite() {
            return Err(SolveError("singular boundary element system".into()));
        }
        if p != k {
            for j in 0..n {
                a.swap(k * n + j, p * n + j);
            }
            piv.swap(k, p);
        }
        let d = a[k * n + k];
        let (top, rest) = a.split_at_mut((k + 1) * n);
        let row_k = &top[k * n..k * n + n];
        for i in 0..n - k - 1 {
            let row = &mut rest[i * n..i * n + n];
            let f = row[k] / d;
            row[k] = f;
            if f != 0.0 {
                for j in k + 1..n {
                    row[j] -= f * row_k[j];
                }
            }
        }
    }
    for b in rhs.iter_mut() {
        let mut x: Vec<f64> = piv.iter().map(|&p| b[p]).collect();
        for i in 0..n {
            let mut s = x[i];
            for j in 0..i {
                s -= a[i * n + j] * x[j];
            }
            x[i] = s;
        }
        for i in (0..n).rev() {
            let mut s = x[i];
            for j in i + 1..n {
                s -= a[i * n + j] * x[j];
            }
            x[i] = s / a[i * n + i];
        }
        *b = x;
    }
    Ok(())
}

/// Largest number of unknowns accepted (keeps a solve well under a second).
const MAX_PANELS: usize = 4000;

fn panels(cs: &CrossSection, pr: &Prepared, acc: Accuracy, vacuum: bool) -> Vec<Panel> {
    let mut out = Vec::new();
    let mut layers = cs.layers.clone();
    let mut mask = cs.mask;
    if mask.is_some() && pr.envelope.is_none() {
        let m = mask.take().expect("checked");
        layers.push(Layer { thickness: m.thickness, er: m.er });
    }
    for s in &pr.segs {
        let us = pr.split(s, acc.grading);
        for w in us.windows(2) {
            let p = |u: f64| [s.a[0] + (s.b[0] - s.a[0]) * u, s.a[1] + (s.b[1] - s.a[1]) * u];
            let (a, b) = (p(w[0]), p(w[1]));
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            if len <= 0.0 {
                continue;
            }
            let t = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
            let n = [-t[1], t[0]];
            let m = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
            let d = 1e-3 * len.min(pr.fine);
            let probe = |sgn: f64| {
                if vacuum { 1.0 } else { pr.eps_at(&layers, mask, [m[0] + sgn * d * n[0], m[1] + sgn * d * n[1]]) }
            };
            let kind = match s.cond {
                Some(c) if s.sheet => Kind::Sheet { c, neg: probe(-1.0), pos: probe(1.0) },
                // Conductor polygons run counter-clockwise: the outside is to the right.
                Some(c) => Kind::Conductor { c, eps: probe(-1.0) },
                None => {
                    let (neg, pos) = (probe(-1.0), probe(1.0));
                    if (neg - pos).abs() <= 1e-12 * neg.max(pos) {
                        continue;
                    }
                    Kind::Interface { neg, pos }
                }
            };
            out.push(Panel { a, b, m, n, len, kind });
        }
    }
    out
}

/// Free charge on each half-structure conductor for each right-hand side, of one parity.
type Charges = (Vec<Vec<f64>>, Vec<Vec<f64>>);

/// Free charge on each half-structure conductor for each right-hand side, of one parity:
/// weighted by the panels' permittivities, and by 1 (the vacuum charges, valid as such when
/// there are no interface panels).
fn solve_parity(ps: &[Panel], parity: f64, plane_b: Option<f64>, rhs_v: &[Vec<f64>]) -> Result<Charges, SolveError> {
    let (rhs, sheet_rows) = densities(ps, parity, plane_b, rhs_v)?;
    let nc = rhs_v.first().map_or(0, |v| v.len());
    let (mut out, mut vac) = (Vec::new(), Vec::new());
    for q in &rhs {
        let (mut charge, mut charge0) = (vec![0.0; nc], vec![0.0; nc]);
        for (k, p) in ps.iter().enumerate() {
            if let Kind::Conductor { c, eps } = p.kind {
                charge[c] += eps * q[k] * p.len;
                charge0[c] += q[k] * p.len;
            }
        }
        for (i, field) in &sheet_rows {
            let p = &ps[*i];
            let Kind::Sheet { c, neg, pos } = p.kind else { continue };
            let e: f64 = field.iter().zip(q).map(|(f, x)| f * x).sum();
            let qi = q[*i];
            charge[c] += (neg * (0.5 * qi - e) + pos * (0.5 * qi + e)) * p.len;
            charge0[c] += qi * p.len;
        }
        out.push(charge);
        vac.push(charge0);
    }
    Ok((out, vac))
}

/// Panel charge densities for each right-hand side (conductor voltages of the half structure),
/// of one parity, and the normal-field rows of sheet panels.
type Densities = (Vec<Vec<f64>>, Vec<(usize, Vec<f64>)>);

fn densities(ps: &[Panel], parity: f64, plane_b: Option<f64>, rhs_v: &[Vec<f64>]) -> Result<Densities, SolveError> {
    let n = ps.len();
    let mut a = vec![0.0; n * n];
    // Normal field rows of sheet panels (to split their charge between the two faces).
    let mut sheet_rows: Vec<(usize, Vec<f64>)> = Vec::new();
    for (i, pi) in ps.iter().enumerate() {
        let need_field = !matches!(pi.kind, Kind::Conductor { .. });
        let mut field = if need_field { vec![0.0; n] } else { Vec::new() };
        let row = &mut a[i * n..i * n + n];
        for (j, pj) in ps.iter().enumerate() {
            let (p1, g1) = kernel(pi.m, pj.a, pj.b, i == j, plane_b);
            let (p2, g2) = kernel(pi.m, [-pj.b[0], pj.b[1]], [-pj.a[0], pj.a[1]], false, plane_b);
            let pot = p1 + parity * p2;
            match pi.kind {
                Kind::Conductor { .. } => row[j] = pot,
                Kind::Sheet { .. } | Kind::Interface { .. } => {
                    let e = -((g1[0] + parity * g2[0]) * pi.n[0] + (g1[1] + parity * g2[1]) * pi.n[1]);
                    field[j] = e;
                    match pi.kind {
                        Kind::Sheet { .. } => row[j] = pot,
                        Kind::Interface { neg, pos } => {
                            row[j] = (pos - neg) * e + if i == j { 0.5 * (pos + neg) } else { 0.0 };
                        }
                        Kind::Conductor { .. } => unreachable!(),
                    }
                }
            }
        }
        if matches!(pi.kind, Kind::Sheet { .. }) {
            sheet_rows.push((i, field));
        }
    }
    let mut rhs: Vec<Vec<f64>> = rhs_v
        .iter()
        .map(|v| {
            ps.iter()
                .map(|p| match p.kind {
                    Kind::Conductor { c, .. } | Kind::Sheet { c, .. } => v[c],
                    Kind::Interface { .. } => 0.0,
                })
                .collect()
        })
        .collect();
    lu_solve(&mut a, n, &mut rhs)?;
    Ok((rhs, sheet_rows))
}

/// Capacitance matrices of a cross-section.
pub fn solve(cs: &CrossSection, acc: Accuracy) -> Result<Capacitance, SolveError> {
    let pr = prepare(cs, acc)?;
    let nsig = cs.conductors.iter().filter(|c| matches!(c.role, Role::Signal(_))).count();
    let sig = |i: usize| match cs.conductors[i].role {
        Role::Signal(n) => Some(n),
        Role::Ground => None,
    };
    // Excitations on the half structure: even parity: each self-symmetric signal alone, each
    // mirror pair together; odd parity: each mirror pair in antiphase.
    let nh = pr.half.len();
    let mut even: Vec<(usize, Vec<f64>)> = Vec::new();
    let mut odd: Vec<(usize, Vec<f64>)> = Vec::new();
    for (h, &i) in pr.half.iter().enumerate() {
        if sig(i).is_none() {
            continue;
        }
        let mut v = vec![0.0; nh];
        v[h] = 1.0;
        if matches!(pr.sym[i], Sym::Right(_)) {
            odd.push((h, v.clone()));
        }
        even.push((h, v));
    }
    let mut c_out = Vec::new();
    let mut panels_used = 0;
    let ev: Vec<Vec<f64>> = even.iter().map(|(_, v)| v.clone()).collect();
    let ov: Vec<Vec<f64>> = odd.iter().map(|(_, v)| v.clone()).collect();
    // Charges with the dielectrics, then in vacuum: the dielectric solve gives both when it has
    // no interface panels (the conductor equations do not depend on the permittivities).
    let mut reuse = None;
    for vacuum in [false, true] {
        let (qe, qo) = match reuse.take() {
            Some(r) => r,
            None => {
                let ps = panels(cs, &pr, acc, vacuum);
                if ps.len() > MAX_PANELS {
                    return Err(SolveError(format!(
                        "the cross-section needs {} unknowns (limit {MAX_PANELS})",
                        ps.len()
                    )));
                }
                let (qe, qe0) = solve_parity(&ps, 1.0, pr.plane_b, &ev)?;
                let (qo, qo0) =
                    if ov.is_empty() { (Vec::new(), Vec::new()) } else { solve_parity(&ps, -1.0, pr.plane_b, &ov)? };
                if !vacuum {
                    panels_used = ps.len();
                    if !ps.iter().any(|p| matches!(p.kind, Kind::Interface { .. })) {
                        reuse = Some((qe0, qo0));
                    }
                }
                (qe, qo)
            }
        };
        // Full Maxwell matrix.
        let mut c = vec![vec![0.0; nsig]; nsig];
        // Charge on full conductor `i` (full index) in even solve k / odd solve k.
        let full_q = |q: &Vec<f64>, i: usize, parity: f64| -> f64 {
            match pr.sym[i] {
                Sym::Centered => 2.0 * q[pr.half.iter().position(|&x| x == i).expect("half")],
                Sym::Right(_) => q[pr.half.iter().position(|&x| x == i).expect("half")],
                Sym::Left(p) => parity * q[pr.half.iter().position(|&x| x == p).expect("half")],
            }
        };
        for (k, (h, _)) in even.iter().enumerate() {
            let src = pr.half[*h];
            let sn = sig(src).expect("signal");
            match pr.sym[src] {
                Sym::Centered => {
                    for i in 0..cs.conductors.len() {
                        if let Some(n) = sig(i) {
                            c[n][sn] = full_q(&qe[k], i, 1.0);
                        }
                    }
                }
                Sym::Right(partner) => {
                    let pn = sig(partner).expect("signal");
                    let ko = odd.iter().position(|(x, _)| x == h).expect("odd");
                    for i in 0..cs.conductors.len() {
                        let Some(n) = sig(i) else { continue };
                        let qe_i = full_q(&qe[k], i, 1.0);
                        if matches!(pr.sym[i], Sym::Centered) {
                            c[n][sn] = qe_i / 2.0;
                            c[n][pn] = qe_i / 2.0;
                        } else {
                            let qo_i = full_q(&qo[ko], i, -1.0);
                            c[n][sn] = (qe_i + qo_i) / 2.0;
                            c[n][pn] = (qe_i - qo_i) / 2.0;
                        }
                    }
                }
                Sym::Left(_) => unreachable!(),
            }
        }
        c_out.push(c);
    }
    let c0 = c_out.pop().expect("vacuum");
    let c = c_out.pop().expect("dielectric");
    Ok(Capacitance { c, c0, panels: panels_used })
}

// ---------------------------------------------------------------------------------------------
// Losses
//
// Dielectric loss: G = ω Σᵣ tan δᵣ εᵣ ∂C/∂εᵣ over the dielectric regions r (the first-order
// perturbation of C with complex permittivities εᵣ(1 − j tan δᵣ); εᵣ ∂C/∂εᵣ is the partial
// capacitance of region r, i.e. its share of the electric energy). The derivatives are one-sided
// second-order differences of the boundary element solution, one pair of extra solves per
// distinct loss tangent.
//
// Conductor loss: the perturbation (power-loss) method, R = Rs ∮ |Jₛ|² dl / I², with the
// surface current of the quasi-TEM mode. Its magnetic field is that of the vacuum structure,
// so the surface current is proportional to the vacuum surface charge (Jₛ = c ρₛ): the charge
// densities of the vacuum boundary element solution on the traces and coplanar grounds, and
// the charge they induce on the ground planes (computed in closed form from the plane Green's
// functions). Valid when the skin depth is small against the copper thickness; edges of
// zero-thickness strips make the integral diverge, so traces must have a thickness.

/// Loss tangents of a [`Structure`]'s dielectrics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LossTangents {
    /// The dielectric between the plane and the (lower) trace.
    pub below: f64,
    /// The dielectric around and above the (upper) trace (embedded microstrip, stripline).
    pub above: f64,
    /// Between the traces of a broadside pair.
    pub broadside: f64,
    /// Solder mask.
    pub mask: f64,
}

/// Per-unit-length matrices of a line or a pair, in SI units, for the loss calculation
/// (`[signal][signal]`; 1×1 for a single line). Lengths of the structure in millimeters.
#[derive(Clone, Debug, PartialEq)]
pub struct LineMatrices {
    /// Capacitance, F/m.
    pub c: Vec<Vec<f64>>,
    /// External inductance (perfect conductors), H/m.
    pub l: Vec<Vec<f64>>,
    /// Conductance divided by the angular frequency, F/m: Σ tan δ · ε ∂C/∂ε.
    pub g_over_omega: Vec<Vec<f64>>,
    /// Skin-effect resistance divided by the surface resistance Rs, 1/m: ∮ ĵᵢ ĵⱼ dl over all
    /// conductor surfaces, the planes included, ĵ the surface current per unit current.
    pub r_over_rs: Vec<Vec<f64>>,
}

fn invert(m: &[Vec<f64>]) -> Result<Vec<Vec<f64>>, SolveError> {
    match m.len() {
        1 => Ok(vec![vec![1.0 / m[0][0]]]),
        2 => {
            let i = inv2([[m[0][0], m[0][1]], [m[1][0], m[1][1]]]);
            Ok(vec![i[0].to_vec(), i[1].to_vec()])
        }
        n => Err(SolveError(format!("losses are computed for one or two signal conductors, not {n}"))),
    }
}

fn scale(m: &[Vec<f64>], f: f64) -> Vec<Vec<f64>> {
    m.iter().map(|r| r.iter().map(|x| x * f).collect()).collect()
}

impl Structure {
    /// Loss tangents in the order of [`Structure::cross_section`]'s layers.
    fn layer_loss_tangents(&self, t: &LossTangents) -> Vec<f64> {
        let mut v = vec![t.below];
        if self.broadside.is_some() {
            v.push(t.broadside);
        }
        if self.model != Model::Microstrip {
            v.push(t.above);
        }
        v
    }

    /// Per-unit-length matrices (C, L, G/ω, R/Rs) of the single trace (`pair` false; with a
    /// broadside pair, the lower trace alone) or of the pair (edge-coupled with `gap`, or
    /// broadside).
    pub fn line_matrices(
        &self,
        w: f64,
        gap: Option<f64>,
        pair: bool,
        tans: &LossTangents,
        acc: Accuracy,
    ) -> Result<LineMatrices, SolveError> {
        if pair && !self.is_pair(gap) {
            return Err(SolveError("a pair needs a gap or a broadside separation".into()));
        }
        let mut cs = self.cross_section(w, if pair { gap } else { None })?;
        if !pair {
            cs.conductors.retain(|c| c.role != Role::Signal(1));
        }
        let tl = self.layer_loss_tangents(tans);
        let cap = solve(&cs, acc)?;
        let g = dielectric_loss(&cs, &cap, &tl, tans.mask, acc)?;
        let r = conductor_loss(&cs, acc)?;
        let mu0 = 1.0 / (EPS0 * C0 * C0);
        Ok(LineMatrices {
            c: scale(&cap.c, EPS0),
            l: scale(&invert(&cap.c0)?, mu0),
            g_over_omega: scale(&g, EPS0),
            // 1/mm to 1/m.
            r_over_rs: scale(&r, 1000.0),
        })
    }
}

/// Corner refinement of the conductor loss solve relative to [`Accuracy::fine`].
const LOSS_FINE: f64 = 1e-3;

/// Relative step of the permittivity derivatives.
const EPS_STEP: f64 = 1e-3;

/// G/ω in units of ε₀ (`[signal][signal]`): Σ over distinct loss tangents of tan δ times the
/// derivative of C when the permittivities of the regions with that loss tangent are scaled
/// together (Euler: Σᵣ εᵣ ∂C/∂εᵣ over those regions). `base` is the solution at the nominal
/// permittivities; `tans` follows `cs.layers`.
pub fn dielectric_loss(
    cs: &CrossSection,
    base: &Capacitance,
    tans: &[f64],
    tan_mask: f64,
    acc: Accuracy,
) -> Result<Vec<Vec<f64>>, SolveError> {
    let mut groups: Vec<f64> =
        tans.iter().copied().chain(cs.mask.map(|_| tan_mask)).filter(|t| *t > 0.0 && t.is_finite()).collect();
    groups.sort_by(f64::total_cmp);
    groups.dedup();
    let n = base.c.len();
    let mut g = vec![vec![0.0; n]; n];
    for tan in groups {
        let scaled = |f: f64| {
            let mut c = cs.clone();
            for (l, t) in c.layers.iter_mut().zip(tans) {
                if *t == tan {
                    l.er *= 1.0 + f;
                }
            }
            if let Some(m) = &mut c.mask
                && tan_mask == tan
            {
                m.er *= 1.0 + f;
            }
            c
        };
        let c1 = solve(&scaled(EPS_STEP), acc)?.c;
        let c2 = solve(&scaled(2.0 * EPS_STEP), acc)?.c;
        for i in 0..n {
            for j in 0..n {
                let d = (-3.0 * base.c[i][j] + 4.0 * c1[i][j] - c2[i][j]) / (2.0 * EPS_STEP);
                g[i][j] += tan * d;
            }
        }
    }
    Ok(g)
}

/// Charge density induced on the plane y = 0 at (x, 0) by a unit charge density (σ/ε₀ = 1)
/// on segment a→b: the plane alone (its image), or with `plane_b` the lower plate of the
/// parallel-plate pair. Closed-form integrals of −y′/(π((x′−x)² + y′²)) and of
/// −sin θ / (2b (cosh u − cos θ)), u = π(x′−x)/b, θ = πy′/b, along the segment.
fn plane_density(x: f64, a: [f64; 2], b: [f64; 2], plane_b: Option<f64>) -> f64 {
    let len = (b[0] - a[0]).hypot(b[1] - a[1]);
    let e = C64::new((b[0] - a[0]) / len, (b[1] - a[1]) / len);
    match plane_b {
        None => {
            // −(1/π) Im ∫ ds / conj(ζ), ζ = z′ − x (conj ζ stays in the lower half plane).
            let za = C64::new(a[0] - x, -a[1]);
            let zb = C64::new(b[0] - x, -b[1]);
            -(e * (zb.ln() - za.ln())).im / PI
        }
        Some(bb) => {
            // (1/π) Im[e^(−iφ) (Ln sinh(w_b/2) − Ln sinh(w_a/2))], w = π ζ / b (sinh(w/2)
            // stays in the upper half plane).
            let k = 0.5 * PI / bb;
            let wa = C64::new(k * (a[0] - x), k * a[1]);
            let wb = C64::new(k * (b[0] - x), k * b[1]);
            (e.conj() * (wb.ln_sinh() - wa.ln_sinh())).im / PI
        }
    }
}

/// Integrates over the ground plane(s), x ≥ 0: calls `f(weight, densities)` at quadrature
/// points with the plane charge density of each excitation `qs[k]` = (parity, panel densities),
/// for each plane (the planes' contributions are separate calls).
fn plane_quadrature(
    cs: &CrossSection,
    pr: &Prepared,
    ps: &[Panel],
    qs: &[(f64, Vec<f64>)],
    mut f: impl FnMut(f64, &[f64]),
) {
    let xmax = pr.xmax;
    let bottom = cs.conductors.iter().map(|c| c.y).fold(f64::INFINITY, f64::min);
    let mut scale = bottom;
    if let Some(b) = pr.plane_b {
        scale = scale.min(b - cs.conductors.iter().map(|c| c.y + c.t).fold(0.0, f64::max));
    }
    let foot: Vec<(f64, f64)> = cs.conductors.iter().map(|c| (finite_or(c.x0, xmax), finite_or(c.x1, xmax))).collect();
    let dist = |x: f64| {
        foot.iter()
            .map(|&(l, r)| if x >= l && x <= r { 0.0 } else { (x - l).abs().min((x - r).abs()) })
            .fold(f64::INFINITY, f64::min)
    };
    let mut bps: Vec<f64> = vec![0.0, xmax];
    bps.extend(foot.iter().flat_map(|&(l, r)| [l, r]).filter(|&x| x > 0.0 && x < xmax));
    bps.sort_by(f64::total_cmp);
    bps.dedup_by(|a, b| (*a - *b).abs() <= 1e-12 * xmax);
    let planes: Vec<Option<f64>> = match pr.plane_b {
        None => vec![None],
        Some(_) => vec![Some(0.0), Some(1.0)],
    };
    let mut sig = vec![0.0; qs.len()];
    let (mut kd, mut km) = (vec![0.0; ps.len()], vec![0.0; ps.len()]);
    for w in bps.windows(2) {
        let (a, b) = (w[0], w[1]);
        let mut x = a;
        while x < b {
            let s = 0.2 * scale.max(dist(x));
            let mut nx = (x + s).min(b);
            if b - nx < 0.2 * s {
                nx = b;
            }
            for (gx, gw) in GAUSS4 {
                let xp = x + (nx - x) * 0.5 * (1.0 + gx);
                let wt = (nx - x) * 0.5 * gw;
                for top in &planes {
                    // The top plate is the bottom one of the mirrored structure y → b − y.
                    let flip = |p: [f64; 2]| match (top, pr.plane_b) {
                        (Some(t), Some(bb)) if *t > 0.5 => [p[0], bb - p[1]],
                        _ => p,
                    };
                    for (j, p) in ps.iter().enumerate() {
                        kd[j] = plane_density(xp, flip(p.a), flip(p.b), pr.plane_b);
                        km[j] = plane_density(xp, flip([-p.b[0], p.b[1]]), flip([-p.a[0], p.a[1]]), pr.plane_b);
                    }
                    for (k, (parity, q)) in qs.iter().enumerate() {
                        sig[k] = q.iter().enumerate().map(|(j, qj)| qj * (kd[j] + parity * km[j])).sum();
                    }
                    f(wt, &sig);
                }
            }
            x = nx;
        }
    }
}

/// The vacuum solution's excitations for the loss calculation: (parity, panel densities) per
/// excitation, and the full-structure signal charges `[signal][excitation]`.
type Excitations = (Vec<(f64, Vec<f64>)>, Vec<Vec<f64>>);

fn vacuum_excitations(cs: &CrossSection, pr: &Prepared, ps: &[Panel]) -> Result<Excitations, SolveError> {
    let nsig = cs.conductors.iter().filter(|c| matches!(c.role, Role::Signal(_))).count();
    let sig = |i: usize| match cs.conductors[i].role {
        Role::Signal(n) => Some(n),
        Role::Ground => None,
    };
    let nh = pr.half.len();
    let (mut ev, mut ov) = (Vec::new(), Vec::new());
    for (h, &i) in pr.half.iter().enumerate() {
        if sig(i).is_none() {
            continue;
        }
        let mut v = vec![0.0; nh];
        v[h] = 1.0;
        if matches!(pr.sym[i], Sym::Right(_)) {
            ov.push(v.clone());
        }
        ev.push(v);
    }
    let mut qs: Vec<(f64, Vec<f64>)> = densities(ps, 1.0, pr.plane_b, &ev)?.0.into_iter().map(|q| (1.0, q)).collect();
    if !ov.is_empty() {
        qs.extend(densities(ps, -1.0, pr.plane_b, &ov)?.0.into_iter().map(|q| (-1.0, q)));
    }
    if qs.len() != nsig {
        return Err(SolveError("unsupported conductor arrangement for the loss calculation".into()));
    }
    let pos = |i: usize| pr.half.iter().position(|&x| x == i).expect("half");
    let mut charges = vec![vec![0.0; qs.len()]; nsig];
    for (k, (parity, q)) in qs.iter().enumerate() {
        let mut half_q = vec![0.0; nh];
        for (j, p) in ps.iter().enumerate() {
            if let Kind::Conductor { c, .. } = p.kind {
                half_q[c] += q[j] * p.len;
            }
        }
        for i in 0..cs.conductors.len() {
            let Some(n) = sig(i) else { continue };
            charges[n][k] = match pr.sym[i] {
                Sym::Centered => 2.0 * half_q[pos(i)],
                Sym::Right(_) => half_q[pos(i)],
                Sym::Left(p) => parity * half_q[pos(p)],
            };
        }
    }
    Ok((qs, charges))
}

/// Skin-effect resistance per surface resistance, `[signal][signal]`, in 1/(length unit) (see
/// [`LineMatrices::r_over_rs`]): ∮ σ̂ᵢ σ̂ⱼ dl over the conductors and the ground planes, σ̂ᵢ
/// the vacuum surface charge density with a unit charge on signal i and none on the other
/// signals, which is the surface current density per unit current on signal i.
pub fn conductor_loss(cs: &CrossSection, acc: Accuracy) -> Result<Vec<Vec<f64>>, SolveError> {
    if cs.conductors.iter().any(|c| c.t <= 0.0) {
        return Err(SolveError(
            "conductor loss needs a copper thickness (the current at the edges of a zero-thickness strip makes the loss \
             diverge)"
                .into(),
        ));
    }
    // The current crowds at the corners (σ ~ r^(−1/3) at a right-angled edge), and a
    // piecewise-constant density misses part of ∫σ² in the smallest panels (in proportion to
    // their size^(1/3)): grade much further into the corners than the capacitance needs.
    let acc = Accuracy { grading: acc.grading, fine: acc.fine * LOSS_FINE };
    let pr = prepare(cs, acc)?;
    let ps = panels(cs, &pr, acc, true);
    if ps.len() > MAX_PANELS {
        return Err(SolveError(format!("the cross-section needs {} unknowns (limit {MAX_PANELS})", ps.len())));
    }
    let (qs, charges) = vacuum_excitations(cs, &pr, &ps)?;
    let m = qs.len();
    // Gram matrix of the excitations' surface charges over the whole structure (both halves:
    // twice the right half for equal parities, zero for opposite ones).
    let mut s = vec![vec![0.0; m]; m];
    for k in 0..m {
        for l in 0..m {
            if qs[k].0 != qs[l].0 {
                continue;
            }
            s[k][l] = 2.0 * ps.iter().enumerate().map(|(j, p)| qs[k].1[j] * qs[l].1[j] * p.len).sum::<f64>();
        }
    }
    plane_quadrature(cs, &pr, &ps, &qs, |w, sig| {
        for k in 0..m {
            for l in 0..m {
                if qs[k].0 == qs[l].0 {
                    s[k][l] += 2.0 * w * sig[k] * sig[l];
                }
            }
        }
    });
    // Unit-charge distributions: σ̂ᵢ = Σₖ Aₖᵢ σₖ with A = Q⁻¹.
    let a = invert(&charges)?;
    let n = charges.len();
    let mut r = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..n {
            for k in 0..m {
                for l in 0..m {
                    r[i][j] += a[k][i] * s[k][l] * a[l][j];
                }
            }
        }
    }
    Ok(r)
}

/// Total charge induced on the ground plane(s) by each excitation of the vacuum solution, and
/// the signal charges (tests: Gauss's law).
#[cfg(test)]
fn plane_charge(cs: &CrossSection, acc: Accuracy) -> (Vec<f64>, Vec<Vec<f64>>) {
    let pr = prepare(cs, acc).unwrap();
    let ps = panels(cs, &pr, acc, true);
    let (qs, charges) = vacuum_excitations(cs, &pr, &ps).unwrap();
    let mut tot = vec![0.0; qs.len()];
    plane_quadrature(cs, &pr, &ps, &qs, |w, sig| {
        for (t, s) in tot.iter_mut().zip(sig) {
            *t += 2.0 * w * s;
        }
    });
    (tot, charges)
}

// ---------------------------------------------------------------------------------------------
// Reference formulas used by the tests and the docs

/// Complete elliptic integral of the first kind K(k), by the arithmetic-geometric mean.
pub fn ellip_k(k: f64) -> f64 {
    let (mut a, mut b) = (1.0f64, (1.0 - k * k).sqrt());
    for _ in 0..64 {
        let (an, bn) = ((a + b) / 2.0, (a * b).sqrt());
        if (an - bn).abs() <= 1e-16 * an {
            a = an;
            break;
        }
        a = an;
        b = bn;
    }
    PI / (2.0 * a)
}

/// K(k)/K(k') with k' = √(1 − k²).
pub fn ellip_ratio(k: f64) -> f64 {
    ellip_k(k) / ellip_k((1.0 - k * k).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::electrical::impedance::{microstrip, stripline};

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b.abs()
    }

    fn strip(model: Model, h: f64, h2: f64, t: f64, er: f64) -> Structure {
        Structure { model, h, er, h2, er2: er, t, etch: 0.0, broadside: None, coplanar_gap: None, mask: None }
    }

    /// Zero-thickness centered stripline, exact (Cohn 1954, conformal mapping):
    /// Z₀ = (η₀ / 4√εr) K(k)/K(k'), k = sech(πW/2b).
    fn cohn_stripline(w: f64, b: f64, er: f64) -> f64 {
        let k = 1.0 / (PI * w / (2.0 * b)).cosh();
        ETA0 / (4.0 * er.sqrt()) * ellip_ratio(k)
    }

    #[test]
    fn stripline_zero_thickness_exact() {
        for (w, b) in [(0.2, 0.5), (1.0, 1.0), (0.1, 1.0), (3.0, 1.0)] {
            let s = strip(Model::Stripline, b / 2.0, b / 2.0, 0.0, 4.0);
            let l = s.line(w, Accuracy::default()).unwrap();
            let z = cohn_stripline(w, b, 4.0);
            assert!(close(l.z0, z, 0.001), "w={w} b={b}: {} vs {z}", l.z0);
            assert!(close(l.er_eff, 4.0, 1e-9), "{l:?}");
        }
    }

    #[test]
    fn edge_coupled_stripline_exact() {
        // Cohn 1955, zero thickness: Z₀e,o = (η₀/4√εr) K(k')/K(k), k_e = tanh(πW/2b) tanh(π(W+S)/2b),
        // k_o = tanh(πW/2b) coth(π(W+S)/2b) — written here as K(k')/K(k) = 1/ellip_ratio(k).
        let (w, s, b, er) = (0.15, 0.2, 0.5, 3.0);
        let a = (PI * w / (2.0 * b)).tanh();
        let c = (PI * (w + s) / (2.0 * b)).tanh();
        let ze = ETA0 / (4.0 * f64::sqrt(er)) / ellip_ratio(a * c);
        let zo = ETA0 / (4.0 * f64::sqrt(er)) / ellip_ratio(a / c);
        let st = strip(Model::Stripline, b / 2.0, b / 2.0, 0.0, er);
        let p = st.pair(w, Some(s), Accuracy::default()).unwrap();
        assert!(close(p.z_even, ze, 0.001), "{p:?} vs {ze}");
        assert!(close(p.z_odd, zo, 0.001), "{p:?} vs {zo}");
        assert!(close(p.zdiff, 2.0 * zo, 0.001));
        assert!(close(p.er_eff_odd, er, 1e-9) && close(p.er_eff_even, er, 1e-9));
    }

    #[test]
    fn microstrip_matches_hammerstad_jensen() {
        for (u, er) in [(0.25, 4.4), (0.5, 4.4), (1.0, 10.0), (2.0, 4.4), (4.0, 2.2)] {
            let s = strip(Model::Microstrip, 1.0, 0.0, 0.0, er);
            let l = s.line(u, Accuracy::default()).unwrap();
            let hj = microstrip(u, 1.0, 0.0, er);
            assert!(close(l.z0, hj.z0, 0.003), "u={u} er={er}: {l:?} vs {hj:?}");
            assert!(close(l.er_eff, hj.er_eff, 0.003), "u={u} er={er}: {l:?} vs {hj:?}");
        }
    }

    #[test]
    fn thick_lines_match_closed_forms() {
        let s = strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.5);
        for w in [0.15, 0.35, 0.8] {
            let l = s.line(w, Accuracy::default()).unwrap();
            let hj = microstrip(w, 0.2, 0.035, 4.5);
            assert!(close(l.z0, hj.z0, 0.015), "w={w}: {l:?} vs {hj:?}");
        }
        let s = strip(Model::Stripline, 0.2, 0.2, 0.0175, 4.2);
        for w in [0.1, 0.2, 0.4] {
            let l = s.line(w, Accuracy::default()).unwrap();
            let wh = stripline(w, 0.2, 0.2, 0.0175, 4.2);
            assert!(close(l.z0, wh.z0, 0.015), "w={w}: {l:?} vs {wh:?}");
        }
    }

    #[test]
    fn deterministic() {
        let s = Structure {
            mask: Some(Mask { thickness: 0.02, over_copper: 0.012, er: 3.8 }),
            ..strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.4)
        };
        let a = s.pair(0.2, Some(0.15), Accuracy::default()).unwrap();
        let b = s.pair(0.2, Some(0.15), Accuracy::default()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn cpwg_matches_conformal_mapping() {
        // Grounded coplanar waveguide, zero thickness, wide grounds: the conformal-mapping
        // (partial capacitance) result, e.g. R. N. Simons, Coplanar Waveguide Circuits,
        // Components, and Systems (2001), conductor-backed CPW: εeff = (K(k)/K(k') + εr K(k₃)/K(k₃')) /
        // (K(k)/K(k') + K(k₃)/K(k₃')), Z₀ = η₀ / (2 √εeff (K(k)/K(k') + K(k₃)/K(k₃'))),
        // k = W/(W+2S), k₃ = tanh(πW/4h) / tanh(π(W+2S)/4h).
        for (w, s, h, er) in [(0.3, 0.15, 0.2, 4.4), (1.0, 0.2, 0.5, 4.4), (0.5, 0.3, 1.0, 9.8)] {
            let st = Structure { coplanar_gap: Some(s), ..strip(Model::Microstrip, h, 0.0, 0.0, er) };
            let l = st.line(w, Accuracy::default()).unwrap();
            let k = w / (w + 2.0 * s);
            let k3 = (PI * w / (4.0 * h)).tanh() / (PI * (w + 2.0 * s) / (4.0 * h)).tanh();
            let (a, b) = (ellip_ratio(k), ellip_ratio(k3));
            let ee = (a + er * b) / (a + b);
            let z = ETA0 / (2.0 * ee.sqrt() * (a + b));
            assert!(close(l.z0, z, 0.01), "w={w} s={s} h={h}: {l:?} vs {z}");
            assert!(close(l.er_eff, ee, 0.01), "w={w} s={s} h={h}: {l:?} vs {ee}");
        }
    }

    #[test]
    fn solder_mask() {
        let base = strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.4);
        let acc = Accuracy::default();
        let bare = base.line(0.35, acc).unwrap();
        let mask = |thickness, er| Structure { mask: Some(Mask { thickness, over_copper: 0.015, er }), ..base };
        let coated = mask(0.025, 3.8).line(0.35, acc).unwrap();
        // A typical LPI coat lowers a 50 Ω microstrip by 1 to 3 Ω and raises εeff.
        assert!(coated.z0 < bare.z0 - 1.0 && coated.z0 > bare.z0 - 3.0, "{bare:?} {coated:?}");
        assert!(coated.er_eff > bare.er_eff);
        // A mask of εr 1 is no mask (only the discretization differs).
        let air = mask(0.025, 1.0).line(0.35, acc).unwrap();
        assert!(close(air.z0, bare.z0, 5e-4), "{air:?} {bare:?}");
        // Thicker is lower.
        let thick = mask(0.05, 3.8).line(0.35, acc).unwrap();
        assert!(thick.z0 < coated.z0);
        // Inductance does not depend on dielectrics.
        assert!(close(coated.l, bare.l, 1e-3));
        // Over an embedded line it is a flat layer.
        let em = strip(Model::EmbeddedMicrostrip, 0.2, 0.05, 0.035, 4.4);
        let em_mask = Structure { mask: Some(Mask { thickness: 0.02, over_copper: 0.02, er: 3.8 }), ..em };
        assert!(em_mask.line(0.2, acc).unwrap().z0 < em.line(0.2, acc).unwrap().z0);
        // Mask with a top plane is refused.
        let st = Structure {
            mask: Some(Mask { thickness: 0.02, over_copper: 0.02, er: 3.8 }),
            ..strip(Model::Stripline, 0.2, 0.2, 0.0, 4.0)
        };
        assert!(st.line(0.2, acc).is_err());
    }

    #[test]
    fn trapezoidal_etch() {
        let acc = Accuracy::default();
        let base = strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.4);
        let etched = Structure { etch: 0.01, ..base }.line(0.35, acc).unwrap().z0;
        let bottom = base.line(0.35, acc).unwrap().z0;
        let top = base.line(0.33, acc).unwrap().z0;
        assert!(bottom < etched && etched < top, "{bottom} {etched} {top}");
        // Too much etch for the width.
        assert!(Structure { etch: 0.2, ..base }.line(0.35, acc).is_err());
    }

    #[test]
    fn embedded_and_split_dielectrics() {
        let acc = Accuracy::default();
        // A thick cover: εeff tends to εr.
        let l = strip(Model::EmbeddedMicrostrip, 0.2, 2.0, 0.035, 4.4).line(0.2, acc).unwrap();
        assert!(close(l.er_eff, 4.4, 0.005), "{l:?}");
        // A thin cover sits between the surface microstrip and the fully covered line.
        let thin = strip(Model::EmbeddedMicrostrip, 0.2, 0.05, 0.035, 4.4).line(0.2, acc).unwrap();
        let surf = strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.4).line(0.2, acc).unwrap();
        assert!(surf.er_eff < thin.er_eff && thin.er_eff < l.er_eff);
        // A stripline with two dielectrics: εeff between them, exact for equal ones.
        let mut s = strip(Model::Stripline, 0.2, 0.2, 0.0175, 3.0);
        s.er2 = 5.0;
        let l = s.line(0.15, acc).unwrap();
        assert!(l.er_eff > 3.2 && l.er_eff < 4.8, "{l:?}");
        // Asymmetric stripline against the parallel-combination approximation (a few %).
        let a = strip(Model::Stripline, 0.15, 0.3, 0.0175, 4.2).line(0.15, acc).unwrap();
        let cf = stripline(0.15, 0.15, 0.3, 0.0175, 4.2);
        assert!(close(a.z0, cf.z0, 0.03), "{a:?} {cf:?}");
    }

    #[test]
    fn coupled_pairs() {
        let acc = Accuracy::default();
        let ms = strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.4);
        let z0 = ms.line(0.2, acc).unwrap().z0;
        // Far apart: Zdiff = 2 Z₀, Zcomm = Z₀ / 2.
        let far = ms.pair(0.2, Some(6.0), acc).unwrap();
        assert!(close(far.zdiff, 2.0 * z0, 0.005) && close(far.zcomm, z0 / 2.0, 0.005), "{far:?} {z0}");
        // Coupling lowers the odd mode, raises the even mode; the odd mode is faster.
        let near = ms.pair(0.2, Some(0.15), acc).unwrap();
        assert!(near.z_odd < z0 && near.z_even > z0 && near.er_eff_odd < near.er_eff_even, "{near:?}");
        // Edge-coupled microstrip near the IPC-2141 estimate (±10 %).
        let ipc =
            crate::electrical::impedance::Geometry { model: Model::Microstrip, h: 0.2, h2: 0.0, t: 0.035, er: 4.4 }
                .differential(0.2, 0.15);
        assert!(close(near.zdiff, ipc, 0.1), "{near:?} {ipc}");
        // Broadside-coupled stripline, homogeneous and symmetric: both modes at εr, equal
        // self capacitances, and at a small separation the odd mode tends to the parallel-plate
        // value η₀ (s/2) / (√εr W) (fringing lowers it).
        let bs = Structure { broadside: Some((0.01, 4.0)), ..strip(Model::Stripline, 0.5, 0.5, 0.0, 4.0) };
        let cap = solve(&bs.cross_section(0.3, None).unwrap(), acc).unwrap();
        assert!(close(cap.c[0][0], cap.c[1][1], 1e-3) && close(cap.c[0][1], cap.c[1][0], 1e-9), "{cap:?}");
        let p = cap.pair();
        assert!(close(p.er_eff_odd, 4.0, 1e-9) && close(p.er_eff_even, 4.0, 1e-9));
        let plate = ETA0 * 0.005 / (2.0 * 0.3);
        assert!(p.z_odd < plate && p.z_odd > 0.85 * plate, "{p:?} {plate}");
        // Broadside on the surface (lower trace embedded): the pair is asymmetric, still
        // reported through the mixed-mode impedances.
        let bm = Structure { broadside: Some((0.15, 3.5)), ..ms };
        let p = bm.pair(0.2, None, acc).unwrap();
        assert!(p.zdiff > 0.0 && p.zcomm > 0.0 && p.zdiff < 2.0 * z0, "{p:?}");
        assert!(bm.pair(0.2, Some(0.1), acc).is_err());
    }

    #[test]
    fn solve_width_roundtrip() {
        let acc = Accuracy::default();
        let ms = Structure { coplanar_gap: Some(0.15), ..strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.4) };
        let w = ms.solve_width(50.0, None, 0.3, acc).unwrap().unwrap();
        assert!(close(ms.line(w, acc).unwrap().z0, 50.0, 1e-3), "{w}");
        let w = ms.solve_width(90.0, Some(0.15), 0.2, acc).unwrap().unwrap();
        assert!(close(ms.pair(w, Some(0.15), acc).unwrap().zdiff, 90.0, 1e-3), "{w}");
        assert_eq!(ms.solve_width(2000.0, None, 0.3, acc).unwrap(), None);
    }

    #[test]
    fn plane_charge_balances() {
        // Gauss: the planes carry minus the charge of the conductors (vacuum solution, even
        // excitations), for one plane and for two.
        let acc = Accuracy::default();
        for st in [
            strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.4),
            strip(Model::Stripline, 0.15, 0.25, 0.0175, 4.2),
            Structure { coplanar_gap: Some(0.15), ..strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.4) },
        ] {
            let mut cs = st.cross_section(0.3, None).unwrap();
            let grounds = cs.conductors.iter().any(|c| c.role == Role::Ground);
            let (plane, charges) = plane_charge(&cs, acc);
            if grounds {
                // Part of the return charge sits on the coplanar grounds.
                assert!(plane[0] < 0.0 && -plane[0] < charges[0][0], "{plane:?} {charges:?}");
                cs.conductors.retain(|c| c.role != Role::Ground);
            }
            let (plane, charges) = plane_charge(&cs, acc);
            // A single plane's density decays as Q ȳ / (π x²): add the two tails beyond the
            // truncation (ȳ: the trace's mid-height).
            let tail = if cs.top_plane {
                0.0
            } else {
                let xmax = prepare(&cs, acc).unwrap().xmax;
                2.0 * charges[0][0] * (st.h + st.t / 2.0) / (PI * xmax)
            };
            assert!(close(tail - plane[0], charges[0][0], 1e-3), "{st:?}: {plane:?} {tail} {charges:?}");
        }
    }

    #[test]
    fn conductor_loss_converged() {
        // The skin-effect resistance and the dielectric conductance at the default
        // discretization against a finer one.
        let acc = Accuracy::default();
        let fine = Accuracy { grading: 0.12, fine: 0.0003 };
        let mask = Some(Mask { thickness: 0.025, over_copper: 0.015, er: 3.8 });
        let ms = strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.4);
        let split = Structure { er2: 3.6, ..strip(Model::Stripline, 0.15, 0.25, 0.0175, 4.2) };
        for (st, gap) in [
            (ms, None),
            (Structure { mask, coplanar_gap: Some(0.15), etch: 0.008, ..ms }, Some(0.15)),
            (split, Some(0.12)),
        ] {
            let cs = st.cross_section(0.2, gap).unwrap();
            let (a, b) = (conductor_loss(&cs, acc).unwrap(), conductor_loss(&cs, fine).unwrap());
            for i in 0..a.len() {
                assert!(close(a[i][i], b[i][i], 0.01), "{st:?}: {a:?} {b:?}");
            }
        }
        let cs = split.cross_section(0.2, Some(0.12)).unwrap();
        let tans = [0.02, 0.015];
        let g = |acc| dielectric_loss(&cs, &solve(&cs, acc).unwrap(), &tans, 0.0, acc).unwrap();
        let (a, b) = (g(acc), g(fine));
        assert!(close(a[0][0], b[0][0], 2e-3) && close(a[0][1], b[0][1], 5e-3), "{a:?} {b:?}");
        // Zero thickness: no conductor loss model.
        let thin = strip(Model::Microstrip, 0.2, 0.0, 0.0, 4.4);
        assert!(thin.line_matrices(0.2, None, false, &LossTangents::default(), acc).is_err());
    }

    #[test]
    fn converged() {
        // The default discretization against a much finer one, on the features without an
        // analytic reference: thick conductors, mask, coplanar ground, etch, split dielectrics.
        let fine = Accuracy { grading: 0.1, fine: 0.0002 };
        let mask = Some(Mask { thickness: 0.025, over_copper: 0.015, er: 3.8 });
        let ms = strip(Model::Microstrip, 0.2, 0.0, 0.035, 4.4);
        let cases = [
            (Structure { mask, coplanar_gap: Some(0.15), etch: 0.008, ..ms }, Some(0.15)),
            (Structure { mask, ..ms }, None),
            (Structure { er2: 3.6, ..strip(Model::Stripline, 0.15, 0.25, 0.0175, 4.2) }, Some(0.12)),
            (Structure { broadside: Some((0.1, 3.9)), ..strip(Model::Stripline, 0.2, 0.2, 0.0175, 4.2) }, None),
        ];
        for (st, gap) in cases {
            let w = 0.2;
            let (a, b) = (st.evaluate(w, gap, Accuracy::default()).unwrap(), st.evaluate(w, gap, fine).unwrap());
            assert!(close(a.line.z0, b.line.z0, 5e-4), "{st:?}: {a:?} {b:?}");
            assert!(close(a.line.er_eff, b.line.er_eff, 5e-4), "{st:?}: {a:?} {b:?}");
            if let (Some(p), Some(q)) = (a.pair, b.pair) {
                assert!(close(p.zdiff, q.zdiff, 5e-4) && close(p.zcomm, q.zcomm, 5e-4), "{st:?}: {p:?} {q:?}");
            }
        }
    }
}
