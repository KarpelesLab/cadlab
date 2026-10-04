//! Characteristic impedance of PCB transmission lines, from published closed-form models.
//!
//! All lengths are in the same unit (millimeters by convention); only ratios matter. Floating
//! point is used here, inside the algorithm only: callers store the results as exact
//! quantities ([`crate::value::Quantity`]) or nanometers.
//!
//! | Model | Formula | Stated accuracy |
//! |---|---|---|
//! | surface microstrip | Hammerstad & Jensen (1980), with their strip thickness correction | < 1 % for 0.01 ≤ W/H ≤ 100 (zero thickness: 0.2 %) |
//! | embedded microstrip | IPC-2141A: εr' = εr (1 − e^(−1.55 H₁/H)), Z = 60/√εr' · ln(5.98 H / (0.8 W + T)) | approximate (commonly quoted ±5 %) for 0.1 < W/H < 2, 1 < εr < 15 |
//! | stripline (symmetric) | Wheeler (1978) with thickness correction, as given by Wadell §3.5.1 | 0.5 % for W/(b − T) < 10 |
//! | stripline (asymmetric) | parallel combination of two symmetric striplines of spacing 2h₁ + T and 2h₂ + T (Wadell / Cohn approximation) | a few % |
//! | edge-coupled differential microstrip | IPC-2141: Zdiff = 2 Z₀ (1 − 0.48 e^(−0.96 S/H)) | approximate (±10 %) |
//! | edge-coupled differential stripline | IPC-2141: Zdiff = 2 Z₀ (1 − 0.347 e^(−2.9 S/b)) | approximate (±10 %) |
//!
//! Sources and discussion in `docs/ELECTRICAL.md`.

use std::f64::consts::{E, PI};

/// Impedance of free space, in ohms (μ₀c).
pub const ETA0: f64 = 376.730_313_668;

/// Speed of light in vacuum, mm per picosecond.
const C_MM_PER_PS: f64 = 0.299_792_458;

/// Transmission line model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Model {
    /// Trace on an outer layer, one reference plane below, air above (solder mask ignored).
    Microstrip,
    /// Trace under a covering dielectric of thickness `cover`, one reference plane below.
    EmbeddedMicrostrip,
    /// Trace between two reference planes (inner layer), at `h` from one and `h2` from the other.
    Stripline,
}

/// Cross-section of a line, without its width (and gap).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// Model.
    pub model: Model,
    /// Dielectric height between the trace and its (nearer) reference plane.
    pub h: f64,
    /// Stripline: dielectric height to the other plane. Embedded microstrip: thickness of the
    /// covering dielectric above the trace. Unused for a surface microstrip.
    pub h2: f64,
    /// Trace (copper) thickness.
    pub t: f64,
    /// Relative permittivity.
    pub er: f64,
}

/// Single-ended line properties.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    /// Characteristic impedance, Ω.
    pub z0: f64,
    /// Effective relative permittivity (propagation).
    pub er_eff: f64,
}

impl Line {
    /// Propagation delay, ps per mm.
    pub fn delay_ps_per_mm(&self) -> f64 {
        self.er_eff.sqrt() / C_MM_PER_PS
    }
}

fn coth(x: f64) -> f64 {
    1.0 / x.tanh()
}

/// Hammerstad–Jensen zero-thickness impedance in air for W/H = `u`.
fn hj_z01(u: f64) -> f64 {
    let f = 6.0 + (2.0 * PI - 6.0) * (-(30.666 / u).powf(0.7528)).exp();
    ETA0 / (2.0 * PI) * (f / u + (1.0 + 4.0 / (u * u)).sqrt()).ln()
}

/// Hammerstad–Jensen effective permittivity for W/H = `u`.
fn hj_er_eff(u: f64, er: f64) -> f64 {
    let a = 1.0
        + ((u.powi(4) + (u / 52.0).powi(2)) / (u.powi(4) + 0.432)).ln() / 49.0
        + (1.0 + (u / 18.1).powi(3)).ln() / 18.7;
    let b = 0.564 * ((er - 0.9) / (er + 3.0)).powf(0.053);
    (er + 1.0) / 2.0 + (er - 1.0) / 2.0 * (1.0 + 10.0 / u).powf(-a * b)
}

/// Surface microstrip of width `w` on a dielectric of height `h`, thickness `t`, permittivity
/// `er` (Hammerstad & Jensen 1980, quasi-static, with their thickness correction).
pub fn microstrip(w: f64, h: f64, t: f64, er: f64) -> Line {
    let u = w / h;
    let (u1, ur) = if t > 0.0 {
        let tn = t / h;
        let du1 = tn / PI * (1.0 + 4.0 * E / (tn * coth((6.517 * u).sqrt()).powi(2))).ln();
        let dur = 0.5 * (1.0 + 1.0 / (er - 1.0).sqrt().cosh()) * du1;
        (u + du1, u + dur)
    } else {
        (u, u)
    };
    let e_r = hj_er_eff(ur, er);
    let z0 = hj_z01(ur) / e_r.sqrt();
    let er_eff = e_r * (hj_z01(u1) / hj_z01(ur)).powi(2);
    Line { z0, er_eff }
}

/// Embedded microstrip (IPC-2141A): trace of width `w`, thickness `t`, at height `h` above its
/// plane, covered by `cover` of the same dielectric above the trace.
pub fn embedded_microstrip(w: f64, h: f64, t: f64, er: f64, cover: f64) -> Line {
    let h1 = h + t + cover;
    let er1 = er * (1.0 - (-1.55 * h1 / h).exp());
    let z0 = 60.0 / er1.sqrt() * (5.98 * h / (0.8 * w + t)).ln();
    Line { z0, er_eff: er1 }
}

/// Symmetric stripline: width `w`, thickness `t`, plane spacing `b` (Wheeler 1978).
fn stripline_symmetric(w: f64, b: f64, t: f64, er: f64) -> f64 {
    let x = t / b;
    let bt = b - t;
    let dw = if x > 0.0 {
        let n = 2.0 / (1.0 + 2.0 / 3.0 * x / (1.0 - x));
        let inner = (x / (2.0 - x)).powi(2) + (0.0796 * x / (w / b + 1.1 * x)).powf(n);
        x / (PI * (1.0 - x)) * (1.0 - 0.5 * inner.ln())
    } else {
        0.0
    };
    let m = w / bt + dw;
    let k = 8.0 / (PI * m);
    ETA0 / (4.0 * PI * er.sqrt()) * (1.0 + 4.0 / (PI * m) * (k + (k * k + 6.27).sqrt())).ln()
}

/// Stripline: width `w`, thickness `t`, dielectric `h1` to one plane and `h2` to the other.
/// Symmetric when `h1 == h2` (Wheeler); otherwise the parallel combination of the two
/// symmetric lines of spacing `2 h1 + t` and `2 h2 + t`.
pub fn stripline(w: f64, h1: f64, h2: f64, t: f64, er: f64) -> Line {
    let z = if (h1 - h2).abs() <= 1e-12 * (h1 + h2) {
        stripline_symmetric(w, h1 + h2 + t, t, er)
    } else {
        let za = stripline_symmetric(w, 2.0 * h1 + t, t, er);
        let zb = stripline_symmetric(w, 2.0 * h2 + t, t, er);
        2.0 * za * zb / (za + zb)
    };
    Line { z0: z, er_eff: er }
}

impl Geometry {
    /// Single-ended line of width `w`.
    pub fn line(&self, w: f64) -> Line {
        match self.model {
            Model::Microstrip => microstrip(w, self.h, self.t, self.er),
            Model::EmbeddedMicrostrip => embedded_microstrip(w, self.h, self.t, self.er, self.h2),
            Model::Stripline => stripline(w, self.h, self.h2, self.t, self.er),
        }
    }

    /// Differential impedance of an edge-coupled pair of width `w`, gap `s` (IPC-2141).
    pub fn differential(&self, w: f64, s: f64) -> f64 {
        let z0 = self.line(w).z0;
        match self.model {
            Model::Microstrip | Model::EmbeddedMicrostrip => 2.0 * z0 * (1.0 - 0.48 * (-0.96 * s / self.h).exp()),
            Model::Stripline => {
                let b = self.h + self.h2 + self.t;
                2.0 * z0 * (1.0 - 0.347 * (-2.9 * s / b).exp())
            }
        }
    }

    /// Impedance of width `w`: single-ended, or differential with a gap.
    pub fn impedance(&self, w: f64, gap: Option<f64>) -> f64 {
        match gap {
            Some(s) => self.differential(w, s),
            None => self.line(w).z0,
        }
    }

    /// Notes about inputs outside a formula's stated range (empty when within range).
    pub fn range_notes(&self, w: f64, gap: Option<f64>) -> Vec<String> {
        let mut v = Vec::new();
        let u = w / self.h;
        match self.model {
            Model::Microstrip => {
                if !(0.01..=100.0).contains(&u) {
                    v.push(format!("W/H = {u:.3} is outside 0.01..100 (Hammerstad-Jensen)"));
                }
            }
            Model::EmbeddedMicrostrip => {
                if !(0.1..=2.0).contains(&u) {
                    v.push(format!("W/H = {u:.3} is outside 0.1..2 (IPC-2141A embedded microstrip)"));
                }
                if !(1.0..=15.0).contains(&self.er) {
                    v.push(format!("εr = {} is outside 1..15 (IPC-2141A)", self.er));
                }
            }
            Model::Stripline => {
                let b = self.h + self.h2 + self.t;
                let r = w / (b - self.t);
                if r > 10.0 {
                    v.push(format!("W/(b - T) = {r:.3} is above 10 (Wheeler stripline)"));
                }
                if self.t / b > 0.25 {
                    v.push(format!("T/b = {:.3} is above 0.25 (Wheeler stripline)", self.t / b));
                }
            }
        }
        if let Some(s) = gap
            && s <= 0.0
        {
            v.push("the gap must be positive".into());
        }
        v
    }

    /// The width giving impedance `target` (with gap `gap` for a differential pair), searched
    /// between `h / 1000` and `100 h`: impedance falls monotonically with width. `None` when the
    /// target is outside what that range reaches.
    pub fn solve_width(&self, target: f64, gap: Option<f64>) -> Option<f64> {
        let (mut lo, mut hi) = ((self.h / 1000.0).ln(), (self.h * 100.0).ln());
        let z = |lw: f64| self.impedance(lw.exp(), gap);
        let (zlo, zhi) = (z(lo), z(hi));
        if !(zlo.is_finite() && zhi.is_finite()) || target > zlo || target < zhi {
            return None;
        }
        for _ in 0..200 {
            let mid = 0.5 * (lo + hi);
            if z(mid) > target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Some((0.5 * (lo + hi)).exp())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b.abs()
    }

    #[test]
    fn microstrip_pozar_example() {
        // Pozar, Microwave Engineering, Example 3.7: εr = 2.2, d = 0.159 cm, Z0 = 50 Ω gives
        // W = 0.490 cm and εe = 1.87 (zero thickness; Pozar's formulas are within ~1 %).
        let l = microstrip(4.90, 1.59, 0.0, 2.2);
        assert!(close(l.z0, 50.0, 0.015), "{l:?}");
        assert!(close(l.er_eff, 1.87, 0.01), "{l:?}");
    }

    #[test]
    fn microstrip_matches_pozar_closed_form() {
        // Pozar (Microwave Engineering, eq. 3.195-3.196, accuracy ~1 %), zero thickness.
        let pozar = |u: f64, er: f64| {
            let ee = (er + 1.0) / 2.0 + (er - 1.0) / 2.0 / (1.0 + 12.0 / u).sqrt();
            let z = if u <= 1.0 {
                60.0 / ee.sqrt() * (8.0 / u + u / 4.0).ln()
            } else {
                120.0 * PI / (ee.sqrt() * (u + 1.393 + 0.667 * (u + 1.444).ln()))
            };
            (z, ee)
        };
        for (u, er) in [(0.2, 4.4), (0.5, 4.4), (1.0, 10.0), (2.0, 4.4), (3.0, 2.2), (8.0, 3.5)] {
            let l = microstrip(u, 1.0, 0.0, er);
            let (z, ee) = pozar(u, er);
            assert!(close(l.z0, z, 0.015), "u={u} er={er}: {l:?} vs {z}");
            assert!(close(l.er_eff, ee, 0.015), "u={u} er={er}: {l:?} vs {ee}");
        }
    }

    #[test]
    fn microstrip_thickness_lowers_impedance() {
        let a = microstrip(0.3, 0.2, 0.0, 4.5).z0;
        let b = microstrip(0.3, 0.2, 0.035, 4.5).z0;
        assert!(b < a && b > 0.85 * a, "{a} {b}");
        // Close to the IPC-2141 estimate 87/√(εr+1.41)·ln(5.98H/(0.8W+T)) in its range.
        let ipc = 87.0 / (4.5f64 + 1.41).sqrt() * (5.98f64 * 0.2 / (0.8 * 0.3 + 0.035)).ln();
        assert!(close(b, ipc, 0.08), "{b} vs {ipc}");
    }

    #[test]
    fn stripline_pozar_example() {
        // Pozar Example 3.5: b = 0.32 cm, εr = 2.2, Z0 = 50 Ω gives W = 0.266 cm (zero thickness).
        let l = stripline(2.66, 1.6, 1.6, 0.0, 2.2);
        assert!(close(l.z0, 50.0, 0.01), "{l:?}");
        let w = Geometry { model: Model::Stripline, h: 1.6, h2: 1.6, t: 0.0, er: 2.2 }.solve_width(50.0, None).unwrap();
        assert!(close(w, 2.66, 0.02), "{w}");
    }

    #[test]
    fn stripline_thickness_and_asymmetry() {
        let sym = stripline(0.15, 0.2, 0.2, 0.0175, 4.2).z0;
        let thin = stripline(0.15, 0.2, 0.2, 0.0, 4.2).z0;
        assert!(sym < thin);
        // Moving the trace towards one plane lowers the impedance.
        let asym = stripline(0.15, 0.15, 0.25, 0.0175, 4.2).z0;
        assert!(asym < sym, "{asym} {sym}");
        // Thickness-corrected Wheeler stays near the IPC-2141 estimate 60/√εr·ln(1.9b/(0.8W+T)).
        let b = 0.4175;
        let ipc = 60.0 / 4.2f64.sqrt() * (1.9f64 * b / (0.8 * 0.15 + 0.0175)).ln();
        assert!(close(sym, ipc, 0.08), "{sym} vs {ipc}");
    }

    #[test]
    fn embedded_microstrip_ipc() {
        // Direct evaluation of the IPC-2141A formula.
        let l = embedded_microstrip(0.2, 0.2, 0.035, 4.2, 0.1);
        let er1 = 4.2 * (1.0 - (-1.55f64 * 0.335 / 0.2).exp());
        let z = 60.0 / er1.sqrt() * (5.98f64 * 0.2 / (0.8 * 0.2 + 0.035)).ln();
        assert!(close(l.z0, z, 1e-12));
        // A cover raises εeff and lowers Z compared with the bare microstrip.
        assert!(l.z0 < microstrip(0.2, 0.2, 0.035, 4.2).z0);
    }

    #[test]
    fn differential_limits() {
        let g = Geometry { model: Model::Microstrip, h: 0.2, h2: 0.0, t: 0.035, er: 4.5 };
        let z0 = g.line(0.2).z0;
        // Far apart: twice the single-ended impedance; close: less.
        assert!(close(g.differential(0.2, 10.0), 2.0 * z0, 1e-6));
        assert!(g.differential(0.2, 0.1) < 2.0 * z0 * 0.8);
        let s = Geometry { model: Model::Stripline, h: 0.2, h2: 0.2, t: 0.0175, er: 4.2 };
        let z = s.differential(0.1, 0.15);
        let z0 = s.line(0.1).z0;
        assert!(close(z, 2.0 * z0 * (1.0 - 0.347 * (-2.9f64 * 0.15 / 0.4175).exp()), 1e-12));
    }

    #[test]
    fn solve_roundtrips() {
        for model in [Model::Microstrip, Model::EmbeddedMicrostrip, Model::Stripline] {
            let g = Geometry { model, h: 0.2, h2: 0.2, t: 0.035, er: 4.4 };
            for target in [40.0, 50.0, 75.0] {
                let w = g.solve_width(target, None).unwrap();
                assert!(close(g.line(w).z0, target, 1e-6), "{model:?} {target}");
            }
            let w = g.solve_width(90.0, Some(0.15)).unwrap();
            assert!(close(g.differential(w, 0.15), 90.0, 1e-6));
        }
        let g = Geometry { model: Model::Microstrip, h: 0.2, h2: 0.0, t: 0.035, er: 4.4 };
        assert!(g.solve_width(1000.0, None).is_none());
        assert!(g.solve_width(1.0, None).is_none());
    }
}
