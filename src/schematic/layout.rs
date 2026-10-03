//! Automatic schematic layout. See the module documentation of [`crate::schematic`].
//!
//! The layout is built in groups ("items"), each in its own coordinates with pins on the grid:
//! one per anchor (box symbol) with what attaches to it, one per leftover chain of two-terminal
//! parts, and one framed item per block instance holding that instance's groups. Inside a group,
//! every element is checked against the shapes already placed ([`Sketch`]): boxes must not
//! overlap, wires must not cross boxes or other wires, and a wire may only touch connection
//! points of its own net. Items are then packed onto sheets ([`super::pack`]).

use std::collections::{BTreeMap, BTreeSet};

use super::pack;
use super::shapes::{Rect, label_rect, nm, on_seg, rects_overlap, seg_hits_rect, segs_cross, symbol_rects, union};
use super::symbol::{pin_ends, rot, symbol_of};
use super::{Dir, Frame, GRID, Hints, Label, LabelKind, Placement, SheetLayout};
use crate::geom::Point;
use crate::model::Project;
use crate::model::circuit::PinRef;
use crate::model::part::{Category, ParamValue, PinKind, Side, Symbol, SymbolStyle};
use crate::model::sections::natural_cmp;
use crate::render::font;
use crate::symbolgen::is_ground;
use crate::units::Nm;

const G: i64 = GRID.0;
/// Text size used for labels (mm).
const TEXT: f64 = 1.27;
/// How far out (grid steps) attachments are tried before falling back to labels.
const REACH: i64 = 24;
/// Clearance kept around designators and values (nm).
const TEXT_GAP: i64 = 500_000;
/// Space between packed groups (grid steps).
const GAP: i64 = 2;
/// Sheet margin around the drawing area.
const MARGIN: i64 = 4 * G;

fn up_grid(v: i64) -> i64 {
    v.div_euclid(G) * G + if v.rem_euclid(G) == 0 { 0 } else { G }
}

fn down_grid(v: i64) -> i64 {
    v.div_euclid(G) * G
}

fn add(p: Point, d: Dir, len: i64) -> Point {
    let (dx, dy) = d.vec();
    Point::new(Nm(p.x.0 + dx * len), Nm(p.y.0 + dy * len))
}

/// Places a two-terminal symbol so that pin `near` sits at `target` and the body extends along
/// `d`. Returns the placement, the far pin's end and the far pin's number.
fn place_inline(sym: &Symbol, near: &str, target: Point, d: Dir) -> (Placement, Point, String) {
    let pin = sym.pins.iter().find(|p| p.number == near).expect("pin exists");
    let far = sym.pins.iter().find(|p| p.number != near).expect("two pins");
    let side = |s: Side| match s {
        Side::Left => Dir::Left,
        Side::Right => Dir::Right,
        Side::Top => Dir::Up,
        Side::Bottom => Dir::Down,
    };
    let r = (0..4).find(|r| side(pin.side.expect("generated")).rotated(*r) == d.opposite()).expect("some rotation");
    let n = pin.at.expect("generated");
    let (nx, ny) = rot((n.x.0, n.y.0), r);
    let center = Point::new(Nm(target.x.0 - nx), Nm(target.y.0 - ny));
    let f = far.at.expect("generated");
    let (fx, fy) = rot((f.x.0, f.y.0), r);
    (Placement { at: center, rot: r }, Point::new(Nm(center.x.0 + fx), Nm(center.y.0 + fy)), far.number.clone())
}

// ---- circuit view ----

struct Comp {
    sym: Symbol,
    two_terminal: bool,
    value: String,
    block: Option<String>,
    /// Capacitance in farads, for capacitors.
    farads: Option<f64>,
}

struct NetInfo<'a> {
    pins: Vec<&'a PinRef>,
    power: Option<LabelKind>,
}

struct Ctx<'a> {
    comps: BTreeMap<String, Comp>,
    nets: BTreeMap<&'a str, NetInfo<'a>>,
    pin_net: BTreeMap<&'a PinRef, &'a str>,
    no_connect: &'a BTreeSet<PinRef>,
}

impl<'a> Ctx<'a> {
    fn new(p: &'a Project) -> Ctx<'a> {
        let c = p.circuit();
        let lib = p.library();
        let mut comps = BTreeMap::new();
        for (r, comp) in &c.components {
            let Some(part) = lib.parts.get(&comp.part) else { continue };
            let sym = symbol_of(part);
            let two_terminal = sym.style != SymbolStyle::Box && sym.pins.len() == 2;
            let farads = match part.params.0.get("capacitance") {
                Some(ParamValue::Quantity(q)) if part.category == Category::Capacitor => Some(q.to_f64()),
                _ => None,
            };
            comps.insert(r.clone(), Comp { sym, two_terminal, value: part.value(), block: comp.block.clone(), farads });
        }
        let kind_of = |pin: &PinRef| -> Option<(PinKind, String)> {
            let comp = c.components.get(&pin.refdes)?;
            let sp = lib.parts.get(&comp.part)?.symbol.pins.iter().find(|s| s.number == pin.pin)?;
            Some((sp.kind, sp.label().to_string()))
        };
        let mut nets = BTreeMap::new();
        for (name, n) in &c.nets {
            let kinds: Vec<(PinKind, String)> = n.pins.iter().filter_map(kind_of).collect();
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
            nets.insert(name.as_str(), NetInfo { pins: n.pins.iter().collect(), power });
        }
        Ctx { comps, nets, pin_net: c.pin_index(), no_connect: &c.no_connect }
    }

    fn net_of(&self, r: &str, pin: &str) -> Option<&'a str> {
        self.pin_net.get(&PinRef::new(r, pin)).copied()
    }

    fn power(&self, net: &str) -> Option<LabelKind> {
        self.nets.get(net).and_then(|n| n.power)
    }

    /// Net key of a pin for connection checks: its net, or a key of its own when unconnected.
    fn key(&self, r: &str, pin: &str) -> String {
        self.net_of(r, pin).map(str::to_string).unwrap_or_else(|| format!("\u{0}{r}.{pin}"))
    }

    /// The other pin of a two-terminal part.
    fn other_pin(&self, r: &str, pin: &str) -> String {
        self.comps[r].sym.pins.iter().find(|p| p.number != pin).map(|p| p.number.clone()).unwrap_or_default()
    }
}

/// Direction of a power symbol at the free end of a part extending along `d`: supplies point up
/// and grounds down beside horizontal parts, vertical parts keep their direction.
fn power_dir(kind: LabelKind, d: Dir) -> Dir {
    match (d.horizontal(), kind) {
        (true, LabelKind::Ground) => Dir::Down,
        (true, _) => Dir::Up,
        (false, _) => d,
    }
}

// ---- occupancy ----

/// Shapes placed so far in a group, and the drawing built with them. Rectangles carry an owner:
/// 0 for permanent ones, otherwise the reserved fallback label of an anchor pin, which an
/// attachment for that pin may replace.
#[derive(Clone, Default)]
struct Sketch {
    rects: Vec<(Rect, usize)>,
    segs: Vec<(Point, Point, String)>,
    pts: Vec<(Point, String)>,
    placements: BTreeMap<String, Placement>,
    wires: Vec<(Point, Point)>,
    labels: Vec<(Label, usize)>,
    no_connects: Vec<Point>,
    frames: Vec<Frame>,
    /// Owner whose reserved shapes are ignored by checks (the pin being attached).
    ignore: usize,
}

impl Sketch {
    fn rect_ok(&self, r: &Rect) -> bool {
        self.rects.iter().all(|(o, own)| (*own != 0 && *own == self.ignore) || !rects_overlap(o, r))
            && self.segs.iter().all(|(a, b, _)| !seg_hits_rect(*a, *b, r))
    }

    fn pt_ok(&self, p: Point, net: &str) -> bool {
        self.segs.iter().all(|(a, b, n)| n == net || !on_seg(p, *a, *b))
            && self.pts.iter().all(|(q, n)| n == net || *q != p)
    }

    fn seg_ok(&self, a: Point, b: Point, net: &str) -> bool {
        self.rects.iter().all(|(o, own)| (*own != 0 && *own == self.ignore) || !seg_hits_rect(a, b, o))
            && self.segs.iter().all(|(c, d, n)| {
                !segs_cross(a, b, *c, *d)
                    && (n == net || !(on_seg(*c, a, b) || on_seg(*d, a, b) || on_seg(a, *c, *d) || on_seg(b, *c, *d)))
            })
            && self.pts.iter().all(|(p, n)| n == net || !on_seg(*p, a, b))
    }

    fn add_rect(&mut self, r: Rect, owner: usize) {
        self.rects.push((r, owner));
    }

    /// Adds a wire if it fits.
    fn wire(&mut self, a: Point, b: Point, net: &str) -> bool {
        if a == b {
            return true;
        }
        if !self.seg_ok(a, b, net) {
            return false;
        }
        self.segs.push((a, b, net.to_string()));
        self.wires.push((a, b));
        true
    }

    /// Adds a symbol if it fits.
    fn symbol(&mut self, ctx: &Ctx, r: &str, pl: Placement) -> bool {
        let comp = &ctx.comps[r];
        let mut rects = symbol_rects(&comp.sym, &pl, r, &comp.value);
        // Keep text of different parts apart so each reads as its own.
        let n = rects.len();
        for t in &mut rects[n - 2..] {
            *t = [t[0] - TEXT_GAP, t[1] - TEXT_GAP, t[2] + TEXT_GAP, t[3] + TEXT_GAP];
        }
        let ends = pin_ends(&comp.sym, &pl);
        if !rects.iter().all(|x| self.rect_ok(x)) || !ends.iter().all(|(n, e, _)| self.pt_ok(*e, &ctx.key(r, n))) {
            return false;
        }
        for x in rects {
            self.add_rect(x, 0);
        }
        for (n, e, _) in ends {
            self.pts.push((e, ctx.key(r, &n)));
        }
        self.placements.insert(r.to_string(), pl);
        true
    }

    /// Adds a label if it fits (or unconditionally with `force`).
    fn label(&mut self, at: Point, dir: Dir, net: &str, kind: LabelKind, owner: usize, force: bool) -> bool {
        let r = label_rect(at, dir, net, kind);
        if !force && (!self.rect_ok(&r) || !self.pt_ok(at, net)) {
            return false;
        }
        self.add_rect(r, owner);
        self.pts.push((at, net.to_string()));
        self.labels.push((Label { at, dir, net: net.to_string(), kind }, owner));
        true
    }

    /// Drops the reserved shapes of `owner` (its attachment replaced them).
    fn release(&mut self, owner: usize) {
        self.rects.retain(|(_, o)| *o != owner);
        self.labels.retain(|(_, o)| *o != owner);
    }

    /// Bounding box of everything drawn.
    fn bbox(&self) -> Option<Rect> {
        let mut b: Option<Rect> = None;
        let mut grow = |r: Rect| b = Some(b.map_or(r, |x| union(x, r)));
        for (r, _) in &self.rects {
            grow(*r);
        }
        for (a, c, _) in &self.segs {
            grow([a.x.0.min(c.x.0), a.y.0.min(c.y.0), a.x.0.max(c.x.0), a.y.0.max(c.y.0)]);
        }
        for p in &self.no_connects {
            grow([p.x.0 - nm(0.8), p.y.0 - nm(0.8), p.x.0 + nm(0.8), p.y.0 + nm(0.8)]);
        }
        b
    }

    /// Runs `f` on a copy; keeps the copy if `f` succeeds. A non-zero `ignore` names the pin
    /// whose reserved label the attempt may replace (released on success); nested attempts keep
    /// the outer one.
    fn attempt(&mut self, ignore: usize, f: impl FnOnce(&mut Sketch) -> bool) -> bool {
        let outer = self.ignore;
        let mut t = self.clone();
        if ignore != 0 {
            t.ignore = ignore;
        }
        if f(&mut t) {
            if ignore != 0 {
                t.release(ignore);
            }
            t.ignore = outer;
            *self = t;
            true
        } else {
            false
        }
    }
}

// ---- groups ----

/// A laid-out group in its own coordinates.
#[derive(Clone, Default)]
struct Item {
    placements: BTreeMap<String, Placement>,
    wires: Vec<(Point, Point)>,
    labels: Vec<Label>,
    no_connects: Vec<Point>,
    frames: Vec<Frame>,
    bbox: Rect,
    /// Items with the same group (the main circuit's groups) are kept on one sheet if possible.
    group: usize,
}

impl Item {
    fn from_sketch(s: Sketch) -> Option<Item> {
        let bbox = s.bbox()?;
        Some(Item {
            placements: s.placements,
            wires: s.wires,
            labels: s.labels.into_iter().map(|(l, _)| l).collect(),
            no_connects: s.no_connects,
            frames: s.frames,
            bbox,
            group: 0,
        })
    }

    fn shifted(&self, o: (i64, i64)) -> Item {
        let sh = |p: Point| Point::new(Nm(p.x.0 + o.0), Nm(p.y.0 + o.1));
        Item {
            placements: self
                .placements
                .iter()
                .map(|(r, pl)| (r.clone(), Placement { at: sh(pl.at), rot: pl.rot }))
                .collect(),
            wires: self.wires.iter().map(|(a, b)| (sh(*a), sh(*b))).collect(),
            labels: self.labels.iter().map(|l| Label { at: sh(l.at), ..l.clone() }).collect(),
            no_connects: self.no_connects.iter().map(|p| sh(*p)).collect(),
            frames: self
                .frames
                .iter()
                .map(|f| Frame { title: f.title.clone(), min: sh(f.min), max: sh(f.max) })
                .collect(),
            bbox: [self.bbox[0] + o.0, self.bbox[1] + o.1, self.bbox[2] + o.0, self.bbox[3] + o.1],
            group: self.group,
        }
    }

    fn merge(&mut self, o: Item) {
        self.placements.extend(o.placements);
        self.wires.extend(o.wires);
        self.labels.extend(o.labels);
        self.no_connects.extend(o.no_connects);
        self.frames.extend(o.frames);
        self.bbox = union(self.bbox, o.bbox);
    }

    /// Size on the grid (with rounding slack and the gap to neighbours).
    fn grid_size(&self) -> (i64, i64) {
        let b = self.bbox;
        ((b[2] - b[0] + G - 1) / G + 1 + GAP, (b[3] - b[1] + G - 1) / G + 1 + GAP)
    }
}

/// Two-terminal parts still to place in the current section.
type Pool = BTreeSet<String>;

/// Parts to attach, with the pin that faces the anchor: (refdes, pin).
type Parts = Vec<(String, String)>;

/// Pins of one side: position along it, end, and the supply net if it is a power pin.
type SidePins<'a> = Vec<(i64, Point, Option<&'a str>)>;

/// The signal pin being attached.
struct PinAt<'s> {
    owner: usize,
    refdes: &'s str,
    num: &'s str,
    end: Point,
    dir: Dir,
    net: &'s str,
}

/// A chain of two-terminal parts from an anchor pin through two-pin nets: (refdes, near pin).
fn chain_from(ctx: &Ctx, pool: &Pool, first: (String, String)) -> Vec<(String, String)> {
    let mut out = vec![first];
    while out.len() < 4 {
        let (cur, near) = out.last().expect("not empty").clone();
        let far = ctx.other_pin(&cur, &near);
        let Some(n2) = ctx.net_of(&cur, &far) else { break };
        let info = &ctx.nets[n2];
        if info.power.is_some() || info.pins.len() != 2 {
            break;
        }
        let Some(next) = info.pins.iter().find(|p| p.refdes != cur) else { break };
        if !pool.contains(&next.refdes) || out.iter().any(|(r, _)| *r == next.refdes) {
            break;
        }
        out.push((next.refdes.clone(), next.pin.clone()));
    }
    out
}

/// Places the parts of a chain in a line from `start` along `d`, then the label at its far end.
fn place_chain(ctx: &Ctx, t: &mut Sketch, chain: &[(String, String)], start: Point, d: Dir) -> bool {
    let mut target = start;
    let mut last: Option<(Point, String, String)> = None;
    for (r, near) in chain {
        if let Some((far_end, prev, far_pin)) = &last {
            target = add(*far_end, d, 2 * G);
            if !t.wire(*far_end, target, &ctx.key(prev, far_pin)) {
                return false;
            }
        }
        let (pl, far_end, far_pin) = place_inline(&ctx.comps[r].sym, near, target, d);
        if !t.symbol(ctx, r, pl) {
            return false;
        }
        last = Some((far_end, r.clone(), far_pin));
    }
    let Some((end, r, pin)) = last else { return true };
    end_label(ctx, t, &r, &pin, end, d)
}

/// Label at the free end of a placed part pointing along `d`.
fn end_label(ctx: &Ctx, t: &mut Sketch, r: &str, pin: &str, end: Point, d: Dir) -> bool {
    match ctx.net_of(r, pin) {
        Some(net) => match ctx.power(net) {
            // A sideways power symbol may need a short wire to clear the part's own text.
            Some(kind) => (0..3).any(|k| {
                let at = add(end, d, k * G);
                t.attempt(0, |t2| t2.wire(end, at, net) && t2.label(at, power_dir(kind, d), net, kind, 0, false))
            }),
            None => t.label(end, d, net, LabelKind::Net, 0, false),
        },
        None => {
            if ctx.no_connect.contains(&PinRef::new(r, pin)) {
                t.no_connects.push(end);
            }
            true
        }
    }
}

/// Attaches series parts (a chain) and rail parts (branches to a supply) to an anchor pin.
fn attach(ctx: &Ctx, sk: &mut Sketch, p: &PinAt, chain: &[(String, String)], rails: &[(String, String)]) -> bool {
    let net = p.net;
    let mut drawn: BTreeSet<PinRef> = BTreeSet::new();
    drawn.insert(PinRef::new(p.refdes, p.num));
    for (r, near) in chain.iter().take(1).chain(rails) {
        drawn.insert(PinRef::new(r.clone(), near.clone()));
    }
    let needs_label = ctx.nets[net].pins.iter().any(|q| !drawn.contains(*q));
    let at = |t: i64| add(p.end, p.dir, t);
    let tries = if needs_label { 10 } else { 1 };
    for k in 0..tries {
        let ok = sk.attempt(p.owner, |t| {
            let mut prev = p.end;
            let mut tmin = 2 * G;
            if needs_label {
                let la = G / 2 + k * G;
                if !t.label(at(la), p.dir, net, LabelKind::Wire, 0, false) {
                    return false;
                }
                tmin = tmin.max(up_grid(la + nm(font::width(net, TEXT) + 0.8)));
            }
            for (r, near) in rails {
                let far = ctx.other_pin(r, near);
                let kind = ctx.net_of(r, &far).and_then(|n| ctx.power(n)).unwrap_or(LabelKind::Power);
                let bdir = match (p.dir.horizontal(), kind) {
                    (true, LabelKind::Ground) => Dir::Down,
                    (true, _) => Dir::Up,
                    (false, LabelKind::Ground) => Dir::Left,
                    (false, _) => Dir::Right,
                };
                let mut placed = false;
                for s in 0..REACH {
                    let tt = tmin + s * G;
                    let w = at(tt);
                    let ok = t.attempt(0, |t2| {
                        let (pl, far_end, far_pin) = place_inline(&ctx.comps[r].sym, near, w, bdir);
                        t2.wire(prev, w, net) && t2.symbol(ctx, r, pl) && end_label(ctx, t2, r, &far_pin, far_end, bdir)
                    });
                    if ok {
                        prev = w;
                        tmin = tt + G;
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    return false;
                }
            }
            if chain.is_empty() {
                return true;
            }
            (0..REACH).any(|s| {
                let tt = tmin + s * G;
                t.attempt(0, |t2| t2.wire(prev, at(tt), net) && place_chain(ctx, t2, chain, at(tt), p.dir))
            })
        });
        if ok {
            return true;
        }
    }
    false
}

/// Places a two-terminal part (a crystal) between two pins on one side of the anchor, with
/// optional load capacitors to a supply continuing outward from its pins.
fn bridge(
    ctx: &Ctx,
    sk: &mut Sketch,
    pa: &PinAt,
    pb: &PinAt,
    x: &str,
    caps: &[Option<(String, String)>; 2],
    variant: usize,
) -> bool {
    let d = pa.dir;
    let along = |p: Point| if d.horizontal() { p.y.0 } else { p.x.0 };
    let (hi, lo, cap_hi, cap_lo) =
        if along(pa.end) > along(pb.end) { (pa, pb, &caps[0], &caps[1]) } else { (pb, pa, &caps[1], &caps[0]) };
    let (s_hi, s_lo) = (along(hi.end), along(lo.end));
    // The crystal's top pin on the upper pin's row (its wire runs straight), else centered on
    // the two pins.
    let mut tops = vec![up_grid((s_hi + s_lo + 3 * G).div_euclid(2))];
    if s_hi - 3 * G <= s_lo && tops[0] != s_hi {
        tops.insert(0, s_hi);
    }
    let down = if d.horizontal() { Dir::Down } else { Dir::Left };
    let pt = |t: i64, s: i64| {
        let base = add(hi.end, d, t);
        if d.horizontal() { Point::new(base.x, Nm(s)) } else { Point::new(Nm(s), base.y) }
    };
    let x_near =
        ctx.comps[x].sym.pins.iter().find(|q| ctx.net_of(x, &q.number) == Some(hi.net)).map(|q| q.number.clone());
    let Some(x_near) = x_near else { return false };
    // Wires run out from the pins to a column `j`, turn to the crystal's pin rows (one up, one
    // down) and reach its pins from the side; the crystal stands 2 grid steps further out.
    // The preferred variant first.
    let n = tops.len();
    tops.rotate_left(variant % n);
    for top in tops {
        let bottom = top - 3 * G;
        for c in 4..REACH {
            let c = c * G;
            let j = c - 2 * G;
            let ok = sk.attempt(hi.owner, |t| {
                // Both pins' reserved labels go away.
                t.rects.retain(|(_, o)| *o != lo.owner);
                t.labels.retain(|(_, o)| *o != lo.owner);
                let (pl, _, _) = place_inline(&ctx.comps[x].sym, &x_near, pt(c, top), down);
                if !(t.wire(hi.end, pt(j, s_hi), hi.net)
                    && t.wire(pt(j, s_hi), pt(j, top), hi.net)
                    && t.wire(pt(j, top), pt(c, top), hi.net)
                    && t.wire(lo.end, pt(j, s_lo), lo.net)
                    && t.wire(pt(j, s_lo), pt(j, bottom), lo.net)
                    && t.wire(pt(j, bottom), pt(c, bottom), lo.net)
                    && t.symbol(ctx, x, pl))
                {
                    return false;
                }
                // Each load capacitor continues outward from its crystal pin, as close as fits.
                [(cap_hi, top, hi.net), (cap_lo, bottom, lo.net)].into_iter().all(|(cap, row, net)| {
                    let Some((r, near)) = cap else { return true };
                    (2..10).any(|cc| {
                        let target = pt(c + cc * G, row);
                        let (pl, far_end, far_pin) = place_inline(&ctx.comps[r].sym, near, target, d);
                        t.attempt(0, |t2| {
                            t2.wire(pt(c, row), target, net)
                                && t2.symbol(ctx, r, pl)
                                && end_label(ctx, t2, r, &far_pin, far_end, d)
                        })
                    })
                })
            });
            if ok {
                return true;
            }
        }
    }
    false
}

/// Decoupling capacitors (both pins on supplies) as rows on shared rail wires under the anchor.
fn decap_rows(ctx: &Ctx, sk: &mut Sketch, anchor: &str, caps: &[String]) {
    // Group by (top net, bottom net): the supply on top, ground (or the other supply) below.
    let mut groups: BTreeMap<(String, String), Vec<(String, String)>> = BTreeMap::new();
    for r in caps {
        let pins: Vec<String> = ctx.comps[r].sym.pins.iter().map(|p| p.number.clone()).collect();
        let (Some(n0), Some(n1)) = (ctx.net_of(r, &pins[0]), ctx.net_of(r, &pins[1])) else { continue };
        let g0 = ctx.power(n0) == Some(LabelKind::Ground);
        let g1 = ctx.power(n1) == Some(LabelKind::Ground);
        let first_top = if g0 != g1 { g1 } else { natural_cmp(n0, n1).is_le() };
        let (top_pin, top, bottom) = if first_top { (&pins[0], n0, n1) } else { (&pins[1], n1, n0) };
        groups.entry((top.to_string(), bottom.to_string())).or_default().push((r.clone(), top_pin.clone()));
    }
    let Some(bb) = sk.bbox() else { return };
    let anchor_x0 =
        sk.placements.get(anchor).map_or(bb[0], |pl| super::shapes::body_rects(&ctx.comps[anchor].sym, pl)[0][0]);
    let mut y = down_grid(bb[1] - nm(4.2));
    let mut x = down_grid(anchor_x0);
    for ((top, bottom), members) in groups {
        let (tk, bk) = (ctx.power(&top).unwrap_or(LabelKind::Power), ctx.power(&bottom).unwrap_or(LabelKind::Power));
        let mut prev: Option<(Point, Point)> = None;
        for (r, top_pin) in &members {
            let mut placed = false;
            'search: for dy in 0..6 {
                for s in 0..60 {
                    let target = Point::new(Nm(x + s * G), Nm(y - dy * G));
                    let ok = sk.attempt(0, |t| {
                        let (pl, far_end, _) = place_inline(&ctx.comps[r].sym, top_pin, target, Dir::Down);
                        if !t.symbol(ctx, r, pl) {
                            return false;
                        }
                        match prev {
                            Some((pt, pb)) => {
                                pt.y == target.y && t.wire(pt, target, &top) && t.wire(pb, far_end, &bottom)
                            }
                            None => {
                                t.label(target, Dir::Up, &top, tk, 0, false)
                                    && t.label(far_end, Dir::Down, &bottom, bk, 0, false)
                            }
                        }
                    });
                    if ok {
                        let (_, far_end, _) = place_inline(&ctx.comps[r].sym, top_pin, target, Dir::Down);
                        prev = Some((target, far_end));
                        x = target.x.0 + 2 * G;
                        y = target.y.0;
                        placed = true;
                        break 'search;
                    }
                }
                if prev.is_some() {
                    break;
                }
            }
            if !placed {
                // Stand-alone: labels on both pins, further right.
                prev = None;
            }
        }
        x += 2 * G;
    }
}

/// Lays out an anchor and everything that attaches to it. Takes the attached parts out of `pool`.
fn anchor_item(ctx: &Ctx, a: &str, pool: &mut Pool, decaps: &[String]) -> Sketch {
    // Crystal placements interact with the other pins' parts: try each, keep the one that
    // keeps crystals with their load capacitors, then attaches the most parts, then the most
    // compact.
    let mut best: Option<((usize, usize, i64), Pool, Sketch)> = None;
    for variant in 0..2 {
        let mut p = pool.clone();
        let (sk, missed) = anchor_variant(ctx, a, &mut p, decaps, variant);
        let area = sk.bbox().map_or(0, |b| (b[2] - b[0]) / G * ((b[3] - b[1]) / G));
        let score = (missed, p.len(), area);
        if best.as_ref().is_none_or(|(s, _, _)| score < *s) {
            best = Some((score, p, sk));
        }
    }
    let (_, p, sk) = best.expect("two variants");
    *pool = p;
    sk
}

/// One layout of an anchor; also returns how many crystal-group parts were left out.
fn anchor_variant(ctx: &Ctx, a: &str, pool: &mut Pool, decaps: &[String], variant: usize) -> (Sketch, usize) {
    let mut missed = 0;
    let mut sk = Sketch::default();
    let apl = Placement { at: Point::ORIGIN, rot: 0 };
    sk.symbol(ctx, a, apl);
    let ends = pin_ends(&ctx.comps[a].sym, &apl);

    // Power pins: consecutive pins of one net on a side share one symbol on a short bus.
    let mut sides: BTreeMap<Dir, SidePins> = BTreeMap::new();
    let mut signals: Vec<(usize, String, Point, Dir, &str)> = Vec::new();
    for (i, (num, end, d)) in ends.iter().enumerate() {
        let net = ctx.net_of(a, num);
        let kind = net.and_then(|n| ctx.power(n));
        let along = if d.horizontal() { end.y.0 } else { end.x.0 };
        sides.entry(*d).or_default().push((along, *end, if kind.is_some() { net } else { None }));
        match (net, kind) {
            (None, _) => {
                if ctx.no_connect.contains(&PinRef::new(a, num.as_str())) {
                    sk.no_connects.push(*end);
                }
            }
            (Some(_), Some(_)) => {}
            (Some(n), None) => {
                sk.label(*end, *d, n, LabelKind::Net, i + 1, true);
                signals.push((i + 1, num.clone(), *end, *d, n));
            }
        }
    }
    let mut runs: Vec<(Dir, Vec<Point>, &str)> = Vec::new();
    for (d, mut pins) in sides {
        pins.sort_by_key(|p| p.0);
        let mut cur: Option<(Vec<Point>, &str)> = None;
        for (_, end, net) in pins {
            match (net, &mut cur) {
                (Some(n), Some((v, cn))) if *cn == n => v.push(end),
                (Some(n), _) => {
                    if let Some((v, cn)) = cur.take() {
                        runs.push((d, v, cn));
                    }
                    cur = Some((vec![end], n));
                }
                // A signal pin breaks a run; unconnected pins do not.
                (None, _) => {
                    if signals.iter().any(|s| s.2 == end)
                        && let Some((v, cn)) = cur.take()
                    {
                        runs.push((d, v, cn));
                    }
                }
            }
        }
        if let Some((v, cn)) = cur {
            runs.push((d, v, cn));
        }
    }
    let mut attach_points = Vec::new();
    for (d, pins, net) in &runs {
        if pins.len() == 1 {
            attach_points.push((*d, pins[0], *net));
            continue;
        }
        let tops: Vec<Point> = pins.iter().map(|p| add(*p, *d, G)).collect();
        for (p, t) in pins.iter().zip(&tops) {
            sk.segs.push((*p, *t, net.to_string()));
            sk.wires.push((*p, *t));
        }
        for w in tops.windows(2) {
            sk.segs.push((w[0], w[1], net.to_string()));
            sk.wires.push((w[0], w[1]));
        }
        attach_points.push((*d, tops[(tops.len() - 1) / 2], *net));
    }
    attach_points.sort_by_key(|(d, p, _)| (*d, p.x.0));
    for (d, p, net) in attach_points {
        let kind = ctx.power(net).unwrap_or(LabelKind::Power);
        let placed = (0..8).any(|level| {
            let at = add(p, d, level * 2 * G);
            sk.attempt(0, |t| t.wire(p, at, net) && t.label(at, d, net, kind, 0, false))
        });
        if !placed {
            sk.label(p, d, net, kind, 0, true);
        }
    }

    // Crystals between two pins of one side.
    let mut done: BTreeSet<usize> = BTreeSet::new();
    for i in 0..signals.len() {
        for j in 0..signals.len() {
            let (si, sj) = (&signals[i], &signals[j]);
            if i == j || done.contains(&si.0) || done.contains(&sj.0) || si.3 != sj.3 || si.4 == sj.4 {
                continue;
            }
            let dist = (si.2.x.0 - sj.2.x.0).abs() + (si.2.y.0 - sj.2.y.0).abs();
            if dist > 4 * G {
                continue;
            }
            let x = pool.iter().find(|r| {
                let pins = &ctx.comps[*r].sym.pins;
                let nets: BTreeSet<Option<&str>> = pins.iter().map(|q| ctx.net_of(r, &q.number)).collect();
                nets.contains(&Some(si.4)) && nets.contains(&Some(sj.4)) && nets.len() == 2
            });
            let Some(x) = x.cloned() else { continue };
            let cap_for = |net: &str| -> Option<(String, String)> {
                pool.iter().filter(|r| **r != x).find_map(|r| {
                    let pins = &ctx.comps[r].sym.pins;
                    let near = pins.iter().find(|q| ctx.net_of(r, &q.number) == Some(net))?;
                    let far = ctx.net_of(r, &ctx.other_pin(r, &near.number))?;
                    ctx.power(far).map(|_| (r.clone(), near.number.clone()))
                })
            };
            let caps = [cap_for(si.4), cap_for(sj.4)];
            let pa = PinAt { owner: si.0, refdes: a, num: &si.1, end: si.2, dir: si.3, net: si.4 };
            let pb = PinAt { owner: sj.0, refdes: a, num: &sj.1, end: sj.2, dir: sj.3, net: sj.4 };
            let ok = bridge(ctx, &mut sk, &pa, &pb, &x, &caps, variant)
                || bridge(ctx, &mut sk, &pa, &pb, &x, &[None, None], variant);
            let group = 1 + caps.iter().flatten().count();
            if ok {
                pool.remove(&x);
                for (r, _) in caps.iter().flatten() {
                    if sk.placements.contains_key(r) {
                        pool.remove(r);
                    } else {
                        missed += 1;
                    }
                }
                done.insert(si.0);
                done.insert(sj.0);
            } else {
                missed += group;
            }
        }
    }

    // Series parts and rail parts on the other signal pins. Branches toward ground hang down:
    // those pins go bottom-up (and branches toward a supply top-down), so a later pin's wire runs
    // past the earlier branches instead of across them.
    let hangs_down = |net: &str| {
        let mut kinds = ctx.nets[net]
            .pins
            .iter()
            .filter(|q| pool.contains(&q.refdes))
            .filter_map(|q| ctx.net_of(&q.refdes, &ctx.other_pin(&q.refdes, &q.pin)).and_then(|n| ctx.power(n)));
        let first = kinds.next();
        first == Some(LabelKind::Ground) && kinds.all(|k| k == LabelKind::Ground)
    };
    let mut order: Vec<usize> = (0..signals.len()).filter(|i| !done.contains(&signals[*i].0)).collect();
    order.sort_by_key(|&i| {
        let (_, _, end, d, net) = &signals[i];
        let along = if d.horizontal() { end.y.0 } else { end.x.0 };
        let down = hangs_down(net);
        (*d, down, if down { along } else { -along })
    });
    for i in order {
        let (owner, num, end, dir, net) = &signals[i];
        let mut chain_first = None;
        let mut rails = Vec::new();
        for q in &ctx.nets[net].pins {
            if !pool.contains(&q.refdes) || rails.iter().any(|(r, _)| *r == q.refdes) {
                continue;
            }
            let far = ctx.other_pin(&q.refdes, &q.pin);
            match ctx.net_of(&q.refdes, &far) {
                Some(n) if n == *net => {}
                Some(n) if ctx.power(n).is_some() => {
                    if rails.len() < 4 {
                        rails.push((q.refdes.clone(), q.pin.clone()));
                    }
                }
                _ => {
                    if chain_first.is_none() {
                        chain_first = Some((q.refdes.clone(), q.pin.clone()));
                    }
                }
            }
        }
        let chain = chain_first.map(|f| chain_from(ctx, pool, f)).unwrap_or_default();
        if chain.is_empty() && rails.is_empty() {
            continue;
        }
        let p = PinAt { owner: *owner, refdes: a, num, end: *end, dir: *dir, net };
        // Options: everything (with a lone supply part inline at the end of the wire, or as a
        // branch), then subsets. The one placing the most parts wins, then the most compact.
        let mut options: Vec<(Parts, Parts)> = Vec::new();
        if chain.is_empty() && !rails.is_empty() {
            let last = rails.len() - 1;
            options.push((vec![rails[last].clone()], rails[..last].to_vec()));
        }
        options.push((chain.clone(), rails.clone()));
        options.push((chain.clone(), Vec::new()));
        options.push((Vec::new(), rails.clone()));
        options.push((Vec::new(), rails.iter().take(1).cloned().collect()));
        let mut best: Option<((usize, i64), Sketch, Vec<String>)> = None;
        for (c, r) in options {
            if c.is_empty() && r.is_empty() {
                continue;
            }
            let n = c.len() + r.len();
            if best.as_ref().is_some_and(|((m, _), _, _)| usize::MAX - m > n) {
                continue;
            }
            let mut t = sk.clone();
            if !attach(ctx, &mut t, &p, &c, &r) {
                continue;
            }
            let area = t.bbox().map_or(0, |b| ((b[2] - b[0]) / G) * ((b[3] - b[1]) / G));
            let score = (usize::MAX - n, area);
            if best.as_ref().is_none_or(|(s, _, _)| score < *s) {
                best = Some((score, t, c.iter().chain(&r).map(|(x, _)| x.clone()).collect()));
            }
        }
        if let Some((_, t, used)) = best {
            sk = t;
            for x in used {
                pool.remove(&x);
            }
        }
    }

    let caps: Vec<String> = decaps.iter().filter(|r| pool.contains(*r)).cloned().collect();
    decap_rows(ctx, &mut sk, a, &caps);
    for r in &caps {
        if sk.placements.contains_key(r) {
            pool.remove(r);
        }
    }
    (sk, missed)
}

/// Leftover two-terminal parts as vertical chains joined through two-pin nets: supply or signal
/// on top, ground at the bottom.
fn loose_chains(ctx: &Ctx, pool: &Pool) -> Vec<Sketch> {
    // Links through two-pin signal nets between pool parts.
    let mut links: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new(); // r → (pin, other r, other pin)
    for info in ctx.nets.values() {
        if info.power.is_some() || info.pins.len() != 2 {
            continue;
        }
        let (a, b) = (info.pins[0], info.pins[1]);
        if a.refdes != b.refdes && pool.contains(&a.refdes) && pool.contains(&b.refdes) {
            links.entry(a.refdes.clone()).or_default().push((a.pin.clone(), b.refdes.clone(), b.pin.clone()));
            links.entry(b.refdes.clone()).or_default().push((b.pin.clone(), a.refdes.clone(), a.pin.clone()));
        }
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut parts: Vec<&String> = pool.iter().collect();
    parts.sort_by(|a, b| {
        let deg = |r: &String| links.get(r).map_or(0, Vec::len).min(2);
        deg(a).cmp(&deg(b)).then_with(|| natural_cmp(a, b))
    });
    let mut out = Vec::new();
    for start in parts {
        if seen.contains(start) {
            continue;
        }
        // Walk: (refdes, entry pin) from the start's free pin.
        let mut chain: Vec<(String, String)> = Vec::new();
        let first_free = {
            let linked: BTreeSet<&str> =
                links.get(start).map(|v| v.iter().map(|l| l.0.as_str()).collect()).unwrap_or_default();
            let pins = &ctx.comps[start].sym.pins;
            pins.iter().find(|p| !linked.contains(p.number.as_str())).unwrap_or(&pins[0]).number.clone()
        };
        let mut cur = (start.clone(), first_free);
        loop {
            seen.insert(cur.0.clone());
            chain.push(cur.clone());
            let exit = ctx.other_pin(&cur.0, &cur.1);
            let next = links
                .get(&cur.0)
                .and_then(|v| v.iter().find(|(pin, other, _)| *pin == exit && !seen.contains(other)))
                .map(|(_, o, op)| (o.clone(), op.clone()));
            match next {
                Some(n) => cur = n,
                None => break,
            }
        }
        // Orientation: ground at the bottom, supplies and signals on top.
        let kind_at = |r: &str, pin: &str| ctx.net_of(r, pin).map(|n| ctx.power(n));
        let top_kind = kind_at(&chain[0].0, &chain[0].1);
        let (lr, lp) = chain.last().expect("one part").clone();
        let bottom_pin = ctx.other_pin(&lr, &lp);
        let bottom_kind = kind_at(&lr, &bottom_pin);
        let flip = top_kind == Some(Some(LabelKind::Ground)) && bottom_kind != Some(Some(LabelKind::Ground))
            || (bottom_kind == Some(Some(LabelKind::Power)) && top_kind != Some(Some(LabelKind::Power)));
        if flip {
            chain = chain.iter().rev().map(|(r, p)| (r.clone(), ctx.other_pin(r, p))).collect();
        }
        let mut sk = Sketch::default();
        let (r0, p0) = &chain[0];
        let top_ok = match ctx.net_of(r0, p0) {
            Some(n) => {
                let kind = ctx.power(n).unwrap_or(LabelKind::Net);
                sk.label(Point::ORIGIN, Dir::Up, n, kind, 0, true)
            }
            None => true,
        };
        let _ = top_ok;
        place_chain(ctx, &mut sk, &chain, Point::ORIGIN, Dir::Down);
        // A chain that could not be drawn in one line (should not happen): force the rest.
        for (r, near) in &chain {
            if !sk.placements.contains_key(r) {
                let (pl, _, _) = place_inline(&ctx.comps[r].sym, near, Point::ORIGIN, Dir::Down);
                sk.placements.insert(r.clone(), pl);
            }
        }
        out.push(sk);
    }
    out
}

/// Assigns decoupling capacitors (both pins on supply/ground nets) to anchors: bulk capacitors
/// (1 µF and more) to the regulator driving their supply, others to the anchor with the most
/// supply pins on it.
fn assign_decaps(ctx: &Ctx, anchors: &[String], pool: &Pool) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in pool {
        let comp = &ctx.comps[r];
        let nets: Vec<Option<&str>> = comp.sym.pins.iter().map(|p| ctx.net_of(r, &p.number)).collect();
        if nets.len() != 2 || nets.iter().any(|n| n.is_none_or(|n| ctx.power(n).is_none())) {
            continue;
        }
        let rails: Vec<&str> =
            nets.iter().flatten().filter(|n| ctx.power(n) == Some(LabelKind::Power)).copied().collect();
        let pins_on = |a: &str, kinds: &[PinKind]| -> usize {
            ctx.comps[a]
                .sym
                .pins
                .iter()
                .filter(|p| kinds.contains(&p.kind) && ctx.net_of(a, &p.number).is_some_and(|n| rails.contains(&n)))
                .count()
        };
        let bulk = comp.farads.is_some_and(|f| f >= 0.99e-6);
        let regulator = anchors.iter().find(|a| pins_on(a, &[PinKind::PowerOut]) > 0);
        let user = anchors
            .iter()
            .filter(|a| pins_on(a, &[PinKind::PowerIn, PinKind::PowerOut]) > 0)
            .max_by_key(|a| (pins_on(a, &[PinKind::PowerIn]), std::cmp::Reverse(anchors.iter().position(|x| x == *a))));
        let pick = if bulk { regulator.or(user) } else { user.or(regulator) };
        if let Some(a) = pick {
            out.entry(a.clone()).or_default().push(r.clone());
        }
    }
    out
}

/// Lays out the components of one section (a block instance or the main circuit).
fn section_items(ctx: &Ctx, refs: &[String]) -> Vec<Item> {
    let mut anchors: Vec<String> = refs.iter().filter(|r| !ctx.comps[*r].two_terminal).cloned().collect();
    anchors
        .sort_by(|a, b| ctx.comps[b].sym.pins.len().cmp(&ctx.comps[a].sym.pins.len()).then_with(|| natural_cmp(a, b)));
    let mut pool: Pool = refs.iter().filter(|r| ctx.comps[*r].two_terminal).cloned().collect();
    let decaps = assign_decaps(ctx, &anchors, &pool);
    let mut items = Vec::new();
    for a in &anchors {
        let sk = anchor_item(ctx, a, &mut pool, decaps.get(a).map_or(&[][..], Vec::as_slice));
        items.extend(Item::from_sketch(sk));
    }
    items.extend(loose_chains(ctx, &pool).into_iter().filter_map(Item::from_sketch));
    items
}

/// Packs items into one, at most about `width` wide (grid steps), with grid-aligned offsets.
fn combine(items: Vec<Item>, width: i64) -> Item {
    let sizes: Vec<(i64, i64)> = items.iter().map(Item::grid_size).collect();
    let w = width.max(sizes.iter().map(|s| s.0).max().unwrap_or(0));
    let spots = pack::pack(&sizes, &[], (w, i64::MAX / 4), None, 1).expect("unbounded height");
    let mut out: Option<Item> = None;
    for (it, (_, gx, gy)) in items.iter().zip(spots) {
        // Top-left of the item at (gx, -gy) grid steps.
        let o = (up_grid(gx * G - it.bbox[0]), -up_grid(gy * G + it.bbox[3]));
        let s = it.shifted(o);
        match &mut out {
            Some(x) => x.merge(s),
            None => out = Some(s),
        }
    }
    out.unwrap_or_default()
}

/// All groups to pack, plus the components placed by hints.
fn plan<'a>(p: &'a Project, hints: &Hints) -> (Ctx<'a>, Vec<Item>) {
    let ctx = Ctx::new(p);
    let mut sections: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
    for (r, comp) in &ctx.comps {
        if !hints.contains_key(r) {
            sections.entry(comp.block.clone()).or_default().push(r.clone());
        }
    }
    let mut items = Vec::new();
    let mut order: Vec<(&Option<String>, &Vec<String>)> = sections.iter().collect();
    order.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => natural_cmp(x, y),
        (x, y) => x.cmp(y),
    });
    for (block, refs) in order {
        let sub = section_items(&ctx, refs);
        let Some(name) = block else {
            items.extend(sub);
            continue;
        };
        if sub.is_empty() {
            continue;
        }
        let area: i64 = sub.iter().map(|i| i.grid_size().0 * i.grid_size().1).sum();
        let width = ((area as f64).sqrt() * 1.4) as i64;
        let mut inner = combine(sub, width);
        let b = inner.bbox;
        let title = match p.circuit().instances.get(name) {
            Some(block) => format!("{name} ({block})"),
            None => name.clone(),
        };
        let min = Point::new(Nm(down_grid(b[0]) - G), Nm(down_grid(b[1]) - G));
        let title_w = nm(font::width(&title, 1.8) + 2.6);
        let max = Point::new(Nm(up_grid(b[2].max(min.x.0 + title_w)) + G), Nm(up_grid(b[3]) + 3 * G));
        inner.frames.push(Frame { title, min, max });
        inner.bbox = union(inner.bbox, [min.x.0, min.y.0, max.x.0, max.y.0]);
        inner.group = items.len() + 1;
        items.push(inner);
    }
    (ctx, items)
}

const PAPERS: [(&str, f64, f64); 5] =
    [("A4", 297.0, 210.0), ("A3", 420.0, 297.0), ("A2", 594.0, 420.0), ("A1", 841.0, 594.0), ("A0", 1189.0, 841.0)];

/// Drawing area of a paper in grid steps, and the title block's area in it.
fn area_of(w: i64, h: i64) -> ((i64, i64), [i64; 4]) {
    let (aw, ah) = ((w - 2 * MARGIN) / G, (h - 2 * MARGIN) / G);
    let bx = (w - nm(95.0) - MARGIN) / G - 1;
    let by = (h - nm(19.0) - MARGIN) / G - 1;
    ((aw, ah), [bx, by, aw + 1, ah + 1])
}

/// Builds sheets from packed items.
fn assemble(
    p: &Project,
    ctx: &Ctx,
    hints: &Hints,
    items: &[Item],
    spots: &[pack::Spot],
    paper: (&str, i64, i64),
) -> Vec<SheetLayout> {
    let (name, w, h) = paper;
    let count = spots.iter().map(|s| s.0 + 1).max().unwrap_or(1);
    let mut sheets: Vec<SheetLayout> = (0..count)
        .map(|i| SheetLayout {
            size: (Nm(w), Nm(h)),
            paper: name.to_string(),
            sheet: i as u32 + 1,
            sheets: count as u32,
            ..Default::default()
        })
        .collect();
    for (it, &(b, gx, gy)) in items.iter().zip(spots) {
        let ox = up_grid(MARGIN + gx * G - it.bbox[0]);
        // KiCad's Y runs down from the top edge: keep h − y on the grid.
        let k =
            (MARGIN + gy * G + it.bbox[3]).div_euclid(G) + i64::from((MARGIN + gy * G + it.bbox[3]).rem_euclid(G) != 0);
        let s = it.shifted((ox, h - k * G));
        let sh = &mut sheets[b];
        sh.placements.extend(s.placements);
        sh.wires.extend(s.wires);
        sh.labels.extend(s.labels);
        sh.no_connects.extend(s.no_connects);
        sh.frames.extend(s.frames);
    }
    // Hinted components on the first sheet: their placement as given, every pin labeled.
    let first = &mut sheets[0];
    for (r, mut pl) in hints.iter().filter(|(r, _)| ctx.comps.contains_key(*r)).map(|(r, p)| (r.clone(), *p)) {
        // Keep pins on the 1.27 mm grid measured from the top edge (KiCad's origin).
        let half = G / 2;
        pl.at.y = Nm(h - ((h - pl.at.y.0) as f64 / half as f64).round() as i64 * half);
        first.placements.insert(r.clone(), pl);
        for (num, end, d) in pin_ends(&ctx.comps[&r].sym, &pl) {
            let pin = PinRef::new(r.clone(), num);
            match ctx.pin_net.get(&pin) {
                Some(net) => {
                    let kind = ctx.power(net).unwrap_or(LabelKind::Net);
                    first.labels.push(Label { at: end, dir: d, net: net.to_string(), kind });
                }
                None if p.circuit().no_connect.contains(&pin) => first.no_connects.push(end),
                None => {}
            }
        }
    }
    sheets
}

/// Lays out the circuit on one sheet, on the smallest paper (A4 to A0, or a custom size) that
/// holds it. The KiCad export uses this.
pub fn layout(p: &Project, hints: &Hints) -> SheetLayout {
    let (ctx, items) = plan(p, hints);
    let sizes: Vec<(i64, i64)> = items.iter().map(Item::grid_size).collect();
    let groups: Vec<usize> = items.iter().map(|i| i.group).collect();
    for (name, w, h) in PAPERS {
        let (w, h) = (nm(w), nm(h));
        let (area, blocked) = area_of(w, h);
        if let Some(spots) = pack::pack(&sizes, &groups, area, Some(blocked), 1) {
            return assemble(p, &ctx, hints, &items, &spots, (name, w, h)).remove(0);
        }
    }
    // Larger than A0: a custom sheet as wide as A0 (or the widest group), as tall as needed.
    let aw = sizes.iter().map(|s| s.0).max().unwrap_or(0).max((nm(1189.0) - 2 * MARGIN) / G);
    let spots = pack::pack(&sizes, &groups, (aw, i64::MAX / 4), None, 1).expect("unbounded height");
    let used = sizes.iter().zip(&spots).map(|(s, sp)| sp.2 + s.1).max().unwrap_or(0);
    let (w, h) = (aw * G + 2 * MARGIN, used * G + 2 * MARGIN + nm(20.0));
    let (h, w) = (up_grid(h), up_grid(w));
    let spots: Vec<pack::Spot> = spots.into_iter().map(|(_, x, y)| (0, x, y)).collect();
    assemble(p, &ctx, hints, &items, &spots, ("User", w, h)).remove(0)
}

/// Lays out the circuit on sheets of at most A3: one A4 or A3 sheet when everything fits,
/// otherwise as many A3 sheets as needed (groups are never split). A group larger than A3 makes
/// all sheets the smallest paper that holds it.
pub fn layout_sheets(p: &Project, hints: &Hints) -> Vec<SheetLayout> {
    let (ctx, items) = plan(p, hints);
    let sizes: Vec<(i64, i64)> = items.iter().map(Item::grid_size).collect();
    let groups: Vec<usize> = items.iter().map(|i| i.group).collect();
    for (name, w, h) in &PAPERS[..2] {
        let (w, h) = (nm(*w), nm(*h));
        let (area, blocked) = area_of(w, h);
        if let Some(spots) = pack::pack(&sizes, &groups, area, Some(blocked), 1) {
            return assemble(p, &ctx, hints, &items, &spots, (name, w, h));
        }
    }
    for (name, w, h) in &PAPERS[1..] {
        let (w, h) = (nm(*w), nm(*h));
        let (area, blocked) = area_of(w, h);
        if let Some(spots) = pack::pack(&sizes, &groups, area, Some(blocked), usize::MAX) {
            return assemble(p, &ctx, hints, &items, &spots, (name, w, h));
        }
    }
    vec![layout(p, hints)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_helpers() {
        assert_eq!(up_grid(1), G);
        assert_eq!(up_grid(G), G);
        assert_eq!(up_grid(-1), 0);
        assert_eq!(down_grid(-1), -G);
        assert_eq!(add(Point::ORIGIN, Dir::Left, G), Point::new(Nm(-G), Nm(0)));
    }
}
