//! Spatial index over obstacles and routed copper, with exact clearance checks for track
//! segments and vias. Used for pad access stubs, post-processing and failure explanations.

use super::geo::{BoxF, P};
use super::model::{ObKind, RouterBoard};
use super::scoped::St;

/// Distance slack (nm) when comparing with rule values: the DRC accepts a 2 µm deficit
/// (`crate::drc::TOLERANCE`), the router uses half of it.
pub(crate) const TOL: f64 = 1_000.0;

/// An indexed item.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Item {
    /// `RouterBoard::obstacles[i]`.
    Static(u32),
    /// A routed track centerline on a stackup layer.
    Seg { net: u32, layer: u8, a: P, b: P },
    /// A routed (through) via.
    Via { net: u32, at: P },
    /// A routed track centerline with its own half width (a neck-down net's track widened
    /// back to its class width).
    Sized { net: u32, layer: u8, a: P, b: P, hw: f64 },
}

/// What blocks a segment or via.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Blocker {
    /// An obstacle.
    Obstacle(u32),
    /// Routed copper of a net, on a stackup layer (`None`: a via).
    Net(u32, Option<u8>),
    /// A drilled hole too close (hole-to-hole).
    Hole,
    /// Outside the board.
    Outside,
}

/// Uniform bucket grid.
#[derive(Clone)]
pub(crate) struct Index {
    origin: P,
    cell: f64,
    cols: i64,
    rows: i64,
    buckets: Vec<Vec<u32>>,
    items: Vec<Option<(Item, BoxF)>>,
    /// Largest clearance-like distance any check adds beyond the item's own extent.
    pub reach: f64,
}

impl Index {
    pub fn new(area: BoxF, cell: f64, reach: f64) -> Index {
        let cell = cell.max(1_000.0);
        let area = if area.is_empty() { BoxF::of2(P::new(0.0, 0.0), P::new(1.0, 1.0)) } else { area };
        let cols = (((area.max.x - area.min.x) / cell).ceil() as i64 + 1).clamp(1, 4096);
        let rows = (((area.max.y - area.min.y) / cell).ceil() as i64 + 1).clamp(1, 4096);
        Index {
            origin: area.min,
            cell,
            cols,
            rows,
            buckets: vec![Vec::new(); (cols * rows) as usize],
            items: Vec::new(),
            reach,
        }
    }

    fn span(&self, b: &BoxF) -> (i64, i64, i64, i64) {
        let cx = |x: f64| (((x - self.origin.x) / self.cell).floor() as i64).clamp(0, self.cols - 1);
        let cy = |y: f64| (((y - self.origin.y) / self.cell).floor() as i64).clamp(0, self.rows - 1);
        (cx(b.min.x), cy(b.min.y), cx(b.max.x), cy(b.max.y))
    }

    pub fn insert(&mut self, item: Item, bbox: BoxF) -> u32 {
        let id = self.items.len() as u32;
        let (x0, y0, x1, y1) = self.span(&bbox);
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.buckets[(y * self.cols + x) as usize].push(id);
            }
        }
        self.items.push(Some((item, bbox)));
        id
    }

    pub fn remove(&mut self, id: u32) {
        let Some((_, bbox)) = self.items[id as usize].take() else { return };
        let (x0, y0, x1, y1) = self.span(&bbox);
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.buckets[(y * self.cols + x) as usize].retain(|i| *i != id);
            }
        }
    }

    /// Items whose box meets `b`, in id order.
    pub fn query(&self, b: &BoxF) -> Vec<(u32, Item)> {
        let (x0, y0, x1, y1) = self.span(b);
        let mut ids = Vec::new();
        for y in y0..=y1 {
            for x in x0..=x1 {
                ids.extend(self.buckets[(y * self.cols + x) as usize].iter().copied());
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .filter_map(|i| match &self.items[i as usize] {
                Some((it, bb)) if bb.intersects(b) => Some((i, *it)),
                _ => None,
            })
            .collect()
    }
}

/// Exact clearance checks against an index.
pub(crate) struct Checker<'a> {
    pub rb: &'a RouterBoard,
    pub index: &'a Index,
}

impl Checker<'_> {
    /// First thing a track of `net` (half width from its profile) on stackup layer `layer`
    /// along `a`–`b` would violate, ignoring copper of its own net.
    pub fn seg(&self, layer: usize, a: P, b: P, net: u32) -> Option<Blocker> {
        self.seg_margin(layer, a, b, net, 0.0)
    }

    /// [`Checker::seg`] keeping `extra` more than the rules require (for chords standing for
    /// an arc).
    pub fn seg_margin(&self, layer: usize, a: P, b: P, net: u32, extra: f64) -> Option<Blocker> {
        self.seg_ext(layer, a, b, net, extra, None)
    }

    /// [`Checker::seg_margin`] that also ignores the copper of net `skip` (the other net of a
    /// differential pair, checked separately against the pair's gap).
    pub fn seg_ext(&self, layer: usize, a: P, b: P, net: u32, extra: f64, skip: Option<u32>) -> Option<Blocker> {
        self.seg_impl(layer, a, b, net, extra, skip, false)
    }

    /// [`Checker::seg`] against the static obstacles only (routed copper in the index is
    /// ignored).
    pub fn seg_static(&self, layer: usize, a: P, b: P, net: u32) -> Option<Blocker> {
        self.seg_impl(layer, a, b, net, 0.0, None, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn seg_impl(
        &self,
        layer: usize,
        a: P,
        b: P,
        net: u32,
        extra: f64,
        skip: Option<u32>,
        statics: bool,
    ) -> Option<Blocker> {
        let rb = self.rb;
        let pr = rb.profile(net);
        if !rb.inside(a) || !rb.inside(b) {
            return Some(Blocker::Outside);
        }
        let scoped = rb.scoped.on;
        // Pieces of the track with the state of their points (scoped rules).
        let mine: Vec<(P, P, St)> =
            if scoped { rb.pieces(pr, layer, a, b, pr.hw + extra) } else { vec![(a, b, St::default())] };
        let q = BoxF::of2(a, b).expand(pr.hw + extra + self.index.reach);
        let bit = 1u64 << layer.min(63);
        for (_, it) in self.index.query(&q) {
            match it {
                Item::Static(o) => {
                    let ob = &rb.obstacles[o as usize];
                    let own = ob.net == Some(net) || (skip.is_some() && ob.net == skip);
                    if ob.tracks & bit == 0 || (own && matches!(ob.kind, ObKind::Pad | ObKind::Copper)) {
                        continue;
                    }
                    if !scoped || ob.item.is_none() {
                        let req = pr.hw + ob.clear.with(pr.c) + extra;
                        if !ob.bbox.expand(req).intersects(&BoxF::of2(a, b)) {
                            continue;
                        }
                        if ob.shape.dist_seg(a, b) < req - TOL {
                            return Some(Blocker::Obstacle(o));
                        }
                        continue;
                    }
                    for &(pa, pb, st) in &mine {
                        let req = pr.hw + rb.c_ob(pr, st, ob) + extra;
                        if ob.bbox.expand(req).intersects(&BoxF::of2(pa, pb)) && ob.shape.dist_seg(pa, pb) < req - TOL {
                            return Some(Blocker::Obstacle(o));
                        }
                    }
                }
                _ if statics => {}
                Item::Seg { net: m, layer: l, a: c, b: d } => {
                    if m == net || Some(m) == skip || l as usize != layer {
                        continue;
                    }
                    let po = rb.profile(m);
                    if rb.segs_conflict(pr, layer, &mine, extra, po, c, d, po.hw) {
                        return Some(Blocker::Net(m, Some(l)));
                    }
                }
                Item::Sized { net: m, layer: l, a: c, b: d, hw } => {
                    if m == net || Some(m) == skip || l as usize != layer {
                        continue;
                    }
                    let po = rb.profile(m);
                    if rb.segs_conflict(pr, layer, &mine, extra, po, c, d, hw) {
                        return Some(Blocker::Net(m, Some(l)));
                    }
                }
                Item::Via { net: m, at } => {
                    if m == net || Some(m) == skip {
                        continue;
                    }
                    let po = rb.profile(m);
                    if rb.seg_via_conflict(pr, &mine, extra, po, at) {
                        return Some(Blocker::Net(m, None));
                    }
                }
            }
        }
        None
    }

    /// First thing a through via of `net` at `at` would violate. Vias of its own net are
    /// checked for hole-to-hole distance (`skip`: index id of the via itself).
    pub fn via(&self, at: P, net: u32, skip: Option<u32>) -> Option<Blocker> {
        self.via_impl(at, net, skip, false)
    }

    /// [`Checker::via`] against the static obstacles and holes only.
    pub fn via_static(&self, at: P, net: u32) -> Option<Blocker> {
        self.via_impl(at, net, None, true)
    }

    /// [`Checker::via`] for a via of pad radius `rv` and drill radius `dr` instead of the
    /// net's.
    pub fn via_sized(&self, at: P, net: u32, rv: f64, dr: f64) -> Option<Blocker> {
        let mut pr = self.rb.profile(net).clone();
        pr.rv = rv;
        pr.dr = dr;
        self.via_with(&pr, at, net, None, false)
    }

    fn via_impl(&self, at: P, net: u32, skip: Option<u32>, statics: bool) -> Option<Blocker> {
        self.via_with(self.rb.profile(net), at, net, skip, statics)
    }

    fn via_with(
        &self,
        pr: &super::model::Profile,
        at: P,
        net: u32,
        skip: Option<u32>,
        statics: bool,
    ) -> Option<Blocker> {
        let rb = self.rb;
        if !rb.inside(at) {
            return Some(Blocker::Outside);
        }
        for h in &rb.holes {
            if h.at.dist(at) < pr.dr + h.r + rb.h2h - TOL {
                return Some(Blocker::Hole);
            }
        }
        let st = rb.st_via(pr, at, 0.0);
        let q = BoxF::of2(at, at).expand(pr.rv.max(pr.dr) + self.index.reach);
        for (id, it) in self.index.query(&q) {
            if Some(id) == skip {
                continue;
            }
            match it {
                Item::Static(o) => {
                    let ob = &rb.obstacles[o as usize];
                    if !ob.vias {
                        continue;
                    }
                    let own = ob.net == Some(net);
                    let req = match ob.kind {
                        // No via in a pad, even of its own net.
                        ObKind::Pad if own => pr.rv,
                        ObKind::Copper if own => continue,
                        _ => rb.via_out(pr, rb.c_ob(pr, st, ob)),
                    };
                    if ob.shape.dist_point(at) < req - TOL {
                        return Some(Blocker::Obstacle(o));
                    }
                }
                _ if statics => {}
                Item::Seg { net: m, layer, a, b } => {
                    if m == net {
                        continue;
                    }
                    let po = rb.profile(m);
                    if rb.via_seg_conflict(pr, at, st, po, layer as usize, a, b, po.hw) {
                        return Some(Blocker::Net(m, Some(layer)));
                    }
                }
                Item::Sized { net: m, layer, a, b, hw } => {
                    if m == net {
                        continue;
                    }
                    let po = rb.profile(m);
                    if rb.via_seg_conflict(pr, at, st, po, layer as usize, a, b, hw) {
                        return Some(Blocker::Net(m, Some(layer)));
                    }
                }
                Item::Via { net: m, at: o } => {
                    let po = rb.profile(m);
                    let req = if m == net {
                        pr.dr + po.dr + rb.h2h
                    } else {
                        rb.via_via(pr, po, rb.c_cu(pr, st, po, rb.st_via(po, o, 0.0)))
                    };
                    if at.dist(o) < req - TOL {
                        return Some(Blocker::Net(m, None));
                    }
                }
            }
        }
        None
    }
}
