//! The [guide](https://crates.io/crates/guide) routing engine as a second backend
//! (`router: guide`, DECISIONS D85, D88, D91, D93, D95, D96, D97, D101, D104, D106, D108 and D110; docs/ROUTER.md, "guide as a
//! backend").
//!
//! [`adapt`] turns the board into guide's `pcb::Design` through guide's API (no DSN file in
//! between): pads with their exact shapes, pad stacks and holes, existing tracks and vias,
//! copper drawings, net ties, keep-outs with their net exceptions, the outline with its
//! cutouts, the rules with net classes, pair gaps and the custom clearance rules that are an
//! area's or a footprint's pads' ([`custom_rules`]), differential pairs, length groups, and the
//! zones with cadlab's own fills of them.
//! [`route`] runs `guide::pcb::route_with` on it under the hooks (progress, cancellation), the
//! time budget and the work budget, turns the result back into tracks and vias, verifies them
//! with cadlab's own DRC as the native router's are (flagged copper is taken out and reported)
//! and counts completion on the board with its zones filled again by cadlab (D80). What the
//! board leaves open is reported with guide's cause for it (`Routed::open`).
//!
//! guide plans on cadlab's fills (`Zones::fills`), not on a fill of its own, and is asked for
//! none at the end (`RouteOptions::fills`) and for no joining of what its own fill leaves apart
//! (`RouteOptions::join`): zone filling stays cadlab's, and what cadlab's fill leaves apart goes
//! to a second run. The first run has the nets with zones among its nets, open or not
//! ([`first_nets`]), and guide joins the pads its routes cut off those fills again itself
//! (`RouteOptions::pour_cuts`, D97) and settles what its passes of rip-up leave open (`RouteOptions::settle`,
//! D101), each open connection by a negotiation before its chains (`RouteOptions::negotiate`, D104). After the run
//! on the board with the first run's routes fixed, a last run has those routes back as routes
//! that it may move (`pcb::route_from`, [`Adapted::take_back`], D106), kept if the board then has
//! less open, and for as long as there is time and something open, another such run has the
//! nets in another order (D108). Once the runs are over, the stubs to a plane whose pads
//! cadlab's fill joins without them are taken out again (D108). The spokes of the pads that the fill
//! relieves go over as the fill makes them, and their room costs other nets' tracks (`Zones::spokes`,
//! `RouteOptions::spoke_cost`, D110); what the board itself keeps apart is told as such and left out of the
//! later runs (`Cause::Apart`, D110). In a run with a budget, the first run of guide routes every net
//! against every other at once on one grid before its passes, as the native router's negotiation does
//! (`RouteOptions::whole`, D113), with the directions the fill falls back to as spare spokes and the number
//! of spokes a pad keeps (`Zones::spare_spokes`, `Zones::min_spokes`). What guide's API cannot express is passed the stricter way and named in
//! [`RouteResult::notes`](super::RouteResult::notes): custom rules that hold new copper by
//! kind or containment, rules between two different scopes and `disallow` rules (D61, D96).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ::guide::geom::drc as gdrc;
use ::guide::geom::{LayerId, LayerSet, NetClass, Overrides, Point as GPoint, RuleArea, RuleSpec, Rules, Shape};
use ::guide::id::NetId;
use ::guide::pcb::{
    self, Blocker, Cause, CopperKind, CopperZone, Design, DesignItem, DiffPair, Fill, Islands, LengthConstraint,
    NetSpec, PadConnection as GPadConnection, Phase, RunControl, Spoke as GSpoke, Stopped, ViaSize, ZoneConnection,
    Zones,
};

use crate::board::{self as geo, COPPER_TOL, CopperItem, ItemRef, PlacedPad, RatLine};
use crate::diag::Diagnostic;
use crate::geom::Point;
use crate::geom::poly::PolygonSet;
use crate::id::ObjectId;
use crate::model::Project;
use crate::model::board::{ItemKind, PadConnection, RuleScope, Track, Via};
use crate::refs::ObjectRef;
use crate::units::Nm;

use super::model::profile_key;
use super::{
    ConnStatus, ConnectionReport, Hooks, PairReport, PairStatus, RouteError, RouteOptions, RouteResult, RouteStats,
    Scope, tracks_length,
};

/// How much of the box of a net's pads its fills must cover for guide to plan the net on them
/// as a plane (`RouteOptions::plane_coverage`): below it they are copper of the net as filled,
/// and the net is routed as tracks. Zero: every zone is a plane. On corne-cherry, whose routing
/// is zones as wide tracks, 0.1, 0.25 and 0.5 left 96 connections open in 45 s where zero
/// leaves 91 in 13 s (D88).
const PLANE_COVERAGE: f64 = 0.0;

/// The share of the time budget guide's first run may take: the rest is for the verification,
/// the refill, a second run on the routed board and the report. guide itself ends its routing
/// at 0.92 of what it is given, with no zones to join, and cleans up in the rest. Measured
/// with guide 0.0.8 (D97) on the twelve corpus boards at 60 s, connections open summed over
/// three orders of the nets: 917 with 0.45 and 783 with 0.7, and in a run of the release's
/// source before it, 863 with 0.45, 786 with 0.6, 781 with 0.7 and 821 with 0.8. Until
/// 0.0.7 two things pulled apart and the share was 0.45 (D95, D96): glasgow-revD1's first run
/// made nothing more after some 20 s while a second run on the routed board made 150
/// connections, and the eleven other boards wanted the first run longer (with 0.0.7 here,
/// 975 with 0.45 and 1190 with 0.7, of which glasgow-revD1 537 and 821, and the eleven 438
/// and 369). In guide 0.0.8 a net that found no way in its corridor is searched for with
/// nothing moved and keeps what that reaches before any net crosses another's routes, which
/// is what the second run did on a board where nothing moves: glasgow-revD1's first run has
/// at 27 s nearly what its two runs had, and the board is the same with either share (465
/// and 422 over the three orders), so the eleven others decide: 452 with 0.45, 361 with 0.7.
const FIRST_SHARE: f64 = 0.7;

/// How many times what the first run needed after it (the verification and the refill) is
/// kept for after a second run, whose board has more copper.
const AFTER_SHARE: f64 = 1.5;

/// The time kept for the report.
const REPORT: Duration = Duration::from_millis(500);

/// The least time worth a second run.
const SECOND_MIN: Duration = Duration::from_secs(1);

/// The shortest time between two progress reports of a run, which is also how soon a
/// cancellation is seen.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// A board as guide takes it, with what maps its identifiers back.
pub(crate) struct Adapted {
    /// The design.
    pub design: Design,
    /// Net names by `NetId`.
    pub nets: Vec<String>,
    /// Copper layer names by `LayerId`.
    pub layers: Vec<String>,
    /// What could not be expressed (`route.guide_*` warnings with subjects and hints).
    pub notes: Vec<Diagnostic>,
    /// The lattice pitch guide derives: half the smallest track width plus clearance of the
    /// nets routed (0 when none is).
    pub pitch: Nm,
    /// Pad labels (`U1.3`; the designator alone for a hole without copper) by position in
    /// the design's pads.
    pub pads: Vec<String>,
    /// The board's tracks by position in the design's.
    pub tracks: Vec<ObjectId>,
    /// The board's vias by position in the design's.
    pub vias: Vec<ObjectId>,
    /// Keep-out names by position in the design's.
    pub keepouts: Vec<String>,
    /// Copper drawings by position in the design's filled zones.
    pub copper: Vec<String>,
    /// Copper drawings without a net by position in the design's obstacles.
    pub obstacles: Vec<String>,
    /// The routes of an earlier run that guide may move ([`Adapted::take_back`]); empty for a
    /// first run.
    pub earlier: pcb::Earlier,
}

fn gp(q: Point) -> GPoint {
    GPoint::new(q.x.0, q.y.0)
}

fn back(q: GPoint) -> Point {
    Point::new(Nm(q.x), Nm(q.y))
}

fn geom_err(what: &str, e: impl std::fmt::Display) -> RouteError {
    RouteError::Backend(format!("{what}: {e}"))
}

/// The pieces of a polygon set as guide shapes (rings resolved by winding, so holes stay
/// holes).
fn region(set: &PolygonSet) -> Result<Vec<Shape>, ::guide::geom::GeomError> {
    let mut out = Vec::new();
    for pg in set {
        let rings: Vec<Vec<GPoint>> = std::iter::once(&pg.outer)
            .chain(pg.holes.iter())
            .map(|r| r.0.iter().map(|q| GPoint::new(q.x, q.y)).collect())
            .collect();
        let refs: Vec<&[GPoint]> = rings.iter().map(Vec::as_slice).collect();
        out.extend(Shape::region(&refs)?);
    }
    Ok(out)
}

/// A simple ring as one shape, or its pieces when it is not simple.
fn ring_shapes(ring: &[GPoint]) -> Result<Vec<Shape>, ::guide::geom::GeomError> {
    match Shape::polygon(ring, &[]) {
        Ok(s) => Ok(vec![s]),
        Err(_) => Shape::region(&[ring]),
    }
}

/// The exact shape of a copper item where it has one (`board::exact`: a core grown by a
/// radius, which is guide's shape model too), else its polygons.
fn item_shapes(p: &Project, it: &CopperItem) -> Result<Vec<Shape>, ::guide::geom::GeomError> {
    if let Some(core) = geo::exact::item_core(p, it) {
        let d = (2.0 * core.radius).round() as i64;
        let pts: Vec<GPoint> = core.points.iter().map(|q| gp(*q)).collect();
        let exact = match (pts.len(), core.closed) {
            (1, _) if d > 0 => Shape::circle(pts[0], d).ok(),
            (2, false) if d > 0 => Shape::track(pts[0], pts[1], d).ok(),
            (n, true) if n >= 3 => Shape::rounded_polygon(&pts, &[], d / 2).ok().filter(|_| d % 2 == 0),
            _ => None,
        };
        if let Some(s) = exact {
            return Ok(vec![s]);
        }
    }
    region(&it.shape)
}

fn layer_set(names: &[String], layers: &[String]) -> LayerSet {
    layers
        .iter()
        .filter_map(|l| names.iter().position(|n| n == l))
        .fold(LayerSet::EMPTY, |s, i| s.with(LayerId(i as u8)))
}

fn connection(c: PadConnection) -> ZoneConnection {
    match c {
        PadConnection::Thermal => ZoneConnection::Thermal,
        PadConnection::Solid => ZoneConnection::Solid,
        PadConnection::None => ZoneConnection::None,
        PadConnection::ThtThermal => ZoneConnection::ThermalThroughHole,
    }
}

/// The hole of a pad as a shape: a circle, or a slot.
fn hole_shape(pp: &PlacedPad) -> Option<Shape> {
    let (d, _) = pp.hole?;
    match pp.slot {
        Some((a, b)) => Shape::track(gp(a), gp(b), d.0).ok(),
        None => Shape::circle(gp(pp.hole_at), d.0).ok(),
    }
}

/// A box: its least corner and its greatest (x, y, x, y).
type Bounds = [i64; 4];

fn bounds(points: impl Iterator<Item = (i64, i64)>) -> Option<Bounds> {
    points.fold(None, |b: Option<Bounds>, (x, y)| {
        Some(b.map_or([x, y, x, y], |b| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]))
    })
}

/// Whether two boxes are within `margin` of each other.
fn near(a: &Bounds, b: &Bounds, margin: i64) -> bool {
    a[0] - margin <= b[2] && b[0] - margin <= a[2] && a[1] - margin <= b[3] && b[1] - margin <= a[3]
}

/// How near a pad a later rule's courtyard or area must be for the rule to count at the pad
/// ([`custom_rules`]): a via or the end of a track beside the pad can then be in it.
const RULE_NEAR: i64 = 1_000_000;

/// The board's custom clearance rules as guide takes them ([`custom_rules`]).
struct Custom {
    /// The rules that are an area's, the last in board order first.
    areas: Vec<RuleArea>,
    /// By copper item, the clearance of its own that the last rule holding it gives a pad or a
    /// copper drawing.
    own: Vec<Option<i64>>,
}

/// The custom clearance rules as guide can take them, and a note naming those it cannot.
///
/// The DRC (`board::clearance`) gives every item the last rule in board order whose scope
/// holds it, and a pair of items the later of their two rules, in place of their local and
/// class clearances; the native router routes by the same resolution (`router::scoped`).
/// guide has two places for that:
///
/// - *A rule area* (`RuleSpec::areas`), whose clearance is between two pieces of copper that
///   both touch it, in place of any other. A rule goes over as one when its scope is one
///   positional term that copper is in by touching (`in_courtyard` or `touches_area`), with or
///   without a layer, its kinds take in tracks and vias (every piece of new copper in the area
///   is then in the scope, and the DRC applies the rule to a pair when either item is: which
///   other kinds it names does not matter), and it is between the scope and anything or
///   between the scope and itself. The first area wins in guide and the last rule in the DRC:
///   the areas are in reverse order.
/// - *A clearance of an item's own* (`Pad::clearance`), which replaces the rules' between it
///   and anything else, as a rule that holds a pad does in the DRC: every pad and copper
///   drawing has the clearance of the last rule holding it, in place of its local one. That is
///   where a rule for the pads of a footprint goes, and what an area's rule asks between a pad
///   in it and a track outside it.
///
/// An area's clearance replaces the items' own in guide, where the DRC takes the later rule.
/// So each area (each courtyard on its own) has the largest of its rule's clearance, those of
/// the existing copper in it that a later rule holds (zone fills apart: they are filled again
/// around the routes), and those of the later rules that can hold a new track or via and whose
/// courtyards or areas meet it. A pad's own clearance is raised by such later rules near it
/// the same way. On tinytapeout-demo: 0.16 mm in the QFN-80's courtyard and 0.18 mm at its
/// pads, a later rule, is 0.18 mm in the courtyard and at the pads (D96).
///
/// Never looser than the DRC but for what the verification is for: a rule that holds new
/// copper and is no area (by kind without both tracks and vias, by containment, between two
/// different scopes) and tightens the clearance, a track long enough to reach into two areas
/// far apart, and copper that touches an area's outline without reaching into it.
fn custom_rules(
    p: &Project,
    items: &[CopperItem],
    layers: &[String],
    notes: &mut Vec<Diagnostic>,
) -> Result<Custom, RouteError> {
    /// What a rule with a clearance is to guide.
    struct Seen {
        value: i64,
        /// Whether it can hold a new track or via.
        new: bool,
        /// The boxes of the courtyards or areas its scope names (`None`: it has no such term).
        boxes: Option<Vec<Bounds>>,
        /// As a rule area: its layers and outlines.
        area: Option<(LayerSet, Vec<Vec<GPoint>>)>,
    }
    let rules = &p.board().custom_rules;
    let mut out = Custom { areas: Vec::new(), own: vec![None; items.len()] };
    if rules.iter().all(|c| c.clearance.is_none() && c.disallow.is_empty()) {
        return Ok(out);
    }
    let all = LayerSet::first(layers.len());
    let ring_box = |r: &[GPoint]| bounds(r.iter().map(|q| (q.x, q.y)));
    let mut seen: Vec<Option<Seen>> = Vec::with_capacity(rules.len());
    let mut left: Vec<&str> = Vec::new();
    for c in rules {
        let Some(value) = c.clearance else {
            if !c.disallow.is_empty() {
                left.push(&c.name);
            }
            seen.push(None);
            continue;
        };
        let s = &c.scope;
        let kind = |s: &RuleScope, k: ItemKind| s.kinds.is_empty() || s.kinds.contains(&k);
        // A scope naming footprints holds their pads and drawings only.
        let holds_new = |s: &RuleScope| s.footprint.is_none() && (kind(s, ItemKind::Track) || kind(s, ItemKind::Via));
        let one_term = s.in_courtyard.is_some() != s.touches_area.is_some();
        let is_area = c.disallow.is_empty()
            && one_term
            && kind(s, ItemKind::Track)
            && kind(s, ItemKind::Via)
            && s.footprint.is_none()
            && s.in_area.is_none()
            && c.with.as_ref().is_none_or(|w| w == s);
        // A rule between a scope and itself needs both items in it, as guide's area does: it
        // raises nothing where one item is.
        let new = match &c.with {
            None => holds_new(s),
            Some(w) => !is_area && (holds_new(s) || holds_new(w)),
        };
        if !c.disallow.is_empty() || (!is_area && new) {
            left.push(&c.name);
        }
        let pl = geo::clearance::placement(p, s);
        let rings = |rs: &Option<Vec<_>>| -> Option<Vec<Vec<GPoint>>> {
            rs.as_ref().map(|rs: &Vec<polyclip::Ring>| {
                rs.iter().map(|r| r.0.iter().map(|q| GPoint::new(q.x, q.y)).collect()).collect()
            })
        };
        // A positional term is necessary for the scope: the first one's outlines bound it.
        let term = [&pl.courtyards, &pl.areas, &pl.touched].into_iter().find_map(rings);
        let boxes = match &c.with {
            None => term.as_ref().map(|rs| rs.iter().filter_map(|r| ring_box(r)).collect()),
            Some(_) => None,
        };
        let area = is_area.then(|| {
            let set = match &s.layer {
                Some(l) => layer_set(layers, std::slice::from_ref(l)),
                None => all,
            };
            (set, term.unwrap_or_default().into_iter().filter(|r| r.len() >= 3).collect())
        });
        seen.push(Some(Seen { value: value.0.max(0), new, boxes, area }));
    }
    // The largest clearance of the rules after `k` that can hold new copper within `margin` of
    // a box.
    let later = |k: usize, at: &Bounds, margin: i64| -> i64 {
        seen[k + 1..]
            .iter()
            .flatten()
            .filter(|r| r.new && r.boxes.as_ref().is_none_or(|bs| bs.iter().any(|b| near(b, at, margin))))
            .map(|r| r.value)
            .max()
            .unwrap_or(0)
    };
    // Existing copper: the last rule holding each item, as the DRC has it.
    let last = geo::clearance::rule_matches(p, items, |r| r.clearance);
    let boxes: Vec<Option<Bounds>> = items
        .iter()
        .zip(&last)
        .map(|(it, held)| {
            held.filter(|_| !matches!(it.item, ItemRef::Zone(..)))
                .and_then(|_| bounds(it.shape.iter().flat_map(|pg| pg.outer.0.iter().map(|q| (q.x, q.y)))))
        })
        .collect();
    for (i, it) in items.iter().enumerate() {
        if let (ItemRef::Pad(..) | ItemRef::Graphic(..) | ItemRef::FpGraphic(..), Some((k, v)), Some(b)) =
            (&it.item, last[i], &boxes[i])
        {
            out.own[i] = Some(v.0.max(0).max(later(k, b, RULE_NEAR)));
        }
    }
    for (k, c) in rules.iter().enumerate().rev() {
        let Some(Seen { value, area: Some((set, rings)), .. }) = &seen[k] else { continue };
        for ring in rings {
            let Some(b) = ring_box(ring) else { continue };
            let mut clearance = (*value).max(later(k, &b, 0));
            for (i, it) in items.iter().enumerate() {
                let (Some((k2, v2)), Some(ib)) = (last[i], &boxes[i]) else { continue };
                if k2 > k
                    && v2.0 > clearance
                    && near(ib, &b, 0)
                    && layer_set(layers, &it.layers).iter().any(|l| set.contains(l))
                {
                    clearance = v2.0;
                }
            }
            for shape in ring_shapes(ring).map_err(|e| geom_err(&format!("rule `{}`", c.name), e))? {
                out.areas.push(RuleArea {
                    shape,
                    layers: *set,
                    overrides: Overrides { track_width: None, clearance: Some(clearance) },
                });
            }
        }
    }
    if !left.is_empty() {
        let mut d = Diagnostic::warning(
            "route.guide_custom_rules",
            format!(
                "custom rule(s) {} cannot be given to guide (it takes a clearance for the tracks and vias in an \
                 area or a courtyard and one of a footprint's pads, none for new copper by kind or containment, \
                 between two scopes, or `disallow`): copper that breaks one is taken out after routing",
                left.join(", ")
            ),
        )
        .with_hint("route with the native router where such a rule matters, or check `stats.drc_removed`");
        for c in &left {
            d = d.with_subject(ObjectRef::Named { kind: "rule".into(), name: (*c).to_string() });
        }
        notes.push(d);
    }
    Ok(out)
}

/// Builds guide's design from the project. `items` are the board's copper items, zone fills
/// included (`board::copper_items`); `route` the nets guide is to route (every other net's
/// copper is fixed). The zones go over with cadlab's fills of them, which guide plans on in
/// place of a fill of its own.
pub(crate) fn adapt(
    p: &Project,
    items: &[CopperItem],
    route: &BTreeSet<String>,
    opts: &RouteOptions,
) -> Result<Adapted, RouteError> {
    let board = p.board();
    let circuit = p.circuit();
    let r = &board.rules;
    let layers = board.stackup.copper_names();
    let all = LayerSet::first(layers.len());
    let mut notes: Vec<Diagnostic> = Vec::new();

    // Nets: the circuit's, and any net named by board copper or a zone.
    let mut names: BTreeSet<String> = circuit.nets.keys().cloned().collect();
    names.extend(board.tracks.iter().filter_map(|t| t.net.clone()));
    names.extend(board.vias.iter().filter_map(|v| v.net.clone()));
    names.extend(board.zones.iter().filter_map(|z| z.net.clone()));
    names.extend(items.iter().filter_map(|it| it.net.clone()));
    let nets: Vec<String> = names.into_iter().collect();
    let ids: BTreeMap<&str, NetId> = nets.iter().enumerate().map(|(i, n)| (n.as_str(), NetId(i as u32))).collect();
    let id = |n: &Option<String>| n.as_deref().and_then(|n| ids.get(n)).copied();

    // Rules: the board's, one class per net class with a clearance, pair gaps, and the custom
    // clearance rules that are an area's.
    let layer_refs: Vec<&str> = layers.iter().map(String::as_str).collect();
    let mut spec = RuleSpec::new(&layer_refs, r.min_track_width.0.max(1), r.clearance.0.max(0));
    spec.min_drill = r.min_drill.0.max(0);
    spec.min_annular_ring = r.min_annular_ring.0.max(0);
    spec.hole_to_hole = r.hole_to_hole.0.max(0);
    spec.min_clearance = r.min_clearance.0.max(0);
    spec.hole_clearance = r.hole_clearance.0.max(0);
    for (name, class) in &circuit.netclasses {
        let Some(c) = class.clearance else { continue };
        let members: Vec<NetId> = circuit
            .nets
            .iter()
            .filter(|(_, n)| n.class.as_deref() == Some(name.as_str()))
            .filter_map(|(k, _)| ids.get(k.as_str()).copied())
            .collect();
        if !members.is_empty() {
            spec.classes.push(NetClass {
                name: name.clone(),
                nets: members,
                overrides: Overrides { track_width: None, clearance: Some(c.0.max(0)) },
                layers: Vec::new(),
                via: None,
            });
        }
    }
    for d in circuit.diffpairs.values() {
        if let (Some(&a), Some(&b), Some(g)) =
            (ids.get(d.p.as_str()), ids.get(d.n.as_str()), crate::lengths::pair_gap(circuit, &d.p, &d.n))
        {
            // As the DRC: the smaller of the pair's gap and the clearance between the two nets.
            let between = profile_key(p, Some(&d.p)).clear.max(profile_key(p, Some(&d.n)).clear);
            spec.pairs.push((a, b, g.min(between).0.max(0)));
        }
    }
    let custom = custom_rules(p, items, &layers, &mut notes)?;
    spec.areas = custom.areas;
    let hole_clearance = spec.hole_clearance;
    let rules = Rules::new(spec).map_err(|e| geom_err("design rules", e))?;

    let mut gb = gdrc::Board::default();
    // How many spokes a soldered pad is to keep (`rules.min_thermal_spokes`, D109): a pad with
    // no more than that has the room of each held harder (guide 0.0.17, D113).
    let mut zones = Zones { min_spokes: p.board().rules.min_thermal_spokes, ..Default::default() };
    let mut obstacles: Vec<(Shape, LayerId)> = Vec::new();
    let (mut pad_names, mut copper_names, mut obstacle_names) = (Vec::new(), Vec::new(), Vec::new());

    // Pads: one guide pad per pad, a pad stack's other shapes as its `stack` (one piece of
    // copper through its hole). Zone fills: the zones they are of, with them as host fills.
    let placed = geo::placed_pads(p);
    let pad_of = |refdes: &str, number: &str, at: Point| {
        placed.iter().find(|pp| pp.refdes == refdes && pp.number == number && pp.center == at)
    };
    let mut holed: BTreeSet<(String, String, i64, i64)> = BTreeSet::new();
    // Pad stack number -> its pad's position; the copper drawings of net ties, by position in
    // the filled zones, with their nets.
    let mut stack_at: BTreeMap<usize, usize> = BTreeMap::new();
    // By pad stack's position, the clearance its first shape has of a custom rule.
    let mut stack_own: BTreeMap<usize, Option<i64>> = BTreeMap::new();
    let mut tie_copper: Vec<(usize, Vec<String>)> = Vec::new();
    let mut zone_at: BTreeMap<u64, usize> = BTreeMap::new();
    // By zone with a net that is no teardrop, the zone and its fill settings, for the spokes of
    // its pads; and what each pad is for (a heat pad or a pad that is not soldered is held to no
    // number of spokes).
    let relief_params: Vec<(&crate::model::board::Zone, geo::zones::ZoneParams)> = board
        .zones
        .iter()
        .filter(|z| z.net.is_some() && !geo::thermal::is_teardrop(z))
        .map(|z| (z, geo::zones::zone_params(p, z)))
        .collect();
    let classes = geo::thermal::classes(&placed);
    for (i, it) in items.iter().enumerate() {
        let set = layer_set(&layers, &it.layers);
        // The clearance of its own: that of the last custom rule holding it, else its local one.
        let own = custom.own[i].or(it.local.clearance.map(|c| c.0.max(0)));
        match &it.item {
            ItemRef::Pad(refdes, number) => {
                let shapes = item_shapes(p, it).map_err(|e| geom_err(&format!("pad {}", it.item), e))?;
                if let Some(&at) = it.local.stack.and_then(|k| stack_at.get(&k)) {
                    // One pad, one clearance: the larger, and without a rule for every shape
                    // the local one.
                    let pad = &mut gb.pads[at];
                    pad.clearance = match (stack_own[&at], custom.own[i]) {
                        (Some(a), Some(b)) => pad.clearance.max(Some(a.max(b))),
                        (None, None) => pad.clearance,
                        _ => it.local.clearance.map(|c| c.0.max(0)),
                    };
                    pad.stack.extend(shapes.into_iter().map(|s| (set, s)));
                    continue;
                }
                let first = holed.insert((refdes.clone(), number.clone(), it.anchor.x.0, it.anchor.y.0));
                let hole = if first { pad_of(refdes, number, it.anchor).and_then(hole_shape) } else { None };
                let mut shapes = shapes.into_iter();
                let Some(shape) = shapes.next() else { continue };
                let at = gb.pads.len();
                gb.pads.push(gdrc::Pad {
                    shape,
                    layers: set,
                    hole,
                    net: id(&it.net),
                    clearance: own,
                    stack: shapes.map(|s| (set, s)).collect(),
                });
                pad_names.push(it.item.to_string());
                if let Some(k) = it.local.stack {
                    stack_at.insert(k, at);
                    stack_own.insert(at, custom.own[i]);
                }
                let th = it.local.thermal;
                // The spokes of the pad's reliefs as cadlab's fill makes them (BOARD.md, "What a
                // router must be told", D112): per zone of its net that holds it and per layer,
                // where the connection in effect there is thermal and the pad is soldered, each
                // spoke of `Relief::of` (gap, the width fitted to the pad, its directions) from
                // the pad's anchor to its end in the pour. guide keeps their room (D110).
                let soldered = classes.get(&(refdes.clone(), number.clone())).is_none_or(|c| c.solder());
                let mut fitted: Option<i64> = None;
                for (z, zp) in relief_params.iter().filter(|(z, _)| soldered && z.net == it.net) {
                    if !in_ring(&z.outline, it.anchor) {
                        continue;
                    }
                    let relief = geo::zones::Relief::of(it, zp);
                    for name in z.layers.iter().filter(|l| it.layers.contains(l)) {
                        let Some(l) = layers.iter().position(|n| n == name) else { continue };
                        if geo::zones::connection_on(it, name, zp.pads) != PadConnection::Thermal {
                            continue;
                        }
                        fitted = Some(fitted.map_or(relief.w, |w| w.min(relief.w)));
                        for (dir, pg) in relief.dirs.iter().zip(geo::zones::spokes(it, &relief)) {
                            let rad = (dir.0 as f64 / 1000.0).to_radians();
                            let (ux, uy) = (rad.cos(), rad.sin());
                            let (cx, cy) = (it.anchor.x.0 as f64, it.anchor.y.0 as f64);
                            let far = pg
                                .outer
                                .0
                                .iter()
                                .map(|q| (q.x as f64 - cx) * ux + (q.y as f64 - cy) * uy)
                                .fold(0.0, f64::max);
                            zones.spokes.push(GSpoke {
                                pad: at,
                                layer: LayerId(l as u8),
                                from: gp(it.anchor),
                                to: GPoint::new((cx + ux * far).round() as i64, (cy + uy * far).round() as i64),
                                width: relief.w,
                            });
                        }
                        // The directions the fill falls back to, where those have no room (D112):
                        // a pad that the routes leave none of the first is still joined by one
                        // of these that nothing crosses, which guide counts on when it tells
                        // the pads that routes cut off their pours (D113).
                        for (dir, pg) in relief.alt.iter().zip(geo::zones::alt_spokes(it, &relief)) {
                            let rad = (dir.0 as f64 / 1000.0).to_radians();
                            let (ux, uy) = (rad.cos(), rad.sin());
                            let (cx, cy) = (it.anchor.x.0 as f64, it.anchor.y.0 as f64);
                            let far = pg
                                .outer
                                .0
                                .iter()
                                .map(|q| (q.x as f64 - cx) * ux + (q.y as f64 - cy) * uy)
                                .fold(0.0, f64::max);
                            zones.spare_spokes.push(GSpoke {
                                pad: at,
                                layer: LayerId(l as u8),
                                from: gp(it.anchor),
                                to: GPoint::new((cx + ux * far).round() as i64, (cy + uy * far).round() as i64),
                                width: relief.w,
                            });
                        }
                    }
                }
                zones.set_pad(
                    at,
                    GPadConnection {
                        connection: it.local.connection.map(connection),
                        thermal_gap: th.and_then(|t| t.gap).map(|g| g.0),
                        spoke_width: th.and_then(|t| t.spoke).map(|s| s.0).or(fitted),
                        anchor: None,
                        // The pad's own first spoke (45° for round pads, turned with the pad).
                        spoke_direction: th.map(|t| {
                            let a = (t.first_spoke(None).0 as f64 / 1000.0).to_radians();
                            ((a.cos() * 1e6).round() as i64, (a.sin() * 1e6).round() as i64)
                        }),
                        spoke_reach: None,
                    },
                );
            }
            ItemRef::Track(..) | ItemRef::Via(..) => {}
            ItemRef::Zone(zone, layer, _) => {
                let Some(layer) = layers.iter().position(|n| n == layer) else { continue };
                let Some(z) = board.zones.iter().find(|z| z.id == *zone) else { continue };
                let at = *zone_at.entry(zone.0).or_insert_with(|| {
                    let zp = geo::zones::zone_params(p, z);
                    zones.zones.push(CopperZone {
                        outline: z.outline.iter().map(|q| gp(*q)).collect(),
                        layers: layer_set(&layers, &z.layers),
                        net: id(&z.net),
                        priority: z.priority,
                        clearance: zp.clearance.0.max(0),
                        min_width: zp.min_width.0.max(0),
                        connection: connection(zp.pads),
                        thermal_gap: zp.thermal_gap.0.max(0),
                        spoke_width: zp.thermal_spoke.0.max(1),
                        islands: if z.net.is_some() { Islands::Remove } else { Islands::Keep },
                        spokes: zp.thermal_spokes,
                        // Plane or pour is guide's to say (`CopperZone::plane`): the board has
                        // no word for it. Every zone a plane, as before guide 0.0.4, left 86
                        // connections open on corne-cherry where its own choice leaves 54, and
                        // 30 against 12 on lumenpnp-mobo (D91).
                        plane: None,
                        ..Default::default()
                    });
                    zones.zones.len() - 1
                });
                for shape in region(&it.shape).map_err(|e| geom_err(&it.item.to_string(), e))? {
                    zones.fills.push(Fill { zone: at, layer: LayerId(layer as u8), shape });
                }
            }
            ItemRef::Graphic(..) | ItemRef::FpGraphic(..) => {
                let Some(&layer) = it.layers.first().and_then(|l| layers.iter().position(|n| n == l)).as_ref() else {
                    continue;
                };
                let tie = !it.local.tie_nets.is_empty();
                let shapes = region(&it.shape).map_err(|e| geom_err(&it.item.to_string(), e))?;
                for shape in shapes {
                    let net = id(&it.net);
                    if net.is_none() && !tie {
                        obstacles.push((shape, LayerId(layer as u8)));
                        obstacle_names.push(it.item.to_string());
                        continue;
                    }
                    // Copper of a net, or the bridge of a net tie, which is an item its nets
                    // may touch.
                    if tie {
                        tie_copper.push((gb.zones.len(), it.local.tie_nets.clone()));
                    }
                    gb.zones.push(gdrc::Zone { shape, layer: LayerId(layer as u8), net, clearance: own });
                    copper_names.push(it.item.to_string());
                }
            }
        }
    }
    // Net ties: the copper between the pads of a group is where its nets may touch, as in the
    // DRC (`board::tied`). The pads themselves are not: a track of one net keeps its clearance
    // from the pad of the other there too.
    for (at, names) in tie_copper {
        let mut nets: Vec<NetId> = names.iter().filter_map(|n| ids.get(n.as_str()).copied()).collect();
        nets.extend(gb.zones[at].net);
        nets.sort_by_key(|n| n.index());
        nets.dedup();
        if nets.len() > 1 {
            gb.ties.push(gdrc::NetTie { nets, items: vec![gdrc::Item::Zone(at)] });
        }
    }
    // Holes without copper (NPTH, mounting holes). With a hole clearance the hole keeps it.
    // Without one the DRC, like guide, only forbids copper over the hole, and the native router
    // keeps the copper clearance from it all the same (`model`): to route by the same rule the
    // hole is then given as copper of no net on every layer.
    for pp in placed.iter().filter(|pp| pp.layers.is_empty()) {
        let Some(shape) = hole_shape(pp) else { continue };
        let mut pad = gdrc::Pad::new(shape.clone(), if hole_clearance == 0 { all } else { LayerSet::EMPTY }, None);
        pad.hole = Some(shape);
        gb.pads.push(pad);
        pad_names.push(pp.refdes.clone());
    }
    // Existing tracks and vias: fixed.
    let (mut track_ids, mut via_ids) = (Vec::new(), Vec::new());
    for t in &board.tracks {
        let Some(layer) = layers.iter().position(|n| *n == t.layer) else { continue };
        gb.tracks.push(gdrc::Track {
            start: gp(t.start),
            end: gp(t.end),
            mid: t.mid.map(gp),
            width: t.width.0,
            layer: LayerId(layer as u8),
            net: id(&t.net),
        });
        track_ids.push(t.id);
    }
    for v in &board.vias {
        let a = layers.iter().position(|n| *n == v.from).unwrap_or(0);
        let b = layers.iter().position(|n| *n == v.to).unwrap_or(layers.len() - 1);
        gb.vias.push(gdrc::Via {
            at: gp(v.at),
            drill: v.drill.0,
            diameter: v.diameter.0,
            from: LayerId(a.min(b) as u8),
            to: LayerId(a.max(b) as u8),
            net: id(&v.net),
        });
        via_ids.push(v.id);
    }
    // Keep-outs for tracks and vias, with the nets they except (D76). One for pours has
    // nothing to say here: the fills are cadlab's.
    let mut keepout_names = Vec::new();
    for k in &board.keepouts {
        if k.outline.len() < 3 || !(k.no_tracks || k.no_vias) {
            continue;
        }
        let set = if k.layers.is_empty() { all } else { layer_set(&layers, &k.layers) };
        let ring: Vec<GPoint> = k.outline.iter().map(|q| gp(*q)).collect();
        for shape in ring_shapes(&ring).map_err(|e| geom_err(&format!("keep-out `{}`", k.name), e))? {
            gb.keepouts.push(gdrc::Keepout {
                shape,
                layers: set,
                tracks: k.no_tracks,
                vias: k.no_vias,
                pads: false,
                except: k.allow_nets.iter().filter_map(|n| ids.get(n.as_str()).copied()).collect(),
            });
            keepout_names.push(k.name.clone());
        }
    }
    // The outline and its cutouts, as edges.
    let rings: Vec<Vec<GPoint>> = board
        .outline
        .contours
        .iter()
        .map(|c| geo::contour_ring(c, COPPER_TOL).iter().map(|q| GPoint::new(q.x, q.y)).collect())
        .collect();
    let outline = pcb::edges(&rings.iter().map(Vec::as_slice).collect::<Vec<_>>());

    // How each net is routed.
    let mut pitch: Option<i64> = None;
    let specs: Vec<NetSpec> = nets
        .iter()
        .map(|n| {
            let k = profile_key(p, Some(n));
            let on = route.contains(n);
            if on {
                let half = (k.width.0 + k.clear.0) / 2;
                pitch = Some(pitch.map_or(half, |x| x.min(half)));
            }
            NetSpec {
                route: on,
                track_width: k.width.0.max(1),
                via: (k.via_dia > k.via_drill && k.via_drill.0 > 0 && layers.len() > 1)
                    .then_some(ViaSize { diameter: k.via_dia.0, drill: k.via_drill.0 }),
                layers: None,
            }
        })
        .collect();

    // Pairs routed coupled, their skew, and length groups whose members are all routed.
    let mut pairs = Vec::new();
    let mut lengths = Vec::new();
    if opts.pairs != Some(false) {
        for d in circuit.diffpairs.values() {
            if !(route.contains(&d.p) && route.contains(&d.n)) {
                continue;
            }
            let (Some(&a), Some(&b)) = (ids.get(d.p.as_str()), ids.get(d.n.as_str())) else { continue };
            let pr = crate::lengths::pair_rules(p, d);
            pairs.push(DiffPair { positive: a, negative: b, width: pr.width.0.max(1), gap: pr.gap.0.max(0) });
            if let Some(skew) = d.max_skew.filter(|_| opts.skew != Some(false)) {
                lengths.push(LengthConstraint {
                    nets: vec![a, b],
                    max_skew: Some(skew.0.max(0)),
                    ..Default::default()
                });
            }
        }
    }
    for g in circuit.length_groups.values() {
        let mut members: Vec<NetId> = Vec::new();
        let mut whole = !g.members.is_empty();
        for m in &g.members {
            let names: Vec<&String> = match circuit.diffpairs.get(m) {
                Some(d) => vec![&d.p, &d.n],
                None => vec![m],
            };
            for n in names {
                match ids.get(n.as_str()).filter(|_| route.contains(n)) {
                    Some(&i) => members.push(i),
                    None => whole = false,
                }
            }
        }
        if !whole {
            continue;
        }
        lengths.push(match g.target {
            Some(t) => LengthConstraint {
                nets: members,
                min: Some((t.0 - g.tolerance.0).max(0)),
                max: Some(t.0 + g.tolerance.0),
                target: Some(t.0),
                max_skew: None,
            },
            None => LengthConstraint { nets: members, max_skew: Some(g.tolerance.0.max(0)), ..Default::default() },
        });
    }

    let design = Design {
        board: gb,
        rules,
        outline,
        edge_clearance: r.copper_to_edge.0.max(0),
        nets: specs,
        obstacles,
        pairs,
        lengths,
        zones,
    };
    Ok(Adapted {
        design,
        nets,
        layers,
        notes,
        pitch: Nm(pitch.unwrap_or(0)),
        pads: pad_names,
        tracks: track_ids,
        vias: via_ids,
        keepouts: keepout_names,
        copper: copper_names,
        obstacles: obstacle_names,
        earlier: pcb::Earlier::default(),
    })
}

impl Adapted {
    /// Hands the tracks and vias `mine` of the board, an earlier run's, to guide as routes of
    /// their nets that this run may move (`pcb::route_from`, D106) in place of fixed copper,
    /// and has those nets routed. Returns the items handed over; which of their nets guide
    /// took back is in its result (`Routed::taken_back`).
    fn take_back(&mut self, mine: &BTreeSet<ObjectId>) -> BTreeSet<ObjectId> {
        let moves = |net: Option<NetId>, id: &ObjectId| net.is_some() && mine.contains(id);
        let mut taken = BTreeSet::new();
        let board = &mut self.design.board;
        let (mut tracks, mut track_ids) = (Vec::new(), Vec::new());
        for (t, id) in std::mem::take(&mut board.tracks).into_iter().zip(std::mem::take(&mut self.tracks)) {
            if moves(t.net, &id) {
                taken.insert(id);
                self.earlier.tracks.push(t);
            } else {
                tracks.push(t);
                track_ids.push(id);
            }
        }
        (board.tracks, self.tracks) = (tracks, track_ids);
        let (mut vias, mut via_ids) = (Vec::new(), Vec::new());
        for (v, id) in std::mem::take(&mut board.vias).into_iter().zip(std::mem::take(&mut self.vias)) {
            if moves(v.net, &id) {
                taken.insert(id);
                self.earlier.vias.push(v);
            } else {
                vias.push(v);
                via_ids.push(id);
            }
        }
        (board.vias, self.vias) = (vias, via_ids);
        for n in self.earlier.tracks.iter().filter_map(|t| t.net).chain(self.earlier.vias.iter().filter_map(|v| v.net))
        {
            if let Some(spec) = self.design.nets.get_mut(n.index()) {
                spec.route = true;
            }
        }
        taken
    }

    /// An item of the design as the board names it, with the object to point at.
    fn item(&self, item: DesignItem) -> (String, Option<ObjectRef>) {
        let named = |names: &[String], i: usize, what: &str| match names.get(i) {
            Some(n) => (format!("{what}{n}"), ObjectRef::parse(n).ok()),
            None => (format!("{what}{i}"), None),
        };
        let numbered = |ids: &[ObjectId], i: usize, kind: &str| match ids.get(i) {
            Some(id) => (format!("{kind}#{}", id.0), Some(ObjectRef::Item { kind: kind.into(), index: id.0 })),
            None => (format!("a {kind}"), None),
        };
        match item {
            DesignItem::Pad(i) => named(&self.pads, i, "pad "),
            DesignItem::Track(i) => numbered(&self.tracks, i, "track"),
            DesignItem::Via(i) => numbered(&self.vias, i, "via"),
            DesignItem::Zone(i) => named(&self.copper, i, ""),
            DesignItem::Obstacle(i) => named(&self.obstacles, i, ""),
            DesignItem::Keepout(i) => match self.keepouts.get(i) {
                Some(n) => {
                    (format!("keep-out `{n}`"), Some(ObjectRef::Named { kind: "keepout".into(), name: n.clone() }))
                }
                None => ("a keep-out".to_string(), None),
            },
            DesignItem::Outline(_) => ("the board edge".to_string(), None),
            other => (other.to_string(), None),
        }
    }
}

/// A connection guide's result leaves open, in the board's terms.
struct Open {
    net: String,
    /// The pad of the piece left out, and a pad of the piece it would join.
    to: String,
    from: String,
    at: Point,
    layer: Option<String>,
    /// Whether the cause is that pad's own (it is shut in), not the net's.
    of_pad: bool,
    reason: String,
    hints: Vec<String>,
    subjects: Vec<ObjectRef>,
}

/// What guide says of a connection it left open (`Routed::open`), as a reason with subjects
/// and hints.
///
/// `tried` is whether the net had a search of its own in the run (`Routed::tried`): a net the
/// run was stopped before is one it never got to, or one that had its turn and was without a
/// way when the run stopped.
///
/// `first` is whether the run was the first: only then is the board that guide found two pads
/// apart on (`Cause::Apart`) the board itself, with none of guide's own routes as its copper.
fn explain(ad: &Adapted, o: &pcb::OpenConnection, stopped: Option<Stopped>, tried: bool, first: bool) -> Open {
    let net = ad.nets.get(o.net.index()).cloned().unwrap_or_default();
    let pad = |e: &pcb::Endpoint| e.pad.and_then(|i| ad.pads.get(i)).cloned().unwrap_or_else(|| "a pad".to_string());
    let (from, to) = (pad(&o.from), pad(&o.to));
    let mut subjects = vec![ObjectRef::Net(net.clone())];
    let mut hints: Vec<String> = Vec::new();
    let native = format!("route net {net} with the native router (`route.nets {net}`), which can move other nets");
    let later_apart = o.cause == Cause::Apart && !first;
    let cause = if later_apart { &Cause::NoPath } else { &o.cause };
    let reason = match cause {
        Cause::NoLayer => {
            hints.push(format!("give net {net} a via (its class's via size) or a layer both pads are on"));
            format!("{to} and {from} share no copper layer net {net} may be routed on, and the net has no via")
        }
        Cause::Enclosed { by } => {
            let mut parts: Vec<String> = Vec::new();
            for b in by {
                let (label, subject) = match *b {
                    Blocker::Net(n) => {
                        let other = ad.nets.get(n.index()).cloned().unwrap_or_default();
                        hints.push(format!(
                            "rip net {other} (`route.rip`) and route net {net} first (`route.nets {net}`), then the rest"
                        ));
                        (format!("the routing of net {other}"), Some(ObjectRef::Net(other)))
                    }
                    Blocker::Item(item) => {
                        match item {
                            DesignItem::Keepout(i) => hints.push(format!(
                                "change keep-out `{}`, or except net {net} from it (`keepout.set --allow-nets`)",
                                ad.keepouts.get(i).map_or("", String::as_str)
                            )),
                            DesignItem::Outline(_) => {
                                hints.push("move parts away from the board edge, or lower `copper_to_edge`".into());
                            }
                            DesignItem::Track(_) | DesignItem::Via(_) => {
                                hints.push("rip the existing routing in the way (`route.rip`) and route again".into());
                            }
                            _ => {}
                        }
                        ad.item(item)
                    }
                    _ => ("something guide does not name".to_string(), None),
                };
                if let Some(s) = subject.filter(|s| !subjects.contains(s)) {
                    subjects.push(s);
                }
                if parts.len() < 4 && !parts.contains(&label) {
                    parts.push(label);
                }
            }
            hints.push(format!("move the parts around {to} apart (`place.move`)"));
            if parts.is_empty() {
                format!("{to} is shut in: no track of net {net} can leave it")
            } else {
                format!("{to} is shut in by {}: no track of net {net} can leave it", parts.join(", "))
            }
        }
        // Also after a run that was stopped: guide looks at what is around the pad then too
        // (0.0.5), and a pad that is shut in has `Enclosed`.
        Cause::NoPath => {
            hints.push(native.clone());
            format!("guide found no way from {to} to {from} for net {net}; {to} is not shut in nearby")
        }
        Cause::OutOfBudget => {
            hints.push(native.clone());
            format!("a search of guide for net {net} reached its node limit before it joined {to} to {from}")
        }
        Cause::Stopped => {
            hints.push(match stopped {
                Some(Stopped::WorkBudget) => "give a larger `work_budget`".to_string(),
                _ => "give a larger `budget`".to_string(),
            });
            let why = match stopped {
                Some(Stopped::WorkBudget) => "the work budget was spent",
                Some(Stopped::Deadline) => "the time budget ended",
                _ => "the run was stopped",
            };
            if tried {
                // It had its turn: another net's route took its way since, or its search was
                // the one the stop cut short.
                hints.push(native.clone());
                format!(
                    "{why} before guide had routed net {net} to {to} again: the net had its turn, and was \
                     without a way there when the run stopped"
                )
            } else {
                hints.push("route fewer nets at a time (`route.nets`)".to_string());
                format!("{why} before guide got to net {net}: {to} is apart from {from}")
            }
        }
        // The board itself keeps the two apart (guide 0.0.16, D110): no router joins them.
        Cause::Apart => {
            hints.push(format!(
                "join the two parts of the board that net {net} is on (copper across what divides them, a connector \
                 or a wire), or change the keep-out or cutout between them"
            ));
            format!(
                "the board keeps {to} apart from {from}: with no other net routed, no track or via of net {net} \
                 gets from one to the other on any layer (its copper, keep-outs, holes and edges are between them)"
            )
        }
        // `Unknown`, and any cause newer than this adapter: guide has no say on the net.
        _ => {
            hints.push(format!(
                "route net {net} again (`route.nets {net} --router guide`): a run on the routed board may join it"
            ));
            hints.push(native.clone());
            format!("guide left {to} apart from {from} on net {net} and gives no cause for it")
        }
    };
    if *cause != Cause::Apart {
        if !hints.contains(&native) {
            hints.push(native);
        }
        hints.push("move the parts apart or allow more layers".to_string());
    }
    Open {
        net,
        to,
        from,
        at: back(o.to.at),
        layer: o.to.layers.iter().next().and_then(|l| ad.layers.get(usize::from(l.0))).cloned(),
        of_pad: matches!(o.cause, Cause::Enclosed { .. }),
        reason,
        hints,
        subjects,
    }
}

/// What a run of guide says of every connection it left open.
fn explain_all(ad: &Adapted, r: &pcb::Routed, first: bool) -> Vec<Open> {
    let tried: BTreeSet<NetId> = r.tried.iter().copied().collect();
    r.open.iter().map(|o| explain(ad, o, r.stopped, tried.contains(&o.net), first)).collect()
}

/// Whether guide can run this request at all; the refusal says what to do instead.
fn supported(scope: &Scope) -> Result<(), RouteError> {
    if matches!(scope, Scope::Connection { .. }) {
        return Err(RouteError::Unsupported {
            what: "guide routes whole nets: it has no call for one connection between two pads, and no \
                   push-and-shove"
                .to_string(),
            option: "router".to_string(),
            hint: "leave `router` out for `route.connection` (the native router), or route the net with \
                   `route.nets <net> --router guide`"
                .to_string(),
        });
    }
    Ok(())
}

/// The threads that guide's negotiation routes nets side by side on: rayon's, as the native
/// router's batches have them, or one without the `parallel` feature. The routes are the
/// same on any number.
fn threads() -> usize {
    #[cfg(feature = "parallel")]
    {
        rayon::current_num_threads().max(1)
    }
    #[cfg(not(feature = "parallel"))]
    {
        1
    }
}

/// guide's options for a run of `opts` on `p`, the budgets apart. `slots` are the routing
/// layers (`routing_slots`).
fn options(p: &Project, opts: &RouteOptions, slots: &[usize]) -> pcb::RouteOptions {
    let mut gopts = pcb::RouteOptions::default();
    gopts.pitch = opts.grid.map(|g| g.0);
    gopts.couple_pairs = opts.pairs != Some(false);
    gopts.tune_lengths = opts.skew != Some(false);
    gopts.vias_in_pads = opts.via_in_pad;
    gopts.plane_coverage = PLANE_COVERAGE;
    gopts.keep_partial = true;
    // Zone filling is cadlab's: nothing here reads guide's fills, and the count is the board's.
    gopts.fills = false;
    // Nor does it join the pieces its own fill leaves a zone's net in: cadlab fills again around
    // the routes and gives guide what that fill leaves apart, in a second run. Left on, the
    // twelve corpus boards have 432 connections open at 60 s against 420, and a board that
    // routes in a quarter of a second takes a second (guide 0.0.6, D95; with 0.0.5 a run with
    // a work budget was stopped by it too, D93, which 0.0.6 no longer does).
    gopts.join = false;
    // What its routes cut off the fills it planned on, guide joins again itself before it
    // ends (guide 0.0.8, its default): cadlab's second run then has less to do, on a board
    // where nothing moves. The eleven corpus boards without glasgow-revD1 have 358
    // connections open with it over three orders at 60 s and 381 without (corne-cherry 106
    // and 118, tinytapeout-demo 22 and 28).
    gopts.pour_cuts = true;
    // What its passes of rip-up leave open, guide settles itself (guide 0.0.10, its default):
    // a pass ends where it has stopped gaining, and each open connection is then looked for
    // a route across other nets' routes, with the nets it crosses joined up again around it
    // at once, kept only if the board has less open for it. D101.
    gopts.settle = true;
    // And before its chains, by a negotiation (guide 0.0.12, its default): the connection is
    // routed across the routes in its way at a price by the length shared, the nets it runs
    // into are routed again the same way near there, in rounds at a rising price with a
    // history of where routes were in each other's way, and the routes are kept if a round
    // ends with none in another's way and the board has less open. D104.
    gopts.negotiate = true;
    // The ways out of a row of fine pitch planned for the whole row (guide 0.0.8, off by
    // default) are left off again: they were on for boards of two layers with 0.0.10 and
    // 0.0.11 (D101), and with the negotiations of 0.0.12 they make no more: at 60 s over
    // six orders corne-cherry has 160 connections open without them for 155 with, and
    // nrfmicro 19 for 18 (D104).
    gopts.row_fanout = false;
    // Two nets whose pads take turns along a row of fine pitch, with no room to cross on its
    // layer, have their crossing planned on another layer as fixed copper (guide 0.0.15, its
    // default, set by name): the D+ of nrfmicro's USB receptacle, whose pads have those of D-
    // between them and a diode's pad in front (D108).
    gopts.crossings = true;
    // The room that the spokes of a pad in a pour of its net need, from the pad's edge across
    // its gap and into the pour, costs other nets' tracks this much more for their length
    // (guide 0.0.16, off by default there): fewer pads are left short of two spokes after a
    // run (D110 has the counts). A cost and no wall: a net with no other way takes it.
    gopts.spoke_cost = 4.0;
    let layers = p.board().stackup.copper_names().len();
    // The smallest via the rules allow, for a pad trapped in its footprint: the native
    // fanout's rule (`fanout::small_via`). guide uses it only where a net's own does not fit.
    let (dia, drill) = super::smallest_via(&p.board().rules);
    gopts.fanout_via = (layers > 1 && dia > drill).then_some(ViaSize { diameter: dia.0, drill: drill.0 });
    // Every net against every other at once, on one grid, before guide's passes (guide 0.0.17,
    // off by default there): PathFinder over the whole board, as the native router does it,
    // which is what closes the packages of fine pitch that a net at a time leaves shut in
    // (D113). In a run with a budget, where guide gives the board up to its passes itself if
    // the first round says the negotiation has no room in its share; and in the first run
    // only: the later ones go on from its routes. On as many threads as the native router has.
    gopts.whole = opts.budget.is_some() || opts.work.left().is_some();
    gopts.threads = threads();
    gopts.seed = (opts.seed != 0).then_some(opts.seed);
    if !opts.layers.is_empty() {
        gopts.layers = Some(slots.iter().fold(LayerSet::EMPTY, |s, &i| s.with(LayerId(i as u8))));
    }
    gopts
}

/// Whether `at` is inside the ring `ring` (by crossings).
fn in_ring(ring: &[Point], at: Point) -> bool {
    let mut odd = false;
    for k in 0..ring.len() {
        let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
        if (a.y.0 > at.y.0) != (b.y.0 > at.y.0) {
            let t = (at.y.0 - a.y.0) as f64 / (b.y.0 - a.y.0) as f64;
            if a.x.0 as f64 + t * (b.x.0 - a.x.0) as f64 > at.x.0 as f64 {
                odd = !odd;
            }
        }
    }
    odd
}

/// One new item of the result: a track (index into the tracks) or a via.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum New {
    Track(usize),
    Via(usize),
}

/// The new copper as connected pieces: tracks of a net sharing an end on a layer, and vias at
/// an end. Returns each item's piece (its lowest member's position, tracks before vias).
fn pieces(tracks: &[Track], vias: &[Via]) -> Vec<usize> {
    let n = tracks.len() + vias.len();
    let mut root: Vec<usize> = (0..n).collect();
    fn find(root: &mut [usize], mut i: usize) -> usize {
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    let join = |root: &mut Vec<usize>, a: usize, b: usize| {
        let (a, b) = (find(root, a), find(root, b));
        root[a.max(b)] = a.min(b);
    };
    // (net, x, y) -> via; (net, layer, x, y) -> first track end there.
    let mut at_via: BTreeMap<(Option<&str>, i64, i64), usize> = BTreeMap::new();
    for (i, v) in vias.iter().enumerate() {
        at_via.insert((v.net.as_deref(), v.at.x.0, v.at.y.0), tracks.len() + i);
    }
    let mut at_end: BTreeMap<(Option<&str>, &str, i64, i64), usize> = BTreeMap::new();
    for (i, t) in tracks.iter().enumerate() {
        for e in [t.start, t.end] {
            if let Some(&v) = at_via.get(&(t.net.as_deref(), e.x.0, e.y.0)) {
                join(&mut root, i, v);
            }
            match at_end.entry((t.net.as_deref(), t.layer.as_str(), e.x.0, e.y.0)) {
                std::collections::btree_map::Entry::Occupied(o) => join(&mut root, i, *o.get()),
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(i);
                }
            }
        }
    }
    (0..n).map(|i| find(&mut root, i)).collect()
}

/// Most runs after the first ([`route`], "A second run"): one on the board with the first run's
/// routes fixed, and one that takes them back to move them (D106).
const POUR_ROUNDS: usize = 2;

/// How many more runs that move routes there may be after the first of them, each with
/// another order of the nets, and what the seed moves by from one to the next.
const MOVE_AGAIN: usize = 4;
const AGAIN_SEED: u64 = 7919;

/// The most tracks a stub to a plane has (`route`, "The stubs that the fill makes needless").
const STUB_TRACKS: usize = 4;

/// What taking the needless stubs out needs, in times what the first run needed after it: two
/// fills of the board.
const PRUNE_SHARE: f64 = 2.5;

/// The pieces of new copper (by [`pieces`]) that are stubs to a plane: of a net with zones,
/// one via that is no escape's and one to [`STUB_TRACKS`] tracks.
fn stub_pieces(
    tracks: &[Track],
    vias: &[Via],
    piece: &[usize],
    poured: &BTreeSet<&str>,
    escapes: &BTreeSet<(i64, i64)>,
) -> BTreeSet<usize> {
    let nt = tracks.len();
    // By piece: its vias and tracks.
    let mut size: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    let mut not: BTreeSet<usize> = BTreeSet::new();
    for (i, t) in tracks.iter().enumerate() {
        if t.net.as_deref().is_some_and(|n| poured.contains(n)) {
            size.entry(piece[i]).or_default().1 += 1;
        }
    }
    for (i, v) in vias.iter().enumerate() {
        if v.net.as_deref().is_some_and(|n| poured.contains(n)) {
            size.entry(piece[nt + i]).or_default().0 += 1;
            if escapes.contains(&(v.at.x.0, v.at.y.0)) {
                not.insert(piece[nt + i]);
            }
        }
    }
    size.iter()
        .filter(|(k, (v, t))| *v == 1 && (1..=STUB_TRACKS).contains(t) && !not.contains(k))
        .map(|(k, _)| *k)
        .collect()
}

/// The least time a run that takes the routes back is begun with: it places every route
/// before it routes anything.
const MOVE_MIN: Duration = Duration::from_secs(3);

fn phase(p: Phase) -> &'static str {
    match p {
        Phase::Prepare => "escapes and planes",
        Phase::Global => "global routing",
        Phase::Detail => "routing",
        Phase::Fine => "routing on the finer lattice",
        Phase::Settle => "settling what is open",
        Phase::Cleanup => "cleanup",
        Phase::Pairs => "pairs",
        Phase::Zones => "zones",
        Phase::Lengths => "lengths",
        Phase::Report => "report",
        _ => "routing",
    }
}

/// One run of guide's router under the hooks: its progress goes to the progress sink, a
/// cancellation seen there stops it, and `deadline` does. A panic in it is an error of the
/// backend, not of the host.
fn call(
    ad: &Adapted,
    options: &pcb::RouteOptions,
    deadline: Option<Instant>,
    hooks: &Hooks<'_>,
) -> Result<pcb::Routed, RouteError> {
    let cancel = AtomicBool::new(false);
    let heard = |g: &pcb::Progress| {
        if (hooks.cancelled)() {
            cancel.store(true, Ordering::Relaxed);
        }
        (hooks.progress)(
            g.connected as u64,
            (g.connections > 0).then_some(g.connections as u64),
            &format!("guide: {}, {} of {} nets, {} waiting", phase(g.phase), g.nets_routed, g.nets, g.queued),
        );
    };
    let mut control = RunControl::default();
    control.cancel = Some(&cancel);
    control.deadline = deadline;
    control.progress = Some(&heard);
    control.progress_interval = PROGRESS_INTERVAL;
    // cadlab refills the zones itself, so a stopped run does not wait for guide's fill.
    control.fill_on_stop = false;
    let run = std::panic::AssertUnwindSafe(|| pcb::route_from(&ad.design, &ad.earlier, options, &control));
    let routed = match std::panic::catch_unwind(run) {
        Ok(Ok(r)) => r,
        Ok(Err(pcb::RouteError::Item { item, error })) => {
            return Err(RouteError::Backend(format!("invalid geometry of {}: {error}", ad.item(item).0)));
        }
        Ok(Err(e)) => return Err(RouteError::Backend(e.to_string())),
        Err(e) => {
            let msg =
                e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()));
            return Err(RouteError::Backend(format!("guide panicked: {}", msg.unwrap_or_default())));
        }
    };
    if routed.stopped == Some(Stopped::Cancelled) || (hooks.cancelled)() {
        return Err(RouteError::Cancelled);
    }
    Ok(routed)
}

/// guide's tracks and vias as the board's (without IDs yet).
fn convert(ad: &Adapted, routed: &pcb::Routed) -> (Vec<Track>, Vec<Via>) {
    let name = |n: Option<NetId>| n.and_then(|n| ad.nets.get(n.index())).cloned();
    let layer = |l: LayerId| ad.layers.get(usize::from(l.0)).cloned().unwrap_or_default();
    let tracks = routed
        .tracks
        .iter()
        .map(|t| Track {
            id: ObjectId(0),
            layer: layer(t.layer),
            width: Nm(t.width),
            net: name(t.net),
            start: back(t.start),
            end: back(t.end),
            mid: t.mid.map(back),
            locked: false,
        })
        .collect();
    let vias = routed
        .vias
        .iter()
        .map(|v| Via {
            id: ObjectId(0),
            at: back(v.at),
            drill: Nm(v.drill),
            diameter: Nm(v.diameter),
            net: name(v.net),
            from: layer(v.from),
            to: layer(v.to),
            locked: false,
        })
        .collect();
    (tracks, vias)
}

/// Verification, as the native router's: cadlab's DRC on `p` with the new copper (IDs from
/// `p`'s allocator, in order); a piece of new copper holding a flagged item is taken out, a
/// net's whole after three rounds. Returns the verified board; `reasons` gets the first error
/// per net and `removed` counts the items taken out.
fn verify(
    p: &Project,
    tracks: &mut Vec<Track>,
    vias: &mut Vec<Via>,
    reasons: &mut BTreeMap<String, String>,
    removed: &mut usize,
) -> Project {
    let mut round = 0;
    loop {
        let mut proj = p.clone();
        for t in tracks.iter_mut() {
            t.id = proj.alloc_id();
            proj.board_mut().tracks.push(t.clone());
        }
        for v in vias.iter_mut() {
            v.id = proj.alloc_id();
            proj.board_mut().vias.push(v.clone());
        }
        let new_t: BTreeMap<u64, usize> = tracks.iter().enumerate().map(|(i, t)| (t.id.0, i)).collect();
        let new_v: BTreeMap<u64, usize> = vias.iter().enumerate().map(|(i, v)| (v.id.0, i)).collect();
        let mut bad: BTreeMap<New, String> = BTreeMap::new();
        for d in crate::drc::check_copper(&proj)
            .iter()
            .filter(|d| d.severity == crate::diag::Severity::Error && d.code != "drc.unrouted")
        {
            for s in &d.subjects {
                let ObjectRef::Item { kind, index } = s else { continue };
                let hit = match kind.as_str() {
                    "track" => new_t.get(index).map(|&i| New::Track(i)),
                    "via" => new_v.get(index).map(|&i| New::Via(i)),
                    _ => None,
                };
                if let Some(h) = hit {
                    bad.entry(h).or_insert_with(|| format!("{}: {}", d.code, d.message));
                }
            }
        }
        if bad.is_empty() {
            return proj;
        }
        round += 1;
        let piece = pieces(tracks, vias);
        let nt = tracks.len();
        let mut gone_pieces: BTreeSet<usize> = BTreeSet::new();
        let mut gone_nets: BTreeSet<String> = BTreeSet::new();
        for (&n, why) in &bad {
            let (pos, net) = match n {
                New::Track(i) => (i, tracks[i].net.clone()),
                New::Via(i) => (nt + i, vias[i].net.clone()),
            };
            gone_pieces.insert(piece[pos]);
            if let Some(net) = net {
                reasons.entry(net.clone()).or_insert_with(|| why.clone());
                if round >= 4 {
                    gone_nets.insert(net);
                }
            }
        }
        let gone = |i: usize, net: &Option<String>| {
            gone_pieces.contains(&piece[i]) || net.as_ref().is_some_and(|n| gone_nets.contains(n))
        };
        let keep_t: Vec<bool> = tracks.iter().enumerate().map(|(i, t)| !gone(i, &t.net)).collect();
        let keep_v: Vec<bool> = vias.iter().enumerate().map(|(i, v)| !gone(nt + i, &v.net)).collect();
        *removed += keep_t.iter().chain(&keep_v).filter(|k| !**k).count();
        let mut k = keep_t.iter();
        tracks.retain(|_| *k.next().expect("one flag per track"));
        let mut k = keep_v.iter();
        vias.retain(|_| *k.next().expect("one flag per via"));
    }
}

/// The nets a second run on the routed board is not for: after a run that ended, the open
/// nets whose own search found no way (`Routed::failed`), since that run had its time and
/// the board has no more room for them. After a run that a budget stopped, none. guide's list
/// is then the same nets as those with a connection whose cause is a search that failed,
/// which were left out before (420 and 421 connections open on the twelve corpus boards at
/// 60 s, one rule and the other, the first run with half the budget), but a search that
/// failed in a run that was cut is not the last word on the routed board: with those nets in
/// the second run too, 401 to 419 (tinytapeout-demo 39 to 41 against 46, cynthion 21 to 24
/// against 25), and the second run, which ended on its own with 9 to 26 s unused on five
/// boards, has the time for them (D95).
fn gave_up_on(ad: &Adapted, r: &pcb::Routed) -> BTreeSet<String> {
    match r.stopped {
        None => r.failed.iter().filter_map(|n| ad.nets.get(n.index()).cloned()).collect(),
        Some(_) => BTreeSet::new(),
    }
}

/// The nets guide's first run may route: those of the board's open connections, `open`, and
/// the nets with zones in scope, open or not. cadlab's fill of a pour joins pads that the
/// routes of other nets then cut off, and a net that the fill joins whole has no open
/// connection before routing: left out of the first run, its pads are found off their pour
/// on the board filled again, when guide is asked for all their stubs at once on a board
/// where nothing moves. lumenpnp-mobo's ground is such a net (its F.Cu pour joins every pad
/// of it, and guide plans a net that has a plane on an inner layer on that plane alone):
/// the second run was asked for some 220 stubs to the plane, and 5 to 10 pads of the LQFP
/// and the drivers were shut in by then. Asked in the first run, guide makes them while the
/// board is free: 0, 1 and 0 connections open over three orders of the nets at 60 s for 9,
/// 10 and 6, with 713 vias for 748 (D97).
fn first_nets(p: &Project, open: &BTreeSet<String>, wanted: &dyn Fn(&str) -> bool) -> BTreeSet<String> {
    let zoned = p.board().zones.iter().filter_map(|z| z.net.clone()).filter(|n| wanted(n));
    open.iter().cloned().chain(zoned).collect()
}

/// The nets that a run found apart on the board itself (`Cause::Apart`): what they have open
/// no run joins, and the runs after it are not for them.
fn apart_nets(ad: &Adapted, r: &pcb::Routed) -> BTreeSet<String> {
    r.open.iter().filter(|o| o.cause == Cause::Apart).filter_map(|o| ad.nets.get(o.net.index()).cloned()).collect()
}

/// What guide says of a run, for the progress sink.
fn said(r: &pcb::Routed) -> String {
    let gs = &r.stats;
    let of = |kind: CopperKind| r.via_kinds.iter().filter(|k| **k == kind).count();
    format!(
        "guide: {} of {} nets, {} got to, {} with no way of their own, {} moved aside, {} of {} connections on \
         its board, {} left open, {} rip-ups, {} nets to the finer pass, {} escape vias, {} stub vias to zones, \
         {} nets with zones in pieces, {} of {} pairs side by side, {} of {} lengths met, {} nodes{}",
        gs.routed,
        gs.nets,
        r.tried.len(),
        r.failed.len(),
        gs.shoved,
        gs.islands.1,
        gs.islands.0,
        r.open.len(),
        gs.rip_ups,
        gs.fine_pass,
        of(CopperKind::Escape),
        of(CopperKind::Stub),
        gs.planes_broken,
        gs.pairs.1,
        gs.pairs.0,
        gs.lengths.1,
        gs.lengths.0,
        gs.work,
        r.stopped.map_or(String::new(), |s| format!("; {s}")),
    )
}

/// Routes `scope` with guide. See the module documentation.
///
/// **Budgets.** The work budget is guide's (`RouteOptions::work_budget`, in search nodes, which
/// [`RouteStats::work`] reports): a run that spends it returns what it has, the same on any
/// machine. The time budget is a deadline for guide's first run at [`FIRST_SHARE`] of it, and
/// for the second at its end less what comes after a run, as measured on the first. Either
/// way the result is verified and counted as any other, `budget_exhausted` is set, and what
/// is left open says so. A run with a budget of either kind is not the run without one: guide
/// then routes for the most connections soonest.
///
/// **The first run's nets** are those of the board's open connections in scope and the nets
/// with zones in scope ([`first_nets`]): a net that cadlab's fill joins whole is cut by the
/// routes of the others.
///
/// **A second run.** guide plans on cadlab's fills of the zones, as they are before routing;
/// routes change them. After the run the zones are filled again around the new copper, and
/// guide is given the routed board and its fills once more: for the nets with zones that the
/// board still has open, less those whose own search found no way (`Routed::failed`), and
/// after a run that its deadline stopped for every net it has open. It is kept when it leaves
/// fewer connections open. Not after a run that the work budget stopped: nothing is left of
/// it.
///
/// **A last run that moves routes** (D106). In what time or work the second run leaves, guide
/// is given every net the board still has open and the routes of the runs before as routes of
/// their nets (`pcb::route_from`), not as fixed copper: a pad that the first run left shut in,
/// or that a pour no longer reaches, can then have a route moved for it. guide says which
/// nets' routes it took back (`Routed::taken_back`); what it returns stands in place of
/// those, and the others stay as they were. Kept when it leaves fewer connections open, so
/// that no board ends with more than the two runs before it left.
pub(crate) fn route(
    p: &Project,
    scope: &Scope,
    opts: &RouteOptions,
    hooks: &Hooks<'_>,
) -> Result<RouteResult, RouteError> {
    supported(scope)?;
    let start = Instant::now();
    let slots = super::routing_slots(p, &opts.layers)?;
    (hooks.progress)(0, None, "preparing");
    let items = geo::copper_items(p);
    let isl = geo::islands(&items);
    if let Scope::Nets(list) = scope {
        let known: BTreeSet<&str> = p
            .circuit()
            .nets
            .keys()
            .map(String::as_str)
            .chain(items.iter().filter_map(|it| it.net.as_deref()))
            .collect();
        if let Some(n) = list.iter().find(|n| !known.contains(n.as_str())) {
            return Err(RouteError::UnknownNet(n.clone()));
        }
    }
    let wanted = |net: &str| match scope {
        Scope::Nets(list) => list.iter().any(|n| n == net),
        _ => true,
    };
    // The plan: the board's open connections in scope, as the ratsnest has them.
    let planned: Vec<RatLine> = geo::ratsnest_from(&items, &isl).into_iter().filter(|l| wanted(&l.net)).collect();
    let nets: BTreeSet<String> = planned.iter().map(|l| l.net.clone()).collect();
    let ad = adapt(p, &items, &first_nets(p, &nets, &wanted), opts)?;
    let mut stats = RouteStats { grid: opts.grid.unwrap_or(ad.pitch), completion: 100.0, ..Default::default() };
    if planned.is_empty() {
        (hooks.progress)(1, Some(1), "done");
        return Ok(RouteResult {
            tracks: vec![],
            vias: vec![],
            connections: vec![],
            stats,
            removed_tracks: vec![],
            removed_vias: vec![],
            rerouted: vec![],
            pairs: vec![],
            notes: ad.notes,
        });
    }
    if (hooks.cancelled)() {
        return Err(RouteError::Cancelled);
    }

    // The run, under the hooks and the budgets.
    let deadline = opts.budget.map(|b| start + b.mul_f64(FIRST_SHARE));
    let mut gopts = options(p, opts, &slots);
    gopts.work_budget = opts.work.left();
    let routed = call(&ad, &gopts, deadline, hooks)?;
    gopts.whole = false;
    opts.work.charge(routed.stats.work);
    (hooks.progress)(0, None, &said(&routed));
    let mut stopped = routed.stopped;
    let mut opens: Vec<Open> = explain_all(&ad, &routed, true);
    let mut gave_up = gave_up_on(&ad, &routed);
    // Told by the first run alone: in a later one the routes of the runs before are copper of
    // the board, and what they keep apart another order of the nets may join.
    let apart = apart_nets(&ad, &routed);
    let mut escapes: BTreeSet<(i64, i64)> = routed
        .vias
        .iter()
        .zip(&routed.via_kinds)
        .filter(|(_, k)| **k == CopperKind::Escape)
        .map(|(v, _)| (v.at.x, v.at.y))
        .collect();
    let (mut tracks, mut vias) = convert(&ad, &routed);
    (hooks.progress)(0, None, "verifying");
    let ran = Instant::now();
    let mut drc_reason: BTreeMap<String, String> = BTreeMap::new();
    let mut proj = verify(p, &mut tracks, &mut vias, &mut drc_reason, &mut stats.drc_removed);
    let mut fitems = geo::copper_items(&proj);
    let mut fisl = geo::islands(&fitems);
    let mut work = routed.stats.work;
    let mut calls = 1;
    // What a run needs after it, as the first did: the verification and the refill.
    let after = ran.elapsed();

    // A second run: the nets with zones that cadlab's fill around the routes leaves open, less
    // those a run that ended found no way for, and after a run that its deadline stopped every
    // net left open. And a last one, which takes the routes back to move them.
    let poured: BTreeSet<&str> = p.board().zones.iter().filter_map(|z| z.net.as_deref()).collect();
    let open = |items: &[CopperItem], isl: &[usize]| -> Vec<RatLine> {
        geo::ratsnest_from(items, isl).into_iter().filter(|l| wanted(&l.net)).collect()
    };
    // Its deadline: the end of the budget less `AFTER_SHARE` times what the first run needed
    // after it and the report, and no sooner than the first run's.
    let second = opts
        .budget
        .zip(deadline)
        .map(|(b, d)| (start + b).checked_sub(after.mul_f64(AFTER_SHARE) + REPORT).map_or(d, |t| t.max(d)));
    for round in 1..=POUR_ROUNDS + MOVE_AGAIN {
        // The last runs are those that move routes: the first of them has what time the run
        // before it leaves, and every net left open, those that a run on the fixed board found
        // no way for among them. And for as long as there is time and something open, another
        // has the same in another order of the nets (D108): rip-up ends where its order of the
        // nets took it, and a run that ends on its own with one connection open has said all
        // it has to say of that order.
        let moves = round >= POUR_ROUNDS;
        if round > POUR_ROUNDS {
            gopts.seed = Some(opts.seed.wrapping_add((round - POUR_ROUNDS) as u64 * AGAIN_SEED));
        }
        let least = if moves { MOVE_MIN } else { SECOND_MIN };
        if stopped == Some(Stopped::WorkBudget) || second.is_some_and(|d| Instant::now() + least >= d) {
            break;
        }
        let left = open(&fitems, &fisl);
        let again: BTreeSet<String> = left
            .iter()
            .filter(|l| !apart.contains(&l.net))
            .filter(|l| {
                // A net with zones has its run on the filled board whatever the first run said
                // of it: cadlab's fill is not the fill guide planned on, and a run that ended
                // early has left the time (sweep-v2.2: 3 open in four orders for 4, D110).
                let zoned = poured.contains(l.net.as_str());
                moves || zoned || (stopped.is_some() && !gave_up.contains(&l.net))
            })
            .map(|l| l.net.clone())
            .collect();
        if again.is_empty() {
            if left.is_empty() {
                break;
            }
            continue;
        }
        (hooks.progress)(
            0,
            None,
            &format!("guide: run {}, {} net(s) open on the filled board", round + 1, again.len()),
        );
        let mut ad = adapt(&proj, &fitems, &again, opts)?;
        // The routes of the runs before are this run's to move (D106): a pad that the fill
        // no longer reaches, or one that the first run left shut in, can then have a route
        // moved for it, where on a board with them fixed it had none.
        let handed = if moves {
            let mine: BTreeSet<ObjectId> = tracks.iter().map(|t| t.id).chain(vias.iter().map(|v| v.id)).collect();
            ad.take_back(&mine)
        } else {
            BTreeSet::new()
        };
        gopts.work_budget = opts.work.left();
        let more = call(&ad, &gopts, second, hooks)?;
        opts.work.charge(more.stats.work);
        work += more.stats.work;
        calls += 1;
        (hooks.progress)(0, None, &said(&more));
        let (t, v) = convert(&ad, &more);
        if t.is_empty() && v.is_empty() {
            continue;
        }
        // What guide took back is in its result as it is now; what it left where it was
        // (`Routed::taken_back` has the nets it took) stays as the first run had it.
        let moved: BTreeSet<&str> =
            more.taken_back.iter().filter_map(|n| ad.nets.get(n.index())).map(String::as_str).collect();
        let stays = |id: &ObjectId, net: &Option<String>| {
            !(handed.contains(id) && net.as_deref().is_some_and(|n| moved.contains(n)))
        };
        let (mut tracks2, mut vias2) = (tracks.clone(), vias.clone());
        tracks2.retain(|t| stays(&t.id, &t.net));
        vias2.retain(|v| stays(&v.id, &v.net));
        tracks2.extend(t);
        vias2.extend(v);
        let (mut reasons, mut removed) = (drc_reason.clone(), stats.drc_removed);
        let proj2 = verify(p, &mut tracks2, &mut vias2, &mut reasons, &mut removed);
        let items2 = geo::copper_items(&proj2);
        let isl2 = geo::islands(&items2);
        // A run that leaves no less open is as if it had not been: the board stays as the
        // runs before left it.
        if open(&items2, &isl2).len() >= left.len() {
            continue;
        }
        (tracks, vias, proj, fitems, fisl) = (tracks2, vias2, proj2, items2, isl2);
        (drc_reason, stats.drc_removed) = (reasons, removed);
        stopped = stopped.or(more.stopped);
        gave_up.extend(gave_up_on(&ad, &more));
        escapes.extend(
            more.vias
                .iter()
                .zip(&more.via_kinds)
                .filter(|(_, k)| **k == CopperKind::Escape)
                .map(|(v, _)| (v.at.x, v.at.y)),
        );
        // What this run says of its nets replaces what the first said of them.
        opens.retain(|o| !again.contains(&o.net) && !moved.contains(o.net.as_str()));
        opens.extend(explain_all(&ad, &more, false));
    }

    // The stubs that the fill makes needless (D108): guide gives every pad of a net with a
    // plane a stub through a via to it, and plans on no pour of an outer layer, which routes
    // cut. Where cadlab's fill around the routed board joins such a pad to its net all the
    // same, the stub and its via are taken out again: all of them at once, the board filled
    // again, and those put back whose pad the fill then leaves apart. Kept if the board has no
    // more open for it. After the last run, as a run for a net with a plane gives every pad
    // that guide's own fill does not join its stub again.
    let mut pruned = 0;
    let prune = stopped != Some(Stopped::WorkBudget)
        && opts.budget.is_none_or(|b| Instant::now() + after.mul_f64(PRUNE_SHARE) + REPORT < start + b);
    if prune && !poured.is_empty() {
        let piece = pieces(&tracks, &vias);
        let nt = tracks.len();
        let stubs = stub_pieces(&tracks, &vias, &piece, &poured, &escapes);
        if !stubs.is_empty() {
            let before = open(&fitems, &fisl).len();
            let without = |needed: &BTreeSet<usize>| {
                let gone = |i: usize| stubs.contains(&piece[i]) && !needed.contains(&piece[i]);
                let t: Vec<Track> =
                    tracks.iter().enumerate().filter(|(i, _)| !gone(*i)).map(|(_, t)| t.clone()).collect();
                let v: Vec<Via> =
                    vias.iter().enumerate().filter(|(i, _)| !gone(nt + *i)).map(|(_, v)| v.clone()).collect();
                (t, v)
            };
            // Copper taken out breaks no rule that the board kept: no check, the fill alone.
            let placed = |t: &mut [Track], v: &mut [Via]| {
                let mut q = p.clone();
                for t in t.iter_mut() {
                    t.id = q.alloc_id();
                    q.board_mut().tracks.push(t.clone());
                }
                for v in v.iter_mut() {
                    v.id = q.alloc_id();
                    q.board_mut().vias.push(v.clone());
                }
                q
            };
            let (mut t2, mut v2) = without(&BTreeSet::new());
            let mut proj2 = placed(&mut t2, &mut v2);
            let mut items2 = geo::copper_items(&proj2);
            let mut isl2 = geo::islands(&items2);
            // By net, its largest island, and by net the items with their islands.
            let mut members: BTreeMap<(&str, usize), usize> = BTreeMap::new();
            let mut of_net: BTreeMap<&str, Vec<(Point, usize)>> = BTreeMap::new();
            for (it, &k) in items2.iter().zip(&isl2) {
                if let Some(n) = it.net.as_deref().filter(|n| poured.contains(n)) {
                    *members.entry((n, k)).or_default() += 1;
                    of_net.entry(n).or_default().push((it.anchor, k));
                }
            }
            let mut main: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
            for (&(n, k), &c) in &members {
                let m = main.entry(n).or_insert((c, k));
                if c > m.0 {
                    *m = (c, k);
                }
            }
            // A stub is needed if the item of its net nearest one of its ends, the pad it was
            // for, is no longer on that island.
            let mut needed: BTreeSet<usize> = BTreeSet::new();
            for (i, t) in tracks.iter().enumerate().filter(|(i, _)| stubs.contains(&piece[*i])) {
                let Some(n) = t.net.as_deref() else { continue };
                let near = |at: Point| {
                    of_net.get(n).and_then(|items| {
                        items
                            .iter()
                            .map(|(q, k)| {
                                let (dx, dy) = ((q.x.0 - at.x.0) as i128, (q.y.0 - at.y.0) as i128);
                                (dx * dx + dy * dy, *k)
                            })
                            .min()
                    })
                };
                let joined = [t.start, t.end]
                    .into_iter()
                    .all(|e| near(e).is_some_and(|(_, k)| main.get(n).is_some_and(|m| m.1 == k)));
                if !joined {
                    needed.insert(piece[i]);
                }
            }
            if !needed.is_empty() && needed.len() < stubs.len() {
                (t2, v2) = without(&needed);
                proj2 = placed(&mut t2, &mut v2);
                items2 = geo::copper_items(&proj2);
                isl2 = geo::islands(&items2);
            }
            if needed.len() < stubs.len() && open(&items2, &isl2).len() <= before {
                pruned = vias.len() - v2.len();
                (tracks, vias, proj, fitems, fisl) = (t2, v2, proj2, items2, isl2);
                (hooks.progress)(
                    0,
                    None,
                    &format!("guide: {pruned} stub(s) to a plane taken out, their pads on a pour"),
                );
            }
        }
    }

    // The report, from the board with its zones filled again around the new copper (D80).
    (hooks.progress)(0, None, "reporting");
    let index = |items: &[CopperItem], isl: &[usize]| {
        let mut m: BTreeMap<String, Vec<(Point, usize)>> = BTreeMap::new();
        for (i, it) in items.iter().enumerate() {
            m.entry(it.item.to_string()).or_default().push((it.anchor, isl[i]));
        }
        m
    };
    // Several pads can share a label: an end is the item of its label nearest to it.
    let island_at = |m: &BTreeMap<String, Vec<(Point, usize)>>, label: &str, at: Point| -> Option<usize> {
        m.get(label)?
            .iter()
            .min_by_key(|(q, _)| {
                let (dx, dy) = ((q.x.0 - at.x.0) as i128, (q.y.0 - at.y.0) as i128);
                dx * dx + dy * dy
            })
            .map(|(_, k)| *k)
    };
    let (before, after) = (index(&items, &isl), index(&fitems, &fisl));
    let mut reports: Vec<ConnectionReport> = Vec::new();
    let report = |net: &str, from: &str, to: &str, status: ConnStatus| ConnectionReport {
        net: net.to_string(),
        from: from.to_string(),
        to: to.to_string(),
        status,
        reason: None,
        at: None,
        layer: None,
        hints: vec![],
        subjects: vec![],
    };
    // Routed: guide joins a net's islands as a tree of its own, not along the planned lines.
    // Each island of the board after routing that holds `k` of the islands planned for stands
    // for `k - 1` connections made, reported between one end of each.
    for net in &nets {
        // (island after, island before) -> the first planned end there.
        let mut ends: BTreeMap<usize, Vec<(usize, &str)>> = BTreeMap::new();
        for l in planned.iter().filter(|l| &l.net == net) {
            for (label, at) in [(&l.from, l.from_at), (&l.to, l.to_at)] {
                let (Some(b), Some(a)) = (island_at(&before, label, at), island_at(&after, label, at)) else {
                    continue;
                };
                let group = ends.entry(a).or_default();
                if !group.iter().any(|(k, _)| *k == b) {
                    group.push((b, label.as_str()));
                }
            }
        }
        for group in ends.values() {
            for w in group.windows(2) {
                reports.push(report(net, w[0].1, w[1].1, ConnStatus::Routed));
            }
        }
    }
    // Failed: what the board has open for the nets in scope, pads the routing cut off from a
    // pour included, each with what guide said of the connection nearest to it: the one that
    // ends at one of its pads, else one whose pad is in the copper at one of its ends, else,
    // for a cause that is the net's and not a pad's, any of the net.
    let mut told = vec![false; opens.len()];
    for l in open(&fitems, &fisl) {
        let mut rep = report(&l.net, &l.from, &l.to, ConnStatus::Failed);
        rep.at = Some(l.from_at);
        rep.subjects = vec![ObjectRef::Net(l.net.clone())];
        let ends = [island_at(&after, &l.from, l.from_at), island_at(&after, &l.to, l.to_at)];
        let said = opens
            .iter()
            .enumerate()
            .filter(|(_, o)| o.net == l.net)
            .filter_map(|(i, o)| {
                let near = if o.to == l.from || o.to == l.to {
                    3
                } else if island_at(&after, &o.to, o.at).is_some_and(|k| ends.contains(&Some(k))) {
                    2
                } else {
                    usize::from(o.from == l.from || o.from == l.to)
                };
                (near > 0 || !o.of_pad).then_some((near, !told[i], std::cmp::Reverse(i)))
            })
            .max();
        rep.hints = vec![
            format!("route net {} with the native router (`route.nets {}`), which can move other nets", l.net, l.net),
            "move the parts apart or allow more layers".to_string(),
        ];
        if let Some(why) = drc_reason.get(&l.net) {
            rep.reason = Some(format!("guide's routing of net {} broke a rule and was taken out ({why})", l.net));
        } else if let Some((_, _, std::cmp::Reverse(i))) = said {
            let o = &opens[i];
            told[i] = true;
            rep.reason = Some(o.reason.clone());
            rep.at = Some(o.at);
            rep.layer = o.layer.clone();
            rep.hints = o.hints.clone();
            rep.subjects = o.subjects.clone();
        } else if poured.contains(l.net.as_str()) {
            rep.reason = Some(format!(
                "{} is not joined to the pour of net {}: the fill around the routed board does not reach it",
                l.from, l.net
            ));
        } else {
            rep.reason = Some(format!("guide left net {} in pieces without reporting it open", l.net));
        }
        reports.push(rep);
    }

    // Pairs, measured on the routed board.
    let c = proj.circuit();
    let mut pair_reports = Vec::new();
    for d in &ad.design.pairs {
        let (Some(pn), Some(nn)) = (ad.nets.get(d.positive.index()), ad.nets.get(d.negative.index())) else { continue };
        let Some((pair, dp)) = c.diffpair_of(pn) else { continue };
        let cp = crate::lengths::coupling(&proj, dp, Nm(d.gap));
        let of = |status: ConnStatus| {
            reports.iter().filter(|r| (&r.net == pn || &r.net == nn) && r.status == status).count()
        };
        let (made, open) = (of(ConnStatus::Routed), of(ConnStatus::Failed));
        let coupled = cp.coupled_p.min(cp.coupled_n);
        let mut on: Vec<String> = tracks
            .iter()
            .filter(|t| t.net.as_ref() == Some(pn) || t.net.as_ref() == Some(nn))
            .map(|t| t.layer.clone())
            .collect();
        on.sort();
        on.dedup();
        pair_reports.push(PairReport {
            name: pair.to_string(),
            p: pn.clone(),
            n: nn.clone(),
            status: match (open, coupled.0 > 0) {
                (0, true) => PairStatus::Routed,
                (_, true) => PairStatus::Partial,
                _ if made + open == 0 => PairStatus::Nothing,
                _ => PairStatus::Failed,
            },
            width: Nm(d.width),
            gap: Nm(d.gap),
            connections: made + open,
            routed: made,
            layers: on,
            length_p: cp.length_p.total,
            length_n: cp.length_n.total,
            skew: cp.skew,
            skew_bumps: 0,
            coupled,
            uncoupled: cp.uncoupled,
            left: 0,
            failures: reports
                .iter()
                .filter(|r| (&r.net == pn || &r.net == nn) && r.status == ConnStatus::Failed)
                .cloned()
                .collect(),
        });
    }

    stats.connections = reports.len();
    stats.routed = reports.iter().filter(|r| r.status == ConnStatus::Routed).count();
    stats.failed = stats.connections - stats.routed;
    stats.completion = if stats.connections == 0 {
        100.0
    } else {
        (stats.routed as f64 * 1000.0 / stats.connections as f64).round() / 10.0
    };
    stats.tracks = tracks.len();
    stats.vias = vias.len();
    stats.length = tracks_length(&tracks);
    stats.iterations = calls;
    stats.vias_removed = routed.stats.before_cleanup.0.saturating_sub(routed.stats.after_cleanup.0) + pruned;
    stats.work = work;
    stats.budget_exhausted = stopped.is_some();
    stats.fanout_vias = vias.iter().filter(|v| escapes.contains(&(v.at.x.0, v.at.y.0))).count();
    (hooks.progress)(1, Some(1), "done");
    Ok(RouteResult {
        tracks,
        vias,
        connections: reports,
        stats,
        removed_tracks: vec![],
        removed_vias: vec![],
        rerouted: vec![],
        pairs: pair_reports,
        notes: ad.notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kicad_import::{BoardImportOptions, import};
    use crate::model::circuit::{DiffPair as ModelPair, LengthGroup, NetClass as ModelClass};

    /// A 30 × 20 mm two-layer board at (100, 50)–(130, 70): R1 (round-rectangle and rectangle
    /// pads), J1 (a round through-hole pad, an oval one with a slot and four pads of a pair),
    /// an unplated hole, a track with an arc and a via, a GND zone on the back and a keep-out
    /// for tracks.
    const BOARD: &str = r#"(kicad_pcb (version 20221018) (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
  (net 0 "")
  (net 1 "GND")
  (net 2 "SIG")
  (net 3 "DP")
  (net 4 "DM")
  (footprint "R:R" (layer "F.Cu") (at 110 60)
    (property "Reference" "R1") (property "Value" "10k")
    (pad "1" smd roundrect (at -0.5 0) (size 0.6 0.5) (layers "F.Cu" "F.Mask") (roundrect_rratio 0.25) (net 2 "SIG"))
    (pad "2" smd rect (at 0.5 0) (size 0.6 0.5) (layers "F.Cu" "F.Mask") (net 1 "GND")))
  (footprint "J:J" (layer "F.Cu") (at 120 60)
    (property "Reference" "J1") (property "Value" "J")
    (pad "1" thru_hole circle (at 0 0) (size 1.6 1.6) (drill 0.8) (layers "*.Cu" "*.Mask") (net 2 "SIG"))
    (pad "2" thru_hole oval (at 3 0) (size 1 1.6) (drill oval 0.6 1.2) (layers "*.Cu" "*.Mask") (net 1 "GND"))
    (pad "3" smd rect (at 0 4) (size 0.5 0.5) (layers "F.Cu" "F.Mask") (net 3 "DP"))
    (pad "4" smd rect (at 1 4) (size 0.5 0.5) (layers "F.Cu" "F.Mask") (net 4 "DM"))
    (pad "5" smd rect (at 0 7) (size 0.5 0.5) (layers "F.Cu" "F.Mask") (net 3 "DP"))
    (pad "6" smd rect (at 1 7) (size 0.5 0.5) (layers "F.Cu" "F.Mask") (net 4 "DM")))
  (footprint "H:H" (layer "F.Cu") (at 104 54)
    (property "Reference" "H1") (property "Value" "H")
    (pad "" np_thru_hole circle (at 0 0) (size 2 2) (drill 2) (layers "*.Cu" "*.Mask")))
  (gr_rect (start 100 50) (end 130 70) (stroke (width 0.1)) (fill none) (layer "Edge.Cuts"))
  (segment (start 109.5 60) (end 109.5 64) (width 0.25) (layer "F.Cu") (net 2))
  (arc (start 109.5 64) (mid 110.5 65) (end 111.5 64) (width 0.25) (layer "F.Cu") (net 2))
  (via (at 111.5 64) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 2))
  (zone (net 1) (net_name "GND") (layers "B.Cu") (name "gnd") (hatch edge 0.5)
    (connect_pads yes (clearance 0.3)) (min_thickness 0.25)
    (fill yes (thermal_gap 0.4) (thermal_bridge_width 0.35))
    (polygon (pts (xy 100 50) (xy 130 50) (xy 130 70) (xy 100 70))))
  (zone (net 0) (net_name "") (layers "F.Cu") (name "wall") (hatch edge 0.5)
    (keepout (tracks not_allowed) (vias allowed) (pads allowed) (copperpour allowed) (footprints allowed))
    (polygon (pts (xy 126 51) (xy 129 51) (xy 129 54) (xy 126 54))))
)"#;

    fn project() -> Project {
        let mut p = Project::new("t");
        import(&mut p, BOARD, &BoardImportOptions { file_name: "t.kicad_pcb".into(), ..Default::default() })
            .expect("import");
        p
    }

    fn adapted(p: &Project, nets: &[&str]) -> Adapted {
        let route: BTreeSet<String> = nets.iter().map(|n| (*n).to_string()).collect();
        adapt(p, &geo::copper_items(p), &route, &RouteOptions::default()).expect("adapt")
    }

    /// Board millimetres (origin at the outline's lower-left corner, Y up) as a guide point.
    fn at(x: f64, y: f64) -> GPoint {
        GPoint::new((x * 1e6).round() as i64, (y * 1e6).round() as i64)
    }

    #[test]
    fn pads_keep_their_exact_shapes_layers_and_holes() {
        let p = project();
        let ad = adapted(&p, &["SIG"]);
        let d = &ad.design;
        assert_eq!(ad.layers, ["F.Cu", "B.Cu"]);
        let net = |n: &str| Some(NetId(ad.nets.iter().position(|x| x == n).expect("net") as u32));
        let (top, both) = (LayerSet::EMPTY.with(LayerId(0)), LayerSet::first(2));
        let has = |pad: gdrc::Pad| assert!(d.board.pads.contains(&pad), "{pad:?} not in {:#?}", d.board.pads);
        // Round rectangle: the rectangle between the corner centers grown by the radius, exact.
        let core = [at(9.325, 9.875), at(9.675, 9.875), at(9.675, 10.125), at(9.325, 10.125)];
        has(gdrc::Pad {
            shape: Shape::rounded_polygon(&core, &[], 125_000).unwrap(),
            layers: top,
            hole: None,
            net: net("SIG"),
            clearance: None,
            stack: Vec::new(),
        });
        // Round through-hole pad: a circle on every layer, with its hole.
        has(gdrc::Pad {
            shape: Shape::circle(at(20.0, 10.0), 1_600_000).unwrap(),
            layers: both,
            hole: Some(Shape::circle(at(20.0, 10.0), 800_000).unwrap()),
            net: net("SIG"),
            clearance: None,
            stack: Vec::new(),
        });
        // Oval pad with a slot: both are segments grown by a radius.
        has(gdrc::Pad {
            shape: Shape::track(at(23.0, 9.7), at(23.0, 10.3), 1_000_000).unwrap(),
            layers: both,
            hole: Some(Shape::track(at(23.0, 9.7), at(23.0, 10.3), 600_000).unwrap()),
            net: net("GND"),
            clearance: None,
            stack: Vec::new(),
        });
        // An unplated hole with no hole clearance in the rules: copper of no net on every
        // layer, so that routes keep the copper clearance from it as the native router's do.
        let hole = Shape::circle(at(4.0, 16.0), 2_000_000).unwrap();
        has(gdrc::Pad { hole: Some(hole.clone()), ..gdrc::Pad::new(hole.clone(), both, None) });
        assert_eq!(d.zones.pads.len(), 8, "one zone connection per pad with copper");
        assert_eq!(ad.pads.len(), d.board.pads.len());
        assert_eq!(ad.pads[..2], ["J1.1", "J1.2"]);
        assert_eq!(ad.pads.last().map(String::as_str), Some("H1"));
        // With a hole clearance it is a hole only.
        let mut q = p.clone();
        q.board_mut().rules.hole_clearance = Nm(250_000);
        let ad = adapted(&q, &["SIG"]);
        assert_eq!(ad.design.rules.spec().hole_clearance, 250_000);
        let only_hole = gdrc::Pad { hole: Some(hole.clone()), ..gdrc::Pad::new(hole, LayerSet::EMPTY, None) };
        assert!(ad.design.board.pads.contains(&only_hole));
    }

    #[test]
    fn copper_rules_zones_and_keepouts_are_carried_over() {
        let mut p = project();
        p.circuit_mut().netclasses.insert(
            "wide".into(),
            ModelClass { clearance: Some(Nm(300_000)), track_width: Some(Nm(400_000)), ..Default::default() },
        );
        p.circuit_mut().nets.get_mut("SIG").expect("net").class = Some("wide".into());
        p.circuit_mut().diffpairs.insert(
            "D".into(),
            ModelPair {
                p: "DP".into(),
                n: "DM".into(),
                class: None,
                max_skew: Some(Nm(100_000)),
                max_uncoupled: None,
                from: None,
                to: None,
                max_stub: None,
            },
        );
        p.circuit_mut().length_groups.insert(
            "G".into(),
            LengthGroup {
                members: vec!["D".into(), "SIG".into()],
                target: Some(Nm(20_000_000)),
                tolerance: Nm(500_000),
                from: None,
                to: None,
                max_stub: None,
            },
        );
        p.board_mut().keepouts[0].allow_nets = vec!["SIG".into()];
        let r = p.board().rules.clone();
        let ad = adapted(&p, &["SIG", "DP", "DM"]);
        let d = &ad.design;
        let id = |n: &str| NetId(ad.nets.iter().position(|x| x == n).expect("net") as u32);
        let top = LayerId(0);
        // Existing copper: the track, the arc with its middle, the via through both layers.
        assert_eq!(d.board.tracks.len(), 2);
        assert_eq!(
            d.board.tracks[0],
            gdrc::Track {
                start: at(9.5, 10.0),
                end: at(9.5, 6.0),
                mid: None,
                width: 250_000,
                layer: top,
                net: Some(id("SIG"))
            }
        );
        assert_eq!(d.board.tracks[1].mid, Some(at(10.5, 5.0)));
        assert_eq!(
            d.board.vias,
            [gdrc::Via {
                at: at(11.5, 6.0),
                drill: 300_000,
                diameter: 600_000,
                from: top,
                to: LayerId(1),
                net: Some(id("SIG"))
            }]
        );
        // Rules: the class's clearance for its nets, the board's for the others.
        assert_eq!(d.rules.clearance(Some(id("SIG")), top), 300_000);
        assert_eq!(d.rules.clearance(Some(id("GND")), top), r.clearance.0);
        assert_eq!(d.rules.clearance_between(Some(id("SIG")), Some(id("GND")), top), 300_000);
        assert_eq!(d.edge_clearance, r.copper_to_edge.0);
        assert_eq!(d.outline.len(), 4, "the rectangle's four edges");
        assert!(d.rules.spec().areas.is_empty());
        // Nets: only those asked for are routed, each at its class's width and the rules' via.
        let spec = |n: &str| d.nets[id(n).index()];
        assert!(spec("SIG").route && spec("DP").route && !spec("GND").route);
        assert_eq!(spec("SIG").track_width, 400_000);
        assert_eq!(spec("DP").track_width, r.track_width.0);
        assert_eq!(spec("DP").via, Some(ViaSize { diameter: r.via_diameter.0, drill: r.via_drill.0 }));
        assert_eq!(ad.pitch, Nm((r.track_width.0 + r.clearance.0) / 2));
        // The pair with its skew, and the group: both of the pair's nets and SIG, at the target.
        assert_eq!(
            d.pairs,
            [DiffPair { positive: id("DP"), negative: id("DM"), width: r.track_width.0, gap: r.clearance.0 }]
        );
        assert_eq!(
            d.lengths,
            [
                LengthConstraint { nets: vec![id("DP"), id("DM")], max_skew: Some(100_000), ..Default::default() },
                LengthConstraint {
                    nets: vec![id("DP"), id("DM"), id("SIG")],
                    min: Some(19_500_000),
                    max: Some(20_500_000),
                    target: Some(20_000_000),
                    max_skew: None
                },
            ]
        );
        // The zone's settings, and cadlab's fill of it for guide to plan on: on the back, around
        // the through-hole pads and the via of SIG.
        assert!(d.board.zones.is_empty());
        let [z] = &d.zones.zones[..] else { panic!("one zone: {:?}", d.zones.zones) };
        assert_eq!((z.net, z.layers, z.clearance), (Some(id("GND")), LayerSet::EMPTY.with(LayerId(1)), 300_000));
        assert_eq!((z.min_width, z.thermal_gap, z.spoke_width), (250_000, 400_000, 350_000));
        assert_eq!((z.outline.len(), z.spokes, z.plane), (4, 4, None));
        let fills = geo::copper_items(&p).iter().filter(|it| matches!(it.item, ItemRef::Zone(..))).count();
        assert!(fills > 0);
        assert_eq!(d.zones.fills.len(), fills);
        assert!(d.zones.fills.iter().all(|f| f.zone == 0 && f.layer == LayerId(1)));
        assert!(d.zones.fills.iter().any(|f| f.shape.rings().is_some_and(|r| r.len() > 1)), "holes around the pads");
        // The keep-out with the net it excepts, and no other: no via in a pad is guide's own
        // rule now (`vias_in_pads`), not a keep-out over every pad.
        let [k] = &d.board.keepouts[..] else { panic!("one keep-out: {:?}", d.board.keepouts) };
        assert!(k.tracks && !k.vias && k.layers == LayerSet::EMPTY.with(top));
        assert_eq!(k.except, [id("SIG")]);
        assert_eq!(ad.keepouts, ["wall"]);
        assert_eq!(ad.notes, []);
        assert_eq!((ad.tracks.len(), ad.vias.len()), (2, 1));
    }

    #[test]
    fn routes_and_reports_from_the_board() {
        let p = project();
        let r = route(&p, &Scope::Nets(vec!["SIG".into()]), &RouteOptions::default(), &Hooks::none()).expect("route");
        assert_eq!((r.stats.connections, r.stats.routed, r.stats.failed), (1, 1, 0), "{:?}", r.connections);
        assert!(!r.tracks.is_empty() && r.tracks.iter().all(|t| t.net.as_deref() == Some("SIG")));
        let mut q = p.clone();
        q.board_mut().tracks.extend(r.tracks.iter().cloned());
        q.board_mut().vias.extend(r.vias.iter().cloned());
        assert!(geo::ratsnest(&q).iter().all(|l| l.net != "SIG"));
        // One connection is the native router's.
        let conn = Scope::Connection { from: "R1.1".into(), to: "J1.1".into() };
        match route(&p, &conn, &RouteOptions::default(), &Hooks::none()) {
            Err(RouteError::Unsupported { option, .. }) => assert_eq!(option, "router"),
            other => panic!("{other:?}"),
        }
        let layers = RouteOptions { layers: vec!["In1.Cu".into()], ..Default::default() };
        assert_eq!(route(&p, &Scope::All, &layers, &Hooks::none()), Err(RouteError::InvalidLayer("In1.Cu".into())));
        assert_eq!(
            route(&p, &Scope::Nets(vec!["NOPE".into()]), &RouteOptions::default(), &Hooks::none()),
            Err(RouteError::UnknownNet("NOPE".into()))
        );
    }

    /// An exposed pad with a thermal via pad of the same number at its center (the RP2040 of
    /// buspirate5-rev10): each copper item goes over with its own outline. `board::exact` used
    /// to give both the first pad's, so guide saw a 0.6 mm circle for the 3 mm pad and routed
    /// other nets across it.
    #[test]
    fn pads_sharing_a_number_and_a_position_keep_their_own_shapes() {
        let text = BOARD.replace(
            "(footprint \"H:H\"",
            r#"(footprint "U:U" (layer "F.Cu") (at 115 56)
    (property "Reference" "U1") (property "Value" "U")
    (pad "3" thru_hole circle (at 0 0) (size 0.6 0.6) (drill 0.3) (layers "*.Cu") (net 1 "GND"))
    (pad "3" smd rect (at 0 0) (size 3 3) (layers "F.Cu" "F.Mask") (net 1 "GND")))
  (footprint "H:H""#,
        );
        let mut p = Project::new("t");
        import(&mut p, &text, &BoardImportOptions { file_name: "t.kicad_pcb".into(), ..Default::default() })
            .expect("import");
        let ad = adapted(&p, &["SIG"]);
        let (top, both) = (LayerSet::EMPTY.with(LayerId(0)), LayerSet::first(2));
        let at_center = |pad: &&gdrc::Pad| {
            let b = pad.shape.bbox();
            (b.min().x + b.max().x, b.min().y + b.max().y) == (30_000_000, 28_000_000)
        };
        let mine: Vec<&gdrc::Pad> = ad.design.board.pads.iter().filter(at_center).collect();
        let size = |pad: &gdrc::Pad| pad.shape.bbox().max().x - pad.shape.bbox().min().x;
        let mut seen: Vec<(i64, LayerSet, bool)> =
            mine.iter().map(|pad| (size(pad), pad.layers, pad.hole.is_some())).collect();
        seen.sort_by_key(|s| s.0);
        assert_eq!(seen, [(600_000, both, true), (3_000_000, top, false)]);
    }

    /// The pads of net DP (J1.3, J1.5) and DM in a keep-out for tracks and vias on both layers:
    /// what the board leaves open is reported with guide's cause, the pad shut in and the
    /// keep-out that does it. With DP excepted from the keep-out (D76) it is routed.
    #[test]
    fn an_open_connection_says_what_shuts_its_pad_in() {
        let mut p = project();
        let mut k = p.board().keepouts[0].clone();
        k.id = p.alloc_id();
        k.name = "box".into();
        k.layers = Vec::new();
        k.no_vias = true;
        let q = |x: i64, y: i64| Point::new(Nm(x * 1_000_000), Nm(y * 1_000_000));
        k.outline = vec![q(19, 2), q(22, 2), q(22, 4), q(19, 4)];
        p.board_mut().keepouts.push(k);
        let dp = Scope::Nets(vec!["DP".into()]);
        let r = route(&p, &dp, &RouteOptions::default(), &Hooks::none()).expect("route");
        assert_eq!(
            (r.stats.connections, r.stats.failed, r.stats.budget_exhausted),
            (1, 1, false),
            "{:?}",
            r.connections
        );
        let [c] = &r.connections[..] else { panic!("{:?}", r.connections) };
        assert_eq!((c.net.as_str(), c.status), ("DP", ConnStatus::Failed));
        let reason = c.reason.as_deref().unwrap_or_default();
        assert!(reason.contains("J1.5 is shut in by keep-out `box`"), "{reason}");
        assert!(c.subjects.contains(&ObjectRef::Net("DP".into())), "{:?}", c.subjects);
        assert!(
            c.subjects.contains(&ObjectRef::Named { kind: "keepout".into(), name: "box".into() }),
            "{:?}",
            c.subjects
        );
        assert!(c.hints.iter().any(|h| h.contains("keepout.set --allow-nets")), "{:?}", c.hints);
        assert_eq!((c.at, c.layer.as_deref()), (Some(q(20, 3)), Some("F.Cu")));
        assert!(r.tracks.is_empty() && r.notes.is_empty());
        p.board_mut().keepouts[1].allow_nets = vec!["DP".into()];
        let r = route(&p, &dp, &RouteOptions::default(), &Hooks::none()).expect("route");
        assert_eq!((r.stats.connections, r.stats.failed), (1, 0), "{:?}", r.connections);
    }

    /// The spokes of a relieved pad go to guide as the fill makes them (D110, D112): the pin of
    /// GND in its pour on B.Cu has four, on that layer alone, each from its anchor past the gap;
    /// a pad set solid has none.
    #[test]
    fn the_spokes_of_relieved_pads_are_given_as_the_fill_makes_them() {
        // The zone of the board joins its pads solidly (`connect_pads yes`): no spokes.
        let solid = project();
        assert!(adapted(&solid, &["GND", "SIG"]).design.zones.spokes.is_empty());
        let mut p = project();
        for z in &mut p.board_mut().zones {
            z.pads = PadConnection::Thermal;
        }
        let ad = adapted(&p, &["GND", "SIG"]);
        let pin = ad.pads.iter().position(|n| n == "J1.2").expect("J1.2");
        let spokes = &ad.design.zones.spokes;
        assert_eq!(spokes.len(), 4, "{spokes:?}");
        // With the four directions that the fill falls back to (D112), as spare ones, and the
        // number that a soldered pad is to keep (D113).
        let spare = &ad.design.zones.spare_spokes;
        assert_eq!(spare.len(), 4, "{spare:?}");
        assert!(spare.iter().all(|s| s.pad == pin && s.layer == LayerId(1) && s.to != s.from), "{spare:?}");
        assert!(spare.iter().all(|s| spokes.iter().all(|m| m.to != s.to)), "{spare:?}");
        assert_eq!(ad.design.zones.min_spokes, p.board().rules.min_thermal_spokes);
        assert!(spokes.iter().all(|s| s.pad == pin && s.layer == LayerId(1) && s.width == 350_000), "{spokes:?}");
        // Along the axes, to the gap of 0.4 mm and the spoke's width past the pad's box
        // (1 mm by 1.6 mm).
        let mut reach: Vec<(i64, i64)> = spokes.iter().map(|s| (s.to.x - s.from.x, s.to.y - s.from.y)).collect();
        reach.sort_unstable();
        assert_eq!(reach, [(-1_250_000, 0), (0, -1_550_000), (0, 1_550_000), (1_250_000, 0)]);
    }

    /// Two pads that a keep-out across the whole board keeps apart on every layer: guide looks
    /// once with no other net routed and says so (`Cause::Apart`, D110), in a run with a budget.
    #[test]
    fn a_connection_the_board_keeps_apart_is_told() {
        let mut p = project();
        let mut k = p.board().keepouts[0].clone();
        k.id = p.alloc_id();
        k.name = "moat".into();
        k.layers = Vec::new();
        k.no_vias = true;
        let q = |x: i64, y: i64| Point::new(Nm(x * 1_000_000), Nm(y * 1_000_000));
        k.outline = vec![q(-5, 4), q(35, 4), q(35, 5), q(-5, 5)];
        p.board_mut().keepouts.push(k);
        let dp = Scope::Nets(vec!["DP".into()]);
        let opts = RouteOptions { budget: Some(Duration::from_secs(30)), ..Default::default() };
        let began = Instant::now();
        let r = route(&p, &dp, &opts, &Hooks::none()).expect("route");
        assert_eq!((r.stats.connections, r.stats.failed), (1, 1), "{:?}", r.connections);
        let [c] = &r.connections[..] else { panic!("{:?}", r.connections) };
        let reason = c.reason.as_deref().unwrap_or_default();
        assert!(reason.starts_with("the board keeps J1.5 apart from J1.3"), "{reason}");
        assert!(c.hints.iter().any(|h| h.contains("join the two parts of the board")), "{:?}", c.hints);
        assert!(!c.hints.iter().any(|h| h.contains("native router")), "{:?}", c.hints);
        // Nor are the runs that move routes for it: the run is over long before its budget.
        assert!(began.elapsed() < Duration::from_secs(15), "{:?}", began.elapsed());
        assert_eq!(r.stats.iterations, 1);
    }

    /// Budgets, cancellation, progress and the seed, on the nets SIG, DP and DM.
    #[test]
    fn a_run_keeps_its_budgets_and_hears_the_hooks() {
        use super::super::WorkMeter;
        let p = project();
        let nets = Scope::Nets(vec!["SIG".into(), "DP".into(), "DM".into()]);
        let full = route(&p, &nets, &RouteOptions { pairs: Some(false), ..Default::default() }, &Hooks::none())
            .expect("route");
        assert_eq!((full.stats.connections, full.stats.failed, full.stats.budget_exhausted), (3, 0, false));
        assert!(full.stats.work > 0);
        // A work budget of one node: the run stops, says so, and is the same on every run. The
        // meter is charged what guide spent.
        let short = || {
            let opts = RouteOptions { pairs: Some(false), work: WorkMeter::limited(1), ..Default::default() };
            let r = route(&p, &nets, &opts, &Hooks::none()).expect("route");
            assert_eq!(opts.work.used(), r.stats.work);
            r
        };
        let r = short();
        assert!(r.stats.budget_exhausted && r.stats.failed > 0 && r.stats.work < full.stats.work, "{:?}", r.stats);
        let c = r.connections.iter().find(|c| c.status == ConnStatus::Failed).expect("failed");
        assert!(c.reason.as_deref().is_some_and(|m| m.starts_with("the work budget was spent")), "{c:?}");
        assert!(c.hints.iter().any(|h| h.contains("work_budget")), "{c:?}");
        assert_eq!(r, short());
        // Enough of it: complete, and the same on every run. Not the run without a budget by
        // rule: with one guide routes for the most connections soonest (D91).
        let enough = || {
            let work = WorkMeter::limited(full.stats.work * 100);
            let opts = RouteOptions { pairs: Some(false), work, ..Default::default() };
            route(&p, &nets, &opts, &Hooks::none()).expect("route")
        };
        let r = enough();
        assert_eq!((r.stats.connections, r.stats.failed, r.stats.budget_exhausted), (3, 0, false), "{:?}", r.stats);
        assert_eq!(r, enough());
        // A time budget that has ended: stopped by the deadline.
        let opts = RouteOptions { budget: Some(Duration::ZERO), ..Default::default() };
        let r = route(&p, &nets, &opts, &Hooks::none()).expect("route");
        assert!(r.stats.budget_exhausted && r.stats.failed == 3 && r.tracks.is_empty(), "{:?}", r.stats);
        // No net had its turn (`Routed::tried`), and the reason says that.
        let got_to = |c: &ConnectionReport| {
            c.reason.as_deref().is_some_and(|m| m.starts_with("the time budget ended before guide got to net "))
        };
        assert!(r.connections.iter().all(got_to), "{:?}", r.connections);
        // Progress comes from guide's phases, and a cancellation seen there stops the run.
        let heard = std::cell::RefCell::new(Vec::new());
        let progress = |_: u64, _: Option<u64>, m: &str| heard.borrow_mut().push(m.to_string());
        let r = route(&p, &nets, &RouteOptions::default(), &Hooks { progress: &progress, cancelled: &|| false });
        assert!(r.is_ok());
        let msgs = heard.take();
        for phase in ["guide: escapes and planes, ", "guide: routing, ", "guide: report, "] {
            assert!(msgs.iter().any(|m| m.starts_with(phase)), "{phase}: {msgs:?}");
        }
        let polls = std::cell::Cell::new(0);
        let cancelled = || {
            polls.set(polls.get() + 1);
            polls.get() > 1
        };
        let r = route(&p, &nets, &RouteOptions::default(), &Hooks { progress: &|_, _, _| {}, cancelled: &cancelled });
        assert_eq!(r, Err(RouteError::Cancelled));
        // A seed is another order of the nets: the same result for the same seed.
        let seeded = |seed| {
            let opts = RouteOptions { seed, pairs: Some(false), ..Default::default() };
            route(&p, &nets, &opts, &Hooks::none()).expect("route")
        };
        assert_eq!(seeded(7), seeded(7));
        assert_eq!(seeded(7).stats.failed, 0);
        assert_eq!(seeded(0), full);
    }

    /// Which nets a second run leaves out: those guide says its own search found no way for
    /// (`Routed::failed`) after a run that ended, and none after a run that a budget stopped.
    #[test]
    fn a_second_run_leaves_out_the_nets_a_run_that_ended_found_no_way_for() {
        let p = project();
        let ad = adapted(&p, &["SIG", "DP", "DM"]);
        let gopts = options(&p, &RouteOptions::default(), &[0, 1]);
        let mut r = call(&ad, &gopts, None, &Hooks::none()).expect("route");
        assert!(r.stopped.is_none() && r.failed.is_empty() && gave_up_on(&ad, &r).is_empty(), "{:?}", r.failed);
        let sig = ad.nets.iter().position(|n| n == "SIG").expect("SIG");
        r.failed = vec![NetId(sig as u32)];
        assert_eq!(gave_up_on(&ad, &r).into_iter().collect::<Vec<_>>(), ["SIG"]);
        r.stopped = Some(Stopped::Deadline);
        assert!(gave_up_on(&ad, &r).is_empty());
    }

    /// The routes of a run, on the board, are handed to guide as routes that it may move
    /// (`pcb::route_from`): they leave the fixed copper, their net is routed, and a run from
    /// them returns them as they were when nothing is in their way, as the net's own again.
    #[test]
    fn a_run_s_routes_are_handed_back_as_routes_that_move() {
        let p = project();
        let r = route(&p, &Scope::Nets(vec!["SIG".into()]), &RouteOptions::default(), &Hooks::none()).expect("route");
        assert!(!r.tracks.is_empty());
        let mut q = p.clone();
        let mut mine = BTreeSet::new();
        for t in &r.tracks {
            let mut t = t.clone();
            t.id = q.alloc_id();
            mine.insert(t.id);
            q.board_mut().tracks.push(t);
        }
        for v in &r.vias {
            let mut v = v.clone();
            v.id = q.alloc_id();
            mine.insert(v.id);
            q.board_mut().vias.push(v);
        }
        // As copper of the board, SIG is whole and not routed; handed back, it is a net to
        // route with its routes as the earlier run's.
        let mut ad = adapted(&q, &[]);
        let sig = ad.nets.iter().position(|n| n == "SIG").expect("SIG");
        let fixed = ad.design.board.tracks.len();
        assert!(!ad.design.nets[sig].route);
        let handed = ad.take_back(&mine);
        assert_eq!(handed, mine);
        assert_eq!(ad.earlier.tracks.len(), r.tracks.len());
        assert_eq!(ad.earlier.vias.len(), r.vias.len());
        assert_eq!(ad.design.board.tracks.len(), fixed - r.tracks.len());
        assert_eq!(ad.tracks.len(), ad.design.board.tracks.len());
        assert!(ad.design.nets[sig].route);
        let gopts = options(&q, &RouteOptions::default(), &[0, 1]);
        let again = call(&ad, &gopts, None, &Hooks::none()).expect("route");
        assert_eq!(again.taken_back, [NetId(sig as u32)]);
        assert!(again.open.is_empty() && again.unrouted.is_empty(), "{:?}", again.open);
        let (t, v) = convert(&ad, &again);
        assert_eq!((t.len(), v.len()), (r.tracks.len(), r.vias.len()));
    }

    /// A stub to a plane is a piece of a net with zones of one via and a few tracks: not a
    /// route between two pads through two vias, not an escape, not another net's.
    #[test]
    fn the_stubs_to_a_plane_are_told_from_routes() {
        let track = |net: &str, a: (i64, i64), b: (i64, i64), layer: &str| Track {
            id: ObjectId(0),
            layer: layer.into(),
            width: Nm(200_000),
            net: Some(net.into()),
            start: Point { x: Nm(a.0), y: Nm(a.1) },
            end: Point { x: Nm(b.0), y: Nm(b.1) },
            mid: None,
            locked: false,
        };
        let via = |net: &str, at: (i64, i64)| Via {
            id: ObjectId(0),
            at: Point { x: Nm(at.0), y: Nm(at.1) },
            drill: Nm(300_000),
            diameter: Nm(600_000),
            net: Some(net.into()),
            from: "F.Cu".into(),
            to: "B.Cu".into(),
            locked: false,
        };
        let tracks = vec![
            // A stub of GND, a route of GND through two vias, a stub of SIG, an escape of GND.
            track("GND", (0, 0), (1_000_000, 0), "F.Cu"),
            track("GND", (0, 5_000_000), (1_000_000, 5_000_000), "F.Cu"),
            track("GND", (1_000_000, 5_000_000), (4_000_000, 5_000_000), "B.Cu"),
            track("GND", (4_000_000, 5_000_000), (5_000_000, 5_000_000), "F.Cu"),
            track("SIG", (0, 9_000_000), (1_000_000, 9_000_000), "F.Cu"),
            track("GND", (0, 12_000_000), (1_000_000, 12_000_000), "F.Cu"),
        ];
        let vias = vec![
            via("GND", (1_000_000, 0)),
            via("GND", (1_000_000, 5_000_000)),
            via("GND", (4_000_000, 5_000_000)),
            via("SIG", (1_000_000, 9_000_000)),
            via("GND", (1_000_000, 12_000_000)),
        ];
        let piece = pieces(&tracks, &vias);
        let poured: BTreeSet<&str> = ["GND"].into();
        let escapes: BTreeSet<(i64, i64)> = [(1_000_000, 12_000_000)].into();
        assert_eq!(stub_pieces(&tracks, &vias, &piece, &poured, &escapes), [piece[0]].into());
    }

    /// The first run has the nets with zones in scope beside those with open connections.
    #[test]
    fn the_first_run_has_the_nets_with_zones_too() {
        let p = project();
        let open: BTreeSet<String> = ["SIG".to_string()].into();
        let all = first_nets(&p, &open, &|_| true);
        assert_eq!(all.iter().map(String::as_str).collect::<Vec<_>>(), ["GND", "SIG"]);
        // Not a net out of scope.
        assert_eq!(first_nets(&p, &open, &|n| n == "SIG"), open);
        // A run for everything still routes and counts what the board has open.
        let r = route(&p, &Scope::All, &RouteOptions::default(), &Hooks::none()).expect("route");
        assert_eq!(r.stats.failed, 0, "{:?}", r.connections);
    }

    /// What guide is told beside the board: no fill of its own at the end and no joining of zones'
    /// pieces on it, the pads its routes cut off the fills joined again, what its passes leave
    /// open settled, by negotiations first, the rows of fine pitch not planned on any board,
    /// no via in a pad, and for the escapes of trapped pads the smallest via the rules allow,
    /// the native fanout's.
    #[test]
    fn guide_gets_no_fill_and_the_smallest_via_for_escapes() {
        let mut p = project();
        p.board_mut().rules.min_drill = Nm(200_000);
        p.board_mut().rules.min_annular_ring = Nm(100_000);
        let g = options(&p, &RouteOptions::default(), &[0, 1]);
        assert!(!g.fills && !g.join && !g.vias_in_pads && g.keep_partial);
        assert!(g.pour_cuts && g.settle && g.negotiate && !g.row_fanout && g.crossings && !g.negotiate_places);
        // The room of the spokes of pads in pours costs other nets' tracks (D110), and a via
        // what guide has it cost.
        assert_eq!((g.spoke_cost, g.via_cost), (4.0, pcb::RouteOptions::default().via_cost));
        // Every net at once before the passes in a run with a budget, of time or of work, and
        // in no other (D113); on the threads that the native router's batches have.
        assert!(!g.whole && g.threads >= 1);
        let timed = RouteOptions { budget: Some(Duration::from_secs(30)), ..Default::default() };
        assert!(options(&p, &timed, &[0, 1]).whole);
        let counted = RouteOptions { work: super::super::WorkMeter::limited(1_000_000), ..Default::default() };
        assert!(options(&p, &counted, &[0, 1]).whole);
        let four = BOARD.replace(
            r#"(0 "F.Cu" signal) (31 "B.Cu" signal)"#,
            r#"(0 "F.Cu" signal) (1 "In1.Cu" signal) (2 "In2.Cu" signal) (31 "B.Cu" signal)"#,
        );
        let mut q = Project::new("t");
        import(&mut q, &four, &BoardImportOptions { file_name: "t.kicad_pcb".into(), ..Default::default() })
            .expect("import");
        assert_eq!(q.board().stackup.copper_names().len(), 4);
        assert!(!options(&q, &RouteOptions::default(), &[0, 1, 2, 3]).row_fanout);
        assert_eq!(g.fanout_via, Some(ViaSize { diameter: 400_000, drill: 200_000 }));
        assert_eq!((g.seed, g.layers, g.work_budget), (None, None, None));
        let opts = RouteOptions { seed: 7, layers: vec!["B.Cu".into()], ..Default::default() };
        let g = options(&p, &opts, &[1]);
        assert_eq!((g.seed, g.layers), (Some(7), Some(LayerSet::EMPTY.with(LayerId(1)))));
    }

    /// `layers`: tracks on the back only. SIG's through-hole pad J1.1 and its via are on both
    /// layers, so the net is joined there.
    #[test]
    fn tracks_keep_to_the_layers_given() {
        let p = project();
        let opts = RouteOptions { layers: vec!["B.Cu".into()], ..Default::default() };
        let r = route(&p, &Scope::Nets(vec!["SIG".into()]), &opts, &Hooks::none()).expect("route");
        assert_eq!((r.stats.connections, r.stats.failed), (1, 0), "{:?}", r.connections);
        assert!(!r.tracks.is_empty() && r.tracks.iter().all(|t| t.layer == "B.Cu"), "{:?}", r.tracks);
    }

    /// Custom clearance rules: one that is an area's goes over as a rule area (the last rule in
    /// board order first), the others are named. An area has the larger clearance of a later
    /// rule that can hold new copper in it, as the DRC would take that rule for the pair: `neck`
    /// has the 0.3 mm of `vias`, and `top` the 0.15 mm of `inside`.
    #[test]
    fn area_rules_are_carried_over_and_the_others_named() {
        use crate::model::board::{CustomRule, ItemKind, RuleScope};
        let mut p = project();
        let mut k = p.board().keepouts[0].clone();
        k.id = p.alloc_id();
        k.name = "fine".into();
        k.no_tracks = false;
        let q = |x: i64, y: i64| Point::new(Nm(x * 1_000_000), Nm(y * 1_000_000));
        k.outline = vec![q(8, 8), q(12, 8), q(12, 12), q(8, 12)];
        p.board_mut().keepouts.push(k);
        let area = RuleScope { touches_area: Some("fine".into()), ..Default::default() };
        let rule = |name: &str, scope: RuleScope, clearance: i64| CustomRule {
            clearance: Some(Nm(clearance)),
            ..CustomRule::new(name, scope)
        };
        let top = RuleScope { layer: Some("F.Cu".into()), ..area.clone() };
        let rules = vec![
            rule("neck", area.clone(), 100_000),
            rule("vias", RuleScope { kinds: vec![ItemKind::Via], ..area.clone() }, 300_000),
            CustomRule { with: Some(top.clone()), ..rule("top", top, 120_000) },
            rule("inside", RuleScope { in_area: Some("fine".into()), ..Default::default() }, 150_000),
            CustomRule { disallow: vec![ItemKind::Via], ..CustomRule::new("no_vias", area.clone()) },
            CustomRule { track_width: Some(Nm(150_000)), ..CustomRule::new("narrow", area) },
        ];
        p.board_mut().custom_rules = rules;
        let ad = adapted(&p, &["SIG"]);
        let square = Shape::polygon(&[at(8.0, 8.0), at(12.0, 8.0), at(12.0, 12.0), at(8.0, 12.0)], &[]).unwrap();
        let both = LayerSet::first(2);
        let over = |clearance| Overrides { track_width: None, clearance: Some(clearance) };
        assert_eq!(
            ad.design.rules.spec().areas,
            [
                RuleArea { shape: square.clone(), layers: LayerSet::EMPTY.with(LayerId(0)), overrides: over(150_000) },
                RuleArea { shape: square, layers: both, overrides: over(300_000) },
            ]
        );
        // The rule area forbids nothing: it is no keep-out of guide's.
        assert_eq!(ad.keepouts, ["wall"]);
        let [note] = &ad.notes[..] else { panic!("{:?}", ad.notes) };
        assert_eq!(note.code, "route.guide_custom_rules");
        let named = |n: &str| ObjectRef::Named { kind: "rule".into(), name: n.into() };
        assert_eq!(note.subjects, [named("vias"), named("inside"), named("no_vias")]);
        assert!(note.hint.is_some());
    }

    /// A row of pads 0.2 mm apart on a board with a clearance of 0.25 mm: no track leaves the
    /// three in the middle. With the two rules such a part has in KiCad, 0.16 mm for what is in
    /// its courtyard on the front and is no zone, and after it 0.18 mm for its pads, the
    /// courtyard is an area of guide's at 0.18 mm, the larger of the two as the DRC takes the
    /// later rule between a track and a pad, the pads have 0.18 mm of their own, no rule is
    /// left to name, and every net is routed with nothing for the verification to take out.
    /// The native router, which resolves the same rules itself, routes them too.
    #[test]
    fn a_courtyard_rule_and_a_rule_for_the_pads_of_the_footprint() {
        use crate::model::board::{CustomRule, ItemKind, RuleScope};
        let mut text = String::from(
            r#"(kicad_pcb (version 20221018) (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user) (47 "F.CrtYd" user))
  (net 0 "") (net 1 "N1") (net 2 "N2") (net 3 "N3") (net 4 "N4") (net 5 "N5")
  (footprint "Package_DFN_QFN:QFN-5" (layer "F.Cu") (at 110 62)
    (property "Reference" "U1") (property "Value" "U")
    (fp_line (start -1.25 -0.65) (end 1.25 -0.65) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
    (fp_line (start 1.25 -0.65) (end 1.25 0.65) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
    (fp_line (start 1.25 0.65) (end -1.25 0.65) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
    (fp_line (start -1.25 0.65) (end -1.25 -0.65) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
"#,
        );
        for k in 0..5 {
            let x = -0.8 + 0.4 * f64::from(k);
            text += &format!(
                "    (pad \"{n}\" smd rect (at {x:.1} 0) (size 0.2 0.8) (layers \"F.Cu\") (net {n} \"N{n}\"))\n",
                n = k + 1
            );
        }
        text += ")\n  (footprint \"J:J\" (layer \"F.Cu\") (at 110 55)\n    (property \"Reference\" \"J1\") (property \"Value\" \"J\")\n";
        for k in 0..5 {
            text += &format!(
                "    (pad \"{n}\" smd rect (at {x} 0) (size 1 1) (layers \"F.Cu\") (net {n} \"N{n}\"))\n",
                n = k + 1,
                x = -4 + 2 * k
            );
        }
        text += ")\n  (gr_rect (start 100 50) (end 120 70) (stroke (width 0.1)) (fill none) (layer \"Edge.Cuts\"))\n)";
        let mut p = Project::new("t");
        import(&mut p, &text, &BoardImportOptions { file_name: "t.kicad_pcb".into(), ..Default::default() })
            .expect("import");
        let rules = &mut p.board_mut().rules;
        (rules.clearance, rules.track_width) = (Nm(250_000), Nm(200_000));
        let nets = ["N1", "N2", "N3", "N4", "N5"];
        let all = || Scope::All;
        let opts = RouteOptions::default();

        // Without the rules: the pads in the middle are shut in.
        let r = route(&p, &all(), &opts, &Hooks::none()).expect("route");
        assert_eq!(r.stats.connections, 5);
        assert!(r.stats.failed >= 3, "{:?}", r.stats);

        let kinds = vec![ItemKind::Pad, ItemKind::Track, ItemKind::Via, ItemKind::Graphic];
        let yard =
            RuleScope { kinds, layer: Some("F.Cu".into()), in_courtyard: Some("QFN-5*".into()), ..Default::default() };
        let pads = RuleScope { kinds: vec![ItemKind::Pad], footprint: Some("QFN-5*".into()), ..Default::default() };
        p.board_mut().custom_rules = vec![
            CustomRule { clearance: Some(Nm(160_000)), ..CustomRule::new("neck", yard) },
            CustomRule { clearance: Some(Nm(180_000)), ..CustomRule::new("neck pads", pads) },
        ];
        let ad = adapted(&p, &nets);
        // U1 is at (10, 8) from the lower-left corner, Y up.
        let yard = Shape::polygon(&[at(8.75, 7.35), at(11.25, 7.35), at(11.25, 8.65), at(8.75, 8.65)], &[]).unwrap();
        let [area] = &ad.design.rules.spec().areas[..] else { panic!("{:?}", ad.design.rules.spec().areas) };
        assert_eq!(area.shape.bbox(), yard.bbox());
        assert_eq!(area.layers, LayerSet::EMPTY.with(LayerId(0)));
        assert_eq!(area.overrides, Overrides { track_width: None, clearance: Some(180_000) });
        for (name, pad) in ad.pads.iter().zip(&ad.design.board.pads) {
            assert_eq!(pad.clearance, name.starts_with("U1.").then_some(180_000), "{name}");
        }
        assert_eq!(ad.notes, []);

        let r = route(&p, &all(), &opts, &Hooks::none()).expect("route");
        assert_eq!((r.stats.connections, r.stats.failed, r.stats.drc_removed), (5, 0, 0), "{:?}", r.connections);
        // What it routed passes the DRC under the rules, and only under them: a track is nearer
        // a pad of another net than the board's 0.25 mm.
        let mut routed = p.clone();
        for t in &r.tracks {
            let id = routed.alloc_id();
            routed.board_mut().tracks.push(Track { id, ..t.clone() });
        }
        let errors = |p: &Project| {
            crate::drc::check_copper(p)
                .into_iter()
                .filter(|d| d.severity == crate::diag::Severity::Error && d.code != "drc.unrouted")
                .filter(|d| d.subjects.iter().any(|s| matches!(s, ObjectRef::Item { kind, .. } if kind == "track")))
                .count()
        };
        assert_eq!(errors(&routed), 0);
        routed.board_mut().custom_rules.clear();
        assert!(errors(&routed) > 0);

        // The native router under the same rules.
        let native = super::super::route(&p, &all(), &opts, &Hooks::none()).expect("native");
        assert_eq!((native.stats.connections, native.stats.failed), (5, 0), "{:?}", native.connections);
    }

    /// A pad stack is one pad of guide's with its other shapes on their layers and its hole, and
    /// the copper between the pads of a net tie is where its nets may touch.
    #[test]
    fn a_pad_stack_is_one_pad_and_a_net_tie_is_where_its_nets_touch() {
        let text = r#"(kicad_pcb (version 20241229) (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (1 "In1.Cu" signal) (2 "In2.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
  (net 0 "") (net 1 "A") (net 2 "B")
  (footprint "Test:Stack" (layer "F.Cu") (at 110 60)
    (property "Reference" "J1")
    (pad "1" thru_hole rect (at 0 0) (size 2 2) (drill 0.8) (layers "*.Cu" "*.Mask") (net 1 "A")
      (padstack (mode front_inner_back)
        (layer "Inner" (shape circle) (size 1.2 1.2))
        (layer "B.Cu" (shape oval) (size 2 1.5)))))
  (footprint "Jumper:NetTie" (layer "F.Cu") (at 120 60)
    (property "Reference" "NT1") (property "Value" "NetTie")
    (net_tie_pad_groups "1, 2")
    (fp_line (start -0.5 0) (end 0.5 0) (stroke (width 0.3) (type solid)) (layer "F.Cu"))
    (pad "1" smd rect (at -0.5 0) (size 0.6 0.6) (layers "F.Cu") (net 1 "A"))
    (pad "2" smd rect (at 0.5 0) (size 0.6 0.6) (layers "F.Cu") (net 2 "B")))
  (gr_rect (start 100 50) (end 130 70) (stroke (width 0.1)) (fill none) (layer "Edge.Cuts"))
)"#;
        let mut p = Project::new("t");
        import(&mut p, text, &BoardImportOptions { file_name: "t.kicad_pcb".into(), ..Default::default() })
            .expect("import");
        let ad = adapted(&p, &["A"]);
        let d = &ad.design;
        let (a, b) = (NetId(0), NetId(1));
        assert_eq!(ad.nets, ["A", "B"]);
        // J1.1: the rectangle on top, the circle on the two inner layers, the oval below; one
        // pad with its hole, and no via standing in for the barrel.
        let stacked: Vec<&gdrc::Pad> =
            ad.pads.iter().zip(&d.board.pads).filter(|(n, _)| *n == "J1.1").map(|(_, pad)| pad).collect();
        let [pad] = &stacked[..] else { panic!("{stacked:?}") };
        assert_eq!(pad.layers, LayerSet::EMPTY.with(LayerId(0)));
        assert_eq!(pad.hole, Some(Shape::circle(at(10.0, 10.0), 800_000).unwrap()));
        let inner = LayerSet::EMPTY.with(LayerId(1)).with(LayerId(2));
        assert_eq!(pad.stack.len(), 2, "{:?}", pad.stack);
        assert_eq!(pad.stack[0], (inner, Shape::circle(at(10.0, 10.0), 1_200_000).unwrap()));
        assert_eq!(pad.stack[1].0, LayerSet::EMPTY.with(LayerId(3)));
        assert_eq!(pad.copper_layers(), LayerSet::first(4));
        assert!(d.board.vias.is_empty());
        // The tie: the line between NT1's pads, copper of the pad it starts on, which A and B
        // may both touch.
        assert_eq!(ad.copper, ["copper:NT1/0"]);
        assert_eq!(d.board.ties, [gdrc::NetTie { nets: vec![a, b], items: vec![gdrc::Item::Zone(0)] }]);
        assert!(d.board.zones[0].net.is_some());
        assert_eq!(ad.notes, []);
        // A is routed from the stack to the tie's pad and the board is clean.
        let r = route(&p, &Scope::All, &RouteOptions::default(), &Hooks::none()).expect("route");
        assert_eq!((r.stats.connections, r.stats.failed, r.stats.drc_removed), (1, 0, 0), "{:?}", r.connections);
    }

    #[test]
    fn new_copper_is_taken_out_by_connected_pieces() {
        let t = |net: &str, layer: &str, a: (i64, i64), b: (i64, i64)| Track {
            id: ObjectId(0),
            layer: layer.into(),
            width: Nm(200_000),
            net: Some(net.into()),
            start: Point::new(Nm(a.0), Nm(a.1)),
            end: Point::new(Nm(b.0), Nm(b.1)),
            mid: None,
            locked: false,
        };
        let tracks = [
            t("A", "F.Cu", (0, 0), (10, 0)),
            t("A", "F.Cu", (10, 0), (10, 10)),
            t("A", "B.Cu", (10, 10), (20, 10)),
            // Another net at the same point, and the same net elsewhere: pieces of their own.
            t("B", "F.Cu", (10, 0), (30, 0)),
            t("A", "F.Cu", (50, 50), (60, 50)),
        ];
        let vias = [Via {
            id: ObjectId(0),
            at: Point::new(Nm(10), Nm(10)),
            drill: Nm(300_000),
            diameter: Nm(600_000),
            net: Some("A".into()),
            from: "F.Cu".into(),
            to: "B.Cu".into(),
            locked: false,
        }];
        assert_eq!(pieces(&tracks, &vias), [0, 0, 0, 3, 4, 0]);
    }
}
