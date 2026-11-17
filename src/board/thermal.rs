//! Pads in copper of their own net: what joins each pad to a pour, measured on the filled
//! board. See `docs/BOARD.md`, "Thermal relief".
//!
//! A pad soldered by hand, by wave or in an oven that sits solidly in a large copper area cannot
//! be heated properly: the copper carries the heat away (cold joints, tombstoned chip parts). A
//! thermal relief joins the pad through a few narrow spokes instead. Whether a pad is relieved
//! is read here from the copper itself, not from the zone settings: in a band [`BAND`] wide
//! around the pad's outline,
//!
//! ```text
//! spokes  = separate pieces of zone copper in the band        (the necks reaching the pad)
//! necks   = the same with the net's tracks, vias and copper drawings
//! contact = area of that copper in the band / BAND             (outline length it covers)
//! share   = contact / (area of the band / BAND)                (of the pad's outline)
//! ```
//!
//! so a pad in a zone set to `solid`, a pad whose spokes are as wide as its sides, a pad on a
//! filled copper drawing and a pad entered by a track wider than itself all read as what they
//! are, and a pad of a `solid` zone that other nets leave joined at one end only reads as the
//! neck it is. [`joints`] gives the measure per pad and layer, [`check`] the DRC findings,
//! [`census`] the counts, [`relieve`] the changes that bring a board to relieved pads.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::zones::{self, OBSTACLE_TOL, Relief, ZoneParams};
use super::{CopperItem, ItemRef, PlacedPad, placed_pads};
use crate::diag::Diagnostic;
use crate::geom::poly::{self, Boolean, FillRule, Geometry, Join, Op, Polygon, PolygonSet, Rect};
use crate::geom::{Point, RTree};
use crate::model::Project;
use crate::model::board::{PadConnection, PadJoin, Rules, Zone};
use crate::model::footprint::{MaskOpening, PadKind, Paste};
use crate::refs::ObjectRef;
use crate::units::Nm;

/// Width of the band around a pad's outline in which its joints are measured (nm). A gap
/// narrower than this is no gap.
pub const BAND: i64 = 10_000;

/// A heat pad is at least this many times the median pad area of its footprint, in tenths.
const HEAT_RATIO_TENTHS: i128 = 25;
/// Largest plated hole of a one-pad footprint taken for a via (no component lead fits).
const VIA_LIKE_DRILL: Nm = Nm(500_000);

/// What a pad is for, as far as soldering heat goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PadClass {
    /// A plated through-hole pin: soldered by hand or by wave, on every layer it has copper.
    Through,
    /// A surface-mount solder pad.
    Smd,
    /// A pad that spreads heat on purpose: an exposed pad (a grid of paste windows, or one
    /// window on a pad far larger than the others), the tab of a power package, or a pad
    /// inside one with its number (thermal via pads). Solid is right.
    Heat,
    /// Not soldered: covered by solder mask, a pad without a number (mounting hole), or the
    /// pad of a one-pad footprint that is an SMD pad without paste (test point, fiducial) or
    /// has a hole no lead fits (a stitching via as a footprint).
    Bare,
}

impl PadClass {
    /// A pad that is soldered and wants its heat kept: through-hole pins and SMD pads.
    pub fn solder(self) -> bool {
        matches!(self, PadClass::Through | PadClass::Smd)
    }

    /// Lower-case name.
    pub fn name(self) -> &'static str {
        match self {
            PadClass::Through => "through",
            PadClass::Smd => "smd",
            PadClass::Heat => "heat",
            PadClass::Bare => "bare",
        }
    }
}

/// The class of every placed pad, by (designator, pad number); pads sharing a number take the
/// first's class unless one of them is a heat pad.
pub fn classes(pads: &[PlacedPad]) -> BTreeMap<(String, String), PadClass> {
    let mut by_fp: BTreeMap<&str, Vec<&PlacedPad>> = BTreeMap::new();
    for pp in pads {
        by_fp.entry(pp.refdes.as_str()).or_default().push(pp);
    }
    let mut out = BTreeMap::new();
    for (refdes, pads) in by_fp {
        // Unsigned: the pads of a footprint on the bottom side are mirrored, their outlines
        // run the other way.
        let area = |pp: &PlacedPad| poly::area2(&pp.shape).abs();
        let smd: Vec<&&PlacedPad> = pads.iter().filter(|pp| matches!(pp.pad.kind, PadKind::Smd)).collect();
        let mut areas: Vec<i128> = smd.iter().map(|pp| area(pp)).collect();
        areas.sort_unstable();
        let median = areas.get(areas.len() / 2).copied().unwrap_or(0);
        let numbers: BTreeSet<&str> = pads.iter().map(|pp| pp.number.as_str()).filter(|n| !n.is_empty()).collect();
        // Heat pads: far larger than the footprint's pads, and either with a grid of paste
        // windows (two or more), or with no other pad as large outside it (the lugs of a
        // connector come in several, whether they share a number or not; the pieces of a split
        // exposed pad lie inside it). One paste window alone says nothing: an imported pin
        // with a paste aperture of its own has one.
        let heat: Vec<&PlacedPad> = smd
            .iter()
            .filter(|pp| {
                let a = area(pp);
                let large = a * 10 >= median * HEAT_RATIO_TENTHS;
                if let Some(Paste::Windows { at, .. }) = &pp.pad.paste
                    && (at.len() >= 2 || (large && smd.len() >= 2))
                {
                    return true;
                }
                numbers.len() >= 3
                    && large
                    && !smd.iter().any(|o| {
                        !std::ptr::eq::<PlacedPad>(**o, ***pp)
                            && area(o) * 10 >= a * 8
                            && !poly::intersects(&o.shape, &pp.shape)
                    })
            })
            .map(|pp| **pp)
            .collect();
        let one_pad = numbers.len() <= 1 && pads.len() == 1;
        // Several drilled pads of one number and nothing else: a mounting hole with its ring of
        // vias (no part has several leads of one pin and no other pin).
        let hole_ring = numbers.len() == 1 && pads.len() > 1 && smd.is_empty();
        for pp in &pads {
            let covered = pp.pad.mask == MaskOpening::None;
            let class = if heat.iter().any(|h| {
                std::ptr::eq(*h, *pp)
                    || (h.number == pp.number && {
                        let c = poly::Point::from(pp.center);
                        poly::intersects(&h.shape, &rect(&Rect::new(c, c).expand(1)))
                    })
            }) {
                PadClass::Heat
            } else if covered || pp.number.is_empty() || hole_ring {
                PadClass::Bare
            } else {
                match pp.pad.kind {
                    PadKind::Npth { .. } => PadClass::Bare,
                    // Without paste a pad may still be soldered by hand; alone on its footprint
                    // it is a test point or a fiducial.
                    PadKind::Smd if one_pad && matches!(pp.pad.paste, Some(Paste::None)) => PadClass::Bare,
                    PadKind::Smd => PadClass::Smd,
                    PadKind::Tht { .. } if one_pad && pp.hole.is_some_and(|(d, _)| d <= VIA_LIKE_DRILL) => {
                        PadClass::Bare
                    }
                    PadKind::Tht { .. } => PadClass::Through,
                }
            };
            let key = (refdes.to_string(), pp.number.clone());
            let e = out.entry(key).or_insert(class);
            if class == PadClass::Heat {
                *e = class;
            }
        }
    }
    out
}

/// How a pad joins the copper of its net on one layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Joint {
    /// Designator.
    pub refdes: String,
    /// Pad number.
    pub number: String,
    /// Net.
    pub net: String,
    /// Copper layer.
    pub layer: String,
    /// Pad center.
    pub at: Point,
    /// What the pad is.
    pub class: PadClass,
    /// The zone of its net it lies in on this layer (the first in fill order whose outline
    /// meets the pad), if any.
    pub zone: Option<String>,
    /// How that zone connects the pad (pad, else footprint, else zone; `tht_thermal`
    /// resolved).
    pub style: Option<PadConnection>,
    /// The connection is the pad's own or its footprint's: said on purpose.
    pub own: bool,
    /// Spokes asked for (thermal style).
    pub planned: u8,
    /// Relief gap in effect (thermal style).
    pub gap: Nm,
    /// Spoke width in effect (thermal style).
    pub spoke: Nm,
    /// Separate pieces of zone copper reaching the pad.
    pub spokes: u8,
    /// The same with the net's tracks, vias and copper drawings.
    pub necks: u8,
    /// Outline length covered by zone copper.
    pub zone_contact: Nm,
    /// Outline length covered by any copper of the net (other pads left out).
    pub contact: Nm,
    /// Outline length.
    pub perimeter: Nm,
    /// Of the spokes asked for, those that the other pads of the pad's own footprint leave
    /// room for (their copper grown by the zone's clearance): a pin in a row has its ends only.
    pub possible: u8,
    /// The shorter side of the pad (of the rectangle with its area and outline length).
    pub short: Nm,
    /// Copper that is no pour and is wider than the pad touches it: a track wider than the
    /// pad's shorter side, or a copper drawing (teardrops and vias are neither).
    pub wide: bool,
    /// A pour is at the pad: zone copper within the relief's reach that goes on beyond it (a
    /// pad no spoke joins is cut off its pour, not merely far from it; the rim a fill leaves
    /// around a solid pad that other nets shut in is no pour).
    pub near: bool,
}

impl Joint {
    /// Whether the pad is in a pour of its net on this layer: the zone connects pads and a
    /// pour is at the pad ([`Joint::near`]).
    pub fn in_pour(&self) -> bool {
        self.zone.is_some() && self.style != Some(PadConnection::None) && self.near
    }

    /// Share of the outline joined to copper of the net, in percent.
    pub fn share(&self) -> u32 {
        if self.perimeter.0 <= 0 { 0 } else { ((self.contact.0 * 100) / self.perimeter.0).clamp(0, 100) as u32 }
    }

    /// Share of the outline joined to zone copper, in percent.
    pub fn zone_share(&self) -> u32 {
        if self.perimeter.0 <= 0 { 0 } else { ((self.zone_contact.0 * 100) / self.perimeter.0).clamp(0, 100) as u32 }
    }

    /// Whether the pad has no effective relief, from `max_share` percent of its outline in
    /// copper of its net (0: never): [`Solid::Pour`] when the pour alone covers that much,
    /// [`Solid::Copper`] when it takes the net's other copper and some of it is wider than the
    /// pad ([`Joint::wide`]). Tracks no wider than the pad are how a pad is connected, however
    /// much of a small pad's outline they take.
    pub fn solid(&self, max_share: u8) -> Option<Solid> {
        let max = u32::from(max_share);
        if max == 0 || self.contact.0 <= 0 {
            None
        } else if self.in_pour() && self.zone_share() >= max {
            Some(Solid::Pour)
        } else if self.wide && self.share() >= max {
            Some(Solid::Copper)
        } else {
            None
        }
    }

    /// Whether the pad is relieved by fewer necks than `min` asks for, or than its own
    /// footprint leaves room for when that is less (never fewer than one).
    pub fn starved(&self, min: u8, max_share: u8) -> bool {
        self.in_pour()
            && self.style == Some(PadConnection::Thermal)
            && self.solid(max_share).is_none()
            && self.necks < min.min(self.possible.max(1))
    }

    /// The pin as a subject.
    pub fn pin(&self) -> ObjectRef {
        ObjectRef::Pin { component: self.refdes.clone(), pin: self.number.clone() }
    }
}

/// Prefix of the names of teardrop zones (KiCad writes the teardrops of pads, vias and tracks
/// as zones named `$teardrop_padvia$` and `$teardrop_track$`): copper like a track's, not a
/// pour, and no zone whose pad connection is changed.
pub const TEARDROP_PREFIX: &str = "$teardrop_";

/// Whether a zone is a teardrop ([`TEARDROP_PREFIX`]).
pub fn is_teardrop(z: &Zone) -> bool {
    z.name.starts_with(TEARDROP_PREFIX)
}

/// Why a pad has no effective relief ([`Joint::solid`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Solid {
    /// The pour covers its outline.
    Pour,
    /// Wide tracks or copper drawings of its net cover it.
    Copper,
}

fn rect(b: &Rect) -> Polygon {
    let p = poly::Point::new;
    Polygon::new(vec![p(b.min.x, b.min.y), p(b.max.x, b.min.y), p(b.max.x, b.max.y), p(b.min.x, b.max.y)], vec![])
}

fn length(area2: i128) -> Nm {
    Nm((area2 / 2 / i128::from(BAND)) as i64)
}

/// The items of a net on a layer: its pads, its zone copper, its other copper (indices).
type NetItems = (Vec<usize>, Vec<usize>, Vec<usize>);

/// The joints of every pad with a net, per copper layer, in item order: pads in a pour of their
/// net, and pads that other copper of their net touches. `items` are the board's copper items
/// with the zone fills ([`super::copper_items`]).
pub fn joints(p: &Project, items: &[CopperItem]) -> Vec<Joint> {
    let board = p.board();
    let class = classes(&placed_pads(p));
    // Zones in fill order, with their settings.
    let mut order: Vec<usize> = (0..board.zones.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(board.zones[i].priority), i));
    let teardrops: BTreeSet<crate::id::ObjectId> =
        board.zones.iter().filter(|z| is_teardrop(z)).map(|z| z.id).collect();
    let zoned: Vec<(&Zone, ZoneParams, Polygon)> = order
        .iter()
        .filter(|&&i| !is_teardrop(&board.zones[i]))
        .map(|&i| {
            let z = &board.zones[i];
            let ring: Vec<poly::Point> = z.outline.iter().map(|&q| q.into()).collect();
            (z, zones::zone_params(p, z), Polygon::new(ring, vec![]))
        })
        .collect();
    let widths: BTreeMap<crate::id::ObjectId, Nm> = board.tracks.iter().map(|t| (t.id, t.width)).collect();
    let mut out = Vec::new();
    for layer in board.stackup.copper_names() {
        // Per net on this layer: its pads, its zone copper, its other copper.
        let mut nets: BTreeMap<&str, NetItems> = BTreeMap::new();
        // Every pad of the layer, whatever its net: what a footprint leaves room for.
        let mut layer_pads: Vec<usize> = Vec::new();
        for (i, it) in items.iter().enumerate() {
            if !it.layers.contains(&layer) {
                continue;
            }
            if let ItemRef::Pad(..) = it.item {
                layer_pads.push(i);
            }
            let Some(net) = it.net.as_deref() else { continue };
            let e = nets.entry(net).or_default();
            match &it.item {
                ItemRef::Pad(..) => e.0.push(i),
                ItemRef::Zone(id, ..) if !teardrops.contains(id) => e.1.push(i),
                _ => e.2.push(i),
            }
        }
        let pad_index = RTree::new(layer_pads.iter().map(|&i| items[i].shape.bbox()));
        for (net, (pads, fills, others)) in nets {
            if pads.is_empty() || (fills.is_empty() && others.is_empty()) {
                continue;
            }
            // Each pad's zone, relief and window.
            struct Job<'a> {
                it: &'a CopperItem,
                zone: Option<usize>,
                relief: Option<Relief>,
                bbox: Rect,
                window: Rect,
            }
            let jobs: Vec<Job<'_>> = pads
                .iter()
                .filter_map(|&i| {
                    let it = &items[i];
                    let bbox = it.shape.bbox()?;
                    let zone = zoned.iter().position(|(z, _, outline)| {
                        z.net.as_deref() == Some(net)
                            && z.layers.contains(&layer)
                            && outline.bbox().is_some_and(|b| b.intersects(&bbox))
                            && poly::intersects(outline, &it.shape)
                    });
                    let relief = zone.map(|k| Relief::of(it, &zoned[k].1));
                    let reach = relief.as_ref().map_or(0, |r| r.gap.max(0) + r.w.max(0));
                    Some(Job { it, zone, relief, bbox, window: bbox.expand(reach + 2 * BAND) })
                })
                .collect();
            // Zone copper near the pads only: one clip for the layer and net.
            let fill: PolygonSet = fills.iter().flat_map(|&i| items[i].shape.iter().cloned()).collect();
            let local: PolygonSet = if fill.is_empty() {
                vec![]
            } else {
                let windows: Vec<Polygon> = jobs.iter().map(|j| rect(&j.window)).collect();
                Boolean::new()
                    .subject(&fill, FillRule::NonZero)
                    .clip(&windows, FillRule::NonZero)
                    .op(Op::Intersection)
                    .execute()
                    .unwrap_or_default()
            };
            let local_index = RTree::new(local.iter().map(|q| q.bbox()));
            let other_index = RTree::new(others.iter().map(|&i| items[i].shape.bbox()));
            for j in jobs {
                let grown = j.bbox.expand(2 * BAND);
                let mut near_fill: Vec<u32> = local_index.query(&j.window);
                near_fill.sort_unstable();
                let mut near_other: Vec<u32> = other_index.query(&grown);
                near_other.sort_unstable();
                if near_fill.is_empty() && near_other.is_empty() {
                    continue;
                }
                let Ok(outer) = poly::offset(&j.it.shape, BAND, Join::Round, OBSTACLE_TOL) else { continue };
                let Ok(band) = Boolean::new()
                    .subject(&outer, FillRule::NonZero)
                    .clip(&j.it.shape, FillRule::NonZero)
                    .op(Op::Difference)
                    .execute()
                else {
                    continue;
                };
                let zone_copper: PolygonSet = near_fill.iter().map(|&k| local[k as usize].clone()).collect();
                let mut all = zone_copper.clone();
                all.extend(near_other.iter().flat_map(|&k| items[others[k as usize]].shape.iter().cloned()));
                let within = |copper: &PolygonSet| -> PolygonSet {
                    if copper.is_empty() {
                        return vec![];
                    }
                    Boolean::new()
                        .subject(copper, FillRule::NonZero)
                        .clip(&band, FillRule::NonZero)
                        .op(Op::Intersection)
                        .execute()
                        .unwrap_or_default()
                };
                let (zc, ac) = (within(&zone_copper), within(&all));
                // A pour: zone copper within the relief's reach that goes on beyond it. The rim
                // a fill leaves around a solid pad shut in by other nets is the pad, not a pour.
                let reach = rect(&j.bbox.expand(j.relief.as_ref().map_or(0, |r| r.gap.max(0) + r.w.max(0)) + BAND));
                let inside = j.window.expand(-1);
                let near = zone_copper
                    .iter()
                    .any(|q| q.bbox().is_some_and(|b| !inside.contains_rect(&b)) && poly::intersects(q, &reach));
                if ac.is_empty() && !near {
                    continue;
                }
                let ItemRef::Pad(refdes, number) = &j.it.item else { continue };
                let zone = j.zone.map(|k| &zoned[k]);
                let short = zones::short_side(&j.it.shape).max(1);
                let wide = near_other.iter().map(|&k| &items[others[k as usize]]).any(|o| {
                    let wider = match &o.item {
                        ItemRef::Track(id) => widths.get(id).is_some_and(|w| w.0 > short),
                        ItemRef::Via(..) | ItemRef::Zone(..) => false,
                        _ => true,
                    };
                    wider && poly::intersects(&o.shape, &band)
                });
                // Spokes the footprint's own pads of other nets leave room for.
                let possible = match (&j.relief, zone) {
                    (Some(r), Some((_, prm, _))) => {
                        let c = prm.clearance.0.max(0);
                        let mut blockers: Vec<u32> = pad_index.query(&j.window.expand(c));
                        blockers.sort_unstable();
                        let blockers: Vec<Polygon> = blockers
                            .into_iter()
                            .map(|k| &items[layer_pads[k as usize]])
                            .filter(|o| o.net != j.it.net && matches!(&o.item, ItemRef::Pad(r, _) if r == refdes))
                            .filter_map(|o| o.shape.bbox().map(|b| rect(&b.expand(c))))
                            .collect();
                        // The relief's other directions count too, up to the spokes asked for.
                        zones::spokes(j.it, r)
                            .iter()
                            .chain(&zones::alt_spokes(j.it, r))
                            .filter(|s| !blockers.iter().any(|b| poly::intersects(b, *s)))
                            .count()
                            .min(r.dirs.len())
                            .min(255) as u8
                    }
                    _ => 0,
                };
                let style = zone.map(|(_, prm, _)| zones::connection_on(j.it, &layer, prm.pads));
                out.push(Joint {
                    refdes: refdes.clone(),
                    number: number.clone(),
                    net: net.to_string(),
                    layer: layer.clone(),
                    at: j.it.anchor,
                    class: class.get(&(refdes.clone(), number.clone())).copied().unwrap_or(PadClass::Bare),
                    zone: zone.map(|(z, _, _)| z.name.clone()),
                    style,
                    own: zones::own_connection(j.it, &layer).is_some(),
                    planned: zone.map_or(0, |(_, prm, _)| prm.thermal_spokes),
                    gap: Nm(j.relief.as_ref().map_or(0, |r| r.gap)),
                    spoke: Nm(j.relief.as_ref().map_or(0, |r| r.w)),
                    spokes: zc.len().min(255) as u8,
                    necks: ac.len().min(255) as u8,
                    zone_contact: length(poly::area2(&zc)),
                    contact: length(poly::area2(&ac)),
                    perimeter: length(poly::area2(&band)),
                    possible,
                    short: Nm(short),
                    wide,
                    near,
                });
            }
        }
    }
    out
}

/// Counts of the pads in pours of their net, one per pad and layer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Census {
    /// Pads in a pour (pad and layer pairs).
    pub pads: usize,
    /// Relieved solder pads by the number of necks reaching them: 0, 1, 2, 3, 4 or more.
    pub relieved: [usize; 5],
    /// Relieved solder pads with fewer necks than the rules ask for and their footprint leaves
    /// room for (`drc.starved_thermal`, per pad and layer).
    pub starved: usize,
    /// Solder pads without effective relief: through-hole.
    pub solid_through: usize,
    /// Solder pads without effective relief: surface-mount.
    pub solid_smd: usize,
    /// Of those, pads whose own or footprint's setting says solid (on purpose).
    pub solid_own: usize,
    /// Heat pads (exposed pads, tabs) joined solidly.
    pub heat_solid: usize,
    /// Heat pads joined through spokes.
    pub heat_relieved: usize,
    /// Pads that are not soldered (covered, bare, mounting, via-like).
    pub bare: usize,
    /// Of the pads in a pour, those on inner layers.
    pub inner: usize,
    /// Solder pads without effective relief on inner layers (through-hole pins in planes).
    pub inner_solid: usize,
}

impl Census {
    /// Solder pads without relief that nothing marks as meant.
    pub fn solid(&self) -> usize {
        self.solid_through + self.solid_smd - self.solid_own
    }

    /// One line.
    pub fn text(&self) -> String {
        let r = self.relieved;
        format!(
            "{} pad(s) in pours: relieved by necks 0/1/2/3/4+ = {}/{}/{}/{}/{} ({} starved); solid through-hole {}, SMD {} \
             (of them {} set on the pad); heat pads solid {}, relieved {}; not soldered {}; on inner layers {} ({} solid)",
            self.pads,
            r[0],
            r[1],
            r[2],
            r[3],
            r[4],
            self.starved,
            self.solid_through,
            self.solid_smd,
            self.solid_own,
            self.heat_solid,
            self.heat_relieved,
            self.bare,
            self.inner,
            self.inner_solid
        )
    }
}

/// Counts the pads in pours ([`Joint::in_pour`]); a pad counts as solid from `max_share`
/// percent of its outline in copper.
pub fn census(p: &Project, joints: &[Joint], max_share: u8) -> Census {
    let min = p.board().rules.min_thermal_spokes;
    let copper = p.board().stackup.copper_names();
    let is_inner = |l: &str| copper.first().is_some_and(|f| f != l) && copper.last().is_some_and(|b| b != l);
    let mut c = Census::default();
    for j in joints.iter().filter(|j| j.in_pour()) {
        c.pads += 1;
        let inner = is_inner(&j.layer);
        c.inner += usize::from(inner);
        let solid = j.solid(max_share) == Some(Solid::Pour);
        match j.class {
            PadClass::Bare => c.bare += 1,
            PadClass::Heat if solid => c.heat_solid += 1,
            PadClass::Heat => c.heat_relieved += 1,
            PadClass::Through | PadClass::Smd if solid => {
                if j.class == PadClass::Through {
                    c.solid_through += 1;
                } else {
                    c.solid_smd += 1;
                }
                c.solid_own += usize::from(j.own && j.style == Some(PadConnection::Solid));
                c.inner_solid += usize::from(inner);
            }
            PadClass::Through | PadClass::Smd => {
                c.relieved[usize::from(j.necks.min(4))] += 1;
                c.starved += usize::from(j.starved(min, max_share));
            }
        }
    }
    c
}

/// Layers as a list for messages.
fn layer_list(js: &[&Joint]) -> String {
    js.iter().map(|j| j.layer.as_str()).collect::<Vec<_>>().join(", ")
}

fn zone_ref(name: &str) -> ObjectRef {
    ObjectRef::Named { kind: "zone".into(), name: name.to_string() }
}

/// The thermal relief rules on a board (`drc.solid_pad`, `drc.starved_thermal`,
/// `drc.thermal_nominal`, `drc.thermal_size`, `drc.current_spokes`): warnings, one per pad
/// (its layers together). `rules` gives the minimum spoke count, the largest share of a
/// solder pad's outline in copper, and the minimum track width and clearance that the spokes
/// and gaps are held to; `currents` adds the check of the spokes against the nets' currents.
pub fn check(p: &Project, rules: &Rules, items: &[CopperItem], currents: bool) -> Vec<Diagnostic> {
    let js = joints(p, items);
    let mut out = Vec::new();
    // Per pad, its joints on every layer.
    let mut by_pad: BTreeMap<(&str, &str), Vec<&Joint>> = BTreeMap::new();
    for j in &js {
        by_pad.entry((j.refdes.as_str(), j.number.as_str())).or_default().push(j);
    }
    // Two-pad surface-mount parts: a pad without relief beside one with it pulls the part up.
    let mut pads_of: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let class = classes(&placed_pads(p));
    for ((r, n), c) in &class {
        if *c != PadClass::Bare || !n.is_empty() {
            pads_of.entry(r.as_str()).or_default().insert(n.as_str());
        }
    }
    let max = rules.thermal_max_contact;
    let solid_pad = |r: &str, n: &str| by_pad.get(&(r, n)).is_some_and(|v| v.iter().any(|j| j.solid(max).is_some()));
    let min_gap = if rules.min_clearance > Nm::ZERO { rules.min_clearance } else { rules.clearance };
    // Sizes below the minimums: one finding per zone and value.
    let mut sizes: BTreeMap<(String, bool, Nm), Vec<&Joint>> = BTreeMap::new();
    for ((refdes, number), v) in &by_pad {
        let first = v[0];
        let pin = first.pin();
        if !first.class.solder() {
            continue;
        }
        // Without relief.
        let solid: Vec<&Joint> = v.iter().copied().filter(|j| j.solid(max).is_some()).collect();
        let meant = |j: &Joint| j.own && j.style == Some(PadConnection::Solid);
        let unmeant: Vec<&Joint> = solid.iter().copied().filter(|j| !meant(j)).collect();
        if let Some(j) = unmeant.first() {
            let poured: Vec<&Joint> = unmeant.iter().copied().filter(|j| j.solid(max) == Some(Solid::Pour)).collect();
            let other = pads_of.get(refdes).filter(|s| s.len() == 2 && first.class == PadClass::Smd).and_then(|s| {
                s.iter().find(|n| **n != *number).filter(|n| !solid_pad(refdes, n)).map(|n| format!("{refdes}.{n}"))
            });
            let tomb = other
                .map(|o| format!("; {o} is not, so the part can lift on that side in reflow (tombstoning)"))
                .unwrap_or_default();
            let what = if first.class == PadClass::Through { "through-hole pin" } else { "solder pad" };
            let mut d = if let Some(z) = poured.first().and_then(|j| j.zone.as_deref()) {
                let styled = poured.iter().any(|j| j.style == Some(PadConnection::Thermal));
                let (code, how) = if styled {
                    ("drc.thermal_nominal", "has thermal spokes but no effective relief in")
                } else {
                    ("drc.solid_pad", "is joined solidly to")
                };
                Diagnostic::warning(
                    code,
                    format!(
                        "{what} {refdes}.{number} (net {}) {how} zone:{z} on {}: copper on {} % of its outline \
                         (limit {max} %), it cannot be heated for soldering{tomb}",
                        j.net,
                        layer_list(&poured),
                        poured.iter().map(|j| j.share()).max().unwrap_or(0),
                    ),
                )
                .with_subject(pin.clone())
                .with_subject(zone_ref(z))
                .with_hint(if styled {
                    format!(
                        "narrow the spokes or widen the gap (`zone.set {z} --thermal-spoke .. --thermal-gap ..`, or \
                         `footprint.set` for this pad), or use fewer spokes (`--thermal-spokes 2`)"
                    )
                } else {
                    format!(
                        "`zone.relieve` gives every solder pad thermal spokes (`zone.set {z} --pads thermal` for \
                         this zone); if this pad is solid on purpose, say so on the pad or footprint \
                         (`footprint.set <footprint> --pads {number} --zone-connection solid`) or waive it \
                         (`drc.waive drc.solid_pad --subjects zone:{z} --reason ..`)"
                    )
                })
            } else {
                Diagnostic::warning(
                    "drc.solid_pad",
                    format!(
                        "{what} {refdes}.{number} (net {}) has copper of its net on {} % of its outline on {} \
                         (limit {max} %): tracks wider than the pad or copper drawings, it cannot be heated for \
                         soldering{tomb}",
                        j.net,
                        unmeant.iter().map(|j| j.share()).max().unwrap_or(0),
                        layer_list(&unmeant),
                    ),
                )
                .with_subject(pin.clone())
                .with_hint(
                    "enter the pad with one track no wider than the pad, or put the copper in a zone of the net \
                     (zone.add: its pads get thermal spokes); waive it if it is meant (drc.waive)",
                )
            };
            d = d.with_subject(ObjectRef::Net(j.net.clone())).at(j.at);
            out.push(d);
        }
        // Relieved, but by too few spokes.
        let starved: Vec<&Joint> = v.iter().copied().filter(|j| j.starved(rules.min_thermal_spokes, max)).collect();
        if let Some(j) = starved.first() {
            let z = j.zone.as_deref().unwrap_or_default();
            let per: Vec<String> = starved.iter().map(|j| format!("{} on {}", j.necks, j.layer)).collect();
            let cut = starved.iter().all(|j| j.necks == 0);
            out.push(
                Diagnostic::warning(
                    "drc.starved_thermal",
                    format!(
                        "{refdes}.{number} (net {}) is joined to zone:{z} by {} of {} thermal spoke(s) (minimum {}, \
                         its footprint leaves room for {}){}",
                        j.net,
                        per.join(", "),
                        j.planned,
                        rules.min_thermal_spokes,
                        starved.iter().map(|j| j.possible).max().unwrap_or(0),
                        if cut {
                            ": no spoke reaches the pour"
                        } else {
                            ": one spoke carries all its current and breaks easily"
                        },
                    ),
                )
                .with_subject(pin.clone())
                .with_subject(zone_ref(z))
                .with_subject(ObjectRef::Net(j.net.clone()))
                .at(j.at)
                .with_hint(format!(
                    "`zone.relieve` takes each such pad alone (a narrower relief, then a short track of the net into \
                     the pour) and says what it could not do; by hand: move the tracks and vias of other nets off the \
                     pad's sides, set the relief of this pad (`zone.pad {refdes}.{number} --thermal-spoke .. \
                     --thermal-gap ..`, `--thermal-angle 45deg`), or keep it solid on purpose (`zone.pad \
                     {refdes}.{number} --connection solid --reason ..`); `board.rules --min-thermal-spokes` sets the \
                     minimum"
                )),
            );
        }
        for j in v.iter().copied().filter(|j| j.in_pour() && j.style == Some(PadConnection::Thermal)) {
            let z = j.zone.clone().unwrap_or_default();
            if j.gap < min_gap {
                sizes.entry((z.clone(), true, j.gap)).or_default().push(j);
            }
            if j.spoke < rules.min_track_width {
                sizes.entry((z, false, j.spoke)).or_default().push(j);
            }
        }
    }
    for ((z, is_gap, value), v) in sizes {
        let pins: BTreeSet<String> = v.iter().map(|j| format!("{}.{}", j.refdes, j.number)).collect();
        let (what, min, opt) = if is_gap {
            ("gap", min_gap, "--thermal-gap")
        } else {
            ("spoke width", rules.min_track_width, "--thermal-spoke")
        };
        let shown: Vec<&str> = pins.iter().take(5).map(String::as_str).collect();
        let mut d = Diagnostic::warning(
            "drc.thermal_size",
            format!(
                "thermal relief {what} {value} in zone:{z} is below the minimum {min} ({} pad(s): {}{})",
                pins.len(),
                shown.join(", "),
                if pins.len() > shown.len() { ", ..." } else { "" }
            ),
        )
        .with_subject(zone_ref(&z))
        .at(v[0].at)
        .with_hint(format!(
            "`zone.set {z} {opt} {min}` or more (a pad or footprint value: `footprint.set .. {opt}`); the minimum is \
             the board's {}",
            if is_gap { "minimum clearance (min_clearance, else clearance)" } else { "min_track_width" }
        ));
        for pin in pins.iter().take(5) {
            if let Ok(r) = ObjectRef::parse(pin) {
                d = d.with_subject(r);
            }
        }
        out.push(d);
    }
    if currents {
        out.extend(current_spokes(p, &js, max));
    }
    out
}

/// Pads of nets with a `current` whose spokes and tracks together are narrower than IPC-2152
/// asks on their layer: one warning per net and layer (`drc.current_spokes`).
fn current_spokes(p: &Project, js: &[Joint], max: u8) -> Vec<Diagnostic> {
    use crate::electrical::{self, DEFAULT_TEMP_RISE, current};
    let stackup = &p.board().stackup;
    let mut by: BTreeMap<(&str, &str), Vec<&Joint>> = BTreeMap::new();
    for j in js {
        if j.in_pour() && j.style == Some(PadConnection::Thermal) && j.class != PadClass::Bare && j.solid(max).is_none()
        {
            by.entry((j.net.as_str(), j.layer.as_str())).or_default().push(j);
        }
    }
    let mut out = Vec::new();
    for ((net, layer), v) in by {
        let Some(n) = p.circuit().nets.get(net) else { continue };
        let Some(amps) = n.current else { continue };
        let rise = n.temp_rise.unwrap_or(DEFAULT_TEMP_RISE);
        if amps.to_f64() <= 0.0 || rise.to_f64() <= 0.0 {
            continue;
        }
        let need = electrical::required_width(stackup, layer, current::Method::Ipc2152, &amps, &rise);
        // A through-hole pin shares its current between its layers: its copper on all of them.
        let total = |j: &Joint| -> i64 {
            js.iter().filter(|o| o.refdes == j.refdes && o.number == j.number).map(|o| o.contact.0).sum()
        };
        let narrow: Vec<&&Joint> = v.iter().filter(|j| j.necks > 0 && total(j) < need.0).collect();
        if narrow.is_empty() {
            continue;
        }
        let least = narrow.iter().map(|j| total(j)).min().expect("non-empty");
        let shown: Vec<String> = narrow.iter().take(8).map(|j| format!("{}.{}", j.refdes, j.number)).collect();
        let mut d = Diagnostic::warning(
            "drc.current_spokes",
            format!(
                "net `{net}` carries {amps}: IPC-2152 asks for {need} on {layer}, the thermal spokes and tracks of \
                 {} pad(s) add up to less (down to {}): {}{}",
                narrow.len(),
                Nm(least),
                shown.join(", "),
                if narrow.len() > shown.len() { ", ..." } else { "" }
            ),
        )
        .with_subject(ObjectRef::Net(net.to_string()))
        .with_subject(ObjectRef::Layer(layer.to_string()))
        .at(narrow[0].at)
        .with_hint(format!(
            "widen the spokes of the pins that carry the current (`footprint.set .. --thermal-spoke`, or \
             `zone.set --thermal-spoke`; four spokes of {} carry it), or join those pins solidly on purpose \
             (`footprint.set .. --zone-connection solid`); pins that carry less can be waived (drc.waive)",
            Nm((need.0 + 3) / 4)
        ));
        for j in narrow.iter().take(8) {
            d = d.with_subject(j.pin());
        }
        out.push(d);
    }
    out
}

/// What [`relieve`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Relieved {
    /// Zones whose pad connection became `thermal`: name and what it was.
    pub zones: Vec<(String, String)>,
    /// Solid settings removed from solder pads and footprints: `footprint` or `footprint pad N`.
    pub removed: Vec<String>,
    /// Spoke widths set on footprint pads whose spokes at the zone's width left them without
    /// effective relief: `footprint pad N: width`.
    pub narrowed: Vec<String>,
    /// Solid settings kept (heat pads, pads that are not soldered) or given to heat pads so
    /// that they stay solid: `footprint pad N`.
    pub kept: Vec<String>,
    /// Placed pads given a narrower relief of their own (gap and spoke at the board's
    /// minimums) because it brings them more spokes: `J1.3: gap 0.15 mm, spoke 0.15 mm`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fitted: Vec<String>,
    /// Short tracks of the net added from a pad into its pour where no spoke fits:
    /// `J1.3 on In1.Cu: 0.2 mm wide, 0.65 mm long`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<String>,
    /// Placed pads kept solid, each alone and with the reason: neither a spoke nor a track
    /// fits where the pour joined them (`J1.3: ...`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub solid: Vec<String>,
    /// Pads left with fewer spokes than the minimum, with what was tried (`J1.3` to reason).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasons: BTreeMap<String, String>,
    /// Pads the pour joined on a layer before and that nothing joins to it now
    /// (`J1.3 on In1.Cu`): none unless a pad could neither be reached nor kept solid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<String>,
}

/// Brings the board to relieved solder pads: every zone that joins pads solidly (`solid`,
/// `tht_thermal`) becomes `thermal` (all zones, or those named); `solid` settings on solder pads
/// and footprints go, unless `keep_own`; heat pads (exposed pads, tabs) of the footprints on
/// the board get a `solid` setting of their own so that they stay solid, and pads that are not
/// soldered keep theirs. Zones that connect no pads (`none`) and the zones' relief sizes are
/// left alone. Then each pad still short of spokes is taken alone (`repair_pads`): a narrower
/// relief of its own, a short track into the pour, or, where the pour joined it before and
/// nothing else fits, solid for that one pad. The fills follow on their own (they are
/// derived). Also returns the joints of the board as it is left.
pub fn relieve(p: &mut Project, only: Option<&[String]>, keep_own: bool) -> (Relieved, Vec<Joint>) {
    let mut out = Relieved::default();
    // The solder pads a pour joins as the board stands: none of them may end without a joint.
    let had: BTreeSet<Key3> = joints(p, &super::copper_items(p))
        .iter()
        .filter(|j| j.zone_contact.0 > 0 && j.class.solder() && j.style != Some(PadConnection::None))
        .map(key3)
        .collect();
    let pads = placed_pads(p);
    let class = classes(&pads);
    // Footprints in use, and per footprint pad number its class (the first placement's).
    let mut fp_class: BTreeMap<(String, String), PadClass> = BTreeMap::new();
    let mut used: BTreeMap<String, String> = BTreeMap::new();
    for refdes in p.board().footprints.keys() {
        if let Some(fp) = super::footprint_for(p, refdes) {
            used.entry(fp.name.clone()).or_insert_with(|| refdes.clone());
        }
    }
    for (name, refdes) in &used {
        for ((r, n), c) in class.range((refdes.clone(), String::new())..) {
            if r != refdes {
                break;
            }
            fp_class.insert((name.clone(), n.clone()), *c);
        }
    }
    let touched =
        |z: &Zone| !is_teardrop(z) && only.is_none_or(|names| names.iter().any(|n| n.eq_ignore_ascii_case(&z.name)));
    for z in p.board_mut().zones.iter_mut().filter(|z| touched(z)) {
        let was = match z.pads {
            PadConnection::Solid => "solid",
            PadConnection::ThtThermal => "tht_thermal",
            PadConnection::Thermal | PadConnection::None => continue,
        };
        z.pads = PadConnection::Thermal;
        out.zones.push((z.name.clone(), was.to_string()));
    }
    let lib = p.library_mut();
    for (name, _) in used {
        let Some(fp) = lib.footprints.get_mut(&name) else { continue };
        let fp_solid = fp.overrides.zone_connection == Some(PadConnection::Solid);
        let solder = |n: &str| fp_class.get(&(name.clone(), n.to_string())).is_some_and(|c| c.solder());
        let any_solder = fp.pads.iter().any(|pad| solder(&pad.number));
        // A footprint's setting covers all its pads: it goes when solder pads are among them,
        // and the pads that are right solid get it as their own.
        let lifted = fp_solid && any_solder && !keep_own;
        if lifted {
            fp.overrides.zone_connection = None;
            out.removed.push(name.clone());
        }
        let fp_solid = fp_solid && !lifted;
        for pad in &mut fp.pads {
            let label = format!("{name} pad {}", pad.number);
            let own = pad.overrides.zone_connection;
            if solder(&pad.number) {
                if own == Some(PadConnection::Solid) && !keep_own {
                    pad.overrides.zone_connection = None;
                    out.removed.push(label);
                }
            } else if own == Some(PadConnection::Solid) || fp_solid {
                out.kept.push(label);
            } else if own.is_none()
                && (lifted || fp_class.get(&(name.clone(), pad.number.clone())) == Some(&PadClass::Heat))
            {
                pad.overrides.zone_connection = Some(PadConnection::Solid);
                out.kept.push(label);
            }
        }
    }
    out.removed.sort();
    out.removed.dedup();
    out.kept.sort();
    out.kept.dedup();
    // Spokes as wide as the pad's sides relieve nothing (a 0.5 mm spoke on an 0402 pad): the
    // pads that still read solid through their spokes get a width of their own, half their
    // shorter side, between the minimum track width and the width they had.
    let rules = p.board().rules.clone();
    let items = super::copper_items(p);
    let mut widths: BTreeMap<(String, String), Nm> = BTreeMap::new();
    for j in joints(p, &items) {
        if j.class.solder()
            && j.in_pour()
            && j.style == Some(PadConnection::Thermal)
            && j.solid(rules.thermal_max_contact) == Some(Solid::Pour)
            && let Some(fp) = super::footprint_for(p, &j.refdes)
        {
            let w = Nm(zones::fitted_spoke(j.short.0)).max(rules.min_track_width);
            if w < j.spoke {
                let e = widths.entry((fp.name.clone(), j.number.clone())).or_insert(w);
                *e = (*e).min(w);
            }
        }
    }
    let lib = p.library_mut();
    for ((name, number), w) in widths {
        let Some(fp) = lib.footprints.get_mut(&name) else { continue };
        for pad in fp.pads.iter_mut().filter(|pad| pad.number == number) {
            pad.overrides.thermal_spoke = Some(w);
        }
        out.narrowed.push(format!("{name} pad {number}: {w}"));
    }
    let js = repair_pads(p, only, &had, &mut out);
    (out, js)
}

/// A pad on a layer: designator, pad number, layer.
type Key3 = (String, String, String);

fn key3(j: &Joint) -> Key3 {
    (j.refdes.clone(), j.number.clone(), j.layer.clone())
}

/// Farthest a track added by [`relieve`] ends beyond the middle of its pad, on top of half the
/// pad's larger side (nm).
pub const TRACK_REACH: i64 = 2_500_000;
/// Step of the lengths tried for such a track (nm).
const TRACK_STEP: i64 = 50_000;
/// Directions tried for such a track: integer vectors (exact on every platform), 16 around.
const TRACK_DIRS: [(i64, i64); 16] = [
    (1, 0),
    (2, 1),
    (1, 1),
    (1, 2),
    (0, 1),
    (-1, 2),
    (-1, 1),
    (-2, 1),
    (-1, 0),
    (-2, -1),
    (-1, -1),
    (-1, -2),
    (0, -1),
    (1, -2),
    (1, -1),
    (2, -1),
];

/// A pad short of spokes on a layer, as [`repair_pads`] sees it.
#[derive(Clone, Debug)]
struct Short {
    layer: String,
    necks: u8,
    want: u8,
    gap: Nm,
    spoke: Nm,
}

/// The pads short of spokes, per pad its layers: relieved solder pads in a pour (or that a
/// pour joined before, `had`) with fewer necks than `min` asks and their footprint leaves room
/// for, and pads of `had` that nothing joins any more.
fn shorts(
    js: &[Joint],
    had: &BTreeSet<Key3>,
    scope: &dyn Fn(Option<&str>) -> bool,
    min: u8,
    max: u8,
) -> BTreeMap<(String, String), Vec<Short>> {
    let mut out: BTreeMap<(String, String), Vec<Short>> = BTreeMap::new();
    let mut seen: BTreeSet<Key3> = BTreeSet::new();
    for j in js {
        let k = key3(j);
        let was = had.contains(&k);
        seen.insert(k);
        let want = min.max(1).min(j.possible.max(1));
        if j.class.solder()
            && j.style == Some(PadConnection::Thermal)
            && scope(j.zone.as_deref())
            && (j.in_pour() || was)
            && j.solid(max).is_none()
            && j.necks < want
        {
            out.entry((j.refdes.clone(), j.number.clone())).or_default().push(Short {
                layer: j.layer.clone(),
                necks: j.necks,
                want,
                gap: j.gap,
                spoke: j.spoke,
            });
        }
    }
    for k in had.difference(&seen) {
        out.entry((k.0.clone(), k.1.clone())).or_default().push(Short {
            layer: k.2.clone(),
            necks: 0,
            want: 1,
            gap: Nm::ZERO,
            spoke: Nm::ZERO,
        });
    }
    out
}

/// Layers without any joint, then spokes missing in all: lower is better.
fn score(v: Option<&Vec<Short>>) -> (usize, usize) {
    v.map_or((0, 0), |v| {
        (v.iter().filter(|s| s.necks == 0).count(), v.iter().map(|s| usize::from(s.want - s.necks)).sum())
    })
}

/// A straight track from the middle of a pad: its end, its length and its direction.
struct Reach {
    end: poly::Point,
    len: i64,
    dir: (i64, i64),
}

/// The stroke of a track of width `w` from `c` along `d` over the length `t`.
fn stroke(c: poly::Point, d: (i64, i64), t: i64, w: i64) -> (Polygon, poly::Point) {
    let n = ((d.0 * d.0 + d.1 * d.1) as f64).sqrt();
    let (ux, uy) = (d.0 as f64 / n, d.1 as f64 / n);
    let h = w as f64 / 2.0;
    let at = |s: f64, o: f64| {
        poly::Point::new((c.x as f64 + ux * s - uy * o).round() as i64, (c.y as f64 + uy * s + ux * o).round() as i64)
    };
    let t = t as f64;
    (Polygon::new(vec![at(0.0, -h), at(t, -h), at(t, h), at(0.0, h)], vec![]), at(t, 0.0))
}

/// Tracks of width `w` that join a pad to its pour where the copper allows: straight from the
/// pad's middle, inside `allowed` (the pour as it would be with the pad solid, which keeps
/// every clearance of the net), ending `w` deep in `pour` (the pour as it is), and crossing the
/// pad's outline where no copper of `pour` does yet. Shortest first, up to `want`, each at a
/// right angle or more from the ones before.
fn reaches(it: &CopperItem, allowed: &PolygonSet, pour: &PolygonSet, w: i64, want: usize) -> Vec<Reach> {
    let Some(b) = it.shape.bbox() else { return vec![] };
    if want == 0 || w <= 0 || pour.is_empty() {
        return vec![];
    }
    let c: poly::Point = it.anchor.into();
    let Ok(allowed) = Boolean::new()
        .subject(allowed, FillRule::NonZero)
        .subject(&it.shape, FillRule::NonZero)
        .op(Op::Union)
        .execute()
    else {
        return vec![];
    };
    // The band of the pad's outline, widened by the track: a track must cross it clear of
    // the copper that is there, or it is no neck of its own.
    let band = |grow: i64| -> PolygonSet {
        let Ok(outer) = poly::offset(&it.shape, grow, Join::Round, OBSTACLE_TOL) else { return vec![] };
        Boolean::new()
            .subject(&outer, FillRule::NonZero)
            .clip(&it.shape, FillRule::NonZero)
            .op(Op::Difference)
            .execute()
            .unwrap_or_default()
    };
    let ring = band(2 * BAND);
    let Ok(taken) =
        Boolean::new().subject(pour, FillRule::NonZero).clip(&ring, FillRule::NonZero).op(Op::Intersection).execute()
    else {
        return vec![];
    };
    let steps = (b.width().max(b.height()) / 2 + TRACK_REACH) / TRACK_STEP;
    let mut found: Vec<Reach> = Vec::new();
    for d in TRACK_DIRS {
        let fits = |i: i64| poly::contains(&allowed, &stroke(c, d, i * TRACK_STEP, w).0);
        if !fits(1) {
            continue;
        }
        // The longest track that fits (a longer one holds every shorter one).
        let (mut lo, mut hi) = (1, steps);
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        for i in 1..=lo {
            let (s, end) = stroke(c, d, i * TRACK_STEP, w);
            if b.expand(BAND).contains_point(end) {
                continue;
            }
            let tip = rect(&Rect::new(end, end).expand(w / 2));
            if !poly::contains(pour, &tip) {
                continue;
            }
            // Clear of the necks the pad has: the track, grown by the band, in the ring.
            let wide = stroke(c, d, i * TRACK_STEP, w + 2 * BAND).0;
            let crossing = Boolean::new()
                .subject(std::slice::from_ref(&wide), FillRule::NonZero)
                .clip(&ring, FillRule::NonZero)
                .op(Op::Intersection)
                .execute()
                .unwrap_or_default();
            if !poly::intersects(&crossing, &taken) {
                found.push(Reach { end, len: i * TRACK_STEP, dir: d });
            }
            let _ = s;
            break;
        }
    }
    found.sort_by_key(|r| r.len);
    let mut out: Vec<Reach> = Vec::new();
    for r in found {
        if out.len() < want && out.iter().all(|o| o.dir.0 * r.dir.0 + o.dir.1 * r.dir.1 <= 0) {
            out.push(r);
        }
    }
    out
}

/// The second half of [`relieve`]: each pad still short of spokes, alone.
///
/// 1. A narrower relief of its own (gap at the minimum clearance, spokes at the minimum track
///    width), kept when it brings the pad more spokes.
/// 2. Tracks of the net from the pad into the pour, where the pour as it would be with the
///    pad solid has copper for one (so it keeps every clearance), kept when they are necks of
///    their own.
/// 3. A pad the pour joined before and that nothing joins now is kept solid, that pad alone,
///    with the reason.
///
/// Returns the joints of the board as it is left.
fn repair_pads(p: &mut Project, only: Option<&[String]>, had: &BTreeSet<Key3>, out: &mut Relieved) -> Vec<Joint> {
    let rules = p.board().rules.clone();
    let (min, max) = (rules.min_thermal_spokes, rules.thermal_max_contact);
    let scope =
        |z: Option<&str>| only.is_none_or(|names| z.is_none_or(|z| names.iter().any(|n| n.eq_ignore_ascii_case(z))));
    let label = |k: &(String, String)| format!("{}.{}", k.0, k.1);
    let measure = |p: &Project| {
        let items = super::copper_items(p);
        let js = joints(p, &items);
        (items, js)
    };
    let (_, js) = measure(p);
    let mut short = shorts(&js, had, &scope, min, max);
    if short.is_empty() {
        return js;
    }
    let mut js = js;
    let mut tried: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();

    // 1. A narrower relief.
    let min_gap = if rules.min_clearance > Nm::ZERO { rules.min_clearance } else { rules.clearance };
    let min_w = rules.min_track_width;
    let mut set: Vec<((String, String), Option<PadJoin>, PadJoin)> = Vec::new();
    for (k, v) in &short {
        let Some(pf) = p.board().footprints.get(&k.0) else { continue };
        let old = pf.pads.get(&k.1).cloned();
        let cur = old.clone().unwrap_or_default();
        if !cur.is_empty() {
            continue;
        }
        let gap = v.iter().map(|s| s.gap).max().unwrap_or(Nm::ZERO);
        let spoke = v.iter().map(|s| s.spoke).max().unwrap_or(Nm::ZERO);
        let unknown = v.iter().any(|s| s.spoke == Nm::ZERO);
        let new = PadJoin {
            thermal_gap: (min_gap > Nm::ZERO && (unknown || min_gap < gap)).then_some(min_gap),
            thermal_spoke: (min_w > Nm::ZERO && (unknown || min_w < spoke)).then_some(min_w),
            reason: Some("zone.relieve: the zone's relief left the pad short of spokes".into()),
            ..cur
        };
        if new.thermal_gap.is_none() && new.thermal_spoke.is_none() {
            continue;
        }
        set.push((k.clone(), old, new));
    }
    if !set.is_empty() {
        for (k, _, new) in &set {
            if let Some(pf) = p.board_mut().footprints.get_mut(&k.0) {
                pf.pads.insert(k.1.clone(), new.clone());
            }
        }
        let (_, js1) = measure(p);
        let after = shorts(&js1, had, &scope, min, max);
        let mut reverted = false;
        for (k, old, new) in set {
            if score(after.get(&k)) < score(short.get(&k)) {
                let size = |v: Option<Nm>, what: &str| v.map(|v| format!("{what} {v}"));
                let sizes: Vec<String> =
                    [size(new.thermal_gap, "gap"), size(new.thermal_spoke, "spoke")].into_iter().flatten().collect();
                out.fitted.push(format!("{}: {}", label(&k), sizes.join(", ")));
            } else {
                reverted = true;
                tried
                    .entry(k.clone())
                    .or_default()
                    .push(format!("a relief at the minimums (gap {min_gap}, spoke {min_w}) brings no further spoke"));
                if let Some(pf) = p.board_mut().footprints.get_mut(&k.0) {
                    match old {
                        Some(o) => pf.pads.insert(k.1.clone(), o),
                        None => pf.pads.remove(&k.1),
                    };
                }
            }
        }
        js = if reverted { measure(p).1 } else { js1 };
        short = shorts(&js, had, &scope, min, max);
    }

    // 2. Tracks into the pour.
    let mut ours: BTreeSet<Key3> = BTreeSet::new();
    if !short.is_empty() && min_w > Nm::ZERO {
        let items = super::copper_items(p);
        let teardrops: BTreeSet<crate::id::ObjectId> =
            p.board().zones.iter().filter(|z| is_teardrop(z)).map(|z| z.id).collect();
        // The pours as they would be with every pad solid: copper the net may have.
        let mut base = super::base_copper_items(p);
        for it in &mut base {
            if let ItemRef::Pad(..) = it.item {
                it.local.connection = Some(PadConnection::Solid);
            }
        }
        let solid = zones::fill_zones_uncached(p, &base);
        // Per net and layer, the pads to reach.
        // (pad item, pad, spokes missing)
        type Job<'a> = (&'a CopperItem, (String, String), u8);
        let mut jobs: BTreeMap<(String, String), Vec<Job<'_>>> = BTreeMap::new();
        for (k, v) in &short {
            for s in v {
                let Some(it) = items.iter().find(|it| {
                    matches!(&it.item, ItemRef::Pad(r, n) if *r == k.0 && *n == k.1) && it.layers.contains(&s.layer)
                }) else {
                    continue;
                };
                let Some(net) = it.net.clone() else { continue };
                jobs.entry((net, s.layer.clone())).or_default().push((it, k.clone(), s.want - s.necks));
            }
        }
        let mut add: Vec<(Key3, String, Reach, Point)> = Vec::new();
        for ((net, layer), pads) in &jobs {
            let windows: Vec<Polygon> = pads
                .iter()
                .filter_map(|(it, ..)| it.shape.bbox())
                .map(|b| rect(&b.expand(b.width().max(b.height()) / 2 + TRACK_REACH + min_w.0)))
                .collect();
            let local = |set: PolygonSet| -> PolygonSet {
                if set.is_empty() {
                    return set;
                }
                Boolean::new()
                    .subject(&set, FillRule::NonZero)
                    .clip(&windows, FillRule::NonZero)
                    .op(Op::Intersection)
                    .execute()
                    .unwrap_or_default()
            };
            let pour = local(
                items
                    .iter()
                    .filter(|it| {
                        it.net.as_deref() == Some(net.as_str())
                            && it.layers.contains(layer)
                            && matches!(&it.item, ItemRef::Zone(id, ..) if !teardrops.contains(id))
                    })
                    .flat_map(|it| it.shape.iter().cloned())
                    .collect(),
            );
            let allowed = local(
                solid
                    .iter()
                    .filter(|f| {
                        f.net.as_deref() == Some(net.as_str()) && f.layer == *layer && !teardrops.contains(&f.zone)
                    })
                    .flat_map(|f| f.fill.iter().cloned())
                    .collect(),
            );
            let pour_index = RTree::new(pour.iter().map(|q| q.bbox()));
            let allowed_index = RTree::new(allowed.iter().map(|q| q.bbox()));
            for (it, k, need) in pads {
                let Some(b) = it.shape.bbox() else { continue };
                let w = b.expand(b.width().max(b.height()) / 2 + TRACK_REACH + min_w.0);
                let pick = |set: &PolygonSet, index: &RTree| -> PolygonSet {
                    let mut near = index.query(&w);
                    near.sort_unstable();
                    near.into_iter().map(|i| set[i as usize].clone()).collect()
                };
                let found = reaches(
                    it,
                    &pick(&allowed, &allowed_index),
                    &pick(&pour, &pour_index),
                    min_w.0,
                    usize::from(*need),
                );
                if found.is_empty() {
                    tried.entry(k.clone()).or_default().push(format!(
                        "no track of the net {min_w} wide reaches the pour on {layer} within {} of the pad",
                        Nm(TRACK_REACH)
                    ));
                }
                for r in found {
                    add.push(((k.0.clone(), k.1.clone(), layer.clone()), net.clone(), r, it.anchor));
                }
            }
        }
        if !add.is_empty() {
            let necks = |js: &[Joint]| -> BTreeMap<Key3, u8> { js.iter().map(|j| (key3(j), j.necks)).collect() };
            let before = necks(&js);
            let mut ids: Vec<(Key3, crate::id::ObjectId, String)> = Vec::new();
            for (k, net, r, from) in add {
                let id = p.alloc_id();
                p.board_mut().tracks.push(crate::model::board::Track {
                    id,
                    layer: k.2.clone(),
                    width: min_w,
                    net: Some(net),
                    start: from,
                    end: r.end.into(),
                    mid: None,
                    locked: false,
                });
                ids.push((k.clone(), id, format!("{}.{} on {}: {min_w} wide, {} long", k.0, k.1, k.2, Nm(r.len))));
            }
            let (_, js1) = measure(p);
            let after = necks(&js1);
            // A track that is no neck of its own goes again.
            let mut drop: BTreeSet<crate::id::ObjectId> = BTreeSet::new();
            for (k, id, text) in &ids {
                if after.get(k).copied().unwrap_or(0) > before.get(k).copied().unwrap_or(0) {
                    ours.insert(k.clone());
                    out.tracks.push(text.clone());
                } else {
                    drop.insert(*id);
                    tried
                        .entry((k.0.clone(), k.1.clone()))
                        .or_default()
                        .push(format!("a track into the pour on {} joins the copper that is there already", k.2));
                }
            }
            js = if drop.is_empty() {
                js1
            } else {
                p.board_mut().tracks.retain(|t| !drop.contains(&t.id));
                measure(p).1
            };
        }
    }

    // 3. Solid, for the pads the pour joined and that nothing joins now.
    let lost = |js: &[Joint]| -> BTreeSet<Key3> {
        let joined: BTreeSet<Key3> =
            js.iter().filter(|j| j.zone_contact.0 > 0 || ours.contains(&key3(j))).map(key3).collect();
        had.difference(&joined).cloned().collect()
    };
    let mut cut = lost(&js);
    if !cut.is_empty() {
        let mut by_pad: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for k in &cut {
            by_pad.entry((k.0.clone(), k.1.clone())).or_default().push(k.2.clone());
        }
        for (k, layers) in by_pad {
            let reason = format!(
                "zone.relieve: kept solid, no thermal spoke and no track of the net fits on {} where the pour joined the pad",
                layers.join(", ")
            );
            let Some(pf) = p.board_mut().footprints.get_mut(&k.0) else { continue };
            let join = pf.pads.entry(k.1.clone()).or_default();
            for l in &layers {
                join.layers.insert(l.clone(), PadConnection::Solid);
            }
            join.reason = Some(reason.clone());
            out.solid.push(format!("{}: {}", label(&k), reason.trim_start_matches("zone.relieve: ")));
        }
        js = measure(p).1;
        cut = lost(&js);
    }
    out.cut = cut.iter().map(|(r, n, l)| format!("{r}.{n} on {l}")).collect();
    out.fitted.sort();
    out.tracks.sort();
    for (k, v) in shorts(&js, had, &scope, min, max) {
        let state: Vec<String> = v.iter().map(|s| format!("{} of {} on {}", s.necks, s.want, s.layer)).collect();
        let mut why = tried.remove(&k).unwrap_or_default();
        if why.is_empty() {
            why.push("its relief is set on the pad or its placement, left as it is".into());
        }
        out.reasons.insert(label(&k), format!("{}: {}", state.join(", "), why.join("; ")));
    }
    js
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::board::BoardSide;
    use crate::model::footprint::{Pad, PadShape};

    fn placed(refdes: &str, number: &str, at: (i64, i64), size: (i64, i64), kind: PadKind) -> PlacedPad {
        let c = Point::new(Nm(at.0), Nm(at.1));
        let (hw, hh) = (size.0 / 2, size.1 / 2);
        let b = Rect::new(poly::Point::new(at.0 - hw, at.1 - hh), poly::Point::new(at.0 + hw, at.1 + hh));
        let hole = match kind {
            PadKind::Tht { drill } => Some((drill, true)),
            PadKind::Npth { drill, .. } => Some((drill, false)),
            PadKind::Smd => None,
        };
        let pad = Pad::new(number, c, PadShape::Rect { w: Nm(size.0), h: Nm(size.1) }, kind);
        PlacedPad {
            refdes: refdes.into(),
            number: number.into(),
            net: Some("GND".into()),
            center: c,
            hole_at: c,
            shape: rect(&b),
            layers: vec!["F.Cu".into()],
            layer_shapes: vec![],
            hole,
            pad,
            side: BoardSide::Top,
            pad_side: BoardSide::Top,
            overrides: Default::default(),
            slot: None,
            tie: None,
        }
    }

    fn class_of(pads: &[PlacedPad], refdes: &str, number: &str) -> PadClass {
        classes(pads)[&(refdes.to_string(), number.to_string())]
    }

    /// What each pad is for: pins and SMD pads are soldered; an exposed pad (paste windows),
    /// a tab far larger than the leads and the via pads inside it spread heat; mounting holes,
    /// covered pads, test points and via-like one-pad footprints are not soldered. A connector's
    /// two equal lugs are solder pads, not heat pads.
    #[test]
    fn pad_classes() {
        let smd = PadKind::Smd;
        let mm = 1_000_000;
        let mut pads = vec![
            placed("U1", "1", (-2 * mm, 0), (mm / 2, mm), smd),
            placed("U1", "2", (-2 * mm, 2 * mm), (mm / 2, mm), smd),
            placed("U1", "3", (-2 * mm, 4 * mm), (mm / 2, mm), smd),
            placed("U1", "4", (3 * mm, 2 * mm), (4 * mm, 4 * mm), smd),
            // A thermal via pad of the tab's number inside it.
            placed("U1", "4", (3 * mm, 2 * mm), (mm / 2, mm / 2), PadKind::Tht { drill: Nm(300_000) }),
            placed("J1", "1", (10 * mm, 0), (1_700_000, 1_700_000), PadKind::Tht { drill: Nm(mm) }),
            placed("J1", "2", (10 * mm, 3 * mm), (1_700_000, 1_700_000), PadKind::Tht { drill: Nm(mm) }),
            placed("J1", "", (10 * mm, 6 * mm), (3 * mm, 3 * mm), PadKind::Npth { drill: Nm(3 * mm), copper: true }),
            placed("J2", "1", (20 * mm, 0), (mm / 3, mm), smd),
            placed("J2", "2", (20 * mm, mm), (mm / 3, mm), smd),
            placed("J2", "3", (20 * mm, 2 * mm), (mm / 3, mm), smd),
            placed("J2", "S1", (18 * mm, mm), (2 * mm, 2 * mm), smd),
            placed("J2", "S2", (22 * mm, mm), (2 * mm, 2 * mm), smd),
            placed("J3", "1", (50 * mm, 0), (mm / 3, mm), smd),
            placed("J3", "2", (50 * mm, mm), (mm / 3, mm), smd),
            placed("J3", "3", (50 * mm, 2 * mm), (mm / 3, mm), smd),
            placed("J3", "MP", (48 * mm, mm), (2 * mm, 2 * mm), smd),
            placed("J3", "MP", (52 * mm, mm), (2 * mm, 2 * mm), smd),
            placed("V1", "1", (30 * mm, 0), (600_000, 600_000), PadKind::Tht { drill: Nm(300_000) }),
            placed("TP1", "1", (32 * mm, 0), (mm, mm), smd),
            placed("H1", "1", (34 * mm, 0), (6 * mm, 6 * mm), PadKind::Tht { drill: Nm(3 * mm) }),
            placed("C1", "1", (40 * mm, 0), (mm / 2, mm / 2), smd),
            placed("C1", "2", (41 * mm, 0), (mm / 2, mm / 2), smd),
        ];
        let at = |r: &str, n: &str| pads.iter().position(|pp| pp.refdes == r && pp.number == n).unwrap();
        let (tp, c2) = (at("TP1", "1"), at("C1", "2"));
        pads[tp].pad.paste = Some(Paste::None);
        pads[c2].pad.mask = MaskOpening::None;
        // A footprint on the bottom side is mirrored: its outlines run clockwise, and its pads
        // are what they are all the same.
        for pp in pads.iter_mut().filter(|pp| pp.refdes == "J2") {
            pp.shape.outer.0.reverse();
        }
        assert_eq!(class_of(&pads, "U1", "1"), PadClass::Smd);
        assert_eq!(class_of(&pads, "U1", "4"), PadClass::Heat);
        assert_eq!(class_of(&pads, "J1", "1"), PadClass::Through);
        assert_eq!(class_of(&pads, "J1", ""), PadClass::Bare);
        assert_eq!(class_of(&pads, "J2", "S1"), PadClass::Smd);
        assert_eq!(class_of(&pads, "J3", "MP"), PadClass::Smd, "two lugs of one number");
        assert_eq!(class_of(&pads, "V1", "1"), PadClass::Bare);
        assert_eq!(class_of(&pads, "TP1", "1"), PadClass::Bare);
        assert_eq!(class_of(&pads, "H1", "1"), PadClass::Through, "a 3 mm plated hole takes a lead or a screw");
        assert_eq!(class_of(&pads, "C1", "1"), PadClass::Smd);
        assert_eq!(class_of(&pads, "C1", "2"), PadClass::Bare, "covered by mask");
        // A mounting hole with a ring of vias: several drilled pads of one number.
        let ring = vec![
            placed("H2", "1", (0, 0), (6 * mm, 6 * mm), PadKind::Tht { drill: Nm(3 * mm) }),
            placed("H2", "1", (2 * mm, 0), (800_000, 800_000), PadKind::Tht { drill: Nm(400_000) }),
            placed("H2", "1", (-2 * mm, 0), (800_000, 800_000), PadKind::Tht { drill: Nm(400_000) }),
        ];
        assert_eq!(class_of(&ring, "H2", "1"), PadClass::Bare);
        // A grid of paste windows says exposed pad, whatever the size; one window on a pin
        // (an imported pad with a paste aperture of its own) does not.
        let w = |n: usize| Some(Paste::Windows { size: (Nm(1), Nm(1)), at: vec![Point::default(); n] });
        pads[0].pad.paste = w(1);
        assert_eq!(class_of(&pads, "U1", "1"), PadClass::Smd);
        pads[0].pad.paste = w(4);
        assert_eq!(class_of(&pads, "U1", "1"), PadClass::Heat);
    }

    fn joint(class: PadClass, style: PadConnection, necks: u8, zone_contact: i64, contact: i64) -> Joint {
        Joint {
            refdes: "U1".into(),
            number: "1".into(),
            net: "GND".into(),
            layer: "F.Cu".into(),
            at: Point::default(),
            class,
            zone: Some("Z".into()),
            style: Some(style),
            own: false,
            planned: 4,
            gap: Nm(200_000),
            spoke: Nm(250_000),
            spokes: necks,
            necks,
            zone_contact: Nm(zone_contact),
            contact: Nm(contact),
            perimeter: Nm(4_000_000),
            possible: 4,
            short: Nm(1_000_000),
            wide: false,
            near: true,
        }
    }

    /// Solid is read from the copper: the pour on most of the outline, or wide copper of the
    /// net; tracks no wider than the pad never make a pad solid. Starved: fewer necks than
    /// the minimum, or than the footprint leaves room for.
    #[test]
    fn solid_and_starved() {
        let t = PadConnection::Thermal;
        let relieved = joint(PadClass::Smd, t, 4, 1_000_000, 1_000_000);
        assert_eq!((relieved.solid(67), relieved.starved(2, 67)), (None, false));
        let full = joint(PadClass::Smd, PadConnection::Solid, 1, 4_000_000, 4_000_000);
        assert_eq!(full.solid(67), Some(Solid::Pour));
        assert_eq!(full.solid(0), None, "0 turns the check off");
        let mut tracks = joint(PadClass::Smd, t, 3, 500_000, 3_000_000);
        assert_eq!(tracks.solid(67), None, "tracks no wider than the pad");
        tracks.wide = true;
        assert_eq!(tracks.solid(67), Some(Solid::Copper));
        let one = joint(PadClass::Smd, t, 1, 250_000, 250_000);
        assert!(one.starved(2, 67) && !one.starved(1, 67) && !one.starved(0, 67));
        let mut row = one.clone();
        row.possible = 1;
        assert!(!row.starved(2, 67), "its own footprint leaves room for one spoke");
        let cut = joint(PadClass::Smd, t, 0, 0, 0);
        assert!(cut.starved(2, 67));
        let mut row0 = cut.clone();
        row0.possible = 0;
        assert!(row0.starved(2, 67), "never fewer than one");
    }
}
