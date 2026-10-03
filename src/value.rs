//! Electrical and physical quantities as exact decimals: `10k`, `100nF`, `4.7uF`, `3V3`, `1%`.
//!
//! A [`Quantity`] is `mantissa × 10^exp` with a [`Unit`]. It never goes through floating point,
//! so `4.7uF` stays exactly `4.7uF`. Parsing accepts SI prefixes (`p n u µ m k M G`) and RKM
//! notation (`4k7`, `2R2`, `3V3`, `4n7`), which is common on schematics and part markings.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Unit of a [`Quantity`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    /// Dimensionless (counts, ratios, or a value whose unit comes from context).
    None,
    /// Ohm (Ω).
    Ohm,
    /// Farad.
    Farad,
    /// Henry.
    Henry,
    /// Volt.
    Volt,
    /// Ampere.
    Ampere,
    /// Watt.
    Watt,
    /// Hertz.
    Hertz,
    /// Second.
    Second,
    /// Degree Celsius.
    Celsius,
    /// Percent.
    Percent,
    /// Parts per million.
    Ppm,
    /// Decibel.
    Decibel,
}

impl Unit {
    /// Symbol used when displaying.
    pub const fn symbol(self) -> &'static str {
        match self {
            Unit::None => "",
            Unit::Ohm => "Ω",
            Unit::Farad => "F",
            Unit::Henry => "H",
            Unit::Volt => "V",
            Unit::Ampere => "A",
            Unit::Watt => "W",
            Unit::Hertz => "Hz",
            Unit::Second => "s",
            Unit::Celsius => "°C",
            Unit::Percent => "%",
            Unit::Ppm => "ppm",
            Unit::Decibel => "dB",
        }
    }

    /// Whether SI prefixes are used when displaying (`°C`, `%`, `ppm`, `dB` are shown plainly).
    const fn takes_prefix(self) -> bool {
        !matches!(self, Unit::Celsius | Unit::Percent | Unit::Ppm | Unit::Decibel)
    }

    fn parse(s: &str) -> Option<Unit> {
        Some(match s {
            "" => Unit::None,
            "Ω" | "ohm" | "ohms" | "Ohm" | "R" | "r" => Unit::Ohm,
            "F" | "f" => Unit::Farad,
            "H" => Unit::Henry,
            "V" | "v" => Unit::Volt,
            "A" => Unit::Ampere,
            "W" => Unit::Watt,
            "Hz" | "hz" | "HZ" => Unit::Hertz,
            "s" => Unit::Second,
            "°C" | "C" | "degC" | "℃" => Unit::Celsius,
            "%" => Unit::Percent,
            "ppm" => Unit::Ppm,
            "dB" | "db" => Unit::Decibel,
            _ => return None,
        })
    }
}

/// Error parsing a quantity.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValueError {
    /// Not a number.
    #[error("`{0}` is not a value (examples: \"10k\", \"100nF\", \"4.7uF\", \"3V3\", \"1%\")")]
    NotAValue(String),
    /// Unknown unit or prefix.
    #[error("unknown unit `{unit}` in `{input}`")]
    UnknownUnit {
        /// The input.
        input: String,
        /// The unrecognized part.
        unit: String,
    },
    /// Too many significant digits.
    #[error("`{0}` has too many significant digits")]
    TooPrecise(String),
    /// The unit does not match what was expected.
    #[error("`{input}` is in {found:?}, expected {expected:?}")]
    WrongUnit {
        /// The input.
        input: String,
        /// Unit found.
        found: Unit,
        /// Unit expected.
        expected: Unit,
    },
}

/// An exact decimal quantity: `mantissa × 10^exp` in `unit`.
///
/// Always normalized: the mantissa has no trailing zeros (zero is `0 × 10^0`), so equal values
/// have equal representations and derived equality is value equality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Quantity {
    mantissa: i64,
    exp: i8,
    /// Unit.
    pub unit: Unit,
}

const MAX_DIGITS: usize = 18;

const PREFIXES: &[(char, i8)] = &[
    ('f', -15),
    ('p', -12),
    ('n', -9),
    ('u', -6),
    ('µ', -6),
    ('μ', -6),
    ('m', -3),
    ('k', 3),
    ('K', 3),
    ('M', 6),
    ('G', 9),
    ('T', 12),
];

fn prefix_exp(c: char) -> Option<i8> {
    PREFIXES.iter().find(|(p, _)| *p == c).map(|(_, e)| *e)
}

impl Quantity {
    /// `mantissa × 10^exp` in `unit`, normalized.
    pub fn new(mantissa: i64, exp: i8, unit: Unit) -> Self {
        let (mut m, mut e) = (mantissa, exp);
        if m == 0 {
            e = 0;
        }
        while m != 0 && m % 10 == 0 && e < i8::MAX {
            m /= 10;
            e += 1;
        }
        Quantity { mantissa: m, exp: e, unit }
    }

    /// Integer value in `unit`.
    pub fn int(v: i64, unit: Unit) -> Self {
        Self::new(v, 0, unit)
    }

    /// Mantissa (normalized).
    pub fn mantissa(&self) -> i64 {
        self.mantissa
    }

    /// Power of ten.
    pub fn exp(&self) -> i8 {
        self.exp
    }

    /// Approximate float value, for display or float-based maths only.
    pub fn to_f64(&self) -> f64 {
        self.mantissa as f64 * 10f64.powi(self.exp as i32)
    }

    /// Same value with another unit.
    pub fn with_unit(self, unit: Unit) -> Self {
        Quantity { unit, ..self }
    }

    /// Parses a quantity; the unit may be omitted (`Unit::None`).
    pub fn parse(s: &str) -> Result<Quantity, ValueError> {
        let t = s.trim();
        let err = || ValueError::NotAValue(t.to_string());
        let mut chars = t.char_indices().peekable();
        let mut neg = false;
        if let Some(&(_, c)) = chars.peek()
            && (c == '-' || c == '+')
        {
            neg = c == '-';
            chars.next();
        }
        let mut int_digits = String::new();
        let mut frac_digits = String::new();
        let mut rest_start = t.len();
        let mut rkm_exp: Option<i8> = None;
        let mut rkm_unit: Option<Unit> = None;
        let mut seen_point = false;
        while let Some(&(i, c)) = chars.peek() {
            if c.is_ascii_digit() {
                if seen_point || rkm_exp.is_some() || rkm_unit.is_some() {
                    frac_digits.push(c);
                } else {
                    int_digits.push(c);
                }
                chars.next();
                continue;
            }
            if !seen_point && rkm_exp.is_none() && rkm_unit.is_none() && !int_digits.is_empty() {
                if c == '.' {
                    seen_point = true;
                    chars.next();
                    continue;
                }
                // RKM: a prefix letter, R (ohm) or V (volt) used as the decimal point: 4k7, 2R2, 3V3.
                let next_is_digit = t[i + c.len_utf8()..].chars().next().is_some_and(|d| d.is_ascii_digit());
                if next_is_digit {
                    if let Some(e) = prefix_exp(c) {
                        rkm_exp = Some(e);
                        chars.next();
                        continue;
                    }
                    if c == 'R' || c == 'r' {
                        rkm_unit = Some(Unit::Ohm);
                        chars.next();
                        continue;
                    }
                    if c == 'V' || c == 'v' {
                        rkm_unit = Some(Unit::Volt);
                        chars.next();
                        continue;
                    }
                }
            }
            if c == '.' && int_digits.is_empty() && !seen_point {
                seen_point = true;
                chars.next();
                continue;
            }
            rest_start = i;
            break;
        }
        if int_digits.is_empty() && frac_digits.is_empty() {
            return Err(err());
        }
        let rest = t[rest_start..].trim();
        let (mut exp, mut unit) = (rkm_exp.unwrap_or(0), rkm_unit.unwrap_or(Unit::None));
        if rkm_exp.is_some() || rkm_unit.is_some() {
            // 4k7F, 2R2 (rest empty), 4n7F.
            let u = Unit::parse(rest)
                .ok_or_else(|| ValueError::UnknownUnit { input: t.to_string(), unit: rest.to_string() })?;
            if rkm_unit.is_none() {
                unit = u;
            } else if u != Unit::None && u != unit {
                return Err(ValueError::UnknownUnit { input: t.to_string(), unit: rest.to_string() });
            }
        } else if let Some(u) = Unit::parse(rest) {
            unit = u;
        } else {
            let mut rc = rest.chars();
            let first = rc.next().unwrap_or(' ');
            let after = rc.as_str();
            match (prefix_exp(first), Unit::parse(after)) {
                (Some(e), Some(u)) => {
                    exp = e;
                    unit = u;
                }
                _ => {
                    return Err(ValueError::UnknownUnit { input: t.to_string(), unit: rest.to_string() });
                }
            }
        }
        let digits = format!("{int_digits}{frac_digits}");
        let digits = digits.trim_start_matches('0');
        if digits.len() > MAX_DIGITS {
            return Err(ValueError::TooPrecise(t.to_string()));
        }
        let m: i64 = if digits.is_empty() { 0 } else { digits.parse().map_err(|_| err())? };
        let e = exp as i32 - frac_digits.len() as i32;
        let e = i8::try_from(e).map_err(|_| ValueError::TooPrecise(t.to_string()))?;
        Ok(Quantity::new(if neg { -m } else { m }, e, unit))
    }

    /// Parses, assigning `expected` when no unit is given and rejecting other units.
    pub fn parse_as(s: &str, expected: Unit) -> Result<Quantity, ValueError> {
        let q = Quantity::parse(s)?;
        match q.unit {
            Unit::None => Ok(q.with_unit(expected)),
            u if u == expected => Ok(q),
            found => Err(ValueError::WrongUnit { input: s.trim().to_string(), found, expected }),
        }
    }

    /// Engineering notation without the unit symbol: `10k`, `4.7u`, `100n`, `1`.
    pub fn display_bare(&self) -> String {
        if !self.unit.takes_prefix() {
            return format_decimal(self.mantissa, self.exp as i32);
        }
        let digits = if self.mantissa == 0 { 1 } else { self.mantissa.unsigned_abs().ilog10() as i32 + 1 };
        let order = digits - 1 + self.exp as i32; // power of ten of the leading digit
        let eng = order.div_euclid(3) * 3;
        let eng = eng.clamp(-15, 12);
        let prefix = match eng {
            -15 => "f",
            -12 => "p",
            -9 => "n",
            -6 => "u",
            -3 => "m",
            3 => "k",
            6 => "M",
            9 => "G",
            12 => "T",
            _ => "",
        };
        format!("{}{prefix}", format_decimal(self.mantissa, self.exp as i32 - eng))
    }

    /// The value in units of `10^exp` as an exact decimal string: `100nF` in µF (`exp = -6`) is
    /// `"0.1"`.
    pub fn decimal_in(&self, exp: i32) -> String {
        format_decimal(self.mantissa, self.exp as i32 - exp)
    }

    /// Compares values, ignoring units.
    pub fn cmp_value(&self, o: &Quantity) -> Ordering {
        let sign = |m: i64| m.signum();
        if sign(self.mantissa) != sign(o.mantissa) {
            return sign(self.mantissa).cmp(&sign(o.mantissa));
        }
        if self.mantissa == 0 {
            return Ordering::Equal;
        }
        // Same sign, both non-zero: compare orders of magnitude, then exact digits.
        let order = |q: &Quantity| q.mantissa.unsigned_abs().ilog10() as i32 + q.exp as i32;
        let (oa, ob) = (order(self), order(o));
        let by_mag = if oa != ob {
            oa.cmp(&ob)
        } else {
            let e = self.exp.min(o.exp) as i32;
            let a = self.mantissa.unsigned_abs() as u128 * 10u128.pow((self.exp as i32 - e) as u32);
            let b = o.mantissa.unsigned_abs() as u128 * 10u128.pow((o.exp as i32 - e) as u32);
            a.cmp(&b)
        };
        if self.mantissa < 0 { by_mag.reverse() } else { by_mag }
    }
}

/// `m × 10^e` as a plain decimal string.
fn format_decimal(m: i64, e: i32) -> String {
    let sign = if m < 0 { "-" } else { "" };
    let digits = m.unsigned_abs().to_string();
    if e >= 0 {
        return format!("{sign}{digits}{}", "0".repeat(e as usize));
    }
    let frac = (-e) as usize;
    if digits.len() > frac {
        let (i, f) = digits.split_at(digits.len() - frac);
        format!("{sign}{i}.{f}")
    } else {
        format!("{sign}0.{}{digits}", "0".repeat(frac - digits.len()))
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.display_bare(), self.unit.symbol())
    }
}

impl FromStr for Quantity {
    type Err = ValueError;
    fn from_str(s: &str) -> Result<Self, ValueError> {
        Quantity::parse(s)
    }
}

impl Serialize for Quantity {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Quantity {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = Cow::<str>::deserialize(d)?;
        Quantity::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Quantity {
    fn schema_name() -> Cow<'static, str> {
        "Quantity".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Value with optional SI prefix and unit: \"10k\", \"100nF\", \"4.7uF\", \"3V3\", \"500mA\", \"1%\"."
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> Quantity {
        Quantity::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn parses() {
        assert_eq!(q("10k"), Quantity::new(10, 3, Unit::None));
        assert_eq!(q("100nF"), Quantity::new(100, -9, Unit::Farad));
        assert_eq!(q("4.7uF"), Quantity::new(47, -7, Unit::Farad));
        assert_eq!(q("4.7µF"), q("4.7uF"));
        assert_eq!(q("4k7"), q("4.7k"));
        assert_eq!(q("2R2"), Quantity::new(22, -1, Unit::Ohm));
        assert_eq!(q("3V3"), Quantity::new(33, -1, Unit::Volt));
        assert_eq!(q("4n7F"), q("4.7nF"));
        assert_eq!(q("500mA"), Quantity::new(5, -1, Unit::Ampere));
        assert_eq!(q("1%"), Quantity::new(1, 0, Unit::Percent));
        assert_eq!(q("16 V"), Quantity::new(16, 0, Unit::Volt));
        assert_eq!(q("10MHz"), Quantity::new(1, 7, Unit::Hertz));
        assert_eq!(q("10mHz"), Quantity::new(1, -2, Unit::Hertz));
        assert_eq!(q("1M"), Quantity::new(1, 6, Unit::None));
        assert_eq!(q("10R"), Quantity::new(1, 1, Unit::Ohm));
        assert_eq!(q("10kΩ"), q("10kohm"));
        assert_eq!(q("-40°C"), Quantity::new(-4, 1, Unit::Celsius));
        assert_eq!(q("50ppm"), Quantity::new(5, 1, Unit::Ppm));
        assert_eq!(q(".1uF"), q("100nF"));
        assert_eq!(q("0"), Quantity::new(0, 0, Unit::None));
        assert!(Quantity::parse("abc").is_err());
        assert!(Quantity::parse("10 parsecs").is_err());
        assert!(Quantity::parse("").is_err());
    }

    #[test]
    fn displays_engineering() {
        assert_eq!(q("10k").to_string(), "10k");
        assert_eq!(q("10000").to_string(), "10k");
        assert_eq!(q("4k7").to_string(), "4.7k");
        assert_eq!(q("100nF").to_string(), "100nF");
        assert_eq!(q("0.1uF").to_string(), "100nF");
        assert_eq!(q("4.7uF").to_string(), "4.7uF");
        assert_eq!(q("2R2").to_string(), "2.2Ω");
        assert_eq!(q("3V3").to_string(), "3.3V");
        assert_eq!(q("500mA").to_string(), "500mA");
        assert_eq!(q("1%").to_string(), "1%");
        assert_eq!(q("0.5%").to_string(), "0.5%");
        assert_eq!(q("1000ppm").to_string(), "1000ppm");
        assert_eq!(q("0").to_string(), "0");
        assert_eq!(q("1.5M").to_string(), "1.5M");
        assert_eq!(q("12MHz").to_string(), "12MHz");
        assert_eq!(q("-40°C").to_string(), "-40°C");
    }

    #[test]
    fn roundtrips_through_display() {
        for s in ["10k", "100nF", "4.7uF", "2.2Ω", "3.3V", "500mA", "1%", "12MHz", "33pF", "0.5W", "-40°C"] {
            assert_eq!(q(&q(s).to_string()), q(s), "{s}");
        }
    }

    #[test]
    fn compares() {
        assert_eq!(q("10k").cmp_value(&q("9999")), Ordering::Greater);
        assert_eq!(q("100nF").cmp_value(&q("0.1uF")), Ordering::Equal);
        assert_eq!(q("-1").cmp_value(&q("0")), Ordering::Less);
        assert_eq!(q("-2").cmp_value(&q("-1")), Ordering::Less);
        assert_eq!(q("1p").cmp_value(&q("1T")), Ordering::Less);
    }

    #[test]
    fn parse_as_unit() {
        assert_eq!(Quantity::parse_as("10k", Unit::Ohm).unwrap(), q("10kΩ"));
        assert!(matches!(Quantity::parse_as("10uF", Unit::Ohm), Err(ValueError::WrongUnit { .. })));
    }
}
