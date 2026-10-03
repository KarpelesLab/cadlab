//! Search queries: keywords plus parameter filters (`current_out >= 500mA`).

use std::cmp::Ordering;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Candidate, Lifecycle, Money};
use crate::model::part::{Category, ParamValue};
use crate::value::ValueError;

/// Comparison of a parameter filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Equal (for a range parameter: the value is inside the range).
    Eq,
    /// At least.
    Ge,
    /// At most.
    Le,
    /// Greater than.
    Gt,
    /// Less than.
    Lt,
}

/// A parameter filter: `current_out >= 500mA`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamFilter {
    /// Parameter key.
    pub key: String,
    /// Comparison.
    pub op: Op,
    /// Value to compare with.
    pub value: ParamValue,
}

impl ParamFilter {
    /// Parses `key` and `">=500mA"`, `"<=1.2V"`, `"3.3V"`, `"X7R"`.
    pub fn parse(key: &str, expr: &str) -> Result<ParamFilter, ValueError> {
        let e = expr.trim();
        let (op, rest) = [
            (">=", Op::Ge),
            ("≥", Op::Ge),
            ("<=", Op::Le),
            ("≤", Op::Le),
            (">", Op::Gt),
            ("<", Op::Lt),
            ("=", Op::Eq),
        ]
        .iter()
        .find_map(|(p, op)| e.strip_prefix(p).map(|r| (*op, r)))
        .unwrap_or((Op::Eq, e));
        Ok(ParamFilter {
            key: key.to_string(),
            op,
            value: ParamValue::parse(key, rest)?,
        })
    }

    /// Whether a candidate's value passes.
    pub fn accepts(&self, v: &ParamValue) -> bool {
        use ParamValue::*;
        let cmp_ok = |o: Ordering, op: Op| match op {
            Op::Eq => o == Ordering::Equal,
            Op::Ge => o != Ordering::Less,
            Op::Le => o != Ordering::Greater,
            Op::Gt => o == Ordering::Greater,
            Op::Lt => o == Ordering::Less,
        };
        match (v, &self.value) {
            (Quantity(a), Quantity(b)) => a.unit == b.unit && cmp_ok(a.cmp_value(b), self.op),
            (Range { min, max }, Quantity(b)) => {
                if min.unit != b.unit {
                    return false;
                }
                match self.op {
                    Op::Eq => min.cmp_value(b) != Ordering::Greater && max.cmp_value(b) != Ordering::Less,
                    Op::Ge | Op::Gt => cmp_ok(max.cmp_value(b), self.op),
                    Op::Le | Op::Lt => cmp_ok(min.cmp_value(b), self.op),
                }
            }
            // A required range must fit inside the candidate's range.
            (Range { min, max }, Range { min: lo, max: hi }) => {
                min.unit == lo.unit && min.cmp_value(lo) != Ordering::Greater && max.cmp_value(hi) != Ordering::Less
            }
            (Text(a), Text(b)) => self.op == Op::Eq && a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }
}

/// Normalizes package names for comparison: `SOT-23-5`, `sot23-5` and `SOT 23 5` are equal.
pub fn package_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// A search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    /// Keywords; every one must appear in the MPN, manufacturer, description, package or SKU.
    pub text: String,
    /// Category.
    pub category: Option<Category>,
    /// Package (normalized comparison).
    pub package: Option<String>,
    /// Parameter filters.
    pub filters: Vec<ParamFilter>,
    /// Quantity needed (for stock and price).
    pub quantity: u64,
    /// Only parts with at least `quantity` in stock.
    pub in_stock: bool,
    /// Maximum unit price at `quantity`.
    pub max_price: Option<Money>,
    /// Include obsolete / last-time-buy parts.
    pub include_obsolete: bool,
    /// Maximum results.
    pub limit: usize,
}

impl Default for SearchQuery {
    fn default() -> Self {
        SearchQuery {
            text: String::new(),
            category: None,
            package: None,
            filters: Vec::new(),
            quantity: 1,
            in_stock: false,
            max_price: None,
            include_obsolete: false,
            limit: 10,
        }
    }
}

impl SearchQuery {
    /// Whether a candidate satisfies every criterion.
    pub fn matches(&self, c: &Candidate) -> bool {
        let hay = format!(
            "{} {} {} {} {} {}",
            c.mpn,
            c.manufacturer.as_deref().unwrap_or(""),
            c.description,
            c.package.as_deref().unwrap_or(""),
            c.sku,
            c.category.map(|k| k.label()).unwrap_or("")
        )
        .to_lowercase();
        let pkg_hay = c.package.as_deref().map(package_key).unwrap_or_default();
        for t in self.text.split_whitespace() {
            let t = t.to_lowercase();
            if !hay.contains(&t) && !(pkg_hay.len() > 1 && pkg_hay == package_key(&t)) {
                return false;
            }
        }
        if let (Some(want), Some(have)) = (self.category, c.category)
            && want != have
        {
            return false;
        }
        if let Some(p) = &self.package
            && c.package.as_deref().map(package_key) != Some(package_key(p))
        {
            return false;
        }
        for f in &self.filters {
            match c.params.get(&f.key) {
                Some(v) if f.accepts(v) => {}
                _ => return false,
            }
        }
        if self.in_stock && c.stock < self.quantity.max(1) {
            return false;
        }
        if let Some(max) = &self.max_price {
            match c.unit_price(self.quantity) {
                Some(p) if p.currency == max.currency && p.micros <= max.micros => {}
                _ => return false,
            }
        }
        if !self.include_obsolete && matches!(c.lifecycle, Lifecycle::Obsolete | Lifecycle::LastTimeBuy) {
            return false;
        }
        true
    }

    /// Sorts best first: in stock, active, cheapest at the quantity, most stock.
    pub fn rank(&self, cands: &mut [Candidate]) {
        let q = self.quantity.max(1);
        cands.sort_by(|a, b| {
            let key = |c: &Candidate| {
                (
                    c.stock < q,
                    c.lifecycle != Lifecycle::Active,
                    c.unit_price(q)
                        .map_or(i64::MAX, |p| p.micros.saturating_mul(c.order_qty(q) as i64)),
                    std::cmp::Reverse(c.stock),
                )
            };
            key(a)
                .cmp(&key(b))
                .then_with(|| a.mpn.cmp(&b.mpn))
                .then_with(|| a.provider.cmp(&b.provider))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pv(k: &str, v: &str) -> ParamValue {
        ParamValue::parse(k, v).unwrap()
    }

    #[test]
    fn filters() {
        let f = ParamFilter::parse("current_out", ">=500mA").unwrap();
        assert!(f.accepts(&pv("current_out", "600mA")));
        assert!(f.accepts(&pv("current_out", "0.5A")));
        assert!(!f.accepts(&pv("current_out", "300mA")));
        let f = ParamFilter::parse("voltage_out", "3.3V").unwrap();
        assert!(f.accepts(&pv("voltage_out", "3V3")));
        assert!(!f.accepts(&pv("voltage_out", "3.0V")));
        // Ranges: 5 V input inside 2.5..6 V.
        let f = ParamFilter::parse("voltage_in", "5V").unwrap();
        assert!(f.accepts(&pv("voltage_in", "2.5V..6V")));
        assert!(!f.accepts(&pv("voltage_in", "2.5V..4.5V")));
        let f = ParamFilter::parse("voltage_in", ">=12V").unwrap();
        assert!(f.accepts(&pv("voltage_in", "4V..16V")));
        let f = ParamFilter::parse("temperature", "-40..85").unwrap();
        assert!(f.accepts(&pv("temperature", "-40..125°C")));
        assert!(!f.accepts(&pv("temperature", "0..70°C")));
        let f = ParamFilter::parse("dielectric", "x7r").unwrap();
        assert!(f.accepts(&pv("dielectric", "X7R")));
        assert!(ParamFilter::parse("resistance", ">=10uF").is_err());
    }

    #[test]
    fn packages_normalize() {
        assert_eq!(package_key("SOT-23-5"), package_key("sot23 5"));
    }
}
