//! Automatic schematic layout. See the module documentation of [`crate::schematic`].

use std::collections::{BTreeMap, BTreeSet};

use super::symbol::{local_extent, pin_ends, rot, symbol_of};
use super::{Dir, GRID, Hints, Label, LabelKind, Placement, SheetLayout};
use crate::geom::Point;
use crate::model::Project;
use crate::model::circuit::PinRef;
use crate::model::part::{PinKind, Symbol, SymbolStyle};
use crate::model::sections::natural_cmp;
use crate::render::font;
use crate::symbolgen::is_ground;
use crate::units::Nm;

const G: i64 = GRID.0;
/// Text size used to size label room (mm).
const TEXT: f64 = 1.27;
/// Minimum spacing between aligned attachments on one side of an anchor.
const ATTACH_PITCH: i64 = 3 * G;

fn nm(v: f64) -> i64 {
    (v * 1e6).round() as i64
}

fn up_grid(v: i64) -> i64 {
    v.div_euclid(G) * G + if v.rem_euclid(G) == 0 { 0 } else { G }
}

/// Label length along its direction (mm), for spacing.
fn label_len(net: &str, kind: LabelKind) -> f64 {
    match kind {
        LabelKind::Ground => 3.0,
        LabelKind::Power => 2.0 + TEXT + 0.6,
        LabelKind::Net | LabelKind::Wire => font::width(net, TEXT) + 2.4,
    }
}

/// Footprint of a label on the sheet: (x0, y0, x1, y1) in nm.
fn label_box(l: &Label) -> (i64, i64, i64, i64) {
    let len = nm(label_len(&l.net, l.kind));
    let across = match l.kind {
        LabelKind::Power => nm(font::width(&l.net, TEXT) / 2.0 + 1.0),
        _ => nm(1.6),
    };
    let (x, y) = (l.at.x.0, l.at.y.0);
    match l.dir {
        Dir::Right => (x, y - across, x + len, y + across),
        Dir::Left => (x - len, y - across, x, y + across),
        Dir::Up => (x - across, y, x + across, y + len),
        Dir::Down => (x - across, y - len, x + across, y),
    }
}

struct Comp {
    sym: Symbol,
    two_terminal: bool,
    /// Width of the designator/value text (nm), for spacing.
    text_w: i64,
}

struct Net<'a> {
    pins: Vec<&'a PinRef>,
    power: Option<LabelKind>,
}

/// A group of components laid out together, in local coordinates.
#[derive(Default)]
struct Group {
    placements: BTreeMap<String, Placement>,
    wires: Vec<(Point, Point, PinRef, PinRef)>,
}

fn add(p: Point, d: Dir, len: i64) -> Point {
    let (dx, dy) = d.vec();
    Point::new(Nm(p.x.0 + dx * len), Nm(p.y.0 + dy * len))
}

/// Places a two-terminal symbol so that pin `near` sits at `target`, pointing back along `d`.
fn place_inline(sym: &Symbol, near: &str, target: Point, d: Dir) -> (Placement, Point, String) {
    let pin = sym.pins.iter().find(|p| p.number == near).expect("pin exists");
    let far = sym.pins.iter().find(|p| p.number != near).expect("two pins");
    let side = |s: crate::model::part::Side| match s {
        crate::model::part::Side::Left => Dir::Left,
        crate::model::part::Side::Right => Dir::Right,
        crate::model::part::Side::Top => Dir::Up,
        crate::model::part::Side::Bottom => Dir::Down,
    };
    let r = (0..4).find(|r| side(pin.side.expect("generated")).rotated(*r) == d.opposite()).expect("some rotation");
    let n = pin.at.expect("generated");
    let (nx, ny) = rot((n.x.0, n.y.0), r);
    let center = Point::new(Nm(target.x.0 - nx), Nm(target.y.0 - ny));
    let pl = Placement { at: center, rot: r };
    let f = far.at.expect("generated");
    let (fx, fy) = rot((f.x.0, f.y.0), r);
    (pl, Point::new(Nm(center.x.0 + fx), Nm(center.y.0 + fy)), far.number.clone())
}

/// Lays out the circuit on one sheet.
pub fn layout(p: &Project, hints: &Hints) -> SheetLayout {
    let c = p.circuit();
    let lib = p.library();

    // Components with usable symbols.
    let mut comps: BTreeMap<String, Comp> = BTreeMap::new();
    for (r, comp) in &c.components {
        if let Some(part) = lib.parts.get(&comp.part) {
            let sym = symbol_of(part);
            let two_terminal = sym.style != SymbolStyle::Box && sym.pins.len() == 2;
            let text_w = nm(font::width(&part.value(), TEXT).max(font::width(r, TEXT)) + 1.0);
            comps.insert(r.clone(), Comp { sym, two_terminal, text_w });
        }
    }
    let kind_of = |pin: &PinRef| -> Option<(PinKind, String)> {
        let comp = c.components.get(&pin.refdes)?;
        let sp = lib.parts.get(&comp.part)?.symbol.pins.iter().find(|s| s.number == pin.pin)?;
        Some((sp.kind, sp.label().to_string()))
    };

    // Nets, with power classification.
    let mut nets: BTreeMap<&str, Net> = BTreeMap::new();
    for (name, n) in &c.nets {
        let kinds: Vec<(PinKind, String)> = n.pins.iter().filter_map(&kind_of).collect();
        let has_power = kinds.iter().any(|(k, _)| matches!(k, PinKind::PowerIn | PinKind::PowerOut));
        let ground = is_ground(name)
            || (has_power
                && kinds.iter().filter(|(k, _)| *k == PinKind::PowerIn).all(|(_, l)| is_ground(l))
                && !kinds.iter().any(|(k, _)| *k == PinKind::PowerOut));
        let power = if ground {
            Some(LabelKind::Ground)
        } else if has_power || n.driven {
            Some(LabelKind::Power)
        } else {
            None
        };
        nets.insert(name, Net { pins: n.pins.iter().collect(), power });
    }
    let pin_net: BTreeMap<&PinRef, &str> = c.pin_index();
    let net_of = |r: &str, pin: &str| pin_net.get(&PinRef::new(r, pin)).copied();
    let is_power = |n: &str| nets.get(n).is_some_and(|x| x.power.is_some());

    // Anchors: box symbols, most pins first.
    let mut anchors: Vec<&String> =
        comps.iter().filter(|(r, x)| !x.two_terminal && !hints.contains_key(*r)).map(|(r, _)| r).collect();
    anchors.sort_by(|a, b| comps[*b].sym.pins.len().cmp(&comps[*a].sym.pins.len()).then_with(|| natural_cmp(a, b)));
    let mut passives: Vec<&String> =
        comps.iter().filter(|(r, x)| x.two_terminal && !hints.contains_key(*r)).map(|(r, _)| r).collect();
    passives.sort_by(|a, b| natural_cmp(a, b));

    let mut placed: BTreeSet<String> = BTreeSet::new();
    let mut groups: Vec<Group> = Vec::new();
    // Label room on a wire: the wire is long enough to hold the net name.
    let wire_gap = |net: &str, extra_pins: bool| -> i64 {
        if extra_pins { 2 * G + up_grid(nm(font::width(net, TEXT) + 1.5)) } else { 2 * G }
    };

    for a in &anchors {
        let mut g = Group::default();
        let asym = &comps[*a].sym;
        let apl = Placement { at: Point::ORIGIN, rot: 0 };
        g.placements.insert((*a).clone(), apl);
        placed.insert((*a).clone());
        // Per side: (position along the side, distance from the anchor reached by the attachment).
        let mut used: BTreeMap<Dir, Vec<(i64, i64)>> = BTreeMap::new();
        let ends = pin_ends(asym, &apl);
        // Inline attachments.
        for (num, end, d) in &ends {
            let Some(net) = net_of(a, num) else { continue };
            if is_power(net) {
                continue;
            }
            let along = if d.horizontal() { end.y.0 } else { end.x.0 };
            // Neighbours closer than two rows: no room for text, use labels instead. Two rows
            // apart: stagger outward past the neighbour so designators do not collide.
            let near: Vec<(i64, i64)> = used
                .get(d)
                .map(|v| v.iter().filter(|(u, _)| (u - along).abs() < ATTACH_PITCH).copied().collect())
                .unwrap_or_default();
            if near.iter().any(|(u, _)| (u - along).abs() < 2 * G) {
                continue;
            }
            let stagger = near.iter().map(|(_, reach)| reach + 2 * G).max().unwrap_or(0);
            let cand = nets[net].pins.iter().find(|pin| {
                passives.contains(&&pin.refdes) && !placed.contains(&pin.refdes) && comps[&pin.refdes].two_terminal
            });
            let Some(first) = cand else { continue };
            let extra = nets[net].pins.len() > 2;
            let target = add(*end, *d, wire_gap(net, extra).max(up_grid(stagger)));
            let (pl, mut far_end, mut far_pin) = place_inline(&comps[&first.refdes].sym, &first.pin, target, *d);
            g.placements.insert(first.refdes.clone(), pl);
            g.wires.push((*end, target, PinRef::new((*a).clone(), num.clone()), (*first).clone()));
            placed.insert(first.refdes.clone());
            // Chains through two-pin nets: R2 then D1.
            let mut cur = first.refdes.clone();
            for _ in 0..3 {
                let Some(n2) = net_of(&cur, &far_pin) else { break };
                if is_power(n2) || nets[n2].pins.len() != 2 {
                    break;
                }
                let Some(next) = nets[n2].pins.iter().find(|pin| pin.refdes != cur) else {
                    break;
                };
                if placed.contains(&next.refdes)
                    || !comps.get(&next.refdes).is_some_and(|x| x.two_terminal)
                    || hints.contains_key(&next.refdes)
                {
                    break;
                }
                let t2 = add(far_end, *d, 2 * G);
                let (pl2, fe2, fp2) = place_inline(&comps[&next.refdes].sym, &next.pin, t2, *d);
                g.placements.insert(next.refdes.clone(), pl2);
                g.wires.push((far_end, t2, PinRef::new(cur.clone(), far_pin.clone()), (*next).clone()));
                placed.insert(next.refdes.clone());
                cur = next.refdes.clone();
                far_end = fe2;
                far_pin = fp2;
            }
            let reach = (far_end.x.0 - end.x.0).abs() + (far_end.y.0 - end.y.0).abs() + 6 * G;
            used.entry(*d).or_default().push((along, reach));
        }
        groups.push(g);
    }

    // Decoupling capacitors (both pins on power nets): a row under the anchor powering them.
    for (gi, a) in anchors.iter().enumerate() {
        let asym = &comps[*a].sym;
        let rails: BTreeSet<&str> = asym
            .pins
            .iter()
            .filter(|sp| sp.kind == PinKind::PowerIn)
            .filter_map(|sp| net_of(a, &sp.number))
            .filter(|n| nets[n].power == Some(LabelKind::Power))
            .collect();
        let (_, y0, _, _) = local_extent(asym);
        let mut x = nm(local_extent(asym).0);
        let y_top = nm(y0) - 7 * G;
        for r in &passives {
            if placed.contains(*r) {
                continue;
            }
            let sym = &comps[*r].sym;
            let n: Vec<Option<&str>> = sym.pins.iter().map(|sp| net_of(r, &sp.number)).collect();
            let (Some(n0), Some(n1)) = (n[0], n[1]) else { continue };
            if !(is_power(n0) && is_power(n1)) {
                continue;
            }
            let (top_pin, _) = if rails.contains(n0) {
                (&sym.pins[0].number, n1)
            } else if rails.contains(n1) {
                (&sym.pins[1].number, n0)
            } else {
                continue;
            };
            // Vertical, the rail pin on top: place so that pin points up.
            let target = Point::new(Nm(up_grid(x)), Nm(y_top));
            let (pl, _, _) = place_inline(sym, top_pin, target, Dir::Down);
            groups[gi].placements.insert((*r).clone(), pl);
            placed.insert((*r).clone());
            x = up_grid(x) + 4 * G;
        }
    }

    // Everything else: a row of loose parts, each pin labeled.
    let mut loose = Group::default();
    let mut x = 0i64;
    for r in passives.iter().chain(anchors.iter()) {
        if placed.contains(*r) {
            continue;
        }
        let sym = &comps[*r].sym;
        let room = up_grid(nm(label_room(sym, r, &net_of)));
        let (x0, _, x1, _) = local_extent(sym);
        // Two-terminal symbols have pins at ±1.5 grid: a half-grid center keeps them on grid.
        let cx = up_grid(x + room + nm(-x0)) + if comps[*r].two_terminal { G / 2 } else { 0 };
        loose.placements.insert((*r).clone(), Placement { at: Point::new(Nm(cx), Nm(0)), rot: 0 });
        placed.insert((*r).clone());
        x = cx + nm(x1) + room + 2 * G;
    }
    if !loose.placements.is_empty() {
        groups.push(loose);
    }

    // Connectivity clusters: pins joined by wires.
    let mut cluster: BTreeMap<PinRef, usize> = BTreeMap::new();
    let mut next = 0usize;
    for g in &groups {
        for (_, _, pa, pb) in &g.wires {
            let ca = cluster.get(pa).copied();
            let cb = cluster.get(pb).copied();
            match (ca, cb) {
                (Some(x), Some(y)) if x != y => {
                    for v in cluster.values_mut() {
                        if *v == y {
                            *v = x;
                        }
                    }
                }
                (Some(x), None) => {
                    cluster.insert(pb.clone(), x);
                }
                (None, Some(y)) => {
                    cluster.insert(pa.clone(), y);
                }
                (None, None) => {
                    cluster.insert(pa.clone(), next);
                    cluster.insert(pb.clone(), next);
                    next += 1;
                }
                _ => {}
            }
        }
    }

    // Labels per group, local coordinates.
    let mut group_labels: Vec<Vec<Label>> = vec![Vec::new(); groups.len()];
    let mut group_nc: Vec<Vec<Point>> = vec![Vec::new(); groups.len()];
    for (gi, g) in groups.iter().enumerate() {
        for (r, pl) in &g.placements {
            for (num, end, d) in pin_ends(&comps[r].sym, pl) {
                let pin = PinRef::new(r.clone(), num.clone());
                let Some(net) = pin_net.get(&pin).copied() else {
                    if c.no_connect.contains(&pin) {
                        group_nc[gi].push(end);
                    }
                    continue;
                };
                let info = &nets[net];
                if let Some(kind) = info.power {
                    if !cluster.contains_key(&pin) {
                        group_labels[gi].push(Label { at: end, dir: d, net: net.to_string(), kind });
                    }
                    continue;
                }
                match cluster.get(&pin) {
                    None => {
                        group_labels[gi].push(Label { at: end, dir: d, net: net.to_string(), kind: LabelKind::Net })
                    }
                    Some(cl) => {
                        // A wired cluster needs a name when the net has pins outside it: on the
                        // wire leaving the anchor-side pin.
                        let outside = info.pins.iter().any(|q| cluster.get(*q) != Some(cl));
                        if outside && let Some((a, b, _, _)) = g.wires.iter().find(|(_, _, pa, _)| *pa == pin) {
                            let wd = if a.x == b.x {
                                if b.y > a.y { Dir::Up } else { Dir::Down }
                            } else if b.x > a.x {
                                Dir::Right
                            } else {
                                Dir::Left
                            };
                            group_labels[gi].push(Label {
                                at: add(*a, wd, G / 2),
                                dir: wd,
                                net: net.to_string(),
                                kind: LabelKind::Wire,
                            });
                        }
                    }
                }
            }
        }
    }

    // Group extents (nm), including labels and designator text.
    let extents: Vec<(i64, i64, i64, i64)> = groups
        .iter()
        .enumerate()
        .map(|(gi, g)| {
            let mut b = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
            let mut grow = |x0: i64, y0: i64, x1: i64, y1: i64| {
                b = (b.0.min(x0), b.1.min(y0), b.2.max(x1), b.3.max(y1));
            };
            for (r, pl) in &g.placements {
                let (x0, y0, x1, y1) = local_extent(&comps[r].sym);
                let corners = [(nm(x0), nm(y0)), (nm(x1), nm(y1))];
                let pts: Vec<(i64, i64)> = corners.iter().map(|&(x, y)| rot((x, y), pl.rot)).collect();
                let (xa, xb) = (pts[0].0.min(pts[1].0), pts[0].0.max(pts[1].0));
                let (ya, yb) = (pts[0].1.min(pts[1].1), pts[0].1.max(pts[1].1));
                let text = nm(5.5);
                // Box symbols carry their text outside the top-right corner; two-terminal symbols
                // center it on the body.
                let tw = comps[r].text_w;
                let (left, right) = if comps[r].two_terminal { (tw / 2, tw / 2) } else { (0, tw) };
                grow(
                    pl.at.x.0 + xa.min(-left) - nm(1.0),
                    pl.at.y.0 + ya - text,
                    pl.at.x.0 + xb.max(right) + nm(1.0) + if comps[r].two_terminal { 0 } else { tw },
                    pl.at.y.0 + yb + text,
                );
            }
            for l in &group_labels[gi] {
                let (x0, y0, x1, y1) = label_box(l);
                grow(x0, y0, x1, y1);
            }
            b
        })
        .collect();

    // Pack groups in rows.
    let gap = 4 * G;
    let max_w = extents.iter().map(|e| e.2 - e.0).max().unwrap_or(0).max(nm(260.0));
    let mut offsets = Vec::new();
    let (mut cx, mut cy, mut row_h) = (0i64, 0i64, 0i64);
    for e in &extents {
        let (w, h) = (e.2 - e.0, e.3 - e.1);
        if cx > 0 && cx + w > max_w {
            cx = 0;
            cy -= row_h + gap;
            row_h = 0;
        }
        // Group's top-left goes to (cx, cy); snap to grid.
        let dx = up_grid(cx - e.0);
        let dy = -up_grid(-(cy - e.3));
        offsets.push((dx, dy));
        cx += up_grid(w) + gap;
        row_h = row_h.max(up_grid(h));
    }

    // Content bounds after packing, plus hinted components.
    let mut out = SheetLayout::default();
    let mut bounds = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    let mut grow = |x: i64, y: i64| {
        bounds = (bounds.0.min(x), bounds.1.min(y), bounds.2.max(x), bounds.3.max(y));
    };
    let shift = |p: Point, o: (i64, i64)| Point::new(Nm(p.x.0 + o.0), Nm(p.y.0 + o.1));
    for (gi, g) in groups.iter().enumerate() {
        let o = offsets[gi];
        for (r, pl) in &g.placements {
            out.placements.insert(r.clone(), Placement { at: shift(pl.at, o), rot: pl.rot });
        }
        for (a, b, _, _) in &g.wires {
            out.wires.push((shift(*a, o), shift(*b, o)));
        }
        for l in &group_labels[gi] {
            out.labels.push(Label { at: shift(l.at, o), ..l.clone() });
        }
        for n in &group_nc[gi] {
            out.no_connects.push(shift(*n, o));
        }
        let e = extents[gi];
        grow(e.0 + o.0, e.1 + o.1);
        grow(e.2 + o.0, e.3 + o.1);
    }
    // Hinted components: their placement as given, every pin labeled.
    let hinted_at: Vec<(String, Placement)> =
        hints.iter().filter(|(r, _)| comps.contains_key(*r)).map(|(r, p)| (r.clone(), *p)).collect();

    // Choose the paper and place the content at its top-left, keeping KiCad's grid (Y down from
    // the top edge) aligned.
    let margin = 5 * G;
    let title_h = nm(16.0);
    let (cw, ch) = if bounds.0 == i64::MAX { (0, 0) } else { (bounds.2 - bounds.0, bounds.3 - bounds.1) };
    let papers =
        [("A4", 297.0, 210.0), ("A3", 420.0, 297.0), ("A2", 594.0, 420.0), ("A1", 841.0, 594.0), ("A0", 1189.0, 841.0)];
    let (paper, w, h) = papers
        .iter()
        .find(|(_, w, h)| nm(*w) >= cw + 2 * margin && nm(*h) >= ch + 2 * margin + title_h)
        .map(|(n, w, h)| (n.to_string(), nm(*w), nm(*h)))
        .unwrap_or_else(|| ("User".into(), up_grid(cw + 2 * margin), up_grid(ch + 2 * margin + title_h)));
    let dx = if bounds.0 == i64::MAX { margin } else { up_grid(margin - bounds.0) };
    // KiCad y = h - y must stay on grid: dy = h - m·G.
    let m = if bounds.3 == i64::MIN { 2 } else { (margin + bounds.3 + G - 1).div_euclid(G) };
    let dy = h - m * G;
    let o = (dx, dy);
    out.placements =
        out.placements.into_iter().map(|(r, pl)| (r, Placement { at: shift(pl.at, o), rot: pl.rot })).collect();
    out.wires = out.wires.into_iter().map(|(a, b)| (shift(a, o), shift(b, o))).collect();
    out.labels = out.labels.into_iter().map(|l| Label { at: shift(l.at, o), ..l }).collect();
    out.no_connects = out.no_connects.into_iter().map(|p| shift(p, o)).collect();
    for (r, mut pl) in hinted_at {
        // Keep pins on the 1.27 mm grid measured from the top edge (KiCad's origin).
        let half = G / 2;
        pl.at.y = Nm(h - ((h - pl.at.y.0) as f64 / half as f64).round() as i64 * half);
        out.placements.insert(r.clone(), pl);
        for (num, end, d) in pin_ends(&comps[&r].sym, &pl) {
            let pin = PinRef::new(r.clone(), num);
            match pin_net.get(&pin) {
                Some(net) => {
                    let kind = nets[net].power.unwrap_or(LabelKind::Net);
                    out.labels.push(Label { at: end, dir: d, net: net.to_string(), kind });
                }
                None if c.no_connect.contains(&pin) => out.no_connects.push(end),
                None => {}
            }
        }
    }
    out.size = (Nm(w), Nm(h));
    out.paper = paper;
    out
}

/// Room needed beside a loose symbol for its pin labels (mm).
fn label_room<'a>(sym: &Symbol, r: &str, net_of: &impl Fn(&str, &str) -> Option<&'a str>) -> f64 {
    sym.pins.iter().filter_map(|p| net_of(r, &p.number)).map(|n| label_len(n, LabelKind::Net)).fold(0.0, f64::max) + 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_helpers() {
        assert_eq!(up_grid(1), G);
        assert_eq!(up_grid(G), G);
        assert_eq!(up_grid(-1), 0);
        assert_eq!(add(Point::ORIGIN, Dir::Left, G), Point::new(Nm(-G), Nm(0)));
    }
}
