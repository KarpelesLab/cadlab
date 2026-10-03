//! Substitute candidates for BOM lines that cannot be sourced as designed (DECISIONS D26).
//!
//! A line needs a substitute when its MPNs are not found at the providers asked (for a fab:
//! its preferred catalog, e.g. LCSC for JLCPCB), are short of stock, or are end of life; a
//! generic line without an MPN gets candidates too. Candidates come from two sources only:
//!
//! - **Drop-in**: MPNs that a provider's cross-reference data ([`Candidate::drop_in`]) lists for
//!   one of the line's MPNs, in the same package. This is the only source for ICs, regulators,
//!   transistors, diodes, connectors and other non-passive parts: cadlab never guesses pin
//!   compatibility from names or MPN prefixes.
//! - **Parametric**: for passives (resistors, capacitors, inductors, ferrite beads, LEDs,
//!   fuses), parts in the same category and package with the same value, a tolerance at most
//!   and ratings at least the part's ([`crate::sourcing::query_for`]).
//!
//! Every candidate is in stock for the quantity needed and not obsolete. Ranking is
//! deterministic: drop-ins before parametric matches, then in stock, active, cheapest for the
//! quantity, most stock, then MPN and provider. Substitutes are suggestions: nothing here changes
//! the project, and an applied substitution is recorded per fab in `fab-lock.json`.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bom::BomRow;
use crate::model::part::{Category, Part};
use crate::sourcing::{self, Availability};
use crate::supplier::query::{Op, package_key};
use crate::supplier::{Candidate, Lifecycle, SearchQuery, Suppliers};

/// How a substitute was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// Listed as a drop-in replacement by a provider's cross-reference data.
    DropIn,
    /// Same category, package and value with equal or better tolerance and ratings (passives).
    Parametric,
    /// Chosen by the user outside the candidates (`fab.substitute` with `force`).
    Manual,
}

/// A substitute candidate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Substitute {
    /// How it was found.
    pub basis: Basis,
    /// The offer.
    pub offer: Candidate,
    /// The criteria it meets (`package 0402`, `tolerance <= 1%`, `drop-in for X`).
    pub matches: Vec<String>,
}

/// Substitute candidates for one BOM line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LineSubstitutes {
    /// Part ID of the line.
    pub part: String,
    /// Populated designators.
    pub designators: Vec<String>,
    /// Why it needs a substitute.
    pub status: Availability,
    /// The MPN the line orders today, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    /// Quantity needed (per-board quantity × boards).
    pub needed: u64,
    /// Candidates, best first.
    pub candidates: Vec<Substitute>,
    /// Why there is no candidate, when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Categories substituted on parameters alone (two-terminal parts whose footprint and function
/// follow from package and value).
pub fn parametric(c: Category) -> bool {
    matches!(
        c,
        Category::Resistor
            | Category::Capacitor
            | Category::Inductor
            | Category::FerriteBead
            | Category::Led
            | Category::Fuse
    )
}

fn op_str(op: Op) -> &'static str {
    match op {
        Op::Eq => "=",
        Op::Ge => ">=",
        Op::Le => "<=",
        Op::Gt => ">",
        Op::Lt => "<",
    }
}

fn usable(c: &Candidate, needed: u64) -> bool {
    c.stock >= needed.max(1) && !matches!(c.lifecycle, Lifecycle::Obsolete | Lifecycle::LastTimeBuy)
}

/// Finds substitutes for `row` (whose part is `part`, availability `status`) at the providers
/// in `only` (all when empty), at most `limit`. Provider errors are appended to `errors`.
#[allow(clippy::too_many_arguments)]
pub fn for_line(
    row: &BomRow,
    part: Option<&Part>,
    status: Availability,
    needed: u64,
    suppliers: &Suppliers,
    only: &[String],
    limit: usize,
    errors: &mut Vec<String>,
) -> LineSubstitutes {
    let own = sourcing::line_mpns(row);
    let is_own = |m: &str| own.iter().any(|o| o.eq_ignore_ascii_case(m));
    let mut err = |e: Vec<String>| {
        for e in e {
            if !errors.contains(&e) {
                errors.push(e);
            }
        }
    };
    let pkg = row.package.as_deref().map(package_key);
    let rank_q = SearchQuery { quantity: needed.max(1), include_obsolete: true, ..Default::default() };

    // Drop-ins from cross-reference data of the line's own MPNs (asked at every provider: the
    // data may come from one that does not stock the part for this fab).
    let mut refs: BTreeSet<String> = BTreeSet::new();
    for m in &own {
        let r = suppliers.lookup(m, &[]);
        err(r.errors);
        for c in r.candidates {
            refs.extend(c.drop_in.into_iter().filter(|d| !is_own(d)));
        }
    }
    let mut drop_ins = Vec::new();
    for d in &refs {
        let r = suppliers.lookup(d, only);
        err(r.errors);
        for c in r.candidates {
            let same_pkg = match (&pkg, c.package.as_deref().map(package_key)) {
                (Some(a), Some(b)) => *a == b,
                _ => true,
            };
            let same_cat = match (part.map(|p| p.category), c.category) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            };
            if usable(&c, needed) && same_pkg && same_cat {
                let mut matches = vec![format!("drop-in for {}", own.join(" / "))];
                if let Some(p) = &c.package {
                    matches.push(format!("package {p}"));
                }
                drop_ins.push(Substitute { basis: Basis::DropIn, offer: c, matches });
            }
        }
    }
    rank_q.rank_by(&mut drop_ins, |s| &s.offer);

    // Parametric matches for passives.
    let mut params = Vec::new();
    let mut note = None;
    match part {
        Some(p) if parametric(p.category) => {
            let mut q = sourcing::query_for(p, needed.max(1));
            q.limit = limit.max(1) * 4 + own.len();
            if q.package.is_none() || q.filters.is_empty() {
                note = Some(format!("`{}` has no package or value parameters to match substitutes on", p.id));
            } else {
                let r = suppliers.search(&q, only);
                err(r.errors);
                let mut matches = vec![format!("package {}", q.package.clone().unwrap_or_default())];
                matches.extend(q.filters.iter().map(|f| format!("{} {} {}", f.key, op_str(f.op), f.value)));
                for c in r.candidates {
                    if is_own(&c.mpn) || !usable(&c, needed) || c.category.is_some_and(|k| k != p.category) {
                        continue;
                    }
                    params.push(Substitute { basis: Basis::Parametric, offer: c, matches: matches.clone() });
                }
                if params.is_empty() && drop_ins.is_empty() {
                    note = Some(format!("no part in stock for {} with: {}", needed.max(1), matches.join(", ")));
                }
            }
        }
        Some(p) if drop_ins.is_empty() => {
            note = Some(format!(
                "{} parts are substituted only from drop-in cross-reference data, and no provider lists an available \
                 drop-in for {}; pick a replacement from its datasheet and approve it (bom.approve)",
                p.category.label(),
                if own.is_empty() { p.id.clone() } else { own.join(" / ") }
            ));
        }
        None if drop_ins.is_empty() => note = Some(format!("`{}` is not in the project library", row.part)),
        _ => {}
    }

    let mut candidates = drop_ins;
    candidates.extend(params);
    let mut seen = BTreeSet::new();
    candidates.retain(|s| seen.insert((s.offer.provider.clone(), s.offer.sku.clone())));
    candidates.truncate(limit.max(1));
    LineSubstitutes {
        part: row.part.clone(),
        designators: row.refdes.clone(),
        status,
        mpn: row.order_mpn().map(|(_, m)| m.to_string()),
        needed,
        candidates,
        note,
    }
}

/// One-line description of a candidate: `Samsung CL05B104KO5NNNC (lcsc C1525, stock 2000000)`.
pub fn describe(s: &Substitute) -> String {
    let o = &s.offer;
    format!(
        "{}{} ({} {}, stock {})",
        o.manufacturer.as_deref().map(|m| format!("{m} ")).unwrap_or_default(),
        o.mpn,
        o.provider,
        o.sku,
        o.stock
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::supplier::catalog::Catalog;

    fn cand(v: serde_json::Value) -> Candidate {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn categories() {
        assert!(parametric(Category::Resistor) && parametric(Category::Capacitor));
        assert!(!parametric(Category::Ldo) && !parametric(Category::Mosfet) && !parametric(Category::Diode));
    }

    #[test]
    fn drop_ins_need_cross_reference_data() {
        let parts = vec![
            cand(serde_json::json!({"sku": "A1", "mpn": "OLD-LDO", "category": "ldo", "package": "SOT-23-5",
                "stock": 0, "drop_in": ["NEW-LDO", "WRONG-PKG"]})),
            cand(serde_json::json!({"sku": "A2", "mpn": "NEW-LDO", "category": "ldo", "package": "SOT-23-5",
                "stock": 500, "lifecycle": "active"})),
            cand(serde_json::json!({"sku": "A3", "mpn": "WRONG-PKG", "category": "ldo", "package": "SOT-223",
                "stock": 500, "lifecycle": "active"})),
            cand(serde_json::json!({"sku": "A4", "mpn": "LOOKALIKE", "category": "ldo", "package": "SOT-23-5",
                "stock": 500, "lifecycle": "active"})),
        ];
        let s = Suppliers::new().with(Arc::new(Catalog::from_parts("cat", parts)));
        let row = BomRow {
            part: "OLD-LDO".into(),
            category: Category::Ldo,
            refdes: vec!["U1".into()],
            dnp: vec![],
            quantity: 1,
            value: "OLD-LDO".into(),
            description: String::new(),
            footprint: None,
            package: Some("SOT-23-5".into()),
            mount: None,
            manufacturer: None,
            mpn: Some("OLD-LDO".into()),
            approved: vec![],
            notes: None,
        };
        let mut errors = Vec::new();
        let l = for_line(&row, None, Availability::LowStock, 1, &s, &[], 5, &mut errors);
        let mpns: Vec<&str> = l.candidates.iter().map(|c| c.offer.mpn.as_str()).collect();
        assert_eq!(mpns, ["NEW-LDO"], "same package only, never by resemblance");
        assert_eq!(l.candidates[0].basis, Basis::DropIn);
        assert!(errors.is_empty());
    }
}
