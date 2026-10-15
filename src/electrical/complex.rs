//! The few complex-number operations the loss calculations need (no dependency for them).

use std::f64::consts::{LN_2, PI};
use std::ops::{Add, Div, Mul, Neg, Sub};

/// A complex number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct C64 {
    /// Real part.
    pub re: f64,
    /// Imaginary part.
    pub im: f64,
}

impl C64 {
    /// `re + i·im`.
    pub const fn new(re: f64, im: f64) -> Self {
        C64 { re, im }
    }

    /// Complex conjugate.
    pub fn conj(self) -> Self {
        C64::new(self.re, -self.im)
    }

    /// Modulus.
    pub fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }

    /// Principal logarithm (imaginary part in (−π, π]).
    pub fn ln(self) -> Self {
        C64::new(self.abs().ln(), self.im.atan2(self.re))
    }

    /// Exponential.
    pub fn exp(self) -> Self {
        let m = self.re.exp();
        C64::new(m * self.im.cos(), m * self.im.sin())
    }

    /// Principal square root (non-negative real part).
    pub fn sqrt(self) -> Self {
        let r = self.abs();
        if r == 0.0 {
            return C64::new(0.0, 0.0);
        }
        let re = ((r + self.re) / 2.0).sqrt();
        let im = ((r - self.re) / 2.0).sqrt();
        C64::new(re, if self.im < 0.0 { -im } else { im })
    }

    /// Principal logarithm of sinh(z), computed without overflow for large |Re z|.
    pub fn ln_sinh(self) -> Self {
        let one = C64::new(1.0, 0.0);
        let r = if self.re >= 0.0 {
            // sinh z = e^z (1 − e^(−2z)) / 2
            self + (one - (C64::new(-2.0, 0.0) * self).exp()).ln() - C64::new(LN_2, 0.0)
        } else {
            // sinh z = −e^(−z) (1 − e^(2z)) / 2
            -self + (one - (C64::new(2.0, 0.0) * self).exp()).ln() + C64::new(-LN_2, PI)
        };
        // Back to the principal branch.
        let im = (r.im + PI).rem_euclid(2.0 * PI) - PI;
        C64::new(r.re, if im == -PI { PI } else { im })
    }
}

impl Add for C64 {
    type Output = C64;
    fn add(self, o: C64) -> C64 {
        C64::new(self.re + o.re, self.im + o.im)
    }
}

impl Sub for C64 {
    type Output = C64;
    fn sub(self, o: C64) -> C64 {
        C64::new(self.re - o.re, self.im - o.im)
    }
}

impl Mul for C64 {
    type Output = C64;
    fn mul(self, o: C64) -> C64 {
        C64::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}

impl Div for C64 {
    type Output = C64;
    fn div(self, o: C64) -> C64 {
        let d = o.re * o.re + o.im * o.im;
        C64::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }
}

impl Neg for C64 {
    type Output = C64;
    fn neg(self) -> C64 {
        C64::new(-self.re, -self.im)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ln_sinh_matches_direct() {
        for (re, im) in [(0.3, 0.2), (-0.7, 1.1), (2.0, 0.05), (-3.0, 1.5), (0.01, 0.4)] {
            let z = C64::new(re, im);
            let s = C64::new(re.sinh() * im.cos(), re.cosh() * im.sin());
            let a = z.ln_sinh();
            let b = s.ln();
            assert!((a.re - b.re).abs() < 1e-12 && (a.im - b.im).abs() < 1e-12, "{z:?}: {a:?} {b:?}");
        }
        let z = C64::new(400.0, 0.3);
        assert!((z.ln_sinh().re - (400.0 - LN_2)).abs() < 1e-9);
    }

    #[test]
    fn sqrt_and_div() {
        let z = C64::new(-3.0, 4.0);
        let r = z.sqrt();
        assert!((r * r - z).abs() < 1e-12 && r.re >= 0.0);
        assert!(((z / r) - r).abs() < 1e-12);
    }
}
