//! Copper zone fill (pours). See `docs/BOARD.md`, "Zone fill".
//!
//! For each zone and each of its layers, in priority order (higher first):
//!
//! ```text
//! fill = zone outline ∩ board area (outline − cutouts, shrunk by copper-to-edge)
//!        − other-net copper on the layer, inflated by max(zone clearance, the DRC clearance of
//!          the item's net and of the zone's net: net class, else rules)
//!        − NPTH holes, inflated by the zone clearance
//!        − higher-priority fills of other nets, inflated by the larger of both clearances
//!        − keep-outs forbidding pours
//!        − same-net pads per the pad connection (thermal: gap; none: clearance; solid: nothing)
//! fill = opening(fill, min_width / 2)                 (copper narrower than min_width goes)
//! fill = fill ∪ thermal spokes that lie entirely in the allowed area and reach the fill
//! fill = islands touching a same-net pad, via or track  (unconnected copper goes)
//! ```
//!
//! Obstacles are approximated outward and the fill boundary inward ([`OBSTACLE_TOL`],
//! [`FILL_TOL`]), and obstacle inflation carries a [`SAFETY`] margin covering the snap rounding
//! of booleans, so the approximation never creates a clearance violation. Each zone only
//! receives what can reach its outline (items from a per-layer [`RTree`], holes, keep-outs, and
//! the parts of earlier fills near it); farther shapes cannot change the result (D42). Fills
//! are derived data: recomputed on demand and never stored on disk; the last few results are
//! reused in process when every input is identical ([`fill_zones`]). Timings in
//! `docs/BOARD.md`.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;

use crate::board::{CopperItem, ItemRef, contour_ring, placed_pads};
use crate::geom::RTree;
use crate::geom::poly::{self, ArcTol, Boolean, Circle, FillRule, Geometry, Join, Op, Polygon, PolygonSet, Ring, Side};
use crate::id::ObjectId;
use crate::model::Project;
use crate::model::board::{Board, PadConnection, Zone};
use crate::model::circuit::NetClass;
use crate::units::{Angle, Nm};

/// Arc tolerance for fill boundaries: approximations stay inside the true region.
pub const FILL_TOL: ArcTol = ArcTol::new(5_000, Side::Inside);
/// Arc tolerance for obstacles: approximations contain the true region.
pub const OBSTACLE_TOL: ArcTol = ArcTol::new(5_000, Side::Outside);
/// Extra inflation (nm) of every keep-away region, covering vertex rounding of booleans and
/// offsets (each ≤ √2/2 nm).
pub const SAFETY: i64 = 10;

/// Effective fill settings of a zone (defaults resolved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneParams {
    /// Clearance to other nets.
    pub clearance: Nm,
    /// Minimum copper width (0: no opening).
    pub min_width: Nm,
    /// Pad connection style.
    pub pads: PadConnection,
    /// Gap around thermally relieved pads.
    pub thermal_gap: Nm,
    /// Thermal spoke width.
    pub thermal_spoke: Nm,
    /// First spoke direction relative to each pad (`None`: per pad shape, see
    /// [`PadThermal::first_spoke`](crate::board::PadThermal::first_spoke)).
    pub thermal_angle: Option<Angle>,
    /// Spokes per pad.
    pub thermal_spokes: u8,
}

/// Default number of thermal spokes per pad.
pub const DEFAULT_SPOKES: u8 = 4;
/// Largest number of thermal spokes per pad.
pub const MAX_SPOKES: u8 = 8;

fn class_of<'a>(p: &'a Project, net: Option<&str>) -> Option<&'a crate::model::circuit::NetClass> {
    let c = p.circuit();
    net.and_then(|n| c.nets.get(n)).and_then(|n| n.class.as_ref()).and_then(|k| c.netclasses.get(k))
}

/// The net class clearance of a net, if its class sets one.
pub fn class_clearance(p: &Project, net: Option<&str>) -> Option<Nm> {
    class_of(p, net).and_then(|c| c.clearance)
}

/// Resolves a zone's settings: clearance defaults to the larger of the rules' and its net
/// class's; min width to the rules' zone minimum; thermal gap to the clearance; spoke width to
/// max(track width of the net, 0.25 mm).
pub fn zone_params(p: &Project, z: &Zone) -> ZoneParams {
    let rules = &p.board().rules;
    let net = z.net.as_deref();
    let clearance = z.clearance.unwrap_or_else(|| rules.clearance.max(class_clearance(p, net).unwrap_or(Nm::ZERO)));
    let track = class_of(p, net).and_then(|c| c.track_width).unwrap_or(rules.track_width);
    ZoneParams {
        clearance,
        min_width: z.min_width.unwrap_or(rules.zone_min_width),
        pads: z.pads,
        thermal_gap: z.thermal_gap.unwrap_or(clearance),
        thermal_spoke: z.thermal_spoke.unwrap_or(track.max(Nm::from_um(250))),
        thermal_angle: z.thermal_angle,
        thermal_spokes: z.thermal_spokes.unwrap_or(DEFAULT_SPOKES).clamp(1, MAX_SPOKES),
    }
}

/// Copper-allowed board area: outer contour (approximated inward) minus cutouts (outward),
/// shrunk by `edge` (+ [`SAFETY`]). `None` when the board has no outline.
pub fn board_area(p: &Project, edge: Nm) -> Result<Option<PolygonSet>, poly::Error> {
    let contours = &p.board().outline.contours;
    let Some(outer) = contours.first() else { return Ok(None) };
    let outer: Ring = contour_ring(outer, FILL_TOL).into();
    let cuts: Vec<Ring> = contours[1..].iter().map(|c| contour_ring(c, OBSTACLE_TOL).into()).collect();
    let region = Boolean::new()
        .subject(&outer, FillRule::NonZero)
        .clip(&cuts, FillRule::NonZero)
        .op(Op::Difference)
        .execute()?;
    if edge.0 <= 0 {
        return Ok(Some(region));
    }
    Ok(Some(poly::offset(&region, -(edge.0 + SAFETY), Join::Round, FILL_TOL)?))
}

/// One layer's fill job, independent of the project (used directly by tests).
#[derive(Clone, Debug)]
pub struct LayerInput<'a> {
    /// The zone's net (`None`: every item is an obstacle and islands are kept).
    pub net: Option<&'a str>,
    /// Zone outline.
    pub outline: &'a [poly::Point],
    /// Copper-allowed board area (already shrunk by the edge clearance), if any.
    pub board: Option<&'a PolygonSet>,
    /// Copper items on this layer, each with the clearance the zone keeps from it when it
    /// belongs to another net.
    pub items: Vec<(&'a CopperItem, Nm)>,
    /// Further keep-away regions, already inflated (NPTH holes, higher-priority fills, keep-outs).
    pub keepaway: Vec<Polygon>,
    /// Settings.
    pub params: ZoneParams,
}

fn rect(x0: i64, y0: i64, x1: i64, y1: i64) -> Polygon {
    let p = poly::Point::new;
    Polygon::new(vec![p(x0, y0), p(x1, y0), p(x1, y1), p(x0, y1)], vec![])
}

/// A thermal relief of a pad: gap, spoke width and the board directions of its spokes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relief {
    /// Gap between the pad and the pour (nm).
    pub gap: i64,
    /// Spoke width (nm).
    pub w: i64,
    /// Board directions of the spokes.
    pub dirs: Vec<Angle>,
}

impl Relief {
    /// The relief of a pad in a zone: the pad's (or its footprint's) gap, width and angle,
    /// else the zone's, with the zone's spoke count.
    pub fn of(it: &CopperItem, prm: &ZoneParams) -> Relief {
        let t = it.local.thermal.unwrap_or_default();
        let gap = t.gap.unwrap_or(prm.thermal_gap).0;
        let w = t.spoke.unwrap_or(prm.thermal_spoke).0;
        let first = t.first_spoke(prm.thermal_angle);
        let n = prm.thermal_spokes.clamp(1, MAX_SPOKES) as i64;
        let dirs = (0..n).map(|k| (first + Angle(((k * 360_000) / n) as i32)).normalized()).collect();
        Relief { gap, w, dirs }
    }

    /// Whether every spoke runs along an axis.
    fn axis_aligned(&self) -> bool {
        self.dirs.iter().all(|a| a.quarter_turns().is_some())
    }

    /// How far beyond the pad's bounding box the relief reaches.
    pub fn reach(&self) -> i64 {
        // A slanted spoke's end corner can stick out by up to its half width beyond its length.
        self.gap + self.w + if self.axis_aligned() { 0 } else { self.w }
    }
}

/// The spokes of a thermal relief around a pad, from the pad center along each direction.
/// Axis directions end `gap + w` beyond the pad's bounding box; slanted ones `gap + w` beyond
/// the farthest crossing of the pad's outline.
pub fn spokes(item: &CopperItem, r: &Relief) -> Vec<Polygon> {
    let Some(b) = item.shape.bbox() else { return vec![] };
    let (gap, w) = (r.gap, r.w);
    let (cx, cy) = (item.anchor.x.0, item.anchor.y.0);
    let h = w / 2;
    let reach = gap + w;
    r.dirs
        .iter()
        .map(|&a| match a.quarter_turns() {
            Some(0) => rect(cx - h, cy - h, b.max.x + reach, cy + (w - h)),
            Some(1) => rect(cx - h, cy - h, cx + (w - h), b.max.y + reach),
            Some(2) => rect(b.min.x - reach, cy - h, cx + (w - h), cy + (w - h)),
            Some(_) => rect(cx - h, b.min.y - reach, cx + (w - h), cy + (w - h)),
            None => slanted_spoke(item, &b, a, gap, w),
        })
        .collect()
}

/// A spoke along a direction that is not an axis (floating point only to place its corners,
/// rounded to the nanometer).
fn slanted_spoke(item: &CopperItem, b: &poly::Rect, a: Angle, gap: i64, w: i64) -> Polygon {
    let rad = (a.0 as f64 / 1000.0).to_radians();
    let (ux, uy) = (rad.cos(), rad.sin());
    let (cx, cy) = (item.anchor.x.0 as f64, item.anchor.y.0 as f64);
    // Farthest crossing of the ray with the pad's outlines; without one (an anchor outside the
    // copper), the farthest corner of its box.
    let mut t_max: Option<f64> = None;
    for pg in &item.shape {
        let pts = &pg.outer.0;
        for k in 0..pts.len() {
            let (p0, p1) = (pts[k], pts[(k + 1) % pts.len()]);
            let (ex, ey) = ((p1.x - p0.x) as f64, (p1.y - p0.y) as f64);
            let den = ux * ey - uy * ex;
            if den.abs() < 1e-12 {
                continue;
            }
            let (qx, qy) = (p0.x as f64 - cx, p0.y as f64 - cy);
            let t = (qx * ey - qy * ex) / den;
            let s = (qx * uy - qy * ux) / den;
            if t >= 0.0 && (0.0..=1.0).contains(&s) {
                t_max = Some(t_max.map_or(t, |m: f64| m.max(t)));
            }
        }
    }
    let t = t_max.unwrap_or_else(|| {
        [(b.min.x, b.min.y), (b.max.x, b.min.y), (b.max.x, b.max.y), (b.min.x, b.max.y)]
            .iter()
            .map(|&(x, y)| (x as f64 - cx).hypot(y as f64 - cy))
            .fold(0.0, f64::max)
    });
    let (h, len) = ((w / 2) as f64, t + (gap + w) as f64);
    let (o0, o1) = (-h, (w - w / 2) as f64);
    let (nx, ny) = (-uy, ux);
    let at =
        |s: f64, o: f64| poly::Point::new((cx + ux * s + nx * o).round() as i64, (cy + uy * s + ny * o).round() as i64);
    Polygon::new(vec![at(-h, o0), at(len, o0), at(len, o1), at(-h, o1)], vec![])
}

/// A spoke trimmed where it would cross another item's clearance (a pad squeezed between
/// other-net items, where no whole spoke fits): `spoke` ∩ `avail`, kept only within the pad's
/// thermal gap and the pour, in pieces joining the pad to the pour over at least half the spoke
/// width. Otherwise nothing.
pub fn trimmed_spoke(
    it: &CopperItem,
    r: &Relief,
    spoke: &Polygon,
    avail: &PolygonSet,
    fill: &PolygonSet,
) -> Result<PolygonSet, poly::Error> {
    if !poly::intersects(fill, spoke) {
        return Ok(vec![]);
    }
    let gap = poly::offset(&it.shape, r.gap, Join::Round, OBSTACLE_TOL)?;
    let room = Boolean::new().subject(fill, FillRule::NonZero).subject(&gap, FillRule::NonZero).execute()?;
    let inside = Boolean::new()
        .subject(avail, FillRule::NonZero)
        .clip(&room, FillRule::NonZero)
        .op(Op::Intersection)
        .execute()?;
    let mut piece = Boolean::new()
        .subject(std::slice::from_ref(spoke), FillRule::NonZero)
        .clip(&inside, FillRule::NonZero)
        .op(Op::Intersection)
        .execute()?;
    piece.retain(|q| poly::intersects(q, &it.shape) && poly::intersects(q, fill));
    let joint = Boolean::new()
        .subject(&piece, FillRule::NonZero)
        .clip(fill, FillRule::NonZero)
        .op(Op::Intersection)
        .execute()?;
    if joint.is_empty() || poly::offset(&joint, -(r.w / 4), Join::Round, FILL_TOL)?.is_empty() {
        return Ok(vec![]);
    }
    Ok(piece)
}

/// How a same-net pad joins a zone: its own override, else the zone's style; `tht_thermal`
/// resolves to thermal reliefs for plated through-hole pads and solid for the rest.
pub fn connection(it: &CopperItem, zone: PadConnection) -> PadConnection {
    match it.local.connection.unwrap_or(zone) {
        PadConnection::ThtThermal if it.local.through => PadConnection::Thermal,
        PadConnection::ThtThermal => PadConnection::Solid,
        c => c,
    }
}

fn same_net(a: Option<&str>, b: Option<&str>) -> bool {
    a.is_some() && a == b
}

/// Whether an obstacle with bounding box `b`, inflated by `d` (arcs approximated outward,
/// plus [`SAFETY`]), can reach into `area`.
fn reaches(b: &poly::Rect, d: i64, area: &poly::Rect) -> bool {
    b.expand(d + OBSTACLE_TOL.tolerance + SAFETY + 1).intersects(area)
}

/// `set` without what cannot matter where `near` holds: polygons whose outer box is not
/// `near` are dropped, holes whose box is not `near` are filled. Within the boxes `near`
/// accepts, the region is unchanged, so clipping it to them (or growing it, when `near` is
/// grown by the growth distance) gives the same result as with `set`. Also returns a key
/// naming what was kept: per kept polygon, its index, its number of kept holes and their
/// indices.
fn cull_selection(set: &PolygonSet, near: impl Fn(&poly::Rect) -> bool) -> (Vec<u32>, PolygonSet) {
    let mut sel = Vec::new();
    let mut out = Vec::new();
    for (i, p) in set.iter().enumerate() {
        if !p.outer.bbox().is_some_and(|b| near(&b)) {
            continue;
        }
        let holes: Vec<u32> =
            (0..p.holes.len() as u32).filter(|&h| p.holes[h as usize].bbox().is_some_and(|b| near(&b))).collect();
        sel.push(i as u32);
        sel.push(holes.len() as u32);
        sel.extend(&holes);
        out.push(Polygon {
            outer: p.outer.clone(),
            holes: holes.iter().map(|&h| p.holes[h as usize].clone()).collect(),
        });
    }
    (sel, out)
}

/// Fills one zone layer. The result is canonical (deterministic).
pub fn fill_layer(input: &LayerInput<'_>) -> Result<PolygonSet, poly::Error> {
    let prm = &input.params;
    // Area: zone outline ∩ board area.
    let outline: Ring = input.outline.to_vec().into();
    let area = match input.board {
        Some(b) => Boolean::new()
            .subject(&outline, FillRule::NonZero)
            .clip(b, FillRule::NonZero)
            .op(Op::Intersection)
            .execute()?,
        None => poly::union_all(&outline, FillRule::NonZero)?,
    };
    if area.is_empty() {
        return Ok(vec![]);
    }
    let abox = area.bbox().expect("non-empty");

    // Hard keep-away: other-net copper inflated by its clearance (grouped by clearance so each
    // group is offset in one pass) plus the given regions. Soft keep-away: gaps around same-net
    // pads (thermal gap, or clearance for `none`), which shape this zone only; spokes cross them.
    let mut hard_groups: BTreeMap<i64, Vec<Polygon>> = BTreeMap::new();
    let mut soft_groups: BTreeMap<i64, Vec<Polygon>> = BTreeMap::new();
    let mut thermal: Vec<(&CopperItem, Relief)> = Vec::new();
    let mut same: Vec<&CopperItem> = Vec::new();
    // Shapes far from the area are left out: they cannot change it (see `reaches`).
    for &(it, c) in &input.items {
        if same_net(it.net.as_deref(), input.net) {
            same.push(it);
            if let ItemRef::Pad(..) = it.item {
                let Some(b) = it.shape.bbox() else { continue };
                let d = match connection(it, prm.pads) {
                    PadConnection::Solid | PadConnection::ThtThermal => continue,
                    PadConnection::Thermal => {
                        // A spoke stays inside its pad's window; outside the area it is never kept.
                        let r = Relief::of(it, prm);
                        let gap = r.gap;
                        if b.expand(r.reach() + 1).intersects(&abox) {
                            thermal.push((it, r));
                        }
                        gap
                    }
                    PadConnection::None => prm.clearance.0,
                };
                if reaches(&b, d, &abox) {
                    soft_groups.entry(d).or_default().extend(it.shape.iter().cloned());
                }
            }
            continue;
        }
        let c = c.max(prm.clearance).0;
        if it.shape.bbox().is_some_and(|b| b.expand(c + SAFETY + 1).intersects(&abox)) {
            hard_groups.entry(c).or_default().extend(it.shape.iter().cloned());
        }
    }
    if input.net.is_some() && same.is_empty() {
        // Nothing of the net on this layer: every island would be unconnected.
        return Ok(vec![]);
    }
    let mut hard: Vec<Polygon> = input.keepaway.clone();
    // Item outlines may overlap (a teardrop over its pad): merge each group first so `offset` gets
    // canonical input; its linear "looks canonical" check cannot see overlaps, and growing
    // overlapping rings unmerged rounds differently (docs/POLYGON_LIB.md §8).
    for (c, shapes) in hard_groups {
        let shapes = Boolean::new().subject(&shapes, FillRule::NonZero).op(Op::Union).execute()?;
        hard.extend(poly::offset(&shapes, c + SAFETY, Join::Round, OBSTACLE_TOL)?);
    }
    let mut soft: Vec<Polygon> = Vec::new();
    for (d, shapes) in soft_groups {
        let shapes = Boolean::new().subject(&shapes, FillRule::NonZero).op(Op::Union).execute()?;
        soft.extend(poly::offset(&shapes, d, Join::Round, OBSTACLE_TOL)?);
    }

    // Allowed copper (where spokes may go), then the pour proper.
    let avail =
        Boolean::new().subject(&area, FillRule::NonZero).clip(&hard, FillRule::NonZero).op(Op::Difference).execute()?;
    let mut fill = if soft.is_empty() {
        avail.clone()
    } else {
        Boolean::new().subject(&avail, FillRule::NonZero).clip(&soft, FillRule::NonZero).op(Op::Difference).execute()?
    };
    // Minimum width: morphological opening. Both offsets approximate arcs inward, so the result
    // stays inside the fill up to vertex rounding (< 2 nm), which SAFETY covers.
    if prm.min_width.0 > 1 && !fill.is_empty() {
        fill = poly::opening(&fill, prm.min_width.0 / 2, FILL_TOL)?;
    }
    // Thermal spokes: whole, inside the allowed area, and reaching the pour. Tested against the
    // allowed area and the pour clipped to small windows around the pads (one boolean each), so
    // each test only scans local edges.
    let mut kept: Vec<Polygon> = Vec::new();
    if !thermal.is_empty() && !fill.is_empty() {
        let boxes: Vec<poly::Rect> =
            thermal.iter().filter_map(|(it, r)| it.shape.bbox().map(|b| b.expand(r.reach() + 1))).collect();
        let windows: Vec<Polygon> = boxes.iter().map(|b| rect(b.min.x, b.min.y, b.max.x, b.max.y)).collect();
        let index = RTree::new(boxes.iter().map(|b| Some(*b)));
        // Rings meeting no window cannot change the clipped sets: they are left out first.
        let clip = |set: &PolygonSet| {
            Boolean::new()
                .subject(&cull_selection(set, |b| index.any(b)).1, FillRule::NonZero)
                .clip(&windows, FillRule::NonZero)
                .op(Op::Intersection)
                .execute()
        };
        let (local_avail, local_fill) = (clip(&avail)?, clip(&fill)?);
        let near = |set: &PolygonSet, r: &poly::Rect| -> PolygonSet {
            set.iter().filter(|q| q.bbox().is_some_and(|b| b.intersects(r))).cloned().collect()
        };
        let fits = |s: &Polygon| {
            let b = s.bbox().expect("spoke");
            poly::contains(&near(&local_avail, &b), s) && poly::intersects(&near(&local_fill, &b), s)
        };
        for (it, r) in &thermal {
            // A spoke whose end would cross another item's clearance is trimmed instead of
            // dropped when the rest still joins the pour (`trimmed_spoke`).
            for s in spokes(it, r) {
                if fits(&s) {
                    kept.push(s);
                } else {
                    let b = s.bbox().expect("spoke");
                    kept.extend(trimmed_spoke(it, r, &s, &near(&local_avail, &b), &near(&local_fill, &b))?);
                }
            }
        }
    }
    if !kept.is_empty() {
        fill = Boolean::new().subject(&fill, FillRule::NonZero).subject(&kept, FillRule::NonZero).execute()?;
    }
    // Unconnected islands go (netless zones keep everything).
    if input.net.is_some() {
        fill.retain(|poly| same.iter().any(|it| poly::intersects(poly, &it.shape)));
    }
    Ok(fill)
}

/// A filled zone layer.
#[derive(Clone, Debug)]
pub struct ZoneFill {
    /// Zone ID.
    pub zone: ObjectId,
    /// Zone name.
    pub name: String,
    /// Net.
    pub net: Option<String>,
    /// Copper layer.
    pub layer: String,
    /// Effective clearance used.
    pub clearance: Nm,
    /// The fill: disjoint polygons (islands), canonical.
    pub fill: PolygonSet,
    /// Geometry failure, if the fill could not be computed (the fill is then empty).
    pub error: Option<String>,
}

/// Everything a fill depends on: the board (zones, keep-outs, outline, rules, ...), net
/// classes and net assignments, the non-zone copper and NPTH holes. Compared field by field
/// (the board first by pointer: an unchanged project shares it), so a hit means identical
/// inputs.
struct MemoEntry {
    board: Arc<Board>,
    netclasses: BTreeMap<String, NetClass>,
    net_classes: Vec<(String, Option<String>)>,
    base: Vec<CopperItem>,
    npth: Vec<NpthHole>,
    /// The clearance each base item keeps (custom rules, local and class clearances: they
    /// depend on library footprints too, which the board does not hold).
    item_c: Vec<Nm>,
    fills: Vec<ZoneFill>,
}

impl MemoEntry {
    fn matches(&self, p: &Project, base: &[CopperItem], npth: &[NpthHole], item_c: &[Nm]) -> bool {
        let c = p.circuit();
        (Arc::ptr_eq(&self.board, p.board_arc()) || *self.board == *p.board())
            && self.netclasses == c.netclasses
            && self.net_classes.len() == c.nets.len()
            && self.net_classes.iter().zip(&c.nets).all(|((n, k), (name, net))| n == name && *k == net.class)
            && self.base == base
            && self.npth == npth
            && self.item_c == item_c
    }
}

/// Recently computed fills (in-process), most recent first. Fills are derived data, recomputed
/// whenever any input differs.
static MEMO: std::sync::Mutex<Vec<MemoEntry>> = std::sync::Mutex::new(Vec::new());
const MEMO_ENTRIES: usize = 4;

/// Fills every zone of the board, given the board's non-zone copper (pads, tracks, vias).
/// Results are in board order (zone, then its layers in the order given). Results for identical
/// inputs are reused within the process (see [`fill_zones_uncached`]).
pub fn fill_zones(p: &Project, base: &[CopperItem]) -> Vec<ZoneFill> {
    if p.board().zones.is_empty() {
        return vec![];
    }
    let npth = npth_holes(p);
    let item_c = item_clearances(p, base);
    if let Ok(m) = MEMO.lock()
        && let Some(e) = m.iter().find(|e| e.matches(p, base, &npth, &item_c))
    {
        return e.fills.clone();
    }
    let fills = fill_with(p, base, &npth, &item_c);
    if let Ok(mut m) = MEMO.lock() {
        m.retain(|e| !e.matches(p, base, &npth, &item_c));
        let c = p.circuit();
        m.insert(
            0,
            MemoEntry {
                board: p.board_arc().clone(),
                netclasses: c.netclasses.clone(),
                net_classes: c.nets.iter().map(|(n, net)| (n.clone(), net.class.clone())).collect(),
                base: base.to_vec(),
                npth: npth.clone(),
                item_c,
                fills: fills.clone(),
            },
        );
        m.truncate(MEMO_ENTRIES);
    }
    fills
}

/// Forgets the fills kept in process by [`fill_zones`] (benchmarks measure cold runs with it).
pub fn clear_fill_cache() {
    if let Ok(mut m) = MEMO.lock() {
        m.clear();
    }
}

/// [`fill_zones`] without the in-process reuse.
pub fn fill_zones_uncached(p: &Project, base: &[CopperItem]) -> Vec<ZoneFill> {
    fill_with(p, base, &npth_holes(p), &item_clearances(p, base))
}

/// The clearance the DRC requires around each base item: its custom rule's, else its local
/// clearance, else its net's (class, else the rules'), never below `min_clearance`. A zone
/// with a smaller clearance of its own still keeps this much, so a fill never violates the DRC.
fn item_clearances(p: &Project, base: &[CopperItem]) -> Vec<Nm> {
    let resolved = super::clearance::Clearances::new(p, base, true);
    (0..base.len()).map(|i| resolved.item(i)).collect()
}

/// A non-plated hole: the ends of its slot (equal for a round hole) and its radius.
type NpthHole = (poly::Point, poly::Point, i64);

fn npth_holes(p: &Project) -> Vec<NpthHole> {
    placed_pads(p)
        .into_iter()
        .filter_map(|pp| match pp.hole {
            Some((d, false)) => {
                let (a, b) = pp.slot.unwrap_or((pp.hole_at, pp.hole_at));
                Some((a.into(), b.into(), d.0 / 2))
            }
            _ => None,
        })
        .collect()
}

/// The farthest a thermal relief of any pad among `items` can reach beyond the pad's box (its
/// own settings, else the zone's; see `Relief::reach`).
fn max_relief(items: &[(&CopperItem, Nm)], prm: &ZoneParams) -> i64 {
    let zone = prm.thermal_gap.0 + 2 * prm.thermal_spoke.0;
    items
        .iter()
        .filter_map(|(it, _)| it.local.thermal)
        .map(|t| t.gap.unwrap_or(prm.thermal_gap).0 + 2 * t.spoke.unwrap_or(prm.thermal_spoke).0)
        .fold(zone, i64::max)
}

fn fill_with(p: &Project, base: &[CopperItem], npth: &[NpthHole], item_c: &[Nm]) -> Vec<ZoneFill> {
    let board = p.board();
    if board.zones.is_empty() {
        return vec![];
    }
    let rules = &board.rules;
    let area = board_area(p, rules.copper_to_edge);
    let net_c = |n: Option<&str>| class_clearance(p, n).unwrap_or(rules.clearance);
    // Zones interact only within a layer (priorities), so layers fill independently, in
    // parallel; within a layer, higher priority first (ties: board order).
    let mut by_layer: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for (zi, z) in board.zones.iter().enumerate() {
        for (li, layer) in z.layers.iter().enumerate() {
            by_layer.entry(layer.as_str()).or_default().push((zi, li));
        }
    }
    for jobs in by_layer.values_mut() {
        jobs.sort_by_key(|&(zi, _)| (std::cmp::Reverse(board.zones[zi].priority), zi));
    }
    let fill_one_layer = |layer: &str, jobs: &[(usize, usize)]| -> Vec<(usize, usize, ZoneFill)> {
        let items_on: Vec<(&CopperItem, Nm)> = base
            .iter()
            .zip(item_c)
            .filter(|(it, _)| it.layers.iter().any(|l| l == layer))
            .map(|(it, c)| (it, *c))
            .collect();
        let index = RTree::new(items_on.iter().map(|(it, _)| it.shape.bbox()));
        let max_c = items_on.iter().map(|(_, c)| c.0).max().unwrap_or(0);
        let mut done: Vec<(usize, usize, ZoneFill)> = Vec::new();
        // Earlier fills grown by a keep-away distance, by (index in `done`, distance, kept
        // rings): later zones of the layer often need the same ones.
        let mut grown: BTreeMap<(usize, i64, Vec<u32>), PolygonSet> = BTreeMap::new();
        for &(zi, li) in jobs {
            let z = &board.zones[zi];
            let mut prm = zone_params(p, z);
            // The DRC clearance of the zone's own net applies to its copper too.
            prm.clearance = prm.clearance.max(net_c(z.net.as_deref()));
            // Everything that can change the fill lies near the zone outline: items, holes,
            // earlier fills and keep-outs farther away are left out (`fill_layer` would ignore
            // them or subtract them where there is nothing to subtract from).
            let outline: Vec<poly::Point> = z.outline.iter().map(|&q| q.into()).collect();
            let window = poly::Rect::of_points(&outline);
            let near = |b: &poly::Rect, d: i64| window.is_none_or(|w| reaches(b, d, &w));
            let mut fill = ZoneFill {
                zone: z.id,
                name: z.name.clone(),
                net: z.net.clone(),
                layer: layer.to_string(),
                clearance: prm.clearance,
                fill: vec![],
                error: None,
            };
            let result = (|| -> Result<PolygonSet, poly::Error> {
                let barea = match &area {
                    Ok(a) => a.as_ref(),
                    Err(e) => return Err(e.clone()),
                };
                let mut keepaway: Vec<Polygon> = Vec::new();
                for &(a, b, r) in npth {
                    let r = r + prm.clearance.0 + SAFETY;
                    if !near(&poly::Rect::new(a, a).union(&poly::Rect::new(b, b)), r) {
                        continue;
                    }
                    if a == b {
                        let ring = Circle::new(a, r).to_ring(OBSTACLE_TOL)?;
                        keepaway.push(Polygon::new(ring, vec![]));
                    } else {
                        let path = vec![poly::Path(vec![a, b])];
                        keepaway.extend(poly::offset_paths(&path, r, Join::Round, poly::EndCap::Round, OBSTACLE_TOL)?);
                    }
                }
                for (k, (_, _, f)) in done.iter().enumerate() {
                    if !same_net(f.net.as_deref(), z.net.as_deref()) && !f.fill.is_empty() {
                        let c = prm.clearance.max(f.clearance).0 + SAFETY;
                        let (sel, part) = cull_selection(&f.fill, |b| near(b, c));
                        if part.is_empty() {
                            continue;
                        }
                        let g = match grown.entry((k, c, sel)) {
                            Entry::Occupied(e) => e.into_mut(),
                            Entry::Vacant(e) => e.insert(poly::offset(&part, c, Join::Round, OBSTACLE_TOL)?),
                        };
                        keepaway.extend(g.iter().cloned());
                    }
                }
                for k in &board.keepouts {
                    if k.no_pours
                        && (k.layers.is_empty() || k.layers.iter().any(|l| l == layer))
                        && k.outline.len() >= 3
                    {
                        // Counter-clockwise, as every other keep-away region (an outline drawn
                        // clockwise would cancel what it overlaps in the non-zero union).
                        let mut ring: Ring = k.outline.iter().map(|&q| q.into()).collect::<Vec<_>>().into();
                        if !ring.bbox().is_some_and(|b| near(&b, 0)) {
                            continue;
                        }
                        if !ring.is_ccw() {
                            ring.reverse_orientation();
                        }
                        keepaway.push(Polygon::new(ring, vec![]));
                    }
                }
                fill_layer(&LayerInput {
                    net: z.net.as_deref(),
                    outline: &outline,
                    board: barea,
                    items: match window {
                        Some(w) => {
                            // Every distance `fill_layer` grows an item by, plus the arc and
                            // rounding margins of `reaches`.
                            let d = max_c.max(prm.clearance.0).max(max_relief(&items_on, &prm));
                            let q = w.expand(d + OBSTACLE_TOL.tolerance + SAFETY + 1);
                            index.query(&q).into_iter().map(|i| items_on[i as usize]).collect()
                        }
                        None => items_on.clone(),
                    },
                    keepaway,
                    params: prm,
                })
            })();
            match result {
                Ok(f) => fill.fill = f,
                Err(e) => fill.error = Some(e.to_string()),
            }
            done.push((zi, li, fill));
        }
        done
    };
    let mut all: Vec<(usize, usize, ZoneFill)> = if by_layer.len() == 1 {
        by_layer.iter().flat_map(|(l, jobs)| fill_one_layer(l, jobs)).collect()
    } else {
        std::thread::scope(|sc| {
            let handles: Vec<_> = by_layer.iter().map(|(l, jobs)| sc.spawn(|| fill_one_layer(l, jobs))).collect();
            handles.into_iter().flat_map(|h| h.join().expect("zone fill thread")).collect()
        })
    };
    all.sort_by_key(|(zi, li, _)| (*zi, *li));
    all.into_iter().map(|(_, _, f)| f).collect()
}

/// Zone fills as copper items: one item per island, `ItemRef::Zone(id, layer, island)`.
pub fn zone_items(p: &Project, base: &[CopperItem]) -> Vec<CopperItem> {
    let mut out = Vec::new();
    for f in fill_zones(p, base) {
        for (i, poly) in f.fill.into_iter().enumerate() {
            let anchor = poly.outer.0.first().copied().map(Into::into).unwrap_or_default();
            out.push(CopperItem {
                item: ItemRef::Zone(f.zone, f.layer.clone(), i),
                net: f.net.clone(),
                layers: vec![f.layer.clone()],
                shape: vec![poly],
                anchor,
                local: Default::default(),
            });
        }
    }
    out
}

/// Area of a polygon set in mm².
pub fn area_mm2(s: &PolygonSet) -> f64 {
    poly::area2(s) as f64 / 2.0 / 1e12
}

#[cfg(test)]
mod tests;
