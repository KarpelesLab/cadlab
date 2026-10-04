//! Free-space decomposition for the gridless search (`expansion`): the *rooms* of a layer.
//!
//! For a net on a layer, every obstacle a track centerline of the net must keep away from
//! (other nets' pads and copper, keep-outs, holes, the board edge and, for exact searches, the
//! routing of other nets) is grown into its *clearance region*: the shape widened by the
//! distance the rules require from the centerline plus a 0.5 µm margin, as an outer polygon
//! (convex hulls widened by an octagon whose inradius is that distance, per triangle for
//! non-convex shapes). The free space is the board (within a search region) minus the union of
//! these regions, computed exactly on integers by `polyclip`; a centerline anywhere in it keeps
//! every clearance. It is split into trapezoids by a vertical decomposition (walls from every
//! vertex up and down to the nearest boundary), and trapezoids larger than a few track pitches
//! are cut into a lattice of smaller ones so that the search can choose its way through open
//! areas. These convex pieces are the *rooms*; rooms touching along a segment of positive
//! length are joined by a *portal* (the shared segment). Any segment between two points of one
//! room lies inside it, so a path that goes from room to room through portals is legal by
//! construction. Vias get the same treatment: the via-center free space (through vias, every
//! layer, hole-to-hole, no via in a pad) decomposed the same way gives the candidate via sites.
//!
//! Classic free-space (cell) decomposition for path planning (Chazelle 1987; de Berg et al.,
//! *Computational Geometry*, ch. 6 and 13), as used by shape-based PCB routers ("expansion
//! rooms"); written from those descriptions.

use std::sync::OnceLock;

use crate::geom::poly as pc;

use super::geo::{BoxF, P, Shape};
use super::index::{Index, Item};
use super::model::{ObKind, RouterBoard};
use super::shove::{MARGIN, octagon_hull, octagon_hull_pts};

/// A room: the convex trapezoid between `x0` and `x1` under the top edge (`yt0` at `x0` to
/// `yt1` at `x1`) and over the bottom edge (`yb0` to `yb1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Room {
    pub x0: f64,
    pub x1: f64,
    pub yb0: f64,
    pub yb1: f64,
    pub yt0: f64,
    pub yt1: f64,
}

impl Room {
    fn lerp(x0: f64, x1: f64, y0: f64, y1: f64, x: f64) -> f64 {
        if x1 - x0 <= 0.0 { y0 } else { y0 + (y1 - y0) * (x - x0) / (x1 - x0) }
    }

    /// Bottom edge height at `x`.
    pub fn yb(&self, x: f64) -> f64 {
        Room::lerp(self.x0, self.x1, self.yb0, self.yb1, x)
    }

    /// Top edge height at `x`.
    pub fn yt(&self, x: f64) -> f64 {
        Room::lerp(self.x0, self.x1, self.yt0, self.yt1, x)
    }

    /// Whether `p` lies in the room (within `eps`).
    pub fn contains(&self, p: P, eps: f64) -> bool {
        p.x >= self.x0 - eps && p.x <= self.x1 + eps && p.y >= self.yb(p.x) - eps && p.y <= self.yt(p.x) + eps
    }

    /// The center of the room.
    pub fn center(&self) -> P {
        let xm = (self.x0 + self.x1) / 2.0;
        P::new(xm, (self.yb(xm) + self.yt(xm)) / 2.0)
    }

    /// Bounding box.
    pub fn bbox(&self) -> BoxF {
        BoxF { min: P::new(self.x0, self.yb0.min(self.yb1)), max: P::new(self.x1, self.yt0.max(self.yt1)) }
    }
}

/// A segment of positive length shared by two rooms.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Portal {
    pub a: P,
    pub b: P,
    pub rooms: [u32; 2],
}

/// The rooms of a layer (or the via sites) within a region.
#[derive(Debug, Default)]
pub(crate) struct Rooms {
    pub rooms: Vec<Room>,
    pub portals: Vec<Portal>,
    origin: P,
    cell: f64,
    cols: usize,
    rows: usize,
    buckets: Vec<Vec<u32>>,
}

/// Shortest portal kept (nm).
const MIN_PORTAL: f64 = 2.0;

impl Rooms {
    /// Decomposes `free` (canonical polygons) into rooms no wider or taller than `max_room`
    /// (when positive), with their portals.
    pub fn from_free(free: &pc::PolygonSet, max_room: f64) -> Rooms {
        let traps = if free.is_empty() { Ok(vec![]) } else { pc::trapezoids(free) };
        let traps = traps.unwrap_or_default();
        let mut rooms: Vec<Room> = Vec::new();
        let mut portals: Vec<Portal> = Vec::new();
        for t in &traps {
            let y = |e: (pc::Point, pc::Point), x: i64| -> f64 {
                let (a, b) = e;
                a.y as f64 + (x - a.x) as f64 * (b.y - a.y) as f64 / (b.x - a.x) as f64
            };
            let base = Room {
                x0: t.x0 as f64,
                x1: t.x1 as f64,
                yb0: y(t.bottom, t.x0),
                yb1: y(t.bottom, t.x1),
                yt0: y(t.top, t.x0),
                yt1: y(t.top, t.x1),
            };
            split(base, max_room, &mut rooms, &mut portals);
        }
        // Vertical portals: rooms ending at x against rooms starting at x.
        let mut ends: Vec<(f64, f64, f64, u32)> = Vec::new();
        let mut starts: Vec<(f64, f64, f64, u32)> = Vec::new();
        for (i, r) in rooms.iter().enumerate() {
            ends.push((r.x1, r.yb1, r.yt1, i as u32));
            starts.push((r.x0, r.yb0, r.yt0, i as u32));
        }
        let key = |a: &(f64, f64, f64, u32), b: &(f64, f64, f64, u32)| {
            a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.3.cmp(&b.3))
        };
        ends.sort_by(key);
        starts.sort_by(key);
        // Per wall abscissa: the intervals ending there against those starting there (both
        // sorted bottom up, disjoint within each list).
        let (mut i, mut j) = (0, 0);
        while i < ends.len() && j < starts.len() {
            let x = ends[i].0;
            match x.total_cmp(&starts[j].0) {
                std::cmp::Ordering::Less => {
                    i += 1;
                    continue;
                }
                std::cmp::Ordering::Greater => {
                    j += 1;
                    continue;
                }
                std::cmp::Ordering::Equal => {}
            }
            let ie = ends[i..].iter().position(|e| e.0 != x).map_or(ends.len(), |k| i + k);
            let js = starts[j..].iter().position(|e| e.0 != x).map_or(starts.len(), |k| j + k);
            let (mut a, mut b) = (i, j);
            while a < ie && b < js {
                let (e, s) = (ends[a], starts[b]);
                let (lo, hi) = (e.1.max(s.1), e.2.min(s.2));
                if hi - lo > MIN_PORTAL {
                    portals.push(Portal { a: P::new(x, lo), b: P::new(x, hi), rooms: [e.3, s.3] });
                }
                if e.2 < s.2 {
                    a += 1;
                } else {
                    b += 1;
                }
            }
            i = ie;
            j = js;
        }
        let mut out = Rooms { rooms, portals, ..Default::default() };
        out.index();
        out
    }

    fn index(&mut self) {
        let mut b = BoxF::EMPTY;
        for r in &self.rooms {
            b = b.union(r.bbox());
        }
        if b.is_empty() {
            return;
        }
        let n = self.rooms.len().max(1) as f64;
        let (w, h) = ((b.max.x - b.min.x).max(1.0), (b.max.y - b.min.y).max(1.0));
        let cell = (w * h / n).sqrt().max(1_000.0) * 2.0;
        self.origin = b.min;
        self.cell = cell;
        self.cols = ((w / cell).ceil() as usize).clamp(1, 1024);
        self.rows = ((h / cell).ceil() as usize).clamp(1, 1024);
        self.buckets = vec![Vec::new(); self.cols * self.rows];
        for (i, r) in self.rooms.iter().enumerate() {
            let rb = r.bbox();
            let (c0, r0) = self.bucket(rb.min);
            let (c1, r1) = self.bucket(rb.max);
            for y in r0..=r1 {
                for x in c0..=c1 {
                    self.buckets[y * self.cols + x].push(i as u32);
                }
            }
        }
    }

    fn bucket(&self, p: P) -> (usize, usize) {
        let c = (((p.x - self.origin.x) / self.cell).floor().max(0.0) as usize).min(self.cols - 1);
        let r = (((p.y - self.origin.y) / self.cell).floor().max(0.0) as usize).min(self.rows - 1);
        (c, r)
    }

    /// The room containing `p` (the first in index order; boundaries count as inside within
    /// `eps`).
    pub fn locate(&self, p: P, eps: f64) -> Option<u32> {
        if self.buckets.is_empty() {
            return None;
        }
        let (c, r) = self.bucket(p);
        self.buckets[r * self.cols + c].iter().copied().find(|&i| self.rooms[i as usize].contains(p, eps))
    }
}

/// Cuts a trapezoid into a lattice of at most `max` by `max` pieces (columns at equal x steps,
/// rows at equal fractions between the bottom and top edges) and records the portals between
/// pieces of one column (the shared slanted edges; vertical ones are found later).
fn split(t: Room, max: f64, rooms: &mut Vec<Room>, portals: &mut Vec<Portal>) {
    let w = t.x1 - t.x0;
    let h = (t.yt0 - t.yb0).max(t.yt1 - t.yb1);
    let nx = if max > 0.0 { ((w / max).ceil() as usize).max(1) } else { 1 };
    let ny = if max > 0.0 { ((h / max).ceil() as usize).max(1) } else { 1 };
    let xs: Vec<f64> = (0..=nx).map(|i| if i == nx { t.x1 } else { t.x0 + w * i as f64 / nx as f64 }).collect();
    let frac = |j: usize, yb: f64, yt: f64| if j == ny { yt } else { yb + (yt - yb) * j as f64 / ny as f64 };
    for i in 0..nx {
        let (xa, xb) = (xs[i], xs[i + 1]);
        let (ba, bb, ta, tb) = (t.yb(xa), t.yb(xb), t.yt(xa), t.yt(xb));
        let first = rooms.len() as u32;
        for j in 0..ny {
            rooms.push(Room {
                x0: xa,
                x1: xb,
                yb0: frac(j, ba, ta),
                yb1: frac(j, bb, tb),
                yt0: frac(j + 1, ba, ta),
                yt1: frac(j + 1, bb, tb),
            });
            if j > 0 {
                let r = rooms[rooms.len() - 1];
                if xb - xa > MIN_PORTAL {
                    portals.push(Portal {
                        a: P::new(xa, r.yb0),
                        b: P::new(xb, r.yb1),
                        rooms: [first + j as u32 - 1, first + j as u32],
                    });
                }
            }
        }
    }
}

// ---- clearance regions ----------------------------------------------------------------

fn ring_of(pts: &[P]) -> pc::Ring {
    pc::Ring(pts.iter().map(|q| pc::Point::new(q.x.round() as i64, q.y.round() as i64)).collect())
}

fn convex(r: &[P]) -> bool {
    let n = r.len();
    if n < 4 {
        return true;
    }
    let mut sign = 0.0f64;
    for i in 0..n {
        let (a, b, c) = (r[i], r[(i + 1) % n], r[(i + 2) % n]);
        let cr = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        if cr.abs() < 1e-3 {
            continue;
        }
        if sign == 0.0 {
            sign = cr.signum();
        } else if cr.signum() != sign {
            return false;
        }
    }
    true
}

/// The clearance region of `shape` at distance `d` (outer polygons, see the module docs).
pub(crate) fn grow(shape: &Shape, d: f64, out: &mut Vec<pc::Ring>) {
    match shape {
        Shape::Capsule { a, b, r } => out.push(ring_of(&octagon_hull(*a, *b, r + d))),
        Shape::Polys(v) => {
            for poly in v {
                if poly.rings.len() == 1 && convex(&poly.rings[0]) {
                    out.push(ring_of(&octagon_hull_pts(&poly.rings[0], d)));
                    continue;
                }
                let rings: Vec<pc::Ring> = poly.rings.iter().map(|r| ring_of(r)).collect();
                let pg = pc::Polygon::new(rings[0].clone(), rings[1..].to_vec());
                match pc::triangulate(&pg) {
                    Ok(t) if !t.triangles.is_empty() => {
                        for k in 0..t.triangles.len() {
                            let tri = t.triangle(k).map(|q| P::new(q.x as f64, q.y as f64));
                            out.push(ring_of(&octagon_hull_pts(&tri, d)));
                        }
                    }
                    // Not triangulable (invalid input): its convex hull, conservatively.
                    _ => out.push(ring_of(&octagon_hull_pts(&poly.rings[0], d))),
                }
            }
        }
    }
}

/// Cached clearance regions of the static obstacles, per profile.
pub(crate) struct Grown {
    /// `[profile][obstacle]`: track regions on any layer.
    track: Vec<Vec<OnceLock<Vec<pc::Ring>>>>,
    /// `[profile][obstacle]`: via regions (`None` for obstacles vias ignore).
    via: Vec<Vec<OnceLock<Vec<pc::Ring>>>>,
}

impl Grown {
    pub fn new(rb: &RouterBoard) -> Grown {
        let n = rb.obstacles.len();
        let mk = || (0..rb.profiles.len()).map(|_| (0..n).map(|_| OnceLock::new()).collect()).collect();
        Grown { track: mk(), via: mk() }
    }

    fn track(&self, rb: &RouterBoard, prof: usize, o: usize) -> &[pc::Ring] {
        self.track[prof][o].get_or_init(|| {
            let (pr, ob) = (&rb.profiles[prof], &rb.obstacles[o]);
            let mut v = Vec::new();
            grow(&ob.shape, pr.hw + ob.clear.with(pr.c) + MARGIN, &mut v);
            v
        })
    }

    fn via(&self, rb: &RouterBoard, prof: usize, o: usize, own_pad: bool) -> &[pc::Ring] {
        self.via[prof][o].get_or_init(|| {
            let (pr, ob) = (&rb.profiles[prof], &rb.obstacles[o]);
            let d = if own_pad { pr.rv } else { pr.rv + ob.clear.with(pr.c) };
            let mut v = Vec::new();
            grow(&ob.shape, d + MARGIN, &mut v);
            v
        })
    }
}

/// What a decomposition keeps away from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Against {
    /// Static obstacles only (other nets' routing is negotiated).
    Static,
    /// Static obstacles and the routing of other nets in the index (an exact search).
    All,
}

fn region_ring(b: &BoxF) -> pc::Ring {
    ring_of(&[b.min, P::new(b.max.x, b.min.y), b.max, P::new(b.min.x, b.max.y)])
}

/// The board within `region`, minus `holes` (clearance regions).
fn free_space(rb: &RouterBoard, region: &BoxF, holes: &[pc::Ring]) -> pc::PolygonSet {
    let Some(outer) = rb.outer.as_ref() else { return vec![] };
    let board: Vec<pc::Ring> = outer.rings.iter().map(|r| ring_of(r)).collect();
    let inside = pc::Boolean::new()
        .subject(&board, pc::FillRule::EvenOdd)
        .clip(&region_ring(region), pc::FillRule::NonZero)
        .op(pc::Op::Intersection)
        .execute()
        .unwrap_or_default();
    let mut clip: Vec<pc::Ring> = holes.to_vec();
    for c in &rb.cutouts {
        clip.extend(c.rings.iter().map(|r| ring_of(r)));
    }
    pc::Boolean::new()
        .subject(&inside, pc::FillRule::NonZero)
        .clip(&clip, pc::FillRule::NonZero)
        .op(pc::Op::Difference)
        .execute()
        .unwrap_or_default()
}

/// Free space for the track centerlines of `net` on stackup layer `layer` within `region`.
pub(crate) fn track_space(
    rb: &RouterBoard,
    index: &Index,
    grown: &Grown,
    net: u32,
    layer: usize,
    region: &BoxF,
    against: Against,
) -> pc::PolygonSet {
    let prof = rb.net_profile[net as usize];
    let pr = &rb.profiles[prof];
    let bit = 1u64 << layer.min(63);
    let mut holes: Vec<pc::Ring> = Vec::new();
    for (_, it) in index.query(&region.expand(index.reach + pr.hw)) {
        match it {
            Item::Static(o) => {
                let ob = &rb.obstacles[o as usize];
                let own = ob.net == Some(net) && matches!(ob.kind, ObKind::Pad | ObKind::Copper);
                if ob.tracks & bit == 0 || own {
                    continue;
                }
                holes.extend_from_slice(grown.track(rb, prof, o as usize));
            }
            _ if against == Against::Static => {}
            Item::Seg { net: m, layer: l, a, b } => {
                if m != net && l as usize == layer {
                    let po = rb.profile(m);
                    holes.push(ring_of(&octagon_hull(a, b, pr.hw + po.hw + pr.c.max(po.c) + MARGIN)));
                }
            }
            Item::Sized { net: m, layer: l, a, b, hw } => {
                if m != net && l as usize == layer {
                    let po = rb.profile(m);
                    holes.push(ring_of(&octagon_hull(a, b, pr.hw + hw + pr.c.max(po.c) + MARGIN)));
                }
            }
            Item::Via { net: m, at } => {
                if m != net {
                    let po = rb.profile(m);
                    holes.push(ring_of(&octagon_hull(at, at, pr.hw + po.rv + pr.c.max(po.c) + MARGIN)));
                }
            }
        }
    }
    free_space(rb, region, &holes)
}

/// Free space for the centers of through vias of `net` within `region`: every layer's
/// obstacles that vias keep away from, hole-to-hole distance to drilled holes, no via in a pad
/// (of its own net either). Vias of the net itself are left to the search (hole-to-hole).
pub(crate) fn via_space(
    rb: &RouterBoard,
    index: &Index,
    grown: &Grown,
    net: u32,
    region: &BoxF,
    against: Against,
) -> pc::PolygonSet {
    let prof = rb.net_profile[net as usize];
    let pr = &rb.profiles[prof];
    let mut holes: Vec<pc::Ring> = Vec::new();
    for (_, it) in index.query(&region.expand(index.reach + pr.rv)) {
        match it {
            Item::Static(o) => {
                let ob = &rb.obstacles[o as usize];
                if !ob.vias {
                    continue;
                }
                let own = ob.net == Some(net);
                if own && ob.kind == ObKind::Copper {
                    continue;
                }
                holes.extend_from_slice(grown.via(rb, prof, o as usize, own && ob.kind == ObKind::Pad));
            }
            _ if against == Against::Static => {}
            Item::Seg { net: m, a, b, .. } => {
                if m != net {
                    let po = rb.profile(m);
                    holes.push(ring_of(&octagon_hull(a, b, pr.rv + po.hw + pr.c.max(po.c) + MARGIN)));
                }
            }
            Item::Sized { net: m, a, b, hw, .. } => {
                if m != net {
                    let po = rb.profile(m);
                    holes.push(ring_of(&octagon_hull(a, b, pr.rv + hw + pr.c.max(po.c) + MARGIN)));
                }
            }
            Item::Via { net: m, at } => {
                if m != net {
                    let po = rb.profile(m);
                    let d = (pr.rv + po.rv + pr.c.max(po.c)).max(pr.dr + po.dr + rb.h2h);
                    holes.push(ring_of(&octagon_hull(at, at, d + MARGIN)));
                }
            }
        }
    }
    for h in &rb.holes {
        let d = pr.dr + h.r + rb.h2h;
        if region.expand(d).dist(h.at) == 0.0 {
            holes.push(ring_of(&octagon_hull(h.at, h.at, d + MARGIN)));
        }
    }
    free_space(rb, region, &holes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(x0: i64, y0: i64, x1: i64, y1: i64) -> pc::Ring {
        pc::Ring(vec![pc::Point::new(x0, y0), pc::Point::new(x1, y0), pc::Point::new(x1, y1), pc::Point::new(x0, y1)])
    }

    /// A 100 × 100 square with a 40 × 40 hole in the middle.
    fn frame() -> pc::PolygonSet {
        pc::Boolean::new()
            .subject(&sq(0, 0, 100, 100), pc::FillRule::NonZero)
            .clip(&sq(30, 30, 70, 70), pc::FillRule::NonZero)
            .op(pc::Op::Difference)
            .execute()
            .unwrap()
    }

    #[test]
    fn rooms_cover_the_free_space_and_join_through_portals() {
        for max in [0.0, 25.0] {
            let r = Rooms::from_free(&frame(), max);
            let area: f64 = r.rooms.iter().map(|q| (q.x1 - q.x0) * ((q.yt0 - q.yb0) + (q.yt1 - q.yb1)) / 2.0).sum();
            assert!((area - (10_000.0 - 1_600.0)).abs() < 1e-6, "area {area}");
            assert!(r.locate(P::new(50.0, 50.0), 0.0).is_none());
            assert!(r.locate(P::new(10.0, 50.0), 0.0).is_some());
            // Every room is reachable from every other through portals (the frame is connected).
            let mut seen = vec![false; r.rooms.len()];
            seen[0] = true;
            let mut changed = true;
            while changed {
                changed = false;
                for p in &r.portals {
                    let [a, b] = p.rooms.map(|x| x as usize);
                    if seen[a] != seen[b] {
                        seen[a] = true;
                        seen[b] = true;
                        changed = true;
                    }
                }
            }
            assert!(seen.iter().all(|&s| s), "all rooms connected (max {max})");
            if max > 0.0 {
                assert!(r.rooms.iter().all(|q| q.x1 - q.x0 <= 25.0 + 1e-9));
            }
            // Portal endpoints lie on both rooms' boundaries.
            for p in &r.portals {
                for &ri in &p.rooms {
                    let q = r.rooms[ri as usize];
                    assert!(q.contains(p.a, 1e-6) && q.contains(p.b, 1e-6));
                }
            }
        }
    }

    #[test]
    fn clearance_regions_cover_non_convex_shapes_exactly() {
        // An L-shaped polygon grown by 10: everything within 10 of it is covered, the notch
        // (which a convex hull would fill) is not.
        let l = Shape::of_ring(vec![
            P::new(0.0, 0.0),
            P::new(100.0, 0.0),
            P::new(100.0, 20.0),
            P::new(20.0, 20.0),
            P::new(20.0, 100.0),
            P::new(0.0, 100.0),
        ]);
        let mut rings = Vec::new();
        grow(&l, 10.0, &mut rings);
        let set = pc::union_all(&rings, pc::FillRule::NonZero).unwrap();
        let inside =
            |p: P| pc::locate(&set, pc::Point::new(p.x.round() as i64, p.y.round() as i64)) != pc::Location::Outside;
        for p in [P::new(-9.0, 50.0), P::new(50.0, 29.0), P::new(29.0, 50.0), P::new(107.0, 27.0), P::new(50.0, 10.0)] {
            assert!(inside(p), "{p:?} within reach must be covered");
        }
        assert!(!inside(P::new(60.0, 60.0)));
        assert!(!inside(P::new(35.0, 35.0)));
    }
}
