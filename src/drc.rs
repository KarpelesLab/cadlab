//! Design rule check (DRC): the board against its design rules. See `docs/BOARD.md`.
//!
//! [`check`] returns one [`Diagnostic`] per finding, with a stable `drc.<rule>` code, the objects
//! involved, a board location and a fix hint, sorted by code, then location, then message.
//! Rule values come from the net's class (`circuit.netclasses`) where a class sets them, else
//! from the board's rules.
//!
//! Distances are measured exactly between the polygon shapes of `crate::board`. Arcs in those
//! shapes are approximated outward by at most 1 µm, so distance rules accept a deficit of up
//! to [`TOLERANCE`].

use std::collections::BTreeMap;

use polyclip::{Circle, EndCap, FillRule, Geometry, Join, Op, Path, Polygon, PolygonSet, Rect, Ring};

use crate::board::prepared::{self, Prepared};
use crate::board::{self as geo, COPPER_TOL, CopperItem};
use crate::diag::Diagnostic;
use crate::geom::{Point, RTree};
use crate::model::Project;
use crate::model::board::{BoardSide, GraphicKind, Rules};
use crate::model::circuit::NetClass;
use crate::model::footprint::{GraphicGeometry, GraphicLayer, PadKind};
use crate::model::sections::natural_cmp;
use crate::refs::ObjectRef;
use crate::units::Nm;

/// Measurement tolerance of distance rules (two outward arc approximations of 1 µm).
pub const TOLERANCE: Nm = Nm(2_000);

/// Runs every rule on the project's board.
pub fn check(p: &Project) -> Vec<Diagnostic> {
    let ctx = Ctx::new(p);
    let mut out = Vec::new();
    unplaced(&ctx, &mut out);
    let items = geo::copper_items(p);
    let pads = geo::placed_pads(p);
    copper_pairs(&ctx, &items, &mut out);
    track_widths(&ctx, &mut out);
    via_sizes(&ctx, &mut out);
    pad_holes(&ctx, &pads, &mut out);
    hole_to_hole(&ctx, &pads, &mut out);
    let outline = BoardShape::of(p);
    match &outline {
        Some(o) => board_edges(&ctx, &items, o, &mut out),
        None => out.push(
            Diagnostic::error("drc.no_outline", "the board has no outline")
                .with_hint("draw one with board.outline (rect, rounded rect, circle or polygon)"),
        ),
    }
    let courtyards = courtyards(p);
    courtyard_rules(&courtyards, outline.as_ref(), &mut out);
    board_holes(p, &courtyards, outline.as_ref(), &mut out);
    silk_to_pads(&ctx, &pads, &mut out);
    keepouts(&ctx, &courtyards, &mut out);
    unrouted(&items, &mut out);
    out.extend(netclass_conflicts(p));
    out.extend(crate::electrical::current_check(p));
    out.extend(crate::electrical::impedance_check(p));
    out.extend(crate::lengths::checks(p, &items));
    out.sort_by(|a, b| {
        let loc = |d: &Diagnostic| d.location.map(|l| (l.x, l.y));
        (a.code.as_ref(), loc(a), &a.message).cmp(&(b.code.as_ref(), loc(b), &b.message))
    });
    out
}

/// Manufacturing limits only, with `rules` in place of the board's and net class values
/// ignored: clearance (and shorts), track width, hole-to-hole, copper-to-edge (and copper
/// outside the board), silk-to-pad. Same codes as [`check`]; a zero limit is not checked. Used by
/// fab profile checks (`crate::fab::check`), which must not change the project's own rules.
pub fn check_limits(p: &Project, rules: &Rules) -> Vec<Diagnostic> {
    let ctx = Ctx { p, rules, layers: p.board().stackup.copper_names(), classes: false };
    let mut out = Vec::new();
    let items = geo::copper_items(p);
    let pads = geo::placed_pads(p);
    copper_pairs(&ctx, &items, &mut out);
    track_widths(&ctx, &mut out);
    if rules.hole_to_hole > Nm::ZERO {
        hole_to_hole(&ctx, &pads, &mut out);
    }
    if let Some(o) = BoardShape::of(p) {
        board_edges(&ctx, &items, &o, &mut out);
    }
    if rules.silk_to_pad > Nm::ZERO {
        silk_to_pads(&ctx, &pads, &mut out);
    }
    out.sort_by(|a, b| {
        let loc = |d: &Diagnostic| d.location.map(|l| (l.x, l.y));
        (a.code.as_ref(), loc(a), &a.message).cmp(&(b.code.as_ref(), loc(b), &b.message))
    });
    out
}

/// Effective rules.
struct Ctx<'a> {
    p: &'a Project,
    rules: &'a Rules,
    layers: Vec<String>,
    /// Whether net class values override the rules.
    classes: bool,
}

impl<'a> Ctx<'a> {
    fn new(p: &'a Project) -> Self {
        Ctx { p, rules: &p.board().rules, layers: p.board().stackup.copper_names(), classes: true }
    }

    fn class(&self, net: Option<&str>) -> Option<&'a NetClass> {
        if !self.classes {
            return None;
        }
        let c = self.p.circuit();
        net.and_then(|n| c.nets.get(n)).and_then(|n| n.class.as_ref()).and_then(|k| c.netclasses.get(k))
    }

    /// Clearance of a net: its class's, else the rules'.
    fn clearance(&self, net: Option<&str>) -> Nm {
        self.class(net).and_then(|c| c.clearance).unwrap_or(self.rules.clearance)
    }

    /// Bit mask of copper layers.
    fn mask(&self, layers: &[String]) -> u64 {
        layers.iter().filter_map(|l| self.layers.iter().position(|n| n == l)).fold(0, |m, i| m | (1u64 << i.min(63)))
    }

    fn layer_name(&self, mask: u64) -> &str {
        self.layers.get(mask.trailing_zeros() as usize).map(String::as_str).unwrap_or("?")
    }
}

fn pt(p: Point) -> polyclip::Point {
    p.into()
}

fn from_f(x: f64, y: f64) -> Point {
    Point::new(Nm(x.round() as i64), Nm(y.round() as i64))
}

fn nm_f(d: f64) -> Nm {
    Nm(d.round() as i64)
}

/// Subject for a copper item: `U1.3` → pin, `track#4` → item, anything else parsed likewise.
fn item_ref(label: &str) -> ObjectRef {
    ObjectRef::parse(label).unwrap_or_else(|_| ObjectRef::Name(label.to_string()))
}

fn net_label(net: Option<&str>) -> String {
    net.map(|n| format!("net {n}")).unwrap_or_else(|| "no net".into())
}

/// Exact distance between two geometries and the midpoint of the closest points.
fn gap<A: Geometry + ?Sized, B: Geometry + ?Sized>(a: &A, b: &B) -> Option<(f64, Point)> {
    let c = polyclip::distance(a, b)?;
    Some((c.sq.distance_f64(), from_f((c.a.x + c.b.x) / 2.0, (c.a.y + c.b.y) / 2.0)))
}

/// Overlap of two regions wider than [`TOLERANCE`] somewhere (it survives shrinking by half of
/// it on every side): its centroid. Shapes that touch overlap by up to the outward arc
/// approximation (1 µm each) or by rotated-vertex rounding; that is touching, not overlapping.
fn overlap<A, B>(a: &A, b: &B) -> Option<Point>
where
    A: polyclip::RingSource + Geometry + ?Sized,
    B: polyclip::RingSource + Geometry + ?Sized,
{
    if !polyclip::intersects(a, b) {
        return None;
    }
    let inter = polyclip::boolean(Op::Intersection, a, b, FillRule::NonZero).ok()?;
    if polyclip::area2(&inter) <= 0 {
        return None;
    }
    let tol = polyclip::ArcTol::new(1_000, polyclip::Side::Outside);
    let core = polyclip::offset(&inter, -(TOLERANCE.0 / 2), Join::Round, tol).ok()?;
    if core.is_empty() {
        return None;
    }
    polyclip::centroid(&inter).map(|c| from_f(c.x, c.y))
}

// ---------------------------------------------------------------------------------------------
// Spatial index

/// Pairs `(i, j)`, `i < j`, of boxes whose gap is at most `margin` on both axes (a superset of
/// the pairs closer than `margin`), found with a uniform grid. Sorted.
fn near_pairs(boxes: &[Option<Rect>], margin: i64) -> Vec<(usize, usize)> {
    let half = (margin.max(0) + 1) / 2;
    let ex: Vec<(usize, Rect)> = boxes.iter().enumerate().filter_map(|(i, b)| b.map(|b| (i, b.expand(half)))).collect();
    if ex.len() < 2 {
        return Vec::new();
    }
    let all = ex.iter().skip(1).fold(ex[0].1, |acc, (_, b)| acc.union(b));
    let (w, h) = (all.width().max(1) as f64, all.height().max(1) as f64);
    let cell = ((w * h / ex.len() as f64).sqrt().ceil() as i64).max(1);
    let cols = ((w as i64 / cell) + 1).clamp(1, 2048);
    let rows = ((h as i64 / cell) + 1).clamp(1, 2048);
    let cell_x = |x: i64| ((x - all.min.x) / cell).clamp(0, cols - 1);
    let cell_y = |y: i64| ((y - all.min.y) / cell).clamp(0, rows - 1);
    let mut grid: Vec<Vec<u32>> = vec![Vec::new(); (cols * rows) as usize];
    for (k, (_, b)) in ex.iter().enumerate() {
        for cy in cell_y(b.min.y)..=cell_y(b.max.y) {
            for cx in cell_x(b.min.x)..=cell_x(b.max.x) {
                grid[(cy * cols + cx) as usize].push(k as u32);
            }
        }
    }
    let mut out = Vec::new();
    for cy in 0..rows {
        for cx in 0..cols {
            let cell_items = &grid[(cy * cols + cx) as usize];
            for (n, &a) in cell_items.iter().enumerate() {
                let (ia, ba) = &ex[a as usize];
                for &b in &cell_items[n + 1..] {
                    let (ib, bb) = &ex[b as usize];
                    if !ba.intersects(bb) {
                        continue;
                    }
                    // Report each pair once: in the cell holding the overlap's lower-left corner.
                    let (rx, ry) = (ba.min.x.max(bb.min.x), ba.min.y.max(bb.min.y));
                    if cell_x(rx) == cx && cell_y(ry) == cy {
                        out.push(((*ia).min(*ib), (*ia).max(*ib)));
                    }
                }
            }
        }
    }
    out.sort_unstable();
    out
}

// ---------------------------------------------------------------------------------------------
// Rules

fn unplaced(ctx: &Ctx, out: &mut Vec<Diagnostic>) {
    let board = ctx.p.board();
    let mut refs: Vec<&String> = ctx.p.circuit().components.keys().collect();
    refs.sort_by(|a, b| natural_cmp(a, b));
    for r in refs {
        let subject = ObjectRef::Name(r.clone());
        if !board.footprints.contains_key(r) {
            out.push(
                Diagnostic::warning("drc.unplaced", format!("{r} is not placed on the board"))
                    .with_subject(subject)
                    .with_hint("place it with place.set, or place.auto for an initial placement"),
            );
        } else if geo::footprint_for(ctx.p, r).is_none() {
            out.push(
                Diagnostic::warning("drc.no_footprint", format!("{r} has no footprint in the library"))
                    .with_subject(subject)
                    .at(board.footprints[r].at)
                    .with_hint(
                        "give its part a footprint (footprint.generate / part.set) or set the placement's footprint",
                    ),
            );
        }
    }
}

/// Shorts and clearance between copper items of different nets on a shared layer.
fn copper_pairs(ctx: &Ctx, items: &[CopperItem], out: &mut Vec<Diagnostic>) {
    let boxes: Vec<Option<Rect>> = items.iter().map(|it| it.shape.bbox()).collect();
    let masks: Vec<u64> = items.iter().map(|it| ctx.mask(&it.layers)).collect();
    let clear: Vec<i64> = items.iter().map(|it| ctx.clearance(it.net.as_deref()).0).collect();
    let max_clear = clear.iter().copied().max().unwrap_or(0);
    let labels: Vec<String> = items.iter().map(|it| it.item.to_string()).collect();
    // Large shapes (zone fills) are indexed once; queries against them give the same answers.
    let prep: Vec<Option<Prepared>> = items
        .iter()
        .map(|it| {
            (prepared::segment_count(&it.shape) >= prepared::PREPARE_MIN_SEGMENTS).then(|| Prepared::new(&it.shape))
        })
        .collect();
    let close = |i: usize, j: usize, d: i64| match (&prep[i], &prep[j]) {
        (_, Some(pj)) => pj.distance_less_than(&items[i].shape, d),
        (Some(pi), None) => pi.distance_less_than(&items[j].shape, d),
        (None, None) => polyclip::distance_less_than(&items[i].shape, &items[j].shape, d),
    };
    let touch = |i: usize, j: usize| match (&prep[i], &prep[j]) {
        (_, Some(pj)) => pj.intersects(&items[i].shape),
        (Some(pi), None) => pi.intersects(&items[j].shape),
        (None, None) => polyclip::intersects(&items[i].shape, &items[j].shape),
    };
    for (i, j) in near_pairs(&boxes, max_clear) {
        let (a, b) = (&items[i], &items[j]);
        let shared = masks[i] & masks[j];
        if shared == 0 || a.net == b.net {
            continue;
        }
        let mut required = clear[i].max(clear[j]);
        // The two nets of a differential pair may run at the pair's gap.
        if ctx.classes
            && let (Some(na), Some(nb)) = (&a.net, &b.net)
            && let Some(g) = crate::lengths::pair_gap(ctx.p.circuit(), na, nb)
        {
            required = required.min(g.0);
        }
        let (Some(ba), Some(bb)) = (boxes[i], boxes[j]) else { continue };
        if !ba.expand(required).intersects(&bb) {
            continue;
        }
        let limit = (required - TOLERANCE.0).max(1);
        if !close(i, j, limit) {
            continue;
        }
        let layer = ctx.layer_name(shared);
        let (la, lb) = (&labels[i], &labels[j]);
        let mut d = if touch(i, j) {
            let at = gap(&a.shape, &b.shape).map(|g| g.1).unwrap_or(a.anchor);
            let msg = match (&a.net, &b.net) {
                (Some(na), Some(nb)) => format!("short between net {na} ({la}) and net {nb} ({lb}) on {layer}"),
                (Some(n), None) | (None, Some(n)) => {
                    let (with, without) = if a.net.is_some() { (la, lb) } else { (lb, la) };
                    format!("{without} has no net and touches net {n} ({with}) on {layer}")
                }
                (None, None) => unreachable!("same net"),
            };
            Diagnostic::error("drc.short", msg).at(at).with_hint(
                "remove or reroute the copper joining the nets (track.remove / via.remove), or give it the right net",
            )
        } else {
            // Pads of one footprint are spaced by the footprint itself, not by the layout.
            if let (geo::ItemRef::Pad(ra, _), geo::ItemRef::Pad(rb, _)) = (&a.item, &b.item)
                && ra == rb
            {
                continue;
            }
            let (dist, at) = gap(&a.shape, &b.shape).unwrap_or((0.0, a.anchor));
            Diagnostic::error(
                "drc.clearance",
                format!(
                    "{la} ({}) and {lb} ({}) are {} apart on {layer}, clearance is {}",
                    net_label(a.net.as_deref()),
                    net_label(b.net.as_deref()),
                    nm_f(dist),
                    Nm(required)
                ),
            )
            .at(at)
            .with_hint("move or reroute one of them, or lower the net class / rules clearance if the fab allows it")
        };
        d = d.with_subject(item_ref(la)).with_subject(item_ref(lb));
        for n in [&a.net, &b.net].into_iter().flatten() {
            d = d.with_subject(ObjectRef::Net(n.clone()));
        }
        out.push(d);
    }
}

fn track_widths(ctx: &Ctx, out: &mut Vec<Diagnostic>) {
    for t in &ctx.p.board().tracks {
        let mid = from_f((t.start.x.0 as f64 + t.end.x.0 as f64) / 2.0, (t.start.y.0 as f64 + t.end.y.0 as f64) / 2.0);
        let at = t.mid.unwrap_or(mid);
        let subject = ObjectRef::Item { kind: "track".into(), index: t.id.0 };
        let with_net = |d: Diagnostic| match &t.net {
            Some(n) => d.with_subject(ObjectRef::Net(n.clone())),
            None => d,
        };
        let pair_width =
            t.net.as_deref().filter(|_| ctx.classes).and_then(|n| crate::lengths::pair_width(ctx.p.circuit(), n));
        let class_width = pair_width.or_else(|| ctx.class(t.net.as_deref()).and_then(|c| c.track_width));
        if t.width < ctx.rules.min_track_width {
            out.push(with_net(
                Diagnostic::error(
                    "drc.track_width",
                    format!("track#{} is {} wide, minimum is {}", t.id.0, t.width, ctx.rules.min_track_width),
                )
                .with_subject(subject)
                .at(at)
                .with_hint(
                    "remove it and add it again wider (track.add with width), or lower board.rules min_track_width",
                ),
            ));
        } else if let Some(w) = class_width
            && t.width < w
        {
            let net = t.net.as_deref().unwrap_or_default();
            out.push(with_net(
                Diagnostic::warning(
                    "drc.track_width_class",
                    format!("track#{} ({net}) is {} wide, its net class asks for {w}", t.id.0, t.width),
                )
                .with_subject(subject)
                .at(at)
                .with_hint("re-add the track without a width to use the net class width"),
            ));
        }
    }
}

fn via_sizes(ctx: &Ctx, out: &mut Vec<Diagnostic>) {
    for v in &ctx.p.board().vias {
        let subject = || {
            std::iter::once(ObjectRef::Item { kind: "via".into(), index: v.id.0 })
                .chain(v.net.clone().map(ObjectRef::Net))
        };
        if v.drill < ctx.rules.min_drill {
            let mut d = Diagnostic::error(
                "drc.via_drill",
                format!("via#{} drill {} is below the minimum {}", v.id.0, v.drill, ctx.rules.min_drill),
            )
            .at(v.at)
            .with_hint("use a larger drill (via.add drill) or lower board.rules min_drill if the fab allows it");
            d.subjects.extend(subject());
            out.push(d);
        }
        let ring = Nm((v.diameter.0 - v.drill.0) / 2);
        if ring < ctx.rules.min_annular_ring {
            let mut d = Diagnostic::error(
                "drc.via_annular_ring",
                format!(
                    "via#{} annular ring {ring} ({} pad, {} drill) is below the minimum {}",
                    v.id.0, v.diameter, v.drill, ctx.rules.min_annular_ring
                ),
            )
            .at(v.at)
            .with_hint("use a larger via diameter or a smaller drill");
            d.subjects.extend(subject());
            out.push(d);
        }
        if let Some(c) = ctx.class(v.net.as_deref()) {
            let small_drill = c.via_drill.filter(|d| v.drill < *d);
            let small_pad = c.via_diameter.filter(|d| v.diameter < *d);
            if small_drill.is_some() || small_pad.is_some() {
                let net = v.net.as_deref().unwrap_or_default();
                let want = |o: Option<Nm>, have: Nm| o.map_or(have.to_string(), |x| x.to_string());
                let mut d = Diagnostic::warning(
                    "drc.via_size_class",
                    format!(
                        "via#{} ({net}) is {}/{} (pad/drill), its net class asks for {}/{}",
                        v.id.0,
                        v.diameter,
                        v.drill,
                        want(c.via_diameter, v.diameter),
                        want(c.via_drill, v.drill)
                    ),
                )
                .at(v.at)
                .with_hint("remove it and add it again without sizes to use the net class via (via.add)");
                d.subjects.extend(subject());
                out.push(d);
            }
        }
    }
}

/// Net class values below the board's manufacturing minimums (`drc.netclass_rule` warnings):
/// a class track or diff-pair width under `min_track_width`, a via drill under `min_drill`, a
/// via whose annular ring (class or default sizes) is under `min_annular_ring`. Tracks and vias
/// made with such a class would fail the DRC.
pub fn netclass_conflicts(p: &Project) -> Vec<Diagnostic> {
    let r = &p.board().rules;
    let mut out = Vec::new();
    for (name, c) in &p.circuit().netclasses {
        let mut push = |what: String, field: &str, rule: &str| {
            out.push(
                Diagnostic::warning("drc.netclass_rule", format!("net class {name}: {what}"))
                    .with_subject(ObjectRef::Name(name.clone()))
                    .with_hint(format!(
                        "raise its {field} with netclass.set, or lower board.rules {rule} if the fab allows it"
                    )),
            );
        };
        for (label, field, v) in
            [("track width", "track_width", c.track_width), ("diff pair width", "diff_pair_width", c.diff_pair_width)]
        {
            if let Some(v) = v.filter(|v| *v < r.min_track_width) {
                push(
                    format!("{label} {v} is below the minimum track width {}", r.min_track_width),
                    field,
                    "min_track_width",
                );
            }
        }
        if let Some(d) = c.via_drill.filter(|d| *d < r.min_drill) {
            push(format!("via drill {d} is below the minimum drill {}", r.min_drill), "via_drill", "min_drill");
        }
        if c.via_drill.is_some() || c.via_diameter.is_some() {
            let drill = c.via_drill.unwrap_or(r.via_drill);
            let dia = c.via_diameter.unwrap_or(r.via_diameter);
            let ring = Nm((dia.0 - drill.0) / 2);
            if ring < r.min_annular_ring {
                push(
                    format!(
                        "vias of {dia}/{drill} (pad/drill) leave a {ring} annular ring, the minimum is {}",
                        r.min_annular_ring
                    ),
                    "via_diameter",
                    "min_annular_ring",
                );
            }
        }
    }
    out
}

fn pad_subjects(pp: &geo::PlacedPad) -> Vec<ObjectRef> {
    let mut s = vec![ObjectRef::Pin { component: pp.refdes.clone(), pin: pp.number.clone() }];
    s.extend(pp.net.clone().map(ObjectRef::Net));
    s
}

fn pad_label(pp: &geo::PlacedPad) -> String {
    if pp.number.is_empty() { format!("{} hole", pp.refdes) } else { format!("pad {}.{}", pp.refdes, pp.number) }
}

/// THT pad annular ring and drill sizes.
fn pad_holes(ctx: &Ctx, pads: &[geo::PlacedPad], out: &mut Vec<Diagnostic>) {
    for pp in pads {
        let drill = match pp.pad.kind {
            PadKind::Tht { drill } | PadKind::Npth { drill } => drill,
            PadKind::Smd => continue,
        };
        if drill < ctx.rules.min_drill {
            let mut d = Diagnostic::error(
                "drc.pad_drill",
                format!("{} drill {drill} is below the minimum {}", pad_label(pp), ctx.rules.min_drill),
            )
            .at(pp.center)
            .with_hint("use a footprint with a larger hole, or lower board.rules min_drill if the fab allows it");
            d.subjects = pad_subjects(pp);
            out.push(d);
        }
        if matches!(pp.pad.kind, PadKind::Tht { .. }) {
            let (w, h) = pp.pad.shape.size();
            let ring = Nm((w.0.min(h.0) - drill.0) / 2);
            if ring < ctx.rules.min_annular_ring {
                let mut d = Diagnostic::error(
                    "drc.pad_annular_ring",
                    format!(
                        "{} annular ring {ring} ({drill} drill) is below the minimum {}",
                        pad_label(pp),
                        ctx.rules.min_annular_ring
                    ),
                )
                .at(pp.center)
                .with_hint("use a footprint with larger pads or smaller holes");
                d.subjects = pad_subjects(pp);
                out.push(d);
            }
        }
    }
}

/// Edge-to-edge distance between drilled holes (vias, THT pads, NPTH).
fn hole_to_hole(ctx: &Ctx, pads: &[geo::PlacedPad], out: &mut Vec<Diagnostic>) {
    struct Hole {
        at: Point,
        r: i64,
        label: String,
        subjects: Vec<ObjectRef>,
    }
    let mut holes: Vec<Hole> = Vec::new();
    for v in &ctx.p.board().vias {
        holes.push(Hole {
            at: v.at,
            r: v.drill.0 / 2,
            label: format!("via#{}", v.id.0),
            subjects: vec![ObjectRef::Item { kind: "via".into(), index: v.id.0 }],
        });
    }
    for pp in pads {
        if let Some((d, _)) = pp.hole {
            holes.push(Hole { at: pp.center, r: d.0 / 2, label: pad_label(pp), subjects: pad_subjects(pp) });
        }
    }
    let min = ctx.rules.hole_to_hole.0;
    let boxes: Vec<Option<Rect>> = holes.iter().map(|h| Some(Rect::new(pt(h.at), pt(h.at)).expand(h.r))).collect();
    for (i, j) in near_pairs(&boxes, min) {
        let (a, b) = (&holes[i], &holes[j]);
        let (dx, dy) = ((a.at.x.0 - b.at.x.0) as f64, (a.at.y.0 - b.at.y.0) as f64);
        let edge = (dx * dx + dy * dy).sqrt() - (a.r + b.r) as f64;
        if edge >= min as f64 - 0.5 {
            continue;
        }
        let at = from_f((a.at.x.0 + b.at.x.0) as f64 / 2.0, (a.at.y.0 + b.at.y.0) as f64 / 2.0);
        let mut d = Diagnostic::error(
            "drc.hole_to_hole",
            format!(
                "holes of {} and {} are {} apart (edge to edge), minimum is {}",
                a.label,
                b.label,
                nm_f(edge.max(0.0)),
                ctx.rules.hole_to_hole
            ),
        )
        .at(at)
        .with_hint("move the via or footprint so the holes are further apart");
        d.subjects.extend(a.subjects.iter().cloned());
        d.subjects.extend(b.subjects.iter().cloned());
        out.push(d);
    }
}

/// The board outline as polygon rings.
struct BoardShape {
    outer: Ring,
    cutouts: Vec<Ring>,
    /// Every contour as a closed path (the board edges).
    edges: Vec<Path>,
    /// The outer contour, counter-clockwise, when it is a simple convex polygon.
    convex: Option<Ring>,
    /// Boxes of the outer contour's segments.
    outer_index: RTree,
    /// Boxes of every edge segment (outer contour and cutouts).
    edge_index: RTree,
    /// Every outer contour vertex is within `polyclip`'s coordinate range.
    outer_in_range: bool,
}

/// Bounding boxes of a closed ring's segments.
fn segment_boxes(r: &Ring) -> impl Iterator<Item = Option<Rect>> + '_ {
    let v = &r.0;
    (0..v.len()).map(move |i| Some(Rect::new(v[i], v[(i + 1) % v.len()])))
}

/// Even-odd location of `p` in the region bounded by `r`, for a point on no edge: the parity
/// of the edges crossing the rightward ray from `p` (exact, as `polyclip`'s own locator).
fn even_odd_inside(r: &Ring, p: polyclip::Point) -> bool {
    let v = &r.0;
    let mut inside = false;
    for i in 0..v.len() {
        let (a, b) = (v[i], v[(i + 1) % v.len()]);
        if (a.y > p.y) != (b.y > p.y) && (polyclip::predicates::orient(a, b, p) > 0) == (b.y > a.y) {
            inside = !inside;
        }
    }
    inside
}

impl BoardShape {
    fn of(p: &Project) -> Option<BoardShape> {
        let mut rings = p
            .board()
            .outline
            .contours
            .iter()
            .map(|c| Ring::from(geo::contour_ring(c, COPPER_TOL)))
            .filter(|r| r.len() >= 3);
        let outer = rings.next()?;
        Some(BoardShape::new(outer, rings.collect()))
    }

    fn new(outer: Ring, cutouts: Vec<Ring>) -> BoardShape {
        let edges = std::iter::once(&outer).chain(&cutouts).map(|r| Path::from(r.clone())).collect();
        let convex = convex_ccw(&outer);
        let outer_index = RTree::new(segment_boxes(&outer));
        let edge_index = RTree::new(std::iter::once(&outer).chain(&cutouts).flat_map(segment_boxes));
        let outer_in_range = polyclip::in_range(&outer);
        BoardShape { outer, cutouts, edges, convex, outer_index, edge_index, outer_in_range }
    }

    /// Whether `polyclip::contains(&self.outer, g)` holds. When no segment of the outer
    /// contour comes near `g`'s box, the contour's (even-odd) inside does not change over that
    /// box, so `g` is inside exactly when one of its points is: the same answer without
    /// building an arrangement of the whole contour.
    fn outer_contains<G: Geometry + ?Sized>(&self, g: &G) -> bool {
        if let (Some(bb), Some(ba), Some(p)) = (g.bbox(), self.outer.bbox(), g.any_point())
            && self.outer_in_range
            && !self.outer_index.any(&bb)
        {
            return ba.contains_rect(&bb) && polyclip::in_range(g) && even_odd_inside(&self.outer, p);
        }
        polyclip::contains(&self.outer, g)
    }

    /// Whether `g` comes closer than `d` to a board edge.
    fn near_edge<G: Geometry + ?Sized>(&self, g: &G, d: i64) -> bool {
        // Only segments within `d` of `g`'s box can be closer than `d`.
        g.bbox().is_some_and(|b| self.edge_index.any(&b.expand(d)))
            && self.edges.iter().any(|e| polyclip::distance_less_than(g, e, d))
    }

    /// Whether a region lies outside the board or overlaps a cutout.
    fn outside<G: Geometry + polyclip::RingSource + ?Sized>(&self, g: &G) -> bool {
        // A region lies in the convex hull of its vertices: when they are all in a convex outer
        // contour, so is the region (what `contains` would find, without building an
        // arrangement of a possibly huge zone fill). Otherwise ask `contains`.
        let inside = self.convex.as_ref().is_some_and(|c| {
            let mut all = true;
            g.visit_segments(&mut |p, _| {
                all = all && in_convex(c, p);
            });
            all
        });
        !(inside || self.outer_contains(g)) || self.cutouts.iter().any(|c| overlap(c, g).is_some())
    }
}

/// `r` oriented counter-clockwise, if it is a valid (simple) convex polygon ring.
fn convex_ccw(r: &Ring) -> Option<Ring> {
    let mut r = r.clone();
    if !r.is_ccw() {
        r.reverse_orientation();
    }
    let n = r.0.len();
    if n < 3 || polyclip::validate_set(&[Polygon::new(r.clone(), vec![])]).is_err() {
        return None;
    }
    let v = &r.0;
    (0..n).all(|i| polyclip::predicates::orient(v[i], v[(i + 1) % n], v[(i + 2) % n]) >= 0).then_some(r)
}

/// Whether `p` lies in the closed convex polygon `c` (counter-clockwise), exactly.
fn in_convex(c: &Ring, p: polyclip::Point) -> bool {
    let v = &c.0;
    (0..v.len()).all(|i| polyclip::predicates::orient(v[i], v[(i + 1) % v.len()], p) >= 0)
}

/// Copper outside the board, in cutouts or too close to an edge.
fn board_edges(ctx: &Ctx, items: &[CopperItem], o: &BoardShape, out: &mut Vec<Diagnostic>) {
    let min = ctx.rules.copper_to_edge;
    let limit = min.0 - TOLERANCE.0;
    for it in items {
        if it.shape.is_empty() {
            continue;
        }
        let label = it.item.to_string();
        let mut d = if o.outside(&it.shape) {
            Diagnostic::error("drc.outside_board", format!("{label} is outside the board outline or in a cutout"))
                .at(it.anchor)
                .with_hint("move it inside the board outline")
        } else if limit > 0 && o.near_edge(&it.shape, limit) {
            let (dist, at) = o
                .edges
                .iter()
                .filter_map(|e| gap(&it.shape, e))
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .unwrap_or((0.0, it.anchor));
            Diagnostic::error(
                "drc.copper_to_edge",
                format!("{label} is {} from the board edge, minimum is {min}", nm_f(dist)),
            )
            .at(at)
            .with_hint("move it away from the board edge")
        } else {
            continue;
        };
        d = d.with_subject(item_ref(&label));
        if let Some(n) = &it.net {
            d = d.with_subject(ObjectRef::Net(n.clone()));
        }
        out.push(d);
    }
}

/// Placed courtyards by designator, with their side.
fn courtyards(p: &Project) -> BTreeMap<String, (Ring, BoardSide)> {
    let board = p.board();
    board
        .footprints
        .iter()
        .filter_map(|(r, pf)| geo::placed_courtyard(p, r).map(|c| (r.clone(), (c, pf.side))))
        .collect()
}

/// Board holes (mounting holes): non-plated holes outside the board or in a cutout (plated
/// ones are copper and checked with it), and any hole over a footprint courtyard (either side:
/// the hole goes through the board).
fn board_holes(
    p: &Project,
    cy: &BTreeMap<String, (Ring, BoardSide)>,
    outline: Option<&BoardShape>,
    out: &mut Vec<Diagnostic>,
) {
    for h in &p.board().holes {
        let Ok(ring) = (Circle { center: pt(h.at), radius: h.diameter().0 / 2 }).to_ring(COPPER_TOL) else { continue };
        let subject = || ObjectRef::Named { kind: "hole".into(), name: h.name.clone() };
        if h.pad.is_none()
            && let Some(o) = outline
            && o.outside(&ring)
        {
            out.push(
                Diagnostic::error(
                    "drc.outside_board",
                    format!("hole {} is outside the board outline or in a cutout", h.name),
                )
                .with_subject(subject())
                .at(h.at)
                .with_hint("move it inside the board outline (board.hole_remove, then board.hole)"),
            );
        }
        for (r, (c, _)) in cy {
            if let Some(at) = overlap(&ring, c) {
                out.push(
                    Diagnostic::error(
                        "drc.courtyard_overlap",
                        format!("hole {} is inside the courtyard of {r}", h.name),
                    )
                    .with_subject(subject())
                    .with_subject(ObjectRef::Name(r.clone()))
                    .at(at)
                    .with_hint(format!("move {r} away from the hole (place.move), or move the hole")),
                );
            }
        }
    }
}

/// Courtyard overlaps on the same side, and footprints partly outside the board.
fn courtyard_rules(cy: &BTreeMap<String, (Ring, BoardSide)>, outline: Option<&BoardShape>, out: &mut Vec<Diagnostic>) {
    let list: Vec<(&String, &Ring, BoardSide)> = cy.iter().map(|(r, (c, s))| (r, c, *s)).collect();
    let boxes: Vec<Option<Rect>> = list.iter().map(|(_, c, _)| c.bbox()).collect();
    for (i, j) in near_pairs(&boxes, 0) {
        let ((ra, ca, sa), (rb, cb, sb)) = (list[i], list[j]);
        if sa != sb {
            continue;
        }
        if let Some(at) = overlap(ca, cb) {
            out.push(
                Diagnostic::error("drc.courtyard_overlap", format!("courtyards of {ra} and {rb} overlap"))
                    .with_subject(ObjectRef::Name(ra.clone()))
                    .with_subject(ObjectRef::Name(rb.clone()))
                    .at(at)
                    .with_hint(format!("move {ra} or {rb} apart (place.move), or put one on the other side")),
            );
        }
    }
    if let Some(o) = outline {
        for (r, c, _) in &list {
            if o.outside(*c) {
                let at = polyclip::centroid(*c).map(|q| from_f(q.x, q.y)).unwrap_or_default();
                out.push(
                    Diagnostic::error("drc.footprint_outside", format!("{r} is partly outside the board"))
                        .with_subject(ObjectRef::Name((*r).clone()))
                        .at(at)
                        .with_hint(format!("move {r} inside the board outline (place.set / place.move)")),
                );
            }
        }
    }
}

fn stroke(points: Vec<polyclip::Point>, width: Nm) -> PolygonSet {
    polyclip::offset_paths(&vec![Path(points)], (width.0 / 2).max(1), Join::Round, EndCap::Round, COPPER_TOL)
        .unwrap_or_default()
}

/// Silkscreen strokes by owner (designator or `graphic#id`) and side.
fn silk_shapes(p: &Project) -> Vec<(String, BoardSide, PolygonSet)> {
    let mut out = Vec::new();
    let board = p.board();
    for (r, pf) in &board.footprints {
        let Some(fp) = geo::footprint_for(p, r) else { continue };
        let tf = geo::transform(pf);
        let mut set = PolygonSet::new();
        for g in fp.graphics.iter().filter(|g| g.layer == GraphicLayer::Silk) {
            match &g.geometry {
                GraphicGeometry::Path { points } => {
                    set.extend(stroke(points.iter().map(|q| pt(tf(*q))).collect(), g.width));
                }
                GraphicGeometry::Polygon { points } => {
                    let mut v: Vec<polyclip::Point> = points.iter().map(|q| pt(tf(*q))).collect();
                    if let Some(&f) = v.first() {
                        v.push(f);
                    }
                    set.extend(stroke(v, g.width));
                }
                GraphicGeometry::Circle { center, radius, filled } => {
                    let c = pt(tf(*center));
                    if *filled {
                        let ring = Circle { center: c, radius: radius.0 + g.width.0 / 2 }.to_ring(COPPER_TOL);
                        set.extend(ring.ok().map(|r| Polygon::new(r, vec![])));
                    } else if let Ok(r) = (Circle { center: c, radius: radius.0 }).to_ring(COPPER_TOL) {
                        set.extend(stroke(Path::from(r).0, g.width));
                    }
                }
            }
        }
        if !set.is_empty() {
            out.push((r.clone(), pf.side, set));
        }
    }
    for g in &board.graphics {
        let side = match g.layer.as_str() {
            "F.SilkS" => BoardSide::Top,
            "B.SilkS" => BoardSide::Bottom,
            _ => continue,
        };
        if let GraphicKind::Line { points, width } = &g.kind {
            let set = stroke(points.iter().map(|q| pt(*q)).collect(), *width);
            if !set.is_empty() {
                out.push((format!("graphic#{}", g.id.0), side, set));
            }
        }
    }
    out
}

/// Silkscreen over or too close to pads (exposed copper).
fn silk_to_pads(ctx: &Ctx, pads: &[geo::PlacedPad], out: &mut Vec<Diagnostic>) {
    let silk = silk_shapes(ctx.p);
    let min = ctx.rules.silk_to_pad;
    let limit = (min.0 - TOLERANCE.0).max(1);
    // One index over silk owners then pads.
    let mut boxes: Vec<Option<Rect>> = silk.iter().map(|(_, _, s)| s.bbox()).collect();
    boxes.extend(pads.iter().map(|pp| pp.shape.bbox()));
    let ns = silk.len();
    for (i, j) in near_pairs(&boxes, limit) {
        if i >= ns || j < ns {
            continue;
        }
        let ((owner, side, shape), pp) = (&silk[i], &pads[j - ns]);
        let layer = if *side == BoardSide::Top { "F.Cu" } else { "B.Cu" };
        // A minimum within the tolerance only forbids overlaps, which must then be wider than the
        // tolerance (silk touching a pad overlaps it by the outward arc approximation).
        let close = if min > TOLERANCE {
            polyclip::distance_less_than(shape, &pp.shape, limit)
        } else {
            overlap(shape, &pp.shape).is_some()
        };
        if !pp.layers.iter().any(|l| l == layer) || !close {
            continue;
        }
        let (dist, at) = gap(shape, &pp.shape).unwrap_or((0.0, pp.center));
        let what = if dist == 0.0 { "overlaps".to_string() } else { format!("is {} from", nm_f(dist)) };
        let mut d = Diagnostic::warning(
            "drc.silk_over_pad",
            format!("silkscreen of {owner} {what} {}, minimum is {min}", pad_label(pp)),
        )
        .with_subject(item_ref(owner))
        .at(at)
        .with_hint("move the silkscreen or the part; fabs clip silkscreen over pads");
        d.subjects.extend(pad_subjects(pp));
        out.push(d);
    }
}

/// Tracks, vias and footprints inside keep-outs that forbid them.
fn keepouts(ctx: &Ctx, cy: &BTreeMap<String, (Ring, BoardSide)>, out: &mut Vec<Diagnostic>) {
    let board = ctx.p.board();
    for k in &board.keepouts {
        let ring: Ring = k.outline.iter().map(|q| pt(*q)).collect();
        if ring.len() < 3 {
            continue;
        }
        let Some(kb) = ring.bbox() else { continue };
        let kmask = if k.layers.is_empty() { u64::MAX } else { ctx.mask(&k.layers) };
        let kref = ObjectRef::Named { kind: "keepout".into(), name: k.name.clone() };
        let hint = format!("move it out of keep-out `{}`, or change the keep-out", k.name);
        let mut push = |what: String, subject: ObjectRef, at: Point| {
            out.push(
                Diagnostic::error("drc.keepout", format!("{what} is inside keep-out `{}`", k.name))
                    .with_subject(subject)
                    .with_subject(kref.clone())
                    .at(at)
                    .with_hint(hint.clone()),
            );
        };
        if k.no_tracks {
            for t in &board.tracks {
                if ctx.mask(std::slice::from_ref(&t.layer)) & kmask == 0 {
                    continue;
                }
                let shape = geo::track_shape(t);
                if shape.bbox().is_some_and(|b| b.intersects(&kb))
                    && let Some(at) = overlap(&ring, &shape)
                {
                    push(format!("track#{}", t.id.0), ObjectRef::Item { kind: "track".into(), index: t.id.0 }, at);
                }
            }
        }
        if k.no_vias {
            for v in &board.vias {
                if ctx.mask(&geo::via_layers(ctx.p, v)) & kmask == 0 {
                    continue;
                }
                let shape = geo::via_shape(v);
                if shape.bbox().is_some_and(|b| b.intersects(&kb)) && overlap(&ring, &shape).is_some() {
                    push(format!("via#{}", v.id.0), ObjectRef::Item { kind: "via".into(), index: v.id.0 }, v.at);
                }
            }
        }
        if k.no_footprints {
            for (r, (c, side)) in cy {
                let layer = if *side == BoardSide::Top { "F.Cu" } else { "B.Cu" };
                if ctx.mask(&[layer.to_string()]) & kmask == 0 {
                    continue;
                }
                if c.bbox().is_some_and(|b| b.intersects(&kb))
                    && let Some(at) = overlap(&ring, c)
                {
                    push(r.clone(), ObjectRef::Name(r.clone()), at);
                }
            }
        }
    }
}

fn unrouted(items: &[CopperItem], out: &mut Vec<Diagnostic>) {
    for l in geo::ratsnest_items(items) {
        out.push(
            Diagnostic::error(
                "drc.unrouted",
                format!("net {}: {} to {} is not connected ({} apart)", l.net, l.from, l.to, l.length),
            )
            .with_subject(ObjectRef::Net(l.net.clone()))
            .with_subject(item_ref(&l.from))
            .with_subject(item_ref(&l.to))
            .at(l.from_at)
            .with_hint(format!(
                "route it: track.add with points [\"{}\", \"{}\"] (add vias to change layers)",
                l.from, l.to
            )),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x0: i64, y0: i64, x1: i64, y1: i64) -> Option<Rect> {
        Some(Rect::new(polyclip::Point::new(x0, y0), polyclip::Point::new(x1, y1)))
    }

    /// The indexed outline queries answer exactly as `polyclip` on the whole contour.
    #[test]
    fn board_shape_queries_match_polyclip() {
        let p = polyclip::Point::new;
        // A comb (non-convex), a notch meeting the contour at a vertex, and a cutout.
        let mut outer = vec![p(0, 0), p(100_000, 0), p(100_000, 60_000)];
        for k in (0..5).rev() {
            let x = 10_000 + k * 20_000;
            outer.extend([p(x + 8_000, 60_000), p(x + 8_000, 20_000), p(x, 20_000), p(x, 60_000)]);
        }
        outer.extend([p(0, 60_000), p(0, 30_000), p(5_000, 25_000), p(0, 20_000)]);
        let cut = Ring::from(vec![p(60_000, 5_000), p(70_000, 5_000), p(70_000, 12_000), p(60_000, 12_000)]);
        let o = BoardShape::new(Ring::from(outer), vec![cut]);
        assert!(o.convex.is_none());
        let mut shapes: Vec<PolygonSet> = Vec::new();
        for i in -2..=52 {
            for j in -2..=32 {
                let (x, y) = (i * 2_000 + (j % 3) * 300, j * 2_000);
                let sq = |s: i64| Polygon::new(vec![p(x, y), p(x + s, y), p(x + s, y + s), p(x, y + s)], vec![]);
                shapes.push(vec![sq(900)]);
                if (i + j) % 7 == 0 {
                    shapes.push(vec![sq(4_500)]);
                }
            }
        }
        // A large shape with a hole around part of the contour.
        let big = Polygon::new(
            vec![p(-5_000, -5_000), p(30_000, -5_000), p(30_000, 30_000), p(-5_000, 30_000)],
            vec![vec![p(1_000, 1_000), p(1_000, 25_000), p(25_000, 25_000), p(25_000, 1_000)].into()],
        );
        shapes.push(vec![big]);
        let (mut inside, mut near) = (0, 0);
        for g in &shapes {
            let want = polyclip::contains(&o.outer, g);
            assert_eq!(o.outer_contains(g), want, "{g:?}");
            inside += want as usize;
            for d in [1, 1_500, 4_000] {
                let want = o.edges.iter().any(|e| polyclip::distance_less_than(g, e, d));
                assert_eq!(o.near_edge(g, d), want, "{g:?} {d}");
                near += want as usize;
            }
        }
        assert!(inside > 100 && inside < shapes.len() - 100 && near > 100, "{inside} {near}");
    }

    #[test]
    fn near_pairs_matches_brute_force() {
        // A deterministic scatter of boxes of mixed sizes.
        let mut boxes = Vec::new();
        let mut s: u64 = 12345;
        let mut next = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) as i64
        };
        for _ in 0..400 {
            let (x, y) = (next() % 100_000, next() % 100_000);
            let (w, h) = (next() % 3_000, next() % 20_000);
            boxes.push(r(x, y, x + w, y + h));
        }
        boxes.push(None);
        boxes.push(r(0, 0, 100_000, 100_000));
        for margin in [0, 500, 5_000] {
            let fast = near_pairs(&boxes, margin);
            let half = (margin + 1) / 2;
            let mut brute = Vec::new();
            for i in 0..boxes.len() {
                for j in i + 1..boxes.len() {
                    if let (Some(a), Some(b)) = (boxes[i], boxes[j])
                        && a.expand(half).intersects(&b.expand(half))
                    {
                        brute.push((i, j));
                    }
                }
            }
            assert_eq!(fast, brute, "margin {margin}");
        }
        assert!(near_pairs(&[r(0, 0, 1, 1)], 10).is_empty());
    }

    #[test]
    fn overlap_needs_area() {
        let um = 1_000;
        let a: Ring = [(0, 0), (10 * um, 0), (10 * um, 10 * um), (0, 10 * um)].into();
        let touching: Ring = [(10 * um, 0), (20 * um, 0), (20 * um, 10 * um), (10 * um, 10 * um)].into();
        let crossing: Ring = [(5 * um, 5 * um), (15 * um, 5 * um), (15 * um, 15 * um), (5 * um, 15 * um)].into();
        // Within the 2 µm tolerance: touching (arc approximation, rotated vertices).
        let grazing: Ring = [(9 * um, 0), (20 * um, 0), (20 * um, 10 * um), (9 * um, 10 * um)].into();
        let deeper: Ring = [(7 * um, 0), (20 * um, 0), (20 * um, 10 * um), (7 * um, 10 * um)].into();
        assert!(overlap(&a, &touching).is_none());
        assert!(overlap(&a, &grazing).is_none());
        assert!(overlap(&a, &deeper).is_some());
        assert_eq!(overlap(&a, &crossing), Some(from_f(7.5 * um as f64, 7.5 * um as f64)));
    }
}
