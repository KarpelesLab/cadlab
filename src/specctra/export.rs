//! A project's board as a Specctra design ([`Dsn`]).
//!
//! What is written (docs/ROUTER.md, "Specctra DSN/SES"):
//! - layers: the copper layers, all `signal`;
//! - boundary: the outer contour (arcs as chords within 1 µm); cutouts become keep-outs on every
//!   layer; keep-outs forbidding tracks and/or vias become `wire_keepout`/`via_keepout`/`keepout`
//!   per layer;
//! - one image per footprint, pins in footprint-local coordinates (Specctra mirrors and rotates
//!   them for the placement), one padstack per distinct pad shape: pad rotation is part of the
//!   padstack (rectangles turned a quarter swap sides; other angles become polygons), round
//!   rectangles are polygons circumscribing the corner arcs, ovals are paths; non-plated holes
//!   are image keep-outs;
//! - mounting holes as one-pin components (plated) or keep-outs (non-plated);
//! - nets with their pins, one class per net class (width, clearance, via padstack) plus a
//!   default class from the board rules;
//! - existing tracks and vias as wiring, `protect`ed when locked (or all, by option).
//!
//! Zones are not written: cadlab refills them around the imported routing.

use std::collections::BTreeMap;

use super::dsn::{
    ALL_SIGNAL, Class, Dsn, Image, ImagePin, Keepout, KeepoutKind, Layer, Net, Padstack, Place, Rule, Shape, Wire,
    WireVia,
};
use super::{Scale, Unit};
use crate::board::{self as geo};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::{BoardSide, Contour, Segment};
use crate::model::footprint::{Pad, PadKind, PadShape};
use crate::units::{Angle, Nm};

/// Export options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Router resolution: steps per micrometer (default 10, i.e. 0.1 µm).
    pub resolution: u32,
    /// Mark every existing track and via `protect` (default: only locked ones).
    pub protect_all: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { resolution: 10, protect_all: false }
    }
}

/// The design and what could not be written faithfully.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DsnExport {
    /// The design.
    pub dsn: Dsn,
    /// Warnings (items left out or approximated).
    pub warnings: Vec<String>,
}

/// Arc tolerance for outlines and tracks.
const ARC_TOL: i64 = 1_000;
/// Segments per quarter circle for round-rectangle corners.
const CORNER_STEPS: usize = 4;

fn um(n: Nm) -> String {
    Scale { unit: Unit::Um, divisor: 1 }.fmt(n)
}

/// Vertices of a closed contour, arcs as chords.
fn contour_points(c: &Contour) -> Vec<Point> {
    let mut pts = vec![c.start];
    let mut cur = c.start;
    for s in &c.segments {
        match *s {
            Segment::Line { to } => pts.push(to),
            Segment::Arc { mid, to } => pts.extend(geo::arc_points(cur, mid, to, ARC_TOL).into_iter().skip(1)),
        }
        cur = match *s {
            Segment::Line { to } | Segment::Arc { to, .. } => to,
        };
    }
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    pts
}

/// Name of a via padstack: `via_<diameter>_<drill>` in µm, with `_<from>-<to>` layer indexes when
/// it does not span every layer. [`parse_via_name`] reads it back.
pub fn via_name(diameter: Nm, drill: Nm, span: Option<(usize, usize)>) -> String {
    let base = format!("via_{}_{}", um(diameter), um(drill));
    match span {
        Some((a, b)) => format!("{base}_{a}-{b}"),
        None => base,
    }
}

/// A via padstack: diameter, drill and layer span (copper layer indexes; `None`: through).
pub type ViaSpec = (Nm, Nm, Option<(usize, usize)>);

/// Reads a padstack name written by [`via_name`].
pub fn parse_via_name(name: &str) -> Option<ViaSpec> {
    let rest = name.strip_prefix("via_")?;
    let mut parts = rest.split('_');
    let sc = Scale { unit: Unit::Um, divisor: 1 };
    let d = sc.to_nm(parts.next()?)?;
    let drill = sc.to_nm(parts.next()?)?;
    let span = match parts.next() {
        Some(s) => {
            let (a, b) = s.split_once('-')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        }
        None => None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((d, drill, span))
}

fn via_padstack(name: String, diameter: Nm, layers: &[String]) -> Padstack {
    Padstack {
        name,
        shapes: layers.iter().map(|l| Shape::Circle { layer: l.clone(), diameter, at: Point::ORIGIN }).collect(),
        attach: false,
    }
}

/// Round rectangle `w × h`, corner radius `r`, as a polygon circumscribing the corner arcs.
fn round_rect(w: Nm, h: Nm, r: Nm) -> Vec<Point> {
    let (hw, hh) = (w.0 / 2, h.0 / 2);
    let r = r.0.clamp(0, hw.min(hh));
    let step = std::f64::consts::FRAC_PI_2 / CORNER_STEPS as f64;
    let rv = r as f64 / (step / 2.0).cos();
    let mut pts = Vec::new();
    // Corners counter-clockwise from the lower right, each starting at its first tangent point.
    for (k, (cx, cy)) in [(hw - r, -hh + r), (hw - r, hh - r), (-hw + r, hh - r), (-hw + r, -hh + r)].iter().enumerate()
    {
        let a0 = -std::f64::consts::FRAC_PI_2 + k as f64 * std::f64::consts::FRAC_PI_2;
        let p = |a: f64, rad: f64| {
            Point::new(Nm(cx + (rad * a.cos()).round() as i64), Nm(cy + (rad * a.sin()).round() as i64))
        };
        // The tangent points in between are collinear with these vertices.
        for j in 0..CORNER_STEPS {
            pts.push(p(a0 + (j as f64 + 0.5) * step, rv));
        }
    }
    pts.dedup();
    pts
}

/// Copper shape of a pad on `layer`, pad rotation applied, and a name for it.
fn pad_shape(shape: &PadShape, rotation: Angle, layer: &str) -> (String, Shape) {
    if let PadShape::Polygon { points } = shape {
        // Any outline: named by a hash of its rotated vertices (FNV-1a, deterministic).
        let pts: Vec<Point> = points.iter().map(|p| p.rotated(rotation.normalized())).collect();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for p in &pts {
            for v in [p.x.0, p.y.0] {
                for b in v.to_le_bytes() {
                    hash = (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        let shape = Shape::Polygon { layer: layer.to_string(), width: Nm::ZERO, points: pts };
        return (format!("poly_{hash:016x}"), shape);
    }
    // Pad shapes are symmetric under a half turn; a quarter turn swaps the sides.
    let mut rot = Angle(rotation.normalized().0 % 180_000);
    let (mut w, mut h) = shape.size();
    if rot == Angle::DEG_90 {
        (w, h) = (h, w);
        rot = Angle::ZERO;
    }
    let suffix = if rot == Angle::ZERO { String::new() } else { format!("_r{}", super::fmt_angle(rot)) };
    let layer = layer.to_string();
    let poly = |pts: Vec<Point>| Shape::Polygon {
        layer: layer.clone(),
        width: Nm::ZERO,
        points: pts.into_iter().map(|p| p.rotated(rot)).collect(),
    };
    let rect = |w: Nm, h: Nm| {
        let (a, b) = (Point::new(-w / 2, -h / 2), Point::new(w - w / 2, h - h / 2));
        if rot == Angle::ZERO {
            Shape::Rect { layer: layer.clone(), a, b }
        } else {
            poly(vec![a, Point::new(b.x, a.y), b, Point::new(a.x, b.y)])
        }
    };
    match *shape {
        PadShape::Circle { d } => {
            (format!("circle_{}", um(d)), Shape::Circle { layer, diameter: d, at: Point::ORIGIN })
        }
        PadShape::Rect { .. } => (format!("rect_{}x{}{suffix}", um(w), um(h)), rect(w, h)),
        PadShape::RoundRect { r, .. } if r <= Nm::ZERO => (format!("rect_{}x{}{suffix}", um(w), um(h)), rect(w, h)),
        PadShape::RoundRect { r, .. } => {
            (format!("roundrect_{}x{}_{}{suffix}", um(w), um(h), um(r)), poly(round_rect(w, h, r)))
        }
        PadShape::Oval { .. } if w == h => {
            (format!("circle_{}", um(w)), Shape::Circle { layer, diameter: w, at: Point::ORIGIN })
        }
        PadShape::Oval { .. } => {
            let (minor, half) = (w.min(h), (w.max(h) - w.min(h)) / 2);
            let ends = if w > h {
                [Point::new(-half, Nm::ZERO), Point::new(half, Nm::ZERO)]
            } else {
                [Point::new(Nm::ZERO, -half), Point::new(Nm::ZERO, half)]
            };
            (
                format!("oval_{}x{}{suffix}", um(w), um(h)),
                Shape::Path { layer, width: minor, points: ends.iter().map(|p| p.rotated(rot)).collect() },
            )
        }
        PadShape::Polygon { .. } => unreachable!("handled above"),
    }
}

/// The padstack of a copper pad (`None` for a non-plated hole).
fn pad_padstack(pad: &Pad, copper: &[String]) -> Option<Padstack> {
    let (prefix, layers): (String, Vec<String>) = match pad.kind {
        PadKind::Smd => ("smd".into(), vec![copper[0].clone()]),
        PadKind::Tht { drill } => (format!("tht_d{}", um(drill)), copper.to_vec()),
        PadKind::Npth { .. } => return None,
    };
    let mut name = String::new();
    let shapes = layers
        .iter()
        .map(|l| {
            let (n, s) = pad_shape(&pad.shape, pad.rotation, l);
            name = format!("{prefix}_{n}");
            s
        })
        .collect();
    Some(Padstack { name, shapes, attach: false })
}

/// Builds the design for a project's board.
pub fn export(p: &Project, name: &str, opts: &Options) -> DsnExport {
    let board = p.board();
    let copper = board.stackup.copper_names();
    let index_of = |l: &str| copper.iter().position(|c| c == l);
    let mut warnings = Vec::new();
    let mut padstacks: BTreeMap<String, Padstack> = BTreeMap::new();
    let mut images: BTreeMap<String, Image> = BTreeMap::new();
    let mut places = Vec::new();
    // Net → pins, in board order.
    let mut net_pins: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (refdes, pf) in &board.footprints {
        let Some(fp) = geo::footprint_for(p, refdes) else {
            warnings.push(format!("{refdes} has no footprint and is left out"));
            continue;
        };
        let nets = geo::pad_nets(p, refdes);
        let mut seen: Vec<&str> = Vec::new();
        let mut pins = Vec::new();
        let mut keepouts = Vec::new();
        for (k, pad) in fp.pads.iter().enumerate() {
            let Some(ps) = pad_padstack(pad, &copper) else {
                if let PadKind::Npth { drill } = pad.kind {
                    keepouts.push(Keepout {
                        kind: KeepoutKind::All,
                        name: String::new(),
                        shape: Shape::Circle { layer: ALL_SIGNAL.into(), diameter: drill, at: pad.at },
                    });
                }
                continue;
            };
            let id = if pad.number.is_empty() || seen.contains(&pad.number.as_str()) {
                format!("{}@{k}", pad.number)
            } else {
                seen.push(&pad.number);
                pad.number.clone()
            };
            if let Some(net) = nets.get(&pad.number).filter(|_| !pad.number.is_empty()) {
                net_pins.entry(net.clone()).or_default().push(format!("{refdes}-{id}"));
            }
            pins.push(ImagePin { padstack: ps.name.clone(), rotation: Angle::ZERO, id, at: pad.at });
            padstacks.entry(ps.name.clone()).or_insert(ps);
        }
        let mut outline = fp.courtyard.clone();
        if let Some(first) = outline.first().copied() {
            outline.push(first);
        }
        images.entry(fp.name.clone()).or_insert_with(|| Image {
            name: fp.name.clone(),
            outlines: if outline.len() > 2 {
                vec![Shape::Path { layer: ALL_SIGNAL.into(), width: Nm::ZERO, points: outline }]
            } else {
                vec![]
            },
            pins,
            keepouts,
        });
        let part = p.circuit().components.get(refdes).map(|c| c.part.clone());
        places.push(Place {
            image: fp.name.clone(),
            refdes: refdes.clone(),
            at: pf.at,
            side: pf.side,
            rotation: pf.rotation.normalized(),
            locked: pf.locked,
            part,
        });
    }

    for h in &board.holes {
        let pad = geo::holes::hole_pad(h);
        let (image_name, pins, keepouts) = match pad_padstack(&pad, &copper) {
            Some(ps) => {
                let pin =
                    ImagePin { padstack: ps.name.clone(), rotation: Angle::ZERO, id: "1".into(), at: Point::ORIGIN };
                let name = format!("hole_{}_{}", um(h.diameter()), um(h.drill));
                padstacks.entry(ps.name.clone()).or_insert(ps);
                if let Some(net) = &h.net {
                    net_pins.entry(net.clone()).or_default().push(format!("{}-1", h.name));
                }
                (name, vec![pin], vec![])
            }
            None => {
                let k = Keepout {
                    kind: KeepoutKind::All,
                    name: String::new(),
                    shape: Shape::Circle { layer: ALL_SIGNAL.into(), diameter: h.drill, at: Point::ORIGIN },
                };
                (format!("hole_{}", um(h.drill)), vec![], vec![k])
            }
        };
        images.entry(image_name.clone()).or_insert(Image {
            name: image_name.clone(),
            outlines: vec![],
            pins,
            keepouts,
        });
        places.push(Place {
            image: image_name,
            refdes: h.name.clone(),
            at: h.at,
            side: BoardSide::Top,
            rotation: Angle::ZERO,
            locked: true,
            part: None,
        });
    }

    // Outline, cutouts, keep-outs.
    let mut contours = board.outline.contours.iter();
    let boundary = contours.next().map(contour_points).unwrap_or_default();
    if boundary.is_empty() {
        warnings.push("the board has no outline: the design has no boundary".into());
    }
    let mut keepouts: Vec<Keepout> = contours
        .enumerate()
        .map(|(i, c)| Keepout {
            kind: KeepoutKind::All,
            name: format!("cutout{}", i + 1),
            shape: Shape::Polygon { layer: ALL_SIGNAL.into(), width: Nm::ZERO, points: contour_points(c) },
        })
        .collect();
    for k in &board.keepouts {
        let kind = match (k.no_tracks, k.no_vias) {
            (true, true) => KeepoutKind::All,
            (true, false) => KeepoutKind::Wire,
            (false, true) => KeepoutKind::Via,
            (false, false) => {
                warnings.push(format!("keep-out `{}` forbids neither tracks nor vias and is left out", k.name));
                continue;
            }
        };
        let layers: Vec<String> = if k.layers.is_empty() {
            vec![ALL_SIGNAL.into()]
        } else {
            k.layers.iter().filter(|l| index_of(l).is_some()).cloned().collect()
        };
        for layer in layers {
            keepouts.push(Keepout {
                kind,
                name: k.name.clone(),
                shape: Shape::Polygon { layer, width: Nm::ZERO, points: k.outline.clone() },
            });
        }
    }
    if !board.zones.is_empty() {
        warnings.push(format!(
            "{} zone(s) are not written: they are refilled around the routing after `route.import_ses`",
            board.zones.len()
        ));
    }

    // Nets and classes.
    let rules = &board.rules;
    let circuit = p.circuit();
    let mut vias: Vec<String> = Vec::new();
    let mut use_via = |d: Nm, drill: Nm, padstacks: &mut BTreeMap<String, Padstack>| -> String {
        let name = via_name(d, drill, None);
        padstacks.entry(name.clone()).or_insert_with(|| via_padstack(name.clone(), d, &copper));
        if !vias.contains(&name) {
            vias.push(name.clone());
        }
        name
    };
    let default_via = use_via(rules.via_diameter, rules.via_drill, &mut padstacks);
    let mut default_class = "default".to_string();
    while circuit.netclasses.contains_key(&default_class) {
        default_class.push('_');
    }
    let mut nets = Vec::new();
    let mut class_nets: BTreeMap<Option<&str>, Vec<String>> = BTreeMap::new();
    let wired: std::collections::BTreeSet<&str> = board
        .tracks
        .iter()
        .filter_map(|t| t.net.as_deref())
        .chain(board.vias.iter().filter_map(|v| v.net.as_deref()))
        .collect();
    for (name, net) in &circuit.nets {
        let pins = net_pins.remove(name).unwrap_or_default();
        if pins.is_empty() && !wired.contains(name.as_str()) {
            continue;
        }
        if name.contains('"') {
            warnings.push(format!(
                "net `{name}` contains `\"`, which Specctra names cannot hold: it is written with `'` and a session \
                 will not match it"
            ));
        }
        nets.push(Net { name: name.clone(), pins });
        let class = net.class.as_deref().filter(|c| circuit.netclasses.contains_key(*c));
        class_nets.entry(class).or_default().push(name.clone());
    }
    let mut classes = Vec::new();
    for (class, members) in class_nets {
        let (cname, via, rule) = match class {
            None => (
                default_class.clone(),
                default_via.clone(),
                Rule { width: Some(rules.track_width), clearance: Some(rules.clearance) },
            ),
            Some(c) => {
                let nc = &circuit.netclasses[c];
                let via = use_via(
                    nc.via_diameter.unwrap_or(rules.via_diameter),
                    nc.via_drill.unwrap_or(rules.via_drill),
                    &mut padstacks,
                );
                let rule = Rule {
                    width: Some(nc.track_width.unwrap_or(rules.track_width)),
                    clearance: Some(nc.clearance.unwrap_or(rules.clearance)),
                };
                (c.to_string(), via, rule)
            }
        };
        classes.push(Class { name: cname, nets: members, via: Some(via), rule });
    }

    // Existing wiring.
    let protect = |locked: bool| locked || opts.protect_all;
    let wires = board
        .tracks
        .iter()
        .filter(|t| index_of(&t.layer).is_some())
        .map(|t| Wire {
            layer: t.layer.clone(),
            width: t.width,
            points: match t.mid {
                None => vec![t.start, t.end],
                Some(mid) => geo::arc_points(t.start, mid, t.end, ARC_TOL),
            },
            net: t.net.clone(),
            protect: protect(t.locked),
        })
        .collect();
    let mut wire_vias = Vec::new();
    for v in &board.vias {
        let (Some(a), Some(b)) = (index_of(&v.from), index_of(&v.to)) else { continue };
        let (a, b) = (a.min(b), a.max(b));
        let through = a == 0 && b == copper.len() - 1;
        let name = via_name(v.diameter, v.drill, (!through).then_some((a, b)));
        padstacks.entry(name.clone()).or_insert_with(|| via_padstack(name.clone(), v.diameter, &copper[a..=b]));
        wire_vias.push(WireVia { padstack: name, at: v.at, net: v.net.clone(), protect: protect(v.locked) });
    }

    // Placement is written grouped by image.
    places.sort_by(|a, b| a.image.cmp(&b.image));
    let dsn = Dsn {
        name: name.to_string(),
        resolution: (Unit::Um, opts.resolution.max(1)),
        layers: copper.iter().map(|l| Layer { name: l.clone(), kind: "signal".into() }).collect(),
        boundary,
        keepouts,
        vias,
        rule: Rule { width: Some(rules.track_width), clearance: Some(rules.clearance) },
        places,
        images: images.into_values().collect(),
        padstacks: padstacks.into_values().collect(),
        nets,
        classes,
        wires,
        wire_vias,
    };
    DsnExport { dsn, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn via_names_round_trip() {
        let n = via_name(Nm(600_000), Nm(300_000), None);
        assert_eq!(n, "via_600_300");
        assert_eq!(parse_via_name(&n), Some((Nm(600_000), Nm(300_000), None)));
        let n = via_name(Nm(450_500), Nm(200_000), Some((0, 1)));
        assert_eq!(n, "via_450.5_200_0-1");
        assert_eq!(parse_via_name(&n), Some((Nm(450_500), Nm(200_000), Some((0, 1)))));
        assert_eq!(parse_via_name("Via[0-1]_600:300_um"), None);
    }

    #[test]
    fn pad_shapes() {
        let (n, s) = pad_shape(&PadShape::Rect { w: Nm(1_000_000), h: Nm(500_000) }, Angle::DEG_90, "F.Cu");
        assert_eq!(n, "rect_500x1000");
        assert_eq!(
            s,
            Shape::Rect {
                layer: "F.Cu".into(),
                a: Point::new(Nm(-250_000), Nm(-500_000)),
                b: Point::new(Nm(250_000), Nm(500_000))
            }
        );
        let (n, s) = pad_shape(&PadShape::Oval { w: Nm(2_000_000), h: Nm(1_000_000) }, Angle::ZERO, "F.Cu");
        assert_eq!(n, "oval_2000x1000");
        assert!(matches!(s, Shape::Path { width: Nm(1_000_000), .. }));
        let (n, s) =
            pad_shape(&PadShape::RoundRect { w: Nm(1_000_000), h: Nm(600_000), r: Nm(150_000) }, Angle(45_000), "B.Cu");
        assert_eq!(n, "roundrect_1000x600_150_r45");
        let Shape::Polygon { points, .. } = s else { panic!() };
        assert_eq!(points.len(), 4 * CORNER_STEPS);
        // Circumscribed: every vertex at least as far out as the true outline.
        let rr = round_rect(Nm(1_000_000), Nm(600_000), Nm(150_000));
        assert!(rr.iter().all(|p| p.x.0.abs() <= 500_000 + 1 && p.y.0.abs() <= 300_000 + 1));
    }
}
