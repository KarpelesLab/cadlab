//! The router's own view of the board (`RouterBoard` in docs/ROUTER.md): nets with their rule
//! profiles, obstacles with the clearance they demand, drilled holes, the board outline and the
//! connectable copper (terminals) grouped in islands.

use std::collections::{BTreeMap, BTreeSet};

use crate::board::{self as geo, COPPER_TOL, CopperItem, ItemRef};
use crate::model::Project;
use crate::model::board::Rules;
use crate::units::Nm;

use super::geo::{BoxF, P, Poly, Shape, point_seg_d2, seg_seg_dist};
use super::index::TOL;
use super::scoped::{Kind, Scoped, St, Who};

/// Routing rules of a net: track width, clearance and via size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ProfileKey {
    pub width: Nm,
    pub clear: Nm,
    pub via_drill: Nm,
    pub via_dia: Nm,
}

/// A rule profile with its values as floats (half widths and radii).
#[derive(Clone, Debug)]
pub(crate) struct Profile {
    pub key: ProfileKey,
    /// Half track width.
    pub hw: f64,
    /// Clearance.
    pub c: f64,
    /// Via pad radius.
    pub rv: f64,
    /// Via drill radius.
    pub dr: f64,
    /// How far the copper's own centerlines may lie from the routed centerline (a
    /// differential pair's centerline: half its pitch; 0 otherwise), for scoped rules.
    pub inner: f64,
}

/// How much room an obstacle demands.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Clear {
    /// Copper of a net with this clearance: the larger of both nets' clearances applies.
    Net(f64),
    /// A fixed distance (board edge, keep-outs).
    Fixed(f64),
}

impl Clear {
    pub fn with(self, c: f64) -> f64 {
        match self {
            Clear::Net(x) => x.max(c),
            Clear::Fixed(x) => x,
        }
    }
}

/// What an obstacle is, for failure reports and hints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObKind {
    /// A pad.
    Pad,
    /// An existing track or via.
    Copper,
    /// A board edge or cutout.
    Edge,
    /// A keep-out area.
    Keepout,
    /// A non-plated hole.
    Npth,
}

/// Something routing must keep away from.
#[derive(Clone, Debug)]
pub(crate) struct Obstacle {
    pub shape: Shape,
    pub bbox: BoxF,
    /// Copper layers (bit per stackup layer) where tracks must keep away.
    pub tracks: u64,
    /// Whether vias must keep away.
    pub vias: bool,
    /// Net of the copper (`None`: blocks every net).
    pub net: Option<u32>,
    pub clear: Clear,
    pub kind: ObKind,
    /// For reports: `pad U3.4 (net GND)`, `board edge`, ...
    pub label: String,
    /// Component owning it (pads), for hints.
    pub owner: Option<String>,
    /// An unlocked straight track or unlocked via: something push-and-shove may move.
    pub movable: Option<ItemRef>,
    /// The copper item it is (index into the items the board was built from), whose clearance
    /// the scoped rules resolve exactly (`None`: `clear` decides).
    pub item: Option<u32>,
}

/// A drilled hole (vias keep `hole_to_hole` from it).
#[derive(Clone, Debug)]
pub(crate) struct Hole {
    pub at: P,
    pub r: f64,
    /// The via drilling it, if any.
    pub via: Option<crate::id::ObjectId>,
}

/// Where a stub to a terminal ends.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Anchor {
    /// Pad or via center.
    Center(P),
    /// Along a track's centerline.
    Segment(P, P),
}

/// A connectable piece of copper of a net: pad, via or track.
#[derive(Clone, Debug)]
pub(crate) struct Terminal {
    pub net: u32,
    pub shape: Shape,
    /// Copper layers (bit per stackup layer).
    pub layers: u64,
    pub anchor: Anchor,
    /// `U1.3`, `via#4`, `track#7`.
    pub label: String,
    /// Copper island (index of its lowest item).
    pub island: usize,
    /// Whether this is a pad (ratsnest endpoint).
    pub pad: bool,
    /// Representative point.
    pub at: P,
}

/// The router's board.
pub(crate) struct RouterBoard {
    pub layer_names: Vec<String>,
    pub nets: Vec<String>,
    pub net_ids: BTreeMap<String, u32>,
    pub net_profile: Vec<usize>,
    /// The profile of each net's class: differs from `net_profile` for a net routed at a
    /// neck-down width (see [`RouterBoard::neck`]).
    pub class_profile: Vec<usize>,
    pub profiles: Vec<Profile>,
    pub obstacles: Vec<Obstacle>,
    pub holes: Vec<Hole>,
    pub outer: Option<Poly>,
    pub cutouts: Vec<Poly>,
    pub bbox: BoxF,
    pub h2h: f64,
    pub rules: Rules,
    pub terminals: Vec<Terminal>,
    /// Custom clearance rules, local clearances and the hole clearance, for new copper.
    pub scoped: Scoped,
}

fn class_of<'a>(p: &'a Project, net: &str) -> Option<&'a crate::model::circuit::NetClass> {
    let c = p.circuit();
    c.nets.get(net).and_then(|n| n.class.as_ref()).and_then(|k| c.netclasses.get(k))
}

/// The rule profile of a net (class values over the board rules).
pub(crate) fn profile_key(p: &Project, net: Option<&str>) -> ProfileKey {
    let r = &p.board().rules;
    let cl = net.and_then(|n| class_of(p, n));
    ProfileKey {
        width: net
            .and_then(|n| crate::lengths::pair_width(p.circuit(), n))
            .or_else(|| cl.and_then(|c| c.track_width))
            .unwrap_or(r.track_width),
        clear: cl.and_then(|c| c.clearance).unwrap_or(r.clearance),
        via_drill: cl.and_then(|c| c.via_drill).unwrap_or(r.via_drill),
        via_dia: cl.and_then(|c| c.via_diameter).unwrap_or(r.via_diameter),
    }
}

pub(crate) fn layer_mask(names: &[String], layers: &[String]) -> u64 {
    layers.iter().filter_map(|l| names.iter().position(|n| n == l)).fold(0, |m, i| m | (1u64 << i.min(63)))
}

fn ring_p(r: &[polyclip::Point]) -> Vec<P> {
    r.iter().map(|q| P::new(q.x as f64, q.y as f64)).collect()
}

impl RouterBoard {
    /// Builds the router's view. `items` are all copper items (with zone fills, for
    /// connectivity) and `isl` their island indices.
    pub fn build(p: &Project, items: &[CopperItem], isl: &[usize]) -> RouterBoard {
        let board = p.board();
        let rules = board.rules.clone();
        let layer_names = board.stackup.copper_names();
        let all = layer_mask(&layer_names, &layer_names);
        // Nets: circuit nets plus any net named by board copper.
        let mut names: BTreeSet<String> = p.circuit().nets.keys().cloned().collect();
        names.extend(board.tracks.iter().filter_map(|t| t.net.clone()));
        names.extend(board.vias.iter().filter_map(|v| v.net.clone()));
        let nets: Vec<String> = names.into_iter().collect();
        let net_ids: BTreeMap<String, u32> = nets.iter().enumerate().map(|(i, n)| (n.clone(), i as u32)).collect();
        let mut keys: Vec<ProfileKey> = nets.iter().map(|n| profile_key(p, Some(n))).collect();
        let default_key = profile_key(p, None);
        keys.push(default_key);
        let mut uniq: Vec<ProfileKey> = keys.clone();
        uniq.sort();
        uniq.dedup();
        let profiles: Vec<Profile> = uniq
            .iter()
            .map(|k| Profile {
                key: *k,
                hw: k.width.0 as f64 / 2.0,
                c: k.clear.0 as f64,
                rv: k.via_dia.0 as f64 / 2.0,
                dr: k.via_drill.0 as f64 / 2.0,
                inner: 0.0,
            })
            .collect();
        let net_profile: Vec<usize> =
            keys[..nets.len()].iter().map(|k| uniq.binary_search(k).expect("profile")).collect();
        let net_clear = |net: Option<&str>| -> f64 { profile_key(p, net).clear.0 as f64 };

        let scoped = Scoped::new(p, items, &layer_names);
        let mut obstacles = Vec::new();
        let mut terminals = Vec::new();
        let pads = geo::placed_pads(p);
        for (i, it) in items.iter().enumerate() {
            if matches!(it.item, ItemRef::Zone(..)) {
                continue; // fills are derived: they make room for new tracks when refilled
            }
            let mut movable = None;
            let (shape, anchor) = match &it.item {
                ItemRef::Track(id) => {
                    let t = board.tracks.iter().find(|t| t.id == *id).expect("track");
                    // Differential pair copper stays coupled: push-and-shove never moves it.
                    let paired = t.net.as_deref().is_some_and(|n| p.circuit().diffpair_of(n).is_some());
                    if !t.locked && t.mid.is_none() && !paired {
                        movable = Some(it.item.clone());
                    }
                    let (a, b) = (P::of(t.start), P::of(t.end));
                    let shape = if t.mid.is_none() {
                        Shape::Capsule { a, b, r: t.width.0 as f64 / 2.0 }
                    } else {
                        Shape::of_set(&it.shape)
                    };
                    (shape, Anchor::Segment(a, b))
                }
                ItemRef::Via(id) => {
                    let v = board.vias.iter().find(|v| v.id == *id).expect("via");
                    let paired = v.net.as_deref().is_some_and(|n| p.circuit().diffpair_of(n).is_some());
                    if !v.locked && !paired {
                        movable = Some(it.item.clone());
                    }
                    let c = P::of(v.at);
                    (Shape::Capsule { a: c, b: c, r: v.diameter.0 as f64 / 2.0 }, Anchor::Center(c))
                }
                _ => (Shape::of_set(&it.shape), Anchor::Center(P::of(it.anchor))),
            };
            let layers = layer_mask(&layer_names, &it.layers);
            let net = it.net.as_ref().and_then(|n| net_ids.get(n)).copied();
            let (kind, owner) = match &it.item {
                ItemRef::Pad(r, _) => (ObKind::Pad, Some(r.clone())),
                _ => (ObKind::Copper, None),
            };
            let what = match &it.item {
                ItemRef::Pad(..) => format!("pad {}", it.item),
                other => other.to_string(),
            };
            let label = match &it.net {
                Some(n) => format!("{what} (net {n})"),
                None => format!("{what} (no net)"),
            };
            if let Some(n) = net {
                terminals.push(Terminal {
                    net: n,
                    shape: shape.clone(),
                    layers,
                    anchor,
                    label: it.item.to_string(),
                    island: isl[i],
                    pad: matches!(it.item, ItemRef::Pad(..)),
                    at: P::of(it.anchor),
                });
            }
            obstacles.push(Obstacle {
                bbox: shape.bbox(),
                shape,
                tracks: layers,
                vias: true,
                net,
                clear: Clear::Net(net_clear(it.net.as_deref())),
                kind,
                label,
                owner,
                movable,
                item: Some(i as u32),
            });
        }
        // Holes.
        let mut holes = Vec::new();
        for pp in &pads {
            if let Some((d, plated)) = pp.hole {
                let c = P::of(pp.hole_at);
                holes.push(Hole { at: c, r: d.0 as f64 / 2.0, via: None });
                if !plated {
                    let shape = Shape::Capsule { a: c, b: c, r: d.0 as f64 / 2.0 };
                    obstacles.push(Obstacle {
                        bbox: shape.bbox(),
                        shape,
                        tracks: all,
                        vias: true,
                        net: None,
                        // The copper-to-hole clearance when the rules set one (the DRC checks it),
                        // else the copper clearance; a net's larger clearance still applies.
                        clear: Clear::Net(if rules.hole_clearance.0 > 0 {
                            rules.hole_clearance.0 as f64
                        } else {
                            rules.clearance.0 as f64
                        }),
                        kind: ObKind::Npth,
                        label: format!("hole of {}", pp.refdes),
                        owner: Some(pp.refdes.clone()),
                        movable: None,
                        item: None,
                    });
                }
            }
        }
        for v in &board.vias {
            holes.push(Hole { at: P::of(v.at), r: v.drill.0 as f64 / 2.0, via: Some(v.id) });
        }
        // Plated holes keep the hole clearance from copper of other nets (the DRC's
        // `drc.hole_clearance`); a via's hole moves with it.
        if scoped.hc > 0.0 {
            let id_of = |n: &Option<String>| n.as_ref().and_then(|n| net_ids.get(n)).copied();
            for pp in pads.iter().filter(|pp| pp.hole.is_some_and(|h| h.1)) {
                let r = pp.hole.map_or(0.0, |h| h.0.0 as f64 / 2.0);
                let (a, b) = pp.slot.map_or((P::of(pp.hole_at), P::of(pp.hole_at)), |(a, b)| (P::of(a), P::of(b)));
                let shape = Shape::Capsule { a, b, r };
                obstacles.push(Obstacle {
                    bbox: shape.bbox(),
                    shape,
                    tracks: all,
                    vias: true,
                    net: id_of(&pp.net),
                    clear: Clear::Fixed(scoped.hc),
                    kind: ObKind::Copper,
                    label: format!("hole of pad {}.{}", pp.refdes, pp.number),
                    owner: Some(pp.refdes.clone()),
                    movable: None,
                    item: None,
                });
            }
            for v in &board.vias {
                let c = P::of(v.at);
                let shape = Shape::Capsule { a: c, b: c, r: v.drill.0 as f64 / 2.0 };
                let paired = v.net.as_deref().is_some_and(|n| p.circuit().diffpair_of(n).is_some());
                obstacles.push(Obstacle {
                    bbox: shape.bbox(),
                    shape,
                    tracks: all,
                    vias: true,
                    net: id_of(&v.net),
                    clear: Clear::Fixed(scoped.hc),
                    kind: ObKind::Copper,
                    label: format!("hole of via#{}", v.id.0),
                    owner: None,
                    movable: (!v.locked && !paired).then_some(ItemRef::Via(v.id)),
                    item: None,
                });
            }
        }
        // Outline: edges keep copper_to_edge; inside test on the rings.
        let mut outer = None;
        let mut cutouts = Vec::new();
        let mut bbox = BoxF::EMPTY;
        for (k, c) in board.outline.contours.iter().enumerate() {
            let ring = ring_p(&geo::contour_ring(c, COPPER_TOL));
            if ring.len() < 3 {
                continue;
            }
            for e in 0..ring.len() {
                let (a, b) = (ring[e], ring[(e + 1) % ring.len()]);
                let shape = Shape::Capsule { a, b, r: 0.0 };
                obstacles.push(Obstacle {
                    bbox: shape.bbox(),
                    shape,
                    tracks: all,
                    vias: true,
                    net: None,
                    clear: Clear::Fixed(rules.copper_to_edge.0 as f64),
                    kind: ObKind::Edge,
                    label: if k == 0 { "board edge".into() } else { "board cutout".into() },
                    owner: None,
                    movable: None,
                    item: None,
                });
            }
            let poly = Poly::new(vec![ring]);
            if outer.is_none() {
                bbox = poly.bbox;
                outer = Some(poly);
            } else {
                cutouts.push(poly);
            }
        }
        // Keep-outs.
        for k in &board.keepouts {
            if k.outline.len() < 3 || !(k.no_tracks || k.no_vias) {
                continue;
            }
            let shape = Shape::of_ring(k.outline.iter().map(|q| P::of(*q)).collect());
            let layers = if k.layers.is_empty() { all } else { layer_mask(&layer_names, &k.layers) };
            obstacles.push(Obstacle {
                bbox: shape.bbox(),
                shape,
                tracks: if k.no_tracks { layers } else { 0 },
                vias: k.no_vias,
                net: None,
                clear: Clear::Fixed(0.0),
                kind: ObKind::Keepout,
                label: format!("keep-out `{}`", k.name),
                owner: None,
                movable: None,
                item: None,
            });
        }
        RouterBoard {
            layer_names,
            nets,
            net_ids,
            class_profile: net_profile.clone(),
            net_profile,
            profiles,
            obstacles,
            holes,
            outer,
            cutouts,
            bbox,
            h2h: rules.hole_to_hole.0 as f64,
            rules,
            terminals,
            scoped,
        }
    }

    /// Whether `pt` lies inside the board (outer contour, outside every cutout).
    pub fn inside(&self, pt: P) -> bool {
        self.outer.as_ref().is_some_and(|o| o.contains(pt)) && !self.cutouts.iter().any(|c| c.contains(pt))
    }

    /// Routes `net` at `width` (a neck-down: narrower than its class width): its profile
    /// becomes a copy of its class profile with that track width.
    pub fn neck(&mut self, net: u32, width: Nm) {
        let mut key = self.profiles[self.class_profile[net as usize]].key;
        key.width = width;
        let pi = match self.profiles.iter().position(|p| p.key == key) {
            Some(i) => i,
            None => {
                let c = &self.profiles[self.class_profile[net as usize]];
                let p = Profile { key, hw: width.0 as f64 / 2.0, c: c.c, rv: c.rv, dr: c.dr, inner: c.inner };
                self.profiles.push(p);
                self.profiles.len() - 1
            }
        };
        self.net_profile[net as usize] = pi;
    }

    /// Profile of a net's class (its full width, when it is routed necked down).
    pub fn class_of(&self, net: u32) -> &Profile {
        &self.profiles[self.class_profile[net as usize]]
    }

    /// Profile of a net.
    pub fn profile(&self, net: u32) -> &Profile {
        &self.profiles[self.net_profile[net as usize]]
    }

    // ---- clearances (scoped rules, see `scoped`) ------------------------------------------

    /// Clearance between new copper of profile `pr` in state `st` and obstacle `ob`.
    pub fn c_ob(&self, pr: &Profile, st: St, ob: &Obstacle) -> f64 {
        match ob.item {
            Some(i) if self.scoped.on => self.scoped.resolve(Who::New(st, pr.c), Who::Item(i)),
            _ => ob.clear.with(pr.c),
        }
    }

    /// Clearance between new copper of profiles `a` and `b` (different nets) in states `sa`,
    /// `sb`.
    pub fn c_cu(&self, a: &Profile, sa: St, b: &Profile, sb: St) -> f64 {
        if self.scoped.on { self.scoped.resolve(Who::New(sa, a.c), Who::New(sb, b.c)) } else { a.c.max(b.c) }
    }

    /// State of a via of profile `pr` at `p` (centered within `r` of it).
    pub fn st_via(&self, pr: &Profile, p: P, r: f64) -> St {
        if !self.scoped.positional() {
            return St::default();
        }
        self.scoped.st(Kind::Via, 0, p, r, pr.rv)
    }

    /// State of a whole straight track of profile `pr` (or half width `hw`) on `layer`.
    pub fn st_seg(&self, pr: &Profile, layer: usize, a: P, b: P, hw: f64) -> St {
        if !self.scoped.positional() {
            return St::default();
        }
        self.scoped.st_seg(layer, a, b, pr.inner, hw)
    }

    /// Pieces of a straight track of profile `pr` (half width `hw`) with uniform states.
    pub fn pieces(&self, pr: &Profile, layer: usize, a: P, b: P, hw: f64) -> Vec<(P, P, St)> {
        self.scoped.split(layer, a, b, pr.inner, hw)
    }

    /// Distance from the center of a via of profile `pr` to copper of another net at copper
    /// clearance `c` (hole clearance included).
    pub fn via_out(&self, pr: &Profile, c: f64) -> f64 {
        self.scoped.via_out(pr.rv, pr.dr, c)
    }

    /// Distance between the centers of vias of profiles `a` and `b` of different nets at
    /// copper clearance `c`.
    pub fn via_via(&self, a: &Profile, b: &Profile, c: f64) -> f64 {
        self.scoped.via_via((a.rv, a.dr), (b.rv, b.dr), c, self.h2h)
    }

    /// The largest clearance any check may need (custom rules included).
    pub fn max_clear(&self) -> f64 {
        let mut c = (self.rules.clearance.0 as f64).max(self.scoped.max_clear()).max(self.scoped.hc);
        for p in &self.profiles {
            c = c.max(p.c);
        }
        c
    }

    /// Whether a track of profile `pr` (its pieces `mine`, with `extra` beyond its half width)
    /// comes closer than allowed to a track of profile `po` (half width `hwo`) from `c` to `d` on
    /// the same `layer`.
    #[allow(clippy::too_many_arguments)]
    pub fn segs_conflict(
        &self,
        pr: &Profile,
        layer: usize,
        mine: &[(P, P, St)],
        extra: f64,
        po: &Profile,
        c: P,
        d: P,
        hwo: f64,
    ) -> bool {
        let (a, b) = (mine[0].0, mine[mine.len() - 1].1);
        if !self.scoped.on {
            return seg_seg_dist(a, b, c, d) < pr.hw + hwo + pr.c.max(po.c) + extra - TOL;
        }
        if !self.scoped.positional() {
            let cl = self.c_cu(pr, St::default(), po, St::default());
            return seg_seg_dist(a, b, c, d) < pr.hw + hwo + cl + extra - TOL;
        }
        let dist = seg_seg_dist(a, b, c, d);
        if dist >= pr.hw + hwo + self.max_clear() + extra {
            return false;
        }
        let theirs = self.pieces(po, layer, c, d, hwo);
        mine.iter().any(|&(pa, pb, sa)| {
            theirs.iter().any(|&(pc, pd, sb)| {
                seg_seg_dist(pa, pb, pc, pd) < pr.hw + hwo + self.c_cu(pr, sa, po, sb) + extra - TOL
            })
        })
    }

    /// Whether a track of profile `pr` (pieces `mine`, `extra` beyond its half width) comes
    /// closer than allowed to a via of profile `po` at `at` (of another net).
    pub fn seg_via_conflict(&self, pr: &Profile, mine: &[(P, P, St)], extra: f64, po: &Profile, at: P) -> bool {
        let (a, b) = (mine[0].0, mine[mine.len() - 1].1);
        if !self.scoped.on {
            return point_seg_d2(at, a, b).sqrt() < pr.hw + self.via_out(po, pr.c.max(po.c)) + extra - TOL;
        }
        if !self.scoped.positional() {
            let cl = self.c_cu(pr, St::default(), po, St::default());
            return point_seg_d2(at, a, b).sqrt() < pr.hw + self.via_out(po, cl) + extra - TOL;
        }
        let dist = point_seg_d2(at, a, b).sqrt();
        if dist >= pr.hw + self.via_out(po, self.max_clear()) + extra {
            return false;
        }
        let sv = self.st_via(po, at, 0.0);
        mine.iter().any(|&(pa, pb, sa)| {
            point_seg_d2(at, pa, pb).sqrt() < pr.hw + self.via_out(po, self.c_cu(pr, sa, po, sv)) + extra - TOL
        })
    }

    /// Whether a via of profile `pr` at `at` (state `st`) comes closer than allowed to a track of
    /// profile `po` (half width `hwo`) from `a` to `b` on `layer` (of another net).
    #[allow(clippy::too_many_arguments)]
    pub fn via_seg_conflict(
        &self,
        pr: &Profile,
        at: P,
        st: St,
        po: &Profile,
        layer: usize,
        a: P,
        b: P,
        hwo: f64,
    ) -> bool {
        if !self.scoped.on {
            return point_seg_d2(at, a, b).sqrt() < self.via_out(pr, pr.c.max(po.c)) + hwo - TOL;
        }
        if !self.scoped.positional() {
            let cl = self.c_cu(pr, st, po, St::default());
            return point_seg_d2(at, a, b).sqrt() < self.via_out(pr, cl) + hwo - TOL;
        }
        let dist = point_seg_d2(at, a, b).sqrt();
        if dist >= self.via_out(pr, self.max_clear()) + hwo {
            return false;
        }
        self.pieces(po, layer, a, b, hwo).iter().any(|&(pa, pb, sb)| {
            point_seg_d2(at, pa, pb).sqrt() < self.via_out(pr, self.c_cu(pr, st, po, sb)) + hwo - TOL
        })
    }

    /// State of tracks of profile `pr` anywhere in `bbox` on any of `layers` (all copper
    /// layers when empty).
    pub fn st_track_box(&self, pr: &Profile, layers: &[usize], bbox: &BoxF) -> St {
        if !self.scoped.positional() {
            return St::default();
        }
        let all: Vec<usize>;
        let layers = if layers.is_empty() {
            all = (0..self.layer_names.len()).collect();
            &all[..]
        } else {
            layers
        };
        self.scoped.st_box(Kind::Track, layers, &bbox.expand(pr.inner), pr.hw)
    }

    /// State of vias of profile `pr` anywhere in `bbox`.
    pub fn st_via_box(&self, pr: &Profile, bbox: &BoxF) -> St {
        if !self.scoped.positional() {
            return St::default();
        }
        self.scoped.st_box(Kind::Via, &[], bbox, pr.rv)
    }

    /// A routed straight track from `a` to `b` on `layer` (half width `hw`) as the tracks to
    /// emit: split where the state of the scoped rules changes along it, so the DRC decides
    /// each piece as the router assumed (see `scoped`). Endpoints in nanometers; one piece
    /// without positional rules.
    pub fn split_track(&self, layer: usize, a: P, b: P, hw: f64) -> Vec<(crate::geom::Point, crate::geom::Point)> {
        let (pa, pb) = (a.to_point(), b.to_point());
        if !self.scoped.positional() || pa == pb {
            return vec![(pa, pb)];
        }
        let mut pts = vec![pa];
        for (_, q, _) in self.scoped.split(layer, a, b, 0.0, hw) {
            let q = q.to_point();
            if *pts.last().expect("start") != q {
                pts.push(q);
            }
        }
        if *pts.last().expect("start") != pb {
            pts.push(pb);
        }
        pts.windows(2).map(|w| (w[0], w[1])).collect()
    }

    /// The clearance new tracks of terminal `t`'s net keep near it from the other pads of its
    /// component (the largest; the net's class clearance without scoped rules).
    pub fn c_pad_near(&self, t: usize) -> f64 {
        let term = &self.terminals[t];
        let pr = self.profile(term.net);
        if !self.scoped.on {
            return pr.c;
        }
        let Some((comp, _)) = term.label.split_once('.') else { return pr.c };
        let bb = term.shape.bbox().expand(pr.hw + self.max_clear());
        let st = self.st_track_box(pr, &[], &bb);
        let mut c: Option<f64> = None;
        for ob in &self.obstacles {
            if ob.kind == ObKind::Pad
                && ob.owner.as_deref() == Some(comp)
                && ob.net != Some(term.net)
                && ob.bbox.intersects(&bb)
            {
                let v = self.c_ob(pr, st, ob);
                c = Some(c.map_or(v, |x: f64| x.max(v)));
            }
        }
        c.unwrap_or_else(|| self.c_cu(pr, st, pr, st))
    }

    /// The clearance between two new tracks of terminal `t`'s net profile near it (two escapes
    /// side by side).
    pub fn c_tracks_near(&self, t: usize) -> f64 {
        let term = &self.terminals[t];
        let pr = self.profile(term.net);
        if !self.scoped.on {
            return pr.c;
        }
        let bb = term.shape.bbox().expand(pr.hw + self.max_clear());
        let st = self.st_track_box(pr, &[], &bb);
        self.c_cu(pr, st, pr, st)
    }

    /// The narrowest track the custom track width rules allow near terminal `t` (0 when none).
    pub fn min_width_near(&self, t: usize) -> f64 {
        if !self.scoped.has_widths() {
            return 0.0;
        }
        let term = &self.terminals[t];
        let pr = self.profile(term.net);
        let st = self.st_track_box(pr, &[], &term.shape.bbox().expand(pr.hw));
        self.scoped.min_width(st)
    }

    /// The clearance between new tracks of terminal `t`'s net and of net `other` near it.
    pub fn c_nets_near(&self, t: usize, other: u32) -> f64 {
        let term = &self.terminals[t];
        let (pr, po) = (self.profile(term.net), self.profile(other));
        if !self.scoped.on {
            return pr.c.max(po.c);
        }
        let bb = term.shape.bbox().expand(pr.hw + self.max_clear());
        let st = self.st_track_box(pr, &[], &bb);
        self.c_cu(pr, st, po, st)
    }
}
