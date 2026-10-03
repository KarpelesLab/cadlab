use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An amount of money, exact to a millionth of the currency unit. Written `"0.0123 USD"`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Money {
    /// Millionths of the currency unit.
    pub micros: i64,
    /// ISO 4217 code (`USD`, `EUR`, `CNY`).
    pub currency: String,
}

impl Money {
    /// From millionths.
    pub fn new(micros: i64, currency: impl Into<String>) -> Self {
        Money {
            micros,
            currency: currency.into().to_ascii_uppercase(),
        }
    }

    /// Parses `"0.0123 USD"`, `"USD 0.0123"` or `"$0.0123"` (dollar sign means USD).
    pub fn parse(s: &str) -> Result<Money, String> {
        let s = s.trim();
        let (num, cur) = if let Some(n) = s.strip_prefix('$') {
            (n.trim(), "USD".to_string())
        } else if let Some(n) = s.strip_prefix('€') {
            (n.trim(), "EUR".to_string())
        } else {
            let (a, b) = s
                .split_once(' ')
                .ok_or_else(|| format!("`{s}`: expected an amount and a currency, e.g. \"0.012 USD\""))?;
            if a.chars().all(|c| c.is_ascii_alphabetic()) {
                (b.trim(), a.to_string())
            } else {
                (a.trim(), b.trim().to_string())
            }
        };
        if cur.len() != 3 || !cur.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(format!("`{s}`: unknown currency `{cur}`"));
        }
        let (int, frac) = num.split_once('.').unwrap_or((num, ""));
        if int.is_empty() && frac.is_empty()
            || !int.chars().all(|c| c.is_ascii_digit())
            || !frac.chars().all(|c| c.is_ascii_digit())
        {
            return Err(format!("`{s}`: invalid amount"));
        }
        if frac.len() > 6 {
            return Err(format!("`{s}`: more than 6 decimals"));
        }
        let micros = int
            .parse::<i64>()
            .unwrap_or(0)
            .checked_mul(1_000_000)
            .ok_or("amount too large")?
            + format!("{frac:0<6}").parse::<i64>().unwrap_or(0);
        Ok(Money::new(micros, cur))
    }

    /// `self × qty`.
    pub fn times(&self, qty: u64) -> Money {
        Money::new(self.micros.saturating_mul(qty as i64), self.currency.clone())
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.micros < 0 { "-" } else { "" };
        let a = self.micros.unsigned_abs();
        let frac = format!("{:06}", a % 1_000_000);
        let frac = frac.trim_end_matches('0');
        let frac = if frac.len() < 2 {
            format!("{frac:0<2}")
        } else {
            frac.to_string()
        };
        write!(f, "{sign}{}.{frac} {}", a / 1_000_000, self.currency)
    }
}

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = Cow::<str>::deserialize(d)?;
        Money::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Money {
    fn schema_name() -> Cow<'static, str> {
        "Money".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "description": "Amount and ISO currency: \"0.0123 USD\"."})
    }

    fn inline_schema() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_display() {
        assert_eq!(Money::parse("0.0123 USD").unwrap(), Money::new(12_300, "USD"));
        assert_eq!(Money::parse("USD 1.5").unwrap(), Money::new(1_500_000, "USD"));
        assert_eq!(Money::parse("$2").unwrap(), Money::new(2_000_000, "USD"));
        assert_eq!(Money::parse("0.5 eur").unwrap().to_string(), "0.50 EUR");
        assert_eq!(Money::new(12_300, "USD").to_string(), "0.0123 USD");
        assert_eq!(Money::new(12_300, "USD").times(100).to_string(), "1.23 USD");
        assert!(Money::parse("1.2345678 USD").is_err());
        assert!(Money::parse("12").is_err());
    }
}
