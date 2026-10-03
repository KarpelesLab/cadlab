//! Sourcing: turning BOM lines into supplier queries, checking availability, costing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bom::BomRow;
use crate::model::part::Part;
use crate::supplier::query::Op;
use crate::supplier::{Candidate, Lifecycle, Money, ParamFilter, SearchQuery, Suppliers};

/// The query a generic part translates to: same category and package, same main value,
/// ratings at least as good (tolerance at most, voltage/current/power at least).
pub fn query_for(part: &Part, quantity: u64) -> SearchQuery {
    let mut q = SearchQuery { category: Some(part.category), quantity, in_stock: true, limit: 5, ..Default::default() };
    for (k, v) in &part.params.0 {
        let op = match k.as_str() {
            "package" => {
                q.package = v.text().map(String::from);
                continue;
            }
            "tolerance" => Op::Le,
            "voltage_rating" | "current_rating" | "power_rating" | "current_out" => Op::Ge,
            _ => Op::Eq,
        };
        q.filters.push(ParamFilter { key: k.clone(), op, value: v.clone() });
    }
    q
}

/// Availability of one BOM line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Enough stock of an active part.
    Ok,
    /// Found, but not enough stock anywhere.
    LowStock,
    /// Only NRND / last-time-buy / obsolete offers.
    EndOfLife,
    /// No provider knows any of the line's MPNs.
    NotFound,
    /// The line has no MPN to look for (generic part without approved candidates).
    NoMpn,
}

/// The best offer for a line, and the line's status.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LineSourcing {
    /// Part ID.
    pub part: String,
    /// Quantity needed (per-board quantity × boards).
    pub needed: u64,
    /// Status.
    pub status: Availability,
    /// Chosen offer: cheapest in-stock active one, else the best available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer: Option<Candidate>,
    /// Units bought (at least the MOQ).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_qty: Option<u64>,
    /// Unit price at that quantity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_price: Option<Money>,
    /// `unit_price × order_qty`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extended: Option<Money>,
    /// Number of offers considered.
    pub offers: usize,
}

/// MPNs that can fill a line: the part's own, then approved ones.
pub fn line_mpns(row: &BomRow) -> Vec<String> {
    let mut v: Vec<String> = row.mpn.iter().cloned().collect();
    for a in &row.approved {
        if !v.iter().any(|m| m.eq_ignore_ascii_case(&a.mpn)) {
            v.push(a.mpn.clone());
        }
    }
    v
}

/// Finds offers for a line and picks the best one. Provider errors are appended to `errors`.
pub fn source_line(
    row: &BomRow,
    boards: u64,
    suppliers: &Suppliers,
    only: &[String],
    errors: &mut Vec<String>,
) -> LineSourcing {
    let needed = row.quantity as u64 * boards;
    let mpns = line_mpns(row);
    let mut out = LineSourcing {
        part: row.part.clone(),
        needed,
        status: Availability::NoMpn,
        offer: None,
        order_qty: None,
        unit_price: None,
        extended: None,
        offers: 0,
    };
    if mpns.is_empty() {
        return out;
    }
    let mut offers: Vec<Candidate> = Vec::new();
    for m in &mpns {
        let r = suppliers.lookup(m, only);
        for e in r.errors {
            if !errors.contains(&e) {
                errors.push(e);
            }
        }
        offers.extend(r.candidates);
    }
    out.offers = offers.len();
    if offers.is_empty() {
        out.status = Availability::NotFound;
        return out;
    }
    let q = SearchQuery { quantity: needed.max(1), include_obsolete: true, ..Default::default() };
    q.rank(&mut offers);
    let best = offers.remove(0);
    out.status = if best.stock < needed.max(1) {
        Availability::LowStock
    } else if best.lifecycle != Lifecycle::Active && best.lifecycle != Lifecycle::Unknown {
        Availability::EndOfLife
    } else {
        Availability::Ok
    };
    let order = best.order_qty(needed.max(1));
    out.order_qty = Some(order);
    out.unit_price = best.unit_price(order).cloned();
    out.extended = out.unit_price.as_ref().map(|p| p.times(order));
    out.offer = Some(best);
    out
}

/// Sum of extended prices, per currency.
pub fn totals(lines: &[LineSourcing]) -> Vec<Money> {
    let mut by: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for l in lines {
        if let Some(e) = &l.extended {
            *by.entry(e.currency.clone()).or_default() += e.micros;
        }
    }
    by.into_iter().map(|(c, m)| Money::new(m, c)).collect()
}
