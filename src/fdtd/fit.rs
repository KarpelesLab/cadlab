//! Rational fits for the time domain: Debye poles for dispersive dielectrics and a sum of
//! first-order sections for the surface impedance of conductors, both by non-negative least
//! squares over the simulated band (non-negative terms are passive, so the time stepping stays
//! stable).
//!
//! - Dielectrics: ε(ω) ≈ ε∞ + σ/(jωε₀) + Σₚ Δεₚ / (1 + jωτₚ), with relaxation times τₚ shared
//!   by every material of a run (so the permittivities of neighbouring cells average term by
//!   term), fitted to the Djordjevic–Sarkar model ([`crate::electrical::dispersion`]). The
//!   model is itself a continuum of Debye terms, so a few per decade reproduce it closely.
//! - Conductors: Zs(s) ≈ r₀ + Σₚ rₚ s / (s + aₚ) (resistors and parallel R–L sections) fitted to
//!   Zs(ω) = Rs(ω) K(ω) + j Rs(ω): the skin-effect surface impedance √(jωμ₀/σ) with the
//!   roughness factor K on its resistance, as the loss model of D52 (the internal reactance
//!   stays the smooth one).
//!
//! The least-squares solver is Lawson and Hanson's active-set NNLS (C. L. Lawson, R. J. Hanson,
//! *Solving Least Squares Problems*, 1974, ch. 23) on Householder QR.

use std::f64::consts::PI;

use crate::electrical::complex::C64;
use crate::electrical::dispersion::DjordjevicSarkar;
use crate::electrical::loss::{Roughness, surface_resistance};

use super::engine::{EPS0, SurfaceImpedance};

/// Least squares `min |A x − b|` on the columns `cols` of `a` (column-major, `m` rows) by
/// Householder QR; zero for a column that is (numerically) dependent on the previous ones.
fn lstsq(a: &[Vec<f64>], cols: &[usize], b: &[f64]) -> Vec<f64> {
    let m = b.len();
    let k = cols.len();
    let mut q: Vec<Vec<f64>> = cols.iter().map(|&c| a[c].clone()).collect();
    let mut y = b.to_vec();
    let mut diag = vec![0.0; k];
    for j in 0..k {
        let norm = q[j][j..].iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm == 0.0 {
            continue;
        }
        let alpha = if q[j][j] > 0.0 { -norm } else { norm };
        let mut v: Vec<f64> = q[j][j..].to_vec();
        v[0] -= alpha;
        let vn = v.iter().map(|x| x * x).sum::<f64>();
        diag[j] = alpha;
        if vn == 0.0 {
            continue;
        }
        let apply = |col: &mut [f64]| {
            let d = v.iter().zip(&col[j..]).map(|(a, b)| a * b).sum::<f64>() * 2.0 / vn;
            for (c, vi) in col[j..].iter_mut().zip(&v) {
                *c -= d * vi;
            }
        };
        for col in q.iter_mut().skip(j + 1) {
            apply(col);
        }
        apply(&mut y);
    }
    let mut x = vec![0.0; k];
    let scale = diag.iter().fold(0.0f64, |a, d| a.max(d.abs()));
    for j in (0..k).rev() {
        if diag[j].abs() <= 1e-12 * scale {
            continue;
        }
        let mut s = y[j];
        for (i, xi) in x.iter().enumerate().skip(j + 1) {
            s -= q[i][j] * xi;
        }
        x[j] = s / diag[j];
    }
    let _ = m;
    x
}

/// Non-negative least squares: `min |A x − b|` with `x ≥ 0`, `a` column-major (`n` columns of
/// `b.len()` rows). Columns are scaled to unit norm internally.
pub fn nnls(a: &[Vec<f64>], b: &[f64]) -> Vec<f64> {
    let n = a.len();
    let norms: Vec<f64> = a.iter().map(|c| c.iter().map(|v| v * v).sum::<f64>().sqrt()).collect();
    let a: Vec<Vec<f64>> =
        a.iter().zip(&norms).map(|(c, &s)| c.iter().map(|v| if s > 0.0 { v / s } else { 0.0 }).collect()).collect();
    let m = b.len();
    let mut x = vec![0.0; n];
    let mut passive = vec![false; n];
    let residual = |x: &[f64]| -> Vec<f64> {
        let mut r = b.to_vec();
        for (c, xi) in a.iter().zip(x) {
            if *xi != 0.0 {
                for (ri, ci) in r.iter_mut().zip(c) {
                    *ri -= ci * xi;
                }
            }
        }
        r
    };
    let tol = 1e-12 * (m as f64).sqrt();
    for _ in 0..3 * n + 10 {
        let r = residual(&x);
        let w: Vec<f64> = a.iter().map(|c| c.iter().zip(&r).map(|(a, b)| a * b).sum()).collect();
        let Some(t) = (0..n).filter(|&j| !passive[j] && w[j] > tol).max_by(|&i, &j| w[i].total_cmp(&w[j])) else {
            break;
        };
        passive[t] = true;
        loop {
            let cols: Vec<usize> = (0..n).filter(|&j| passive[j]).collect();
            let zp = lstsq(&a, &cols, b);
            let mut z = vec![0.0; n];
            for (&c, v) in cols.iter().zip(&zp) {
                z[c] = *v;
            }
            if cols.iter().all(|&c| z[c] > 0.0) {
                x = z;
                break;
            }
            let alpha =
                cols.iter().filter(|&&c| z[c] <= 0.0).map(|&c| x[c] / (x[c] - z[c])).fold(f64::INFINITY, f64::min);
            for j in 0..n {
                x[j] += alpha * (z[j] - x[j]);
                if passive[j] && x[j] <= 1e-15 {
                    passive[j] = false;
                    x[j] = 0.0;
                }
            }
            if !passive.iter().any(|&p| p) {
                break;
            }
        }
    }
    x.iter().zip(&norms).map(|(v, &s)| if s > 0.0 { v / s } else { 0.0 }).collect()
}

/// `count` frequencies spaced logarithmically from `lo` to `hi` (Hz).
fn log_space(lo: f64, hi: f64, count: usize) -> Vec<f64> {
    let (a, b) = (lo.ln(), hi.ln());
    (0..count).map(|i| (a + (b - a) * i as f64 / (count.max(2) - 1) as f64).exp()).collect()
}

/// Debye relaxation times (s) for the band `lo .. hi` (Hz): three per two decades from a third
/// of `lo` to three times `hi`, at least six (six for a two-decade band: εr within 0.1 %, ε″
/// within 0.6 % of the Djordjevic–Sarkar model).
pub fn relaxation_times(lo: f64, hi: f64) -> Vec<f64> {
    // At least one per decade over 5 decades around a narrow band.
    let per = (5.0 / (9.0 * hi / lo).log10()).max(1.5);
    corners(lo / 3.0, hi * 3.0, per).into_iter().map(|f| 1.0 / (2.0 * PI * f)).collect()
}

/// Corner frequencies log-spaced from `a` to `b` (Hz), `per_decade` per decade (and both ends).
fn corners(a: f64, b: f64, per_decade: f64) -> Vec<f64> {
    let count = ((b / a).log10() * per_decade).ceil() as usize + 1;
    log_space(a, b, count)
}

/// A material as Debye terms on shared relaxation times.
#[derive(Clone, Debug, PartialEq)]
pub struct DebyeFit {
    /// High-frequency relative permittivity (at least 1).
    pub eps_inf: f64,
    /// Conductivity (S/m).
    pub sigma: f64,
    /// Δε per relaxation time.
    pub delta: Vec<f64>,
}

impl DebyeFit {
    /// The fitted relative permittivity ε′ − jε″ at `f` (Hz).
    pub fn eps(&self, taus: &[f64], f: f64) -> C64 {
        let w = 2.0 * PI * f;
        let mut e = C64::new(self.eps_inf, -self.sigma / (w * EPS0));
        for (d, t) in self.delta.iter().zip(taus) {
            e = e + C64::new(*d, 0.0) / C64::new(1.0, w * t);
        }
        e
    }
}

/// Fits a Djordjevic–Sarkar permittivity over `lo .. hi` (Hz) on the relaxation times `taus`:
/// ε′ and ε″ each in relative terms at 64 log-spaced frequencies.
pub fn debye_fit(m: &DjordjevicSarkar, taus: &[f64], lo: f64, hi: f64) -> DebyeFit {
    let freqs = log_space(lo, hi, 64);
    let n = taus.len() + 2;
    let rows = 2 * freqs.len();
    let mut a = vec![vec![0.0; rows]; n];
    let mut b = vec![0.0; rows];
    for (i, &f) in freqs.iter().enumerate() {
        let w = 2.0 * PI * f;
        let e = m.eps(f);
        let (wr, wi) = (1.0 / e.re, 1.0 / (-e.im).max(1e-12 * e.re));
        // Real row: (ε∞ − 1) + Σ Δε / (1 + ω²τ²) = ε′ − 1.
        a[0][2 * i] = wr;
        b[2 * i] = (e.re - 1.0) * wr;
        // Imaginary row: σ/(ωε₀) + Σ Δε ωτ / (1 + ω²τ²) = ε″.
        a[1][2 * i + 1] = wi / (w * EPS0);
        b[2 * i + 1] = -e.im * wi;
        for (p, &t) in taus.iter().enumerate() {
            let d = 1.0 + (w * t).powi(2);
            a[2 + p][2 * i] = wr / d;
            a[2 + p][2 * i + 1] = wi * w * t / d;
        }
    }
    let x = nnls(&a, &b);
    DebyeFit { eps_inf: 1.0 + x[0], sigma: x[1], delta: x[2..].to_vec() }
}

/// The surface impedance of a good conductor of conductivity `sigma` (S/m) with roughness:
/// Rs K + j Rs.
pub fn surface_impedance(f: f64, sigma: f64, roughness: &Roughness) -> C64 {
    let rs = surface_resistance(f, sigma);
    let k = roughness.factor(crate::electrical::loss::skin_depth(f, sigma));
    C64::new(rs * k, rs)
}

/// Fits the surface impedance over `lo .. hi` (Hz): sections with corners two per decade from
/// a decade below to a decade above the band, the resistance in relative terms at 64
/// log-spaced frequencies, the reactance with a fiftieth of that weight.
pub fn surface_fit(sigma: f64, roughness: &Roughness, lo: f64, hi: f64) -> SurfaceImpedance {
    surface_fit_weighted(sigma, roughness, lo, hi, 0.02)
}

fn surface_fit_weighted(sigma: f64, roughness: &Roughness, lo: f64, hi: f64, x_weight: f64) -> SurfaceImpedance {
    let freqs = log_space(lo, hi, 64);
    let corners: Vec<f64> = corners(lo / 10.0, hi * 10.0, 2.0).iter().map(|f| 2.0 * PI * f).collect();
    let n = corners.len() + 1;
    let rows = 2 * freqs.len();
    let mut a = vec![vec![0.0; rows]; n];
    let mut b = vec![0.0; rows];
    for (i, &f) in freqs.iter().enumerate() {
        let z = surface_impedance(f, sigma, roughness);
        let w = 2.0 * PI * f;
        // The resistance (the loss) first: a rough surface's Rs K + j Rs is not a causal pair
        // (a resistance rising faster than √f comes with more reactance), so the fit keeps the
        // resistance and lets the reactance take its causal value (a small part of the line's
        // inductance).
        let (wr, wi) = (1.0 / z.re, x_weight / z.im);
        b[2 * i] = z.re * wr;
        b[2 * i + 1] = z.im * wi;
        a[0][2 * i] = wr;
        for (p, &c) in corners.iter().enumerate() {
            // s/(s + a) = (ω² + jωa) / (ω² + a²).
            let d = w * w + c * c;
            a[1 + p][2 * i] = wr * w * w / d;
            a[1 + p][2 * i + 1] = wi * w * c / d;
        }
    }
    let x = nnls(&a, &b);
    SurfaceImpedance {
        r0: x[0],
        terms: x[1..].iter().zip(&corners).filter(|(r, _)| **r > 0.0).map(|(r, c)| (*r, *c)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::electrical::loss::COPPER_CONDUCTIVITY;

    #[test]
    fn nnls_small() {
        // min |A x − b|, x ≥ 0: the unconstrained optimum (1, −1) is clipped to (x₀ ≥ 0, 0).
        let a = vec![vec![1.0, 0.0, 1.0], vec![0.0, 1.0, 1.0]];
        let x = nnls(&a, &[1.0, -1.0, 0.0]);
        assert!(x[1] == 0.0 && (x[0] - 0.5).abs() < 1e-9, "{x:?}");
        // An exact non-negative solution is found.
        let x = nnls(&a, &[2.0, 3.0, 5.0]);
        assert!((x[0] - 2.0).abs() < 1e-9 && (x[1] - 3.0).abs() < 1e-9, "{x:?}");
    }

    #[test]
    fn debye_fit_reproduces_djordjevic_sarkar() {
        for (er, tan, lo, hi) in [(4.3, 0.02, 1e8, 1e10), (3.5, 0.004, 1e7, 2e10), (4.0, 0.05, 5e8, 5e9)] {
            let m = DjordjevicSarkar::from_reference(er, tan, 1e9);
            let taus = relaxation_times(lo, hi);
            let fit = debye_fit(&m, &taus, lo, hi);
            assert!(fit.eps_inf >= 1.0 && fit.sigma >= 0.0 && fit.delta.iter().all(|d| *d >= 0.0));
            for f in log_space(lo, hi, 101) {
                let (e, w) = (fit.eps(&taus, f), m.eps(f));
                let (te, tw) = (-e.im / e.re, -w.im / w.re);
                assert!((e.re - w.re).abs() < 1e-3 * w.re, "{f}: εr {} vs {}", e.re, w.re);
                assert!((te - tw).abs() < 0.01 * tw, "{f}: tan δ {te} vs {tw}");
            }
        }
    }

    #[test]
    fn surface_fit_reproduces_skin_effect() {
        for rough in [
            Roughness::Smooth,
            Roughness::HammerstadJensen { rms: 1e-6 },
            Roughness::Huray { radius: 0.5e-6, ratio: 2.0 },
        ] {
            let (lo, hi) = (1e8, 1e10);
            let fit = surface_fit(COPPER_CONDUCTIVITY, &rough, lo, hi);
            assert!(fit.r0 >= 0.0 && fit.terms.iter().all(|t| t.0 > 0.0 && t.1 > 0.0));
            for f in log_space(lo, hi, 101) {
                let (z, w) = (fit.z(f), surface_impedance(f, COPPER_CONDUCTIVITY, &rough));
                // Smooth: both within 1 %. Rough: the resistance within 2.5 %, the reactance
                // its causal value, larger than the smooth one D52 adds (at most twice).
                let smooth = rough == Roughness::Smooth;
                let tol = if smooth { 0.01 } else { 0.025 };
                assert!((z.re - w.re).abs() < tol * w.re, "{rough:?} {f}: R {} vs {}", z.re, w.re);
                if smooth {
                    assert!((z.im - w.im).abs() < 0.01 * w.im, "{f}: X {} vs {}", z.im, w.im);
                } else {
                    assert!(z.im > 0.9 * w.im && z.im < 2.0 * w.im, "{rough:?} {f}: X {} vs {}", z.im, w.im);
                }
            }
        }
    }
}
