//! Physical units: lengths in integer nanometers, angles in integer millidegrees.
//!
//! Every textual length must carry a unit (`"0.2mm"`, `"8mil"`, `"0.1in"`). Bare numbers are rejected
//! to rule out mm/mil confusion. Stored data and serialized files always use millimeters, which are
//! exact for any nanometer value.

use std::borrow::Cow;
use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Error returned when parsing a length or an angle.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnitError {
    /// The text does not start with a decimal number.
    #[error("`{0}` is not a number with a unit (examples: \"0.2mm\", \"8mil\", \"0.1in\")")]
    NotANumber(String),
    /// A length was given without a unit.
    #[error("`{0}` has no unit; lengths need one (examples: \"{0}mm\", \"{0}mil\")")]
    MissingUnit(String),
    /// The unit suffix is not recognized.
    #[error("unknown unit `{unit}` in `{input}` (known: {known})")]
    UnknownUnit {
        /// The full input.
        input: String,
        /// The unrecognized suffix.
        unit: String,
        /// The accepted units, for the message.
        known: &'static str,
    },
    /// The value does not fit the internal representation.
    #[error("`{0}` is out of range")]
    OutOfRange(String),
}

/// A length or coordinate, in nanometers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nm(pub i64);

/// Units accepted for lengths, and usable for display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LengthUnit {
    /// Nanometer.
    Nm,
    /// Micrometer.
    Um,
    /// Millimeter.
    #[default]
    Mm,
    /// Centimeter.
    Cm,
    /// Meter.
    M,
    /// Thousandth of an inch.
    Mil,
    /// Inch.
    In,
}

const KNOWN_LENGTH_UNITS: &str = "nm, um, mm, cm, m, mil (thou), in";

impl LengthUnit {
    /// Nanometers per unit.
    pub const fn nm_per_unit(self) -> i64 {
        match self {
            LengthUnit::Nm => 1,
            LengthUnit::Um => 1_000,
            LengthUnit::Mm => 1_000_000,
            LengthUnit::Cm => 10_000_000,
            LengthUnit::M => 1_000_000_000,
            LengthUnit::Mil => 25_400,
            LengthUnit::In => 25_400_000,
        }
    }

    /// Canonical suffix.
    pub const fn suffix(self) -> &'static str {
        match self {
            LengthUnit::Nm => "nm",
            LengthUnit::Um => "um",
            LengthUnit::Mm => "mm",
            LengthUnit::Cm => "cm",
            LengthUnit::M => "m",
            LengthUnit::Mil => "mil",
            LengthUnit::In => "in",
        }
    }

    fn from_suffix(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "nm" => LengthUnit::Nm,
            "um" | "µm" | "μm" => LengthUnit::Um,
            "mm" => LengthUnit::Mm,
            "cm" => LengthUnit::Cm,
            "m" => LengthUnit::M,
            "mil" | "mils" | "thou" => LengthUnit::Mil,
            "in" | "inch" | "inches" | "\"" => LengthUnit::In,
            _ => return None,
        })
    }
}

impl Nm {
    /// Zero length.
    pub const ZERO: Nm = Nm(0);

    /// From whole nanometers.
    pub const fn new(nm: i64) -> Self {
        Nm(nm)
    }

    /// From whole micrometers.
    pub const fn from_um(um: i64) -> Self {
        Nm(um * 1_000)
    }

    /// From whole millimeters.
    pub const fn from_mm(mm: i64) -> Self {
        Nm(mm * 1_000_000)
    }

    /// From whole mils.
    pub const fn from_mil(mil: i64) -> Self {
        Nm(mil * 25_400)
    }

    /// Raw value in nanometers.
    pub const fn nm(self) -> i64 {
        self.0
    }

    /// Approximate value in the given unit, for display and float-based algorithms only.
    pub fn to_f64(self, unit: LengthUnit) -> f64 {
        self.0 as f64 / unit.nm_per_unit() as f64
    }

    /// Absolute value.
    pub const fn abs(self) -> Nm {
        Nm(self.0.abs())
    }

    /// Parses a length with a mandatory unit. Values finer than 1 nm are rounded to the nearest
    /// nanometer, half away from zero.
    pub fn parse(s: &str) -> Result<Nm, UnitError> {
        let (num, rest) = parse_decimal(s)?;
        let unit = rest.trim();
        if unit.is_empty() {
            return Err(UnitError::MissingUnit(s.trim().to_string()));
        }
        let unit = LengthUnit::from_suffix(unit).ok_or_else(|| UnitError::UnknownUnit {
            input: s.trim().to_string(),
            unit: unit.to_string(),
            known: KNOWN_LENGTH_UNITS,
        })?;
        let v = num
            .scale_rounded(unit.nm_per_unit() as i128)
            .ok_or_else(|| UnitError::OutOfRange(s.trim().to_string()))?;
        i64::try_from(v)
            .map(Nm)
            .map_err(|_| UnitError::OutOfRange(s.trim().to_string()))
    }

    /// Formats in the given unit. Millimeters and smaller metric units are exact; mil and inch
    /// are rounded to 1/10000 of the unit.
    pub fn display_in(self, unit: LengthUnit) -> String {
        let per = unit.nm_per_unit();
        let frac_digits = match unit {
            LengthUnit::Nm => 0,
            LengthUnit::Um => 3,
            LengthUnit::Mm => 6,
            LengthUnit::Cm => 7,
            LengthUnit::M => 9,
            LengthUnit::Mil | LengthUnit::In => 4,
        };
        let scale = 10i128.pow(frac_digits);
        // value in units * scale, rounded half away from zero
        let num = self.0 as i128 * scale;
        let den = per as i128;
        let q = div_round(num, den);
        format!("{}{}", format_fixed(q, frac_digits), unit.suffix())
    }
}

/// Shorthand for whole millimeters: `mm(50)`.
pub const fn mm(v: i64) -> Nm {
    Nm::from_mm(v)
}

/// Shorthand for whole mils: `mil(8)`.
pub const fn mil(v: i64) -> Nm {
    Nm::from_mil(v)
}

impl fmt::Display for Nm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_in(LengthUnit::Mm))
    }
}

impl FromStr for Nm {
    type Err = UnitError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Nm::parse(s)
    }
}

impl Serialize for Nm {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Nm {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = Cow::<str>::deserialize(d)?;
        Nm::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Nm {
    fn schema_name() -> Cow<'static, str> {
        "Length".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Length with a mandatory unit: nm, um, mm, cm, m, mil, in. Examples: \"0.2mm\", \"8mil\", \"0.1in\".",
            "pattern": "^\\s*[-+]?([0-9]+\\.?[0-9]*|\\.[0-9]+)\\s*(nm|um|µm|mm|cm|m|mil|mils|thou|in|inch)\\s*$"
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

impl Add for Nm {
    type Output = Nm;
    fn add(self, o: Nm) -> Nm {
        Nm(self.0 + o.0)
    }
}

impl Sub for Nm {
    type Output = Nm;
    fn sub(self, o: Nm) -> Nm {
        Nm(self.0 - o.0)
    }
}

impl AddAssign for Nm {
    fn add_assign(&mut self, o: Nm) {
        self.0 += o.0;
    }
}

impl SubAssign for Nm {
    fn sub_assign(&mut self, o: Nm) {
        self.0 -= o.0;
    }
}

impl Neg for Nm {
    type Output = Nm;
    fn neg(self) -> Nm {
        Nm(-self.0)
    }
}

impl Mul<i64> for Nm {
    type Output = Nm;
    fn mul(self, k: i64) -> Nm {
        Nm(self.0 * k)
    }
}

impl Div<i64> for Nm {
    type Output = Nm;
    fn div(self, k: i64) -> Nm {
        Nm(self.0 / k)
    }
}

/// An angle in millidegrees (0.001°). Not normalized: arc sweeps may be negative or exceed a turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Angle(pub i32);

impl Angle {
    /// Zero.
    pub const ZERO: Angle = Angle(0);
    /// 90°.
    pub const DEG_90: Angle = Angle(90_000);
    /// 180°.
    pub const DEG_180: Angle = Angle(180_000);
    /// 270°.
    pub const DEG_270: Angle = Angle(270_000);
    /// One full turn.
    pub const FULL_TURN: Angle = Angle(360_000);

    /// From whole degrees.
    pub const fn from_deg(deg: i32) -> Self {
        Angle(deg * 1000)
    }

    /// Raw value in millidegrees.
    pub const fn millideg(self) -> i32 {
        self.0
    }

    /// Value in degrees as a float.
    pub fn to_deg_f64(self) -> f64 {
        self.0 as f64 / 1000.0
    }

    /// Value in radians as a float.
    pub fn to_rad_f64(self) -> f64 {
        self.to_deg_f64().to_radians()
    }

    /// Same direction, in `[0°, 360°)`.
    pub const fn normalized(self) -> Angle {
        Angle(self.0.rem_euclid(360_000))
    }

    /// Number of quarter turns if this is a multiple of 90°, in `0..4`.
    pub const fn quarter_turns(self) -> Option<u8> {
        let n = self.normalized().0;
        if n % 90_000 == 0 {
            Some((n / 90_000) as u8)
        } else {
            None
        }
    }

    /// Parses an angle. A bare number is in degrees; `deg` and `°` suffixes are accepted.
    /// Precision finer than 0.001° is rounded.
    pub fn parse(s: &str) -> Result<Angle, UnitError> {
        let (num, rest) = parse_decimal(s)?;
        match rest.trim().to_ascii_lowercase().as_str() {
            "" | "deg" | "°" | "degree" | "degrees" => {}
            other => {
                return Err(UnitError::UnknownUnit {
                    input: s.trim().to_string(),
                    unit: other.to_string(),
                    known: "deg (or no unit, meaning degrees)",
                });
            }
        }
        let v = num
            .scale_rounded(1000)
            .ok_or_else(|| UnitError::OutOfRange(s.trim().into()))?;
        i32::try_from(v)
            .map(Angle)
            .map_err(|_| UnitError::OutOfRange(s.trim().into()))
    }
}

impl fmt::Display for Angle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}deg", format_fixed(self.0 as i128, 3))
    }
}

impl FromStr for Angle {
    type Err = UnitError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Angle::parse(s)
    }
}

impl Serialize for Angle {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Angle {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw<'a> {
            Str(Cow<'a, str>),
            Num(f64),
        }
        match Raw::deserialize(d)? {
            Raw::Str(s) => Angle::parse(&s).map_err(serde::de::Error::custom),
            Raw::Num(n) => Angle::parse(&n.to_string()).map_err(serde::de::Error::custom),
        }
    }
}

impl JsonSchema for Angle {
    fn schema_name() -> Cow<'static, str> {
        "Angle".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": ["string", "number"],
            "description": "Angle in degrees, counter-clockwise. Examples: 90, \"45.5deg\"."
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

impl Add for Angle {
    type Output = Angle;
    fn add(self, o: Angle) -> Angle {
        Angle(self.0 + o.0)
    }
}

impl Sub for Angle {
    type Output = Angle;
    fn sub(self, o: Angle) -> Angle {
        Angle(self.0 - o.0)
    }
}

impl Neg for Angle {
    type Output = Angle;
    fn neg(self) -> Angle {
        Angle(-self.0)
    }
}

/// A parsed decimal: `mantissa / 10^frac_digits`.
struct Decimal {
    mantissa: i128,
    frac_digits: u32,
}

impl Decimal {
    /// `self * factor`, rounded half away from zero to an integer.
    fn scale_rounded(&self, factor: i128) -> Option<i128> {
        let num = self.mantissa.checked_mul(factor)?;
        let den = 10i128.checked_pow(self.frac_digits)?;
        Some(div_round(num, den))
    }
}

/// Splits `s` into a leading decimal number and the remaining suffix.
fn parse_decimal(s: &str) -> Result<(Decimal, &str), UnitError> {
    let t = s.trim();
    let bytes = t.as_bytes();
    let mut i = 0;
    let neg = match bytes.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    let mut mantissa: i128 = 0;
    let mut frac_digits = 0u32;
    let mut digits = 0u32;
    let mut seen_dot = false;
    while i < bytes.len() {
        match bytes[i] {
            c @ b'0'..=b'9' => {
                // Ignore digits beyond what i128 can hold; 30 significant digits is plenty.
                if digits < 30 {
                    mantissa = mantissa * 10 + (c - b'0') as i128;
                    digits += 1;
                    if seen_dot {
                        frac_digits += 1;
                    }
                } else if !seen_dot {
                    return Err(UnitError::OutOfRange(t.to_string()));
                }
            }
            b'.' if !seen_dot => seen_dot = true,
            _ => break,
        }
        i += 1;
    }
    if digits == 0 {
        return Err(UnitError::NotANumber(t.to_string()));
    }
    if neg {
        mantissa = -mantissa;
    }
    Ok((Decimal { mantissa, frac_digits }, &t[i..]))
}

/// Integer division rounding half away from zero.
fn div_round(num: i128, den: i128) -> i128 {
    let q = num / den;
    let r = num % den;
    if 2 * r.abs() >= den.abs() {
        q + num.signum() * den.signum()
    } else {
        q
    }
}

/// Formats `q / 10^frac_digits` with trailing fractional zeros trimmed.
fn format_fixed(q: i128, frac_digits: u32) -> String {
    let sign = if q < 0 { "-" } else { "" };
    let a = q.unsigned_abs();
    let scale = 10u128.pow(frac_digits);
    let int = a / scale;
    let frac = a % scale;
    if frac == 0 {
        format!("{sign}{int}")
    } else {
        let s = format!("{frac:0width$}", width = frac_digits as usize);
        format!("{sign}{int}.{}", s.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_units() {
        assert_eq!(Nm::parse("0.2mm"), Ok(Nm(200_000)));
        assert_eq!(Nm::parse("8mil"), Ok(Nm(203_200)));
        assert_eq!(Nm::parse("0.1in"), Ok(Nm(2_540_000)));
        assert_eq!(Nm::parse(" -1.5 mm "), Ok(Nm(-1_500_000)));
        assert_eq!(Nm::parse("10um"), Ok(Nm(10_000)));
        assert_eq!(Nm::parse("10µm"), Ok(Nm(10_000)));
        assert_eq!(Nm::parse(".5mm"), Ok(Nm(500_000)));
        assert_eq!(Nm::parse("1m"), Ok(Nm(1_000_000_000)));
        assert_eq!(Nm::parse("0.1mil"), Ok(Nm(2_540)));
    }

    #[test]
    fn rounds_below_one_nm() {
        assert_eq!(Nm::parse("0.0000005mm"), Ok(Nm(1)));
        assert_eq!(Nm::parse("0.0000004mm"), Ok(Nm(0)));
        assert_eq!(Nm::parse("-0.0000005mm"), Ok(Nm(-1)));
    }

    #[test]
    fn rejects_bad_lengths() {
        assert!(matches!(Nm::parse("12"), Err(UnitError::MissingUnit(_))));
        assert!(matches!(Nm::parse("12ft"), Err(UnitError::UnknownUnit { .. })));
        assert!(matches!(Nm::parse("mm"), Err(UnitError::NotANumber(_))));
        assert!(matches!(Nm::parse(""), Err(UnitError::NotANumber(_))));
        assert!(matches!(Nm::parse("1e5mm"), Err(UnitError::UnknownUnit { .. })));
        assert!(matches!(Nm::parse("99999999999999mm"), Err(UnitError::OutOfRange(_))));
    }

    #[test]
    fn displays_lengths() {
        assert_eq!(Nm(12_700_000).to_string(), "12.7mm");
        assert_eq!(Nm(0).to_string(), "0mm");
        assert_eq!(Nm(-500).to_string(), "-0.0005mm");
        assert_eq!(Nm(1).to_string(), "0.000001mm");
        assert_eq!(Nm(203_200).display_in(LengthUnit::Mil), "8mil");
        assert_eq!(Nm(2_540_000).display_in(LengthUnit::In), "0.1in");
    }

    #[test]
    fn angles() {
        assert_eq!(Angle::parse("90"), Ok(Angle(90_000)));
        assert_eq!(Angle::parse("45.5deg"), Ok(Angle(45_500)));
        assert_eq!(Angle::parse("-90°"), Ok(Angle(-90_000)));
        assert_eq!(Angle(-90_000).normalized(), Angle(270_000));
        assert_eq!(Angle(450_000).quarter_turns(), Some(1));
        assert_eq!(Angle(45_000).quarter_turns(), None);
        assert_eq!(Angle(45_500).to_string(), "45.5deg");
        assert!(Angle::parse("1rad").is_err());
        let a: Angle = serde_json::from_str("90").unwrap();
        assert_eq!(a, Angle::DEG_90);
    }

    #[test]
    fn serde_roundtrip() {
        let v = serde_json::to_string(&Nm(1_234_567)).unwrap();
        assert_eq!(v, "\"1.234567mm\"");
        let back: Nm = serde_json::from_str(&v).unwrap();
        assert_eq!(back, Nm(1_234_567));
        assert!(serde_json::from_str::<Nm>("\"5\"").is_err());
    }

    proptest! {
        #[test]
        fn display_parse_roundtrip(v in any::<i64>()) {
            let n = Nm(v);
            prop_assert_eq!(Nm::parse(&n.to_string()), Ok(n));
        }

        #[test]
        fn angle_roundtrip(v in any::<i32>()) {
            let a = Angle(v);
            prop_assert_eq!(Angle::parse(&a.to_string()), Ok(a));
        }
    }
}
