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
//! of booleans, so the approximation never creates a clearance violation. Fills are derived
//! data: recomputed on demand and never stored on disk; the last few results are reused in process
//! when every input is identical ([`fill_zones`]). Timings in `docs/BOARD.md`.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use crate::board::{CopperItem, ItemRef, contour_ring, placed_pads};
use crate::geom::poly::{self, ArcTol, Boolean, Circle, FillRule, Geometry, Join, Op, Polygon, PolygonSet, Ring, Side};
use crate::id::ObjectId;
use crate::model::Project;
use crate::model::board::{PadConnection, Zone};
use crate::units::Nm;

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
}

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

/// The four axis-aligned spokes of a thermal relief around a pad: from the pad center to
/// `spoke` beyond the gap.
fn spokes(item: &CopperItem, gap: i64, w: i64) -> Vec<Polygon> {
    let Some(b) = item.shape.bbox() else { return vec![] };
    let (cx, cy) = (item.anchor.x.0, item.anchor.y.0);
    let h = w / 2;
    let reach = gap + w;
    vec![
        rect(cx - h, cy - h, b.max.x + reach, cy + (w - h)),
        rect(cx - h, cy - h, cx + (w - h), b.max.y + reach),
        rect(b.min.x - reach, cy - h, cx + (w - h), cy + (w - h)),
        rect(cx - h, b.min.y - reach, cx + (w - h), cy + (w - h)),
    ]
}

fn same_net(a: Option<&str>, b: Option<&str>) -> bool {
    a.is_some() && a == b
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
    let mut thermal: Vec<&CopperItem> = Vec::new();
    let mut same: Vec<&CopperItem> = Vec::new();
    for &(it, c) in &input.items {
        if same_net(it.net.as_deref(), input.net) {
            same.push(it);
            if let ItemRef::Pad(..) = it.item {
                let d = match prm.pads {
                    PadConnection::Solid => continue,
                    PadConnection::Thermal => {
                        thermal.push(it);
                        prm.thermal_gap.0
                    }
                    PadConnection::None => prm.clearance.0,
                };
                soft_groups.entry(d).or_default().extend(it.shape.iter().cloned());
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
    for (c, shapes) in hard_groups {
        hard.extend(poly::offset(&shapes, c + SAFETY, Join::Round, OBSTACLE_TOL)?);
    }
    let mut soft: Vec<Polygon> = Vec::new();
    for (d, shapes) in soft_groups {
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
        let (gap, w) = (prm.thermal_gap.0, prm.thermal_spoke.0);
        let windows: Vec<Polygon> = thermal
            .iter()
            .filter_map(|it| it.shape.bbox())
            .map(|b| {
                let b = b.expand(gap + w + 1);
                rect(b.min.x, b.min.y, b.max.x, b.max.y)
            })
            .collect();
        let clip = |set: &PolygonSet| {
            Boolean::new()
                .subject(set, FillRule::NonZero)
                .clip(&windows, FillRule::NonZero)
                .op(Op::Intersection)
                .execute()
        };
        let (local_avail, local_fill) = (clip(&avail)?, clip(&fill)?);
        let near = |set: &PolygonSet, r: &poly::Rect| -> PolygonSet {
            set.iter().filter(|q| q.bbox().is_some_and(|b| b.intersects(r))).cloned().collect()
        };
        for it in &thermal {
            for s in spokes(it, gap, w) {
                let r = s.bbox().expect("spoke");
                if poly::contains(&near(&local_avail, &r), &s) && poly::intersects(&near(&local_fill, &r), &s) {
                    kept.push(s);
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

/// Stable 64-bit FNV-1a (std's hasher is not stable across releases).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Everything a fill depends on, as bytes: the board (zones, keep-outs, outline, rules, ...), the
/// non-zone copper, NPTH holes and net classes.
fn memo_key(p: &Project, base: &[CopperItem], npth: &[(poly::Point, i64)]) -> Vec<u8> {
    let mut k = serde_json::to_vec(p.board()).unwrap_or_default();
    let c = p.circuit();
    k.extend(serde_json::to_vec(&c.netclasses).unwrap_or_default());
    for (name, net) in &c.nets {
        k.extend(name.as_bytes());
        k.push(0);
        k.extend(net.class.as_deref().unwrap_or("").as_bytes());
        k.push(0);
    }
    let num = |k: &mut Vec<u8>, v: i64| k.extend(v.to_le_bytes());
    for it in base {
        k.extend(it.item.to_string().as_bytes());
        k.push(0);
        k.extend(it.net.as_deref().unwrap_or("\u{1}").as_bytes());
        k.push(0);
        for l in &it.layers {
            k.extend(l.as_bytes());
            k.push(0);
        }
        num(&mut k, it.anchor.x.0);
        num(&mut k, it.anchor.y.0);
        for q in &it.shape {
            for r in q.rings() {
                num(&mut k, r.0.len() as i64);
                for v in &r.0 {
                    num(&mut k, v.x);
                    num(&mut k, v.y);
                }
            }
        }
        k.push(0xff);
    }
    for &(c, r) in npth {
        num(&mut k, c.x);
        num(&mut k, c.y);
        num(&mut k, r);
    }
    k
}

/// Recently computed fills (in-process): (hash, exact key, fills). Fills are derived data,
/// recomputed whenever any input differs; the full key comparison rules out hash collisions.
type MemoEntry = (u64, Vec<u8>, Vec<ZoneFill>);
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
    let key = memo_key(p, base, &npth);
    let h = fnv1a(&key);
    if let Ok(m) = MEMO.lock()
        && let Some((_, _, f)) = m.iter().find(|(eh, ek, _)| *eh == h && *ek == key)
    {
        return f.clone();
    }
    let fills = fill_with(p, base, &npth);
    if let Ok(mut m) = MEMO.lock() {
        m.retain(|(eh, ek, _)| !(*eh == h && *ek == key));
        m.insert(0, (h, key, fills.clone()));
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
    fill_with(p, base, &npth_holes(p))
}

fn npth_holes(p: &Project) -> Vec<(poly::Point, i64)> {
    placed_pads(p)
        .into_iter()
        .filter_map(|pp| match pp.hole {
            Some((d, false)) => Some((pp.center.into(), d.0 / 2)),
            _ => None,
        })
        .collect()
}

fn fill_with(p: &Project, base: &[CopperItem], npth: &[(poly::Point, i64)]) -> Vec<ZoneFill> {
    let board = p.board();
    if board.zones.is_empty() {
        return vec![];
    }
    let rules = &board.rules;
    let area = board_area(p, rules.copper_to_edge);
    // Per item: the clearance DRC requires around its net (its class's, else the rules'),
    // resolved once. A zone with a smaller clearance of its own still keeps this much, so a
    // fill never violates the DRC.
    let net_c = |n: Option<&str>| class_clearance(p, n).unwrap_or(rules.clearance);
    let mut class_c: BTreeMap<Option<&str>, Nm> = BTreeMap::new();
    for it in base {
        let n = it.net.as_deref();
        class_c.entry(n).or_insert_with(|| net_c(n));
    }
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
            .filter(|it| it.layers.iter().any(|l| l == layer))
            .map(|it| (it, class_c[&it.net.as_deref()]))
            .collect();
        let mut done: Vec<(usize, usize, ZoneFill)> = Vec::new();
        // Earlier fills grown by a keep-away distance, by (index in `done`, distance): later
        // zones of the layer often need the same ones.
        let mut grown: BTreeMap<(usize, i64), PolygonSet> = BTreeMap::new();
        for &(zi, li) in jobs {
            let z = &board.zones[zi];
            let mut prm = zone_params(p, z);
            // The DRC clearance of the zone's own net applies to its copper too.
            prm.clearance = prm.clearance.max(net_c(z.net.as_deref()));
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
                for &(c, r) in npth {
                    let ring = Circle::new(c, r + prm.clearance.0 + SAFETY).to_ring(OBSTACLE_TOL)?;
                    keepaway.push(Polygon::new(ring, vec![]));
                }
                for (k, (_, _, f)) in done.iter().enumerate() {
                    if !same_net(f.net.as_deref(), z.net.as_deref()) && !f.fill.is_empty() {
                        let c = prm.clearance.max(f.clearance).0 + SAFETY;
                        let g = match grown.entry((k, c)) {
                            Entry::Occupied(e) => e.into_mut(),
                            Entry::Vacant(e) => e.insert(poly::offset(&f.fill, c, Join::Round, OBSTACLE_TOL)?),
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
                        if !ring.is_ccw() {
                            ring.reverse_orientation();
                        }
                        keepaway.push(Polygon::new(ring, vec![]));
                    }
                }
                let outline: Vec<poly::Point> = z.outline.iter().map(|&q| q.into()).collect();
                fill_layer(&LayerInput {
                    net: z.net.as_deref(),
                    outline: &outline,
                    board: barea,
                    items: items_on.clone(),
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
