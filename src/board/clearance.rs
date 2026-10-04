//! Copper clearances in effect between items: net classes, local pad and footprint clearances
//! and custom rules (`board.custom_rules`). The DRC and zone fills resolve them here, so a fill
//! never keeps less than the DRC asks. See `docs/BOARD.md`, "Design rules".
//!
//! For two items of different nets, in decreasing priority:
//!
//! 1. the last custom rule (board order) with a clearance whose scope holds either item;
//! 2. when either item has a local clearance: the larger local clearance of the two (the other
//!    item's net class does not count);
//! 3. the larger net clearance of the two (class, else `board.rules` `clearance`).
//!
//! The result is never below `board.rules` `min_clearance`.

use polyclip::{Geometry, Polygon, Ring};

use super::{CopperItem, ItemRef, footprint_for, placed_courtyard};
use crate::model::Project;
use crate::model::board::{CustomRule, ItemKind, RuleScope};
use crate::units::Nm;

/// `*` and `?` wildcard match (whole string).
pub fn glob(pattern: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), s.chars().collect());
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < t.len() {
        if i < p.len() && (p[i] == '?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == '*' {
        i += 1;
    }
    i == p.len()
}

/// The kind of a copper item, for rule scopes.
pub fn item_kind(it: &ItemRef) -> ItemKind {
    match it {
        ItemRef::Pad(..) => ItemKind::Pad,
        ItemRef::Track(_) => ItemKind::Track,
        ItemRef::Via(_) => ItemKind::Via,
        ItemRef::Zone(..) => ItemKind::Zone,
        ItemRef::Graphic(_) | ItemRef::FpGraphic(..) => ItemKind::Graphic,
    }
}

/// A rule scope prepared against a board: the courtyards and areas it names.
struct Scope<'a> {
    scope: &'a RuleScope,
    /// Designators matching `footprint`.
    members: Vec<String>,
    /// Courtyards of footprints matching `in_courtyard`.
    courtyards: Vec<Polygon>,
    /// Outlines of the areas named by `in_area` (no area: nothing matches).
    areas: Vec<Ring>,
}

/// Whether a footprint (designator, footprint name) matches a pattern.
fn footprint_matches(p: &Project, pattern: &str, refdes: &str) -> bool {
    glob(pattern, refdes) || footprint_for(p, refdes).is_some_and(|f| glob(pattern, &f.name))
}

impl<'a> Scope<'a> {
    fn new(p: &Project, scope: &'a RuleScope) -> Scope<'a> {
        let board = p.board();
        let members = match &scope.footprint {
            Some(pat) => board.footprints.keys().filter(|r| footprint_matches(p, pat, r)).cloned().collect(),
            None => Vec::new(),
        };
        let courtyards = match &scope.in_courtyard {
            Some(pat) => board
                .footprints
                .keys()
                .filter(|r| footprint_matches(p, pat, r))
                .filter_map(|r| placed_courtyard(p, r))
                .map(|mut r| {
                    if !r.is_ccw() {
                        r.reverse_orientation();
                    }
                    Polygon::new(r, vec![])
                })
                .collect(),
            None => Vec::new(),
        };
        let areas = match &scope.in_area {
            Some(name) => board
                .keepouts
                .iter()
                .filter(|k| &k.name == name && k.outline.len() >= 3)
                .map(|k| k.outline.iter().map(|&q| q.into()).collect::<Vec<polyclip::Point>>().into())
                .collect(),
            None => Vec::new(),
        };
        Scope { scope, members, courtyards, areas }
    }

    fn holds(&self, it: &CopperItem) -> bool {
        let s = self.scope;
        if !s.kinds.is_empty() && !s.kinds.contains(&item_kind(&it.item)) {
            return false;
        }
        if let Some(l) = &s.layer
            && !it.layers.contains(l)
        {
            return false;
        }
        if s.footprint.is_some() && !it.item.footprint().is_some_and(|r| self.members.iter().any(|m| m == r)) {
            return false;
        }
        if s.in_courtyard.is_some() {
            let bb = it.shape.bbox();
            let hit = self
                .courtyards
                .iter()
                .any(|c| c.bbox().zip(bb).is_some_and(|(a, b)| a.intersects(&b)) && polyclip::intersects(c, &it.shape));
            if !hit {
                return false;
            }
        }
        if s.in_area.is_some() && !self.areas.iter().any(|a| polyclip::contains(a, &it.shape)) {
            return false;
        }
        true
    }
}

/// Clearances of a set of copper items.
pub struct Clearances {
    /// Net (class) clearance per item.
    class: Vec<Nm>,
    /// Local clearance per item.
    local: Vec<Option<Nm>>,
    /// The last custom rule with a clearance holding each item: (rule index, value).
    rule: Vec<Option<(usize, Nm)>>,
    floor: Nm,
}

impl Clearances {
    /// Resolves the clearances of `items`. Without `classes` (fab limit checks), every item has
    /// the rules' clearance and local values and custom rules are ignored.
    pub fn new(p: &Project, items: &[CopperItem], classes: bool) -> Clearances {
        let rules = &p.board().rules;
        if !classes {
            return Clearances {
                class: vec![rules.clearance; items.len()],
                local: vec![None; items.len()],
                rule: vec![None; items.len()],
                floor: Nm::ZERO,
            };
        }
        let mut cache: std::collections::BTreeMap<Option<&str>, Nm> = Default::default();
        let class = items
            .iter()
            .map(|it| {
                let n = it.net.as_deref();
                *cache.entry(n).or_insert_with(|| super::zones::class_clearance(p, n).unwrap_or(rules.clearance))
            })
            .collect();
        let local = items.iter().map(|it| it.local.clearance).collect();
        Clearances { class, local, rule: rule_matches(p, items, |r| r.clearance), floor: rules.min_clearance }
    }

    /// The clearance item `i` keeps from any other net on its own (zone fills): its custom
    /// rule's, else its local, else its net's.
    pub fn item(&self, i: usize) -> Nm {
        let v = match (self.rule[i], self.local[i]) {
            (Some((_, v)), _) => v,
            (None, Some(v)) => v,
            (None, None) => self.class[i],
        };
        v.max(self.floor)
    }

    /// The clearance required between items `i` and `j` (of different nets).
    pub fn pair(&self, i: usize, j: usize) -> Nm {
        let rule = match (self.rule[i], self.rule[j]) {
            (Some(a), Some(b)) => Some(if a.0 >= b.0 { a.1 } else { b.1 }),
            (a, b) => a.or(b).map(|x| x.1),
        };
        let v = match rule {
            Some(v) => v,
            None => match (self.local[i], self.local[j]) {
                (None, None) => self.class[i].max(self.class[j]),
                (a, b) => a.unwrap_or(Nm::ZERO).max(b.unwrap_or(Nm::ZERO)),
            },
        };
        v.max(self.floor)
    }

    /// The largest clearance any pair could need (for spatial search margins).
    pub fn max(&self) -> Nm {
        let a = self.class.iter().copied().max().unwrap_or(Nm::ZERO);
        let b = self.local.iter().flatten().copied().max().unwrap_or(Nm::ZERO);
        let c = self.rule.iter().flatten().map(|r| r.1).max().unwrap_or(Nm::ZERO);
        a.max(b).max(c).max(self.floor)
    }
}

/// For each item, the last custom rule (with a value `pick` gives) whose scope holds it.
pub fn rule_matches(
    p: &Project,
    items: &[CopperItem],
    pick: impl Fn(&CustomRule) -> Option<Nm>,
) -> Vec<Option<(usize, Nm)>> {
    let rules = &p.board().custom_rules;
    let mut out = vec![None; items.len()];
    for (k, r) in rules.iter().enumerate() {
        let Some(v) = pick(r) else { continue };
        let scope = Scope::new(p, &r.scope);
        for (i, it) in items.iter().enumerate() {
            if scope.holds(it) {
                out[i] = Some((k, v));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("QFN-80*", "QFN-80-1EP_10x10mm"));
        assert!(glob("U?", "U1"));
        assert!(!glob("U?", "U12"));
        assert!(glob("*", ""));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b", "aXXc"));
    }
}
