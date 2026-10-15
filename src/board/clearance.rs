//! Copper clearances in effect between items: net classes, local pad and footprint clearances
//! and custom rules (`board.custom_rules`). The DRC and zone fills resolve them here, so a fill
//! never keeps less than the DRC asks. See `docs/BOARD.md`, "Design rules".
//!
//! For two items of different nets, in decreasing priority:
//!
//! 1. the last custom rule (board order) with a clearance whose scope holds either item, or,
//!    for a rule with a `with` scope, whose scope holds one item and `with` the other;
//! 2. when either item has a local clearance: the larger local clearance of the two (the other
//!    item's net class does not count);
//! 3. the larger net clearance of the two (class, else `board.rules` `clearance`).
//!
//! The result is never below `board.rules` `min_clearance`.

use polyclip::{Geometry, Polygon, Ring};

use super::{CopperItem, ItemRef, footprint_for, placed_courtyards};
use crate::model::Project;
use crate::model::board::{CustomRule, ItemKind, RuleScope};
use crate::units::Nm;

/// Whether an item reaches into an area (a courtyard, a rule area) by more than `tol`: copper
/// merely touching the outline is outside, measured on the item's exact outline when it has one,
/// else on the polygons (their overlap must survive shrinking by half of `tol`).
fn reaches_into(p: &Project, area: &Polygon, it: &CopperItem, tol: i64) -> bool {
    let item = &it.shape;
    if !polyclip::intersects(area, item) {
        return false;
    }
    if let Some(core) = super::exact::item_core(p, it) {
        let outline =
            super::exact::Core { points: area.outer.0.iter().map(|&q| q.into()).collect(), closed: true, radius: 0.0 };
        return super::exact::reaches_into(&core, &outline, tol as f64);
    }
    let Ok(inter) = polyclip::boolean(polyclip::Op::Intersection, area, item, polyclip::FillRule::NonZero) else {
        return true;
    };
    let arcs = polyclip::ArcTol::new(1_000, polyclip::Side::Outside);
    polyclip::offset(&inter, -(tol / 2).max(1), polyclip::Join::Round, arcs).is_ok_and(|core| !core.is_empty())
}

/// How far copper must reach into a courtyard or a rule area to be in it: the DRC's measurement
/// tolerance (KiCad's `intersectsCourtyard` takes a track whose straight side reaches 1 µm in,
/// probed with kicad-cli 10.0.7; a round track end 1.4 µm in, measured on its exact outline, is
/// outside in both tools).
const REACH: i64 = crate::drc::TOLERANCE.0;

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
    p: &'a Project,
    scope: &'a RuleScope,
    /// Designators matching `footprint`.
    members: Vec<String>,
    /// Courtyards of footprints matching `in_courtyard`.
    courtyards: Vec<Polygon>,
    /// Outlines of the areas named by `in_area` (no area: nothing matches).
    areas: Vec<Ring>,
    /// Outlines of the areas named by `touches_area`.
    touched: Vec<Polygon>,
}

/// Whether a footprint (designator, footprint name) matches a pattern.
fn footprint_matches(p: &Project, pattern: &str, refdes: &str) -> bool {
    glob(pattern, refdes) || footprint_for(p, refdes).is_some_and(|f| glob(pattern, &f.name))
}

impl<'a> Scope<'a> {
    fn new(p: &'a Project, scope: &'a RuleScope) -> Scope<'a> {
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
                // KiCad's `intersectsCourtyard`: the courtyard on the footprint's own side
                // (every piece of it).
                .flat_map(|r| {
                    let side = board.footprints[r].side;
                    placed_courtyards(p, r).into_iter().filter(move |(_, s)| *s == side).map(|(c, _)| c)
                })
                .map(|mut r| {
                    if !r.is_ccw() {
                        r.reverse_orientation();
                    }
                    Polygon::new(r, vec![])
                })
                .collect(),
            None => Vec::new(),
        };
        let area_rings = |name: &Option<String>| -> Vec<Ring> {
            match name {
                Some(name) => board
                    .keepouts
                    .iter()
                    .filter(|k| &k.name == name && k.outline.len() >= 3)
                    .map(|k| k.outline.iter().map(|&q| q.into()).collect::<Vec<polyclip::Point>>().into())
                    .collect(),
                None => Vec::new(),
            }
        };
        let areas = area_rings(&scope.in_area);
        let touched = area_rings(&scope.touches_area)
            .into_iter()
            .map(|mut r| {
                if !r.is_ccw() {
                    r.reverse_orientation();
                }
                Polygon::new(r, vec![])
            })
            .collect();
        Scope { p, scope, members, courtyards, areas, touched }
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
                .any(|c| c.bbox().zip(bb).is_some_and(|(a, b)| a.intersects(&b)) && reaches_into(self.p, c, it, REACH));
            if !hit {
                return false;
            }
        }
        if s.in_area.is_some() && !self.areas.iter().any(|a| polyclip::contains(a, &it.shape)) {
            return false;
        }
        if s.touches_area.is_some() {
            let bb = it.shape.bbox();
            let hit = self
                .touched
                .iter()
                .any(|a| a.bbox().zip(bb).is_some_and(|(x, y)| x.intersects(&y)) && reaches_into(self.p, a, it, REACH));
            if !hit {
                return false;
            }
        }
        true
    }
}

/// Whether a scope can hold zone fills (its kinds allow them).
fn may_hold_zones(s: &RuleScope) -> bool {
    s.kinds.is_empty() || s.kinds.contains(&ItemKind::Zone)
}

/// Custom clearance rules between two scopes (`with`): per item, the rules whose `scope`
/// holds it and those whose `with` holds it (ascending rule indices).
struct Paired {
    a: Vec<Vec<u32>>,
    b: Vec<Vec<u32>>,
    value: Vec<Option<Nm>>,
    /// Per item, the largest value of a paired rule that holds it on one side and may hold a
    /// zone fill on the other.
    zone: Vec<Nm>,
}

impl Paired {
    fn new(p: &Project, items: &[CopperItem]) -> Option<Paired> {
        let rules = &p.board().custom_rules;
        if !rules.iter().any(|r| r.with.is_some() && r.clearance.is_some()) {
            return None;
        }
        let n = items.len();
        let mut out = Paired {
            a: vec![Vec::new(); n],
            b: vec![Vec::new(); n],
            value: vec![None; rules.len()],
            zone: vec![Nm::ZERO; n],
        };
        for (k, r) in rules.iter().enumerate() {
            let (Some(with), Some(v)) = (&r.with, r.clearance) else { continue };
            out.value[k] = Some(v);
            let (sa, sb) = (Scope::new(p, &r.scope), Scope::new(p, with));
            for (i, it) in items.iter().enumerate() {
                if sa.holds(it) {
                    out.a[i].push(k as u32);
                    if may_hold_zones(with) {
                        out.zone[i] = out.zone[i].max(v);
                    }
                }
                if sb.holds(it) {
                    out.b[i].push(k as u32);
                    if may_hold_zones(&r.scope) {
                        out.zone[i] = out.zone[i].max(v);
                    }
                }
            }
        }
        Some(out)
    }

    /// The last paired rule holding `i` and `j` on opposite sides.
    fn pair(&self, i: usize, j: usize) -> Option<(usize, Nm)> {
        let last_common = |x: &[u32], y: &[u32]| x.iter().rev().find(|k| y.contains(k)).copied();
        let k = match (last_common(&self.a[i], &self.b[j]), last_common(&self.a[j], &self.b[i])) {
            (Some(x), Some(y)) => x.max(y),
            (x, y) => x.or(y)?,
        } as usize;
        Some((k, self.value[k]?))
    }
}

/// For each rule with disallowed kinds, the items in its scope of those kinds: (rule index,
/// item index), by rule then item.
pub fn disallowed(p: &Project, items: &[CopperItem]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (k, r) in p.board().custom_rules.iter().enumerate() {
        if r.disallow.is_empty() {
            continue;
        }
        let scope = Scope::new(p, &r.scope);
        for (i, it) in items.iter().enumerate() {
            if r.disallow.contains(&item_kind(&it.item)) && scope.holds(it) {
                out.push((k, i));
            }
        }
    }
    out
}

/// Clearances of a set of copper items.
pub struct Clearances {
    /// Net (class) clearance per item.
    class: Vec<Nm>,
    /// Local clearance per item.
    local: Vec<Option<Nm>>,
    /// The last custom rule (without `with`) with a clearance holding each item: (rule index,
    /// value).
    rule: Vec<Option<(usize, Nm)>>,
    /// Rules with a clearance between two scopes, if any.
    paired: Option<Paired>,
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
                paired: None,
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
        Clearances {
            class,
            local,
            rule: rule_matches(p, items, |r| r.clearance),
            paired: Paired::new(p, items),
            floor: rules.min_clearance,
        }
    }

    /// The clearance item `i` keeps from any other net on its own (zone fills): its custom
    /// rule's, else its local, else its net's.
    pub fn item(&self, i: usize) -> Nm {
        let v = match (self.rule[i], self.local[i]) {
            (Some((_, v)), _) => v,
            (None, Some(v)) => v,
            (None, None) => self.class[i],
        };
        // A rule between two scopes may apply between this item and a fill: keep the most
        // any of them asks.
        let v = match &self.paired {
            Some(pr) => v.max(pr.zone[i]),
            None => v,
        };
        v.max(self.floor)
    }

    /// The clearance required between items `i` and `j` (of different nets).
    pub fn pair(&self, i: usize, j: usize) -> Nm {
        self.resolve(i, j).0
    }

    /// The custom rule deciding the clearance between items `i` and `j`, if any.
    pub fn pair_rule(&self, i: usize, j: usize) -> Option<usize> {
        self.resolve(i, j).1
    }

    fn resolve(&self, i: usize, j: usize) -> (Nm, Option<usize>) {
        let mut rule = match (self.rule[i], self.rule[j]) {
            (Some(a), Some(b)) => Some(if a.0 >= b.0 { a } else { b }),
            (a, b) => a.or(b),
        };
        if let Some(pr) = &self.paired
            && let Some(x) = pr.pair(i, j)
            && rule.is_none_or(|r| x.0 > r.0)
        {
            rule = Some(x);
        }
        let v = match rule {
            Some((_, v)) => v,
            None => match (self.local[i], self.local[j]) {
                (None, None) => self.class[i].max(self.class[j]),
                (a, b) => a.unwrap_or(Nm::ZERO).max(b.unwrap_or(Nm::ZERO)),
            },
        };
        (v.max(self.floor), rule.map(|r| r.0))
    }

    /// The largest clearance any pair could need (for spatial search margins).
    pub fn max(&self) -> Nm {
        let a = self.class.iter().copied().max().unwrap_or(Nm::ZERO);
        let b = self.local.iter().flatten().copied().max().unwrap_or(Nm::ZERO);
        let c = self.rule.iter().flatten().map(|r| r.1).max().unwrap_or(Nm::ZERO);
        let d = self.paired.as_ref().and_then(|pr| pr.value.iter().flatten().copied().max()).unwrap_or(Nm::ZERO);
        a.max(b).max(c).max(d).max(self.floor)
    }
}

/// For each item, the last custom rule (with a value `pick` gives, and no `with` scope) whose
/// scope holds it.
pub fn rule_matches(
    p: &Project,
    items: &[CopperItem],
    pick: impl Fn(&CustomRule) -> Option<Nm>,
) -> Vec<Option<(usize, Nm)>> {
    let rules = &p.board().custom_rules;
    let mut out = vec![None; items.len()];
    for (k, r) in rules.iter().enumerate() {
        if r.with.is_some() {
            continue;
        }
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
