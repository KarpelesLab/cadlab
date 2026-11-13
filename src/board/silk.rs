//! Silkscreen geometry shared by the DRC, the legend writers and the renders: where each
//! footprint's reference designator is printed ([`ref_text`]), the silkscreen shapes by owner
//! ([`silk_shapes`], [`silk_texts`]), the solder mask openings silkscreen must stay off
//! ([`mask_openings`]), and the pass that moves reference designators to free positions
//! ([`arrange`]). See `docs/BOARD.md`, "Silkscreen".
//!
//! A reference designator is not a drawing of the footprint: it is generated, upright, centered
//! above the placed courtyard unless the placement carries a [`RefText`] (`silk.arrange`,
//! `silk.set`, a KiCad import). Every output asks [`ref_text`], so they all print it at the same
//! place.

use std::collections::{BTreeMap, BTreeSet};

use polyclip::{Circle, EndCap, Geometry, Join, Path, Polygon, PolygonSet, Rect, Ring};

use super::{COPPER_TOL, PlacedPad, contour_ring, footprint_for, fp_graphic_shape, pad_side, placed_courtyards};
use crate::geom::{Point, RTree};
use crate::model::Project;
use crate::model::board::{BoardSide, GraphicKind, RefText, SilkCheck};
use crate::model::footprint::{GraphicGeometry, GraphicLayer};
use crate::model::sections::natural_cmp;
use crate::units::{Angle, Nm};

/// Default cap height of reference designators.
pub const REF_SIZE: Nm = Nm(1_000_000);
/// Gap between the courtyard and a reference designator next to it.
pub const REF_GAP: Nm = Nm(300_000);
/// Smallest clearance [`arrange`] keeps to board edges and other silkscreen, whatever the
/// rules say (KiCad's default silkscreen clearance).
pub const ARRANGE_CLEARANCE: Nm = Nm(150_000);
/// Added to the clearances by [`arrange`]: other tools draw the same text with their own
/// stroke font, a little wider or narrower than cadlab's.
pub const ARRANGE_MARGIN: Nm = Nm(100_000);
/// How much farther than [`REF_GAP`] a reference designator may be moved from its footprint,
/// nearest first.
const FARTHER: [i64; 5] = [0, 250_000, 500_000, 750_000, 1_000_000];
/// Farther still, for a text that found no position nearer or inside its part: it may then
/// stand beyond a neighbour.
const FAR: [i64; 4] = [1_500_000, 2_000_000, 2_500_000, 3_000_000];
/// Step of the slide along a side of the footprint.
const SLIDE: i64 = 250_000;
/// Farthest slide from the middle of a side, in steps.
const SLIDE_STEPS: i64 = 8;
/// A text with this many free positions has enough to choose after the more constrained ones.
const ENOUGH: usize = 6;

fn pt(p: Point) -> polyclip::Point {
    polyclip::Point::new(p.x.0, p.y.0)
}

/// A text as printed on a silkscreen layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedText {
    /// Text center.
    pub at: Point,
    /// Cap height.
    pub size: Nm,
    /// Stroke width.
    pub width: Nm,
    /// Counter-clockwise rotation, as seen from the top.
    pub rotation: Angle,
    /// Board side (texts on the bottom are mirrored).
    pub side: BoardSide,
}

impl PlacedText {
    /// Whether the text is mirrored (bottom side).
    pub fn mirror(&self) -> bool {
        self.side == BoardSide::Bottom
    }

    /// The strokes of `text` as polylines ([`super::text_paths`]).
    pub fn paths(&self, text: &str) -> Vec<Vec<Point>> {
        super::text_paths(text, self.at, self.size, self.rotation, self.mirror())
    }

    /// The inked region of `text`: its strokes at the stroke width.
    pub fn shape(&self, text: &str) -> PolygonSet {
        stroke_paths(self.paths(text), self.width)
    }

    /// Bounding box of the inked region of `text` (`None` for a text without strokes).
    pub fn bbox(&self, text: &str) -> Option<Rect> {
        let h = (self.width.0 / 2).max(1);
        let mut pts = self.paths(text).into_iter().flatten();
        let first = pts.next()?;
        let (mut lo, mut hi) = (first, first);
        for q in pts {
            lo = Point::new(lo.x.min(q.x), lo.y.min(q.y));
            hi = Point::new(hi.x.max(q.x), hi.y.max(q.y));
        }
        Some(Rect::new(polyclip::Point::new(lo.x.0 - h, lo.y.0 - h), polyclip::Point::new(hi.x.0 + h, hi.y.0 + h)))
    }
}

/// Polylines stroked with round caps and joins.
fn stroke_paths(paths: Vec<Vec<Point>>, width: Nm) -> PolygonSet {
    let paths: Vec<Path> =
        paths.into_iter().filter(|p| !p.is_empty()).map(|p| Path(p.into_iter().map(pt).collect())).collect();
    if paths.is_empty() {
        return PolygonSet::new();
    }
    polyclip::offset_paths(&paths, (width.0 / 2).max(1), Join::Round, EndCap::Round, COPPER_TOL).unwrap_or_default()
}

fn stroke(points: Vec<polyclip::Point>, width: Nm) -> PolygonSet {
    polyclip::offset_paths(&vec![Path(points)], (width.0 / 2).max(1), Join::Round, EndCap::Round, COPPER_TOL)
        .unwrap_or_default()
}

/// Stroke width of a reference designator of cap height `size`: 15 % of it, at least the
/// minimum silkscreen width.
pub fn ref_stroke_width(p: &Project, size: Nm) -> Nm {
    Nm(size.0 * 15 / 100).max(p.board().rules.min_silk_width)
}

/// Bounding box (min, max) of the placed courtyard of `refdes`, else of its courtyard drawings.
fn courtyard_box(p: &Project, refdes: &str) -> Option<(Point, Point)> {
    let pf = p.board().footprints.get(refdes)?;
    let fp = footprint_for(p, refdes)?;
    let tf = super::transform(pf);
    let mut cy: Vec<Point> = fp.courtyard.iter().map(|&q| tf(q)).collect();
    if cy.is_empty() {
        // Only courtyard drawings (a footprint whose body is on its other side): around them.
        cy = placed_courtyards(p, refdes).iter().flat_map(|(r, _)| r.0.iter().map(|&q| Point::from(q))).collect();
    }
    let first = *cy.first()?;
    Some(cy.iter().fold((first, first), |(lo, hi), q| {
        (Point::new(lo.x.min(q.x), lo.y.min(q.y)), Point::new(hi.x.max(q.x), hi.y.max(q.y)))
    }))
}

/// Where a reference designator of cap height `size` goes without a [`RefText`] position:
/// centered above the placed courtyard's bounding box (else its courtyard drawings'), or on the
/// footprint origin without any.
pub fn default_ref_at(p: &Project, refdes: &str, size: Nm) -> Option<Point> {
    let pf = p.board().footprints.get(refdes)?;
    footprint_for(p, refdes)?;
    Some(match courtyard_box(p, refdes) {
        Some((lo, hi)) => Point::new(Nm((lo.x.0 + hi.x.0) / 2), hi.y + REF_GAP + Nm(size.0 / 2)),
        None => pf.at,
    })
}

/// Where the legend prints the reference designator of `refdes`, hidden ones included: the
/// placement's [`RefText`] where it says, else upright (mirrored on the bottom side) and
/// centered above the courtyard ([`default_ref_at`]). `None` when the footprint is not placed
/// or unknown.
pub fn ref_text_any(p: &Project, refdes: &str) -> Option<PlacedText> {
    let pf = p.board().footprints.get(refdes)?;
    let t = pf.ref_text.as_ref();
    let size = t.and_then(|t| t.size).unwrap_or(REF_SIZE);
    let at = match t.and_then(|t| t.offset) {
        Some(o) if footprint_for(p, refdes).is_some() => pf.at + o,
        _ => default_ref_at(p, refdes, size)?,
    };
    Some(PlacedText {
        at,
        size,
        width: ref_stroke_width(p, size),
        rotation: t.map_or(Angle::ZERO, |t| t.rotation),
        side: pf.side,
    })
}

/// The reference designator of `refdes` as printed ([`ref_text_any`]); `None` when it is
/// hidden too. The Gerber legend, IPC-2581, the renders and the KiCad export all draw this.
pub fn ref_text(p: &Project, refdes: &str) -> Option<PlacedText> {
    if p.board().footprints.get(refdes)?.ref_text.as_ref().is_some_and(|t| t.hidden) {
        return None;
    }
    ref_text_any(p, refdes)
}

/// How findings name a footprint's reference designator text: `text:U1/Reference` (the name
/// the KiCad export and import give the Reference field).
pub fn ref_text_label(refdes: &str) -> String {
    format!("text:{refdes}/Reference")
}

/// A silkscreen item: the drawings of one footprint, one board drawing, or one text.
#[derive(Clone, Debug)]
pub struct SilkShape {
    /// How findings name it: a designator (a footprint's drawings), `graphic#id` (a board
    /// drawing or text) or `text:U1/Reference`.
    pub label: String,
    /// The footprint it belongs to.
    pub footprint: Option<String>,
    /// Side.
    pub side: BoardSide,
    /// The text, for texts.
    pub text: Option<(String, PlacedText)>,
    /// Bounding box of the inked region.
    pub bbox: Option<Rect>,
    /// The inked region (for a text: empty until [`SilkShape::region`] builds it).
    pub shape: PolygonSet,
}

impl SilkShape {
    /// The inked region (a text's strokes are built here, on demand).
    pub fn region(&self) -> std::borrow::Cow<'_, PolygonSet> {
        match &self.text {
            Some((s, t)) => std::borrow::Cow::Owned(t.shape(s)),
            None => std::borrow::Cow::Borrowed(&self.shape),
        }
    }

    /// What it is, for messages: `silkscreen of J1`, `reference text of J1`, `text graphic#4`.
    pub fn describe(&self) -> String {
        match (&self.text, &self.footprint) {
            (Some(_), Some(r)) => format!("reference text of {r}"),
            (Some(_), None) => format!("text {}", self.label),
            (None, _) => format!("silkscreen of {}", self.label),
        }
    }
}

fn silk_side(layer: &str) -> Option<BoardSide> {
    match layer {
        "F.SilkS" => Some(BoardSide::Top),
        "B.SilkS" => Some(BoardSide::Bottom),
        _ => None,
    }
}

/// Silkscreen drawings by owner and side: the drawings of each footprint as one item
/// (designator order), then each board line and polygon (`graphic#id`). Texts are in
/// [`silk_texts`].
pub fn silk_shapes(p: &Project) -> Vec<SilkShape> {
    let mut out = Vec::new();
    let board = p.board();
    for (r, pf) in &board.footprints {
        let Some(fp) = footprint_for(p, r) else { continue };
        let tf = super::transform(pf);
        let mut set = PolygonSet::new();
        for g in fp.graphics.iter().filter(|g| g.layer == GraphicLayer::Silk) {
            match &g.geometry {
                GraphicGeometry::Path { points } => {
                    set.extend(stroke(points.iter().map(|q| pt(tf(*q))).collect(), g.width));
                }
                GraphicGeometry::Polygon { points } => {
                    let mut v: Vec<polyclip::Point> = points.iter().map(|q| pt(tf(*q))).collect();
                    if let Some(&f) = v.first() {
                        v.push(f);
                    }
                    set.extend(stroke(v, g.width));
                }
                GraphicGeometry::Circle { center, radius, filled } => {
                    let c = pt(tf(*center));
                    if *filled {
                        let ring = Circle { center: c, radius: radius.0 + g.width.0 / 2 }.to_ring(COPPER_TOL);
                        set.extend(ring.ok().map(|r| Polygon::new(r, vec![])));
                    } else if let Ok(r) = (Circle { center: c, radius: radius.0 }).to_ring(COPPER_TOL) {
                        set.extend(stroke(Path::from(r).0, g.width));
                    }
                }
            }
        }
        if !set.is_empty() {
            out.push(SilkShape {
                label: r.clone(),
                footprint: Some(r.clone()),
                side: pf.side,
                text: None,
                bbox: set.bbox(),
                shape: set,
            });
        }
    }
    for g in &board.graphics {
        let Some(side) = silk_side(&g.layer) else { continue };
        let set = match &g.kind {
            GraphicKind::Line { points, width } => stroke(points.iter().map(|q| pt(*q)).collect(), *width),
            GraphicKind::Polygon { points, width } => super::polygon_area(points, *width),
            GraphicKind::Text { .. } => continue,
        };
        if !set.is_empty() {
            out.push(SilkShape {
                label: format!("graphic#{}", g.id.0),
                footprint: None,
                side,
                text: None,
                bbox: set.bbox(),
                shape: set,
            });
        }
    }
    out
}

/// Silkscreen texts: the printed reference designator of each footprint (designator order,
/// `text:U1/Reference`), then the board texts on silkscreen layers (`graphic#id`). Their
/// regions are built on demand ([`SilkShape::region`]).
pub fn silk_texts(p: &Project) -> Vec<SilkShape> {
    let mut out = Vec::new();
    let board = p.board();
    for (r, pf) in &board.footprints {
        let Some(t) = ref_text(p, r) else { continue };
        out.push(SilkShape {
            label: ref_text_label(r),
            footprint: Some(r.clone()),
            side: pf.side,
            bbox: t.bbox(r),
            text: Some((r.clone(), t)),
            shape: PolygonSet::new(),
        });
    }
    for g in &board.graphics {
        let Some(side) = silk_side(&g.layer) else { continue };
        let GraphicKind::Text { text, at, size, rotation } = &g.kind else { continue };
        let t =
            PlacedText { at: *at, size: *size, width: super::text_stroke_width(p, *size), rotation: *rotation, side };
        out.push(SilkShape {
            label: format!("graphic#{}", g.id.0),
            footprint: None,
            side,
            bbox: t.bbox(text),
            text: Some((text.clone(), t)),
            shape: PolygonSet::new(),
        });
    }
    out
}

/// A solder mask opening the silkscreen is checked against.
pub enum Opening<'a> {
    /// A pad's opening (or its copper, with `silk_check: copper`).
    Pad(&'a PlacedPad),
    /// A mask drawing: owner (designator or `graphic#id`).
    Drawn(String),
}

/// Solder mask openings on each side, by owner, with their regions: pads with an opening on
/// that side (grown by their mask margin with `silk_check: mask`, their copper with `copper`),
/// then, with `mask`, footprint and board mask drawings.
pub fn mask_openings<'a>(p: &Project, pads: &'a [PlacedPad]) -> Vec<(Opening<'a>, BoardSide, PolygonSet)> {
    let board = p.board();
    let mask = board.rules.silk_check == SilkCheck::Mask;
    let mut out = Vec::new();
    for side in [BoardSide::Top, BoardSide::Bottom] {
        let layer = if side == BoardSide::Top { "F.Cu" } else { "B.Cu" };
        for pp in pads.iter().filter(|pp| pp.mask_on(side)) {
            if !mask {
                if pp.layers.iter().any(|l| l == layer) {
                    out.push((Opening::Pad(pp), side, vec![pp.shape_on(layer).clone()]));
                }
                continue;
            }
            let m = pp.overrides.mask_margin.unwrap_or(board.rules.mask_expansion).0;
            let shape = if m == 0 {
                vec![pp.shape_on(layer).clone()]
            } else {
                let tol =
                    polyclip::ArcTol::new(1_000, if m > 0 { polyclip::Side::Outside } else { polyclip::Side::Inside });
                polyclip::offset(pp.shape_on(layer), m, Join::Round, tol).unwrap_or_default()
            };
            if !shape.is_empty() {
                out.push((Opening::Pad(pp), side, shape));
            }
        }
    }
    if !mask {
        return out;
    }
    for (r, pf) in &board.footprints {
        let Some(fp) = footprint_for(p, r) else { continue };
        for g in fp.graphics.iter().filter(|g| g.layer == GraphicLayer::Mask) {
            let shape = fp_graphic_shape(pf, g);
            if !shape.is_empty() {
                out.push((Opening::Drawn(r.clone()), pad_side(pf.side, g.back), shape));
            }
        }
    }
    for g in &board.graphics {
        let side = match g.layer.as_str() {
            "F.Mask" => BoardSide::Top,
            "B.Mask" => BoardSide::Bottom,
            _ => continue,
        };
        let set = match &g.kind {
            GraphicKind::Line { points, width } => stroke(points.iter().map(|q| pt(*q)).collect(), *width),
            GraphicKind::Polygon { points, width } => super::polygon_area(points, *width),
            GraphicKind::Text { .. } => continue,
        };
        if !set.is_empty() {
            out.push((Opening::Drawn(format!("graphic#{}", g.id.0)), side, set));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Arrangement

/// The box [`arrange`] reserves for `text` at cap height `size` and stroke `width`, centered on
/// `at` and rotated: as high as the capitals, as wide as cadlab's stroke font draws the text or
/// one cap height per character, whichever is wider (other tools' stroke fonts), plus the
/// stroke.
pub fn text_box(text: &str, at: Point, size: Nm, width: Nm, rotation: Angle) -> Polygon {
    let (w, h) = text_extent(text, size, width);
    let (s, c) = rotation.to_rad_f64().sin_cos();
    let corner = |x: i64, y: i64| {
        let (x, y) = (x as f64, y as f64);
        polyclip::Point::new(at.x.0 + (x * c - y * s).round() as i64, at.y.0 + (x * s + y * c).round() as i64)
    };
    Polygon::new(
        Ring(vec![corner(-w / 2, -h / 2), corner(w / 2, -h / 2), corner(w / 2, h / 2), corner(-w / 2, h / 2)]),
        vec![],
    )
}

/// Width and height of [`text_box`] before rotation.
fn text_extent(text: &str, size: Nm, width: Nm) -> (i64, i64) {
    let drawn = crate::render::font::width(text, size.0 as f64).ceil() as i64;
    let chars = text.chars().count() as i64 * size.0;
    (drawn.max(chars) + width.0, size.0 + width.0)
}

/// What [`arrange`] decided.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Arrangement {
    /// The new [`RefText`] of every footprint whose text was arranged (`None`: the default
    /// position above the courtyard), by designator.
    pub texts: BTreeMap<String, Option<RefText>>,
    /// Designators whose text changed.
    pub moved: Vec<String>,
    /// Designators whose text found no free position and is hidden (natural order).
    pub hidden: Vec<String>,
    /// Texts set by hand, left alone.
    pub manual: usize,
}

/// A fixed thing a reference text must keep away from.
struct Obstacle {
    side: BoardSide,
    /// The footprint it belongs to.
    owner: Option<String>,
    kind: ObstacleKind,
    bbox: Rect,
    shape: PolygonSet,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ObstacleKind {
    /// Silkscreen (drawings, texts that stay): the silk clearance.
    Silk,
    /// A solder mask opening: `silk_to_pad`.
    Opening,
    /// A courtyard: no overlap (a part's own courtyard only binds positions outside it).
    Courtyard,
}

/// One position a reference text may take.
#[derive(Clone)]
struct Candidate {
    at: Point,
    rotation: Angle,
    /// Inside the footprint's own courtyard.
    inside: bool,
    /// Distance from the footprint's courtyard box to the text box (outside positions).
    away: i64,
    area: Polygon,
    bbox: Rect,
}

struct Edges {
    rings: Vec<Ring>,
    segments: Vec<(polyclip::Point, polyclip::Point)>,
    index: RTree,
}

impl Edges {
    fn of(p: &Project) -> Edges {
        let rings: Vec<Ring> = p
            .board()
            .outline
            .contours
            .iter()
            .map(|c| Ring::from(contour_ring(c, COPPER_TOL)))
            .filter(|r| r.len() >= 3)
            .collect();
        let segments: Vec<(polyclip::Point, polyclip::Point)> =
            rings.iter().flat_map(|r| (0..r.0.len()).map(move |i| (r.0[i], r.0[(i + 1) % r.0.len()]))).collect();
        let index = RTree::new(segments.iter().map(|&(a, b)| Some(Rect::new(a, b))));
        Edges { rings, segments, index }
    }

    /// Whether `c` lies on the board (inside the outer contour, outside every cutout) at
    /// least `d` from every edge. Without an outline there is no edge to keep from.
    fn clear(&self, c: &Candidate, d: i64) -> bool {
        if self.rings.is_empty() {
            return true;
        }
        for i in self.index.query(&c.bbox.expand(d)) {
            let (a, b) = self.segments[i as usize];
            if polyclip::distance_less_than(&c.area, &Path(vec![a, b]), d.max(1)) {
                return false;
            }
        }
        // No edge near: the box is on one side of every contour, the side its center is on.
        let center = polyclip::Point::new((c.bbox.min.x + c.bbox.max.x) / 2, (c.bbox.min.y + c.bbox.max.y) / 2);
        even_odd_inside(&self.rings[0], center) && !self.rings[1..].iter().any(|r| even_odd_inside(r, center))
    }
}

/// Even-odd location of `p` in the region bounded by `r`, for a point on no edge.
fn even_odd_inside(r: &Ring, p: polyclip::Point) -> bool {
    let v = &r.0;
    let mut inside = false;
    for i in 0..v.len() {
        let (a, b) = (v[i], v[(i + 1) % v.len()]);
        if (a.y > p.y) != (b.y > p.y) && (polyclip::predicates::orient(a, b, p) > 0) == (b.y > a.y) {
            inside = !inside;
        }
    }
    inside
}

/// The positions tried for the text of a footprint whose courtyard box is `lo`..`hi`, in order
/// of preference. Nearest first ([`FARTHER`]); at each distance above, below, left and right of
/// the box, upright, then turned a quarter beside it, each first centered on its side, then
/// slid along it by [`SLIDE`] steps either way, as far as the text still reaches over the
/// side; then inside the courtyard of a part large enough; with `far`, last, the same
/// positions around the part at the distances of [`FAR`]. `w`, `h`: the upright text box;
/// `gap`: from the courtyard to the box; `quarter`: the rotation of a turned text.
fn candidates(
    lo: Point,
    hi: Point,
    (w, h): (i64, i64),
    gap: i64,
    quarter: Angle,
    far: bool,
) -> Vec<(Point, Angle, Option<i64>)> {
    let (x0, y0, x1, y1) = (lo.x.0, lo.y.0, hi.x.0, hi.y.0);
    let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
    let (bw, bh) = (x1 - x0, y1 - y0);
    let at = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
    let z = Angle::ZERO;
    // Slides along a side of length `len` of a text `along` long: 0, +1, -1, +2, -2 ... steps,
    // as far as the text still reaches over the side by a quarter of its length (half its
    // height at most).
    let slides = |len: i64, along: i64| {
        let n = ((len / 2 + along / 2 - (along / 4).min(h / 2)).max(0) / SLIDE).min(SLIDE_STEPS);
        std::iter::once(0).chain((1..=n).flat_map(|k| [k * SLIDE, -k * SLIDE]))
    };
    let mut out: Vec<(Point, Angle, Option<i64>)> = Vec::new();
    let around = |out: &mut Vec<(Point, Angle, Option<i64>)>, extra: i64| {
        let g = gap + extra;
        let away = Some(g);
        // Centered on each side: above, below, left, right; then turned beside the part.
        out.push((at(cx, y1 + g + h / 2), z, away));
        out.push((at(cx, y0 - g - h / 2), z, away));
        out.push((at(x0 - g - w / 2, cy), z, away));
        out.push((at(x1 + g + w / 2, cy), z, away));
        out.push((at(x0 - g - h / 2, cy), quarter, away));
        out.push((at(x1 + g + h / 2, cy), quarter, away));
        // Slid along each side.
        for d in slides(bw, w).skip(1) {
            out.push((at(cx + d, y1 + g + h / 2), z, away));
            out.push((at(cx + d, y0 - g - h / 2), z, away));
        }
        for d in slides(bh, h).skip(1) {
            out.push((at(x0 - g - w / 2, cy + d), z, away));
            out.push((at(x1 + g + w / 2, cy + d), z, away));
        }
        for d in slides(bh, w).skip(1) {
            out.push((at(x0 - g - h / 2, cy + d), quarter, away));
            out.push((at(x1 + g + h / 2, cy + d), quarter, away));
        }
    };
    for extra in FARTHER {
        around(&mut out, extra);
    }
    // Inside the body, for parts large enough: the middle, then towards each side.
    if w <= bw && h <= bh {
        out.push((at(cx, cy), z, None));
        out.push((at(cx, cy + bh / 4), z, None));
        out.push((at(cx, cy - bh / 4), z, None));
    }
    if h <= bw && w <= bh {
        out.push((at(cx, cy), quarter, None));
        out.push((at(cx - bw / 4, cy), quarter, None));
        out.push((at(cx + bw / 4, cy), quarter, None));
    }
    if far {
        for extra in FAR {
            around(&mut out, extra);
        }
    }
    out
}

/// Moves reference designators to free positions (D82). For every placed footprint (those in
/// `only` when given) whose text was not set by hand, positions around it are tried in a fixed
/// order (nearest first: above, below, left, right, turned a quarter beside the part, slid
/// along each side; then inside the courtyard) and the first free one is taken: on the board at least the edge clearance from the
/// outline and cutouts, `silk_to_pad` from every solder mask opening of its side, the silk
/// clearance from every other silkscreen drawing and text (its own footprint's drawings
/// included), on no other part's courtyard, and no nearer to another part's courtyard than to
/// its own (a designator next to another part reads as that part's). Clearances are the board
/// rules', at least [`ARRANGE_CLEARANCE`], plus [`ARRANGE_MARGIN`]; the text counts as its whole
/// [`text_box`]. Texts with fewer free positions choose first (ties in natural designator
/// order), so the result depends on nothing but the board; a text finding its free positions
/// taken may send the one text in its way to another position. Texts left without a position
/// get a second round without the margin, then a third where they may also sit beside another
/// part, up to 3 mm from their own; a text still without one is hidden.
///
/// Nothing is changed: the caller applies [`Arrangement::texts`].
pub fn arrange(p: &Project, only: Option<&BTreeSet<String>>) -> Arrangement {
    let board = p.board();
    let rules = &board.rules;
    let mut out = Arrangement::default();
    let mut moving: Vec<&String> = Vec::new();
    for (r, pf) in &board.footprints {
        if footprint_for(p, r).is_none() {
            continue;
        }
        if pf.ref_text.as_ref().is_some_and(|t| t.manual) {
            out.manual += 1;
        } else if only.is_none_or(|o| o.contains(r)) {
            moving.push(r);
        }
    }
    if moving.is_empty() {
        return out;
    }
    let moving_set: BTreeSet<&str> = moving.iter().map(|m| m.as_str()).collect();
    let is_moving = |r: &str| moving_set.contains(r);

    // What stays: silkscreen drawings, texts that are not arranged, mask openings, courtyards.
    let mut fixed: Vec<Obstacle> = Vec::new();
    for s in silk_shapes(p) {
        if let Some(bbox) = s.bbox {
            fixed.push(Obstacle { side: s.side, owner: s.footprint, kind: ObstacleKind::Silk, bbox, shape: s.shape });
        }
    }
    for s in silk_texts(p) {
        let Some((text, t)) = &s.text else { continue };
        if s.footprint.as_deref().is_some_and(is_moving) {
            continue;
        }
        let area = text_box(text, t.at, t.size, t.width, t.rotation);
        if let Some(bbox) = area.bbox() {
            // No owner: the text of another part is in the way of this part's text like any other.
            fixed.push(Obstacle { side: s.side, owner: None, kind: ObstacleKind::Silk, bbox, shape: vec![area] });
        }
    }
    let pads = super::placed_pads(p);
    for (_, side, shape) in mask_openings(p, &pads) {
        if let Some(bbox) = shape.bbox() {
            fixed.push(Obstacle { side, owner: None, kind: ObstacleKind::Opening, bbox, shape });
        }
    }
    for r in board.footprints.keys() {
        for (ring, side) in placed_courtyards(p, r) {
            let shape = vec![Polygon::new(ring, vec![])];
            if let Some(bbox) = shape.bbox() {
                fixed.push(Obstacle { side, owner: Some(r.clone()), kind: ObstacleKind::Courtyard, bbox, shape });
            }
        }
    }
    let index = RTree::new(fixed.iter().map(|o| Some(o.bbox)));
    let edges = Edges::of(p);
    let pad_d = rules.silk_to_pad.max(Nm(1)).0;

    // Whether a position is free of what stays, with `margin` added to the clearances; `own`:
    // it may be no nearer to another part's courtyard than to its own part's.
    let free = |r: &str, side: BoardSide, c: &Candidate, (margin, by_own): (i64, bool)| -> bool {
        let edge_d = rules.silk_to_edge.max(ARRANGE_CLEARANCE).0 + margin;
        let silk_d = rules.silk_clearance.max(ARRANGE_CLEARANCE).0 + margin;
        if !edges.clear(c, edge_d) {
            return false;
        }
        for i in index.query(&c.bbox.expand(silk_d.max(pad_d).max(c.away))) {
            let o = &fixed[i as usize];
            if o.side != side {
                continue;
            }
            let own = o.owner.as_deref() == Some(r);
            let hit = match o.kind {
                ObstacleKind::Silk => polyclip::distance_less_than(&c.area, &o.shape, silk_d),
                ObstacleKind::Opening => polyclip::distance_less_than(&c.area, &o.shape, pad_d),
                ObstacleKind::Courtyard if own => !c.inside && polyclip::intersects(&c.area, &o.shape),
                // On another part's courtyard, or nearer to it than to the text's own part.
                ObstacleKind::Courtyard if by_own => polyclip::distance_less_than(&c.area, &o.shape, c.away.max(1)),
                ObstacleKind::Courtyard => polyclip::intersects(&c.area, &o.shape),
            };
            if hit {
                return false;
            }
        }
        true
    };

    // The positions of each text in order of preference.
    let positions = |r: &String, far: bool| -> Vec<Candidate> {
        let pf = &board.footprints[r];
        let size = pf.ref_text.as_ref().and_then(|t| t.size).unwrap_or(REF_SIZE);
        let width = ref_stroke_width(p, size);
        let (w, h) = text_extent(r, size, width);
        let (lo, hi) = courtyard_box(p, r).unwrap_or((pf.at, pf.at));
        // Read from the right of the board as seen from the text's own side.
        let quarter = if pf.side == BoardSide::Top { Angle::DEG_90 } else { Angle::DEG_270 };
        // The gap is to the capitals, as in the default position: the box is half a stroke larger.
        candidates(lo, hi, (w, h), REF_GAP.0 - width.0 / 2, quarter, far)
            .into_iter()
            .filter_map(|(at, rotation, away)| {
                let area = text_box(r, at, size, width, rotation);
                let bbox = area.bbox()?;
                Some(Candidate { at, rotation, inside: away.is_none(), away: away.unwrap_or(0), area, bbox })
            })
            .collect()
    };

    // Three rounds, each for the texts the one before left without a position: with the
    // margin; without it; then also beside another part. In a round, whether a position is
    // free of what stays is found out when it is looked at (`None`: not yet); texts with the
    // fewest free positions choose first, and a text taken is in the way of the next ones. A
    // text finding every free position taken may send the one text in its way to another of
    // that text's positions.
    struct Todo<'a> {
        refdes: &'a String,
        side: BoardSide,
        options: Vec<(Candidate, Option<bool>)>,
        /// Free positions among the first ones, up to [`ENOUGH`].
        free: usize,
    }
    struct Taken<'a> {
        refdes: &'a String,
        side: BoardSide,
        at: Candidate,
    }
    let silk_d = |margin: i64| rules.silk_clearance.max(ARRANGE_CLEARANCE).0 + margin;
    // Whether `c` keeps `d` from the texts taken, but for the one at index `but`.
    let apart = |taken: &[Taken], side: BoardSide, c: &Candidate, d: i64, but: Option<usize>| -> bool {
        !taken.iter().enumerate().any(|(i, o)| {
            Some(i) != but
                && o.side == side
                && c.bbox.expand(d).intersects(&o.at.bbox)
                && polyclip::distance_less_than(&c.area, &o.at.area, d)
        })
    };
    let mut taken: Vec<Taken> = Vec::new();
    let mut left: Vec<&String> = moving.clone();
    for round in [(ARRANGE_MARGIN.0, true), (0, true), (0, false)] {
        let d = silk_d(round.0);
        let mut todo: Vec<Todo> = Vec::new();
        for r in std::mem::take(&mut left) {
            let side = board.footprints[r].side;
            let mut options: Vec<(Candidate, Option<bool>)> =
                positions(r, !round.1).into_iter().map(|c| (c, None)).collect();
            let mut found = 0;
            for (c, is_free) in options.iter_mut() {
                let ok = free(r, side, c, round);
                *is_free = Some(ok);
                found += usize::from(ok);
                if found == ENOUGH {
                    break;
                }
            }
            todo.push(Todo { refdes: r, side, options, free: found });
        }
        todo.sort_by(|a, b| a.free.cmp(&b.free).then_with(|| natural_cmp(a.refdes, b.refdes)));
        for mut t in todo {
            for (c, is_free) in t.options.iter_mut() {
                if is_free.is_none() {
                    *is_free = Some(free(t.refdes, t.side, c, round));
                }
            }
            let open: Vec<&Candidate> = t.options.iter().filter(|(_, f)| *f == Some(true)).map(|(c, _)| c).collect();
            if let Some(c) = open.iter().find(|c| apart(&taken, t.side, c, d, None)) {
                taken.push(Taken { refdes: t.refdes, side: t.side, at: (*c).clone() });
                continue;
            }
            // Every free position is taken: where a single text is in the way and has another
            // position (under the rules of this round), it moves there.
            let mut moved = false;
            'open: for c in &open {
                let mut blocking = (0..taken.len()).filter(|&i| !apart(&taken[i..=i], t.side, c, d, None));
                let (Some(i), None) = (blocking.next(), blocking.next()) else { continue };
                let other = &taken[i];
                for alt in positions(other.refdes, !round.1) {
                    if free(other.refdes, other.side, &alt, round)
                        && apart(&taken, other.side, &alt, d, Some(i))
                        && !(other.side == t.side && polyclip::distance_less_than(&alt.area, &c.area, d))
                    {
                        taken[i].at = alt;
                        taken.push(Taken { refdes: t.refdes, side: t.side, at: (*c).clone() });
                        moved = true;
                        break 'open;
                    }
                }
            }
            if !moved {
                left.push(t.refdes);
            }
        }
    }
    let chosen: BTreeMap<&String, &Candidate> = taken.iter().map(|t| (t.refdes, &t.at)).collect();

    for r in &moving {
        let pf = &board.footprints[*r];
        let old = pf.ref_text.as_ref();
        let size = old.and_then(|t| t.size);
        let new = match chosen.get(r) {
            Some(c) => {
                let default = default_ref_at(p, r, size.unwrap_or(REF_SIZE));
                if default == Some(c.at) && c.rotation == Angle::ZERO && size.is_none() {
                    None
                } else {
                    let offset = (default != Some(c.at)).then(|| c.at - pf.at);
                    Some(RefText { offset, rotation: c.rotation, size, hidden: false, manual: false })
                }
            }
            None => {
                out.hidden.push((*r).clone());
                Some(RefText { offset: None, rotation: Angle::ZERO, size, hidden: true, manual: false })
            }
        };
        if new.as_ref() != old {
            out.moved.push((*r).clone());
        }
        out.texts.insert((*r).clone(), new);
    }
    out.moved.sort_by(|a, b| natural_cmp(a, b));
    out.hidden.sort_by(|a, b| natural_cmp(a, b));
    out
}
