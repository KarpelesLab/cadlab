//! Specctra design language (DSN) and session (SES) files: the interchange format of external
//! autorouters such as freerouting. See docs/ROUTER.md, "Specctra DSN/SES".
//!
//! - [`dsn`]: the design file as data ([`dsn::Dsn`]), its writer and reader.
//! - [`export`]: a project's board as a [`dsn::Dsn`] (layers, outline, keep-outs, images with
//!   padstacks, placement, nets and classes, existing wiring).
//! - [`ses`]: session files (the routed result) and their conversion into board tracks and vias.
//! - [`sexpr`]: the S-expression reader and writer underneath.
//!
//! Everything is implemented from the published Specctra Design Language Reference (DSN) and
//! session file description (SES); no code from other tools is used (DECISIONS D7, D25). This is
//! an algorithm module: it takes the model by reference and returns data; the `export.dsn` and
//! `route.import_ses` commands write files and apply results.

pub mod dsn;
pub mod export;
pub mod ses;
pub mod sexpr;

use crate::units::{Angle, Nm};

/// A Specctra dimension unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Unit {
    /// Inch.
    Inch,
    /// Mil (0.001 inch).
    Mil,
    /// Centimeter.
    Cm,
    /// Millimeter.
    Mm,
    /// Micrometer.
    #[default]
    Um,
}

impl Unit {
    /// Nanometers per unit (exact).
    pub fn nm(self) -> i64 {
        match self {
            Unit::Inch => 25_400_000,
            Unit::Mil => 25_400,
            Unit::Cm => 10_000_000,
            Unit::Mm => 1_000_000,
            Unit::Um => 1_000,
        }
    }

    /// The keyword in files.
    pub fn keyword(self) -> &'static str {
        match self {
            Unit::Inch => "inch",
            Unit::Mil => "mil",
            Unit::Cm => "cm",
            Unit::Mm => "mm",
            Unit::Um => "um",
        }
    }

    /// Parses a unit keyword (case-insensitive).
    pub fn parse(s: &str) -> Option<Unit> {
        Some(match s.to_ascii_lowercase().as_str() {
            "inch" => Unit::Inch,
            "mil" => Unit::Mil,
            "cm" => Unit::Cm,
            "mm" => Unit::Mm,
            "um" => Unit::Um,
            _ => return None,
        })
    }
}

/// How numbers in a file map to nanometers: `value × unit / divisor`. Design files give
/// coordinates in their unit (divisor 1); session files in steps of the resolution (divisor =
/// resolution).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale {
    /// Unit of the numbers.
    pub unit: Unit,
    /// Steps per unit.
    pub divisor: u32,
}

impl Scale {
    /// Converts a decimal number to nanometers, exactly when the value is a whole number of
    /// nanometers and rounded half away from zero otherwise. `None` if it is not a number.
    pub fn to_nm(self, s: &str) -> Option<Nm> {
        let (neg, digits) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
        if int.is_empty() && frac.is_empty()
            || !int.bytes().all(|c| c.is_ascii_digit())
            || !frac.bytes().all(|c| c.is_ascii_digit())
            || frac.len() > 18
        {
            return None;
        }
        let mantissa: i128 = format!("{int}{frac}").parse().ok()?;
        let num = mantissa.checked_mul(self.unit.nm() as i128)?;
        let den = 10i128.pow(frac.len() as u32) * self.divisor.max(1) as i128;
        let q = (num + den / 2) / den;
        let v = i64::try_from(q).ok()?;
        Some(Nm(if neg { -v } else { v }))
    }

    /// Writes a length in this scale, exactly (as many decimals as needed). Only exact for units
    /// whose nanometer count is a power of ten (um, mm, cm) with divisor 1.
    pub fn fmt(self, n: Nm) -> String {
        let den = self.unit.nm() * self.divisor.max(1) as i64;
        decimal(n.0, den)
    }
}

/// `v / den` as a decimal string; `den` must be a power of ten.
fn decimal(v: i64, den: i64) -> String {
    let (sign, a) = if v < 0 { ("-", v.unsigned_abs()) } else { ("", v as u64) };
    let den = den as u64;
    let (i, f) = (a / den, a % den);
    if f == 0 {
        return format!("{sign}{i}");
    }
    let width = den.ilog10() as usize;
    let frac = format!("{f:0width$}");
    format!("{sign}{i}.{}", frac.trim_end_matches('0'))
}

/// Degrees with up to three decimals, in `[0, 360)`.
pub fn fmt_angle(a: Angle) -> String {
    decimal(a.normalized().0 as i64, 1000)
}

/// Parses decimal degrees, rounded to 0.001°.
pub fn parse_angle(s: &str) -> Option<Angle> {
    let v: f64 = s.parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    Some(Angle((v * 1000.0).round() as i32).normalized())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_lengths() {
        let um = Scale { unit: Unit::Um, divisor: 1 };
        assert_eq!(um.to_nm("1234.567"), Some(Nm(1_234_567)));
        assert_eq!(um.to_nm("-0.5"), Some(Nm(-500)));
        assert_eq!(um.to_nm("12"), Some(Nm(12_000)));
        assert_eq!(um.to_nm("1e3"), None);
        assert_eq!(um.to_nm("-"), None);
        assert_eq!(um.fmt(Nm(1_234_567)), "1234.567");
        assert_eq!(um.fmt(Nm(-500)), "-0.5");
        assert_eq!(um.fmt(Nm(3_000)), "3");
        let ses = Scale { unit: Unit::Um, divisor: 10 };
        assert_eq!(ses.to_nm("123457"), Some(Nm(12_345_700)));
        let mil = Scale { unit: Unit::Mil, divisor: 1 };
        assert_eq!(mil.to_nm("10"), Some(Nm(254_000)));
        assert_eq!(Scale { unit: Unit::Inch, divisor: 1000 }.to_nm("1"), Some(Nm(25_400)));
        // 1/3 nm rounds.
        assert_eq!(Scale { unit: Unit::Um, divisor: 3000 }.to_nm("1"), Some(Nm(0)));
        assert_eq!(Scale { unit: Unit::Um, divisor: 3000 }.to_nm("2"), Some(Nm(1)));
    }

    #[test]
    fn angles() {
        assert_eq!(fmt_angle(Angle(-90_000)), "270");
        assert_eq!(fmt_angle(Angle(12_500)), "12.5");
        assert_eq!(parse_angle("270"), Some(Angle(270_000)));
        assert_eq!(parse_angle("-90"), Some(Angle(270_000)));
        assert_eq!(parse_angle("x"), None);
    }
}
