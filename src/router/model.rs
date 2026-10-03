//! The router's own view of the board (`RouterBoard` in docs/ROUTER.md): nets with their rule
//! profiles, obstacles with the clearance they demand, drilled holes, the board outline and the
//! connectable copper (terminals) grouped in islands.

use std::collections::{BTreeMap, BTreeSet};

use crate::board::{self as geo, COPPER_TOL, CopperItem, ItemRef};
use crate::model::Project;
use crate::model::board::Rules;
use crate::units::Nm;

use super::geo::{BoxF, P, Poly, Shape};

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
}

/// A drilled hole (vias keep `hole_to_hole` from it).
#[derive(Clone, Debug)]
pub(crate) struct Hole {
    pub at: P,
    pub r: f64,
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
    pub profiles: Vec<Profile>,
    pub obstacles: Vec<Obstacle>,
    pub holes: Vec<Hole>,
    pub outer: Option<Poly>,
    pub cutouts: Vec<Poly>,
    pub bbox: BoxF,
    pub h2h: f64,
    pub rules: Rules,
    pub terminals: Vec<Terminal>,
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
        width: cl.and_then(|c| c.track_width).unwrap_or(r.track_width),
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
            })
            .collect();
        let net_profile: Vec<usize> =
            keys[..nets.len()].iter().map(|k| uniq.binary_search(k).expect("profile")).collect();
        let net_clear = |net: Option<&str>| -> f64 { profile_key(p, net).clear.0 as f64 };

        let mut obstacles = Vec::new();
        let mut terminals = Vec::new();
        let pads = geo::placed_pads(p);
        for (i, it) in items.iter().enumerate() {
            if matches!(it.item, ItemRef::Zone(..)) {
                continue; // fills are derived: they make room for new tracks when refilled
            }
            let (shape, anchor) = match &it.item {
                ItemRef::Track(id) => {
                    let t = board.tracks.iter().find(|t| t.id == *id).expect("track");
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
            });
        }
        // Holes.
        let mut holes = Vec::new();
        for pp in &pads {
            if let Some((d, plated)) = pp.hole {
                let c = P::of(pp.center);
                holes.push(Hole { at: c, r: d.0 as f64 / 2.0 });
                if !plated {
                    let shape = Shape::Capsule { a: c, b: c, r: d.0 as f64 / 2.0 };
                    obstacles.push(Obstacle {
                        bbox: shape.bbox(),
                        shape,
                        tracks: all,
                        vias: true,
                        net: None,
                        clear: Clear::Net(rules.clearance.0 as f64),
                        kind: ObKind::Npth,
                        label: format!("hole of {}", pp.refdes),
                        owner: Some(pp.refdes.clone()),
                    });
                }
            }
        }
        for v in &board.vias {
            holes.push(Hole { at: P::of(v.at), r: v.drill.0 as f64 / 2.0 });
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
            });
        }
        RouterBoard {
            layer_names,
            nets,
            net_ids,
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
        }
    }

    /// Whether `pt` lies inside the board (outer contour, outside every cutout).
    pub fn inside(&self, pt: P) -> bool {
        self.outer.as_ref().is_some_and(|o| o.contains(pt)) && !self.cutouts.iter().any(|c| c.contains(pt))
    }

    /// Profile of a net.
    pub fn profile(&self, net: u32) -> &Profile {
        &self.profiles[self.net_profile[net as usize]]
    }
}
