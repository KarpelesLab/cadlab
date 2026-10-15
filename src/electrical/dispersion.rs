//! Frequency-dependent permittivity of PCB laminates: the Djordjevic–Sarkar wideband Debye
//! model. See `docs/ELECTRICAL.md` ("Losses", "Dispersion").
//!
//! A. R. Djordjevic, R. M. Biljić, V. D. Likar-Smiljanić, T. K. Sarkar, "Wideband
//! Frequency-Domain Characterization of FR-4 and Time-Domain Causality", IEEE Trans. EMC 43
//! (2001): a continuous distribution of Debye relaxations, uniform in log frequency between
//! ω₁ and ω₂, gives
//!
//! ε(ω) = ε∞ + Δε / ln(ω₂/ω₁) · ln((ω₂ + jω) / (ω₁ + jω))  (time dependence e^{jωt}),
//!
//! a causal permittivity (it is a sum of Debye terms) whose loss tangent is nearly constant
//! between the corners while εr falls slowly (by about (2/π) εr tan δ per decade, the
//! Kramers–Kronig partner of a flat tan δ). Given εr and tan δ at one reference frequency (the
//! datasheet's), the two remaining parameters (ε∞ and Δε) follow in closed form. The corners are
//! fixed at [`F1`] and [`F2`], well outside every PCB band.

use std::f64::consts::PI;

use super::complex::C64;

/// Lower corner of the relaxation distribution (Hz): 10 kHz.
pub const F1: f64 = 1e4;
/// Upper corner of the relaxation distribution (Hz): 1 THz.
pub const F2: f64 = 1e12;

/// The logarithm `ln((ω₂ + jω)/(ω₁ + jω))` as (real part a, minus the imaginary part b > 0).
fn log_term(f: f64) -> (f64, f64) {
    let (w, w1, w2) = (2.0 * PI * f, 2.0 * PI * F1, 2.0 * PI * F2);
    let a = 0.5 * ((w2 * w2 + w * w) / (w1 * w1 + w * w)).ln();
    let b = (w / w1).atan() - (w / w2).atan();
    (a, b)
}

/// A Djordjevic–Sarkar permittivity: ε∞ + k · ln((ω₂ + jω)/(ω₁ + jω)), k = Δε / ln(ω₂/ω₁).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DjordjevicSarkar {
    /// High-frequency permittivity ε∞.
    pub eps_inf: f64,
    /// Δε / ln(ω₂/ω₁).
    pub k: f64,
}

impl DjordjevicSarkar {
    /// The model through εr and tan δ at `f_ref` (Hz).
    pub fn from_reference(er: f64, tan_d: f64, f_ref: f64) -> DjordjevicSarkar {
        let (a, b) = log_term(f_ref);
        let k = tan_d * er / b;
        DjordjevicSarkar { eps_inf: er - k * a, k }
    }

    /// Complex relative permittivity ε′ − jε″ at `f` (Hz).
    pub fn eps(&self, f: f64) -> C64 {
        let (a, b) = log_term(f);
        C64::new(self.eps_inf + self.k * a, -self.k * b)
    }

    /// Static permittivity ε∞ + Δε.
    pub fn eps_static(&self) -> f64 {
        self.eps_inf + self.k * (F2 / F1).ln()
    }
}

/// A dielectric's permittivity and loss: constant (εr and tan δ the same at every frequency, the
/// D52 model), or Djordjevic–Sarkar through εr and tan δ at a reference frequency.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dielectric {
    /// Relative permittivity (at `ref_freq` when set).
    pub er: f64,
    /// Loss tangent (at `ref_freq` when set).
    pub tan_d: f64,
    /// Reference frequency (Hz) of the Djordjevic–Sarkar model; `None`: frequency-independent.
    pub ref_freq: Option<f64>,
}

impl Dielectric {
    /// A frequency-independent dielectric.
    pub fn constant(er: f64, tan_d: f64) -> Dielectric {
        Dielectric { er, tan_d, ref_freq: None }
    }

    /// The Djordjevic–Sarkar model of a dispersive dielectric, or of a constant one anchored at
    /// `anchor` (Hz): the causal model that matches it there.
    pub fn model(&self, anchor: f64) -> DjordjevicSarkar {
        DjordjevicSarkar::from_reference(self.er, self.tan_d, self.ref_freq.unwrap_or(anchor))
    }

    /// Complex relative permittivity ε′ − jε″ at `f` (Hz).
    pub fn eps(&self, f: f64) -> C64 {
        match self.ref_freq {
            None => C64::new(self.er, -self.er * self.tan_d),
            Some(r) => DjordjevicSarkar::from_reference(self.er, self.tan_d, r).eps(f),
        }
    }

    /// εr and tan δ at `f` (Hz).
    pub fn at(&self, f: f64) -> (f64, f64) {
        let e = self.eps(f);
        (e.re, -e.im / e.re)
    }

    /// εr(f) / εr and tan δ(f): the factor on the permittivity and the loss tangent at `f`.
    pub fn ratio(&self, f: f64) -> (f64, f64) {
        let (er, tan) = self.at(f);
        (er / self.er, tan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_point_is_exact() {
        let d = Dielectric { er: 4.3, tan_d: 0.02, ref_freq: Some(1e9) };
        let (er, tan) = d.at(1e9);
        assert!((er - 4.3).abs() < 1e-12 && (tan - 0.02).abs() < 1e-14, "{er} {tan}");
        let c = Dielectric::constant(4.3, 0.02);
        assert_eq!(c.at(7e9), (4.3, 0.02));
        assert_eq!(c.ratio(3e9), (1.0, 0.02));
    }

    #[test]
    fn flat_loss_tangent_and_falling_permittivity() {
        // Between the corners tan δ is nearly flat and εr falls by about (2/π)·εr·tan δ·ln 10
        // per decade (the Kramers–Kronig partner of a constant ε″).
        let d = Dielectric { er: 4.0, tan_d: 0.02, ref_freq: Some(1e9) };
        let (e1, t1) = d.at(1e8);
        let (e2, t2) = d.at(1e10);
        assert!(e1 > 4.0 && e2 < 4.0);
        let slope = (e1 - e2) / 2.0;
        let want = 2.0 / PI * 4.0 * 0.02 * 10f64.ln();
        assert!((slope - want).abs() < 0.02 * want, "{slope} vs {want}");
        assert!((t1 - 0.02).abs() < 0.001 && (t2 - 0.02).abs() < 0.001, "{t1} {t2}");
        // Causal: ε″ > 0 everywhere, ε′ decreasing, the static value above all.
        let m = d.model(0.0);
        let mut last = f64::INFINITY;
        for k in 0..60 {
            let f = 10f64.powf(k as f64 * 0.25);
            let e = m.eps(f);
            assert!(e.im < 0.0 && e.re <= last, "{f}: {e:?}");
            last = e.re;
        }
        assert!(m.eps_static() > m.eps(1.0).re - 1e-9);
    }

    #[test]
    fn sum_of_debye_relaxations() {
        // The model is ε∞ plus Debye terms k/(1 + jω/Ω) spread uniformly in ln Ω over the
        // corners: a midpoint sum over that distribution reproduces it (sign and scale).
        let m = DjordjevicSarkar::from_reference(4.0, 0.02, 1e9);
        let (l1, l2) = ((2.0 * PI * F1).ln(), (2.0 * PI * F2).ln());
        let n = 20_000;
        let h = (l2 - l1) / n as f64;
        for f in [1e6, 1e9, 3e10] {
            let w = 2.0 * PI * f;
            let mut s = C64::new(m.eps_inf, 0.0);
            for i in 0..n {
                let big = (l1 + (i as f64 + 0.5) * h).exp();
                s = s + C64::new(m.k * h, 0.0) / C64::new(1.0, w / big);
            }
            assert!((s - m.eps(f)).abs() < 1e-6, "{f}: {s:?} vs {:?}", m.eps(f));
        }
    }
}
