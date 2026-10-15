//! Footprint placement helpers: automatic placement by schematic-like groups ([`auto_place`])
//! and placement of a part next to a pin or another part ([`place_near`]). Pure functions over
//! the project: they return placements, commands apply them. See `docs/BOARD.md`.
//!
//! Courtyards are handled as their bounding boxes at quarter-turn rotations (exact for the
//! rectangular courtyards of generated footprints, conservative otherwise). The allowed area of a
//! board side is the outer contour shrunk by the edge margin, minus cutouts (grown by the edge
//! margin), footprint keep-outs, board holes and the courtyards of footprints that stay where
//! they are (grown by the gap); a pose is valid when its courtyard box lies in that area and
//! keeps the gap to the other parts being placed. Everything is deterministic: candidates are
//! generated on a 50 µm grid and ties are broken by candidate order.

use std::collections::{BTreeMap, BTreeSet};

use polyclip::{ArcTol, Circle, FillRule, Join, Op, PolygonSet, Ring, Side};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{COPPER_TOL, contour_ring, footprint_for, pad_nets, placed_courtyard, placed_pads};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::{BoardSide, EdgeHint, PlacedFootprint};
use crate::model::circuit::PinRef;
use crate::model::part::{Category, PinKind};
use crate::model::sections::natural_cmp;
use crate::symbolgen::is_ground;
use crate::units::{Angle, Nm};

/// Default courtyard-to-courtyard gap of automatic placement and `place_near`.
pub const DEFAULT_GAP: Nm = Nm(250_000);
/// Placement grid.
const GRID: i64 = 50_000;
/// Fine search step.
const STEP: i64 = 250_000;
/// Share of the free board area the groups' rooms take in all when `place.auto` spreads them.
pub const SPREAD_DENSITY: f64 = 0.6;
/// Largest room of a group, relative to its parts' area, when spreading.
const SPREAD_MAX: f64 = 2.5;
/// Widest empty band, as a share of the board's width or height, spread placement leaves along
/// a side of the board at the default density (it scales inversely with the density).
const SPREAD_MARGIN: f64 = 0.125;
/// How much longer than before the stretch spread placement lets the ratsnest grow, at the
/// default density (it scales with the density): the largest stretch within it is kept.
const SPREAD_RATSNEST: f64 = 0.25;
/// Windows per grid of the evenness measure of spread placement (`spread::Windows`).
const SPREAD_WINDOWS: f64 = 12.0;
/// Weight of the evenness when groups are moved after the stretch, per unit of the board's
/// width plus height: a group moves into emptier windows when the ratsnest it adds is less
/// than this times the unevenness removed.
const SPREAD_EVEN: f64 = 5.0;

/// A placement failure, with a stable code and a fix hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceError {
    /// Stable code (`board.no_outline`, `place.no_room`, ...).
    pub code: &'static str,
    /// Message.
    pub message: String,
    /// How to fix it.
    pub hint: String,
}

impl PlaceError {
    fn new(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        PlaceError { code, message: message.into(), hint: hint.into() }
    }

    fn no_outline() -> Self {
        PlaceError::new("board.no_outline", "the board has no outline", "set one with `board.outline`")
    }
}

/// An axis-aligned box in nanometers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    /// Left.
    pub x0: i64,
    /// Bottom.
    pub y0: i64,
    /// Right.
    pub x1: i64,
    /// Top.
    pub y1: i64,
}

impl Rect {
    /// Bounding box of points.
    pub fn of(pts: impl IntoIterator<Item = Point>) -> Option<Rect> {
        let mut it = pts.into_iter();
        let f = it.next()?;
        let mut r = Rect { x0: f.x.0, y0: f.y.0, x1: f.x.0, y1: f.y.0 };
        for q in it {
            r.x0 = r.x0.min(q.x.0);
            r.y0 = r.y0.min(q.y.0);
            r.x1 = r.x1.max(q.x.0);
            r.y1 = r.y1.max(q.y.0);
        }
        Some(r)
    }

    fn shift(self, d: Point) -> Rect {
        Rect { x0: self.x0 + d.x.0, y0: self.y0 + d.y.0, x1: self.x1 + d.x.0, y1: self.y1 + d.y.0 }
    }

    fn expand(self, m: i64) -> Rect {
        Rect { x0: self.x0 - m, y0: self.y0 - m, x1: self.x1 + m, y1: self.y1 + m }
    }

    /// Whether the boxes are at least `gap` apart along some axis.
    fn clear_of(&self, o: &Rect, gap: i64) -> bool {
        self.x1 + gap <= o.x0 || o.x1 + gap <= self.x0 || self.y1 + gap <= o.y0 || o.y1 + gap <= self.y0
    }

    fn ring(&self) -> Ring {
        Ring::from([(self.x0, self.y0), (self.x1, self.y0), (self.x1, self.y1), (self.x0, self.y1)])
    }

    /// Center.
    pub fn center(&self) -> Point {
        Point::new(Nm((self.x0 + self.x1) / 2), Nm((self.y0 + self.y1) / 2))
    }

    fn w(&self) -> i64 {
        self.x1 - self.x0
    }

    fn h(&self) -> i64 {
        self.y1 - self.y0
    }
}

mod mst;
mod spread;
use mst::dist;

fn snap(v: i64) -> i64 {
    (v as f64 / GRID as f64).round() as i64 * GRID
}

fn side_index(s: BoardSide) -> usize {
    match s {
        BoardSide::Top => 0,
        BoardSide::Bottom => 1,
    }
}

/// Courtyard box of a placed footprint (bounding box of its courtyard, else of its pads).
pub fn courtyard_box(p: &Project, refdes: &str) -> Option<Rect> {
    if let Some(r) = placed_courtyard(p, refdes) {
        return Rect::of(r.0.iter().map(|q| Point::from(*q)));
    }
    let pf = p.board().footprints.get(refdes)?;
    let fp = footprint_for(p, refdes)?;
    let local = local_courtyard(fp);
    let tf = super::transform(pf);
    Rect::of(local.into_iter().map(tf))
}

/// Local courtyard outline: the footprint's, else its pads' box grown by 0.25 mm.
fn local_courtyard(fp: &crate::model::footprint::Footprint) -> Vec<Point> {
    if fp.courtyard.len() >= 3 {
        return fp.courtyard.clone();
    }
    let pts = fp.pads.iter().flat_map(|pd| {
        let (w, h) = pd.size();
        let (hw, hh) = (Nm(w.0 / 2 + 250_000), Nm(h.0 / 2 + 250_000));
        [Point::new(-hw, -hh), Point::new(hw, hh), Point::new(-hw, hh), Point::new(hw, -hh)]
            .map(|c| c.rotated(pd.rotation) + pd.at)
    });
    match Rect::of(pts) {
        Some(r) => {
            [(r.x0, r.y0), (r.x1, r.y0), (r.x1, r.y1), (r.x0, r.y1)].map(|(x, y)| Point::new(Nm(x), Nm(y))).to_vec()
        }
        None => vec![],
    }
}

/// Corners of the box of a footprint's silkscreen (strokes included), if it has any.
fn silk_extent(fp: &crate::model::footprint::Footprint) -> Vec<Point> {
    use crate::model::footprint::{GraphicGeometry, GraphicLayer};
    let mut pts = Vec::new();
    for g in fp.graphics.iter().filter(|g| g.layer == GraphicLayer::Silk) {
        let (centers, r): (Vec<Point>, Nm) = match &g.geometry {
            GraphicGeometry::Path { points } | GraphicGeometry::Polygon { points } => {
                (points.clone(), Nm(g.width.0 / 2))
            }
            GraphicGeometry::Circle { center, radius, .. } => (vec![*center], Nm(radius.0 + g.width.0 / 2)),
        };
        for c in centers {
            pts.extend([Point::new(c.x - r, c.y - r), Point::new(c.x + r, c.y + r)]);
        }
    }
    match Rect::of(pts) {
        Some(r) => {
            [(r.x0, r.y0), (r.x1, r.y0), (r.x1, r.y1), (r.x0, r.y1)].map(|(x, y)| Point::new(Nm(x), Nm(y))).to_vec()
        }
        None => vec![],
    }
}

/// What a part is for grouping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Connector,
    Ic,
    Passive,
    /// A two-terminal part with an edge hint (an indicator LED, a button): placed like an
    /// anchor, at its edge, rather than next to a pin.
    Edge,
}

/// A position of a part being placed: origin, quarter turns, side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pose {
    at: Point,
    rot: u8,
    side: BoardSide,
}

impl Pose {
    fn placed(&self) -> PlacedFootprint {
        PlacedFootprint {
            at: self.at,
            rotation: Angle(self.rot as i32 * 90_000),
            side: self.side,
            locked: false,
            footprint: None,
        }
    }
}

/// A part being placed.
struct Part {
    refdes: String,
    kind: Kind,
    pins: usize,
    footprint: String,
    /// Pad numbers and nets (net index), in footprint order.
    pads: Vec<(String, Option<usize>)>,
    /// Courtyard box and pad centers relative to the origin, per [side][quarter turn].
    rel: [[(Rect, Vec<Point>); 4]; 2],
    /// Edge hint (`place.hint`).
    edge: Option<EdgeHint>,
}

impl Part {
    /// The edge this part goes to: its hint, or the nearest one for connectors.
    fn edge_target(&self) -> Option<EdgeHint> {
        self.edge.or((self.kind == Kind::Connector).then_some(EdgeHint::Any))
    }

    fn rect(&self, pose: &Pose) -> Rect {
        self.rel[side_index(pose.side)][pose.rot as usize].0.shift(pose.at)
    }

    fn pad_at(&self, pose: &Pose, i: usize) -> Point {
        self.rel[side_index(pose.side)][pose.rot as usize].1[i] + pose.at
    }

    fn area(&self) -> i128 {
        let r = self.rel[0][0].0;
        r.w() as i128 * r.h() as i128
    }
}

/// A net endpoint: a pad that stays put, or a pad of a part being placed.
#[derive(Clone, Copy, Debug)]
enum Pin {
    Fixed(Point),
    Part(usize, usize),
}

/// The placement state.
struct Engine<'a> {
    p: &'a Project,
    gap: i64,
    parts: Vec<Part>,
    index: BTreeMap<String, usize>,
    poses: Vec<Option<Pose>>,
    net_names: Vec<String>,
    net_pins: Vec<Vec<Pin>>,
    /// Nets touched by each part.
    part_nets: Vec<Vec<usize>>,
    /// Pads of footprints that stay put: (designator, pad) → (center, net).
    fixed: BTreeMap<(String, String), (Point, Option<usize>)>,
    /// Allowed area per side.
    free: [PolygonSet; 2],
    /// Box of the outer contour.
    board: Rect,
    /// Center of the allowed top area.
    center: Point,
    /// Passives kept next to their target pin during improvement: (part, own pad, target, weight).
    tethers: Vec<(usize, usize, Pin, f64)>,
    /// Room of each anchor when spreading: improvement keeps its courtyard inside.
    rooms: Vec<Option<Rect>>,
    /// Connector spacing when spreading: centers of connectors that stay, and the distance
    /// below which connectors crowd each other (0: no spacing).
    crowd: (Vec<Point>, f64),
    /// Breakout areas of differential pairs in front of their pins (per side), kept free of
    /// passives: a pull-up on D+ placed at its pin blocks the pair's way out.
    pair_zones: Vec<(BoardSide, Rect)>,
    /// The differential pairs' (positive, negative) net indices.
    pair_nets: Vec<(usize, usize)>,
    /// Parts (not passives) with pads of both nets of a pair: ends whose breakouts move with
    /// them.
    pair_end: Vec<bool>,
    /// Spreading (`place.auto` default). The packed placement (`compact`) keeps D50's rules
    /// exactly: connectors are not turned to face their edge, and passives are assigned as then.
    spread: bool,
}

/// With `silk`, the part's box also covers its silkscreen.
fn build_part(
    p: &Project,
    refdes: &str,
    net_index: &BTreeMap<String, usize>,
    silk: bool,
    edge: Option<EdgeHint>,
) -> Option<Part> {
    let fp = footprint_for(p, refdes)?;
    let mut courtyard = local_courtyard(fp);
    if courtyard.is_empty() {
        return None;
    }
    // Silkscreen reaching past the courtyard (pin-1 marks) is part of the box, so it does not
    // land on a neighbour's pads.
    if silk {
        courtyard.extend(silk_extent(fp));
    }
    let nets = pad_nets(p, refdes);
    let comp = p.circuit().components.get(refdes)?;
    let part = p.library().parts.get(&comp.part);
    let pins = part.map_or(fp.pads.len(), |x| x.symbol.pins.len().max(1));
    let connector = part.is_some_and(|x| x.category == Category::Connector);
    let kind = if connector {
        Kind::Connector
    } else if pins > 2 {
        Kind::Ic
    } else if edge.is_some() {
        Kind::Edge
    } else {
        Kind::Passive
    };
    let pads: Vec<(String, Option<usize>)> = fp
        .pads
        .iter()
        .map(|pd| (pd.number.clone(), nets.get(&pd.number).and_then(|n| net_index.get(n).copied())))
        .collect();
    let rel = [BoardSide::Top, BoardSide::Bottom].map(|side| {
        [0u8, 1, 2, 3].map(|rot| {
            let pf = Pose { at: Point::ORIGIN, rot, side }.placed();
            let tf = super::transform(&pf);
            let r = Rect::of(courtyard.iter().map(|q| tf(*q))).expect("non-empty");
            (r, fp.pads.iter().map(|pd| tf(pd.at)).collect())
        })
    });
    Some(Part { refdes: refdes.to_string(), kind, pins, footprint: fp.name.clone(), pads, rel, edge })
}

/// The allowed area of a board side for courtyards: outer contour shrunk by `edge`, minus
/// cutouts grown by `edge`, footprint keep-outs, holes and the courtyards of footprints not
/// in `moving`, grown by `gap`.
fn free_area(p: &Project, side: BoardSide, gap: i64, edge: i64, moving: &BTreeSet<String>) -> Option<PolygonSet> {
    let b = p.board();
    let inside = ArcTol::new(1_000, Side::Inside);
    let outer: Ring = contour_ring(b.outline.contours.first()?, inside).into();
    if outer.len() < 3 {
        return None;
    }
    let base = polyclip::offset(&outer, -edge, Join::Round, inside).ok()?;
    let mut obstacles: PolygonSet = Vec::new();
    let grow = |r: &Ring, d: i64, obstacles: &mut PolygonSet| {
        if let Ok(s) = polyclip::offset(r, d.max(0), Join::Round, COPPER_TOL) {
            obstacles.extend(s);
        }
    };
    for c in b.outline.contours.iter().skip(1) {
        let r: Ring = contour_ring(c, COPPER_TOL).into();
        if r.len() >= 3 {
            grow(&r, edge, &mut obstacles);
        }
    }
    let cu = super::side_layer(side, "F.Cu");
    for k in &b.keepouts {
        if k.no_footprints && (k.layers.is_empty() || k.layers.contains(&cu)) && k.outline.len() >= 3 {
            let r: Ring = k.outline.iter().map(|q| polyclip::Point::from(*q)).collect::<Vec<_>>().into();
            grow(&r, gap, &mut obstacles);
        }
    }
    for h in &b.holes {
        let r = Circle { center: h.at.into(), radius: h.diameter().0 / 2 + gap }.to_ring(COPPER_TOL);
        if let Ok(r) = r {
            obstacles.push(polyclip::Polygon::new(r, vec![]));
        }
    }
    // Routing already on the board: a part dropped onto a track or via of its side shorts it
    // (pads of the moving parts keep the board's clearance from them).
    let clearance = b.rules.clearance.0.max(0);
    for t in b.tracks.iter().filter(|t| t.layer == cu) {
        let pts: Vec<polyclip::Point> =
            std::iter::once(t.start).chain(t.mid).chain(std::iter::once(t.end)).map(polyclip::Point::from).collect();
        let half = (t.width.0 / 2 + clearance).max(1);
        if let Ok(s) =
            polyclip::offset_paths(&vec![polyclip::Path(pts)], half, Join::Round, polyclip::EndCap::Round, COPPER_TOL)
        {
            obstacles.extend(s);
        }
    }
    for v in &b.vias {
        let r = Circle { center: v.at.into(), radius: v.diameter.0 / 2 + clearance }.to_ring(COPPER_TOL);
        if let Ok(r) = r {
            obstacles.push(polyclip::Polygon::new(r, vec![]));
        }
    }
    for (r, pf) in &b.footprints {
        if moving.contains(r) || pf.side != side {
            continue;
        }
        if let Some(c) = placed_courtyard(p, r) {
            grow(&c, gap, &mut obstacles);
        }
    }
    if obstacles.is_empty() {
        return Some(base);
    }
    polyclip::boolean(Op::Difference, &base, &obstacles, FillRule::NonZero).ok()
}

impl<'a> Engine<'a> {
    fn new(
        p: &'a Project,
        moving: &[String],
        gap: i64,
        silk: bool,
        edges: &BTreeMap<String, EdgeHint>,
    ) -> Result<Self, PlaceError> {
        let b = p.board();
        let c = b.outline.contours.first().ok_or_else(PlaceError::no_outline)?;
        let board =
            Rect::of(contour_ring(c, COPPER_TOL).into_iter().map(Point::from)).ok_or_else(PlaceError::no_outline)?;
        let net_names: Vec<String> = p.circuit().nets.keys().cloned().collect();
        let net_index: BTreeMap<String, usize> = net_names.iter().enumerate().map(|(i, n)| (n.clone(), i)).collect();
        let mut parts = Vec::new();
        for r in moving {
            if let Some(part) = build_part(p, r, &net_index, silk, edges.get(r).copied()) {
                parts.push(part);
            }
        }
        let index: BTreeMap<String, usize> = parts.iter().enumerate().map(|(i, x)| (x.refdes.clone(), i)).collect();
        let moving_set: BTreeSet<String> = index.keys().cloned().collect();
        let mut fixed = BTreeMap::new();
        let mut net_pins: Vec<Vec<Pin>> = vec![Vec::new(); net_names.len()];
        for pp in placed_pads(p) {
            if moving_set.contains(&pp.refdes) {
                continue;
            }
            let net = pp.net.as_ref().and_then(|n| net_index.get(n).copied());
            if let Some(n) = net {
                net_pins[n].push(Pin::Fixed(pp.center));
            }
            fixed.insert((pp.refdes.clone(), pp.number.clone()), (pp.center, net));
        }
        let mut part_nets = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let mut ns = BTreeSet::new();
            for (k, (_, n)) in part.pads.iter().enumerate() {
                if let Some(n) = n {
                    net_pins[*n].push(Pin::Part(i, k));
                    ns.insert(*n);
                }
            }
            part_nets.push(ns.into_iter().collect());
        }
        let edge = b.rules.copper_to_edge.0.max(gap);
        let free = [BoardSide::Top, BoardSide::Bottom]
            .map(|s| free_area(p, s, gap, edge, &moving_set).ok_or_else(PlaceError::no_outline));
        let [top, bottom] = free;
        let (top, bottom) = (top?, bottom?);
        let fb = polyclip::Geometry::bbox(&top);
        let center = fb.map(|r| Point::new(Nm((r.min.x + r.max.x) / 2), Nm((r.min.y + r.max.y) / 2)));
        let n = parts.len();
        let pair_nets: Vec<(usize, usize)> = {
            let net_index: BTreeMap<&str, usize> = net_names.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
            p.circuit()
                .diffpairs
                .values()
                .filter_map(|d| Some((*net_index.get(d.p.as_str())?, *net_index.get(d.n.as_str())?)))
                .collect()
        };
        let pair_end: Vec<bool> = parts
            .iter()
            .map(|part| {
                part.kind != Kind::Passive
                    && pair_nets.iter().any(|&(a, b)| {
                        part.pads.iter().any(|x| x.1 == Some(a)) && part.pads.iter().any(|x| x.1 == Some(b))
                    })
            })
            .collect();
        Ok(Engine {
            p,
            gap,
            parts,
            index,
            poses: vec![None; n],
            net_names,
            net_pins,
            part_nets,
            fixed,
            free: [top, bottom],
            board,
            center: center.unwrap_or_else(|| board.center()),
            tethers: Vec::new(),
            rooms: vec![None; n],
            crowd: (Vec::new(), 0.0),
            pair_zones: Vec::new(),
            pair_nets,
            pair_end,
            spread: silk, // the same flag: spread placement also boxes silkscreen
        })
    }

    fn pin_pos(&self, pin: Pin) -> Option<Point> {
        match pin {
            Pin::Fixed(q) => Some(q),
            Pin::Part(i, k) => self.poses[i].map(|pose| self.parts[i].pad_at(&pose, k)),
        }
    }

    /// Position of a pad of any footprint (placed fixed, or part being placed).
    fn pad_pos(&self, refdes: &str, pad: &str) -> Option<Point> {
        if let Some(&i) = self.index.get(refdes) {
            let k = self.parts[i].pads.iter().position(|(n, _)| n == pad)?;
            return self.pin_pos(Pin::Part(i, k));
        }
        self.fixed.get(&(refdes.to_string(), pad.to_string())).map(|x| x.0)
    }

    /// Minimum spanning tree length of a net over its placed pins.
    fn net_cost(&self, n: usize) -> f64 {
        let pts: Vec<Point> = self.net_pins[n].iter().filter_map(|&pin| self.pin_pos(pin)).collect();
        mst::mst_length(&pts)
    }

    fn nets_cost(&self, nets: &[usize]) -> f64 {
        nets.iter().map(|&n| self.net_cost(n)).sum()
    }

    /// Improvement cost around `parts`: ratsnest of `nets` plus the tethers touching them.
    fn local_cost(&self, parts: &[usize], nets: &[usize]) -> f64 {
        self.local_cost_with(parts, self.nets_cost(nets))
    }

    /// [`Engine::local_cost`] with the ratsnest of the nets given.
    fn local_cost_with(&self, parts: &[usize], nets_cost: f64) -> f64 {
        let mut c = nets_cost;
        for &(i, k, target, w) in &self.tethers {
            let touches = parts.contains(&i) || matches!(target, Pin::Part(j, _) if parts.contains(&j));
            if touches && let (Some(a), Some(b)) = (self.pin_pos(Pin::Part(i, k)), self.pin_pos(target)) {
                c += w * dist(a, b);
            }
        }
        c
    }

    /// Total ratsnest length (MST per net over placed pins).
    fn total(&self) -> f64 {
        (0..self.net_names.len()).map(|n| self.net_cost(n)).sum()
    }

    fn is_ground(&self, n: usize) -> bool {
        is_ground(&self.net_names[n])
    }

    /// Distance from a point to the nearest placed pin of net `n` not on part `skip`.
    fn nearest(&self, n: usize, from: Point, skip: usize) -> Option<f64> {
        self.net_pins[n]
            .iter()
            .filter(|pin| !matches!(pin, Pin::Part(i, _) if *i == skip))
            .filter_map(|&pin| self.pin_pos(pin))
            .map(|q| dist(q, from))
            .min_by(|a, b| a.total_cmp(b))
    }

    /// Attraction of part `i` at `pose`: weighted distance of each pad to the nearest placed
    /// pin of its net (ground counts less: it is usually poured).
    fn attraction(&self, i: usize, pose: &Pose) -> f64 {
        let part = &self.parts[i];
        let mut s = 0.0;
        for (k, (_, n)) in part.pads.iter().enumerate() {
            let Some(n) = *n else { continue };
            if let Some(d) = self.nearest(n, part.pad_at(pose, k), i) {
                s += if self.is_ground(n) { 0.3 * d } else { d };
            }
        }
        s
    }

    /// Whether part `i` fits at `pose`: inside the allowed area, clear of the other parts; a
    /// passive clear of the differential pairs' breakouts, and an end of a pair (moving) with
    /// its breakouts clear of the passives.
    fn valid(&self, i: usize, pose: &Pose) -> bool {
        let r = self.parts[i].rect(pose);
        if self.parts[i].kind == Kind::Passive
            && self.pair_zones.iter().any(|(side, z)| *side == pose.side && !r.clear_of(z, 0))
        {
            return false;
        }
        if self.pair_end[i] && self.poses[i].is_some_and(|q| q != *pose) {
            let zones = part_zones(self, i, pose);
            for (side, z) in &zones {
                for (j, q) in self.poses.iter().enumerate() {
                    if let Some(q) = q
                        && j != i
                        && self.parts[j].kind == Kind::Passive
                        && q.side == *side
                        && !self.parts[j].rect(q).clear_of(z, 0)
                    {
                        return false;
                    }
                }
            }
        }
        for (j, q) in self.poses.iter().enumerate() {
            if j == i {
                continue;
            }
            if let Some(q) = q
                && q.side == pose.side
                && !r.clear_of(&self.parts[j].rect(q), self.gap)
            {
                return false;
            }
        }
        polyclip::contains(&self.free[side_index(pose.side)], &r.ring())
    }

    /// The best valid pose among candidates, by score (ties: candidate order).
    fn best(&self, i: usize, cands: Vec<Pose>, score: impl Fn(&Pose) -> f64) -> Option<Pose> {
        let mut scored: Vec<(f64, usize, Pose)> =
            cands.into_iter().enumerate().map(|(k, c)| (score(&c), k, c)).collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|x| x.2).find(|c| self.valid(i, c))
    }

    /// Candidate origins on a `step` grid within `radius` of `around` (whole board when
    /// `None`), all four rotations.
    fn candidates(&self, side: BoardSide, around: Option<(Point, i64)>, step: i64) -> Vec<Pose> {
        let (x0, y0, x1, y1) = match around {
            Some((c, r)) => (c.x.0 - r, c.y.0 - r, c.x.0 + r, c.y.0 + r),
            None => (self.board.x0, self.board.y0, self.board.x1, self.board.y1),
        };
        let start = |v: i64| v.div_euclid(step) * step;
        let mut out = Vec::new();
        let mut y = start(y0);
        while y <= y1 {
            let mut x = start(x0);
            while x <= x1 {
                for rot in 0..4u8 {
                    out.push(Pose { at: Point::new(Nm(x), Nm(y)), rot, side });
                }
                x += step;
            }
            y += step;
        }
        out
    }

    /// Distance from a box to the side `edge` of the board's bounding box (the nearest side
    /// for `any`), and whether that side is vertical.
    fn edge_distance(&self, r: &Rect, edge: EdgeHint) -> (i64, bool) {
        let (d, side) = self.nearest_side(r, edge);
        (d, matches!(side, EdgeHint::Left | EdgeHint::Right))
    }

    /// The side of the board's bounding box `edge` means for a box (the nearest one for
    /// `any`), and the distance to it.
    fn nearest_side(&self, r: &Rect, edge: EdgeHint) -> (i64, EdgeHint) {
        let b = &self.board;
        let sides = [
            (EdgeHint::Left, r.x0 - b.x0),
            (EdgeHint::Right, b.x1 - r.x1),
            (EdgeHint::Bottom, r.y0 - b.y0),
            (EdgeHint::Top, b.y1 - r.y1),
        ];
        let (side, d) =
            sides.into_iter().filter(|s| edge == EdgeHint::Any || s.0 == edge).min_by_key(|s| s.1).expect("four sides");
        (d.max(0), side)
    }

    /// How far connector `i` at `pose` turns its body away from its edge: the body (courtyard
    /// center) should reach further towards the edge than the pads do, so a receptacle's
    /// opening faces out. Zero for symmetric parts (headers) and other kinds.
    fn facing(&self, i: usize, pose: &Pose, edge: EdgeHint) -> f64 {
        let part = &self.parts[i];
        if !self.spread || part.kind != Kind::Connector || part.pads.is_empty() {
            return 0.0;
        }
        let r = part.rect(pose);
        let (_, side) = self.nearest_side(&r, edge);
        let n = part.pads.len() as i64;
        let (mut sx, mut sy) = (0i64, 0i64);
        for k in 0..part.pads.len() {
            let q = part.pad_at(pose, k);
            sx += q.x.0;
            sy += q.y.0;
        }
        // From the pads to the body: the way the part opens.
        let c = r.center();
        let (ox, oy) = ((c.x.0 - sx / n) as f64, (c.y.0 - sy / n) as f64);
        let len = (ox * ox + oy * oy).sqrt();
        // Below 0.1 mm the part is symmetric for this purpose.
        if len < 100_000.0 {
            return 0.0;
        }
        let out = match side {
            EdgeHint::Left | EdgeHint::Any => -ox,
            EdgeHint::Right => ox,
            EdgeHint::Bottom => -oy,
            EdgeHint::Top => oy,
        };
        2.0 * n as f64 * (len - out)
    }

    /// Penalty of connector `i` at `pose` for crowding other connectors (placed ones being
    /// moved, and the fixed ones): per pad, how much closer than the crowding distance their
    /// centers are. Zero unless spreading, and for other parts.
    fn crowding(&self, i: usize, pose: &Pose) -> f64 {
        let (fixed, reach) = &self.crowd;
        if *reach <= 0.0 || self.parts[i].kind != Kind::Connector {
            return 0.0;
        }
        let c = self.parts[i].rect(pose).center();
        let others = self
            .poses
            .iter()
            .enumerate()
            .filter(|&(j, q)| j != i && q.is_some() && self.parts[j].kind == Kind::Connector)
            .filter_map(|(j, q)| q.map(|q| self.parts[j].rect(&q).center()))
            .chain(fixed.iter().copied());
        let close: f64 = others.map(|o| (reach - dist(c, o)).max(0.0)).sum();
        self.parts[i].pads.len().max(2) as f64 * close
    }

    /// Grid step for whole-board searches: at least 0.5 mm, about 4000 positions.
    fn coarse_step(&self) -> i64 {
        let a = self.board.w() as f64 * self.board.h() as f64;
        let s = (a / 4000.0).sqrt();
        ((s / GRID as f64).ceil() as i64 * GRID).max(500_000)
    }
}

/// How far a differential pair's breakout area reaches out from its pins.
const PAIR_BREAKOUT: i64 = 2_500_000;
/// Largest distance between the two pins of a pair end on one part.
const PAIR_END: i64 = 2_000_000;

/// The breakout areas (see [`pair_zones`]) of part `i` (not a passive) at `pose`.
fn part_zones(e: &Engine, i: usize, pose: &Pose) -> Vec<(BoardSide, Rect)> {
    let part = &e.parts[i];
    let mut out = Vec::new();
    for &(np, nn) in &e.pair_nets {
        let pick = |net: usize| -> Vec<Point> {
            part.pads.iter().enumerate().filter(|(_, x)| x.1 == Some(net)).map(|(k, _)| part.pad_at(pose, k)).collect()
        };
        let (pp, pn) = (pick(np), pick(nn));
        if pp.is_empty() || pn.is_empty() {
            continue;
        }
        end_zones(part.rect(pose).center(), pose.side, &pp, &pn, &mut out);
    }
    out
}

/// The breakout areas of the pair ends among pads `pp` (positive) and `pn` (negative) of a part
/// centered at `center`.
fn end_zones(center: Point, side: BoardSide, pp: &[Point], pn: &[Point], out: &mut Vec<(BoardSide, Rect)>) {
    for &a in pp {
        let Some(b) = pn
            .iter()
            .copied()
            .filter(|b| dist(a, *b) <= PAIR_END as f64)
            .min_by(|x, y| dist(a, *x).total_cmp(&dist(a, *y)).then(x.cmp(y)))
        else {
            continue;
        };
        let mid = Point::new(Nm((a.x.0 + b.x.0) / 2), Nm((a.y.0 + b.y.0) / 2));
        let (dx, dy) = (mid.x.0 - center.x.0, mid.y.0 - center.y.0);
        let mut z = Rect::of([a, b]).expect("two points").expand(500_000);
        if dx.abs() >= dy.abs() {
            if dx >= 0 { z.x1 += PAIR_BREAKOUT } else { z.x0 -= PAIR_BREAKOUT }
        } else if dy >= 0 {
            z.y1 += PAIR_BREAKOUT;
        } else {
            z.y0 -= PAIR_BREAKOUT;
        }
        out.push((side, z));
    }
}

/// Breakout areas of the differential pairs on the anchors placed so far (and the footprints
/// that stay): for each part with pads of both nets close together (an end), the box of the
/// two pads (grown by 0.5 mm) stretched `PAIR_BREAKOUT` away from the part's center, along
/// the axis the pads face.
fn pair_zones(e: &Engine) -> Vec<(BoardSide, Rect)> {
    let mut out = Vec::new();
    for &(np, nn) in &e.pair_nets {
        // (part center, side, positive pads, negative pads) per part.
        let mut parts: Vec<(Point, BoardSide, Vec<Point>, Vec<Point>)> = Vec::new();
        for (i, part) in e.parts.iter().enumerate() {
            let Some(pose) = e.poses[i].filter(|_| part.kind != Kind::Passive) else { continue };
            let pick = |net: usize| -> Vec<Point> {
                part.pads
                    .iter()
                    .enumerate()
                    .filter(|(_, x)| x.1 == Some(net))
                    .map(|(k, _)| part.pad_at(&pose, k))
                    .collect()
            };
            parts.push((part.rect(&pose).center(), pose.side, pick(np), pick(nn)));
        }
        let mut fixed: BTreeMap<&str, (Vec<Point>, Vec<Point>)> = BTreeMap::new();
        for ((r, _), (at, net)) in &e.fixed {
            let entry = fixed.entry(r.as_str()).or_default();
            if *net == Some(np) {
                entry.0.push(*at);
            } else if *net == Some(nn) {
                entry.1.push(*at);
            }
        }
        for (r, (pp, pn)) in fixed {
            let side = e.p.board().footprints.get(r).map_or(BoardSide::Top, |f| f.side);
            if let Some(b) = courtyard_box(e.p, r) {
                parts.push((b.center(), side, pp, pn));
            }
        }
        for (center, side, pp, pn) in parts {
            end_zones(center, side, &pp, &pn, &mut out);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Grouping

/// Where a passive goes: next to a pad of another part (anchor or passive).
#[derive(Clone, Debug)]
struct Target {
    refdes: String,
    pad: String,
    /// Pad of the passive that connects there.
    own_pad: usize,
    /// Decoupling capacitor (pulled harder to its pin).
    decap: bool,
}

/// Net classification for grouping.
struct NetInfo {
    power: Vec<bool>,
    ground: Vec<bool>,
}

fn net_info(e: &Engine) -> NetInfo {
    let p = e.p;
    let c = p.circuit();
    let lib = p.library();
    let mut power = Vec::new();
    let mut ground = Vec::new();
    for name in &e.net_names {
        let net = &c.nets[name];
        let kinds: Vec<PinKind> = net
            .pins
            .iter()
            .filter_map(|pin| {
                let comp = c.components.get(&pin.refdes)?;
                lib.parts.get(&comp.part)?.symbol.pins.iter().find(|s| s.number == pin.pin).map(|s| s.kind)
            })
            .collect();
        let g = is_ground(name);
        ground.push(g);
        power.push(!g && (net.driven || kinds.iter().any(|k| matches!(k, PinKind::PowerIn | PinKind::PowerOut))));
    }
    NetInfo { power, ground }
}

/// Anchor pads per (anchor, net) with the pin kind, for every anchor on the board (parts being
/// placed and fixed ones).
struct AnchorPads {
    /// anchor designator → [(pad number, net index, pin kind)].
    pads: BTreeMap<String, Vec<(String, usize, PinKind)>>,
    /// anchor designator → (is IC, pin count).
    info: BTreeMap<String, (bool, usize)>,
    /// Anchors that are regulators (LDOs, switchers): their input takes the first capacitor
    /// of a rail.
    regulators: BTreeSet<String>,
}

fn anchor_pads(e: &Engine) -> AnchorPads {
    let p = e.p;
    let c = p.circuit();
    let lib = p.library();
    let net_index: BTreeMap<&str, usize> = e.net_names.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
    let mut pads = BTreeMap::new();
    let mut info = BTreeMap::new();
    let mut regulators = BTreeSet::new();
    for (r, comp) in &c.components {
        let placed = e.index.contains_key(r) || p.board().footprints.contains_key(r);
        if !placed {
            continue;
        }
        let Some(part) = lib.parts.get(&comp.part) else { continue };
        let connector = part.category == Category::Connector;
        let n = part.symbol.pins.len();
        if !connector && n <= 2 {
            continue;
        }
        let fref = part.footprint();
        let mut v = Vec::new();
        for sp in &part.symbol.pins {
            let Some(net) = c.net_of(&PinRef::new(r, &sp.number)) else { continue };
            let Some(&ni) = net_index.get(net) else { continue };
            let pad = fref.map(|f| f.pads_for(&sp.number)).unwrap_or_else(|| vec![sp.number.clone()]);
            if let Some(pad) = pad.into_iter().next() {
                v.push((pad, ni, sp.kind));
            }
        }
        pads.insert(r.clone(), v);
        info.insert(r.clone(), (!connector, n));
        if matches!(part.category, Category::Ldo | Category::Regulator) {
            regulators.insert(r.clone());
        }
    }
    AnchorPads { pads, info, regulators }
}

/// Anchor preference key: ICs before connectors, then more pins, then designator.
type Rank = (bool, std::cmp::Reverse<usize>, String);

/// Decoupling target preference: (capacitors already there, not a power input, not a regulator
/// input, anchor rank).
type DecapKey = (usize, u8, bool, Rank);

/// Assigns every passive being placed a target: decoupling capacitors to the power pin of an
/// anchor on their rail, other passives to an anchor (or a passive already assigned) sharing a
/// signal net, then a power net.
/// When spreading, a rail's first capacitor goes to a regulator's input, and passives without a
/// signal net go to the smallest power net they share with an anchor (a buck's inductor at its
/// switch pin); the packed placement keeps D50's choices.
fn assign(e: &Engine, nets: &NetInfo, ap: &AnchorPads) -> BTreeMap<usize, Target> {
    let mut out: BTreeMap<usize, Target> = BTreeMap::new();
    // Anchor preference: ICs, then more pins, then designator.
    let anchor_rank = |r: &str| {
        let (ic, n) = ap.info[r];
        (!ic, std::cmp::Reverse(n), r.to_string())
    };
    let mut passives: Vec<usize> = (0..e.parts.len()).filter(|&i| e.parts[i].kind == Kind::Passive).collect();
    passives.sort_by(|&a, &b| natural_cmp(&e.parts[a].refdes, &e.parts[b].refdes));
    let mut decaps_on: BTreeMap<(String, String), usize> = BTreeMap::new();
    // 1. Decoupling capacitors and passives on a signal net of an anchor.
    for &i in &passives {
        let part = &e.parts[i];
        let pn: Vec<Option<usize>> = part.pads.iter().map(|x| x.1).collect();
        let has_ground = pn.iter().flatten().any(|&n| nets.ground[n]);
        let rail = pn.iter().enumerate().find_map(|(k, n)| n.filter(|&n| nets.power[n]).map(|n| (k, n)));
        if has_ground && let Some((own_pad, rail)) = rail {
            // Power pins on the rail: inputs first, then outputs (regulator output caps).
            let mut best: Option<(DecapKey, String, String)> = None;
            for (a, pads) in &ap.pads {
                for (pad, n, kind) in pads {
                    if *n != rail || !matches!(kind, PinKind::PowerIn | PinKind::PowerOut) {
                        continue;
                    }
                    let used = decaps_on.get(&(a.clone(), pad.clone())).copied().unwrap_or(0);
                    let key = (
                        used,
                        u8::from(*kind != PinKind::PowerIn),
                        !(e.spread && *kind == PinKind::PowerIn && ap.regulators.contains(a)),
                        anchor_rank(a),
                    );
                    if best.as_ref().is_none_or(|b| key < b.0) {
                        best = Some((key, a.clone(), pad.clone()));
                    }
                }
            }
            if let Some((_, a, pad)) = best {
                *decaps_on.entry((a.clone(), pad.clone())).or_default() += 1;
                out.insert(i, Target { refdes: a, pad, own_pad, decap: true });
                continue;
            }
        }
        // A signal net shared with an anchor.
        let mut best: Option<(Rank, Target)> = None;
        for (k, n) in pn.iter().enumerate() {
            let Some(n) = *n else { continue };
            if nets.power[n] || nets.ground[n] {
                continue;
            }
            for (a, pads) in &ap.pads {
                if let Some((pad, _, _)) = pads.iter().find(|x| x.1 == n) {
                    let key = anchor_rank(a);
                    if best.as_ref().is_none_or(|b| key < b.0) {
                        best = Some((key, Target { refdes: a.clone(), pad: pad.clone(), own_pad: k, decap: false }));
                    }
                }
            }
        }
        if let Some((_, t)) = best {
            out.insert(i, t);
        }
    }
    // 2. Chains: a signal net shared with an assigned passive.
    loop {
        let mut changed = false;
        for &i in &passives {
            if out.contains_key(&i) {
                continue;
            }
            let part = &e.parts[i];
            'pads: for (k, (_, n)) in part.pads.iter().enumerate() {
                let Some(n) = *n else { continue };
                if nets.power[n] || nets.ground[n] {
                    continue;
                }
                for &j in &passives {
                    if j == i || !out.contains_key(&j) {
                        continue;
                    }
                    if let Some((pad, _)) = e.parts[j].pads.iter().find(|x| x.1 == Some(n)) {
                        let t =
                            Target { refdes: e.parts[j].refdes.clone(), pad: pad.clone(), own_pad: k, decap: false };
                        out.insert(i, t);
                        changed = true;
                        break 'pads;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    // 3. A power net (not ground) shared with an anchor: the smallest net first (a switching
    // regulator's inductor belongs at its switch pin, not at the largest IC on the output rail).
    for &i in &passives {
        if out.contains_key(&i) {
            continue;
        }
        let part = &e.parts[i];
        let mut best: Option<((usize, Rank), Target)> = None;
        for (k, (_, n)) in part.pads.iter().enumerate() {
            let Some(n) = *n else { continue };
            if nets.ground[n] {
                continue;
            }
            for (a, pads) in &ap.pads {
                if let Some((pad, _, _)) = pads.iter().find(|x| x.1 == n) {
                    let key = (if e.spread { e.net_pins[n].len() } else { 0 }, anchor_rank(a));
                    if best.as_ref().is_none_or(|b| key < b.0) {
                        best = Some((key, Target { refdes: a.clone(), pad: pad.clone(), own_pad: k, decap: false }));
                    }
                }
            }
        }
        if let Some((_, t)) = best {
            out.insert(i, t);
        }
    }
    out
}

/// Satellites among the ICs being placed: an IC whose signal nets (neither power nor ground)
/// reach exactly one other anchor, an IC with more pins, is that IC's satellite.
fn satellites(e: &Engine, nets: &NetInfo, ap: &AnchorPads) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for part in &e.parts {
        let r = &part.refdes;
        if part.kind != Kind::Ic {
            continue;
        }
        let Some(pads) = ap.pads.get(r) else { continue };
        let signal: BTreeSet<usize> = pads.iter().map(|x| x.1).filter(|&n| !nets.power[n] && !nets.ground[n]).collect();
        if signal.is_empty() {
            continue;
        }
        let others: BTreeSet<&String> = ap
            .pads
            .iter()
            .filter(|(a, v)| *a != r && v.iter().any(|x| signal.contains(&x.1)))
            .map(|(a, _)| a)
            .collect();
        if let [m] = others.into_iter().collect::<Vec<_>>()[..]
            && ap.info.get(m).is_some_and(|&(ic, pins)| ic && pins > ap.info[r].1)
        {
            out.insert(r.clone(), m.clone());
        }
    }
    out
}

/// Root anchor of a passive's target chain.
fn root_of<'t>(e: &Engine, targets: &'t BTreeMap<usize, Target>, i: usize) -> Option<&'t str> {
    let mut cur = i;
    for _ in 0..e.parts.len() + 1 {
        let t = targets.get(&cur)?;
        match e.index.get(&t.refdes) {
            Some(&j) if e.parts[j].kind == Kind::Passive => cur = j,
            _ => return Some(t.refdes.as_str()),
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Automatic placement

/// Options of [`auto_place`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoOptions {
    /// Courtyard-to-courtyard clearance.
    pub gap: Nm,
    /// Spread groups over the board (else pack them).
    pub spread: bool,
    /// Share of the free board area the groups' rooms take when spreading, in percent
    /// (default [`SPREAD_DENSITY`]).
    pub density: Option<u32>,
    /// Edge hints by designator (`place.hint`): the part goes to that edge of the board.
    pub edges: BTreeMap<String, EdgeHint>,
}

impl Default for AutoOptions {
    fn default() -> Self {
        AutoOptions { gap: DEFAULT_GAP, spread: true, density: None, edges: BTreeMap::new() }
    }
}

/// Result of [`auto_place`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoPlacement {
    /// New placements by designator.
    pub placements: BTreeMap<String, PlacedFootprint>,
    /// Components that found no room.
    pub unplaced: Vec<String>,
}

/// Places footprints by groups: `moving` (designators) are placed, every other footprint
/// stays and is an obstacle. Anchors (ICs and connectors) are spread over the board, largest
/// IC central, connectors at the edges, each with room around it for its passives; passives go
/// next to the anchor pin they connect to (decoupling capacitors next to their power pin),
/// rotated to face it; then local moves (shifts, rotations, swaps of identical footprints)
/// shorten the total ratsnest. `gap` is the courtyard-to-courtyard clearance.
///
/// With `spread`, each group's room grows with the free board area (up to [`SPREAD_DENSITY`]
/// of it in all, or `density`), anchors keep to that room during improvement, and connectors
/// keep apart from each other along the edges, so a board larger than its parts is used
/// rather than packed into one cluster; then the groups' arrangement is stretched where it
/// leaves a wide band of the board empty (an eighth of the board at the default density, more
/// at lower ones) and groups move as
/// blocks where that shortens the ratsnest or evens out the board (`spread` module). Without it groups are packed as tightly as their
/// passives allow. Parts with an edge hint go to that edge (two-terminal parts then are
/// placed like anchors rather than next to a pin) and only move along it or closer.
pub fn auto_place(p: &Project, moving: &[String], o: &AutoOptions) -> Result<AutoPlacement, PlaceError> {
    let spread = o.spread;
    let mut e = Engine::new(p, moving, o.gap.0.max(0), spread, &o.edges)?;
    let nets = net_info(&e);
    let ap = anchor_pads(&e);
    let targets = assign(&e, &nets, &ap);
    let n = e.parts.len();

    // Satellites (when spreading): small ICs whose signal nets reach one larger IC only (a
    // crystal, a buffer next to an MCU). They share that IC's room instead of claiming their own.
    let satellite: BTreeMap<String, String> = if spread { satellites(&e, &nets, &ap) } else { BTreeMap::new() };
    let group_of = |r: &str| satellite.get(r).cloned().unwrap_or_else(|| r.to_string());

    // Room around each anchor for its passives (and satellites).
    let mut group_area: BTreeMap<String, f64> = BTreeMap::new();
    let mut parts_area = 0.0;
    for i in 0..n {
        let r = e.parts[i].rel[0][0].0;
        let a = ((r.w() + e.gap) as f64) * ((r.h() + e.gap) as f64);
        parts_area += a;
        let root = if e.parts[i].kind == Kind::Passive {
            root_of(&e, &targets, i)
        } else {
            satellite.get(&e.parts[i].refdes).map(String::as_str)
        };
        if let Some(root) = root {
            *group_area.entry(group_of(root)).or_default() += a;
        }
    }
    // Spread: the share of the free area each unit of part area gets as room (1 when packed),
    // at most SPREAD_MAX: beyond that groups drift apart for nothing.
    let free_area = polyclip::area2(&e.free[0]) as f64 / 2.0;
    // An explicit density lifts the cap in proportion: 100 % may use the whole board.
    let (density, cap) = match o.density {
        Some(d) => {
            let d = f64::from(d.clamp(1, 100)) / 100.0;
            (d, SPREAD_MAX * (d / SPREAD_DENSITY).max(1.0))
        }
        None => (SPREAD_DENSITY, SPREAD_MAX),
    };
    let factor = if spread && parts_area > 0.0 { (free_area * density / parts_area).clamp(1.0, cap) } else { 1.0 };
    let max_halo = if spread { (e.board.w().min(e.board.h()) / 4).max(8_000_000) } else { 8_000_000 };
    let halo_of = |e: &Engine, i: usize| -> i64 {
        let r = e.parts[i].rel[0][0].0;
        let own = ((r.w() + e.gap) as f64) * ((r.h() + e.gap) as f64);
        let refdes = &e.parts[i].refdes;
        let passives = group_area.get(refdes).copied().unwrap_or(0.0);
        // Ring area around the courtyard: room for the passives, or the group's share of the board.
        let f = if satellite.contains_key(refdes) { 1.0 } else { factor };
        let a = (passives * 1.6).max((own + passives) * f - own);
        let s = (r.w() + r.h()) as f64;
        let h = (-s + (s * s + 4.0 * a).sqrt()) / 4.0;
        (h as i64).clamp(e.gap, max_halo)
    };
    // Connectors keep this far apart (center to center) when spreading: an eighth of the
    // board's perimeter, so a few connectors take different edges or ends of one.
    if spread {
        let fixed: Vec<Point> = p
            .board()
            .footprints
            .keys()
            .filter(|r| !e.index.contains_key(*r))
            .filter(|r| {
                p.circuit()
                    .components
                    .get(*r)
                    .and_then(|c| p.library().parts.get(&c.part))
                    .is_some_and(|x| x.category == Category::Connector)
            })
            .filter_map(|r| courtyard_box(p, r).map(|b| b.center()))
            .collect();
        e.crowd = (fixed, ((e.board.w() + e.board.h()) / 4) as f64);
    }

    // 1. Anchors, most connected to what is already placed first.
    let mut anchors: Vec<usize> = (0..n).filter(|&i| e.parts[i].kind != Kind::Passive).collect();
    anchors.sort_by(|&a, &b| {
        let (pa, pb) = (&e.parts[a], &e.parts[b]);
        (pa.kind != Kind::Ic)
            .cmp(&(pb.kind != Kind::Ic))
            .then(pb.pins.cmp(&pa.pins))
            .then(pb.area().cmp(&pa.area()))
            .then_with(|| natural_cmp(&pa.refdes, &pb.refdes))
    });
    let mut halos: Vec<(usize, Rect)> = Vec::new();
    let mut unplaced = Vec::new();
    let any_fixed = !e.fixed.is_empty();
    let mut first_ic = !any_fixed;
    while !anchors.is_empty() {
        // Next: most nets shared with placed pins (ties: the sort order above).
        let connected = |e: &Engine, i: usize| {
            e.part_nets[i]
                .iter()
                .filter(|&&nn| !e.is_ground(nn))
                .filter(|&&nn| {
                    e.net_pins[nn]
                        .iter()
                        .any(|&pin| !matches!(pin, Pin::Part(j, _) if j == i) && e.pin_pos(pin).is_some())
                })
                .count()
        };
        // Spreading: the largest IC first (it takes the center), and satellites after their IC.
        let waiting = |e: &Engine, i: usize| {
            satellite.get(&e.parts[i].refdes).is_some_and(|m| {
                e.index.get(m).is_some_and(|&j| e.poses[j].is_none() && anchors.contains(&j) && j != i)
            })
        };
        let k = if spread && halos.is_empty() && e.parts[anchors[0]].kind == Kind::Ic {
            0
        } else {
            (0..anchors.len())
                .filter(|&x| !waiting(&e, anchors[x]))
                .max_by(|&x, &y| connected(&e, anchors[x]).cmp(&connected(&e, anchors[y])).then(y.cmp(&x)))
                .unwrap_or(0)
        };
        let i = anchors.remove(k);
        let master = satellite.get(&e.parts[i].refdes).and_then(|m| e.index.get(m).copied());
        let halo = halo_of(&e, i);
        let step = e.coarse_step();
        let cands = e.candidates(BoardSide::Top, None, step);
        let kind = e.parts[i].kind;
        let center = e.center;
        let is_first = kind == Kind::Ic && (first_ic || (spread && halos.is_empty()));
        let edge = e.parts[i].edge_target();
        let score = |e: &Engine, pose: &Pose| -> f64 {
            let r = e.parts[i].rect(pose);
            let mut s = e.attraction(i, pose);
            match edge {
                None => s += dist(r.center(), center) * if is_first { 1.0 } else { 0.25 },
                Some(edge) => {
                    let (d, vertical) = e.edge_distance(&r, edge);
                    // Stronger than the pull of all its pads together: connectors stay at the edge.
                    s += 2.0 * e.parts[i].pads.len().max(2) as f64 * d as f64;
                    // Long side along the edge.
                    let across = if vertical { r.w() - r.h() } else { r.h() - r.w() };
                    s += across.max(0) as f64;
                    s += e.facing(i, pose, edge);
                    s += e.crowding(i, pose);
                }
            }
            s
        };
        let mut found = None;
        // A connector that opens on one side (a receptacle) faces its edge: those poses are
        // tried at every room size before the others.
        let facing_out: Vec<Pose> = match edge {
            Some(edge) => cands.iter().copied().filter(|c| e.facing(i, c, edge) < 1.0).collect(),
            None => Vec::new(),
        };
        let sets: Vec<&[Pose]> = if facing_out.is_empty() || facing_out.len() == cands.len() {
            vec![&cands]
        } else {
            vec![&facing_out, &cands]
        };
        'sets: for set in sets {
            for h in [halo, halo / 2, 0] {
                let fits = |pose: &Pose| {
                    let hr = e.parts[i].rect(pose).expand(h);
                    halos.iter().all(|(j, o)| Some(*j) == master || hr.clear_of(o, 0))
                };
                let c: Vec<Pose> = set.iter().copied().filter(|c| fits(c)).collect();
                if let Some(pose) = e.best(i, c, |c| score(&e, c)) {
                    found = Some((pose, h));
                    break 'sets;
                }
            }
        }
        match found {
            Some((pose, h)) => {
                e.poses[i] = Some(pose);
                let room = e.parts[i].rect(&pose).expand(h);
                halos.push((i, room));
                if spread && master.is_none() {
                    e.rooms[i] = Some(room);
                }
                if kind == Kind::Ic {
                    first_ic = false;
                }
            }
            None => unplaced.push(i),
        }
    }

    // 2. Passives, group by group (anchor order), decoupling capacitors first, then chains;
    // clear of the differential pairs' breakouts.
    if spread {
        e.pair_zones = pair_zones(&e);
    }
    let order_of: BTreeMap<String, usize> =
        halos.iter().enumerate().map(|(k, (i, _))| (e.parts[*i].refdes.clone(), k)).collect();
    let mut passives: Vec<usize> = (0..n).filter(|&i| e.parts[i].kind == Kind::Passive).collect();
    let depth = |i: usize| {
        let mut d = 0;
        let mut cur = i;
        while let Some(t) = targets.get(&cur) {
            match e.index.get(&t.refdes) {
                Some(&j) if e.parts[j].kind == Kind::Passive && d < n => {
                    cur = j;
                    d += 1;
                }
                _ => break,
            }
        }
        d
    };
    passives.sort_by_key(|&i| {
        let root = root_of(&e, &targets, i);
        let group = root.and_then(|r| order_of.get(r).copied()).unwrap_or(usize::MAX - 1);
        let group = if root.is_none() { usize::MAX } else { group };
        let decap = targets.get(&i).is_some_and(|t| t.decap);
        (group, depth(i), !decap, i)
    });
    for i in passives {
        // Side: the anchor's side.
        let side = targets
            .get(&i)
            .and_then(|t| p.board().footprints.get(&t.refdes).filter(|_| !e.index.contains_key(&t.refdes)))
            .map_or(BoardSide::Top, |pf| pf.side);
        let target = targets.get(&i).and_then(|t| e.pad_pos(&t.refdes, &t.pad).map(|q| (q, t.own_pad, t.decap)));
        let around = target.map(|t| t.0).or_else(|| {
            // Loose part: near the nearest placed pin of one of its nets, else the center.
            e.parts[i]
                .pads
                .iter()
                .filter_map(|x| x.1)
                .filter(|&nn| !e.is_ground(nn))
                .find_map(|nn| e.net_pins[nn].iter().find_map(|&pin| e.pin_pos(pin)))
        });
        let score = |e: &Engine, pose: &Pose| -> f64 {
            let mut s = e.attraction(i, pose);
            if let Some((q, own, decap)) = target {
                let d = dist(e.parts[i].pad_at(pose, own), q);
                s += d * if decap { 4.0 } else { 2.0 };
                s += 0.05 * dist(e.parts[i].rect(pose).center(), q);
            }
            s
        };
        let mut found = None;
        if let Some(c) = around {
            for r in [3_000_000, 8_000_000] {
                let cands = e.candidates(side, Some((c, r)), STEP);
                if let Some(pose) = e.best(i, cands, |c| score(&e, c)) {
                    found = Some(pose);
                    break;
                }
            }
        }
        if found.is_none() {
            let step = e.coarse_step();
            let cands = e.candidates(side, None, step);
            let center = e.center;
            found = e.best(i, cands, |c| score(&e, c) + 0.1 * dist(e.parts[i].rect(c).center(), center));
        }
        match found {
            Some(pose) => e.poses[i] = Some(pose),
            None => unplaced.push(i),
        }
    }

    // 3. Local improvement of the total ratsnest length, passives tethered to their pins.
    for (&i, t) in &targets {
        let target = match e.index.get(&t.refdes) {
            Some(&j) => e.parts[j].pads.iter().position(|(n, _)| *n == t.pad).map(|k| Pin::Part(j, k)),
            None => e.fixed.get(&(t.refdes.clone(), t.pad.clone())).map(|x| Pin::Fixed(x.0)),
        };
        if let Some(target) = target {
            e.tethers.push((i, t.own_pad, target, if t.decap { 2.0 } else { 0.5 }));
        }
    }
    // Spreading: the groups' arrangement is stretched over the board, then groups move (as
    // rigid blocks) where that shortens the ratsnest or evens out the board; the largest
    // stretch whose ratsnest stays within budget is kept.
    if spread {
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut slot: BTreeMap<String, usize> = BTreeMap::new();
        for (i, _) in &halos {
            let root = group_of(&e.parts[*i].refdes);
            let k = *slot.entry(root).or_insert_with(|| {
                groups.push(Vec::new());
                groups.len() - 1
            });
            groups[k].push(*i);
        }
        for i in (0..n).filter(|&i| e.parts[i].kind == Kind::Passive) {
            if let Some(&k) = root_of(&e, &targets, i).and_then(|r| slot.get(&group_of(r))) {
                groups[k].push(i);
            }
        }
        let win = spread::Windows::new(&e, parts_area, SPREAD_WINDOWS);
        let weight = SPREAD_EVEN * (e.board.w() + e.board.h()) as f64;
        // The central IC (placed first, at the center) stays.
        let pivot = groups.first().filter(|g| g.first().is_some_and(|&i| e.parts[i].kind == Kind::Ic)).map(|_| 0);
        let pinned: Vec<bool> = groups
            .iter()
            .enumerate()
            .map(|(k, g)| Some(k) == pivot || g.iter().any(|&i| spread::pair_end(&e, i)))
            .collect();
        let margin = (SPREAD_MARGIN * SPREAD_DENSITY / density).min(0.5);
        let budget = e.total() * (1.0 + SPREAD_RATSNEST * density / SPREAD_DENSITY);
        let start = (e.poses.clone(), e.rooms.clone(), e.pair_zones.clone());
        let mut kept = false;
        for fill in [1.0, 0.75, 0.5, 0.25, 0.0] {
            (e.poses, e.rooms, e.pair_zones) = start.clone();
            spread::stretch_groups(&mut e, &groups, &pinned, pivot, margin, fill);
            spread::relax_groups(&mut e, &groups, &pinned, pivot, &win, weight, 24);
            if e.total() <= budget {
                kept = true;
                break;
            }
        }
        if !kept {
            (e.poses, e.rooms, e.pair_zones) = start;
        }
    }
    improve(&mut e, 12);

    let mut placements = BTreeMap::new();
    for (i, pose) in e.poses.iter().enumerate() {
        if let Some(pose) = pose {
            placements.insert(e.parts[i].refdes.clone(), pose.placed());
        }
    }
    let mut unplaced: Vec<String> = unplaced.into_iter().map(|i| e.parts[i].refdes.clone()).collect();
    unplaced.sort_by(|a, b| natural_cmp(a, b));
    Ok(AutoPlacement { placements, unplaced })
}

/// Greedy local improvement: for each part in turn, the best valid shift (0.25 to 4 mm in
/// eight directions) or rotation that shortens the ratsnest, then swaps of parts with the
/// same footprint. Stops after `passes` or when nothing improves.
fn improve(e: &mut Engine, passes: usize) {
    const EPS: f64 = 1_000.0;
    let n = e.parts.len();
    // The ends of differential pairs move too: a move keeps their breakouts clear of passives
    // (`valid`), and the pair router takes them wherever they end up.
    for _ in 0..passes {
        let mut improved = false;
        for i in 0..n {
            let Some(cur) = e.poses[i] else { continue };
            let nets = e.part_nets[i].clone();
            let before = e.local_cost(&[i], &nets) + e.crowding(i, &cur);
            let mut moves: Vec<Pose> = Vec::new();
            for d in [250_000i64, 500_000, 1_000_000, 2_000_000, 4_000_000] {
                for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                    let at = Point::new(Nm(cur.at.x.0 + dx * d), Nm(cur.at.y.0 + dy * d));
                    moves.push(Pose { at, ..cur });
                }
            }
            for rot in 0..4u8 {
                if rot != cur.rot {
                    moves.push(Pose { rot, ..cur });
                }
            }
            // Connectors and parts with an edge hint only move along the edge or closer to it.
            if let Some(edge) = e.parts[i].edge_target() {
                let d0 = e.edge_distance(&e.parts[i].rect(&cur), edge).0;
                let f0 = e.facing(i, &cur, edge);
                moves.retain(|m| e.edge_distance(&e.parts[i].rect(m), edge).0 <= d0 && e.facing(i, m, edge) <= f0);
            }
            // Spread anchors stay in their room.
            if let Some(room) = e.rooms[i] {
                moves.retain(|m| {
                    let r = e.parts[i].rect(m);
                    r.x0 >= room.x0 && r.y0 >= room.y0 && r.x1 <= room.x1 && r.y1 <= room.y1
                });
            }
            let mut scored: Vec<(f64, usize, Pose)> = Vec::new();
            for (k, m) in moves.into_iter().enumerate() {
                e.poses[i] = Some(m);
                let after = e.local_cost(&[i], &nets) + e.crowding(i, &m);
                if after < before - EPS {
                    scored.push((after, k, m));
                }
            }
            e.poses[i] = Some(cur);
            scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            if let Some((_, _, m)) = scored.into_iter().find(|(_, _, m)| e.valid(i, m)) {
                e.poses[i] = Some(m);
                improved = true;
            }
        }
        // Net costs of the current poses, kept until a part of the net moves (the same values
        // `local_cost` would compute again for every pair).
        let mut cached: Vec<Option<f64>> = vec![None; e.net_names.len()];
        for i in 0..n {
            for j in i + 1..n {
                let (Some(a), Some(b)) = (e.poses[i], e.poses[j]) else { continue };
                // Parts with different edge hints keep their edges.
                if e.parts[i].footprint != e.parts[j].footprint
                    || a.side != b.side
                    || e.parts[i].edge != e.parts[j].edge
                {
                    continue;
                }
                let mut nets = e.part_nets[i].clone();
                nets.extend(e.part_nets[j].iter().copied());
                nets.sort_unstable();
                nets.dedup();
                let mut sum = 0.0;
                for &k in &nets {
                    sum += *cached[k].get_or_insert_with(|| e.net_cost(k));
                }
                let before = e.local_cost_with(&[i, j], sum);
                e.poses[i] = Some(b);
                e.poses[j] = Some(a);
                let after = e.local_cost(&[i, j], &nets);
                if after < before - EPS && e.valid(i, &b) && e.valid(j, &a) {
                    improved = true;
                    for &k in &nets {
                        cached[k] = None;
                    }
                } else {
                    e.poses[i] = Some(a);
                    e.poses[j] = Some(b);
                }
            }
        }
        if !improved {
            break;
        }
    }
}

/// Total ratsnest length of the placed footprints: minimum spanning tree per net over pad
/// centers, ignoring copper (an estimate of the routing effort of a placement).
pub fn ratsnest_length(p: &Project) -> Nm {
    match Engine::new(p, &[], 0, false, &BTreeMap::new()) {
        Ok(e) => Nm(e.total().round() as i64),
        Err(_) => Nm::ZERO,
    }
}

// ---------------------------------------------------------------------------------------------
// Place near

/// Which side of the target a part goes to (board view, Y up).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Smaller X.
    Left,
    /// Larger X.
    Right,
    /// Larger Y.
    #[serde(alias = "up", alias = "top")]
    Above,
    /// Smaller Y.
    #[serde(alias = "down", alias = "bottom")]
    Below,
}

/// What to place a part next to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NearTarget {
    /// A pad (by its pin) of a placed footprint.
    Pad {
        /// Designator.
        refdes: String,
        /// Pad number.
        pad: String,
    },
    /// A placed footprint.
    Part(String),
}

/// Places `refdes` next to a pad or footprint: outside the target's courtyard on `dir`
/// (default: the side the pin faces, or the best side for a footprint), `distance` between
/// courtyards, with its pad of the same net aligned with the target pin and facing it (the
/// rotation that minimizes that connection), sliding along the side or away from it until
/// the courtyard is clear of every other footprint and inside the board.
pub fn place_near(
    p: &Project,
    refdes: &str,
    target: &NearTarget,
    dir: Option<Direction>,
    distance: Nm,
) -> Result<PlacedFootprint, PlaceError> {
    let tref = match target {
        NearTarget::Pad { refdes, .. } | NearTarget::Part(refdes) => refdes.as_str(),
    };
    if tref == refdes {
        return Err(PlaceError::new(
            "place.invalid_target",
            format!("{refdes} cannot be placed next to itself"),
            "name another part or pin",
        ));
    }
    let not_placed = || {
        PlaceError::new(
            "place.not_placed",
            format!("{tref} is not placed"),
            format!("place it first: `place.set {tref} --at ...`"),
        )
    };
    let tpf = p.board().footprints.get(tref).ok_or_else(not_placed)?;
    let trect = courtyard_box(p, tref).ok_or_else(not_placed)?;
    let gap = distance.0.clamp(0, DEFAULT_GAP.0);
    let mut e = Engine::new(p, &[refdes.to_string()], gap, false, &BTreeMap::new())?;
    let Some(&i) = e.index.get(refdes) else {
        return Err(PlaceError::new(
            "place.no_footprint",
            format!("{refdes} has no footprint"),
            "give its part a footprint",
        ));
    };
    let side = p.board().footprints.get(refdes).map_or(tpf.side, |f| f.side);
    let (tpoint, tnets): (Point, Vec<usize>) = match target {
        NearTarget::Pad { pad, .. } => {
            let (q, net) = e.fixed.get(&(tref.to_string(), pad.clone())).copied().ok_or_else(|| {
                PlaceError::new("place.unknown_pad", format!("{tref} has no pad {pad}"), "check the pin name or number")
            })?;
            (q, net.into_iter().collect())
        }
        NearTarget::Part(_) => {
            let nets: BTreeSet<usize> =
                e.fixed.iter().filter(|((r, _), _)| r == tref).filter_map(|(_, (_, n))| *n).collect();
            (trect.center(), nets.into_iter().collect())
        }
    };
    // Connecting pad: same net as the target (signal before ground).
    let part = &e.parts[i];
    let own = part
        .pads
        .iter()
        .enumerate()
        .filter(|(_, (_, n))| n.is_some_and(|n| tnets.contains(&n)))
        .min_by_key(|(k, (_, n))| (n.is_some_and(|n| e.is_ground(n)), *k))
        .map(|(k, _)| k);
    let dirs: Vec<Direction> = match (dir, target) {
        (Some(d), _) => vec![d],
        (None, NearTarget::Pad { .. }) => {
            let c = trect.center();
            let (dx, dy) = (tpoint.x.0 - c.x.0, tpoint.y.0 - c.y.0);
            if dx == 0 && dy == 0 {
                vec![Direction::Right, Direction::Left, Direction::Above, Direction::Below]
            } else if dx.abs() >= dy.abs() {
                vec![if dx > 0 { Direction::Right } else { Direction::Left }]
            } else {
                vec![if dy > 0 { Direction::Above } else { Direction::Below }]
            }
        }
        (None, NearTarget::Part(_)) => vec![Direction::Right, Direction::Left, Direction::Above, Direction::Below],
    };
    let dd = distance.0.max(0);
    let mut best: Option<(f64, Pose)> = None;
    for d in dirs {
        for rot in 0..4u8 {
            let probe = Pose { at: Point::ORIGIN, rot, side };
            let rel = e.parts[i].rect(&probe);
            let pc = own.map_or(rel.center(), |k| e.parts[i].pad_at(&probe, k));
            let up = |v: i64| v.div_euclid(GRID) * GRID + if v.rem_euclid(GRID) == 0 { 0 } else { GRID };
            let down = |v: i64| v.div_euclid(GRID) * GRID;
            // Base origin and the outward/sliding unit vectors.
            let (bx, by, out, slide) = match d {
                Direction::Right => (up(trect.x1 + dd - rel.x0), snap(tpoint.y.0 - pc.y.0), (1, 0), (0, 1)),
                Direction::Left => (down(trect.x0 - dd - rel.x1), snap(tpoint.y.0 - pc.y.0), (-1, 0), (0, 1)),
                Direction::Above => (snap(tpoint.x.0 - pc.x.0), up(trect.y1 + dd - rel.y0), (0, 1), (1, 0)),
                Direction::Below => (snap(tpoint.x.0 - pc.x.0), down(trect.y0 - dd - rel.y1), (0, -1), (1, 0)),
            };
            let mut cands: Vec<(i64, i64, i64)> = Vec::new();
            for k in 0..=40i64 {
                for s in -40..=40i64 {
                    cands.push((s.abs() * 2 + k * 3, k, s));
                }
            }
            cands.sort();
            let found = cands.into_iter().find_map(|(_, k, s)| {
                let at =
                    Point::new(Nm(bx + (out.0 * k + slide.0 * s) * STEP), Nm(by + (out.1 * k + slide.1 * s) * STEP));
                let pose = Pose { at, rot, side };
                e.valid(i, &pose).then_some(pose)
            });
            let Some(pose) = found else { continue };
            e.poses[i] = Some(pose);
            let link = own.map_or_else(
                || dist(e.parts[i].rect(&pose).center(), tpoint),
                |k| dist(e.parts[i].pad_at(&pose, k), tpoint),
            );
            let score = link * 4.0 + e.attraction(i, &pose);
            e.poses[i] = None;
            if best.is_none_or(|b| score < b.0 - 1.0) {
                best = Some((score, pose));
            }
        }
    }
    let (_, pose) = best.ok_or_else(|| {
        PlaceError::new(
            "place.no_room",
            format!("no room for {refdes} next to {tref}"),
            "the part needs room clear of courtyards, holes and the tracks and vias already on the board (with the \
             board's clearance): choose another side or target, move nearby parts, let place.auto search the whole \
             board, or place it with place.set",
        )
    })?;
    let mut pf = pose.placed();
    if let Some(old) = p.board().footprints.get(refdes) {
        pf.footprint = old.footprint.clone();
    }
    Ok(pf)
}
