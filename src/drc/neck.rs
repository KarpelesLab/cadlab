//! Necks (`drc.neck`): two pieces of a net's copper joined by less copper than the narrower of
//! them is wide, where that sliver is the only thing joining them.
//!
//! A track is a line with round ends. Two tracks whose centre lines meet are joined by their
//! whole width, whatever their angle. Two tracks that end a little apart still touch, by the
//! overlap of their round ends: the copper across the joint is the lens of two discs, 0.15 mm
//! for two tracks of 0.25 mm that end 0.2 mm apart, and next to nothing as the ends come to
//! one width apart. The same holds for a track that stops short of a pad or a via and reaches
//! it by the tip of its round end. Connectivity counts such a joint as joined (the copper
//! touches), the clearance rules are between nets, and the width rule asks each track for its
//! own width: nothing else sees it (DECISIONS D144, D145).
//!
//! What is measured: for each pair of items of one net that touch on a layer, one of them a
//! track, the widest extent of the copper they share (the chord of the lens for two round ends,
//! the track's width for an end inside a pad), never more than the narrower of the two. A joint
//! is sound when the track's centre line reaches the other item's copper (a pad, a via, a
//! copper drawing) or the other track's centre line, or when what they share is as wide as the
//! narrower of them. A joint that is not sound is a neck unless the two are joined soundly some
//! other way: through other tracks, vias and pads, or through a pour or teardrop of the net
//! that covers the joint or holds both.

use std::collections::BTreeMap;

use polyclip::{ArcTol, Circle, EndCap, FillRule, Geometry, Join, Op, Path, Polygon, PolygonSet, Rect, Side};

use super::TOLERANCE;
use crate::board::{self as geo, COPPER_TOL, CopperItem, ItemRef};
use crate::geom::Point;
use crate::id::ObjectId;
use crate::model::board::{Rules, Track, Via};
use crate::units::Nm;

/// A neck: see the module documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neck {
    /// One side of the joint: an index into the items given (always a track).
    pub a: usize,
    /// The other side: a track, a via, a pad or a copper drawing.
    pub b: usize,
    /// The copper layer of the joint.
    pub layer: String,
    /// Where: the middle of the copper the two share.
    pub at: Point,
    /// The width of the copper across the joint.
    pub width: Nm,
    /// The width the joint should have: that of the narrower of the two (for a pad, its
    /// shorter side).
    pub nominal: Nm,
    /// The least width the rules allow a track (`min_track_width`), or `nominal` if that is
    /// less.
    pub minimum: Nm,
}

impl Neck {
    /// Whether the neck is narrower than the rules allow any track to be (an error; otherwise
    /// it is only narrower than the tracks it joins, a warning).
    pub fn below_minimum(&self) -> bool {
        self.width.0 < self.minimum.0 - TOLERANCE.0
    }
}

/// What an item is to the measure.
enum Kind<'a> {
    Track(&'a Track),
    /// A via, a pad or a copper drawing, with the width it can be joined by (a via's
    /// diameter, a pad's shorter side).
    Solid(i64),
}

impl Kind<'_> {
    fn width(&self) -> i64 {
        match self {
            Kind::Track(t) => t.width.0,
            Kind::Solid(w) => *w,
        }
    }
}

fn pt(p: Point) -> polyclip::Point {
    p.into()
}

fn centre_line(t: &Track) -> Path {
    Path(match t.mid {
        None => vec![pt(t.start), pt(t.end)],
        Some(mid) => geo::arc_points(t.start, mid, t.end, 1_000).into_iter().map(pt).collect(),
    })
}

/// The widest extent of a polygon: the largest distance between two vertices of its outline.
fn extent(pg: &Polygon) -> f64 {
    let pts = &pg.outer.0;
    let mut best: i128 = 0;
    for (k, a) in pts.iter().enumerate() {
        for b in &pts[k + 1..] {
            let (dx, dy) = ((a.x - b.x) as i128, (a.y - b.y) as i128);
            best = best.max(dx * dx + dy * dy);
        }
    }
    (best as f64).sqrt()
}

fn middle(pg: &Polygon, fallback: Point) -> Point {
    polyclip::centroid(pg).map(|c| Point::new(Nm(c.x.round() as i64), Nm(c.y.round() as i64))).unwrap_or(fallback)
}

struct Sets(Vec<usize>);

impl Sets {
    fn find(&mut self, i: usize) -> usize {
        let mut r = i;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut i = i;
        while self.0[i] != r {
            let next = self.0[i];
            self.0[i] = r;
            i = next;
        }
        r
    }
    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

/// A joint that is not sound on its own.
struct Thin {
    a: usize,
    b: usize,
    layer: usize,
    at: Point,
}

/// The necks of a board's copper, by layer and place. `items` are the board's copper items
/// (`board::copper_items`), `tracks` and `vias` the board's, `layers` the copper layers in
/// stackup order. Only `rules.min_track_width` is read.
pub fn necks(rules: &Rules, layers: &[String], tracks: &[Track], vias: &[Via], items: &[CopperItem]) -> Vec<Neck> {
    let track_of: BTreeMap<ObjectId, &Track> = tracks.iter().map(|t| (t.id, t)).collect();
    let via_of: BTreeMap<ObjectId, &Via> = vias.iter().map(|v| (v.id, v)).collect();
    let layer_index = |l: &String| layers.iter().position(|n| n == l);
    // Items by net: the copper that is no fill, and the fills (pours and teardrops).
    let mut by_net: BTreeMap<&str, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        let Some(net) = it.net.as_deref() else { continue };
        let e = by_net.entry(net).or_default();
        match it.item {
            ItemRef::Zone(..) => e.1.push(i),
            _ => e.0.push(i),
        }
    }
    let kind = |i: usize| -> Kind<'_> {
        match &items[i].item {
            ItemRef::Track(id) => match track_of.get(id) {
                Some(t) => Kind::Track(t),
                None => Kind::Solid(geo::zones::short_side(&items[i].shape)),
            },
            ItemRef::Via(id) => Kind::Solid(
                via_of.get(id).map(|v| v.diameter.0).unwrap_or_else(|| geo::zones::short_side(&items[i].shape)),
            ),
            _ => Kind::Solid(geo::zones::short_side(&items[i].shape)),
        }
    };
    // The copper of a thin joint is measured on outlines with arcs to 50 nm: the tips of a lens
    // of two arcs drawn 1 um outside move by several times that.
    let fine = ArcTol::new(50, Side::Outside);
    let fine_shape = |i: usize| -> PolygonSet {
        let shape = match &items[i].item {
            ItemRef::Track(id) => track_of.get(id).and_then(|t| {
                polyclip::offset_paths(&vec![centre_line(t)], t.width.0 / 2, Join::Round, EndCap::Round, fine).ok()
            }),
            ItemRef::Via(id) => via_of.get(id).and_then(|v| {
                let ring = Circle { center: pt(v.at), radius: v.diameter.0 / 2 }.to_ring(fine).ok()?;
                Some(vec![Polygon::new(ring, vec![])])
            }),
            _ => None,
        };
        shape.unwrap_or_else(|| items[i].shape.clone())
    };
    let tol = TOLERANCE.0;
    let mut sets = Sets((0..items.len()).collect());
    let mut out = Vec::new();
    for (solid, fills) in by_net.values() {
        if !solid.iter().any(|&i| matches!(items[i].item, ItemRef::Track(_))) {
            continue;
        }
        // The items of one pad with a pad stack are joined by its plated hole.
        let mut first_of: BTreeMap<usize, usize> = BTreeMap::new();
        for &i in solid {
            if let Some(k) = items[i].local.stack {
                let f = *first_of.entry(k).or_insert(i);
                sets.join(f, i);
            }
        }
        let mut order: Vec<(Rect, usize)> =
            solid.iter().filter_map(|&i| items[i].shape.bbox().map(|b| (b, i))).collect();
        order.sort_by_key(|(b, i)| (b.min.x, *i));
        let mut thin: Vec<Thin> = Vec::new();
        for (k, (bi, i)) in order.iter().enumerate() {
            for (bj, j) in &order[k + 1..] {
                if bj.min.x > bi.max.x {
                    break;
                }
                if !bi.intersects(bj) {
                    continue;
                }
                let (a, b) = (&items[*i], &items[*j]);
                let Some(layer) = a.layers.iter().filter(|l| b.layers.contains(l)).find_map(layer_index) else {
                    continue;
                };
                let (ka, kb) = (kind(*i), kind(*j));
                // The track first.
                let (i, j, ka, kb) = match (&ka, &kb) {
                    (Kind::Track(_), _) => (*i, *j, ka, kb),
                    (_, Kind::Track(_)) => (*j, *i, kb, ka),
                    _ => {
                        if polyclip::intersects(&a.shape, &b.shape) {
                            sets.join(*i, *j);
                        }
                        continue;
                    }
                };
                let Kind::Track(t) = ka else { unreachable!("the track is first") };
                let line = centre_line(t);
                let sound = match &kb {
                    Kind::Track(u) => polyclip::distance_less_than(&line, &centre_line(u), tol + 1),
                    Kind::Solid(_) => {
                        polyclip::intersects(&line, &items[j].shape)
                            || polyclip::distance_less_than(&line, &items[j].shape, tol + 1)
                    }
                };
                if sound {
                    sets.join(i, j);
                    continue;
                }
                if !polyclip::intersects(&items[i].shape, &items[j].shape) {
                    continue;
                }
                let Ok(shared) =
                    polyclip::boolean(Op::Intersection, &items[i].shape, &items[j].shape, FillRule::NonZero)
                else {
                    continue;
                };
                let Some(widest) = shared.iter().max_by(|x, y| extent(x).total_cmp(&extent(y))) else { continue };
                let nominal = t.width.0.min(kb.width());
                if extent(widest) >= (nominal - tol) as f64 {
                    sets.join(i, j);
                } else {
                    thin.push(Thin { a: i, b: j, layer, at: middle(widest, items[i].anchor) });
                }
            }
        }
        if thin.is_empty() {
            continue;
        }
        // The thin joints between the same two groups of soundly joined copper on a layer are
        // taken together: a track that ends beside the corner of two others shares more with
        // the two than with either.
        let mut groups: BTreeMap<(usize, usize, usize), Vec<usize>> = BTreeMap::new();
        for (n, th) in thin.iter().enumerate() {
            let (ra, rb) = (sets.find(th.a), sets.find(th.b));
            if ra != rb {
                groups.entry((th.layer, ra.min(rb), ra.max(rb))).or_default().push(n);
            }
        }
        for ((layer, ..), members) in groups {
            let first = &thin[members[0]];
            let (ra, rb) = (sets.find(first.a), sets.find(first.b));
            if ra == rb {
                continue;
            }
            // The two sides: the items of the joints by the group they are of.
            let (mut side_a, mut side_b): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
            for &n in &members {
                for i in [thin[n].a, thin[n].b] {
                    let side = if sets.find(i) == ra { &mut side_a } else { &mut side_b };
                    if !side.contains(&i) {
                        side.push(i);
                    }
                }
            }
            let widest = |side: &[usize]| side.iter().map(|&i| kind(i).width()).max().unwrap_or(0);
            let nominal = widest(&side_a).min(widest(&side_b));
            let copper = |side: &[usize]| -> PolygonSet { side.iter().flat_map(|&i| fine_shape(i)).collect() };
            let Ok(shared) = polyclip::boolean(Op::Intersection, &copper(&side_a), &copper(&side_b), FillRule::NonZero)
            else {
                continue;
            };
            if shared.is_empty() {
                continue;
            }
            if shared.iter().any(|pg| extent(pg) >= (nominal - tol) as f64) {
                sets.join(ra, rb);
                continue;
            }
            // A pour or a teardrop of the net that holds both sides joins them.
            let layer_name = &layers[layer];
            let held = |side: &[usize], f: usize| {
                side.iter().any(|&i| match kind(i) {
                    Kind::Track(t) => {
                        let line = centre_line(t);
                        let (first, last) = (line.0[0], line.0[line.0.len() - 1]);
                        let mid = match line.0.len() {
                            2 => polyclip::Point::new((first.x + last.x) / 2, (first.y + last.y) / 2),
                            n => line.0[n / 2],
                        };
                        [first, mid, last].into_iter().any(|c| covers(&items[f].shape, c, t.width.0 / 2 - tol))
                    }
                    Kind::Solid(_) => polyclip::intersects(&items[f].shape, &items[i].shape),
                })
            };
            let on_layer: Vec<usize> =
                fills.iter().copied().filter(|&f| items[f].layers.contains(layer_name)).collect();
            if on_layer.iter().any(|&f| held(&side_a, f) && held(&side_b, f)) {
                sets.join(ra, rb);
                continue;
            }
            for pg in &shared {
                let at = middle(pg, first.at);
                // Or one that covers the joint itself.
                if on_layer.iter().any(|&f| covers(&items[f].shape, pt(at), nominal / 2 - tol)) {
                    continue;
                }
                let dist = |q: Point| ((q.x.0 - at.x.0) as f64).hypot((q.y.0 - at.y.0) as f64);
                let nearest = members.iter().map(|&n| &thin[n]).min_by(|x, y| dist(x.at).total_cmp(&dist(y.at)));
                let Some(th) = nearest else { continue };
                let width = (extent(pg).round() as i64).min(nominal);
                out.push(Neck {
                    a: th.a,
                    b: th.b,
                    layer: layer_name.clone(),
                    at,
                    width: Nm(width),
                    nominal: Nm(nominal),
                    minimum: Nm(rules.min_track_width.0.min(nominal)),
                });
            }
        }
    }
    out.sort_by(|x, y| {
        (layer_index(&x.layer), x.at.x, x.at.y, x.a, x.b).cmp(&(layer_index(&y.layer), y.at.x, y.at.y, y.a, y.b))
    });
    out
}

/// Whether `fill` holds the whole disc of `radius` around `c`.
fn covers(fill: &PolygonSet, c: polyclip::Point, radius: i64) -> bool {
    let radius = radius.max(1);
    if !fill.bbox().is_some_and(|b| b.contains_point(c)) {
        return false;
    }
    let Ok(ring) = Circle { center: c, radius }.to_ring(COPPER_TOL) else { return false };
    polyclip::contains(fill, &Polygon::new(ring, vec![]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::ItemLocal;

    const UM: i64 = 1_000;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x * UM), Nm(y * UM))
    }

    fn track(id: u64, a: Point, b: Point, width: i64) -> Track {
        Track {
            id: ObjectId(id),
            layer: "F.Cu".into(),
            width: Nm(width * UM),
            net: Some("N".into()),
            start: a,
            end: b,
            mid: None,
            locked: false,
        }
    }

    fn item(item: ItemRef, shape: PolygonSet, anchor: Point) -> CopperItem {
        CopperItem {
            item,
            net: Some("N".into()),
            layers: vec!["F.Cu".into()],
            shape,
            anchor,
            local: ItemLocal::default(),
        }
    }

    fn rect(x0: i64, y0: i64, x1: i64, y1: i64) -> PolygonSet {
        let q = |x: i64, y: i64| polyclip::Point::new(x * UM, y * UM);
        vec![Polygon::new(vec![q(x0, y0), q(x1, y0), q(x1, y1), q(x0, y1)], vec![])]
    }

    /// A board of tracks, vias and other copper (pads as rectangles, fills as zone items).
    #[derive(Default)]
    struct Board {
        tracks: Vec<Track>,
        vias: Vec<Via>,
        other: Vec<CopperItem>,
    }

    impl Board {
        fn track(mut self, a: Point, b: Point, width: i64) -> Self {
            let id = self.tracks.len() as u64 + 1;
            self.tracks.push(track(id, a, b, width));
            self
        }
        fn via(mut self, at: Point, diameter: i64) -> Self {
            self.vias.push(Via {
                id: ObjectId(100 + self.vias.len() as u64),
                at,
                drill: Nm(diameter * UM / 2),
                diameter: Nm(diameter * UM),
                net: Some("N".into()),
                from: "F.Cu".into(),
                to: "B.Cu".into(),
                locked: false,
            });
            self
        }
        fn pad(mut self, number: &str, shape: PolygonSet) -> Self {
            self.other.push(item(ItemRef::Pad("U1".into(), number.into()), shape, p(0, 0)));
            self
        }
        fn fill(mut self, shape: PolygonSet) -> Self {
            let n = self.other.len();
            self.other.push(item(ItemRef::Zone(ObjectId(900), "F.Cu".into(), n), shape, p(0, 0)));
            self
        }
        fn necks(&self) -> Vec<Neck> {
            let mut items: Vec<CopperItem> =
                self.tracks.iter().map(|t| item(ItemRef::Track(t.id), geo::track_shape(t), t.start)).collect();
            items.extend(self.vias.iter().map(|v| item(ItemRef::Via(v.id), vec![geo::via_shape(v)], v.at)));
            items.extend(self.other.iter().cloned());
            let rules = Rules { min_track_width: Nm(200 * UM), ..Rules::default() };
            necks(&rules, &["F.Cu".to_string(), "B.Cu".to_string()], &self.tracks, &self.vias, &items)
        }
    }

    /// Two tracks of 0.25 mm whose ends are 0.2 mm apart share the lens of their round ends,
    /// 0.15 mm across: under the minimum of 0.2 mm, an error. 0.1 mm apart they share 0.229
    /// mm: over the minimum and under their own width, a warning.
    #[test]
    fn two_round_ends_that_overlap_are_a_neck() {
        let b = Board::default().track(p(0, 0), p(1000, 0), 250).track(p(1200, 0), p(2000, 0), 250);
        let n = b.necks();
        assert_eq!(n.len(), 1, "{n:?}");
        assert_eq!((n[0].a, n[0].b, n[0].layer.as_str()), (0, 1, "F.Cu"));
        assert_eq!((n[0].nominal, n[0].minimum), (Nm(250 * UM), Nm(200 * UM)));
        // The chord of the lens: 2 sqrt(125^2 - 100^2) = 150 um.
        assert!((n[0].width.0 - 150 * UM).abs() <= UM / 2, "{:?}", n[0].width);
        assert!((n[0].at.x.0 - 1100 * UM).abs() <= UM && n[0].at.y.0.abs() <= UM, "{:?}", n[0].at);
        assert!(n[0].below_minimum());
        let b = Board::default().track(p(0, 0), p(1000, 0), 250).track(p(1100, 0), p(2000, 0), 250);
        let n = b.necks();
        assert_eq!(n.len(), 1, "{n:?}");
        assert!((n[0].width.0 - 229_129).abs() <= UM / 2 && !n[0].below_minimum(), "{:?}", n[0]);
        // At an angle as well: the second track leaves upward from 0.2 mm beyond the end.
        let b = Board::default().track(p(0, 0), p(1000, 0), 250).track(p(1200, 0), p(1200, 900), 250);
        assert_eq!(b.necks().len(), 1);
    }

    /// Ordinary joints: ends that meet, a corner, a T on another track's centre line, a
    /// crossing, a narrow track ending anywhere inside a wide one, two tracks side by side
    /// that overlap along their length.
    #[test]
    fn ordinary_joints_are_no_necks() {
        let ends = Board::default().track(p(0, 0), p(1000, 0), 250).track(p(1000, 0), p(2000, 0), 250);
        assert!(ends.necks().is_empty());
        let corner = Board::default().track(p(0, 0), p(1000, 0), 250).track(p(1000, 0), p(1700, 700), 250);
        assert!(corner.necks().is_empty());
        let tee = Board::default().track(p(0, 0), p(2000, 0), 250).track(p(1000, 0), p(1000, 900), 250);
        assert!(tee.necks().is_empty());
        let cross = Board::default().track(p(0, 0), p(2000, 0), 250).track(p(1000, -900), p(1000, 900), 250);
        assert!(cross.necks().is_empty());
        let inside = Board::default().track(p(0, 0), p(2000, 0), 600).track(p(1000, 150), p(1000, 900), 200);
        assert!(inside.necks().is_empty(), "{:?}", inside.necks());
        let beside = Board::default().track(p(0, 0), p(2000, 0), 250).track(p(500, 200), p(1500, 200), 250);
        assert!(beside.necks().is_empty(), "{:?}", beside.necks());
        // A T whose track ends at the other's edge: its whole width goes in.
        let edge = Board::default().track(p(0, 0), p(2000, 0), 250).track(p(1000, 125), p(1000, 900), 250);
        assert!(edge.necks().is_empty(), "{:?}", edge.necks());
        // Tracks that do not touch are no joint at all.
        let apart = Board::default().track(p(0, 0), p(1000, 0), 250).track(p(1300, 0), p(2000, 0), 250);
        assert!(apart.necks().is_empty());
    }

    /// A track that reaches a pad or a via only by the tip of its round end, and one whose
    /// centre line comes to the copper.
    #[test]
    fn a_track_that_reaches_a_pad_or_via_by_its_tip_is_a_neck() {
        // The pad's edge is at x = 1000; the track ends 0.1 mm short: 2 sqrt(125^2 - 100^2).
        let pad = rect(1000, -300, 1600, 300);
        let b = Board::default().track(p(0, 0), p(900, 0), 250).pad("1", pad.clone());
        let n = b.necks();
        assert_eq!(n.len(), 1, "{n:?}");
        assert!((n[0].width.0 - 150 * UM).abs() <= UM / 2 && n[0].below_minimum(), "{:?}", n[0]);
        assert_eq!(n[0].a, 0);
        // Ended at the pad's first copper, and inside it: sound.
        for end in [1000, 1001, 1300] {
            let b = Board::default().track(p(0, 0), p(end, 0), 250).pad("1", pad.clone());
            assert!(b.necks().is_empty(), "{end}");
        }
        // A via of 0.6 mm at x = 1300: its copper begins at x = 1000.
        let b = Board::default().track(p(0, 0), p(900, 0), 250).via(p(1300, 0), 600);
        let n = b.necks();
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(n[0].width.0 < 160 * UM && n[0].below_minimum(), "{:?}", n[0]);
        for end in [1000, 1150, 1300] {
            let b = Board::default().track(p(0, 0), p(end, 0), 250).via(p(1300, 0), 600);
            assert!(b.necks().is_empty(), "{end}");
        }
    }

    /// A track narrowed where it meets a pad (`neck_at_pads`): the narrow piece begins on the
    /// wide one's centre line and ends in the pad, and is joined by its own width at both.
    #[test]
    fn a_track_narrowed_at_its_pad_is_no_neck() {
        let pad = rect(1000, -100, 1600, 100);
        let b = Board::default().track(p(0, 0), p(700, 0), 300).track(p(700, 0), p(1300, 0), 200).pad("1", pad);
        assert!(b.necks().is_empty(), "{:?}", b.necks());
    }

    /// A neck that is not the only way: the two are joined by other tracks, or a pour of the
    /// net covers the joint, or holds both tracks, or a teardrop lies over a track's tip at
    /// its pad.
    #[test]
    fn a_neck_joined_another_way_is_not_reported() {
        let necked = || Board::default().track(p(0, 0), p(1000, 0), 250).track(p(1200, 0), p(2000, 0), 250);
        assert_eq!(necked().necks().len(), 1);
        // A way round by sound joints.
        let round = necked().track(p(1000, 0), p(1000, 800), 250).track(p(1000, 800), p(1200, 800), 250).track(
            p(1200, 800),
            p(1200, 0),
            250,
        );
        assert!(round.necks().is_empty(), "{:?}", round.necks());
        // A pour over the joint.
        assert!(necked().fill(rect(800, -400, 1400, 400)).necks().is_empty());
        // A pour that holds both tracks elsewhere and has a hole at the joint.
        let mut holed = rect(-500, -500, 2500, 500);
        let q = |x: i64, y: i64| polyclip::Point::new(x * UM, y * UM);
        holed[0].holes.push(vec![q(1050, -50), q(1050, 50), q(1150, 50), q(1150, -50)].into());
        assert!(necked().fill(holed).necks().is_empty());
        // A pour that only one of them is in is no way round.
        assert_eq!(necked().fill(rect(-500, -400, 600, 400)).necks().len(), 1);
        // A teardrop (a zone of the net) over the tip of a track that ends short of its pad.
        let pad = rect(1000, -300, 1600, 300);
        let short = || Board::default().track(p(0, 0), p(900, 0), 250).pad("1", pad.clone());
        assert_eq!(short().necks().len(), 1);
        let q = |x: i64, y: i64| polyclip::Point::new(x * UM, y * UM);
        let teardrop = vec![Polygon::new(vec![q(400, -125), q(1100, -300), q(1100, 300), q(400, 125)], vec![])];
        assert!(short().fill(teardrop).necks().is_empty());
    }

    /// A track that ends beside the corner of two others is measured against both at once,
    /// and reported once if what it shares with the two is still thin.
    #[test]
    fn thin_joints_to_one_piece_of_copper_are_one_neck() {
        let b = Board::default().track(p(0, 0), p(1000, 0), 250).track(p(1000, 0), p(1000, 1000), 250).track(
            p(1150, -150),
            p(2000, -1000),
            250,
        );
        let n = b.necks();
        assert_eq!(n.len(), 1, "{n:?}");
        assert_eq!(n[0].a.max(n[0].b), 2);
    }

    /// A track narrower than the minimum is the width rule's finding: its necks are measured
    /// against its own width.
    #[test]
    fn the_minimum_is_never_more_than_the_tracks_own_width() {
        let b = Board::default().track(p(0, 0), p(1000, 0), 150).track(p(1120, 0), p(2000, 0), 150);
        let n = b.necks();
        assert_eq!(n.len(), 1, "{n:?}");
        assert_eq!((n[0].nominal, n[0].minimum), (Nm(150 * UM), Nm(150 * UM)));
        assert!(n[0].below_minimum());
    }
}
