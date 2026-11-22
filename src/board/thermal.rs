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
use crate::model::footprint::{MaskOpening, PadKind, PadPurpose, Paste};
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
            // What the pad says it is solid for goes before what it looks like: a press-fit
            // pin and a stitching pad are not soldered, a heat pad spreads heat; a terminal is
            // soldered (and solid on purpose, which the rules read from its `purpose`).
            let said = match pp.overrides.purpose {
                Some(PadPurpose::PressFit | PadPurpose::Stitching) => Some(PadClass::Bare),
                Some(PadPurpose::Heat) => Some(PadClass::Heat),
                Some(PadPurpose::Terminal) | None => None,
            };
            let class = if let Some(c) = said {
                c
            } else if heat.iter().any(|h| {
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
    /// That own connection is `solid`, whether a zone holds the pad on this layer or not: a
    /// pad joined solidly by tracks or copper drawings says "meant" the same way (D120).
    pub own_solid: bool,
    /// What the pad says it is solid for (`PadPurpose`), if anything.
    pub purpose: Option<PadPurpose>,
    /// The placed pad's own setting has a reason written with it (`PadJoin::reason`).
    pub reasoned: bool,
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

    /// The necks of the pad counted by their copper: the separate pieces that reach it, or,
    /// when that is more, the spokes that the copper on its outline is worth (its length over
    /// the relief's spoke width). Spokes as wide as a small pad's sides meet at its corners
    /// and reach it as one piece of copper on two or three of its sides: that is no pad on a
    /// single spoke (D119).
    pub fn worth(&self) -> u8 {
        let by_width = if self.spoke.0 > 0 { self.contact.0 / self.spoke.0 } else { 0 };
        self.necks.max(by_width.clamp(0, 255) as u8)
    }

    /// Whether the pad is relieved by fewer necks than `min` asks for, or than its own
    /// footprint leaves room for when that is less (never fewer than one); necks by their
    /// copper ([`Joint::worth`]).
    pub fn starved(&self, min: u8, max_share: u8) -> bool {
        self.in_pour()
            && self.style == Some(PadConnection::Thermal)
            && self.solid(max_share).is_none()
            && self.worth() < min.min(self.possible.max(1))
    }

    /// Whether the pad is solid on purpose: it says what for (`purpose`), or its connection
    /// is set solid on the pad, its placement or its footprint (in a zone: the connection the
    /// zone gives it; outside one, where tracks or drawings make it solid: its own setting).
    /// With `required` (the rules' `thermal_required`) a setting alone is not enough: the pad
    /// says what it is solid for, or the placed pad's own setting has its reason.
    pub fn meant(&self, required: bool) -> bool {
        let set = self.own && (self.style == Some(PadConnection::Solid) || (self.style.is_none() && self.own_solid));
        self.purpose.is_some() || (set && (!required || self.reasoned))
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
    joints_of(p, items, &|_| true)
}

/// The joints of the pads of the nets that `wanted` names, as [`joints`] has them: for a
/// caller that looks at the pads of a few nets many times, as a router does with the nets
/// that have pours.
pub fn joints_of(p: &Project, items: &[CopperItem], wanted: &dyn Fn(&str) -> bool) -> Vec<Joint> {
    joints_with(p, items, wanted, true)
}

/// Everything the joint of one pad on one layer is read from ([`pad_joint`]).
#[derive(Clone, PartialEq, Eq)]
struct JointKey {
    layer: String,
    /// The pad.
    pad: CopperItem,
    /// Its zone: name and settings.
    zone: Option<(String, ZoneParams)>,
    /// Its relief there.
    relief: Option<Relief>,
    /// The zone copper of its net near it (the net's fills on the layer, cut to the windows
    /// of the net's pads).
    fill: PolygonSet,
    /// The other copper of its net near it: shape, and what [`Joint::wide`] asks of it (a
    /// track's width; `None` with `true` for a copper drawing, with `false` for a via or a
    /// teardrop).
    others: Vec<(PolygonSet, Option<Nm>, bool)>,
    /// The pads of other nets of its own footprint near it, grown by the zone's clearance.
    blockers: Vec<Rect>,
    class: PadClass,
    reasoned: bool,
}

impl JointKey {
    fn hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.layer.hash(&mut h);
        (self.pad.anchor.x.0, self.pad.anchor.y.0).hash(&mut h);
        self.pad.shape.hash(&mut h);
        self.fill.hash(&mut h);
        self.others.len().hash(&mut h);
        h.finish()
    }

    fn points(&self) -> usize {
        let of = |s: &PolygonSet| s.iter().map(Polygon::vertex_count).sum::<usize>();
        of(&self.pad.shape) + of(&self.fill) + self.others.iter().map(|(s, ..)| of(s)).sum::<usize>() + 16
    }
}

/// The joints read in this process by what they were read from, in two ages, as the reliefs
/// of a fill are kept (`zones::SpokeMemo`): the pads of a board that differs from one read
/// before in a few places are read again near those places alone.
#[derive(Default)]
struct JointMemo {
    now: BTreeMap<u64, Vec<(JointKey, Option<Joint>)>>,
    before: BTreeMap<u64, Vec<(JointKey, Option<Joint>)>>,
    points: usize,
}

/// The most points the newer age of [`JointMemo`] holds.
const JOINT_POINTS: usize = 2_000_000;

impl JointMemo {
    fn get(&mut self, hash: u64, key: &JointKey) -> Option<Option<Joint>> {
        if let Some((_, v)) = self.now.get(&hash).and_then(|b| b.iter().find(|(k, _)| k == key)) {
            return Some(v.clone());
        }
        let bucket = self.before.get_mut(&hash)?;
        let (k, v) = bucket.swap_remove(bucket.iter().position(|(k, _)| k == key)?);
        self.put(hash, k, v.clone());
        Some(v)
    }

    fn put(&mut self, hash: u64, key: JointKey, made: Option<Joint>) {
        if self.points > JOINT_POINTS {
            self.before = std::mem::take(&mut self.now);
            self.points = 0;
        }
        self.points += key.points();
        self.now.entry(hash).or_default().push((key, made));
    }
}

static JOINTS: std::sync::Mutex<Option<JointMemo>> = std::sync::Mutex::new(None);

fn joint_memo<T>(f: impl FnOnce(&mut JointMemo) -> Option<T>) -> Option<T> {
    f(JOINTS.lock().ok()?.get_or_insert_with(JointMemo::default))
}

/// Forgets the joints kept in process by [`joints`] (`zones::clear_fill_cache` calls it).
pub fn clear_joint_cache() {
    if let Ok(mut m) = JOINTS.lock() {
        *m = None;
    }
}

/// The joint of one pad on one layer: a function of `key` alone. `None` when no copper of
/// its net is at the pad.
fn pad_joint(key: &JointKey) -> Option<Joint> {
    let it = &key.pad;
    let ItemRef::Pad(refdes, number) = &it.item else { return None };
    let bbox = it.shape.bbox()?;
    let reach = key.relief.as_ref().map_or(0, |r| r.gap.max(0) + r.w.max(0));
    let window = bbox.expand(reach + 2 * BAND);
    let outer = poly::offset(&it.shape, BAND, Join::Round, OBSTACLE_TOL).ok()?;
    let band = Boolean::new()
        .subject(&outer, FillRule::NonZero)
        .clip(&it.shape, FillRule::NonZero)
        .op(Op::Difference)
        .execute()
        .ok()?;
    let zone_copper = &key.fill;
    let mut all = zone_copper.clone();
    all.extend(key.others.iter().flat_map(|(s, ..)| s.iter().cloned()));
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
    let (zc, ac) = (within(zone_copper), within(&all));
    // A pour: zone copper within the relief's reach that goes on beyond it. The rim a fill
    // leaves around a solid pad shut in by other nets is the pad, not a pour.
    let reach = rect(&bbox.expand(reach + BAND));
    let inside = window.expand(-1);
    let near =
        zone_copper.iter().any(|q| q.bbox().is_some_and(|b| !inside.contains_rect(&b)) && poly::intersects(q, &reach));
    if ac.is_empty() && !near {
        return None;
    }
    let short = zones::short_side(&it.shape).max(1);
    let wide = key
        .others
        .iter()
        .any(|(shape, width, always)| (width.map_or(*always, |w| w.0 > short)) && poly::intersects(shape, &band));
    // Spokes the footprint's own pads of other nets leave room for; the relief's other
    // directions count too, up to the spokes asked for.
    let possible = match (&key.relief, &key.zone) {
        (Some(r), Some(_)) => zones::spokes(it, r)
            .iter()
            .chain(&zones::alt_spokes(it, r))
            .filter(|s| !key.blockers.iter().any(|b| poly::intersects(&rect(b), *s)))
            .count()
            .min(r.dirs.len())
            .min(255) as u8,
        _ => 0,
    };
    Some(Joint {
        refdes: refdes.clone(),
        number: number.clone(),
        net: it.net.clone().unwrap_or_default(),
        layer: key.layer.clone(),
        at: it.anchor,
        class: key.class,
        zone: key.zone.as_ref().map(|(name, _)| name.clone()),
        style: key.zone.as_ref().map(|(_, prm)| zones::connection_on(it, &key.layer, prm.pads)),
        own: zones::own_connection(it, &key.layer).is_some(),
        own_solid: zones::own_connection(it, &key.layer) == Some(PadConnection::Solid),
        purpose: it.local.purpose,
        reasoned: key.reasoned,
        planned: key.zone.as_ref().map_or(0, |(_, prm)| prm.thermal_spokes),
        gap: Nm(key.relief.as_ref().map_or(0, |r| r.gap)),
        spoke: Nm(key.relief.as_ref().map_or(0, |r| r.w)),
        spokes: zc.len().min(255) as u8,
        necks: ac.len().min(255) as u8,
        zone_contact: length(poly::area2(&zc)),
        contact: length(poly::area2(&ac)),
        perimeter: length(poly::area2(&band)),
        possible,
        short: Nm(short),
        wide,
        near,
    })
}

/// [`joints_of`]; with `reuse`, the pads whose every input is as it was when they were last
/// read in this process are taken from then ([`JointMemo`]), which gives the same joints.
fn joints_with(p: &Project, items: &[CopperItem], wanted: &dyn Fn(&str) -> bool, reuse: bool) -> Vec<Joint> {
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
            if pads.is_empty() || (fills.is_empty() && others.is_empty()) || !wanted(net) {
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
            let mut keys: Vec<JointKey> = Vec::new();
            for j in jobs {
                let grown = j.bbox.expand(2 * BAND);
                let mut near_fill: Vec<u32> = local_index.query(&j.window);
                near_fill.sort_unstable();
                let mut near_other: Vec<u32> = other_index.query(&grown);
                near_other.sort_unstable();
                if near_fill.is_empty() && near_other.is_empty() {
                    continue;
                }
                let ItemRef::Pad(refdes, number) = &j.it.item else { continue };
                let zone = j.zone.map(|k| &zoned[k]);
                let blockers: Vec<Rect> = match (&j.relief, zone) {
                    (Some(_), Some((_, prm, _))) => {
                        let c = prm.clearance.0.max(0);
                        let mut blockers: Vec<u32> = pad_index.query(&j.window.expand(c));
                        blockers.sort_unstable();
                        blockers
                            .into_iter()
                            .map(|k| &items[layer_pads[k as usize]])
                            .filter(|o| o.net != j.it.net && matches!(&o.item, ItemRef::Pad(r, _) if r == refdes))
                            .filter_map(|o| o.shape.bbox().map(|b| b.expand(c)))
                            .collect()
                    }
                    _ => vec![],
                };
                keys.push(JointKey {
                    layer: layer.clone(),
                    pad: j.it.clone(),
                    zone: zone.map(|(z, prm, _)| (z.name.clone(), *prm)),
                    relief: j.relief,
                    fill: near_fill.iter().map(|&k| local[k as usize].clone()).collect(),
                    others: near_other
                        .iter()
                        .map(|&k| &items[others[k as usize]])
                        .map(|o| match &o.item {
                            ItemRef::Track(id) => (o.shape.clone(), widths.get(id).copied(), false),
                            ItemRef::Via(..) | ItemRef::Zone(..) => (o.shape.clone(), None, false),
                            _ => (o.shape.clone(), None, true),
                        })
                        .collect(),
                    blockers,
                    class: class.get(&(refdes.clone(), number.clone())).copied().unwrap_or(PadClass::Bare),
                    reasoned: board
                        .footprints
                        .get(refdes)
                        .and_then(|f| f.pads.get(number))
                        .is_some_and(|j| j.reason.as_ref().is_some_and(|r| !r.trim().is_empty())),
                });
            }
            let read = |key: &JointKey| -> Option<Joint> {
                if !reuse {
                    return pad_joint(key);
                }
                let hash = key.hash();
                if let Some(found) = joint_memo(|m| m.get(hash, key)) {
                    return found;
                }
                let made = pad_joint(key);
                joint_memo(|m| {
                    m.put(hash, key.clone(), made.clone());
                    Some(())
                });
                made
            };
            #[cfg(feature = "parallel")]
            let made: Vec<Option<Joint>> = {
                use rayon::prelude::*;
                keys.par_iter().map(read).collect()
            };
            #[cfg(not(feature = "parallel"))]
            let made: Vec<Option<Joint>> = keys.iter().map(read).collect();
            out.extend(made.into_iter().flatten());
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
    // What stands where the missing spokes of the pads short of them would run.
    let starved_keys: BTreeSet<Key3> =
        js.iter().filter(|j| j.class.solder() && j.starved(rules.min_thermal_spokes, max)).map(key3).collect();
    let in_the_way = blockers(p, items, &starved_keys);
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
        let meant = |j: &Joint| j.meant(rules.thermal_required);
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
                         this zone); if this pad is solid on purpose, say what for: `zone.pad {refdes}.{number} \
                         --purpose press_fit|terminal|heat|stitching` for this pad, `footprint.set <footprint> --pads \
                         {number} --purpose ..` for every part with that footprint{}",
                        if rules.thermal_required {
                            " (thermal relief is required by the rules: a solid setting without a purpose or a \
                             reason is not enough, `zone.pad .. --connection solid --reason ..` gives one)"
                                .to_string()
                        } else {
                            format!(
                                ", or set it solid (`--zone-connection solid`), or waive it (`drc.waive drc.solid_pad \
                                 --subjects zone:{z} --reason ..`)"
                            )
                        }
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
                .with_hint(format!(
                    "enter the pad with one track no wider than the pad, or put the copper in a zone of the net \
                     (zone.add: its pads get thermal spokes); {}",
                    if rules.thermal_required {
                        // No waiver takes the fab check's error away: the pad says it.
                        format!(
                            "if it is meant, say what for: `zone.pad {refdes}.{number} --purpose \
                             press_fit|terminal|heat|stitching`, or `zone.pad {refdes}.{number} --connection solid \
                             --reason ..`"
                        )
                    } else {
                        "waive it if it is meant (drc.waive)".to_string()
                    }
                ))
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
            let blocked: BTreeSet<&Blocker> =
                starved.iter().filter_map(|j| in_the_way.get(&key3(j))).flatten().collect();
            let way = if blocked.is_empty() {
                String::new()
            } else {
                format!("; in the way of the others: {}", Blocker::list(blocked.iter().copied()))
            };
            // Whose it is to make room, by what is in the way.
            let has = |class: &str| blocked.iter().any(|b| b.class() == class);
            let mut first = String::new();
            if has("route") {
                let routed: Vec<String> = blocked.iter().filter(|b| b.class() == "route").map(|b| b.text()).collect();
                let nets: BTreeSet<&str> = blocked
                    .iter()
                    .filter_map(|b| match b {
                        Blocker::Track { net, .. } | Blocker::Via { net, .. } => net.as_deref(),
                        _ => None,
                    })
                    .collect();
                first += &format!(
                    "the tracks and vias of other nets beside the pad are the router's to move ({}): `route.rip {}` \
                     and route again (`route.nets`), the routers keep the room of a pad's spokes; ",
                    routed.join(", "),
                    nets.iter().copied().collect::<Vec<_>>().join(" ")
                );
            }
            if has("place") {
                first += "a pad of another net stands where a spoke would run: move the part away (`place.move`) or \
                          accept the pad as it is (a waiver, `drc.waive drc.starved_thermal --subjects <pad>`); ";
            }
            if has("zone") || has("board") {
                first += "the pour ends beside the pad (the zone's outline, the board edge, a keep-out, another \
                          net's pour, or copper narrower than the zone's minimum width): widen the zone there \
                          (`zone.set`, its `min_width`) or move the part; ";
            }
            let mut d = Diagnostic::warning(
                "drc.starved_thermal",
                format!(
                    "{refdes}.{number} (net {}) is joined to zone:{z} by {} of {} thermal spoke(s) (minimum {}, \
                     its footprint leaves room for {}){}{way}",
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
                "{first}`zone.relieve` takes each such pad alone (a narrower relief, then a short track of the net \
                 into the pour) and says what it could not do; by hand: set the relief of this pad (`zone.pad \
                 {refdes}.{number} --thermal-spoke .. --thermal-gap ..`, `--thermal-angle 45deg`), or keep it solid \
                 on purpose (`zone.pad {refdes}.{number} --purpose terminal`, or `--connection solid --reason ..`); \
                 `board.rules --min-thermal-spokes` sets the minimum"
            ));
            // The things in the way that can be named, the router's first.
            let mut named: Vec<&Blocker> = blocked.iter().copied().collect();
            named.sort_by_key(|b| (b.class() != "route", (*b).clone()));
            for s in named.iter().filter_map(|b| b.subject()).take(6) {
                d = d.with_subject(s);
            }
            out.push(d);
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
    /// For the pads left short of spokes, per pad and layer (`J1.3 on F.Cu`): what stands
    /// where the missing spokes would run ([`blockers`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub blocked: BTreeMap<String, Vec<Blocker>>,
    /// Pads the pour joined on a layer before and that nothing joins to it now
    /// (`J1.3 on In1.Cu`): none unless a pad could neither be reached nor kept solid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<String>,
}

/// How [`relieve`] fills and reads the board after each of its changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Measure {
    /// With what earlier fills and readings of this process left where everything they were
    /// made from is as it was (`zones::fill_zones`, [`joints`]).
    Kept,
    /// Every fill and every pad from scratch: the reference.
    Scratch,
}

/// The joints of the board as it stands.
fn measure(p: &Project, how: Measure) -> Vec<Joint> {
    match how {
        Measure::Kept => joints(p, &super::copper_items(p)),
        Measure::Scratch => {
            let mut items = super::base_copper_items(p);
            let fills = zones::fill_zones_uncached(p, &items);
            items.extend(zones::fills_as_items(fills));
            joints_with(p, &items, &|_| true, false)
        }
    }
}

/// Brings the board to relieved solder pads: every zone that joins pads solidly (`solid`,
/// `tht_thermal`) becomes `thermal` (all zones, or those named); `solid` settings on solder pads
/// and footprints go, unless `keep_own` or the pad says what it is solid for
/// (`Overrides::purpose`); heat pads (exposed pads, tabs) of the footprints on the board get a
/// `solid` setting of their own so that they stay solid, and pads that are not soldered keep
/// theirs. Zones that connect no pads (`none`) and the zones' relief sizes are left alone.
/// Then each pad still short of spokes is taken alone (`repair`): a narrower relief of its
/// own, a short track into the pour, or, where the pour joined it before and nothing else
/// fits, solid for that one pad. The fills follow on their own (they are derived). Also
/// returns the joints of the board as it is left.
///
/// The board is filled and its pads read after each change, up to eight times; each time the
/// zone layers, the reliefs and the pads whose every input is as it was are taken from the
/// time before (`zones::fill_zones`, [`joints`]), so a change costs what it touches.
/// [`relieve_uncached`] makes every fill and reading from scratch and gives the same.
pub fn relieve(p: &mut Project, only: Option<&[String]>, keep_own: bool) -> (Relieved, Vec<Joint>) {
    run(p, only, keep_own, None, Measure::Kept)
}

/// [`relieve`], given the joints of the board as it stands when the caller has them
/// ([`joints`] of its copper items).
pub fn relieve_from(
    p: &mut Project,
    only: Option<&[String]>,
    keep_own: bool,
    before: Option<&[Joint]>,
) -> (Relieved, Vec<Joint>) {
    run(p, only, keep_own, before, Measure::Kept)
}

/// [`relieve`] with every fill and every reading of the pads made from scratch: the reference
/// that [`relieve`] is held to (`tests/zone_relieve.rs`), several times slower.
pub fn relieve_uncached(p: &mut Project, only: Option<&[String]>, keep_own: bool) -> (Relieved, Vec<Joint>) {
    run(p, only, keep_own, None, Measure::Scratch)
}

fn run(
    p: &mut Project,
    only: Option<&[String]>,
    keep_own: bool,
    before: Option<&[Joint]>,
    how: Measure,
) -> (Relieved, Vec<Joint>) {
    let mut out = Relieved::default();
    // The solder pads a pour joins as the board stands: none of them may end without a joint.
    let owned;
    let before = match before {
        Some(b) => b,
        None => {
            owned = measure(p, how);
            &owned
        }
    };
    let had: BTreeSet<Key3> = before
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
        // A pad that says what it is solid for is not a solder pad left solid by oversight.
        let fp_meant = fp.overrides.purpose.is_some();
        let solder = |n: &str| fp_class.get(&(name.clone(), n.to_string())).is_some_and(|c| c.solder());
        let any_solder = fp.pads.iter().any(|pad| solder(&pad.number) && pad.overrides.purpose.is_none());
        // A footprint's setting covers all its pads: it goes when solder pads are among them,
        // and the pads that are right solid get it as their own.
        let lifted = fp_solid && any_solder && !keep_own && !fp_meant;
        if lifted {
            fp.overrides.zone_connection = None;
            out.removed.push(name.clone());
        }
        let fp_solid = fp_solid && !lifted;
        for pad in &mut fp.pads {
            let label = format!("{name} pad {}", pad.number);
            let own = pad.overrides.zone_connection;
            let meant = pad.overrides.purpose.is_some() || fp_meant;
            if solder(&pad.number) && !meant {
                if own == Some(PadConnection::Solid) && !keep_own {
                    pad.overrides.zone_connection = None;
                    out.removed.push(label);
                }
            } else if own == Some(PadConnection::Solid) || fp_solid {
                out.kept.push(label);
            } else if own.is_none()
                && (lifted || meant || fp_class.get(&(name.clone(), pad.number.clone())) == Some(&PadClass::Heat))
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
    let js = measure(p, how);
    let mut widths: BTreeMap<(String, String), Nm> = BTreeMap::new();
    for j in &js {
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
    for ((name, number), w) in &widths {
        let Some(fp) = lib.footprints.get_mut(name) else { continue };
        for pad in fp.pads.iter_mut().filter(|pad| pad.number == *number) {
            pad.overrides.thermal_spoke = Some(*w);
        }
        out.narrowed.push(format!("{name} pad {number}: {w}"));
    }
    let js = if widths.is_empty() { js } else { measure(p, how) };
    let js = repair(p, only, &had, &mut out, js, how);
    (out, js)
}

/// A pad on a layer: designator, pad number, layer.
pub type Key3 = (String, String, String);

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

/// A pad short of spokes on a layer, as `repair` sees it.
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
/// for, and pads of `had` that nothing joins any more. Necks as separate pieces of copper,
/// which is what the repair works on, or `by_copper` as the rule counts them
/// ([`Joint::worth`]), which is what it reports as left.
fn shorts(
    js: &[Joint],
    had: &BTreeSet<Key3>,
    scope: &dyn Fn(Option<&str>) -> bool,
    min: u8,
    max: u8,
    by_copper: bool,
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
            && (if by_copper { j.worth() } else { j.necks }) < want
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

/// Most segments of a track that [`detour`] leads around what is in a pad's way.
const DETOUR_SEGMENTS: usize = 5;

/// A track of width `w` from the middle of a pad to its pour that is not straight: a way
/// through the copper the net may have (`allowed`, the pour as it would be with the pad
/// solid) around what stands between the pad and the pour as it is (`pour`), found on a grid
/// of [`TRACK_STEP`] and pulled straight where the copper allows, in at most
/// [`DETOUR_SEGMENTS`] segments and [`TRACK_REACH`] beyond the pad's half length. It leaves
/// the pad clear of the spokes it has, keeps a spoke's width from them on its way through the
/// relief gap (`gap`), and ends `w` deep in the pour beyond that gap: a second way for the
/// heat and the current, not more copper on the spoke that is there. The points of the track,
/// from the pad's middle; `None` when there is no such way.
fn detour(it: &CopperItem, allowed: &PolygonSet, pour: &PolygonSet, w: i64, gap: i64) -> Option<Vec<poly::Point>> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    let b = it.shape.bbox()?;
    if w <= 0 || pour.is_empty() {
        return None;
    }
    let c: poly::Point = it.anchor.into();
    let union = |a: &PolygonSet, b: &PolygonSet| {
        Boolean::new().subject(a, FillRule::NonZero).subject(b, FillRule::NonZero).op(Op::Union).execute()
    };
    let minus = |a: &PolygonSet, b: &PolygonSet| {
        Boolean::new().subject(a, FillRule::NonZero).clip(b, FillRule::NonZero).op(Op::Difference).execute()
    };
    let grow = |a: &PolygonSet, by: i64| poly::offset(a, by, Join::Round, OBSTACLE_TOL);
    let allowed = union(allowed, &it.shape).ok()?;
    // Where the middle of the track may be: half its width inside the allowed copper.
    let room = poly::offset(&allowed, -(w / 2 + 1_000), Join::Round, zones::FILL_TOL).ok()?;
    // The spokes the pad has: the pour inside its relief gap. The track keeps its own width
    // of air from them outside the pad.
    let ring = minus(&grow(&it.shape, gap.max(0) + 2 * BAND).ok()?, &it.shape).ok()?;
    let spokes = Boolean::new()
        .subject(pour, FillRule::NonZero)
        .clip(&ring, FillRule::NonZero)
        .op(Op::Intersection)
        .execute()
        .ok()?;
    let (keep_off, near_spokes) = if spokes.is_empty() {
        (vec![], vec![])
    } else {
        (minus(&grow(&spokes, w / 2 + w).ok()?, &it.shape).ok()?, minus(&grow(&spokes, w / 2).ok()?, &it.shape).ok()?)
    };
    let free = if keep_off.is_empty() { room } else { minus(&room, &keep_off).ok()? };
    // Where it may end: half its width inside the pour beyond the relief gap.
    let beyond = minus(pour, &grow(&it.shape, gap.max(0) + 2 * BAND).ok()?).ok()?;
    let ends = poly::offset(&beyond, -(w / 2 + 1_000), Join::Round, zones::FILL_TOL).ok()?;
    if free.is_empty() || ends.is_empty() {
        return None;
    }
    let (free_at, ends_at) = (poly::Prepared::new(&free), poly::Prepared::new(&ends));
    let inside = |set: &poly::Prepared<'_, PolygonSet>, q: poly::Point| set.locate(q) == poly::Location::Inside;
    // The grid, around the pad's middle; a step along an axis costs 10, a diagonal one 14.
    let reach = b.width().max(b.height()) / 2 + TRACK_REACH;
    let n = (reach / TRACK_STEP) as i32;
    let at = |(i, j): (i32, i32)| poly::Point::new(c.x + i64::from(i) * TRACK_STEP, c.y + i64::from(j) * TRACK_STEP);
    let limit = (reach / TRACK_STEP) as u32 * 10;
    let mut open: BTreeMap<(i32, i32), bool> = BTreeMap::new();
    let mut dist: BTreeMap<(i32, i32), u32> = BTreeMap::new();
    let mut prev: BTreeMap<(i32, i32), (i32, i32)> = BTreeMap::new();
    let mut heap: BinaryHeap<Reverse<(u32, i32, i32)>> = BinaryHeap::new();
    dist.insert((0, 0), 0);
    heap.push(Reverse((0, 0, 0)));
    let mut goal = None;
    while let Some(Reverse((d, i, j))) = heap.pop() {
        if dist.get(&(i, j)).is_some_and(|best| *best < d) {
            continue;
        }
        if inside(&ends_at, at((i, j))) {
            goal = Some((i, j));
            break;
        }
        for (di, dj, cost) in
            [(1, 0, 10), (0, 1, 10), (-1, 0, 10), (0, -1, 10), (1, 1, 14), (-1, 1, 14), (-1, -1, 14), (1, -1, 14)]
        {
            let k = (i + di, j + dj);
            let nd = d + cost;
            if k.0.abs() > n || k.1.abs() > n || nd > limit || dist.get(&k).is_some_and(|best| *best <= nd) {
                continue;
            }
            if !*open.entry(k).or_insert_with(|| inside(&free_at, at(k))) {
                continue;
            }
            dist.insert(k, nd);
            prev.insert(k, (i, j));
            heap.push(Reverse((nd, k.0, k.1)));
        }
    }
    let mut cells = vec![goal?];
    while let Some(&p) = prev.get(cells.last().expect("non-empty")) {
        cells.push(p);
    }
    cells.reverse();
    let pts: Vec<poly::Point> = cells.into_iter().map(at).collect();
    // Pulled straight: from each point, the farthest later one that a straight track reaches
    // inside the allowed copper and clear of the spokes.
    let straight = |a: poly::Point, z: poly::Point| {
        let d = (z.x - a.x, z.y - a.y);
        let len = ((d.0 * d.0 + d.1 * d.1) as f64).sqrt().round() as i64;
        if len == 0 {
            return true;
        }
        let s = stroke(a, d, len, w).0;
        poly::contains(&allowed, &s) && !poly::intersects(&near_spokes, &s)
    };
    let mut out = vec![pts[0]];
    let mut i = 0;
    while i + 1 < pts.len() {
        let j = (i + 1..pts.len()).rev().find(|&j| straight(pts[i], pts[j]))?;
        out.push(pts[j]);
        i = j;
        if out.len() > DETOUR_SEGMENTS + 1 {
            return None;
        }
    }
    (out.len() >= 2).then_some(out)
}

/// The second half of [`relieve`]: each pad still short of spokes, alone.
///
/// 1. A narrower relief of its own (gap at the minimum clearance, spokes at the minimum track
///    width), kept when it brings the pad more spokes.
/// 2. Tracks of the net from the pad into the pour, where the pour as it would be with the
///    pad solid has copper for one (so it keeps every clearance), kept when they are necks of
///    their own: straight from the pad's middle, else around what is in the way ([`detour`]).
/// 3. A pad the pour joined before and that nothing joins now is kept solid, that pad alone,
///    with the reason; so is a pad in a pour that nothing joined before either, where solid
///    joins it, and where it does not (other nets shut it in) the setting is taken back.
///
/// `js` are the joints of the board as it stands. Returns those of the board as it is left.
fn repair(
    p: &mut Project,
    only: Option<&[String]>,
    had: &BTreeSet<Key3>,
    out: &mut Relieved,
    mut js: Vec<Joint>,
    how: Measure,
) -> Vec<Joint> {
    let rules = p.board().rules.clone();
    let (min, max) = (rules.min_thermal_spokes, rules.thermal_max_contact);
    let scope =
        |z: Option<&str>| only.is_none_or(|names| z.is_none_or(|z| names.iter().any(|n| n.eq_ignore_ascii_case(z))));
    let label = |k: &(String, String)| format!("{}.{}", k.0, k.1);
    let mut short = shorts(&js, had, &scope, min, max, false);
    if short.is_empty() {
        return js;
    }
    let mut tried: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut ours: BTreeSet<Key3> = BTreeSet::new();

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
        let js1 = measure(p, how);
        let after = shorts(&js1, had, &scope, min, max, false);
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
        js = if reverted { measure(p, how) } else { js1 };
        short = shorts(&js, had, &scope, min, max, false);
    }

    // 2. Tracks into the pour.
    if !short.is_empty() && min_w > Nm::ZERO {
        let teardrops: BTreeSet<crate::id::ObjectId> =
            p.board().zones.iter().filter(|z| is_teardrop(z)).map(|z| z.id).collect();
        // The pours as they would be with every pad solid: copper the net may have.
        let base = super::base_copper_items(p);
        let mut base_solid = base.clone();
        for it in &mut base_solid {
            if let ItemRef::Pad(..) = it.item {
                it.local.connection = Some(PadConnection::Solid);
            }
        }
        let class_w = |net: &str| {
            let circuit = p.circuit();
            circuit
                .nets
                .get(net)
                .and_then(|n| n.class.as_ref())
                .and_then(|k| circuit.netclasses.get(k))
                .and_then(|c| c.track_width)
                .unwrap_or(min_w)
        };
        // The window a pad's tracks are looked for in.
        let window = |it: &CopperItem, net: &str| {
            it.shape.bbox().map(|b| b.expand(b.width().max(b.height()) / 2 + TRACK_REACH + class_w(net).max(min_w).0))
        };
        // Per net and layer, the pads to reach.
        // (pad item, pad, spokes missing, relief gap)
        type Job<'a> = (&'a CopperItem, (String, String), u8, Nm);
        let mut jobs: BTreeMap<(String, String), Vec<Job<'_>>> = BTreeMap::new();
        for (k, v) in &short {
            for s in v {
                let Some(it) = base.iter().find(|it| {
                    matches!(&it.item, ItemRef::Pad(r, n) if *r == k.0 && *n == k.1) && it.layers.contains(&s.layer)
                }) else {
                    continue;
                };
                let Some(net) = it.net.clone() else { continue };
                jobs.entry((net, s.layer.clone())).or_default().push((it, k.clone(), s.want - s.necks, s.gap));
            }
        }
        // The pours as they are, and as they would be with every pad solid.
        let (pours, solid): (Vec<CopperItem>, Vec<zones::ZoneFill>) = match how {
            Measure::Kept => (zones::zone_items(p, &base), zones::fill_zones_once(p, &base_solid)),
            Measure::Scratch => (
                zones::fills_as_items(zones::fill_zones_uncached(p, &base)),
                zones::fill_zones_uncached(p, &base_solid),
            ),
        };
        // (pad and layer, net, the track's points from the pad's middle, width, the length of
        // a straight one)
        type Added = (Key3, String, Vec<poly::Point>, Nm, Option<i64>);
        let mut add: Vec<Added> = Vec::new();
        for ((net, layer), pads) in &jobs {
            let class_w = class_w(net);
            let windows: Vec<Polygon> = pads.iter().filter_map(|(it, ..)| window(it, net)).map(|b| rect(&b)).collect();
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
                pours
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
            for (it, k, need, gap) in pads {
                let Some(w) = window(it, net) else { continue };
                let pick = |set: &PolygonSet, index: &RTree| -> PolygonSet {
                    let mut near = index.query(&w);
                    near.sort_unstable();
                    near.into_iter().map(|i| set[i as usize].clone()).collect()
                };
                // At the width the net's class asks of its tracks when one that wide fits (and
                // is no wider than the pad), else at the minimum track width.
                let (allowed, pour) = (pick(&allowed, &allowed_index), pick(&pour, &pour_index));
                let mut width = min_w;
                let mut found = Vec::new();
                if class_w > min_w && class_w.0 <= zones::short_side(&it.shape) {
                    found = reaches(it, &allowed, &pour, class_w.0, usize::from(*need));
                    width = class_w;
                }
                if found.is_empty() {
                    found = reaches(it, &allowed, &pour, min_w.0, usize::from(*need));
                    width = min_w;
                }
                let key = (k.0.clone(), k.1.clone(), layer.clone());
                if found.is_empty() {
                    // No straight one: around what is in the way.
                    match detour(it, &allowed, &pour, min_w.0, gap.0) {
                        Some(path) => add.push((key, net.clone(), path, min_w, None)),
                        None => tried.entry(k.clone()).or_default().push(format!(
                            "no track of the net {min_w} wide reaches the pour on {layer} within {} of the pad, \
                             straight or around what is in the way",
                            Nm(TRACK_REACH)
                        )),
                    }
                    continue;
                }
                for r in found {
                    add.push((key.clone(), net.clone(), vec![it.anchor.into(), r.end], width, Some(r.len)));
                }
            }
        }
        if !add.is_empty() {
            let necks = |js: &[Joint]| -> BTreeMap<Key3, u8> { js.iter().map(|j| (key3(j), j.necks)).collect() };
            let before = necks(&js);
            let mut ids: Vec<(Key3, Vec<crate::id::ObjectId>, String)> = Vec::new();
            for (k, net, path, width, straight) in add {
                let mut made = Vec::new();
                let mut len = 0i64;
                for seg in path.windows(2) {
                    let id = p.alloc_id();
                    let (dx, dy) = ((seg[1].x - seg[0].x) as f64, (seg[1].y - seg[0].y) as f64);
                    len += dx.hypot(dy).round() as i64;
                    p.board_mut().tracks.push(crate::model::board::Track {
                        id,
                        layer: k.2.clone(),
                        width,
                        net: Some(net.clone()),
                        start: seg[0].into(),
                        end: seg[1].into(),
                        mid: None,
                        locked: false,
                    });
                    made.push(id);
                }
                let len = straight.unwrap_or(len);
                let bends = if made.len() > 1 { format!(" in {} segments", made.len()) } else { String::new() };
                ids.push((
                    k.clone(),
                    made,
                    format!("{}.{} on {}: {width} wide, {} long{bends}", k.0, k.1, k.2, Nm(len)),
                ));
            }
            let js1 = measure(p, how);
            let after = necks(&js1);
            // A track that is no neck of its own goes again.
            let mut drop: BTreeSet<crate::id::ObjectId> = BTreeSet::new();
            for (k, made, text) in &ids {
                if after.get(k).copied().unwrap_or(0) > before.get(k).copied().unwrap_or(0) {
                    ours.insert(k.clone());
                    out.tracks.push(text.clone());
                } else {
                    drop.extend(made.iter().copied());
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
                measure(p, how)
            };
        }
    }

    // 3. Solid, for the pads the pour joined and that nothing joins now.
    let lost = |js: &[Joint], ours: &BTreeSet<Key3>| -> BTreeSet<Key3> {
        let joined: BTreeSet<Key3> =
            js.iter().filter(|j| j.zone_contact.0 > 0 || ours.contains(&key3(j))).map(key3).collect();
        had.difference(&joined).cloned().collect()
    };
    let cut = lost(&js, &ours);
    // And for the pads in a pour that nothing joins on a layer though no pour joined them
    // before either (a router's tracks around them): solid where that joins them, taken back
    // where it does not (other nets shut the pad in: an open connection, the router's).
    let dry: BTreeSet<Key3> = shorts(&js, had, &scope, min, max, false)
        .iter()
        .flat_map(|(k, v)| v.iter().filter(|s| s.necks == 0).map(|s| (k.0.clone(), k.1.clone(), s.layer.clone())))
        .filter(|k| !cut.contains(k))
        .collect();
    if !cut.is_empty() || !dry.is_empty() {
        let mut by_pad: BTreeMap<(String, String), Vec<(String, bool)>> = BTreeMap::new();
        for (k, joined_before) in cut.iter().map(|k| (k, true)).chain(dry.iter().map(|k| (k, false))) {
            by_pad.entry((k.0.clone(), k.1.clone())).or_default().push((k.2.clone(), joined_before));
        }
        // (pad, its settings before, the layers tried for a pad no pour joined, what to say)
        type Kept = ((String, String), Option<PadJoin>, Vec<String>, String);
        let mut set: Vec<Kept> = Vec::new();
        for (k, layers) in by_pad {
            let names: Vec<&str> = layers.iter().map(|(l, _)| l.as_str()).collect();
            let reason = format!(
                "zone.relieve: kept solid, no thermal spoke and no track of the net fits on {} where the pour {} the pad",
                names.join(", "),
                if layers.iter().all(|(_, before)| *before) { "joined" } else { "reaches" }
            );
            let Some(pf) = p.board_mut().footprints.get_mut(&k.0) else { continue };
            let old = pf.pads.get(&k.1).cloned();
            let join = pf.pads.entry(k.1.clone()).or_default();
            for (l, _) in &layers {
                join.layers.insert(l.clone(), PadConnection::Solid);
            }
            join.reason = Some(reason.clone());
            let unjoined = layers.iter().filter(|(_, before)| !*before).map(|(l, _)| l.clone()).collect();
            set.push((
                k.clone(),
                old,
                unjoined,
                format!("{}: {}", label(&k), reason.trim_start_matches("zone.relieve: ")),
            ));
        }
        js = measure(p, how);
        // Solid that joins nothing is taken back.
        let joined: BTreeSet<Key3> = js.iter().filter(|j| j.zone_contact.0 > 0).map(key3).collect();
        let mut back = false;
        for (k, old, layers, text) in set {
            let vain: Vec<&String> =
                layers.iter().filter(|l| !joined.contains(&(k.0.clone(), k.1.clone(), (*l).clone()))).collect();
            if vain.is_empty() {
                out.solid.push(text);
                continue;
            }
            back = true;
            if let Some(pf) = p.board_mut().footprints.get_mut(&k.0) {
                match old {
                    Some(o) => pf.pads.insert(k.1.clone(), o),
                    None => pf.pads.remove(&k.1),
                };
            }
            let names: Vec<&str> = vain.iter().map(|l| l.as_str()).collect();
            tried.entry(k.clone()).or_default().push(format!(
                "solid on {} joins nothing: other nets shut the pad in, the connection is open",
                names.join(", ")
            ));
        }
        if back {
            js = measure(p, how);
        }
    }
    out.cut = lost(&js, &ours).iter().map(|(r, n, l)| format!("{r}.{n} on {l}")).collect();
    out.fitted.sort();
    out.tracks.sort();
    let left = shorts(&js, had, &scope, min, max, true);
    // What stands where the missing spokes would run.
    let keys: BTreeSet<Key3> =
        left.iter().flat_map(|(k, v)| v.iter().map(|s| (k.0.clone(), k.1.clone(), s.layer.clone()))).collect();
    let in_the_way = if keys.is_empty() {
        BTreeMap::new()
    } else {
        let mut items = super::base_copper_items(p);
        let fills = match how {
            Measure::Kept => zones::fill_zones(p, &items),
            Measure::Scratch => zones::fill_zones_uncached(p, &items),
        };
        items.extend(zones::fills_as_items(fills));
        blockers(p, &items, &keys)
    };
    for (k, v) in left {
        let state: Vec<String> = v.iter().map(|s| format!("{} of {} on {}", s.necks, s.want, s.layer)).collect();
        let mut why = tried.remove(&k).unwrap_or_default();
        if why.is_empty() {
            why.push("its relief is set on the pad or its placement, left as it is".into());
        }
        let mut found: BTreeSet<Blocker> = BTreeSet::new();
        for s in &v {
            if let Some(b) = in_the_way.get(&(k.0.clone(), k.1.clone(), s.layer.clone())) {
                found.extend(b.iter().cloned());
                out.blocked.insert(format!("{} on {}", label(&k), s.layer), b.clone());
            }
        }
        if !found.is_empty() {
            why.push(format!("in the way: {}", Blocker::list(&found)));
        }
        out.reasons.insert(label(&k), format!("{}: {}", state.join(", "), why.join("; ")));
    }
    js
}

/// What stands where a thermal spoke of a pad would run ([`blockers`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Blocker {
    /// A track of another net, within its clearance: the router's to move.
    Track {
        /// The track's number (`track#12`).
        id: u64,
        /// Its net.
        net: Option<String>,
    },
    /// A via of another net.
    Via {
        /// The via's number (`via#12`).
        id: u64,
        /// Its net.
        net: Option<String>,
    },
    /// A pad of another net of another part (`U3.4`), placed beside it.
    Pad {
        /// The pad.
        pad: String,
        /// Its net.
        net: Option<String>,
    },
    /// A pad of another net of the pad's own footprint: the next pin of its row. Nothing on
    /// the board moves it; the directions it takes are not held against the pad
    /// ([`Joint::possible`]), and what else stands in them is not listed.
    Own {
        /// The pad.
        pad: String,
        /// Its net.
        net: Option<String>,
    },
    /// The pour of another net's zone, filled before this one.
    Zone {
        /// The zone.
        zone: String,
        /// Its net.
        net: Option<String>,
    },
    /// A copper drawing or text of another net.
    Copper {
        /// Its net.
        net: Option<String>,
    },
    /// A non-plated hole and the clearance copper keeps from it.
    Hole,
    /// A keep-out that forbids pours.
    Keepout {
        /// Its name.
        name: String,
    },
    /// The board edge and the clearance copper keeps from it.
    Edge,
    /// The outline of the pad's own zone: the pad lies at its border.
    Outline,
    /// The relief gap of another pad of the same net beside it.
    Gap {
        /// That pad.
        pad: String,
    },
    /// No copper of the pour there, with nothing of another net in the way: what is left
    /// between the pad and its neighbours is narrower than the zone's minimum width or than
    /// half a spoke, or an island that nothing of the net joins.
    Narrow,
}

impl Blocker {
    /// As text: `track#12 (net SDA)`.
    pub fn text(&self) -> String {
        let of = |n: &Option<String>| n.as_ref().map(|n| format!(" (net {n})")).unwrap_or_default();
        match self {
            Blocker::Track { id, net } => format!("track#{id}{}", of(net)),
            Blocker::Via { id, net } => format!("via#{id}{}", of(net)),
            Blocker::Pad { pad, net } => format!("pad {pad}{}", of(net)),
            Blocker::Own { pad, net } => format!("its footprint's pad {pad}{}", of(net)),
            Blocker::Zone { zone, net } => format!("the pour of zone:{zone}{}", of(net)),
            Blocker::Copper { net } => format!("a copper drawing{}", of(net)),
            Blocker::Hole => "a non-plated hole".into(),
            Blocker::Keepout { name } => format!("keep-out {name}"),
            Blocker::Edge => "the board edge".into(),
            Blocker::Outline => "the outline of its zone".into(),
            Blocker::Gap { pad } => format!("the relief gap of {pad}"),
            Blocker::Narrow => "no pour wide enough to keep (the zone's minimum width)".into(),
        }
    }

    /// Several, as a list for a message.
    pub fn list<'a>(all: impl IntoIterator<Item = &'a Blocker>) -> String {
        all.into_iter().map(Blocker::text).collect::<Vec<_>>().join(", ")
    }

    /// The object as a subject of a diagnostic, when it is one that can be named.
    pub fn subject(&self) -> Option<ObjectRef> {
        match self {
            Blocker::Track { id, .. } => Some(ObjectRef::Item { kind: "track".into(), index: *id }),
            Blocker::Via { id, .. } => Some(ObjectRef::Item { kind: "via".into(), index: *id }),
            Blocker::Pad { pad, .. } | Blocker::Own { pad, .. } | Blocker::Gap { pad } => ObjectRef::parse(pad).ok(),
            Blocker::Zone { zone, .. } => Some(zone_ref(zone)),
            Blocker::Keepout { name } => Some(ObjectRef::Named { kind: "keepout".into(), name: name.clone() }),
            _ => None,
        }
    }

    /// Whose it is to move: `route` (a track or via: the router's), `place` (a pad of another
    /// part: the placement's), `footprint` (a pad of its own footprint: no one's), `zone`
    /// (another pour, the zone's outline or minimum width, a keep-out, a copper drawing),
    /// `board` (the edge, a hole), `relief` (the gap of a pad of the net beside it).
    pub fn class(&self) -> &'static str {
        match self {
            Blocker::Track { .. } | Blocker::Via { .. } => "route",
            Blocker::Pad { .. } => "place",
            Blocker::Own { .. } => "footprint",
            Blocker::Zone { .. }
            | Blocker::Outline
            | Blocker::Narrow
            | Blocker::Keepout { .. }
            | Blocker::Copper { .. } => "zone",
            Blocker::Edge | Blocker::Hole => "board",
            Blocker::Gap { .. } => "relief",
        }
    }
}

/// For each pad and layer of `pads` that its zone relieves: what stands where the spokes that
/// do not reach it would run, in every direction of the relief (`Relief::dirs` and
/// `Relief::alt`) in which neither the pour nor other copper of the net meets the pad. Sorted,
/// each thing once. `items` are the board's copper items with the fills. A diagnostic's detail:
/// the clearances are those the fill keeps, a custom rule between two items apart.
pub fn blockers(p: &Project, items: &[CopperItem], pads: &BTreeSet<Key3>) -> BTreeMap<Key3, Vec<Blocker>> {
    let mut out: BTreeMap<Key3, Vec<Blocker>> = BTreeMap::new();
    if pads.is_empty() {
        return out;
    }
    let board = p.board();
    let rules = &board.rules;
    let clear = super::clearance::Clearances::new(p, items, true);
    let area = zones::board_area(p, rules.copper_to_edge).ok().flatten();
    let mut order: Vec<usize> = (0..board.zones.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(board.zones[i].priority), i));
    let zoned: Vec<(&Zone, ZoneParams, Polygon)> = order
        .iter()
        .filter(|&&i| !is_teardrop(&board.zones[i]))
        .map(|&i| {
            let z = &board.zones[i];
            let ring: Vec<poly::Point> = z.outline.iter().map(|&q| q.into()).collect();
            (z, zones::zone_params(p, z), Polygon::new(ring, vec![]))
        })
        .collect();
    let zone_names: BTreeMap<crate::id::ObjectId, &str> = board.zones.iter().map(|z| (z.id, z.name.as_str())).collect();
    // The non-plated holes, which pours keep away from (center, radius); a plated hole of
    // another net is named by its pad or its via.
    let holes: Vec<(poly::Point, i64)> = placed_pads(p)
        .into_iter()
        .filter_map(|pp| match pp.hole {
            Some((d, false)) => Some((pp.hole_at.into(), d.0 / 2)),
            _ => None,
        })
        .collect();
    let hole_c = |zone_c: Nm| if rules.hole_clearance > Nm::ZERO { rules.hole_clearance.0 } else { zone_c.0 };
    // The place of each zone in the order of the fills.
    let filled: BTreeMap<crate::id::ObjectId, usize> =
        order.iter().enumerate().map(|(k, &i)| (board.zones[i].id, k)).collect();
    for layer in board.stackup.copper_names() {
        let on: Vec<usize> = (0..items.len()).filter(|&i| items[i].layers.contains(&layer)).collect();
        let index = RTree::new(on.iter().map(|&i| items[i].shape.bbox()));
        for &i in &on {
            let it = &items[i];
            let ItemRef::Pad(refdes, number) = &it.item else { continue };
            let key = (refdes.clone(), number.clone(), layer.clone());
            if !pads.contains(&key) {
                continue;
            }
            let (Some(net), Some(bbox)) = (it.net.as_deref(), it.shape.bbox()) else { continue };
            let Some((z, prm, outline)) = zoned.iter().find(|(z, _, outline)| {
                z.net.as_deref() == Some(net)
                    && z.layers.contains(&layer)
                    && outline.bbox().is_some_and(|b| b.intersects(&bbox))
                    && poly::intersects(outline, &it.shape)
            }) else {
                continue;
            };
            if zones::connection_on(it, &layer, prm.pads) != PadConnection::Thermal {
                continue;
            }
            let my_place = filled.get(&z.id).copied().unwrap_or(usize::MAX);
            let r = Relief::of(it, prm);
            let zone_c = prm.clearance.max(zones::class_clearance(p, z.net.as_deref()).unwrap_or(rules.clearance));
            let Ok(outer) = poly::offset(&it.shape, BAND, Join::Round, OBSTACLE_TOL) else { continue };
            let Ok(band) = Boolean::new()
                .subject(&outer, FillRule::NonZero)
                .clip(&it.shape, FillRule::NonZero)
                .op(Op::Difference)
                .execute()
            else {
                continue;
            };
            let reach = bbox.expand(r.reach() + clear.max().0.max(zone_c.0) + zones::SAFETY + 2 * BAND);
            let mut near: Vec<u32> = index.query(&reach);
            near.sort_unstable();
            let near: Vec<usize> = near.into_iter().map(|k| on[k as usize]).filter(|&k| k != i).collect();
            // Copper of the net that is not a pad: a direction it meets the pad in has its neck.
            let own: PolygonSet = near
                .iter()
                .map(|&k| &items[k])
                .filter(|o| o.net.as_deref() == Some(net) && !matches!(o.item, ItemRef::Pad(..)))
                .flat_map(|o| o.shape.iter().cloned())
                .collect();
            let mut found: BTreeSet<Blocker> = BTreeSet::new();
            // Whether a direction has its neck is asked along the middle of its spoke: a spoke
            // as wide as the pad covers the pad's other sides too.
            let thin = Relief { w: (r.w / 4).max(4 * BAND), ..r.clone() };
            let probes: Vec<Polygon> =
                zones::spokes(it, &thin).into_iter().chain(zones::alt_spokes(it, &thin)).collect();
            for (di, s) in zones::spokes(it, &r).iter().chain(&zones::alt_spokes(it, &r)).enumerate() {
                let piece = Boolean::new()
                    .subject(std::slice::from_ref(&probes[di]), FillRule::NonZero)
                    .clip(&band, FillRule::NonZero)
                    .op(Op::Intersection)
                    .execute()
                    .unwrap_or_default();
                if piece.is_empty() || poly::intersects(&own, &piece) {
                    continue;
                }
                // What stands in this direction; a pad of the footprint itself takes it whole.
                // The spoke grown by a clearance meets what keeps that clearance from it (a
                // pour is large: the spoke is grown, not the pour).
                let mut grown: BTreeMap<i64, PolygonSet> = BTreeMap::new();
                let mut here: BTreeSet<Blocker> = BTreeSet::new();
                for &k in &near {
                    let o = &items[k];
                    if o.net.as_deref() == Some(net) || super::tied(it, o) {
                        continue;
                    }
                    let c = match &o.item {
                        // A pour filled after this one keeps away from it, not this one from it.
                        ItemRef::Zone(id, ..) if filled.get(id).copied().unwrap_or(usize::MAX) > my_place => continue,
                        ItemRef::Zone(..) => zone_c.0,
                        _ => clear.item(k).max(zone_c).0,
                    } + zones::SAFETY;
                    if !o.shape.bbox().is_some_and(|b| s.bbox().is_some_and(|sb| b.expand(c).intersects(&sb))) {
                        continue;
                    }
                    let reach = grown.entry(c).or_insert_with(|| {
                        poly::offset(std::slice::from_ref(s), c, Join::Round, OBSTACLE_TOL).unwrap_or_default()
                    });
                    if !poly::intersects(&o.shape, &*reach) {
                        continue;
                    }
                    here.insert(match &o.item {
                        ItemRef::Track(id) => Blocker::Track { id: id.0, net: o.net.clone() },
                        ItemRef::Via(id) => Blocker::Via { id: id.0, net: o.net.clone() },
                        ItemRef::Pad(r, n) if r == refdes => {
                            Blocker::Own { pad: format!("{r}.{n}"), net: o.net.clone() }
                        }
                        ItemRef::Pad(r, n) => Blocker::Pad { pad: format!("{r}.{n}"), net: o.net.clone() },
                        ItemRef::Zone(id, ..) => Blocker::Zone {
                            zone: zone_names.get(id).copied().unwrap_or_default().to_string(),
                            net: o.net.clone(),
                        },
                        ItemRef::Graphic(..) | ItemRef::FpGraphic(..) => Blocker::Copper { net: o.net.clone() },
                    });
                }
                if here.iter().any(|b| matches!(b, Blocker::Own { .. })) {
                    found.extend(here.into_iter().filter(|b| matches!(b, Blocker::Own { .. })));
                    continue;
                }
                if area.as_ref().is_some_and(|a| !poly::contains(a, s)) {
                    here.insert(Blocker::Edge);
                }
                if !poly::contains(std::slice::from_ref(outline), s) {
                    here.insert(Blocker::Outline);
                }
                for k in &board.keepouts {
                    if k.no_pours
                        && !k.allows(Some(net))
                        && (k.layers.is_empty() || k.layers.contains(&layer))
                        && k.outline.len() >= 3
                    {
                        let ring: Vec<poly::Point> = k.outline.iter().map(|&q| q.into()).collect();
                        if poly::intersects(&Polygon::new(ring, vec![]), s) {
                            here.insert(Blocker::Keepout { name: k.name.clone() });
                        }
                    }
                }
                for (at, radius) in &holes {
                    let grown = radius + hole_c(zone_c) + zones::SAFETY;
                    if s.bbox().is_some_and(|sb| sb.intersects(&Rect::new(*at, *at).expand(grown)))
                        && let Ok(ring) = poly::Circle::new(*at, grown).to_ring(OBSTACLE_TOL)
                        && poly::intersects(&Polygon::new(ring, vec![]), s)
                    {
                        here.insert(Blocker::Hole);
                    }
                }
                if !here.is_empty() {
                    found.extend(here);
                    continue;
                }
                // Nothing of another net: the gap of a pad of the net beside it, or a pour
                // too narrow to be kept.
                let mut gap = false;
                for &k in &near {
                    let o = &items[k];
                    let ItemRef::Pad(r2, n2) = &o.item else { continue };
                    if o.net.as_deref() != Some(net)
                        || zones::connection_on(o, &layer, prm.pads) == PadConnection::Solid
                    {
                        continue;
                    }
                    let g = match zones::connection_on(o, &layer, prm.pads) {
                        PadConnection::None => prm.clearance.0,
                        _ => Relief::of(o, prm).gap,
                    };
                    if poly::offset(&o.shape, g, Join::Round, OBSTACLE_TOL)
                        .is_ok_and(|grown| poly::intersects(&grown, s))
                    {
                        found.insert(Blocker::Gap { pad: format!("{r2}.{n2}") });
                        gap = true;
                    }
                }
                if !gap {
                    found.insert(Blocker::Narrow);
                }
            }
            if !found.is_empty() {
                out.entry(key).or_default().extend(found);
            }
        }
    }
    for v in out.values_mut() {
        v.sort();
        v.dedup();
    }
    out
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
            own_solid: false,
            purpose: None,
            reasoned: false,
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
        // Necks by their copper (D119): one piece of copper as wide as two spokes is no pad
        // on a single spoke; one as wide as a spoke and a half is.
        let wide = joint(PadClass::Smd, t, 1, 500_000, 500_000);
        assert_eq!((wide.necks, wide.worth()), (1, 2));
        assert!(!wide.starved(2, 67));
        let fat = joint(PadClass::Smd, t, 1, 400_000, 400_000);
        assert!(fat.worth() == 1 && fat.starved(2, 67));
    }

    /// What a pad says it is solid for goes before what it looks like (D119): a press-fit pin
    /// and a stitching pad are not soldered, a heat pad spreads heat, a terminal is the solder
    /// pad it is; and a joint is meant by its purpose, by a solid setting while relief is not
    /// required, by a setting with its reason when it is.
    #[test]
    fn purposes() {
        let mm = 1_000_000;
        let tht = PadKind::Tht { drill: Nm(mm) };
        let mut pads = vec![
            placed("J1", "1", (0, 0), (1_700_000, 1_700_000), tht),
            placed("J1", "2", (0, 3 * mm), (1_700_000, 1_700_000), tht),
            placed("J1", "3", (0, 6 * mm), (1_700_000, 1_700_000), tht),
            placed("J1", "4", (0, 9 * mm), (1_700_000, 1_700_000), tht),
            placed("J1", "5", (0, 12 * mm), (1_700_000, 1_700_000), tht),
        ];
        pads[0].overrides.purpose = Some(PadPurpose::PressFit);
        pads[1].overrides.purpose = Some(PadPurpose::Terminal);
        pads[2].overrides.purpose = Some(PadPurpose::Heat);
        pads[3].overrides.purpose = Some(PadPurpose::Stitching);
        let class = |n: &str| class_of(&pads, "J1", n);
        assert_eq!(
            [class("1"), class("2"), class("3"), class("4"), class("5")],
            [PadClass::Bare, PadClass::Through, PadClass::Heat, PadClass::Bare, PadClass::Through]
        );
        let mut j = joint(PadClass::Through, PadConnection::Solid, 1, 4_000_000, 4_000_000);
        assert!(!j.meant(false) && !j.meant(true), "a zone's solid is no one's purpose");
        j.own = true;
        assert!(j.meant(false) && !j.meant(true), "a setting alone, where relief is required, is not enough");
        j.reasoned = true;
        assert!(j.meant(true));
        j.reasoned = false;
        j.purpose = Some(PadPurpose::Terminal);
        assert!(j.meant(true));
    }

    /// A track around what is in the way (D119): from a pad along a corridor of allowed
    /// copper that turns a corner to the pour. No straight track from the pad's middle
    /// reaches it; the detour does, in a few segments, inside the corridor. With the corridor
    /// cut there is none, nor when the only way out runs beside the spoke the pad has.
    #[test]
    fn a_track_around_the_corner() {
        let mm = 1_000_000;
        let sq =
            |x0: i64, y0: i64, x1: i64, y1: i64| rect(&Rect::new(poly::Point::new(x0, y0), poly::Point::new(x1, y1)));
        let pad = CopperItem {
            item: ItemRef::Pad("U1".into(), "1".into()),
            net: Some("GND".into()),
            layers: vec!["F.Cu".into()],
            shape: vec![sq(-mm / 2, -mm / 2, mm / 2, mm / 2)],
            anchor: Point::new(Nm(0), Nm(0)),
            local: Default::default(),
        };
        // East along y = 0 to x = 1.4, then north to the pour at y = 1.2 and beyond.
        let pour: PolygonSet = vec![sq(800_000, 1_200_000, 1_600_000, 2 * mm)];
        let corridor = |cut: bool| -> PolygonSet {
            let mut parts = vec![sq(mm / 2, -200_000, 1_400_000, 200_000), sq(mm, -200_000, 1_400_000, 1_200_000)];
            if cut {
                parts[1] = sq(mm, 600_000, 1_400_000, 1_200_000);
            }
            parts.extend(pour.iter().cloned());
            poly::union_all(&parts, FillRule::NonZero).unwrap()
        };
        let w = 200_000;
        assert!(reaches(&pad, &corridor(false), &pour, w, 1).is_empty(), "no straight track");
        let path = detour(&pad, &corridor(false), &pour, w, 200_000).expect("a way around the corner");
        assert!((3..=4).contains(&path.len()), "round the corner in two or three segments: {path:?}");
        assert_eq!(path[0], poly::Point::new(0, 0));
        let mut allowed = corridor(false);
        allowed.extend(pad.shape.iter().cloned());
        let allowed = poly::union_all(&allowed, FillRule::NonZero).unwrap();
        for seg in path.windows(2) {
            let d = (seg[1].x - seg[0].x, seg[1].y - seg[0].y);
            let len = ((d.0 * d.0 + d.1 * d.1) as f64).sqrt().round() as i64;
            assert!(poly::contains(&allowed, &stroke(seg[0], d, len, w).0), "{seg:?} inside the allowed copper");
        }
        let end = *path.last().unwrap();
        assert!(poly::contains(&pour, &sq(end.x - w / 2, end.y - w / 2, end.x + w / 2, end.y + w / 2)), "{end:?}");
        assert!(detour(&pad, &corridor(true), &pour, w, 200_000).is_none(), "the corridor cut");
        // A spoke the pad has, east in the corridor: the way out would run on it.
        let mut with_spoke = pour.clone();
        with_spoke.push(sq(mm / 2, -100_000, 700_000, 100_000));
        assert!(detour(&pad, &corridor(false), &with_spoke, w, 200_000).is_none(), "beside the spoke it has");
    }
}
