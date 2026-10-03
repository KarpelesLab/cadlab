//! Specctra session files (`.ses`): the routed result of an external router, and its
//! conversion into board tracks and vias.
//!
//! A session holds the placement the router saw and, under `routes`, the wiring per net
//! (`network_out`) plus padstacks the router created (`library_out`). Session coordinates are
//! integers in steps of the `resolution` (`(resolution um 10)`: 0.1 µm), converted exactly to
//! nanometers.

use std::collections::BTreeSet;

use super::dsn::{self, ParseError, Place, Wire, WireVia};
use super::export::parse_via_name;
use super::sexpr::{self, Sx};
use super::{Scale, Unit};
use crate::diag::Diagnostic;
use crate::geom::Point;
use crate::id::ObjectId;
use crate::model::Project;
use crate::model::board::{Track, Via};
use crate::refs::ObjectRef;
use crate::units::Nm;

/// A parsed session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// Session name.
    pub name: String,
    /// Resolution of the routes.
    pub resolution: (Unit, u32),
    /// Placement as routed.
    pub places: Vec<Place>,
    /// Padstacks created by the router (vias).
    pub padstacks: Vec<dsn::Padstack>,
    /// Nets with wiring, in file order.
    pub nets: Vec<String>,
    /// Wires (with their net).
    pub wires: Vec<Wire>,
    /// Vias (with their net).
    pub vias: Vec<WireVia>,
    /// Wires with shapes other than paths (polygons, arcs), not imported.
    pub unsupported_wires: usize,
}

impl Session {
    /// Reads a session file.
    pub fn parse(text: &str) -> Result<Session, ParseError> {
        let root = sexpr::parse(text).map_err(ParseError::Syntax)?;
        if root.head() != Some("session") {
            return Err(ParseError::Invalid("the file is not a Specctra session (no `session` list)".into()));
        }
        let name = root.args().first().and_then(Sx::text).unwrap_or_default().to_string();
        let routes = root.child("routes");
        let resolution = match routes.and_then(|r| r.child("resolution")) {
            Some(r) => dsn::read_resolution(r)?,
            None => (Unit::Inch, 1000),
        };
        let route_scale = Scale { unit: resolution.0, divisor: resolution.1 };
        let places = match root.child("placement") {
            Some(pl) => {
                let res = match pl.child("resolution") {
                    Some(r) => dsn::read_resolution(r)?,
                    None => resolution,
                };
                dsn::read_placement(pl, Scale { unit: res.0, divisor: res.1 })?
            }
            None => Vec::new(),
        };
        let mut s = Session {
            name,
            resolution,
            places,
            padstacks: Vec::new(),
            nets: Vec::new(),
            wires: Vec::new(),
            vias: Vec::new(),
            unsupported_wires: 0,
        };
        let Some(routes) = routes else { return Ok(s) };
        if let Some(lib) = routes.child("library_out") {
            s.padstacks = dsn::read_padstacks(lib, route_scale)?;
        }
        if let Some(net_out) = routes.child("network_out") {
            for net in net_out.children("net") {
                let name = dsn::arg_text(net, 0)?;
                s.unsupported_wires += net.children("wire").filter(|w| w.child("path").is_none()).count();
                let (wires, vias) = dsn::read_wiring(net.args(), route_scale, Some(&name))?;
                if !s.nets.contains(&name) {
                    s.nets.push(name);
                }
                s.wires.extend(wires);
                s.vias.extend(vias);
            }
        }
        Ok(s)
    }

    /// One resolution step in nanometers (at least 1).
    pub fn step(&self) -> Nm {
        Nm((self.resolution.0.nm() / self.resolution.1.max(1) as i64).max(1))
    }
}

/// Import options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// Keep the existing unlocked tracks and vias of the session's nets instead of replacing
    /// them with the session's wiring.
    pub keep_existing: bool,
}

/// What importing a session changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Import {
    /// Tracks to add (IDs to be allocated).
    pub tracks: Vec<Track>,
    /// Vias to add (IDs to be allocated).
    pub vias: Vec<Via>,
    /// Existing tracks to remove (unlocked, on the session's nets).
    pub remove_tracks: Vec<ObjectId>,
    /// Existing vias to remove.
    pub remove_vias: Vec<ObjectId>,
    /// Session items equal to copper that stays on the board (protected wiring), not added.
    /// (Items repeated within the session are added once and not counted.)
    pub duplicates: usize,
    /// Nets with wiring in the session.
    pub nets: usize,
    /// Warnings: placement differences, unsupported wires.
    pub warnings: Vec<Diagnostic>,
}

/// A session that cannot be applied to the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// A net of the session is not in the circuit.
    UnknownNet {
        /// Net name.
        name: String,
        /// Similar net names.
        suggestions: Vec<String>,
    },
    /// A wire is on a layer that is not a copper layer of the board.
    UnknownLayer(String),
    /// A via padstack is neither defined in the session nor a cadlab via name.
    UnknownPadstack(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::UnknownNet { name, .. } => write!(f, "the session's net `{name}` is not in the circuit"),
            ImportError::UnknownLayer(l) => {
                write!(f, "the session routes on `{l}`, which is not a copper layer of the board")
            }
            ImportError::UnknownPadstack(p) => write!(f, "via padstack `{p}` is not defined in the session"),
        }
    }
}

fn close(a: Point, b: Point, tol: Nm) -> bool {
    (a.x - b.x).0.abs() <= tol.0 && (a.y - b.y).0.abs() <= tol.0
}

/// Whether `p` is within `tol` of the segment `a`–`b`.
fn on_segment(p: Point, a: Point, b: Point, tol: Nm) -> bool {
    let f = |q: Point| (q.x.0 as f64, q.y.0 as f64);
    let ((px, py), (ax, ay), (bx, by)) = (f(p), f(a), f(b));
    let (dx, dy) = (bx - ax, by - ay);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 { 0.0 } else { (((px - ax) * dx + (py - ay) * dy) / l2).clamp(0.0, 1.0) };
    let (cx, cy) = (ax + t * dx - px, ay + t * dy - py);
    (cx * cx + cy * cy).sqrt() <= tol.0 as f64
}

/// Converts a session into board changes. Nets are matched by name; lengths are converted
/// exactly from the session's resolution. Unless `keep_existing`, the unlocked tracks and vias
/// of every net in the session are replaced by the session's wiring. Session items matching
/// copper that stays (within one resolution step) are not added again.
pub fn plan(p: &Project, s: &Session, opts: &ImportOptions) -> Result<Import, ImportError> {
    let board = p.board();
    let circuit = p.circuit();
    let copper = board.stackup.copper_names();
    let tol = s.step();
    let mut out = Import { nets: s.nets.len(), ..Default::default() };

    for n in &s.nets {
        if !circuit.nets.contains_key(n) {
            let suggestions = crate::suggest::did_you_mean(n, circuit.nets.keys().map(String::as_str), 3);
            return Err(ImportError::UnknownNet { name: n.clone(), suggestions });
        }
    }
    let session_nets: BTreeSet<&str> = s.nets.iter().map(String::as_str).collect();
    let replaced = |net: &Option<String>, locked: bool| {
        !opts.keep_existing && !locked && net.as_deref().is_some_and(|n| session_nets.contains(n))
    };
    out.remove_tracks = board.tracks.iter().filter(|t| replaced(&t.net, t.locked)).map(|t| t.id).collect();
    out.remove_vias = board.vias.iter().filter(|v| replaced(&v.net, v.locked)).map(|v| v.id).collect();
    let kept_tracks: Vec<&Track> = board.tracks.iter().filter(|t| !replaced(&t.net, t.locked)).collect();
    let kept_vias: Vec<&Via> = board.vias.iter().filter(|v| !replaced(&v.net, v.locked)).collect();

    for w in &s.wires {
        if !copper.contains(&w.layer) {
            return Err(ImportError::UnknownLayer(w.layer.clone()));
        }
        for seg in w.points.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            if a == b {
                continue;
            }
            // Covered by a track: routers split wires at junctions, so a piece of a kept track
            // comes back as a shorter segment.
            let same = |t: &&Track| {
                t.layer == w.layer
                    && t.net == w.net
                    && t.mid.is_none()
                    && (t.width - w.width).0.abs() <= tol.0
                    && on_segment(a, t.start, t.end, tol)
                    && on_segment(b, t.start, t.end, tol)
            };
            if kept_tracks.iter().any(same) {
                out.duplicates += 1;
                continue;
            }
            // Routers may repeat a segment in several wires.
            if out.tracks.iter().any(|t| same(&t)) {
                continue;
            }
            out.tracks.push(Track {
                id: ObjectId(0),
                layer: w.layer.clone(),
                width: w.width,
                net: w.net.clone(),
                start: a,
                end: b,
                mid: None,
                locked: false,
            });
        }
    }

    let rules = &board.rules;
    for v in &s.vias {
        let defined = s.padstacks.iter().find(|ps| ps.name == v.padstack);
        let named = parse_via_name(&v.padstack);
        let class = v.net.as_ref().and_then(|n| circuit.nets.get(n)).and_then(|n| n.class.as_ref());
        let class = class.and_then(|c| circuit.netclasses.get(c));
        let (diameter, drill, span) = match (named, defined) {
            (Some((d, drill, span)), _) => (d, drill, span),
            (None, Some(ps)) => {
                let d = ps
                    .shapes
                    .iter()
                    .filter_map(|sh| match sh {
                        dsn::Shape::Circle { diameter, .. } => Some(*diameter),
                        _ => None,
                    })
                    .max()
                    .ok_or_else(|| ImportError::UnknownPadstack(v.padstack.clone()))?;
                let idx: Vec<usize> =
                    ps.shapes.iter().filter_map(|sh| copper.iter().position(|c| c == sh.layer())).collect();
                let span = match (idx.iter().min(), idx.iter().max()) {
                    (Some(&a), Some(&b)) if !(a == 0 && b == copper.len() - 1) => Some((a, b)),
                    _ => None,
                };
                let drill = class.and_then(|c| c.via_drill).unwrap_or(rules.via_drill);
                (d, drill, span)
            }
            (None, None) => return Err(ImportError::UnknownPadstack(v.padstack.clone())),
        };
        let (from, to) = match span {
            Some((a, b)) if b < copper.len() => (copper[a].clone(), copper[b].clone()),
            _ => (copper[0].clone(), copper[copper.len() - 1].clone()),
        };
        let same = |x: &&Via| x.net == v.net && close(x.at, v.at, tol);
        if kept_vias.iter().any(same) {
            out.duplicates += 1;
            continue;
        }
        if out.vias.iter().any(|x| same(&x)) {
            continue;
        }
        out.vias.push(Via { id: ObjectId(0), at: v.at, drill, diameter, net: v.net.clone(), from, to, locked: false });
    }

    if s.unsupported_wires > 0 {
        out.warnings.push(
            Diagnostic::warning(
                "ses.unsupported_wire",
                format!("{} wire(s) with shapes other than paths were not imported", s.unsupported_wires),
            )
            .with_hint("configure the router to output plain paths (no arcs or polygons)"),
        );
    }
    for pl in &s.places {
        if let Some(pf) = board.footprints.get(&pl.refdes) {
            if !close(pf.at, pl.at, tol) || pf.side != pl.side || pf.rotation.normalized() != pl.rotation {
                out.warnings.push(
                    Diagnostic::warning(
                        "ses.placement_mismatch",
                        format!("{} is placed differently in the session than on the board", pl.refdes),
                    )
                    .with_subject(ObjectRef::Name(pl.refdes.clone()))
                    .at(pl.at)
                    .with_hint("the session was routed for another placement: export the DSN again and re-route"),
                );
            }
        } else if !board.holes.iter().any(|h| h.name == pl.refdes) {
            out.warnings.push(
                Diagnostic::warning(
                    "ses.unknown_component",
                    format!("{} is in the session but not on the board", pl.refdes),
                )
                .with_subject(ObjectRef::Name(pl.refdes.clone()))
                .with_hint("the session belongs to another design or an older version of this one"),
            );
        }
    }
    Ok(out)
}
