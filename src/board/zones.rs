//! Copper zone fill (pours). See `docs/BOARD.md`, "Zone fill".
//!
//! For each zone and each of its layers, in priority order (higher first):
//!
//! ```text
//! fill = zone outline ∩ board area (outline − cutouts, shrunk by copper-to-edge)
//!        − other-net copper on the layer, inflated by max(zone clearance, the DRC clearance of
//!          the item's net and of the zone's net: net class, else rules)
//!        − NPTH holes, inflated by the rules' hole clearance (copper to hole) when set, else
//!          by the zone clearance; with a hole clearance, plated holes of other nets too
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
//! reused in process when every input is identical ([`fill_zones`]), and where a board differs
//! from one filled before, the reliefs of the pads whose surroundings are as they were
//! (`SpokeMemo`, D117): the fill is the same polygons either way. Timings in `docs/BOARD.md`.

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
    /// `Some` when the zone leaves the spoke width to cadlab: a pad narrower than twice
    /// `thermal_spoke` then gets spokes half its shorter side wide, never below this (the
    /// rules' minimum track width), so that small pads are relieved too ([`Relief::of`]).
    /// `None`: the zone's width as given, for every pad.
    pub spoke_floor: Option<Nm>,
    /// The narrowest copper a spoke may be left with where it is trimmed (the rules' minimum
    /// track width; 0: half the spoke's width, as the trim asks on its own).
    pub min_spoke: Nm,
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
/// max(track width of the net, 0.25 mm), narrower on small pads (`ZoneParams::spoke_floor`).
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
        thermal_spoke: z.thermal_spoke.unwrap_or(default_spoke(track)),
        thermal_angle: z.thermal_angle,
        thermal_spokes: z.thermal_spokes.unwrap_or(DEFAULT_SPOKES).clamp(1, MAX_SPOKES),
        spoke_floor: z.thermal_spoke.is_none().then_some(rules.min_track_width),
        min_spoke: rules.min_track_width,
    }
}

/// The spoke width of a zone that sets none, for a net with tracks of width `track`.
pub fn default_spoke(track: Nm) -> Nm {
    track.max(Nm::from_um(250))
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
    /// Directions tried for the spokes that do not survive in `dirs`: the same ones turned by
    /// half a step (for four spokes, the other four of the eight directions), when neither
    /// the pad nor the zone sets an angle; else none (D112).
    pub alt: Vec<Angle>,
    /// The narrowest copper a trimmed spoke may join the pad to the pour by (nm): the rules'
    /// minimum track width, or the spoke's own width when that is less; 0: not asked (D119).
    pub min: i64,
}

impl Relief {
    /// The relief of a pad in a zone: the pad's (or its footprint's) gap, width and angle,
    /// else the zone's, with the zone's spoke count. A zone that leaves the spoke width to
    /// cadlab gives a pad spokes no wider than half its shorter side (whole micrometers, at
    /// least the zone's `spoke_floor`): four spokes as wide as a small pad's sides would be no
    /// relief.
    pub fn of(it: &CopperItem, prm: &ZoneParams) -> Relief {
        let t = it.local.thermal.unwrap_or_default();
        let gap = t.gap.unwrap_or(prm.thermal_gap).0;
        let zone_w = || match prm.spoke_floor {
            Some(floor) => prm.thermal_spoke.0.min(fitted_spoke(short_side(&it.shape)).max(floor.0)).max(1),
            None => prm.thermal_spoke.0,
        };
        let w = t.spoke.map_or_else(zone_w, |s| s.0);
        let first = t.first_spoke(prm.thermal_angle);
        let n = prm.thermal_spokes.clamp(1, MAX_SPOKES) as i64;
        let dirs: Vec<Angle> = (0..n).map(|k| (first + Angle(((k * 360_000) / n) as i32)).normalized()).collect();
        let free = t.angle.is_none() && prm.thermal_angle.is_none();
        let alt =
            if free { dirs.iter().map(|d| (*d + Angle((180_000 / n) as i32)).normalized()).collect() } else { vec![] };
        Relief { gap, w, dirs, alt, min: prm.min_spoke.0.clamp(0, w.max(0)) }
    }

    /// Whether every spoke, the alternatives included, runs along an axis.
    fn axis_aligned(&self) -> bool {
        self.dirs.iter().chain(&self.alt).all(|a| a.quarter_turns().is_some())
    }

    /// How far beyond the pad's bounding box the relief reaches.
    pub fn reach(&self) -> i64 {
        // A slanted spoke's end corner can stick out by up to its half width beyond its length.
        self.gap + self.w + if self.axis_aligned() { 0 } else { self.w }
    }
}

/// The widest spoke fitted to a pad with this shorter side: half of it, in whole micrometers.
/// The side is taken to the nearest micrometer first, so the nanometer by which a turned pad's
/// rounded corners move its measure does not move the width.
pub fn fitted_spoke(short_side: i64) -> i64 {
    (short_side + 500) / 1_000 * 1_000 / 2 / 1_000 * 1_000
}

/// The shorter side of a pad: of the rectangle with the area and the outline length of its
/// copper (the pad's own short side whatever its rotation; 0.79 of a disc's diameter). Floating
/// point only for the lengths of the outline's edges (products, sums and square roots, the same
/// on every platform), rounded to the nanometer.
pub fn short_side(shape: &PolygonSet) -> i64 {
    let half: f64 = shape
        .iter()
        .map(|pg| {
            let pts = &pg.outer.0;
            (0..pts.len())
                .map(|k| {
                    let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
                    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
                    (dx * dx + dy * dy).sqrt()
                })
                .sum::<f64>()
        })
        .sum::<f64>()
        / 2.0;
    // Unsigned: a mirrored pad's outline runs the other way.
    let area = poly::area2(shape).abs() as f64 / 2.0;
    let disc = (half * half - 4.0 * area).max(0.0);
    let bound = shape.bbox().map_or(i64::MAX, |b| b.width().min(b.height()));
    (((half - disc.sqrt()) / 2.0).round() as i64).clamp(0, bound.max(0))
}

/// The spokes of a thermal relief around a pad, from the pad center along each direction.
/// Axis directions end `gap + w` beyond the pad's bounding box; slanted ones `gap + w` beyond
/// the farthest crossing of the pad's outline.
pub fn spokes(item: &CopperItem, r: &Relief) -> Vec<Polygon> {
    spokes_along(item, r, &r.dirs)
}

/// The spokes of the relief's other directions ([`Relief::alt`]), as [`spokes`].
pub fn alt_spokes(item: &CopperItem, r: &Relief) -> Vec<Polygon> {
    spokes_along(item, r, &r.alt)
}

/// Smallest turn between two directions, in millidegrees (0 to 180 000).
fn turn(a: Angle, b: Angle) -> i32 {
    let d = (a.0 - b.0).rem_euclid(360_000);
    d.min(360_000 - d)
}

/// Of the directions `cand` (indices into it are returned), up to `want` that spread best
/// around those already `taken`: each time the one farthest from its nearest taken direction,
/// then farthest from all of them together, then the first.
pub fn spread(taken: &[Angle], cand: &[Angle], want: usize) -> Vec<usize> {
    let mut taken = taken.to_vec();
    let mut left: Vec<usize> = (0..cand.len()).collect();
    let mut out = Vec::new();
    while out.len() < want && !left.is_empty() {
        let key = |&i: &usize| {
            let nearest = taken.iter().map(|t| turn(*t, cand[i])).min().unwrap_or(180_000);
            let sum: i64 = taken.iter().map(|t| i64::from(turn(*t, cand[i]))).sum();
            (nearest, sum, std::cmp::Reverse(i))
        };
        let best = *left.iter().max_by_key(|i| key(i)).expect("non-empty");
        left.retain(|i| *i != best);
        taken.push(cand[best]);
        out.push(best);
    }
    out
}

fn spokes_along(item: &CopperItem, r: &Relief, dirs: &[Angle]) -> Vec<Polygon> {
    let Some(b) = item.shape.bbox() else { return vec![] };
    let (gap, w) = (r.gap, r.w);
    let (cx, cy) = (item.anchor.x.0, item.anchor.y.0);
    let h = w / 2;
    let reach = gap + w;
    dirs.iter()
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
/// width, and by copper nowhere narrower than the relief's minimum (`Relief::min`: what is
/// left of a spoke is still a track that can be made). Otherwise nothing.
pub fn trimmed_spoke(
    it: &CopperItem,
    r: &Relief,
    spoke: &Polygon,
    avail: &PolygonSet,
    fill: &PolygonSet,
) -> Result<PolygonSet, poly::Error> {
    trimmed_spoke_in(it, r, spoke, avail, fill, &mut None)
}

/// [`trimmed_spoke`], with the pad's thermal gap as a region kept in `gap` from one spoke of
/// the pad to the next: it is the same for each of them, and made when the first needs it.
fn trimmed_spoke_in(
    it: &CopperItem,
    r: &Relief,
    spoke: &Polygon,
    avail: &PolygonSet,
    fill: &PolygonSet,
    gap: &mut Option<PolygonSet>,
) -> Result<PolygonSet, poly::Error> {
    if !poly::intersects(fill, spoke) {
        return Ok(vec![]);
    }
    let gap: &PolygonSet = match gap {
        Some(g) => g,
        None => gap.insert(poly::offset(&it.shape, r.gap, Join::Round, OBSTACLE_TOL)?),
    };
    let room = Boolean::new().subject(fill, FillRule::NonZero).subject(gap, FillRule::NonZero).execute()?;
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
    // No narrower than a track may be: shrunk by half the minimum width (less the slack of
    // the arcs), a piece must still reach from the pad to the pour.
    let shrink = r.min / 2 - 2 * FILL_TOL.tolerance;
    if shrink > r.w / 4 {
        let core = poly::offset(&piece, -shrink, Join::Round, FILL_TOL)?;
        if !core.iter().any(|q| poly::intersects(q, &it.shape) && poly::intersects(q, fill)) {
            return Ok(vec![]);
        }
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

/// A pad's own connection on `layer`: the one set for that layer alone, else the pad's.
pub fn own_connection(it: &CopperItem, layer: &str) -> Option<PadConnection> {
    it.local.connection_on.iter().find(|(l, _)| l == layer).map(|(_, c)| *c).or(it.local.connection)
}

/// [`connection`] on one layer ([`own_connection`]).
pub fn connection_on(it: &CopperItem, layer: &str, zone: PadConnection) -> PadConnection {
    match own_connection(it, layer).unwrap_or(zone) {
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

/// Everything the relief of one pad in one fill depends on ([`pad_spokes`]).
#[derive(Clone, PartialEq, Eq)]
struct PadKey {
    /// The pad.
    item: CopperItem,
    /// Its relief.
    relief: Relief,
    /// The allowed area and the pour, clipped to the windows of the layer's relieved pads:
    /// the polygons that meet the box of the pad's spokes, in every direction of the relief.
    avail: PolygonSet,
    fill: PolygonSet,
    /// The other pads of the relief whose windows meet that box, in their order: window and
    /// shape.
    others: Vec<(poly::Rect, PolygonSet)>,
}

impl PadKey {
    /// A number to find the key by; equal keys have equal numbers.
    fn hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.item.anchor.x.0, self.item.anchor.y.0).hash(&mut h);
        self.item.shape.hash(&mut h);
        self.avail.hash(&mut h);
        self.fill.hash(&mut h);
        self.others.len().hash(&mut h);
        h.finish()
    }

    /// How many points it holds, for the memo's bound.
    fn points(&self) -> usize {
        let of = |s: &PolygonSet| s.iter().map(Polygon::vertex_count).sum::<usize>();
        of(&self.item.shape) + of(&self.avail) + of(&self.fill) + self.others.iter().map(|(_, s)| of(s)).sum::<usize>()
    }
}

/// What a pad's relief comes to: the spokes kept, whether one of them joins the pad to the
/// pour, and the spokes that only reach another same-net pad of the relief (a row of pads,
/// each in the others' gaps), by that pad's position in [`PadKey::others`].
#[derive(Clone, Default)]
struct PadSpokes {
    kept: Vec<Polygon>,
    joined: bool,
    to_pads: Vec<(usize, Polygon)>,
}

/// The relief of one pad: a function of `key` alone (`main` and `alt` are its spokes,
/// [`spokes`] and [`alt_spokes`] of its pad and relief), so that a pad whose surroundings are
/// as they were in an earlier fill has the spokes it had there.
fn pad_spokes(key: &PadKey, main: Vec<Polygon>, alt: Vec<Polygon>) -> Result<PadSpokes, poly::Error> {
    let (it, r) = (&key.item, &key.relief);
    let near = |set: &PolygonSet, r: &poly::Rect| -> PolygonSet {
        set.iter().filter(|q| q.bbox().is_some_and(|b| b.intersects(r))).cloned().collect()
    };
    let fits = |s: &Polygon| {
        let b = s.bbox().expect("spoke");
        poly::contains(&near(&key.avail, &b), s) && poly::intersects(&near(&key.fill, &b), s)
    };
    let mut out = PadSpokes::default();
    // The pad's thermal gap as a region, made when the first spoke needs it.
    let mut gap: Option<PolygonSet> = None;
    // A spoke whose end would cross another item's clearance is trimmed instead of dropped
    // when the rest still joins the pour (`trimmed_spoke`).
    let mut taken: Vec<Angle> = Vec::new();
    let mut mine: Vec<Polygon> = Vec::new();
    for (s, dir) in main.into_iter().zip(&r.dirs) {
        let b = s.bbox().expect("spoke");
        if fits(&s) {
            mine.push(s.clone());
            out.kept.push(s);
            out.joined = true;
            taken.push(*dir);
            continue;
        }
        let t = trimmed_spoke_in(it, r, &s, &near(&key.avail, &b), &near(&key.fill, &b), &mut gap)?;
        if !t.is_empty() {
            mine.extend(t.iter().cloned());
            out.kept.extend(t);
            out.joined = true;
            taken.push(*dir);
            continue;
        }
        if !poly::contains(&near(&key.avail, &b), &s) {
            continue;
        }
        if let Some(o) = key.others.iter().position(|(w, shape)| w.intersects(&b) && poly::intersects(shape, &s)) {
            mine.push(s.clone());
            out.to_pads.push((o, s));
            taken.push(*dir);
        }
    }
    // Spokes that did not survive are tried in the relief's other directions, whole or
    // trimmed as above, and the ones that spread best around the pad are kept, up to the
    // spoke count (D112). One that meets a kept spoke outside the pad is left out: on a small
    // pad it would close the gap between two spokes, and the pad would have more copper on
    // its outline, not another neck.
    if taken.len() < r.dirs.len() && !r.alt.is_empty() {
        let apart = |t: &PolygonSet| -> Result<bool, poly::Error> {
            if mine.is_empty() {
                return Ok(true);
            }
            let both = Boolean::new()
                .subject(t, FillRule::NonZero)
                .clip(&mine, FillRule::NonZero)
                .op(Op::Intersection)
                .execute()?;
            Ok(both.iter().all(|q| poly::contains(&it.shape, q)))
        };
        let mut found: Vec<(Angle, PolygonSet)> = Vec::new();
        for (s, dir) in alt.into_iter().zip(&r.alt) {
            let b = s.bbox().expect("spoke");
            let t = if fits(&s) {
                vec![s]
            } else {
                trimmed_spoke_in(it, r, &s, &near(&key.avail, &b), &near(&key.fill, &b), &mut gap)?
            };
            if !t.is_empty() && apart(&t)? {
                found.push((*dir, t));
            }
        }
        let cand: Vec<Angle> = found.iter().map(|(d, _)| *d).collect();
        for i in spread(&taken, &cand, r.dirs.len() - taken.len()) {
            out.kept.extend(found[i].1.iter().cloned());
            out.joined = true;
        }
    }
    Ok(out)
}

/// The reliefs made in this process by what they were made from ([`PadKey`]), in two ages: one
/// that is found again is of the newer, and when that holds [`SPOKE_POINTS`] the older goes.
/// A fill of a board that differs from an earlier one in a few places makes only the reliefs
/// near them again, which is most of what a fill of a plane with hundreds of pads takes
/// (`docs/BOARD.md`, "Zone fill").
#[derive(Default)]
struct SpokeMemo {
    now: BTreeMap<u64, Vec<(PadKey, PadSpokes)>>,
    before: BTreeMap<u64, Vec<(PadKey, PadSpokes)>>,
    points: usize,
}

/// The most points the newer age of [`SpokeMemo`] holds (16 bytes each, and the reliefs).
const SPOKE_POINTS: usize = 4_000_000;

impl SpokeMemo {
    fn get(&mut self, hash: u64, key: &PadKey) -> Option<PadSpokes> {
        if let Some((_, v)) = self.now.get(&hash).and_then(|b| b.iter().find(|(k, _)| k == key)) {
            return Some(v.clone());
        }
        let bucket = self.before.get_mut(&hash)?;
        let (k, v) = bucket.swap_remove(bucket.iter().position(|(k, _)| k == key)?);
        self.put(hash, k, v.clone());
        Some(v)
    }

    fn put(&mut self, hash: u64, key: PadKey, made: PadSpokes) {
        if self.points > SPOKE_POINTS {
            self.before = std::mem::take(&mut self.now);
            self.points = 0;
        }
        self.points += key.points() + made.kept.iter().map(Polygon::vertex_count).sum::<usize>();
        self.now.entry(hash).or_default().push((key, made));
    }
}

static SPOKES: std::sync::Mutex<Option<SpokeMemo>> = std::sync::Mutex::new(None);

/// Something done with the memo of reliefs; `None` if it cannot be had.
fn spoke_memo<T>(f: impl FnOnce(&mut SpokeMemo) -> Option<T>) -> Option<T> {
    f(SPOKES.lock().ok()?.get_or_insert_with(SpokeMemo::default))
}

/// Fills one zone layer. The result is canonical (deterministic).
pub fn fill_layer(input: &LayerInput<'_>) -> Result<PolygonSet, poly::Error> {
    fill_layer_with(input, false, None)
}

/// What a fill makes of one copper item, and nothing else of the item: a zone layer whose
/// items are these, in this order, has the same fill ([`LayerKey`]).
#[derive(Clone, PartialEq, Eq)]
struct ItemKey {
    /// Of the zone's net.
    same: bool,
    /// A pad.
    pad: bool,
    shape: PolygonSet,
    /// Of a same-net pad: where its spokes start, how it joins, its relief.
    anchor: (i64, i64),
    connection: Option<PadConnection>,
    through: bool,
    thermal: Option<crate::board::PadThermal>,
    /// Of an item of another net: the clearance kept from it.
    clearance: i64,
}

/// Everything the fill of one zone layer is made from ([`fill_layer`] is a function of it).
#[derive(Clone, PartialEq, Eq)]
struct LayerKey {
    net: bool,
    outline: Vec<poly::Point>,
    board: Option<PolygonSet>,
    items: Vec<ItemKey>,
    keepaway: Vec<Polygon>,
    params: ZoneParams,
}

impl LayerKey {
    fn of(input: &LayerInput<'_>) -> LayerKey {
        let items = input
            .items
            .iter()
            .map(|(it, c)| {
                let same = same_net(it.net.as_deref(), input.net);
                let pad = matches!(it.item, ItemRef::Pad(..));
                if same && pad {
                    ItemKey {
                        same,
                        pad,
                        shape: it.shape.clone(),
                        anchor: (it.anchor.x.0, it.anchor.y.0),
                        connection: it.local.connection,
                        through: it.local.through,
                        thermal: it.local.thermal,
                        clearance: 0,
                    }
                } else {
                    ItemKey {
                        same,
                        pad,
                        shape: it.shape.clone(),
                        anchor: (0, 0),
                        connection: None,
                        through: false,
                        thermal: None,
                        clearance: if same { 0 } else { c.0 },
                    }
                }
            })
            .collect();
        LayerKey {
            net: input.net.is_some(),
            outline: input.outline.to_vec(),
            board: input.board.cloned(),
            items,
            keepaway: input.keepaway.clone(),
            params: input.params,
        }
    }

    fn points(&self) -> usize {
        let of = |s: &[Polygon]| s.iter().map(Polygon::vertex_count).sum::<usize>();
        self.outline.len()
            + self.board.as_deref().map_or(0, of)
            + self.items.iter().map(|i| of(&i.shape)).sum::<usize>()
            + of(&self.keepaway)
    }
}

/// What is kept of the last fill of one zone on one layer: everything it was made from, the
/// fill, and its allowed area (the zone's area less other nets' copper and the keep-aways,
/// which a change of a pad's relief leaves as it was).
struct LayerSlot {
    key: LayerKey,
    fill: Option<PolygonSet>,
    avail: PolygonSet,
}

impl LayerKey {
    /// Whether the allowed area of a fill made from `self` is that of one made from `o`:
    /// the same area, the same copper of other nets with the same clearances, in the same
    /// order, and the same keep-aways.
    fn same_allowed(&self, o: &LayerKey) -> bool {
        let (a, b) = (self.items.iter().filter(|i| !i.same), o.items.iter().filter(|i| !i.same));
        self.net == o.net
            && self.outline == o.outline
            && self.board == o.board
            && self.params.clearance == o.params.clearance
            && self.keepaway == o.keepaway
            && a.map(|i| (&i.shape, i.clearance)).eq(b.map(|i| (&i.shape, i.clearance)))
    }
}

impl LayerSlot {
    fn points(&self) -> usize {
        let of = |s: &[Polygon]| s.iter().map(Polygon::vertex_count).sum::<usize>();
        self.key.points() + self.fill.as_deref().map_or(0, of) + of(&self.avail)
    }
}

/// The last fill of each zone layer made in this process ([`LayerSlot`]): a fill of a board
/// that differs from an earlier one takes the zone layers whose every input is as it was, and
/// of the others the allowed area where that is (D119). Emptied when it holds
/// [`LAYER_POINTS`].
#[derive(Default)]
struct LayerMemo {
    slots: BTreeMap<(ObjectId, String), LayerSlot>,
    points: usize,
}

/// The most points [`LayerMemo`] holds (16 bytes each).
const LAYER_POINTS: usize = 10_000_000;

impl LayerMemo {
    fn put(&mut self, slot: &(ObjectId, String), new: LayerSlot) {
        let add = new.points();
        if let Some(old) = self.slots.remove(slot) {
            self.points = self.points.saturating_sub(old.points());
        }
        if self.points + add > LAYER_POINTS {
            self.slots.clear();
            self.points = 0;
        }
        if add <= LAYER_POINTS {
            self.points += add;
            self.slots.insert(slot.clone(), new);
        }
    }
}

static LAYERS: std::sync::Mutex<Option<LayerMemo>> = std::sync::Mutex::new(None);

fn layer_memo<T>(f: impl FnOnce(&mut LayerMemo) -> Option<T>) -> Option<T> {
    f(LAYERS.lock().ok()?.get_or_insert_with(LayerMemo::default))
}

/// [`fill_layer`]; with `reuse`, the reliefs of pads whose surroundings are as they were in an
/// earlier fill of this process are taken from it ([`SpokeMemo`]), and with `allowed` the
/// allowed area is the one given when it holds one (that of an earlier fill made from the
/// same area, copper of other nets and keep-aways, [`LayerMemo`]) and is left there when it
/// does not; either gives the same fill.
fn fill_layer_with(
    input: &LayerInput<'_>,
    reuse: bool,
    allowed: Option<&mut Option<PolygonSet>>,
) -> Result<PolygonSet, poly::Error> {
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
    // The allowed copper (where spokes may go) is the area less the hard keep-away.
    let avail = match allowed {
        Some(Some(kept)) => kept.clone(),
        allowed => {
            let mut hard: Vec<Polygon> = input.keepaway.clone();
            // Item outlines may overlap (a teardrop over its pad): merge each group first so
            // `offset` gets canonical input; its linear "looks canonical" check cannot see
            // overlaps, and growing overlapping rings unmerged rounds differently
            // (docs/POLYGON_LIB.md §8).
            for (c, shapes) in hard_groups {
                let shapes = Boolean::new().subject(&shapes, FillRule::NonZero).op(Op::Union).execute()?;
                hard.extend(poly::offset(&shapes, c + SAFETY, Join::Round, OBSTACLE_TOL)?);
            }
            let avail = Boolean::new()
                .subject(&area, FillRule::NonZero)
                .clip(&hard, FillRule::NonZero)
                .op(Op::Difference)
                .execute()?;
            if let Some(out) = allowed {
                *out = Some(avail.clone());
            }
            avail
        }
    };
    let mut soft: Vec<Polygon> = Vec::new();
    for (d, shapes) in soft_groups {
        let shapes = Boolean::new().subject(&shapes, FillRule::NonZero).op(Op::Union).execute()?;
        soft.extend(poly::offset(&shapes, d, Join::Round, OBSTACLE_TOL)?);
    }

    // The pour proper.
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
        let (avail_index, fill_index) =
            (RTree::new(local_avail.iter().map(Polygon::bbox)), RTree::new(local_fill.iter().map(Polygon::bbox)));
        // A pad's relief depends on the pad, on the allowed area and the pour where its
        // spokes can be, and on the same-net pads of the relief near them ([`PadKey`]): each
        // pad on its own, so side by side with the `parallel` feature, put together in the
        // order of the pads, and reused from an earlier fill where all of that is as it was
        // (`reuse`).
        let per_pad = |k: usize| -> Result<(PadSpokes, Vec<usize>), poly::Error> {
            let (it, r) = &thermal[k];
            let (main, alt) = (spokes(it, r), alt_spokes(it, r));
            let Some(reach) = main.iter().chain(&alt).filter_map(Polygon::bbox).reduce(|a, b| a.union(&b)) else {
                return Ok((PadSpokes::default(), vec![]));
            };
            // In the order of the sets, as a spoke's own selection from them is.
            let part = |set: &PolygonSet, index: &RTree| -> PolygonSet {
                index.query(&reach).into_iter().map(|i| set[i as usize].clone()).collect()
            };
            let others: Vec<usize> = index.query(&reach).into_iter().map(|o| o as usize).filter(|&o| o != k).collect();
            let key = PadKey {
                item: (*it).clone(),
                relief: r.clone(),
                avail: part(&local_avail, &avail_index),
                fill: part(&local_fill, &fill_index),
                others: others.iter().map(|&o| (boxes[o], thermal[o].0.shape.clone())).collect(),
            };
            if !reuse {
                return Ok((pad_spokes(&key, main, alt)?, others));
            }
            let hash = key.hash();
            if let Some(found) = spoke_memo(|m| m.get(hash, &key)) {
                return Ok((found, others));
            }
            let made = pad_spokes(&key, main, alt)?;
            spoke_memo(|m| {
                m.put(hash, key, made.clone());
                Some(())
            });
            Ok((made, others))
        };
        #[cfg(feature = "parallel")]
        let per: Vec<Result<(PadSpokes, Vec<usize>), poly::Error>> = {
            use rayon::prelude::*;
            (0..thermal.len()).into_par_iter().map(per_pad).collect()
        };
        #[cfg(not(feature = "parallel"))]
        let per: Vec<Result<(PadSpokes, Vec<usize>), poly::Error>> = (0..thermal.len()).map(per_pad).collect();
        // Pads joined to the pour by a spoke of their own, and spokes that only reach another
        // same-net pad of the relief (a row of pads, each in the others' gaps): (pad, other
        // pad, spoke).
        let mut joined = vec![false; thermal.len()];
        let mut pad_to_pad: Vec<(usize, usize, Polygon)> = Vec::new();
        for (k, pad) in per.into_iter().enumerate() {
            let (pad, others) = pad?;
            kept.extend(pad.kept);
            joined[k] = pad.joined;
            pad_to_pad.extend(pad.to_pads.into_iter().map(|(o, s)| (k, others[o], s)));
        }
        // A spoke reaching a pad that the pour joins (directly or through such spokes) carries
        // the connection on, as copper between the two pads: kept until nothing changes.
        let mut used = vec![false; pad_to_pad.len()];
        loop {
            let mut changed = false;
            for (i, (a, b, s)) in pad_to_pad.iter().enumerate() {
                if !used[i] && (joined[*a] || joined[*b]) {
                    used[i] = true;
                    joined[*a] = true;
                    joined[*b] = true;
                    kept.push(s.clone());
                    changed = true;
                }
            }
            if !changed {
                break;
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
    npth: Vec<DrillHole>,
    /// The clearance each base item keeps (custom rules, local and class clearances: they
    /// depend on library footprints too, which the board does not hold).
    item_c: Vec<Nm>,
    fills: Vec<ZoneFill>,
}

impl MemoEntry {
    fn matches(&self, p: &Project, base: &[CopperItem], npth: &[DrillHole], item_c: &[Nm]) -> bool {
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
/// inputs are reused within the process, and of a board that differs from one filled before,
/// the reliefs of the pads whose surroundings are as they were (see [`fill_zones_uncached`],
/// which gives the same fills from scratch).
pub fn fill_zones(p: &Project, base: &[CopperItem]) -> Vec<ZoneFill> {
    if p.board().zones.is_empty() {
        return vec![];
    }
    let npth = drill_holes(p);
    let item_c = item_clearances(p, base);
    if let Ok(m) = MEMO.lock()
        && let Some(e) = m.iter().find(|e| e.matches(p, base, &npth, &item_c))
    {
        return e.fills.clone();
    }
    let fills = fill_with(p, base, &npth, &item_c, Reuse::Keep);
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

/// Forgets the fills and the reliefs kept in process by [`fill_zones`] (benchmarks measure cold
/// runs with it).
pub fn clear_fill_cache() {
    if let Ok(mut m) = MEMO.lock() {
        m.clear();
    }
    if let Ok(mut m) = SPOKES.lock() {
        *m = None;
    }
    if let Ok(mut m) = LAYERS.lock() {
        *m = None;
    }
    super::thermal::clear_joint_cache();
}

/// [`fill_zones`] for a board that is looked at once and not again (the pours as they would
/// be with every pad solid, which `thermal::relieve` looks for tracks in): the same fills,
/// made with what earlier fills left and not kept as a board's fills are.
pub fn fill_zones_once(p: &Project, base: &[CopperItem]) -> Vec<ZoneFill> {
    fill_with(p, base, &drill_holes(p), &item_clearances(p, base), Reuse::Take)
}

/// [`fill_zones`] without the in-process reuse, of whole fills or of parts of them.
pub fn fill_zones_uncached(p: &Project, base: &[CopperItem]) -> Vec<ZoneFill> {
    fill_with(p, base, &drill_holes(p), &item_clearances(p, base), Reuse::None)
}

/// The clearance the DRC requires around each base item: its custom rule's, else its local
/// clearance, else its net's (class, else the rules'), never below `min_clearance`. A zone
/// with a smaller clearance of its own still keeps this much, so a fill never violates the DRC.
fn item_clearances(p: &Project, base: &[CopperItem]) -> Vec<Nm> {
    let resolved = super::clearance::Clearances::new(p, base, true);
    (0..base.len()).map(|i| resolved.item(i)).collect()
}

/// A drilled hole fills keep away from: the ends of its slot (equal for a round hole), its
/// radius, and for a plated hole its net (`Some`; `None` for a non-plated hole).
type DrillHole = (poly::Point, poly::Point, i64, Option<Option<String>>);

/// Non-plated holes, and with a hole clearance in the rules the plated ones (vias, through-hole
/// pads) too.
fn drill_holes(p: &Project) -> Vec<DrillHole> {
    let plated_too = p.board().rules.hole_clearance > Nm::ZERO;
    let mut out: Vec<DrillHole> = placed_pads(p)
        .into_iter()
        .filter_map(|pp| match pp.hole {
            Some((d, plated)) if !plated || plated_too => {
                let (a, b) = pp.slot.unwrap_or((pp.hole_at, pp.hole_at));
                Some((a.into(), b.into(), d.0 / 2, plated.then(|| pp.net.clone())))
            }
            _ => None,
        })
        .collect();
    if plated_too {
        for v in &p.board().vias {
            out.push((v.at.into(), v.at.into(), v.drill.0 / 2, Some(v.net.clone())));
        }
    }
    out
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

/// What a fill takes from earlier fills of this process and leaves for later ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reuse {
    /// Nothing: every zone layer from scratch.
    None,
    /// Takes what is kept and keeps what it makes: zone layers and their allowed areas
    /// ([`LayerMemo`]), reliefs of pads ([`SpokeMemo`]).
    Keep,
    /// Takes what is kept, and keeps reliefs and allowed areas but not its zone layers: a
    /// board looked at once.
    Take,
}

/// The fills; with `reuse`, parts of earlier fills are taken where everything they were made
/// from is as it was: whole zone layers, their allowed areas, the reliefs of pads.
fn fill_with(p: &Project, base: &[CopperItem], npth: &[DrillHole], item_c: &[Nm], reuse: Reuse) -> Vec<ZoneFill> {
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
        // A pad whose connection is set for this layer alone is filled as a pad with that
        // connection of its own.
        let on_layer: Vec<Option<CopperItem>> = base
            .iter()
            .map(|it| {
                let c = it.local.connection_on.iter().find(|(l, _)| l == layer)?.1;
                let mut own = it.clone();
                own.local.connection = Some(c);
                Some(own)
            })
            .collect();
        let items_on: Vec<(&CopperItem, Nm)> = base
            .iter()
            .zip(&on_layer)
            .zip(item_c)
            .filter(|((it, _), _)| it.layers.iter().any(|l| l == layer))
            .map(|((it, own), c)| (own.as_ref().unwrap_or(it), *c))
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
                for (a, b, r, plated) in npth {
                    let (a, b) = (*a, *b);
                    // A hole is no copper: the copper-to-hole clearance applies when set.
                    let c = match plated {
                        None if rules.hole_clearance > Nm::ZERO => rules.hole_clearance.0,
                        None => prm.clearance.0,
                        Some(n) if same_net(n.as_deref(), z.net.as_deref()) => continue,
                        Some(_) => rules.hole_clearance.0,
                    };
                    let r = r + c + SAFETY;
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
                    // A pour of an excepted net fills the keep-out.
                    if k.no_pours
                        && !k.allows(z.net.as_deref())
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
                let input = LayerInput {
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
                };
                if reuse == Reuse::None {
                    return fill_layer_with(&input, false, None);
                }
                // The whole zone layer, when everything it is made from is as it was; else its
                // allowed area, when what that is made from is.
                let slot = (z.id, layer.to_string());
                let key = LayerKey::of(&input);
                let mut allowed: Option<PolygonSet> = None;
                if let Some(found) = layer_memo(|m| {
                    let kept = m.slots.get(&slot)?;
                    if kept.key == key
                        && let Some(fill) = &kept.fill
                    {
                        return Some(fill.clone());
                    }
                    if kept.key.same_allowed(&key) {
                        allowed = Some(kept.avail.clone());
                    }
                    None
                }) {
                    return Ok(found);
                }
                // An early return of `fill_layer_with` (no area, nothing of the net) leaves no
                // allowed area: nothing is kept then.
                let made = fill_layer_with(&input, true, Some(&mut allowed))?;
                if let Some(avail) = allowed {
                    // A board looked at once leaves its allowed areas and not its fills.
                    let fill = (reuse == Reuse::Keep).then(|| made.clone());
                    layer_memo(|m| {
                        if fill.is_none()
                            && let Some(kept) = m.slots.get(&slot)
                            && kept.fill.is_some()
                            && kept.key.same_allowed(&key)
                        {
                            return Some(());
                        }
                        m.put(&slot, LayerSlot { key, fill, avail });
                        Some(())
                    });
                }
                Ok(made)
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
    fills_as_items(fill_zones(p, base))
}

/// Fills as copper items, as [`zone_items`] gives those of the board.
pub fn fills_as_items(fills: Vec<ZoneFill>) -> Vec<CopperItem> {
    let mut out = Vec::new();
    for f in fills {
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

/// The islands of a fill, largest first: area in mm² and a point on each (a vertex of its
/// outline), so a report can say where the small pieces are.
pub fn islands(s: &PolygonSet) -> Vec<(f64, crate::geom::Point)> {
    let mut v: Vec<(f64, crate::geom::Point)> = s
        .iter()
        .map(|q| {
            let at = q.any_point().map(|p| crate::geom::Point::new(Nm(p.x), Nm(p.y))).unwrap_or_default();
            (poly::area2(q) as f64 / 2.0 / 1e12, at)
        })
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| (a.1.x, a.1.y).cmp(&(b.1.x, b.1.y))));
    v
}

#[cfg(test)]
mod tests;
