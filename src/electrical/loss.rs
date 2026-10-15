//! Frequency-dependent losses of transmission lines: conductor loss with skin effect and
//! surface roughness, dielectric loss from the loss tangent, and the resulting RLGC per unit
//! length, complex characteristic impedance and attenuation. See `docs/ELECTRICAL.md`
//! ("Losses").
//!
//! The frequency-independent part of a line is a [`LineMatrices`] (C, external L, G/ω and the
//! skin-effect resistance per surface resistance R/Rs), from the field solver
//! ([`super::fieldsolver::Structure::line_matrices`]) or from the closed forms
//! ([`closed_form`]). [`at`] and [`pair_at`] evaluate it at a frequency.
//!
//! Sources:
//! - skin effect: Rs = √(π f μ₀ / σ), δ = 1 / √(π f μ₀ σ); internal inductance of a smooth
//!   surface Rs / ω per unit of R/Rs (S. Ramo, J. R. Whinnery, T. Van Duzer, *Fields and Waves in
//!   Communication Electronics*, surface impedance of a good conductor);
//! - Wheeler's incremental inductance rule (H. A. Wheeler, "Formulas for the Skin Effect", Proc.
//!   IRE 30, 1942): R = (Rs / μ₀) ∂L/∂n, every conductor wall receding by n, applied to the
//!   closed-form impedance models (D29);
//! - roughness: Hammerstad–Jensen, K = 1 + (2/π) atan(1.4 (Δ/δ)²) (E. Hammerstad, Ø. Jensen,
//!   IEEE MTT-S 1980), and Huray's snowball model, K = 1 + (3/2) SR / (1 + δ/a + δ²/(2a²)),
//!   SR = N·4πa²/A_flat (P. G. Huray et al., "Impact of Copper Surface Texture on Loss: A Model
//!   that Works", DesignCon 2010; P. G. Huray, *The Foundations of Signal Integrity*, 2009);
//! - dielectric loss: first-order perturbation with complex permittivity, G = ω tan δ ε ∂C/∂ε
//!   (for the closed forms ω C tan δ (εr/εeff) ∂εeff/∂εr; D. M. Pozar, *Microwave
//!   Engineering*, gives the same with εeff taken linear in εr, the filling factor
//!   q = (εeff − 1)/(εr − 1): αd = k₀ εr (εeff − 1) tan δ / (2 √εeff (εr − 1)), about 1 % higher).

use std::f64::consts::PI;

use super::complex::C64;
use super::fieldsolver::{DielectricGroup, LineMatrices, inv2, mul2, sqrt2};
use super::impedance::{Geometry, Model};

/// Conductivity of annealed copper, S/m: 5.8 × 10⁷ (IEC 60028, the International Annealed
/// Copper Standard: 58 MS/m at 20 °C). Electrodeposited PCB copper is close to it.
pub const COPPER_CONDUCTIVITY: f64 = 5.8e7;

/// Speed of light in vacuum, m/s.
const C0: f64 = 299_792_458.0;
/// Vacuum permeability, H/m (μ₀ = η₀ / c).
const MU0: f64 = super::impedance::ETA0 / C0;

/// Neper to decibel: 20 / ln 10.
pub const DB_PER_NEPER: f64 = 8.685_889_638_065_037;

/// Skin depth, m, of a conductor of conductivity `sigma` (S/m) at `f` (Hz).
pub fn skin_depth(f: f64, sigma: f64) -> f64 {
    1.0 / (PI * f * MU0 * sigma).sqrt()
}

/// Surface resistance, Ω, of a conductor of conductivity `sigma` (S/m) at `f` (Hz).
pub fn surface_resistance(f: f64, sigma: f64) -> f64 {
    (PI * f * MU0 / sigma).sqrt()
}

/// Surface roughness model (applied to every conductor surface, the planes included).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Roughness {
    /// Smooth copper.
    #[default]
    Smooth,
    /// Hammerstad–Jensen with this RMS roughness, m.
    HammerstadJensen {
        /// RMS surface roughness, m.
        rms: f64,
    },
    /// Huray's snowball model: sphere radius (m) and surface ratio N·4πa²/A_flat.
    Huray {
        /// Snowball radius, m.
        radius: f64,
        /// Total snowball surface per flat area.
        ratio: f64,
    },
}

impl Roughness {
    /// Loss multiplier at skin depth `delta` (m).
    pub fn factor(&self, delta: f64) -> f64 {
        match *self {
            Roughness::Smooth => 1.0,
            Roughness::HammerstadJensen { rms } => 1.0 + 2.0 / PI * (1.4 * (rms / delta).powi(2)).atan(),
            Roughness::Huray { radius, ratio } => {
                1.0 + 1.5 * ratio / (1.0 + delta / radius + delta * delta / (2.0 * radius * radius))
            }
        }
    }
}

/// Conductor properties for the loss calculation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Conductor {
    /// Conductivity, S/m.
    pub sigma: f64,
    /// Surface roughness.
    pub roughness: Roughness,
    /// DC resistance of one trace, Ω/m (the floor of the resistance at low frequencies).
    pub r_dc: f64,
}

/// A single line at one frequency.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LossPoint {
    /// Frequency, Hz.
    pub freq: f64,
    /// Resistance, Ω/m.
    pub r: f64,
    /// Inductance (external plus internal), H/m.
    pub l: f64,
    /// Conductance, S/m.
    pub g: f64,
    /// Capacitance, F/m.
    pub c: f64,
    /// Complex characteristic impedance, Ω.
    pub z0: C64,
    /// Attenuation, Np/m (real part of the propagation constant).
    pub alpha: f64,
    /// Phase constant, rad/m.
    pub beta: f64,
    /// Conductor part of the attenuation, R / (2 Z₀), Np/m.
    pub alpha_c: f64,
    /// Dielectric part of the attenuation, G Z₀ / 2, Np/m.
    pub alpha_d: f64,
    /// Skin depth, m.
    pub skin_depth: f64,
    /// Roughness multiplier.
    pub roughness: f64,
}

/// A coupled pair at one frequency (differential excitation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairLossPoint {
    /// Attenuation of a differential signal, Np/m.
    pub alpha_diff: f64,
    /// Its conductor part, Np/m.
    pub alpha_diff_c: f64,
    /// Its dielectric part, Np/m.
    pub alpha_diff_d: f64,
}

/// Skin-effect resistance at `f` with the roughness and the DC floor: (R, internal reactance
/// per ω) per unit of `r_over_rs`.
fn surface(f: f64, k: &Conductor) -> (f64, f64, f64, f64) {
    let rs = surface_resistance(f, k.sigma);
    let delta = skin_depth(f, k.sigma);
    let rough = k.roughness.factor(delta);
    (rs, rs / (2.0 * PI * f), delta, rough)
}

/// The single line of `m` (1×1, or element `[0][0]` of a pair's matrices) at `f`.
pub fn at(m: &LineMatrices, f: f64, k: &Conductor) -> LossPoint {
    let w = 2.0 * PI * f;
    let (rs, xi, delta, rough) = surface(f, k);
    let rr = m.r_over_rs[0][0];
    let r = (rs * rough * rr).max(k.r_dc);
    let l = m.l[0][0] + xi * rr;
    let (cf, gf) = m.at_freq(f);
    let (g, c) = (w * gf[0][0], cf[0][0]);
    let zs = C64::new(r, w * l);
    let ys = C64::new(g, w * c);
    let z0 = (zs / ys).sqrt();
    let gamma = (zs * ys).sqrt();
    let z_lossless = (m.l[0][0] / c).sqrt();
    LossPoint {
        freq: f,
        r,
        l,
        g,
        c,
        z0,
        alpha: gamma.re,
        beta: gamma.im,
        alpha_c: r / (2.0 * z_lossless),
        alpha_d: g * z_lossless / 2.0,
        skin_depth: delta,
        roughness: rough,
    }
}

/// A pair's differential attenuation at `f`: the power-loss formula
/// α = (iᵀ R i + vᵀ G v) / (2 vᵀ i) for the differential voltage v = (1, −1) and its current
/// i = Z⁻¹ v, Z the lossless characteristic impedance matrix C⁻¹ (C L)^½ (for a symmetric pair,
/// exactly the odd mode: (R₁₁ − R₁₂) / (2 Z_odd) + (G₁₁ − G₁₂) Z_odd / 2).
pub fn pair_at(m: &LineMatrices, f: f64, k: &Conductor) -> PairLossPoint {
    let w = 2.0 * PI * f;
    let (rs, _, _, rough) = surface(f, k);
    let to2 = |x: &Vec<Vec<f64>>| [[x[0][0], x[0][1]], [x[1][0], x[1][1]]];
    let (cf, gf) = m.at_freq(f);
    let (c, l) = (to2(&cf), to2(&m.l));
    let mut r = to2(&m.r_over_rs);
    for row in &mut r {
        for x in row {
            *x *= rs * rough;
        }
    }
    if r[0][0].min(r[1][1]) < k.r_dc {
        // Below the skin-effect range: each trace's DC resistance, return through the planes.
        r = [[k.r_dc, 0.0], [0.0, k.r_dc]];
    }
    let g = to2(&gf);
    // Y = Z⁻¹ = (C L)^(−½) C.
    let y = mul2(inv2(sqrt2(mul2(c, l))), c);
    let v = [1.0, -1.0];
    let i = [y[0][0] * v[0] + y[0][1] * v[1], y[1][0] * v[0] + y[1][1] * v[1]];
    let quad = |a: [[f64; 2]; 2], x: [f64; 2]| {
        x[0] * (a[0][0] * x[0] + a[0][1] * x[1]) + x[1] * (a[1][0] * x[0] + a[1][1] * x[1])
    };
    let p2 = 2.0 * (v[0] * i[0] + v[1] * i[1]);
    let ac = quad(r, i) / p2;
    let ad = w * quad(g, v) / p2;
    PairLossPoint { alpha_diff: ac + ad, alpha_diff_c: ac, alpha_diff_d: ad }
}

/// External inductance per meter of a closed-form line (independent of the permittivity:
/// Z₀ √εeff / c).
fn closed_inductance(g: &Geometry, w: f64) -> f64 {
    let l = g.line(w);
    l.z0 * l.er_eff.sqrt() / C0
}

/// Wheeler's incremental inductance rule on the closed-form model: R/Rs = (1/μ₀) ∂L/∂n, every
/// wall receding by n (the strip loses n on each face, the planes move n away), as a central
/// difference. In 1/m; lengths of `g` and `w` in millimeters. `None` without copper thickness.
pub fn wheeler_r_over_rs(g: &Geometry, w: f64) -> Option<f64> {
    if g.t <= 0.0 {
        return None;
    }
    let d = 1e-4 * w.min(g.t).min(g.h);
    let receded = |n: f64| {
        let r = match g.model {
            Model::Microstrip | Model::EmbeddedMicrostrip => {
                Geometry { h: g.h + 2.0 * n, t: g.t - 2.0 * n, h2: g.h2 + n, ..*g }
            }
            Model::Stripline => Geometry { h: g.h + 2.0 * n, h2: g.h2 + 2.0 * n, t: g.t - 2.0 * n, ..*g },
        };
        closed_inductance(&r, w - 2.0 * n)
    };
    // n in mm: ∂L/∂n per mm → per m.
    Some((receded(d) - receded(-d)) / (2.0 * d) * 1000.0 / MU0)
}

/// Closed-form line matrices: C and L from the closed-form Z₀ and εeff, G = ω C tan δ ·
/// (εr / εeff) ∂εeff/∂εr from the closed-form εeff (a stripline is fully filled), R/Rs from
/// Wheeler's rule on the closed-form model. `tan_d` is the loss tangent (for a stripline
/// between two dielectrics, their mean weighted by εr × thickness). `None` without copper
/// thickness (no conductor loss model).
pub fn closed_form(g: &Geometry, w: f64, tan_d: f64) -> Option<LineMatrices> {
    closed_form_dispersive(g, w, tan_d, None)
}

/// [`closed_form`] with a Djordjevic–Sarkar reference frequency (Hz) for the dielectric (`εr`
/// and `tan_d` given there): C and G then follow εr(f) and tan δ(f) through the filling factor.
pub fn closed_form_dispersive(g: &Geometry, w: f64, tan_d: f64, ref_freq: Option<f64>) -> Option<LineMatrices> {
    let line = g.line(w);
    let c = line.er_eff.sqrt() / (line.z0 * C0);
    let l = line.z0 * line.er_eff.sqrt() / C0;
    // Share of the electric energy in the dielectric: (εr / εeff) ∂εeff/∂εr (one-sided
    // second-order difference upwards, εr stays at least 1).
    let fill = match g.model {
        Model::Stripline => 1.0,
        Model::Microstrip | Model::EmbeddedMicrostrip => {
            let d = 1e-4 * g.er;
            let ee = |e: f64| Geometry { er: e, ..*g }.line(w).er_eff;
            let de = (-3.0 * line.er_eff + 4.0 * ee(g.er + d) - ee(g.er + 2.0 * d)) / (2.0 * d);
            g.er * de / line.er_eff
        }
    };
    let r = wheeler_r_over_rs(g, w)?;
    Some(LineMatrices {
        c: vec![vec![c]],
        l: vec![vec![l]],
        g_over_omega: vec![vec![c * tan_d * fill]],
        r_over_rs: vec![vec![r]],
        groups: vec![DielectricGroup { tan_d, ref_freq, partial: vec![vec![c * fill]] }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::electrical::fieldsolver::{Accuracy, LossTangents, Structure};
    use crate::electrical::impedance::{microstrip, stripline};

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b.abs()
    }

    fn copper(r_dc: f64) -> Conductor {
        Conductor { sigma: COPPER_CONDUCTIVITY, roughness: Roughness::Smooth, r_dc }
    }

    /// Pucel, Massé, Hartwig, "Losses in Microstrip", IEEE MTT-16 (1968), with the corrections
    /// as given in K. C. Gupta, R. Garg, I. Bahl, P. Bhartia, *Microstrip Lines and Slotlines*:
    /// αc Z₀ h / Rs in dB.
    fn pucel(u: f64, tn: f64) -> f64 {
        let we = if u >= 1.0 / (2.0 * PI) {
            u + tn / PI * (1.0 + (2.0 / tn).ln())
        } else {
            u + tn / PI * (1.0 + (4.0 * PI * u / tn).ln())
        };
        let b = 1.0 + 1.0 / we + 1.0 / (PI * we) * ((2.0 / tn).ln() - tn);
        if u <= 1.0 / (2.0 * PI) {
            let b = 1.0 + 1.0 / we + 1.0 / (PI * we) * ((4.0 * PI * u / tn).ln() + tn / u);
            8.68 / (2.0 * PI) * (1.0 - (we / 4.0).powi(2)) * b
        } else if u <= 2.0 {
            8.68 / (2.0 * PI) * (1.0 - (we / 4.0).powi(2)) * b
        } else {
            let d = we + 2.0 / PI * (2.0 * PI * std::f64::consts::E * (we / 2.0 + 0.94)).ln();
            8.68 / (d * d) * (we + we / PI / (we / 2.0 + 0.94)) * b
        }
    }

    #[test]
    fn skin_depth_of_copper() {
        // Copper (5.8e7 S/m): δ = 2.09 µm and Rs = 8.25 mΩ at 1 GHz, δ = 66.1 µm at 1 MHz.
        assert!(close(skin_depth(1e9, COPPER_CONDUCTIVITY), 2.0898e-6, 1e-3));
        assert!(close(surface_resistance(1e9, COPPER_CONDUCTIVITY), 8.2504e-3, 1e-3));
        assert!(close(skin_depth(1e6, COPPER_CONDUCTIVITY), 66.09e-6, 1e-3));
    }

    #[test]
    fn roughness_factors() {
        let hj = Roughness::HammerstadJensen { rms: 1e-6 };
        assert!(close(hj.factor(1e-6), 1.0 + 2.0 / PI * 1.4f64.atan(), 1e-12));
        assert!(close(hj.factor(1e-9), 2.0, 1e-6));
        assert!(close(hj.factor(1.0), 1.0, 1e-9));
        let hu = Roughness::Huray { radius: 0.5e-6, ratio: 2.0 };
        assert!(close(hu.factor(1e-12), 4.0, 1e-5));
        assert!(close(hu.factor(0.5e-6), 1.0 + 3.0 / 2.5, 1e-12));
    }

    #[test]
    fn pozar_stripline_example() {
        // Pozar, Microwave Engineering, Example 3.5: b = 3.2 mm, εr 2.2, W = 2.66 mm (50 Ω),
        // t = 0.01 mm, copper, tan δ 0.001 at 10 GHz: αd = 0.155 Np/m, αc = 0.122 Np/m
        // (Pozar's closed form of Wheeler's rule, Rs = 0.026 Ω).
        let g = Geometry { model: Model::Stripline, h: 1.595, h2: 1.595, t: 0.01, er: 2.2 };
        let m = closed_form(&g, 2.66, 0.001).unwrap();
        let p = at(&m, 10e9, &copper(0.0));
        assert!(close(p.alpha_d, 0.155, 0.01), "{p:?}");
        assert!(close(p.alpha_c, 0.122, 0.03), "{p:?}");
        assert!(close(p.alpha, p.alpha_c + p.alpha_d, 1e-3), "{p:?}");
        // Pozar's formula itself, A = 1 + 2W/(b−t) + (b+t)/(π(b−t)) ln((2b−t)/t).
        let (b, t, w) = (3.2, 0.01, 2.66);
        let a = 1.0 + 2.0 * w / (b - t) + (b + t) / (PI * (b - t)) * ((2.0 * b - t) / t).ln();
        let rs = surface_resistance(10e9, COPPER_CONDUCTIVITY);
        let z0 = stripline(w, 1.595, 1.595, t, 2.2).z0;
        let pozar = 2.7e-3 * rs * 2.2 * z0 * a / (30.0 * PI * (b - t) * 1e-3);
        assert!(close(p.alpha_c, pozar, 0.03), "{} vs {pozar}", p.alpha_c);
        // The field solver: exact dielectric loss for a homogeneous dielectric, conductor loss
        // within a few % of the closed forms.
        let st = Structure {
            model: Model::Stripline,
            h: 1.595,
            er: 2.2,
            h2: 1.595,
            er2: 2.2,
            t: 0.01,
            etch: 0.0,
            broadside: None,
            coplanar_gap: None,
            mask: None,
        };
        let tans = LossTangents { below: 0.001, above: 0.001, ..LossTangents::default() };
        let fm = st.line_matrices(2.66, None, false, &tans, Accuracy::default()).unwrap();
        let fp = at(&fm, 10e9, &copper(0.0));
        let exact = PI * 10e9 * 2.2f64.sqrt() * 0.001 / C0;
        assert!(close(fp.alpha_d, exact, 1e-4), "{fp:?} vs {exact}");
        assert!(close(fp.alpha_c, pozar, 0.05), "{fp:?} vs {pozar}");
    }

    #[test]
    fn microstrip_conductor_loss() {
        // Wheeler's rule on Hammerstad–Jensen, and the field solver, against Pucel's formulas.
        let (h, t, er) = (0.5, 0.035, 4.4);
        for u in [0.5, 1.0, 4.0] {
            let w = u * h;
            let g = Geometry { model: Model::Microstrip, h, h2: 0.0, t, er };
            let rs = 1.0;
            let z0 = microstrip(w, h, t, er).z0;
            // dB per mm → Np/m.
            let pucel_np = pucel(u, t / h) * rs / (z0 * h) / DB_PER_NEPER * 1000.0;
            let cf = closed_form(&g, w, 0.0).unwrap();
            let a_cf = rs * cf.r_over_rs[0][0] / (2.0 * z0);
            assert!(close(a_cf, pucel_np, 0.08), "u={u}: Wheeler/HJ {a_cf} vs Pucel {pucel_np}");
            let st = Structure {
                model: Model::Microstrip,
                h,
                er,
                h2: 0.0,
                er2: er,
                t,
                etch: 0.0,
                broadside: None,
                coplanar_gap: None,
                mask: None,
            };
            let fm = st.line_matrices(w, None, false, &LossTangents::default(), Accuracy::default()).unwrap();
            let zf = (fm.l[0][0] / fm.c[0][0]).sqrt();
            let a_fs = rs * fm.r_over_rs[0][0] / (2.0 * zf);
            assert!(close(a_fs, a_cf, 0.015), "u={u}: field solver {a_fs} vs Wheeler/HJ {a_cf}");
        }
    }

    #[test]
    fn microstrip_dielectric_filling() {
        // The field solver's partial capacitance against the filling factor of the closed forms
        // (zero thickness, where Hammerstad–Jensen's εeff is within 0.2 %).
        let st = Structure {
            model: Model::Microstrip,
            h: 1.0,
            er: 4.4,
            h2: 0.0,
            er2: 4.4,
            t: 0.0,
            etch: 0.0,
            broadside: None,
            coplanar_gap: None,
            mask: None,
        };
        let cs = st.cross_section(1.0, None).unwrap();
        let cap = crate::electrical::fieldsolver::solve(&cs, Accuracy::default()).unwrap();
        let g = crate::electrical::fieldsolver::dielectric_loss(&cs, &cap, &[0.02], 0.0, Accuracy::default()).unwrap();
        let g = g[0][0] / cap.c[0][0];
        let geo = Geometry { model: Model::Microstrip, h: 1.0, h2: 0.0, t: 0.0, er: 4.4 };
        let cf = closed_form(&Geometry { t: 0.035, ..geo }, 1.0, 0.02).unwrap();
        let zero_t = {
            let d = 1e-4;
            let ee = |e: f64| microstrip(1.0, 1.0, 0.0, e).er_eff;
            4.4 * (ee(4.4 + d) - ee(4.4 - d)) / (2.0 * d) / ee(4.4) * 0.02
        };
        assert!(close(g, zero_t, 0.004), "{g} vs {zero_t}");
        // Pozar's filling factor: within about 1 %.
        let hj = microstrip(1.0, 1.0, 0.0, 4.4);
        let q = 4.4 * (hj.er_eff - 1.0) / (hj.er_eff * 3.4) * 0.02;
        assert!(close(g, q, 0.015), "{g} vs {q}");
        // The closed-form matrices use the same derivative (here with thickness).
        let ratio = cf.g_over_omega[0][0] / cf.c[0][0];
        assert!(close(ratio, zero_t, 0.02), "{ratio} vs {zero_t}");
    }

    #[test]
    fn wide_strip_limit() {
        // A wide microstrip is nearly a parallel-plate line: R → 2 Rs / W_eff (strip and plane),
        // W_eff = h C₀/ε₀ the width that gives its vacuum capacitance.
        let st = Structure {
            model: Model::Microstrip,
            h: 0.1,
            er: 4.0,
            h2: 0.0,
            er2: 4.0,
            t: 0.01,
            etch: 0.0,
            broadside: None,
            coplanar_gap: None,
            mask: None,
        };
        let w = 5.0;
        let fm = st.line_matrices(w, None, false, &LossTangents::default(), Accuracy::default()).unwrap();
        let c0 = MU0 / fm.l[0][0];
        let r = fm.r_over_rs[0][0] * 0.1e-3 * c0;
        assert!(close(r, 2.0, 0.03), "{r}");
    }

    #[test]
    fn dc_floor_and_internal_inductance() {
        let g = Geometry { model: Model::Microstrip, h: 0.2, h2: 0.0, t: 0.035, er: 4.4 };
        let m = closed_form(&g, 0.35, 0.02).unwrap();
        let r_dc = 1.0 / (COPPER_CONDUCTIVITY * 0.35e-3 * 0.035e-3);
        let lo = at(&m, 1e3, &copper(r_dc));
        assert_eq!(lo.r, r_dc);
        let hi = at(&m, 1e9, &copper(r_dc));
        assert!(hi.r > r_dc && hi.l > m.l[0][0] && hi.l < 1.1 * m.l[0][0]);
        // Rougher copper loses more.
        let rough = at(&m, 1e9, &Conductor { roughness: Roughness::HammerstadJensen { rms: 2e-6 }, ..copper(r_dc) });
        assert!(rough.alpha_c > 1.5 * hi.alpha_c);
    }

    #[test]
    fn symmetric_pair_is_odd_mode() {
        let st = Structure {
            model: Model::Stripline,
            h: 0.2,
            er: 3.8,
            h2: 0.2,
            er2: 3.8,
            t: 0.0175,
            etch: 0.0,
            broadside: None,
            coplanar_gap: None,
            mask: None,
        };
        let tans = LossTangents { below: 0.01, above: 0.01, ..LossTangents::default() };
        let m = st.line_matrices(0.12, Some(0.15), true, &tans, Accuracy::default()).unwrap();
        let p = pair_at(&m, 5e9, &copper(0.0));
        // Odd mode: (R₁₁ − R₁₂)/(2 Z_odd) + (G₁₁ − G₁₂) Z_odd / 2.
        let rs = surface_resistance(5e9, COPPER_CONDUCTIVITY);
        let lo = m.l[0][0] - m.l[0][1];
        let co = m.c[0][0] - m.c[0][1];
        let zo = (lo / co).sqrt();
        let ac = rs * (m.r_over_rs[0][0] - m.r_over_rs[0][1]) / (2.0 * zo);
        let ad = 2.0 * PI * 5e9 * (m.g_over_omega[0][0] - m.g_over_omega[0][1]) * zo / 2.0;
        assert!(close(p.alpha_diff_c, ac, 1e-9) && close(p.alpha_diff_d, ad, 1e-9), "{p:?} {ac} {ad}");
        // Homogeneous dielectric: the dielectric part is exact for every mode.
        let exact = PI * 5e9 * 3.8f64.sqrt() * 0.01 / C0;
        assert!(close(p.alpha_diff_d, exact, 1e-4), "{p:?} {exact}");
        // Coupling crowds the current towards the gap: more conductor loss than one line alone.
        let single = st.line_matrices(0.12, None, false, &tans, Accuracy::default()).unwrap();
        assert!(p.alpha_diff_c > at(&single, 5e9, &copper(0.0)).alpha_c);
    }

    #[test]
    fn dispersive_dielectric_first_order() {
        // Djordjevic–Sarkar laminate (εr 4.3, tan δ 0.02 at 1 GHz): C(f) and G(f)/ω from the
        // partial capacitances against a full solve at εr(f) and tan δ(f), for the field solver
        // (a microstrip: two regions, laminate and air) and the closed forms.
        let d = crate::electrical::dispersion::Dielectric { er: 4.3, tan_d: 0.02, ref_freq: Some(1e9) };
        let base = Structure {
            model: Model::Microstrip,
            h: 0.5,
            er: 4.3,
            h2: 0.0,
            er2: 4.3,
            t: 0.035,
            etch: 0.0,
            broadside: None,
            coplanar_gap: None,
            mask: None,
        };
        let acc = Accuracy::default();
        let tans = LossTangents { below: 0.02, ref_freq: [Some(1e9), None, None], ..LossTangents::default() };
        let m = base.line_matrices(1.0, None, false, &tans, acc).unwrap();
        let geo = Geometry { model: Model::Microstrip, h: 0.5, h2: 0.0, t: 0.035, er: 4.3 };
        let cf = closed_form_dispersive(&geo, 1.0, 0.02, Some(1e9)).unwrap();
        for f in [1e7, 1e8, 1e9, 1e10, 5e10] {
            let (er, tan) = d.at(f);
            let r = Structure { er, er2: er, ..base }
                .line_matrices(1.0, None, false, &LossTangents { below: tan, ..LossTangents::default() }, acc)
                .unwrap();
            let (c, g) = m.at_freq(f);
            assert!(close(c[0][0], r.c[0][0], 3e-4), "{f}: C {} vs {}", c[0][0], r.c[0][0]);
            assert!(close(g[0][0], r.g_over_omega[0][0], 2e-3), "{f}: G/ω {} vs {}", g[0][0], r.g_over_omega[0][0]);
            let rc = closed_form(&Geometry { er, ..geo }, 1.0, tan).unwrap();
            let (c, g) = cf.at_freq(f);
            assert!(close(c[0][0], rc.c[0][0], 3e-4), "{f}: closed-form C {} vs {}", c[0][0], rc.c[0][0]);
            assert!(close(g[0][0], rc.g_over_omega[0][0], 2e-3), "{f}: closed-form G/ω");
        }
        // At the reference frequency, and without dispersion, the stored matrices.
        let (c, g) = m.at_freq(1e9);
        assert!(close(c[0][0], m.c[0][0], 1e-9) && close(g[0][0], m.g_over_omega[0][0], 1e-9));
        let flat = base.line_matrices(1.0, None, false, &LossTangents { below: 0.02, ..LossTangents::default() }, acc);
        let flat = flat.unwrap();
        assert_eq!(flat.at_freq(7e9), (flat.c.clone(), flat.g_over_omega.clone()));
        // Higher frequency: lower εr, so less C; tan δ nearly flat, so G/ω about C tan δ.
        let (lo, hi) = (at(&m, 1e8, &copper(0.0)), at(&m, 1e10, &copper(0.0)));
        assert!(hi.c < lo.c && hi.alpha_d > 50.0 * lo.alpha_d);
    }
}
