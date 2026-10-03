//! KiCad board export (`.kicad_pcb`), with the `.kicad_pro` project file holding net classes
//! and board design rules and a `.kicad_dru` custom rules file for net class minimum widths.
//!
//! Used for interoperability and as the input of the KiCad DRC / Gerber oracle (docs/TESTING.md).
//! The writer follows KiCad's published S-expression format documentation and files that
//! `kicad-cli` reads and writes; it shares no code with KiCad (DECISIONS D7).
//!
//! Format notes:
//! - The board is written in the KiCad 8 format (`version 20240108`), which KiCad 8, 9 and 10
//!   read (KiCad 10 upgrades it on load).
//! - KiCad's Y axis points down. cadlab's origin maps to [`Frame::origin`] (written as the
//!   auxiliary axis and grid origin), and the board is centered on an A4 sheet.
//! - Footprint children are stored relative to the footprint position and orientation, Y down.
//!   They are computed from cadlab's absolute geometry, so mirroring and rotation follow from
//!   cadlab's transform. Pad and text angles are absolute. Bottom-side footprints get orientation
//!   `rotation + 180°` (KiCad's left/right flip) and `B.*` layers.
//! - UUIDs are derived from stable data (designators, pad indices, object IDs), so the output is
//!   byte-for-byte deterministic.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::board::{self as geo, side_layer};
use crate::geom::{BBox, Point};
use crate::model::Project;
use crate::model::board::{BoardSide, GraphicKind, PadConnection, PlacedFootprint, Segment};
use crate::model::footprint::{Footprint, GraphicGeometry, GraphicLayer, Mount, Pad, PadKind, PadShape, Paste};
use crate::model::part::PinKind;
use crate::netlist::{LIB, kicad_pin_type};
use crate::units::{Angle, Nm};

/// Board file format version written (KiCad 8).
pub const FORMAT_VERSION: u32 = 20240108;

/// Default thermal relief gap and spoke width when a zone does not set them (KiCad's defaults).
const THERMAL_DEFAULT: Nm = Nm(500_000);
/// Edge.Cuts line width.
const EDGE_WIDTH: Nm = Nm(50_000);
/// Courtyard line width.
const COURTYARD_WIDTH: Nm = Nm(50_000);

/// Everything written by a KiCad export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KicadExport {
    /// The board (`.kicad_pcb`).
    pub pcb: String,
    /// The project file (`.kicad_pro`): net classes and design rules.
    pub project: String,
    /// Custom design rules (`.kicad_dru`).
    pub rules: String,
    /// Things that could not be exported faithfully.
    pub warnings: Vec<String>,
    /// Footprints written.
    pub footprints: usize,
}

/// Mapping from cadlab coordinates (Y up) to KiCad coordinates (Y down).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    /// KiCad position of cadlab's (0, 0).
    pub origin: (Nm, Nm),
}

impl Frame {
    /// The frame used for a project: the outline's bounding box centered on an A4 sheet
    /// (297 × 210 mm), the offset rounded to whole millimeters.
    pub fn of(p: &Project) -> Frame {
        let pts = p.board().outline.contours.iter().flat_map(|c| {
            std::iter::once(c.start).chain(c.segments.iter().map(|s| match *s {
                Segment::Line { to } | Segment::Arc { to, .. } => to,
            }))
        });
        let (cx, cy) = match BBox::of_points(pts) {
            Some(b) => ((b.min.x.0 + b.max.x.0) / 2, (b.min.y.0 + b.max.y.0) / 2),
            None => (0, 0),
        };
        let round_mm = |v: i64| Nm((v as f64 / 1e6).round() as i64 * 1_000_000);
        Frame { origin: (round_mm(148_500_000 - cx), round_mm(105_000_000 + cy)) }
    }

    /// KiCad coordinates of a cadlab point.
    pub fn to_kicad(&self, p: Point) -> (Nm, Nm) {
        (self.origin.0 + p.x, self.origin.1 - p.y)
    }

    /// cadlab coordinates of a KiCad point (e.g. a location in a KiCad DRC report).
    pub fn from_kicad(&self, x: Nm, y: Nm) -> Point {
        Point::new(x - self.origin.0, self.origin.1 - y)
    }
}

/// Quoted, escaped string.
fn q(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Millimeters with up to six decimals, no trailing zeros.
fn mm(n: Nm) -> String {
    let v = n.0;
    let (sign, a) = if v < 0 { ("-", v.unsigned_abs()) } else { ("", v as u64) };
    let (i, f) = (a / 1_000_000, a % 1_000_000);
    if f == 0 {
        return format!("{sign}{i}");
    }
    let frac = format!("{f:06}");
    format!("{sign}{i}.{}", frac.trim_end_matches('0'))
}

/// Degrees with up to three decimals, normalized to [0, 360).
fn deg(a: Angle) -> String {
    let v = a.normalized().0;
    let (i, f) = (v / 1000, v % 1000);
    if f == 0 {
        return i.to_string();
    }
    let frac = format!("{f:03}");
    format!("{i}.{}", frac.trim_end_matches('0'))
}

fn ratio(x: f64) -> String {
    let s = format!("{x:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() { "0".into() } else { s.to_string() }
}

/// Deterministic UUID (RFC 9562 version 8) from a key: FNV-1a 128-bit hash.
fn uuid(key: &str) -> String {
    const PRIME: u128 = 0x0000000001000000000000000000013B;
    let mut h: u128 = 0x6c62272e07bb014262b821756295c58d;
    for b in key.bytes() {
        h ^= b as u128;
        h = h.wrapping_mul(PRIME);
    }
    // Finalizer (xor-shift-multiply) so that similar keys give unrelated-looking UUIDs.
    for _ in 0..2 {
        h ^= h >> 67;
        h = h.wrapping_mul(0x9e3779b97f4a7c15_f39cc0605cedc835);
        h ^= h >> 59;
    }
    let mut b = h.to_be_bytes();
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    let x: String = b.iter().map(|v| format!("{v:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &x[0..8], &x[8..12], &x[12..16], &x[16..20], &x[20..32])
}

/// KiCad 8 layer table: (ordinal, canonical name, type, user name).
fn layer_table(copper: &[String]) -> Vec<(u32, String, &'static str, Option<&'static str>)> {
    let mut v = Vec::new();
    for (i, name) in copper.iter().enumerate() {
        let ord = if name == "B.Cu" { 31 } else { i as u32 };
        v.push((ord, name.clone(), "signal", None));
    }
    let tech: [(u32, &str, Option<&str>); 18] = [
        (32, "B.Adhes", Some("B.Adhesive")),
        (33, "F.Adhes", Some("F.Adhesive")),
        (34, "B.Paste", None),
        (35, "F.Paste", None),
        (36, "B.SilkS", Some("B.Silkscreen")),
        (37, "F.SilkS", Some("F.Silkscreen")),
        (38, "B.Mask", None),
        (39, "F.Mask", None),
        (40, "Dwgs.User", Some("User.Drawings")),
        (41, "Cmts.User", Some("User.Comments")),
        (42, "Eco1.User", Some("User.Eco1")),
        (43, "Eco2.User", Some("User.Eco2")),
        (44, "Edge.Cuts", None),
        (45, "Margin", None),
        (46, "B.CrtYd", Some("B.Courtyard")),
        (47, "F.CrtYd", Some("F.Courtyard")),
        (48, "B.Fab", None),
        (49, "F.Fab", None),
    ];
    for (o, n, u) in tech {
        v.push((o, n.to_string(), "user", u));
    }
    for i in 1..=9u32 {
        v.push((49 + i, format!("User.{i}"), "user", None));
    }
    v
}

/// Net numbering: 0 is "no net", then every net name in sorted order (circuit nets plus any
/// referenced only by board items).
fn net_numbers(p: &Project) -> BTreeMap<String, usize> {
    let b = p.board();
    let mut names: BTreeSet<&str> = p.circuit().nets.keys().map(String::as_str).collect();
    names.extend(b.tracks.iter().filter_map(|t| t.net.as_deref()));
    names.extend(b.vias.iter().filter_map(|v| v.net.as_deref()));
    names.extend(b.zones.iter().filter_map(|z| z.net.as_deref()));
    names.extend(b.holes.iter().filter_map(|h| h.net.as_deref()));
    names.into_iter().enumerate().map(|(i, n)| (n.to_string(), i + 1)).collect()
}

struct Writer<'a> {
    s: String,
    p: &'a Project,
    frame: Frame,
    nets: BTreeMap<String, usize>,
    layers: BTreeSet<String>,
    warnings: Vec<String>,
}

impl Writer<'_> {
    fn xy(&self, p: Point) -> String {
        let (x, y) = self.frame.to_kicad(p);
        format!("{} {}", mm(x), mm(y))
    }

    fn net(&self, name: Option<&str>) -> usize {
        name.and_then(|n| self.nets.get(n).copied()).unwrap_or(0)
    }

    fn line(&mut self, indent: usize, text: &str) {
        for _ in 0..indent {
            self.s.push_str("  ");
        }
        self.s.push_str(text);
        self.s.push('\n');
    }
}

/// The footprint-local frame of a placed footprint, in KiCad terms.
struct FpFrame<'a> {
    pf: &'a PlacedFootprint,
    /// KiCad orientation.
    orient: Angle,
}

impl FpFrame<'_> {
    fn new(pf: &PlacedFootprint) -> FpFrame<'_> {
        let orient = match pf.side {
            BoardSide::Top => pf.rotation,
            BoardSide::Bottom => pf.rotation + Angle::DEG_180,
        }
        .normalized();
        FpFrame { pf, orient }
    }

    /// KiCad footprint-local coordinates of a cadlab footprint-local point.
    fn local(&self, q: Point) -> String {
        let abs = geo::transform(self.pf)(q);
        let d = (abs - self.pf.at).rotated(-self.orient);
        format!("{} {}", mm(d.x), mm(-d.y))
    }

    /// Absolute angle of something rotated by `r` in footprint-local coordinates.
    fn angle(&self, r: Angle) -> Angle {
        match self.pf.side {
            BoardSide::Top => self.pf.rotation + r,
            BoardSide::Bottom => self.pf.rotation - r,
        }
        .normalized()
    }

    fn layer(&self, front: &str) -> String {
        side_layer(self.pf.side, front)
    }
}

/// Exports the board, project file and custom rules. `name` is the project name used in the
/// project file (`<name>.kicad_pro`).
pub fn export(p: &Project, name: &str) -> KicadExport {
    let board = p.board();
    let copper = board.stackup.copper_names();
    let mut w = Writer {
        s: String::new(),
        p,
        frame: Frame::of(p),
        nets: net_numbers(p),
        layers: layer_table(&copper).into_iter().map(|l| l.1).collect(),
        warnings: Vec::new(),
    };

    let _ = writeln!(
        w.s,
        "(kicad_pcb (version {FORMAT_VERSION}) (generator \"cadlab\") (generator_version {})",
        q(env!("CARGO_PKG_VERSION"))
    );
    w.line(1, "(general");
    w.line(2, &format!("(thickness {})", mm(board.stackup.thickness)));
    w.line(2, "(legacy_teardrops no)");
    w.line(1, ")");
    w.line(1, "(paper \"A4\")");
    w.line(1, "(layers");
    for (ord, n, ty, user) in layer_table(&copper) {
        let user = user.map(|u| format!(" {}", q(u))).unwrap_or_default();
        w.line(2, &format!("({ord} {} {ty}{user})", q(&n)));
    }
    w.line(1, ")");
    w.line(1, "(setup");
    w.line(2, "(pad_to_mask_clearance 0)");
    w.line(2, "(allow_soldermask_bridges_in_footprints no)");
    let o = w.xy(Point::new(Nm::ZERO, Nm::ZERO));
    w.line(2, &format!("(aux_axis_origin {o})"));
    w.line(2, &format!("(grid_origin {o})"));
    w.line(1, ")");

    w.line(1, "(net 0 \"\")");
    let nets: Vec<(String, usize)> = w.nets.iter().map(|(n, i)| (n.clone(), *i)).collect();
    for (n, i) in &nets {
        w.line(1, &format!("(net {i} {})", q(n)));
    }

    let mut footprints = 0;
    for (refdes, pf) in &board.footprints {
        if !p.circuit().components.contains_key(refdes) {
            w.warnings.push(format!("{refdes}: placed but not in the circuit; skipped"));
            continue;
        }
        let Some(fp) = geo::footprint_for(p, refdes) else {
            w.warnings.push(format!("{refdes}: no footprint in the library; skipped"));
            continue;
        };
        write_footprint(&mut w, refdes, pf, fp);
        footprints += 1;
    }

    for h in &board.holes {
        write_hole(&mut w, h);
    }

    write_graphics(&mut w);
    write_copper(&mut w);
    w.s.push_str(")\n");

    let project = to_kicad_pro(p, name);
    let (rules, rule_warnings) = dru(p);
    w.warnings.extend(rule_warnings);
    KicadExport { pcb: w.s, project, rules, warnings: w.warnings, footprints }
}

/// The `.kicad_pcb` board text.
pub fn to_kicad_pcb(p: &Project) -> String {
    export(p, &p.manifest().name).pcb
}

/// Pad number → (pin name, pin type) through the part's pin → pad map.
fn pad_pins(p: &Project, refdes: &str) -> BTreeMap<String, (String, PinKind)> {
    let mut out = BTreeMap::new();
    let Some(comp) = p.circuit().components.get(refdes) else { return out };
    let Some(part) = p.library().parts.get(&comp.part) else { return out };
    let fref = part.footprint();
    for pin in &part.symbol.pins {
        let pads = fref.map(|f| f.pads_for(&pin.number)).unwrap_or_else(|| vec![pin.number.clone()]);
        for pad in pads {
            out.entry(pad).or_insert_with(|| (pin.name.clone(), pin.kind));
        }
    }
    out
}

fn text_effects(size: Nm, mirror: bool) -> String {
    let thick = Nm(size.0 * 15 / 100);
    let justify = if mirror { " (justify mirror)" } else { "" };
    format!("(effects (font (size {s} {s}) (thickness {})){justify})", mm(thick), s = mm(size))
}

fn write_footprint(w: &mut Writer<'_>, refdes: &str, pf: &PlacedFootprint, fp: &Footprint) {
    let p = w.p;
    let f = FpFrame::new(pf);
    let key = format!("fp/{refdes}");
    let comp = &p.circuit().components[refdes];
    let part = p.library().parts.get(&comp.part);
    let value = part.map(|x| x.value()).unwrap_or_else(|| comp.part.clone());
    let bottom = pf.side == BoardSide::Bottom;
    let at = w.xy(pf.at);

    w.line(1, &format!("(footprint {} (layer {})", q(&format!("{LIB}:{}", fp.name)), q(&f.layer("F.Cu"))));
    w.line(2, &format!("(uuid {})", q(&uuid(&key))));
    w.line(2, &format!("(at {at} {})", deg(f.orient)));
    if pf.locked {
        w.line(2, "(locked yes)");
    }
    if !fp.description.is_empty() {
        w.line(2, &format!("(descr {})", q(&fp.description)));
    }

    // Texts: reference above the courtyard, value at the center on the fab layer.
    let cy = BBox::of_points(fp.courtyard.iter().copied().chain(fp.pads.iter().map(|x| x.at)));
    let top = cy.map(|b| b.max.y).unwrap_or(Nm::ZERO) + Nm::from_um(1000);
    let text_angle = deg(pf.rotation);
    let size = Nm::from_um(1000);
    let mut props: Vec<(&str, String, Point, &str, bool)> = vec![
        ("Reference", refdes.to_string(), Point::new(Nm::ZERO, top), "F.SilkS", false),
        ("Value", value, Point::new(Nm::ZERO, Nm::ZERO), "F.Fab", false),
        ("Footprint", format!("{LIB}:{}", fp.name), Point::new(Nm::ZERO, Nm::ZERO), "F.Fab", true),
    ];
    if let Some(part) = part {
        if let Some(ds) = &part.datasheet {
            props.push(("Datasheet", ds.clone(), Point::new(Nm::ZERO, Nm::ZERO), "F.Fab", true));
        }
        if !part.description.is_empty() {
            props.push(("Description", part.description.clone(), Point::new(Nm::ZERO, Nm::ZERO), "F.Fab", true));
        }
        if let Some(m) = &part.manufacturer {
            props.push(("Manufacturer", m.clone(), Point::new(Nm::ZERO, Nm::ZERO), "F.Fab", true));
        }
        if let Some(m) = &part.mpn {
            props.push(("MPN", m.clone(), Point::new(Nm::ZERO, Nm::ZERO), "F.Fab", true));
        }
    }
    for (k, v, pos, layer, hide) in props {
        w.line(
            2,
            &format!(
                "(property {} {} (at {} {text_angle}) (layer {}){} (uuid {})",
                q(k),
                q(&v),
                f.local(pos),
                q(&f.layer(layer)),
                if hide { " (hide yes)" } else { "" },
                q(&uuid(&format!("{key}/prop/{k}")))
            ),
        );
        w.line(3, &format!("{})", text_effects(size, bottom)));
    }

    let mut attr = vec![match fp.mount {
        Mount::Smd => "smd",
        Mount::Tht => "through_hole",
    }];
    if p.bom().dnp.contains(refdes) {
        attr.extend(["exclude_from_pos_files", "exclude_from_bom", "dnp"]);
    }
    w.line(2, &format!("(attr {})", attr.join(" ")));

    // Graphics.
    for (gi, g) in fp.graphics.iter().enumerate() {
        let layer = q(&f.layer(match g.layer {
            GraphicLayer::Silk => "F.SilkS",
            GraphicLayer::Fab => "F.Fab",
            GraphicLayer::Courtyard => "F.CrtYd",
        }));
        let stroke = format!("(stroke (width {}) (type solid))", mm(g.width));
        let gk = format!("{key}/g{gi}");
        match &g.geometry {
            GraphicGeometry::Path { points } => {
                for (si, s) in points.windows(2).enumerate() {
                    w.line(
                        2,
                        &format!(
                            "(fp_line (start {}) (end {}) {stroke} (layer {layer}) (uuid {}))",
                            f.local(s[0]),
                            f.local(s[1]),
                            q(&uuid(&format!("{gk}/{si}")))
                        ),
                    );
                }
            }
            GraphicGeometry::Polygon { points } => {
                let pts: Vec<String> = points.iter().map(|x| format!("(xy {})", f.local(*x))).collect();
                w.line(
                    2,
                    &format!(
                        "(fp_poly (pts {}) {stroke} (fill none) (layer {layer}) (uuid {}))",
                        pts.join(" "),
                        q(&uuid(&gk))
                    ),
                );
            }
            GraphicGeometry::Circle { center, radius, filled } => {
                w.line(
                    2,
                    &format!(
                        "(fp_circle (center {}) (end {}) {stroke} (fill {}) (layer {layer}) (uuid {}))",
                        f.local(*center),
                        f.local(*center + Point::new(*radius, Nm::ZERO)),
                        if *filled { "solid" } else { "none" },
                        q(&uuid(&gk))
                    ),
                );
            }
        }
    }
    if fp.courtyard.len() >= 3 {
        let pts: Vec<String> = fp.courtyard.iter().map(|x| format!("(xy {})", f.local(*x))).collect();
        w.line(
            2,
            &format!(
                "(fp_poly (pts {}) (stroke (width {}) (type solid)) (fill none) (layer {}) (uuid {}))",
                pts.join(" "),
                mm(COURTYARD_WIDTH),
                q(&f.layer("F.CrtYd")),
                q(&uuid(&format!("{key}/courtyard")))
            ),
        );
    }

    // Pads.
    let nets = geo::pad_nets(p, refdes);
    let pins = pad_pins(p, refdes);
    for (pi, pad) in fp.pads.iter().enumerate() {
        write_pad(w, &f, &format!("{key}/pad{pi}"), pad, &nets, &pins);
    }
    w.line(1, ")");
}

/// A board hole as a one-pad mounting hole footprint (no courtyard, excluded from BOM and
/// position files).
fn write_hole(w: &mut Writer<'_>, h: &crate::model::board::Hole) {
    let pf = PlacedFootprint { at: h.at, rotation: Angle::ZERO, side: BoardSide::Top, locked: false, footprint: None };
    let f = FpFrame::new(&pf);
    let key = format!("hole/{}", h.id.0);
    let at = w.xy(h.at);
    w.line(1, &format!("(footprint {} (layer \"F.Cu\")", q(&format!("{LIB}:MountingHole"))));
    w.line(2, &format!("(uuid {})", q(&uuid(&key))));
    w.line(2, &format!("(at {at} 0)"));
    let size = Nm::from_um(1000);
    let top = Point::new(Nm::ZERO, Nm(h.diameter().0 / 2) + size);
    let value = match h.pad {
        Some(d) => format!("MountingHole {} pad {}", mm(h.drill), mm(d)),
        None => format!("MountingHole {}", mm(h.drill)),
    };
    let props = [
        ("Reference", h.name.clone(), top, "F.SilkS", false),
        ("Value", value, Point::new(Nm::ZERO, Nm::ZERO), "F.Fab", true),
        ("Footprint", format!("{LIB}:MountingHole"), Point::new(Nm::ZERO, Nm::ZERO), "F.Fab", true),
    ];
    for (k, v, pos, layer, hide) in props {
        w.line(
            2,
            &format!(
                "(property {} {} (at {} 0) (layer {}){} (uuid {})",
                q(k),
                q(&v),
                f.local(pos),
                q(layer),
                if hide { " (hide yes)" } else { "" },
                q(&uuid(&format!("{key}/prop/{k}")))
            ),
        );
        w.line(3, &format!("{})", text_effects(size, false)));
    }
    w.line(2, "(attr exclude_from_pos_files exclude_from_bom)");
    let pad = geo::holes::hole_pad(h);
    let nets: BTreeMap<String, String> =
        h.net.iter().filter(|_| h.pad.is_some()).map(|n| (pad.number.clone(), n.clone())).collect();
    write_pad(w, &f, &format!("{key}/pad"), &pad, &nets, &BTreeMap::new());
    w.line(1, ")");
}

fn write_pad(
    w: &mut Writer<'_>,
    f: &FpFrame<'_>,
    key: &str,
    pad: &Pad,
    nets: &BTreeMap<String, String>,
    pins: &BTreeMap<String, (String, PinKind)>,
) {
    let angle = deg(f.angle(pad.rotation));
    let (kind, layers): (&str, Vec<String>) = match pad.kind {
        PadKind::Smd => {
            let mut l = vec![f.layer("F.Cu"), f.layer("F.Mask")];
            if pad.paste.is_none() {
                l.push(f.layer("F.Paste"));
            }
            ("smd", l)
        }
        PadKind::Tht { .. } => ("thru_hole", vec!["*.Cu".into(), "*.Mask".into()]),
        PadKind::Npth { .. } => ("np_thru_hole", vec!["*.Cu".into(), "*.Mask".into()]),
    };
    let (shape, size, rr) = match (pad.kind, pad.shape) {
        (PadKind::Npth { drill }, _) => ("circle", (drill, drill), None),
        (_, PadShape::Rect { w, h }) => ("rect", (w, h), None),
        (_, PadShape::RoundRect { w, h, r }) => {
            let m = w.0.min(h.0).max(1) as f64;
            ("roundrect", (w, h), Some((r.0 as f64 / m).clamp(0.0, 0.5)))
        }
        (_, PadShape::Oval { w, h }) => ("oval", (w, h), None),
        (_, PadShape::Circle { d }) => ("circle", (d, d), None),
    };
    let layers: Vec<String> = layers.iter().map(|l| q(l)).collect();
    let mut s = format!(
        "(pad {} {kind} {shape} (at {} {angle}) (size {} {}) ",
        q(&pad.number),
        f.local(pad.at),
        mm(size.0),
        mm(size.1)
    );
    match pad.kind {
        PadKind::Tht { drill } | PadKind::Npth { drill } => {
            let _ = write!(s, "(drill {}) ", mm(drill));
        }
        PadKind::Smd => {}
    }
    let _ = write!(s, "(layers {})", layers.join(" "));
    if let Some(r) = rr {
        let _ = write!(s, " (roundrect_rratio {})", ratio(r));
    }
    if !pad.number.is_empty() {
        if let Some(net) = nets.get(&pad.number) {
            let _ = write!(s, " (net {} {})", w.net(Some(net)), q(net));
        }
        if let Some((name, kind)) = pins.get(&pad.number) {
            if !name.is_empty() {
                let _ = write!(s, " (pinfunction {})", q(name));
            }
            let _ = write!(s, " (pintype {})", q(kicad_pin_type(*kind)));
        }
    }
    let _ = write!(s, " (uuid {}))", q(&uuid(key)));
    w.line(2, &s);

    // Paste windows become paste-only aperture pads.
    if let Some(Paste::Windows { size, at }) = &pad.paste {
        for (i, c) in at.iter().enumerate() {
            let center = pad.at + c.rotated(pad.rotation);
            w.line(
                2,
                &format!(
                    "(pad \"\" smd rect (at {} {angle}) (size {} {}) (layers {}) (uuid {}))",
                    f.local(center),
                    mm(size.0),
                    mm(size.1),
                    q(&f.layer("F.Paste")),
                    q(&uuid(&format!("{key}/paste{i}")))
                ),
            );
        }
    }
}

fn write_graphics(w: &mut Writer<'_>) {
    let p = w.p;
    let board = p.board();
    for (ci, c) in board.outline.contours.iter().enumerate() {
        let mut from = c.start;
        for (si, s) in c.segments.iter().enumerate() {
            let id = q(&uuid(&format!("outline/{ci}/{si}")));
            let stroke = format!("(stroke (width {}) (type solid))", mm(EDGE_WIDTH));
            match *s {
                Segment::Line { to } => {
                    if to != from {
                        let t = format!(
                            "(gr_line (start {}) (end {}) {stroke} (layer \"Edge.Cuts\") (uuid {id}))",
                            w.xy(from),
                            w.xy(to)
                        );
                        w.line(1, &t);
                    }
                    from = to;
                }
                Segment::Arc { mid, to } => {
                    let t = format!(
                        "(gr_arc (start {}) (mid {}) (end {}) {stroke} (layer \"Edge.Cuts\") (uuid {id}))",
                        w.xy(from),
                        w.xy(mid),
                        w.xy(to)
                    );
                    w.line(1, &t);
                    from = to;
                }
            }
        }
        if from != c.start {
            let id = q(&uuid(&format!("outline/{ci}/close")));
            let t = format!(
                "(gr_line (start {}) (end {}) (stroke (width {}) (type solid)) (layer \"Edge.Cuts\") (uuid {id}))",
                w.xy(from),
                w.xy(c.start),
                mm(EDGE_WIDTH)
            );
            w.line(1, &t);
        }
    }
    for g in &board.graphics {
        if !w.layers.contains(&g.layer) {
            w.warnings.push(format!("graphic #{}: unknown layer `{}`; skipped", g.id.0, g.layer));
            continue;
        }
        let key = format!("graphic/{}", g.id.0);
        let layer = q(&g.layer);
        match &g.kind {
            GraphicKind::Line { points, width } => {
                for (i, s) in points.windows(2).enumerate() {
                    let t = format!(
                        "(gr_line (start {}) (end {}) (stroke (width {}) (type solid)) (layer {layer}) (uuid {}))",
                        w.xy(s[0]),
                        w.xy(s[1]),
                        mm(*width),
                        q(&uuid(&format!("{key}/{i}")))
                    );
                    w.line(1, &t);
                }
            }
            GraphicKind::Text { text, at, size, rotation } => {
                let t = format!(
                    "(gr_text {} (at {} {}) (layer {layer}) (uuid {})",
                    q(text),
                    w.xy(*at),
                    deg(*rotation),
                    q(&uuid(&key))
                );
                w.line(1, &t);
                w.line(2, &format!("{})", text_effects(*size, g.layer.starts_with("B."))));
            }
        }
    }
}

fn write_copper(w: &mut Writer<'_>) {
    let p = w.p;
    let board = p.board();
    for t in &board.tracks {
        let key = q(&uuid(&format!("track/{}", t.id.0)));
        let net = w.net(t.net.as_deref());
        let lock = if t.locked { " (locked yes)" } else { "" };
        let s = match t.mid {
            None => format!(
                "(segment (start {}) (end {}) (width {}) (layer {}){lock} (net {net}) (uuid {key}))",
                w.xy(t.start),
                w.xy(t.end),
                mm(t.width),
                q(&t.layer)
            ),
            Some(mid) => format!(
                "(arc (start {}) (mid {}) (end {}) (width {}) (layer {}){lock} (net {net}) (uuid {key}))",
                w.xy(t.start),
                w.xy(mid),
                w.xy(t.end),
                mm(t.width),
                q(&t.layer)
            ),
        };
        w.line(1, &s);
    }
    let copper = board.stackup.copper_names();
    for v in &board.vias {
        let span = geo::via_layers(p, v);
        let through = span.first() == copper.first() && span.last() == copper.last();
        let ty = if through { "" } else { " blind" };
        let lock = if v.locked { " (locked yes)" } else { "" };
        let s = format!(
            "(via{ty} (at {}) (size {}) (drill {}) (layers {} {}){lock} (net {}) (uuid {}))",
            w.xy(v.at),
            mm(v.diameter),
            mm(v.drill),
            q(&v.from),
            q(&v.to),
            w.net(v.net.as_deref()),
            q(&uuid(&format!("via/{}", v.id.0)))
        );
        w.line(1, &s);
    }
    let rules = &board.rules;
    for z in &board.zones {
        let net = w.net(z.net.as_deref());
        let layers = z.layers.iter().filter(|l| board.is_copper(l)).map(|l| q(l)).collect::<Vec<_>>();
        if layers.is_empty() || z.outline.len() < 3 {
            w.warnings.push(format!("zone `{}`: no copper layer or outline; skipped", z.name));
            continue;
        }
        let layer_tok =
            if layers.len() == 1 { format!("(layer {})", layers[0]) } else { format!("(layers {})", layers.join(" ")) };
        w.line(
            1,
            &format!(
                "(zone (net {net}) (net_name {}) {layer_tok} (uuid {}) (name {}) (hatch edge 0.5)",
                q(z.net.as_deref().unwrap_or("")),
                q(&uuid(&format!("zone/{}", z.id.0))),
                q(&z.name)
            ),
        );
        if z.priority > 0 {
            w.line(2, &format!("(priority {})", z.priority));
        }
        let clearance = mm(z.clearance.unwrap_or(rules.clearance));
        let connect = match z.pads {
            PadConnection::Thermal => String::new(),
            PadConnection::Solid => "yes ".into(),
            PadConnection::None => "no ".into(),
        };
        w.line(2, &format!("(connect_pads {connect}(clearance {clearance}))"));
        w.line(
            2,
            &format!("(min_thickness {}) (filled_areas_thickness no)", mm(z.min_width.unwrap_or(rules.zone_min_width))),
        );
        w.line(
            2,
            &format!(
                "(fill (thermal_gap {}) (thermal_bridge_width {}))",
                mm(z.thermal_gap.unwrap_or(THERMAL_DEFAULT)),
                mm(z.thermal_spoke.unwrap_or(THERMAL_DEFAULT))
            ),
        );
        let pts: Vec<String> = z.outline.iter().map(|x| format!("(xy {})", w.xy(*x))).collect();
        w.line(2, &format!("(polygon (pts {}))", pts.join(" ")));
        w.line(1, ")");
    }
    for k in &board.keepouts {
        if k.outline.len() < 3 {
            continue;
        }
        let layers: Vec<String> = if k.layers.is_empty() {
            copper.clone()
        } else {
            k.layers.iter().filter(|l| board.is_copper(l)).cloned().collect()
        };
        let layers: Vec<String> = layers.iter().map(|l| q(l)).collect();
        let allow = |forbidden: bool| if forbidden { "not_allowed" } else { "allowed" };
        w.line(
            1,
            &format!(
                "(zone (net 0) (net_name \"\") (layers {}) (uuid {}) (name {}) (hatch edge 0.5)",
                layers.join(" "),
                q(&uuid(&format!("keepout/{}", k.id.0))),
                q(&k.name)
            ),
        );
        w.line(2, "(connect_pads (clearance 0))");
        w.line(2, "(min_thickness 0.25) (filled_areas_thickness no)");
        w.line(
            2,
            &format!(
                "(keepout (tracks {}) (vias {}) (pads allowed) (copperpour {}) (footprints {}))",
                allow(k.no_tracks),
                allow(k.no_vias),
                allow(k.no_pours),
                allow(k.no_footprints)
            ),
        );
        w.line(2, "(fill (thermal_gap 0.5) (thermal_bridge_width 0.5))");
        let pts: Vec<String> = k.outline.iter().map(|x| format!("(xy {})", w.xy(*x))).collect();
        w.line(2, &format!("(polygon (pts {}))", pts.join(" ")));
        w.line(1, ")");
    }
}

fn mmf(n: Nm) -> f64 {
    n.0 as f64 / 1e6
}

/// The `.kicad_pro` project file: board design rules and net classes (KiCad keeps both in the
/// project since version 7). Net class membership is written as exact-name patterns.
pub fn to_kicad_pro(p: &Project, name: &str) -> String {
    use serde_json::json;
    let r = &p.board().rules;
    let class = |name: &str, nc: Option<&crate::model::circuit::NetClass>| {
        let g = |v: Option<Nm>, d: Nm| mmf(v.unwrap_or(d));
        json!({
            "name": name,
            "clearance": g(nc.and_then(|c| c.clearance), r.clearance),
            "track_width": g(nc.and_then(|c| c.track_width), r.track_width),
            "via_diameter": g(nc.and_then(|c| c.via_diameter), r.via_diameter),
            "via_drill": g(nc.and_then(|c| c.via_drill), r.via_drill),
            "diff_pair_width": g(nc.and_then(|c| c.diff_pair_width), r.track_width),
            "diff_pair_gap": g(nc.and_then(|c| c.diff_pair_gap), r.clearance),
            "diff_pair_via_gap": g(nc.and_then(|c| c.diff_pair_gap), r.clearance),
            "microvia_diameter": 0.3,
            "microvia_drill": 0.1,
            "wire_width": 6,
            "bus_width": 12,
            "line_style": 0,
            "pcb_color": "rgba(0, 0, 0, 0.000)",
            "schematic_color": "rgba(0, 0, 0, 0.000)",
            "priority": if name == "Default" { i32::MAX } else { 0 },
        })
    };
    let circuit = p.circuit();
    let mut classes = vec![class("Default", circuit.netclasses.get("Default"))];
    for (i, (n, c)) in circuit.netclasses.iter().filter(|(n, _)| n.as_str() != "Default").enumerate() {
        let mut v = class(n, Some(c));
        v["priority"] = json!(i);
        classes.push(v);
    }
    let patterns: Vec<serde_json::Value> = circuit
        .nets
        .iter()
        .filter_map(|(n, net)| net.class.as_ref().filter(|c| c.as_str() != "Default").map(|c| (n, c)))
        .map(|(n, c)| json!({"netclass": c, "pattern": n}))
        .collect();
    let min_via = r.min_drill + r.min_annular_ring * 2;
    let pro = json!({
        "board": {
            "design_settings": {
                "defaults": {
                    "copper_line_width": 0.2,
                    "silk_line_width": mmf(r.min_silk_width),
                },
                "meta": {"version": 2},
                "rules": {
                    "min_clearance": mmf(r.clearance),
                    "min_connection": 0.0,
                    "min_copper_edge_clearance": mmf(r.copper_to_edge),
                    "min_hole_clearance": mmf(r.clearance),
                    "min_hole_to_hole": mmf(r.hole_to_hole),
                    "min_microvia_diameter": 0.2,
                    "min_microvia_drill": 0.1,
                    "min_resolved_spokes": 2,
                    "min_silk_clearance": 0.0,
                    "min_text_height": 0.8,
                    "min_text_thickness": 0.08,
                    "min_through_hole_diameter": mmf(r.min_drill),
                    "min_track_width": mmf(r.min_track_width),
                    "min_via_annular_width": mmf(r.min_annular_ring),
                    "min_via_diameter": mmf(min_via),
                    "solder_mask_to_copper_clearance": 0.0,
                    "use_height_for_length_calcs": true,
                },
                "track_widths": [0.0],
                "via_dimensions": [{"diameter": 0.0, "drill": 0.0}],
            },
        },
        "meta": {"filename": format!("{name}.kicad_pro"), "version": 1},
        "net_settings": {
            "classes": classes,
            "meta": {"version": 3},
            "net_colors": null,
            "netclass_assignments": null,
            "netclass_patterns": patterns,
        },
    });
    let mut s = serde_json::to_string_pretty(&pro).expect("project serializes");
    s.push('\n');
    s
}

/// Custom rules (`.kicad_dru`): KiCad does not check net class track widths by itself, so each
/// class with a width gets a minimum track width rule.
fn dru(p: &Project) -> (String, Vec<String>) {
    let mut s = String::from("(version 1)\n");
    let mut warnings = Vec::new();
    for (name, c) in &p.circuit().netclasses {
        let Some(width) = c.track_width else { continue };
        if name.contains('\'') || name.contains('"') {
            warnings.push(format!("net class `{name}`: name contains quotes; no width rule written"));
            continue;
        }
        let _ = writeln!(
            s,
            "\n(rule {}\n  (condition \"A.NetClass == '{name}'\")\n  (constraint track_width (min {}mm)))",
            q(&format!("netclass {name} track width")),
            mm(width)
        );
    }
    (s, warnings)
}

/// The `.kicad_dru` custom rules text.
pub fn to_kicad_dru(p: &Project) -> String {
    dru(p).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(mm(Nm(0)), "0");
        assert_eq!(mm(Nm(1_500_000)), "1.5");
        assert_eq!(mm(Nm(-250_000)), "-0.25");
        assert_eq!(mm(Nm(-1)), "-0.000001");
        assert_eq!(deg(Angle(-90_000)), "270");
        assert_eq!(deg(Angle(45_500)), "45.5");
        assert_eq!(ratio(0.25), "0.25");
        assert_eq!(ratio(0.0), "0");
    }

    #[test]
    fn uuids_are_stable_and_well_formed() {
        let a = uuid("fp/R1");
        assert_eq!(a, uuid("fp/R1"));
        assert_ne!(a, uuid("fp/R2"));
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "8", "version 8");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"), "RFC 9562 variant");
    }

    #[test]
    fn frame_round_trips() {
        let f = Frame { origin: (Nm(100_000_000), Nm(120_000_000)) };
        let p = Point::new(Nm(1_000_000), Nm(2_000_000));
        let (x, y) = f.to_kicad(p);
        assert_eq!((x, y), (Nm(101_000_000), Nm(118_000_000)));
        assert_eq!(f.from_kicad(x, y), p);
    }
}
