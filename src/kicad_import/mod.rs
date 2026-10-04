//! KiCad board import: a `.kicad_pcb` (with the rules of the `.kicad_pro` / `.kicad_dru` next
//! to it, [`rules`]) into a cadlab project (DECISIONS D32).
//!
//! Written from KiCad's published S-expression file format documentation and from files
//! `kicad-cli` writes (KiCad 6 to 10 formats: nets by number or by name); it shares no code with
//! KiCad (DECISIONS D7). Footprints embedded in the board are the user's design data and become
//! project footprints; KiCad's libraries are never read.
//!
//! What comes in:
//!
//! - **Board setup:** copper layer count, finished thickness, copper thicknesses, finish and
//!   colors from the stackup.
//! - **Outline** from Edge.Cuts (lines, arcs, circles, rectangles, polygons, curves; also inside
//!   footprints), chained into closed contours: the largest is the outer edge, contours inside it
//!   are cutouts.
//! - **Footprints** become project footprints (pads of every shape: custom pads as polygon pads,
//!   trapezoids as their bounding rectangle, chamfers as rounded corners, slots as round holes,
//!   each reported;
//!   paste-only apertures as paste windows; silkscreen, fab and courtyard drawings) and
//!   placements (position, rotation, side, lock). Identical footprints are shared; a footprint
//!   equal to one in the project (within the few nanometers rotated exports leave) reuses it.
//! - **Mounting holes** (one round hole pad, `MountingHole` footprints or `H`/`MH` designators)
//!   become board holes; board-only footprints made of round holes become vias (stitching vias)
//!   or holes.
//! - **Tracks** (segments and arcs), **vias**, **zones** (outline, net, layers, priority,
//!   clearance, minimum width, pad connection, thermal settings; fills are recomputed) and
//!   **rule areas** (keep-outs; several outlines: one item per part, holes joined by cuts),
//!   **graphics and texts** on non-copper layers (project text variables substituted).
//! - **Nets:** with a circuit in the project, footprints are matched by designator and pads to
//!   pins through the part's pin map, and board nets take the circuit's names; without one, the
//!   circuit is built from the footprints and pad nets through the netlist importer
//!   ([`crate::netlist::import`], DECISIONS D27).
//!
//! Anything that cannot be represented is reported with a diagnostic (code, subject, hint),
//! never dropped silently.
//!
//! The user's own footprint and symbol libraries (`.kicad_mod`, `.pretty`, `.kicad_sym`) are
//! read by [`library`] (DECISIONS D41), with the same footprint conversion.

mod footprint;
pub mod library;
pub mod rules;

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::board::{arc_points, contour_ring, side_layer, transform};
use crate::diag::{Diagnostic, Severity};
use crate::geom::{BBox, Point};
use crate::model::Project;
use crate::model::board::{
    Board, BoardGraphic, Contour, GraphicKind, Hole, Keepout, Outline, PadConnection, Segment, Stackup, Track, Via,
    Zone,
};
use crate::model::circuit::valid_refdes;
use crate::model::footprint::{GraphicLayer, PadKind, PadShape};
use crate::model::part::FootprintRef;
use crate::model::sections::natural_cmp;
use crate::netlist::import::{
    ImportReport, KicadNetlist, LibPart, NetlistComponent, NetlistNet, NetlistPin, Node, net_name,
};
use crate::refs::ObjectRef;
use crate::sexpr::{self, Sexpr};
use crate::units::{Angle, Nm};

pub use crate::netlist::import::{ImportError, ImportErrorKind};

use footprint::{Converted, EdgeItem};

/// Oldest board format read: KiCad 6.0 (`version 20211014`).
pub const MIN_VERSION: u32 = 20211014;

/// Outline endpoints closer than this are joined.
const CHAIN_TOL: i64 = 10_000;

pub(crate) fn invalid(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> ImportError {
    ImportError { kind: ImportErrorKind::Invalid, code, message: message.into(), hint: hint.into(), subjects: vec![] }
}

/// Where cadlab's origin (0, 0) is placed on the KiCad board.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OriginMode {
    /// The auxiliary axis origin when the board sets one (cadlab's export does), else the
    /// lower-left corner of the Edge.Cuts outline.
    #[default]
    Auto,
    /// The lower-left corner of the Edge.Cuts outline's bounding box.
    Outline,
    /// The auxiliary axis (drill/place file) origin; KiCad's page origin when unset.
    Aux,
    /// KiCad's page origin (top-left corner of the sheet).
    Page,
}

/// Import options.
#[derive(Clone, Debug, Default)]
pub struct BoardImportOptions {
    /// Replace the board (every footprint placement, track, via, zone, keep-out, hole, drawing
    /// and the outline); without it the board must be empty.
    pub replace: bool,
    /// Origin placement.
    pub origin: OriginMode,
    /// Name of the imported file, for provenance and reports.
    pub file_name: String,
    /// Rules read from the `.kicad_pro` / `.kicad_dru` files, applied before the board.
    pub rules: Option<rules::KicadRules>,
}

/// How the circuit was obtained.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CircuitSource {
    /// The project's circuit: footprints matched by designator, pads to pins.
    #[default]
    Matched,
    /// Built from the board's footprints and pad nets.
    Built,
}

/// What a board import did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BoardImportReport {
    /// File imported.
    pub source: String,
    /// Board file format version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_version: Option<u32>,
    /// Program that wrote the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    /// KiCad coordinates (Y down) of cadlab's origin.
    pub origin: (Nm, Nm),
    /// Copper layers.
    pub copper_layers: u8,
    /// Finished thickness.
    pub thickness: Nm,
    /// Outline contours (outer edge + cutouts).
    pub contours: usize,
    /// Footprints placed.
    pub footprints: usize,
    /// Footprints added to the project library.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub library_footprints: Vec<String>,
    /// Mounting holes.
    pub holes: usize,
    /// Tracks.
    pub tracks: usize,
    /// Vias.
    pub vias: usize,
    /// Zones.
    pub zones: usize,
    /// Keep-outs (rule areas).
    pub keepouts: usize,
    /// Board drawings and texts.
    pub graphics: usize,
    /// Where the circuit came from.
    pub circuit: CircuitSource,
    /// The circuit built from the board (`circuit: built`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netlist: Option<ImportReport>,
    /// Parts without a footprint that got the board's footprint (`circuit: matched`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts_given_footprint: Vec<String>,
    /// Rules imported from the project files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<rules::RulesReport>,
    /// Items not imported (each reported in the diagnostics).
    pub not_imported: usize,
    /// KiCad UUID → the cadlab object it became (`track#4`, `via#2`, `U1.3`, `U1`, `zone#7`,
    /// `keepout:name`, `edge`, `text:U1/Reference`, `graphic#9`), to map KiCad reports onto the
    /// imported project. Not part of the command output.
    #[serde(skip)]
    #[schemars(skip)]
    pub uuids: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------------------------
// Small readers.

/// KiCad coordinates (millimeters in the file, Y down).
pub(super) type K = (Nm, Nm);

/// A decimal number of millimeters, exactly.
pub(super) fn mm(s: &str) -> Option<Nm> {
    Nm::parse(&format!("{}mm", s.trim())).ok()
}

/// Decimal degrees.
pub(super) fn deg(s: &str) -> Option<Angle> {
    Angle::parse(s).ok()
}

fn xy_items(e: &Sexpr) -> Option<K> {
    let it = e.items();
    Some((mm(it.get(1)?.atom()?)?, mm(it.get(2)?.atom()?)?))
}

/// `(head x y)` child.
pub(super) fn child_xy(e: &Sexpr, head: &str) -> Option<K> {
    xy_items(e.get(head)?)
}

/// `(at x y [angle])`.
pub(super) fn at_of(e: &Sexpr) -> Option<(K, Angle)> {
    let a = e.get("at")?;
    let k = xy_items(a)?;
    let ang = a.items().get(3).and_then(Sexpr::atom).and_then(deg).unwrap_or(Angle::ZERO);
    Some((k, ang))
}

/// Stroke width: `(stroke (width w))` or the older `(width w)`.
pub(super) fn width_of(e: &Sexpr) -> Nm {
    e.get("stroke").and_then(|s| s.child_value("width")).or(e.child_value("width")).and_then(mm).unwrap_or(Nm::ZERO)
}

/// `(layer "name" ...)`.
pub(super) fn layer_of(e: &Sexpr) -> Option<&str> {
    e.child_value("layer")
}

/// A flag written `(head yes)` or as a bare atom (older files).
pub(super) fn yes(e: &Sexpr, head: &str) -> bool {
    e.items().iter().skip(1).any(|c| c.atom() == Some(head))
        || e.get(head).is_some_and(|c| c.value().is_none_or(|v| v == "yes"))
}

/// `(pts (xy x y) ... (arc (start) (mid) (end)))`, arcs as polylines.
pub(super) fn pts_of(e: &Sexpr) -> Option<Vec<K>> {
    let mut out: Vec<K> = Vec::new();
    for c in e.get("pts")?.items().iter().skip(1) {
        match c.head() {
            Some("xy") => out.push(xy_items(c)?),
            Some("arc") => {
                let p = |h: &str| child_xy(c, h).map(|(x, y)| Point::new(x, y));
                for q in arc_points(p("start")?, p("mid")?, p("end")?, 5_000) {
                    let k = (q.x, q.y);
                    if out.last() != Some(&k) {
                        out.push(k);
                    }
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// The import context: coordinate frame, layers and the net table.
pub(super) struct Ctx {
    /// KiCad coordinates of cadlab's origin.
    origin: K,
    copper: Vec<String>,
    nets_by_num: BTreeMap<String, String>,
    /// Project text variables.
    vars: BTreeMap<String, String>,
}

impl Ctx {
    /// cadlab board point of a KiCad point.
    pub(super) fn frame(&self, (x, y): K) -> Point {
        Point::new(x - self.origin.0, self.origin.1 - y)
    }

    /// Layer names a (possibly wildcard) KiCad layer stands for.
    pub(super) fn expand_layer(&self, l: &str) -> Vec<String> {
        match l {
            "*.Cu" => self.copper.clone(),
            "F&B.Cu" => vec!["F.Cu".into(), "B.Cu".into()],
            _ => match l.strip_prefix("*.") {
                Some(rest) => vec![format!("F.{rest}"), format!("B.{rest}")],
                None => vec![l.to_string()],
            },
        }
    }

    /// The KiCad net name of an item: `(net 3 "GND")`, `(net 3)` through the net table, or
    /// `(net "GND")` (KiCad 10).
    pub(super) fn raw_net(&self, e: &Sexpr) -> Option<String> {
        let n = e.get("net")?;
        let it = n.items();
        let name = match it.len() {
            0..=1 => return None,
            2 => {
                let v = it[1].atom()?;
                self.nets_by_num.get(v).cloned().unwrap_or_else(|| {
                    if !self.nets_by_num.is_empty() && v.chars().all(|c| c.is_ascii_digit()) {
                        String::new()
                    } else {
                        v.to_string()
                    }
                })
            }
            _ => it[2].atom()?.to_string(),
        };
        let name = if name.is_empty() { e.child_value("net_name").unwrap_or("").to_string() } else { name };
        (!name.is_empty()).then_some(name)
    }

    fn is_copper(&self, l: &str) -> bool {
        self.copper.iter().any(|c| c == l)
    }
}

/// Diagnostics, with repeated notes aggregated.
#[derive(Default)]
pub(super) struct Notes {
    list: Vec<Diagnostic>,
    agg: BTreeMap<(String, String), (Diagnostic, usize)>,
    /// Items not imported.
    pub(super) skipped: usize,
}

impl Notes {
    pub(super) fn push(&mut self, d: Diagnostic) {
        self.list.push(d);
    }

    /// An item not imported.
    pub(super) fn not_imported(&mut self, d: Diagnostic) {
        self.skipped += 1;
        self.list.push(d);
    }

    /// A note repeated per occurrence, reported once per `(code, key)` with a count.
    /// The aggregated diagnostic names every distinct subject.
    pub(super) fn agg(&mut self, code: &str, key: &str, d: Diagnostic) {
        match self.agg.entry((code.to_string(), key.to_string())) {
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert((d, 1));
            }
            std::collections::btree_map::Entry::Occupied(mut e) => {
                let (first, n) = e.get_mut();
                *n += 1;
                for s in d.subjects {
                    if !first.subjects.contains(&s) {
                        first.subjects.push(s);
                    }
                }
            }
        }
    }

    /// An item not imported, aggregated.
    pub(super) fn not_imported_agg(&mut self, code: &str, key: &str, d: Diagnostic) {
        self.skipped += 1;
        self.agg(code, key, d);
    }

    fn finish(mut self) -> Vec<Diagnostic> {
        for (_, (mut d, n)) in std::mem::take(&mut self.agg) {
            if n > 1 {
                d.message = format!("{} ({n} times)", d.message);
            }
            self.list.push(d);
        }
        self.list
    }
}

// ---------------------------------------------------------------------------------------------
// Import.

/// A track, via, zone or keep-out read from the file, before nets are mapped.
enum RawItem {
    Track { t: Track, net: Option<String>, uuid: Option<String> },
    Via { v: Via, net: Option<String>, uuid: Option<String> },
    Zone { z: Zone, net: Option<String>, uuid: Option<String> },
    Keepout { k: Keepout, uuid: Option<String> },
}

/// Imports a `.kicad_pcb` into `p`. Returns the report and the diagnostics to show.
pub fn import(
    p: &mut Project,
    text: &str,
    opts: &BoardImportOptions,
) -> Result<(BoardImportReport, Vec<Diagnostic>), ImportError> {
    let root = sexpr::parse(text).map_err(|e| {
        invalid(
            "import.parse",
            format!("not a readable KiCad board: {e}"),
            "give a `.kicad_pcb` file saved by KiCad 6 or later",
        )
    })?;
    if root.head() != Some("kicad_pcb") {
        return Err(invalid(
            "import.not_kicad_pcb",
            format!("the file starts with `({}`, not `(kicad_pcb`", root.head().unwrap_or("")),
            "give a KiCad board file (`.kicad_pcb`)",
        ));
    }
    let version: Option<u32> = root.child_value("version").and_then(|v| v.parse().ok());
    if let Some(v) = version
        && v < MIN_VERSION
    {
        return Err(invalid(
            "import.kicad_version",
            format!("board format version {v} is older than KiCad 6 ({MIN_VERSION})"),
            "upgrade the file first: open and save it in KiCad, or run `kicad-cli pcb upgrade <file>`",
        ));
    }
    {
        let b = p.board();
        let empty = b.footprints.is_empty()
            && b.tracks.is_empty()
            && b.vias.is_empty()
            && b.zones.is_empty()
            && b.keepouts.is_empty()
            && b.holes.is_empty()
            && b.graphics.is_empty()
            && b.outline.contours.is_empty();
        if !empty && !opts.replace {
            return Err(ImportError {
                kind: ImportErrorKind::Conflict,
                code: "import.board_not_empty",
                message: "the project's board already has an outline, placements or copper".into(),
                hint: "pass `replace: true` to replace the board, or import into a new project".into(),
                subjects: vec![],
            });
        }
    }
    let mut notes = Notes::default();
    let mut report = BoardImportReport {
        source: opts.file_name.clone(),
        format_version: version,
        generator: root.child_value("generator").map(String::from),
        ..Default::default()
    };

    // Layers.
    let mut copper: Vec<String> = Vec::new();
    let mut known_layers: BTreeSet<String> = BTreeSet::new();
    if let Some(ls) = root.get("layers") {
        for l in ls.items().iter().skip(1) {
            let it = l.items();
            let (Some(name), ty) = (it.get(1).and_then(Sexpr::atom), it.get(2).and_then(Sexpr::atom)) else {
                continue;
            };
            known_layers.insert(name.to_string());
            if name.ends_with(".Cu") && ty != Some("user") {
                copper.push(name.to_string());
            }
        }
    }
    let n = copper.len();
    if n == 0 || n > 32 {
        return Err(invalid(
            "import.layers",
            format!("the board has {n} copper layers"),
            "check the layer table of the board in KiCad (Board Setup → Board Stackup)",
        ));
    }
    let mut stackup = Stackup { copper_layers: n as u8, ..p.board().stackup.clone() };
    let expected = stackup.copper_names();
    let have: BTreeSet<&String> = copper.iter().collect();
    if have != expected.iter().collect::<BTreeSet<_>>() {
        return Err(invalid(
            "import.layers",
            format!("copper layers {} are not numbered F.Cu, In1.Cu, ..., B.Cu", copper.join(", ")),
            "renumber the copper layers in KiCad's board setup",
        ));
    }
    let copper = expected;
    if let Some(t) = root.get("general").and_then(|g| g.child_value("thickness")).and_then(mm) {
        stackup.thickness = t;
    }
    if let Some(su) = root.get("setup").and_then(|s| s.get("stackup")) {
        read_stackup(su, &mut stackup, &mut notes);
    }
    report.copper_layers = stackup.copper_layers;
    report.thickness = stackup.thickness;

    if let Some(su) = root.get("setup") {
        local_overrides(su, &ObjectRef::Name("setup".into()), &mut notes);
    }

    // Origin.
    let aux = root.get("setup").and_then(|s| child_xy(s, "aux_axis_origin")).filter(|(x, y)| x.0 != 0 || y.0 != 0);
    let edge_ll = edge_bbox(&root).map(|b| (b.min.x, b.max.y));
    let origin = match opts.origin {
        OriginMode::Auto => aux.or(edge_ll).unwrap_or((Nm::ZERO, Nm::ZERO)),
        OriginMode::Outline => edge_ll.unwrap_or((Nm::ZERO, Nm::ZERO)),
        OriginMode::Aux => aux.unwrap_or((Nm::ZERO, Nm::ZERO)),
        OriginMode::Page => (Nm::ZERO, Nm::ZERO),
    };
    report.origin = origin;
    let nets_by_num: BTreeMap<String, String> = root
        .all("net")
        .filter_map(|e| {
            let it = e.items();
            Some((it.get(1)?.atom()?.to_string(), it.get(2).and_then(Sexpr::atom).unwrap_or("").to_string()))
        })
        .collect();
    let vars = opts.rules.as_ref().map(|k| k.text_variables.clone()).unwrap_or_default();
    let ctx = Ctx { origin, copper: copper.clone(), nets_by_num, vars };

    // A fresh board: setup and rules first.
    let rules_now = p.board().rules.clone();
    *p.board_mut() = Board { stackup, rules: rules_now, ..Board::default() };
    if let Some(k) = &opts.rules {
        let (r, d) = rules::apply(p, k);
        report.rules = Some(r);
        for d in d {
            notes.push(d);
        }
    }

    // Read every top-level item.
    let mut edges: Vec<(EdgeItem, Option<String>)> = Vec::new();
    let mut graphics: Vec<(BoardGraphic, Option<String>)> = Vec::new();
    let mut raw: Vec<RawItem> = Vec::new();
    let mut fps: Vec<Converted> = Vec::new();
    let mut fills = false;
    for item in root.items().iter().skip(1) {
        let Some(head) = item.head() else { continue };
        let uuid = item.child_value("uuid").or(item.child_value("tstamp")).map(String::from);
        match head {
            "version" | "generator" | "generator_version" | "general" | "paper" | "title_block" | "layers"
            | "setup" | "net" | "property" | "embedded_fonts" | "embedded_files" | "net_class" => {}
            "footprint" => {
                if let Some(c) = footprint::convert(item, &ctx, &mut notes) {
                    fps.push(c);
                }
            }
            "gr_line" | "gr_arc" | "gr_circle" | "gr_rect" | "gr_poly" | "gr_curve" => {
                read_drawing(item, head, &ctx, &known_layers, uuid, &mut edges, &mut graphics, &mut notes)
            }
            "gr_text" => read_text(item, &ctx, &known_layers, uuid, &mut graphics, &mut notes),
            "segment" | "arc" => {
                let layer = layer_of(item).unwrap_or("").to_string();
                let (Some(s), Some(e), w) = (child_xy(item, "start"), child_xy(item, "end"), item.child_value("width"))
                else {
                    notes.not_imported(
                        Diagnostic::warning("import.track_invalid", "a track without start or end; not imported")
                            .with_hint("check the board in KiCad"),
                    );
                    continue;
                };
                let mid = if head == "arc" { child_xy(item, "mid").map(|m| ctx.frame(m)) } else { None };
                if head == "arc" && mid.is_none() {
                    notes.not_imported(
                        Diagnostic::warning("import.track_invalid", "an arc track without a mid point; not imported")
                            .with_hint("save the board with KiCad 6 or later"),
                    );
                    continue;
                }
                if !ctx.is_copper(&layer) {
                    notes.not_imported(
                        Diagnostic::warning("import.track_layer", format!("a track on `{layer}`, not a copper layer; not imported"))
                            .with_subject(ObjectRef::Layer(layer.clone()))
                            .at(ctx.frame(s))
                            .with_hint("check the board's layer table"),
                    );
                    continue;
                }
                let t = Track {
                    id: crate::id::ObjectId(0),
                    layer,
                    width: w.and_then(mm).unwrap_or(Nm::ZERO),
                    net: None,
                    start: ctx.frame(s),
                    end: ctx.frame(e),
                    mid,
                    locked: yes(item, "locked"),
                };
                raw.push(RawItem::Track { t, net: ctx.raw_net(item), uuid });
            }
            "via" => {
                if let Some(v) = read_via(item, &ctx, &mut notes) {
                    raw.push(RawItem::Via { v, net: ctx.raw_net(item), uuid });
                }
            }
            "zone" => {
                fills |= item.get("filled_polygon").is_some();
                for r in read_zone(item, &ctx, uuid, &mut notes) {
                    raw.push(r);
                }
            }
            "group" | "generated" => notes.agg(
                "import.group",
                head,
                Diagnostic::info(
                    "import.group",
                    format!("KiCad `{head}` entries are not kept (their members are imported as ordinary items)"),
                ),
            ),
            other => notes.not_imported_agg(
                "import.unsupported_item",
                other,
                Diagnostic::warning("import.unsupported_item", format!("KiCad `{other}` items are not imported"))
                    .with_subject(ObjectRef::Named { kind: "kicad".into(), name: other.to_string() })
                    .with_hint(
                        "cadlab has no counterpart for them (dimensions, images, text boxes, tables, targets); redraw what matters as board graphics",
                    ),
            ),
        }
    }
    if fills {
        notes.push(Diagnostic::info(
            "import.zone_fills",
            "zone fills in the file are not imported: cadlab recomputes them from the zone settings",
        ));
    }
    for f in &fps {
        edges.extend(f.edges.iter().map(|e| (*e, None)));
    }

    // Outline.
    let (outline, edge_uuids) = chain_outline(edges, &mut notes);
    report.contours = outline.contours.len();
    p.board_mut().outline = outline;
    for u in edge_uuids {
        report.uuids.insert(u, "edge".into());
    }

    // Footprints: mounting holes, drawings only, components.
    let matched = !p.circuit().components.is_empty();
    report.circuit = if matched { CircuitSource::Matched } else { CircuitSource::Built };
    let mut holes: Vec<(Hole, Option<String>, &Converted)> = Vec::new();
    let mut comps: Vec<&Converted> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for c in &fps {
        let refdes = c.refdes.trim().to_uppercase();
        if c.fp.pads.is_empty() {
            footprint_drawings(p, c, &mut graphics, &mut notes);
            continue;
        }
        if let Some(h) = as_hole(c, matched && p.circuit().components.contains_key(&refdes)) {
            holes.push((h, c.pads.first().and_then(|n| n.net.clone()), c));
            continue;
        }
        let board_only = c.attrs.contains("board_only")
            || !valid_refdes(&refdes)
            || (matched && !p.circuit().components.contains_key(&refdes));
        if board_only && let Some(hs) = board_only_holes(c) {
            // One plated hole on a net: a stitching via footprint, so a via.
            if let [(h, Some(net))] = hs.as_slice()
                && let (Some(d), Some(first), Some(last)) = (h.pad, copper.first(), copper.last())
            {
                notes.agg(
                    "import.footprint_as_via",
                    "via",
                    Diagnostic::info(
                        "import.footprint_as_via",
                        "board-only footprints with one plated hole on a net (stitching vias) become vias",
                    )
                    .with_subject(ObjectRef::Name(c.refdes.clone())),
                );
                let v = Via {
                    id: crate::id::ObjectId(0),
                    at: h.at,
                    drill: h.drill,
                    diameter: d,
                    net: None,
                    from: first.clone(),
                    to: last.clone(),
                    locked: c.placement.locked,
                };
                let uuid = c.uuids.iter().find(|(_, w)| w == "fp").map(|(u, _)| u.clone());
                raw.push(RawItem::Via { v, net: Some(net.clone()), uuid });
                continue;
            }
            notes.agg(
                "import.footprint_as_holes",
                "holes",
                Diagnostic::info(
                    "import.footprint_as_holes",
                    "board-only footprints made of round holes (stitching vias, mouse bites) become board holes, one per pad",
                )
                .with_subject(ObjectRef::Name(c.refdes.clone())),
            );
            for (h, net) in hs {
                holes.push((h, net, c));
            }
            continue;
        }
        let subject = ObjectRef::Name(c.refdes.clone());
        if !valid_refdes(&refdes) {
            notes.not_imported(
                Diagnostic::warning(
                    "import.invalid_refdes",
                    format!(
                        "footprint `{}` has designator `{}`, which is not letters then a number; not imported",
                        c.lib_id, c.refdes
                    ),
                )
                .with_subject(subject)
                .at(c.placement.at)
                .with_hint("annotate the board in KiCad (or the schematic, then update the board) and import again"),
            );
            continue;
        }
        if !seen.insert(refdes.clone()) {
            notes.not_imported(
                Diagnostic::warning(
                    "import.duplicate_refdes",
                    format!("`{refdes}` appears twice; the first one is kept"),
                )
                .with_subject(subject)
                .at(c.placement.at)
                .with_hint("annotate the board again in KiCad so designators are unique"),
            );
            continue;
        }
        if matched && !p.circuit().components.contains_key(&refdes) {
            notes.not_imported(
                Diagnostic::warning(
                    "import.component_not_in_circuit",
                    format!("footprint `{refdes}` has no component in the project's circuit; not placed"),
                )
                .with_subject(subject)
                .at(c.placement.at)
                .with_hint("import the netlist that has it (`circuit.import --replace`), or import the board into a project without a circuit to build one from the board"),
            );
            continue;
        }
        comps.push(c);
    }

    // Circuit and net names.
    let mut chosen: BTreeMap<String, String> = BTreeMap::new();
    let net_map: BTreeMap<String, Option<String>> = if matched {
        match_circuit(p, &comps, &mut chosen, &mut report, &mut notes)
    } else {
        build_circuit(p, &comps, opts, &mut chosen, &mut report, &mut notes)?
    };
    let map_net = |k: &Option<String>| -> Option<String> {
        let k = k.as_ref()?;
        match net_map.get(k) {
            Some(v) => v.clone(),
            None => default_net(k),
        }
    };

    // Placements.
    for c in &comps {
        let refdes = c.refdes.trim().to_uppercase();
        let Some(fp_name) = chosen.get(&refdes) else { continue };
        let preferred = p
            .circuit()
            .components
            .get(&refdes)
            .and_then(|comp| p.library().parts.get(&comp.part))
            .and_then(|part| part.footprint())
            .map(|f| f.footprint.clone());
        let mut pf = c.placement.clone();
        pf.footprint = (preferred.as_deref() != Some(fp_name.as_str())).then(|| fp_name.clone());
        p.board_mut().footprints.insert(refdes.clone(), pf);
        if c.attrs.contains("dnp") {
            p.bom_mut().dnp.insert(refdes.clone());
        }
        for (u, what) in &c.uuids {
            report.uuids.insert(u.clone(), uuid_label(&refdes, what));
        }
    }
    report.footprints = p.board().footprints.len();
    let excluded = comps.iter().filter(|c| c.attrs.contains("exclude_from_bom") && !c.attrs.contains("dnp")).count();
    if excluded > 0 {
        notes.push(
            Diagnostic::info(
                "import.exclude_from_bom",
                format!(
                    "{excluded} footprint(s) are excluded from the BOM in KiCad; cadlab has no such flag and lists them"
                ),
            )
            .with_hint("mark parts that are not assembled as DNP (`bom.dnp`)"),
        );
    }

    // Holes.
    let mut hole_names: BTreeSet<String> = BTreeSet::new();
    for (mut h, net, c) in holes {
        let mut name = c.refdes.trim().to_string();
        if name.is_empty()
            || !valid_refdes(&name.to_uppercase())
            || hole_names.contains(&name)
            || p.board().footprints.contains_key(&name)
            || p.circuit().components.contains_key(&name)
        {
            let mut i = 1;
            while hole_names.contains(&format!("H{i}"))
                || p.board().footprints.contains_key(&format!("H{i}"))
                || p.circuit().components.contains_key(&format!("H{i}"))
                || fps.iter().any(|f| f.refdes == format!("H{i}"))
            {
                i += 1;
            }
            name = format!("H{i}");
        }
        hole_names.insert(name.clone());
        h.name = name.clone();
        h.id = p.alloc_id();
        h.net = if h.pad.is_some() { map_net(&net) } else { None };
        for (u, what) in &c.uuids {
            report.uuids.insert(u.clone(), uuid_label(&name, what));
        }
        let skipped = c.fp.graphics.len();
        if skipped > 0 {
            notes.agg(
                "import.hole_drawings",
                "hole",
                Diagnostic::info(
                    "import.hole_drawings",
                    "mounting hole footprints become board holes; their drawings are not kept",
                )
                .with_subject(ObjectRef::Name(name.clone())),
            );
        }
        p.board_mut().holes.push(h);
    }
    report.holes = p.board().holes.len();

    // Copper and areas.
    let mut zone_names: BTreeSet<String> = BTreeSet::new();
    for r in raw {
        match r {
            RawItem::Track { mut t, net, uuid } => {
                t.id = p.alloc_id();
                t.net = map_net(&net);
                if let Some(u) = uuid {
                    report.uuids.insert(u, format!("track#{}", t.id.0));
                }
                p.board_mut().tracks.push(t);
            }
            RawItem::Via { mut v, net, uuid } => {
                v.id = p.alloc_id();
                v.net = map_net(&net);
                if let Some(u) = uuid {
                    report.uuids.insert(u, format!("via#{}", v.id.0));
                }
                p.board_mut().vias.push(v);
            }
            RawItem::Zone { mut z, net, uuid } => {
                z.id = p.alloc_id();
                z.net = map_net(&net);
                z.name = unique_name(&z.name, &mut zone_names, || {
                    format!("{}_{}", z.net.as_deref().unwrap_or("zone"), z.layers.join("_"))
                });
                if let Some(u) = uuid {
                    report.uuids.insert(u, format!("zone#{}", z.id.0));
                }
                p.board_mut().zones.push(z);
            }
            RawItem::Keepout { mut k, uuid } => {
                k.id = p.alloc_id();
                k.name = unique_name(&k.name, &mut zone_names, || "keepout".into());
                if let Some(u) = uuid {
                    report.uuids.insert(u, format!("keepout:{}", k.name));
                }
                p.board_mut().keepouts.push(k);
            }
        }
    }
    for (mut g, uuid) in graphics {
        g.id = p.alloc_id();
        if let Some(u) = uuid {
            report.uuids.insert(u, format!("graphic#{}", g.id.0));
        }
        p.board_mut().graphics.push(g);
    }

    // Net classes, then zone settings equal to cadlab's defaults are left unset.
    if let Some(k) = &opts.rules {
        let kicad_names: BTreeMap<String, String> =
            net_map.iter().filter_map(|(k, v)| v.as_ref().map(|v| (v.clone(), k.clone()))).collect();
        let mut diags = Vec::new();
        let r = report.rules.get_or_insert_with(Default::default);
        rules::assign(p, k, &kicad_names, r, &mut diags);
        for d in diags {
            notes.push(d);
        }
    }
    normalize_zones(p);

    let b = p.board();
    report.tracks = b.tracks.len();
    report.vias = b.vias.len();
    report.zones = b.zones.len();
    report.keepouts = b.keepouts.len();
    report.graphics = b.graphics.len();
    report.not_imported = notes.skipped;
    let mut diags = notes.finish();
    // Errors first is not useful here: keep warnings before notes, stable otherwise.
    diags.sort_by_key(|d| match d.severity {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    });
    Ok((report, diags))
}

fn uuid_label(refdes: &str, what: &str) -> String {
    if let Some(n) = what.strip_prefix("pad:") {
        return if n.is_empty() { refdes.to_string() } else { format!("{refdes}.{n}") };
    }
    if let Some(k) = what.strip_prefix("prop:") {
        return format!("text:{refdes}/{k}");
    }
    refdes.to_string()
}

fn unique_name(name: &str, used: &mut BTreeSet<String>, fallback: impl FnOnce() -> String) -> String {
    let base = if name.trim().is_empty() { fallback() } else { name.trim().to_string() };
    let mut n = base.clone();
    let mut i = 2;
    while used.contains(&n) {
        n = format!("{base}_{i}");
        i += 1;
    }
    used.insert(n.clone());
    n
}

/// Board stackup details: copper thicknesses, finish, mask and silkscreen colors.
fn read_stackup(su: &Sexpr, st: &mut Stackup, notes: &mut Notes) {
    let mut dielectric = false;
    for l in su.all("layer") {
        let name = l.value().unwrap_or("");
        let thick = l.child_value("thickness").and_then(mm);
        match name {
            "F.Cu" => {
                if let Some(t) = thick {
                    st.outer_copper = t;
                }
            }
            "In1.Cu" => {
                if let Some(t) = thick {
                    st.inner_copper = t;
                }
            }
            "F.Mask" | "B.Mask" => {
                if let Some(c) = l.child_value("color").filter(|c| !c.is_empty())
                    && !st.mask_color.iter().any(|x| x == c)
                {
                    st.mask_color.push(c.to_string());
                }
            }
            "F.SilkS" | "B.SilkS" => {
                if let Some(c) = l.child_value("color").filter(|c| !c.is_empty())
                    && !st.silk_color.iter().any(|x| x == c)
                {
                    st.silk_color.push(c.to_string());
                }
            }
            n if n.starts_with("dielectric") => dielectric = true,
            _ => {}
        }
    }
    if let Some(f) = su.child_value("copper_finish").filter(|f| !f.is_empty() && *f != "None")
        && !st.finish.iter().any(|x| x == f)
    {
        st.finish.insert(0, f.to_string());
    }
    if dielectric {
        notes.push(
            Diagnostic::info(
                "import.stackup_dielectric",
                "dielectric layers of the stackup (materials, thicknesses) are not kept: cadlab stores the finished thickness and copper weights",
            )
            .with_hint("give the fab the stackup in the order notes if it matters (impedance control comes in M8)"),
        );
    }
}

/// Bounding box (KiCad coordinates) of the top-level Edge.Cuts drawings.
fn edge_bbox(root: &Sexpr) -> Option<BBox> {
    let mut pts = Vec::new();
    for e in root.items().iter().skip(1) {
        if !matches!(e.head(), Some("gr_line" | "gr_arc" | "gr_circle" | "gr_rect" | "gr_poly" | "gr_curve"))
            || layer_of(e) != Some("Edge.Cuts")
        {
            continue;
        }
        let p = |k: K| Point::new(k.0, k.1);
        if e.head() == Some("gr_circle") {
            if let (Some(c), Some(en)) = (child_xy(e, "center"), child_xy(e, "end")) {
                let d = p(en) - p(c);
                let r = Nm(((d.x.0 as f64).hypot(d.y.0 as f64)).round() as i64);
                pts.push(p(c) - Point::new(r, r));
                pts.push(p(c) + Point::new(r, r));
            }
            continue;
        }
        if e.head() == Some("gr_arc")
            && let (Some(s), Some(m), Some(en)) = (child_xy(e, "start"), child_xy(e, "mid"), child_xy(e, "end"))
        {
            pts.extend(arc_points(p(s), p(m), p(en), 1_000));
            continue;
        }
        for h in ["start", "end"] {
            if let Some(k) = child_xy(e, h) {
                pts.push(p(k));
            }
        }
        if let Some(v) = pts_of(e) {
            pts.extend(v.into_iter().map(p));
        }
    }
    BBox::of_points(pts)
}

/// A board drawing: an outline edge (Edge.Cuts) or a graphic.
#[allow(clippy::too_many_arguments)]
fn read_drawing(
    e: &Sexpr,
    head: &str,
    ctx: &Ctx,
    known: &BTreeSet<String>,
    uuid: Option<String>,
    edges: &mut Vec<(EdgeItem, Option<String>)>,
    graphics: &mut Vec<(BoardGraphic, Option<String>)>,
    notes: &mut Notes,
) {
    let layer = layer_of(e).unwrap_or("").to_string();
    let pt = |h: &str| child_xy(e, h).map(|k| ctx.frame(k));
    let bad = |notes: &mut Notes| {
        notes.not_imported(
            Diagnostic::warning(
                "import.drawing_invalid",
                format!("a `{head}` on `{layer}` could not be read; not imported"),
            )
            .with_subject(ObjectRef::Layer(layer.clone()))
            .with_hint("check the drawing in KiCad"),
        )
    };
    // Geometry as segments (a, b, mid) or a circle.
    let mut segs: Vec<(Point, Point, Option<Point>)> = Vec::new();
    let mut circle: Option<(Point, Nm)> = None;
    match head {
        "gr_line" => match (pt("start"), pt("end")) {
            (Some(a), Some(b)) => segs.push((a, b, None)),
            _ => return bad(notes),
        },
        "gr_arc" => match (pt("start"), pt("mid"), pt("end")) {
            (Some(a), Some(m), Some(b)) => segs.push((a, b, Some(m))),
            _ => return bad(notes),
        },
        "gr_rect" => match (child_xy(e, "start"), child_xy(e, "end")) {
            (Some(s), Some(en)) => {
                let c = [s, (en.0, s.1), en, (s.0, en.1)].map(|k| ctx.frame(k));
                for i in 0..4 {
                    segs.push((c[i], c[(i + 1) % 4], None));
                }
            }
            _ => return bad(notes),
        },
        "gr_poly" => match pts_of(e) {
            Some(v) if v.len() >= 2 => {
                let c: Vec<Point> = v.into_iter().map(|k| ctx.frame(k)).collect();
                for i in 0..c.len() {
                    segs.push((c[i], c[(i + 1) % c.len()], None));
                }
            }
            _ => return bad(notes),
        },
        "gr_curve" => match pts_of(e).map(|v| v.into_iter().map(|k| ctx.frame(k)).collect::<Vec<_>>()) {
            Some(c) => match footprint::bezier(&c) {
                Some(pts) => {
                    notes.agg(
                        "import.curve",
                        "curve",
                        Diagnostic::info("import.curve", "Bézier curves are approximated by 16 straight segments"),
                    );
                    for w in pts.windows(2) {
                        segs.push((w[0], w[1], None));
                    }
                }
                None => return bad(notes),
            },
            None => return bad(notes),
        },
        "gr_circle" => match (pt("center"), pt("end")) {
            (Some(c), Some(en)) => {
                let d = en - c;
                circle = Some((c, Nm(((d.x.0 as f64).hypot(d.y.0 as f64)).round() as i64)));
            }
            _ => return bad(notes),
        },
        _ => return bad(notes),
    }
    if layer == "Edge.Cuts" {
        if let Some((center, radius)) = circle {
            edges.push((EdgeItem::Circle { center, radius }, uuid));
        } else {
            for (i, (a, b, mid)) in segs.into_iter().enumerate() {
                edges.push((EdgeItem::Seg { a, b, mid }, if i == 0 { uuid.clone() } else { None }));
            }
        }
        return;
    }
    if ctx.is_copper(&layer) || layer.ends_with(".Cu") {
        notes.not_imported_agg(
            "import.copper_drawing",
            &layer,
            Diagnostic::warning(
                "import.copper_drawing",
                format!("drawings on copper layer `{layer}` are not imported"),
            )
            .with_subject(ObjectRef::Layer(layer.clone()))
            .with_hint(
                "cadlab copper is tracks, pads and zones: redraw it as tracks (`track.add`) or a zone (`zone.add`)",
            ),
        );
        return;
    }
    if !known.contains(&layer) {
        notes.not_imported_agg(
            "import.drawing_layer",
            &layer,
            Diagnostic::warning(
                "import.drawing_layer",
                format!("drawings on unknown layer `{layer}` are not imported"),
            )
            .with_subject(ObjectRef::Layer(layer.clone()))
            .with_hint("move them to a standard layer in KiCad"),
        );
        return;
    }
    if e.get("fill").and_then(Sexpr::value).is_some_and(|f| matches!(f, "solid" | "yes")) {
        notes.agg(
            "import.filled_drawing",
            "fill",
            Diagnostic::info(
                "import.filled_drawing",
                "filled board drawings are imported as their outline (cadlab board graphics are lines)",
            ),
        );
    }
    let width = width_of(e);
    let points: Vec<Point> = if let Some((c, r)) = circle {
        let (east, west) = (c + Point::new(r, Nm::ZERO), c - Point::new(r, Nm::ZERO));
        let mut v = arc_points(east, c + Point::new(Nm::ZERO, r), west, 5_000);
        v.extend(arc_points(west, c - Point::new(Nm::ZERO, r), east, 5_000).into_iter().skip(1));
        v
    } else if head == "gr_arc" {
        let (a, b, m) = segs[0];
        arc_points(a, m.expect("arc"), b, 5_000)
    } else {
        let mut v = vec![segs[0].0];
        v.extend(segs.iter().map(|s| s.1));
        v
    };
    if head == "gr_arc" || head == "gr_circle" {
        notes.agg(
            "import.drawing_arc",
            "arc",
            Diagnostic::info(
                "import.drawing_arc",
                "board arcs and circles on drawing layers are imported as polylines (5 µm tolerance)",
            ),
        );
    }
    // Consecutive lines of one layer and width that join form one polyline (as cadlab exports them).
    if head == "gr_line"
        && let Some((last, _)) = graphics.last_mut()
        && last.layer == layer
        && let GraphicKind::Line { points: lp, width: lw } = &mut last.kind
        && *lw == width
        && lp.last() == points.first()
        && lp.first() != lp.last()
    {
        lp.push(points[1]);
        return;
    }
    graphics
        .push((BoardGraphic { id: crate::id::ObjectId(0), layer, kind: GraphicKind::Line { points, width } }, uuid));
}

fn read_text(
    e: &Sexpr,
    ctx: &Ctx,
    known: &BTreeSet<String>,
    uuid: Option<String>,
    graphics: &mut Vec<(BoardGraphic, Option<String>)>,
    notes: &mut Notes,
) {
    let layer = layer_of(e).unwrap_or("").to_string();
    let mut text = e.value().unwrap_or("").to_string();
    // Project text variables become their values (cadlab texts have no variables).
    for (k, v) in &ctx.vars {
        let var = format!("${{{k}}}");
        if text.contains(&var) {
            text = text.replace(&var, v);
            notes.agg(
                "import.text_variable",
                "var",
                Diagnostic::info(
                    "import.text_variable",
                    "project text variables in board texts are replaced by their values from the .kicad_pro",
                ),
            );
        }
    }
    let Some((at, rotation)) = at_of(e) else {
        notes.not_imported(
            Diagnostic::warning("import.drawing_invalid", "a board text without position; not imported")
                .with_hint("check the text in KiCad"),
        );
        return;
    };
    if layer.ends_with(".Cu") || !known.contains(&layer) {
        notes.not_imported(
            Diagnostic::warning(
                "import.text_layer",
                format!("text `{text}` on `{layer}` is not imported (texts are kept on non-copper layers only)"),
            )
            .with_subject(ObjectRef::Layer(layer.clone()))
            .at(ctx.frame(at))
            .with_hint("copper text has no cadlab counterpart; put it on the silkscreen"),
        );
        return;
    }
    let size = e
        .get("effects")
        .and_then(|ef| ef.get("font"))
        .and_then(|f| f.get("size"))
        .and_then(|s| s.value())
        .and_then(mm)
        .unwrap_or(Nm::from_mm(1));
    graphics.push((
        BoardGraphic {
            id: crate::id::ObjectId(0),
            layer,
            kind: GraphicKind::Text { text, at: ctx.frame(at), size, rotation: rotation.normalized() },
        },
        uuid,
    ));
}

fn read_via(e: &Sexpr, ctx: &Ctx, notes: &mut Notes) -> Option<Via> {
    let (Some((at, _)), Some(size), Some(drill)) =
        (at_of(e), e.child_value("size").and_then(mm), e.child_value("drill").and_then(mm))
    else {
        notes.not_imported(
            Diagnostic::warning("import.via_invalid", "a via without position, size or drill; not imported")
                .with_hint("check the board in KiCad"),
        );
        return None;
    };
    let layers: Vec<String> = e
        .get("layers")
        .map(|l| l.items().iter().skip(1).filter_map(Sexpr::atom).flat_map(|x| ctx.expand_layer(x)).collect())
        .unwrap_or_default();
    let mut span: Vec<&String> = ctx.copper.iter().filter(|c| layers.contains(c)).collect();
    if span.is_empty() {
        span = vec![ctx.copper.first()?, ctx.copper.last()?];
    }
    if e.get("padstack").is_some() || yes(e, "remove_unused_layers") {
        notes.agg(
            "import.via_padstack",
            "via",
            Diagnostic::info(
                "import.via_padstack",
                "via pad stacks and unused-layer pad removal are not kept: cadlab vias have one pad size on every layer they span",
            ),
        );
    }
    Some(Via {
        id: crate::id::ObjectId(0),
        at: ctx.frame(at),
        drill,
        diameter: size,
        net: None,
        from: span.first().map(|s| s.to_string())?,
        to: span.last().map(|s| s.to_string())?,
        locked: yes(e, "locked"),
    })
}

fn read_zone(e: &Sexpr, ctx: &Ctx, uuid: Option<String>, notes: &mut Notes) -> Vec<RawItem> {
    let name = e.child_value("name").unwrap_or("").to_string();
    let subject =
        ObjectRef::Named { kind: "zone".into(), name: if name.is_empty() { "?".into() } else { name.clone() } };
    let mut layers: Vec<String> = Vec::new();
    if let Some(l) = e.child_value("layer") {
        layers.extend(ctx.expand_layer(l));
    }
    if let Some(ls) = e.get("layers") {
        layers.extend(ls.items().iter().skip(1).filter_map(Sexpr::atom).flat_map(|x| ctx.expand_layer(x)));
    }
    let copper: Vec<String> = ctx.copper.iter().filter(|c| layers.contains(c)).cloned().collect();
    let rings: Vec<Vec<Point>> = e
        .all("polygon")
        .filter_map(pts_of)
        .map(|v| v.into_iter().map(|k| ctx.frame(k)).collect::<Vec<Point>>())
        .filter(|v| v.len() >= 3)
        .collect();
    let outline: Vec<Point> = rings.first().cloned().unwrap_or_default();
    if outline.len() < 3 || copper.is_empty() {
        notes.not_imported(
            Diagnostic::warning(
                "import.zone_invalid",
                format!(
                    "zone `{name}` on {} has no outline or no copper layer; not imported",
                    if layers.is_empty() { "no layer".to_string() } else { layers.join(", ") }
                ),
            )
            .with_subject(subject)
            .with_hint("zones on drawing layers have no cadlab counterpart; redraw what matters as board graphics"),
        );
        return Vec::new();
    }
    // Several outlines: separate areas, or holes (a polygon inside another). cadlab zones and
    // keep-outs have one outline without holes, so the area (even-odd) becomes one item per
    // separate part, each hole joined to its outline by a zero-width cut.
    let outlines: Vec<Vec<Point>> = if rings.len() == 1 {
        rings
    } else {
        match zone_regions(&rings) {
            Ok(parts) if !parts.is_empty() => {
                notes.push(
                    Diagnostic::info(
                        "import.zone_outlines",
                        format!(
                            "zone `{name}` has {} outlines (separate areas or holes); imported as {} item(s), holes joined to their outline by zero-width cuts",
                            rings.len(),
                            parts.len()
                        ),
                    )
                    .with_subject(subject.clone())
                    .at(outline[0]),
                );
                parts
            }
            _ => {
                notes.push(
                    Diagnostic::warning(
                        "import.zone_outlines",
                        format!(
                            "zone `{name}` has {} outlines that do not form an area; only the first is imported",
                            rings.len()
                        ),
                    )
                    .with_subject(subject.clone())
                    .at(outline[0])
                    .with_hint("split it into one zone per outline (`zone.add`)"),
                );
                vec![outline.clone()]
            }
        }
    };
    // One item per outline; the UUID maps to the first.
    let split = |item: RawItem| -> Vec<RawItem> {
        let mut out = Vec::with_capacity(outlines.len());
        for (i, o) in outlines.iter().enumerate() {
            out.push(match &item {
                RawItem::Keepout { k, uuid } => RawItem::Keepout {
                    k: Keepout { outline: o.clone(), ..k.clone() },
                    uuid: if i == 0 { uuid.clone() } else { None },
                },
                RawItem::Zone { z, net, uuid } => RawItem::Zone {
                    z: Zone { outline: o.clone(), ..z.clone() },
                    net: net.clone(),
                    uuid: if i == 0 { uuid.clone() } else { None },
                },
                _ => unreachable!("zones and keep-outs only"),
            });
        }
        out
    };
    if let Some(k) = e.get("keepout") {
        let forbidden = |h: &str| k.child_value(h) == Some("not_allowed");
        let ko = Keepout {
            id: crate::id::ObjectId(0),
            name: if name.is_empty() { String::new() } else { name.clone() },
            layers: if copper.len() == ctx.copper.len() { Vec::new() } else { copper },
            outline,
            no_tracks: forbidden("tracks"),
            no_vias: forbidden("vias"),
            no_pours: forbidden("copperpour"),
            no_footprints: forbidden("footprints"),
        };
        if forbidden("pads") {
            notes.push(
                Diagnostic::warning(
                    "import.keepout_pads",
                    format!("rule area `{name}` forbids pads; cadlab keep-outs cannot forbid pads alone (footprints are forbidden as a whole when KiCad says so)"),
                )
                .with_subject(subject.clone())
                .with_hint("forbid footprints in the keep-out (`keepout.add --no-footprints`) if no part may sit there"),
            );
        }
        if !(ko.no_tracks || ko.no_vias || ko.no_pours || ko.no_footprints) {
            notes.not_imported(
                Diagnostic::warning(
                    "import.rule_area",
                    format!(
                        "rule area `{name}` forbids nothing cadlab checks (placement or custom rule area); not imported"
                    ),
                )
                .with_subject(subject)
                .with_hint("add a keep-out (`keepout.add`) if it should keep something out"),
            );
            return Vec::new();
        }
        return split(RawItem::Keepout { k: ko, uuid });
    }
    let cp = e.get("connect_pads");
    let pads = match cp.and_then(|c| c.items().get(1)).and_then(Sexpr::atom) {
        Some("yes") => PadConnection::Solid,
        Some("no") => PadConnection::None,
        Some("thru_hole_only") => {
            notes.push(
                Diagnostic::info(
                    "import.zone_connection",
                    format!("zone `{name}` connects through-hole pads only with thermal reliefs; cadlab uses thermal reliefs for all pads"),
                )
                .with_subject(subject.clone()),
            );
            PadConnection::Thermal
        }
        _ => PadConnection::Thermal,
    };
    let fill = e.get("fill");
    if fill.and_then(|f| f.child_value("mode")) == Some("hatch") {
        notes.push(
            Diagnostic::info("import.zone_hatch", format!("zone `{name}` is hatched in KiCad; cadlab fills it solid"))
                .with_subject(subject.clone()),
        );
    }
    let z = Zone {
        id: crate::id::ObjectId(0),
        name,
        net: None,
        layers: copper,
        outline,
        priority: e.child_value("priority").and_then(|v| v.parse().ok()).unwrap_or(0),
        clearance: cp.and_then(|c| c.child_value("clearance")).and_then(mm),
        min_width: e.child_value("min_thickness").and_then(mm),
        pads,
        thermal_gap: fill.and_then(|f| f.child_value("thermal_gap")).and_then(mm),
        thermal_spoke: fill.and_then(|f| f.child_value("thermal_bridge_width")).and_then(mm),
    };
    split(RawItem::Zone { z, net: ctx.raw_net(e), uuid })
}

/// Per-item settings cadlab has no counterpart for, on a footprint, a pad or the board setup:
/// solder mask and paste margins (mask and paste openings come out equal to the pads, plus the
/// fab export's mask expansion), local clearances and zone connections (the net's clearance and
/// the zone's connection apply). Reported once per kind with every subject, never dropped
/// silently.
pub(super) fn local_overrides(e: &Sexpr, subject: &ObjectRef, notes: &mut Notes) {
    const KINDS: &[(&str, &str, &str)] = &[
        (
            "solder_mask_margin",
            "solder mask margins are not kept: mask openings equal the pads (plus the fab export's mask expansion)",
            "check the mask openings, or set the expansion at export (`export.gerber --mask-expansion`)",
        ),
        (
            "pad_to_mask_clearance",
            "the board's solder mask expansion is not kept: mask openings equal the pads (plus the fab export's mask expansion)",
            "set the expansion at export (`export.gerber --mask-expansion`)",
        ),
        (
            "solder_paste_margin",
            "solder paste margins are not kept: paste openings equal the pads",
            "check the paste layer; exposed pads can get paste windows (`footprint.generate`)",
        ),
        (
            "solder_paste_margin_ratio",
            "solder paste margin ratios are not kept: paste openings equal the pads",
            "check the paste layer; exposed pads can get paste windows (`footprint.generate`)",
        ),
        (
            "solder_paste_ratio",
            "solder paste margin ratios are not kept: paste openings equal the pads",
            "check the paste layer; exposed pads can get paste windows (`footprint.generate`)",
        ),
        (
            "pad_to_paste_clearance",
            "the board's paste margin is not kept: paste openings equal the pads",
            "check the paste layer",
        ),
        (
            "pad_to_paste_clearance_ratio",
            "the board's paste margin ratio is not kept: paste openings equal the pads",
            "check the paste layer",
        ),
        (
            "clearance",
            "local clearances of pads and footprints are not kept: the net (class) clearance applies",
            "set a net class clearance (`netclass.set`) if the design needs it",
        ),
        (
            "zone_connect",
            "pad and footprint zone connection overrides are not kept: the zone's pad connection applies",
            "set the zone's pad connection (`zone.set --pads`)",
        ),
    ];
    for &(head, msg, hint) in KINDS {
        let Some(v) = e.child_value(head) else { continue };
        // Zero margins and clearances are KiCad's defaults (nothing to keep); a zone connection
        // of 0 means "not connected".
        if head != "zone_connect" && v.parse::<f64>().is_ok_and(|x| x == 0.0) {
            continue;
        }
        notes.agg(
            head,
            "local",
            Diagnostic::warning("import.local_setting", msg).with_subject(subject.clone()).with_hint(hint),
        );
    }
}

/// The area of several zone outlines under the even-odd rule (a polygon inside another is a
/// hole), as hole-free outlines: one per separate part, holes joined by zero-width cuts.
fn zone_regions(rings: &[Vec<Point>]) -> Result<Vec<Vec<Point>>, crate::geom::poly::Error> {
    use crate::geom::poly::{self, FillRule, Ring};
    let rs: Vec<Ring> =
        rings.iter().map(|r| Ring::from(r.iter().map(|&q| q.into()).collect::<Vec<poly::Point>>())).collect();
    let set = poly::union_all(&rs, FillRule::EvenOdd)?;
    set.iter().map(|pg| poly::fracture(pg).map(|ring| ring.0.iter().map(|&q| q.into()).collect())).collect()
}

/// Leaves unset the zone settings equal to what cadlab would use by default.
fn normalize_zones(p: &mut Project) {
    let zones = p.board().zones.clone();
    let mut out = Vec::with_capacity(zones.len());
    for mut z in zones {
        let bare = Zone { clearance: None, min_width: None, thermal_gap: None, thermal_spoke: None, ..z.clone() };
        let d = crate::board::zones::zone_params(p, &bare);
        if z.clearance == Some(d.clearance) {
            z.clearance = None;
        }
        if z.min_width == Some(d.min_width) {
            z.min_width = None;
        }
        let d = crate::board::zones::zone_params(p, &Zone { thermal_gap: None, thermal_spoke: None, ..z.clone() });
        if z.thermal_gap == Some(d.thermal_gap) {
            z.thermal_gap = None;
        }
        if z.thermal_spoke == Some(d.thermal_spoke) {
            z.thermal_spoke = None;
        }
        out.push(z);
    }
    p.board_mut().zones = out;
}

fn near(a: Point, b: Point, tol: i64) -> bool {
    (a.x.0 - b.x.0).abs() <= tol && (a.y.0 - b.y.0).abs() <= tol
}

/// Chains Edge.Cuts drawings into closed contours: the largest is the outer edge, contours
/// inside it are cutouts. Returns the outline and the UUIDs of the drawings used.
fn chain_outline(edges: Vec<(EdgeItem, Option<String>)>, notes: &mut Notes) -> (Outline, Vec<String>) {
    let mut contours: Vec<Contour> = Vec::new();
    let mut uuids = Vec::new();
    let mut segs: Vec<(Point, Point, Option<Point>)> = Vec::new();
    for (e, u) in edges {
        uuids.extend(u);
        match e {
            EdgeItem::Circle { center: c, radius: r } => {
                let (east, west) = (c + Point::new(r, Nm::ZERO), c - Point::new(r, Nm::ZERO));
                contours.push(Contour {
                    start: east,
                    segments: vec![
                        Segment::Arc { mid: c + Point::new(Nm::ZERO, r), to: west },
                        Segment::Arc { mid: c - Point::new(Nm::ZERO, r), to: east },
                    ],
                });
            }
            EdgeItem::Seg { a, b, mid } => {
                if a != b || mid.is_some() {
                    segs.push((a, b, mid));
                }
            }
        }
    }
    let mut used = vec![false; segs.len()];
    let mut snapped = false;
    for i in 0..segs.len() {
        if used[i] {
            continue;
        }
        used[i] = true;
        let (start, b0, m0) = segs[i];
        let mut segments = vec![match m0 {
            Some(mid) => Segment::Arc { mid, to: b0 },
            None => Segment::Line { to: b0 },
        }];
        let mut cur = b0;
        let closed = loop {
            if near(cur, start, CHAIN_TOL) && segments.len() > 1 {
                if cur != start {
                    snapped = true;
                    match segments.last_mut() {
                        Some(Segment::Line { to } | Segment::Arc { to, .. }) => *to = start,
                        None => {}
                    }
                }
                break true;
            }
            // Exact matches first, then the nearest within the tolerance.
            let mut best: Option<(usize, bool, i64)> = None;
            for (j, (a, b, _)) in segs.iter().enumerate() {
                if used[j] {
                    continue;
                }
                for (rev, q) in [(false, *a), (true, *b)] {
                    let d = (q.x.0 - cur.x.0).abs().max((q.y.0 - cur.y.0).abs());
                    if d <= CHAIN_TOL && best.is_none_or(|(_, _, bd)| d < bd) {
                        best = Some((j, rev, d));
                    }
                }
            }
            let Some((j, rev, d)) = best else { break false };
            snapped |= d > 0;
            used[j] = true;
            let (a, b, m) = segs[j];
            let (_, to) = if rev { (b, a) } else { (a, b) };
            segments.push(match m {
                Some(mid) => Segment::Arc { mid, to },
                None => Segment::Line { to },
            });
            cur = to;
        };
        if closed {
            contours.push(Contour { start, segments });
        } else {
            notes.not_imported(
                Diagnostic::warning(
                    "import.outline_open",
                    format!("Edge.Cuts drawings from ({}, {}) do not close; not part of the outline", start.x, start.y),
                )
                .with_subject(ObjectRef::Layer("Edge.Cuts".into()))
                .at(start)
                .with_hint(
                    "close the board outline in KiCad (every Edge.Cuts end must meet another), then import again",
                ),
            );
        }
    }
    if snapped {
        notes.push(
            Diagnostic::info("import.outline_snapped", "Edge.Cuts ends up to 10 µm apart were joined")
                .with_subject(ObjectRef::Layer("Edge.Cuts".into())),
        );
    }
    if contours.is_empty() {
        notes.push(
            Diagnostic::warning("import.no_outline", "the board has no closed Edge.Cuts outline")
                .with_hint("draw the outline with `board.outline`"),
        );
        return (Outline::default(), uuids);
    }
    // Outer contour: the largest bounding box; cutouts: contours inside it.
    let boxes: Vec<BBox> = contours
        .iter()
        .map(|c| {
            BBox::of_points(contour_ring(c, crate::board::COPPER_TOL).into_iter().map(Point::from))
                .unwrap_or(BBox::new(c.start, c.start))
        })
        .collect();
    let area = |b: &BBox| b.width().0 as i128 * b.height().0 as i128;
    let outer = (0..contours.len()).max_by(|&a, &b| area(&boxes[a]).cmp(&area(&boxes[b])).then(b.cmp(&a))).unwrap_or(0);
    let ob = boxes[outer];
    let mut out = vec![contours[outer].clone()];
    for (i, c) in contours.into_iter().enumerate() {
        if i == outer {
            continue;
        }
        if ob.contains(boxes[i].min) && ob.contains(boxes[i].max) {
            out.push(c);
        } else {
            notes.not_imported(
                Diagnostic::warning(
                    "import.outline_extra",
                    "a closed Edge.Cuts contour outside the board outline is not imported (a cadlab board has one outer edge)",
                )
                .with_subject(ObjectRef::Layer("Edge.Cuts".into()))
                .at(c.start)
                .with_hint("panelize after export, or import each board separately"),
            );
        }
    }
    (Outline { contours: out }, uuids)
}

/// A mounting hole footprint as a board hole (name, id and net set by the caller).
fn as_hole(c: &Converted, in_circuit: bool) -> Option<Hole> {
    if in_circuit || c.fp.pads.len() != 1 {
        return None;
    }
    let pad = &c.fp.pads[0];
    let lib = c.lib_id.to_ascii_lowercase();
    let prefix = c.refdes.trim_end_matches(|ch: char| ch.is_ascii_digit()).to_ascii_uppercase();
    if !(lib.contains("mountinghole") || matches!(prefix.as_str(), "H" | "MH")) {
        return None;
    }
    if !(pad.number.is_empty() || pad.number == "1") {
        return None;
    }
    let round = match pad.shape {
        PadShape::Circle { d } => Some(d),
        PadShape::Oval { w, h } if w == h => Some(w),
        _ => None,
    };
    let (drill, padd) = match (pad.kind, round) {
        (PadKind::Npth { drill }, _) => (drill, None),
        (PadKind::Tht { drill }, Some(d)) => (drill, Some(d)),
        _ => return None,
    };
    let at = transform(&c.placement)(pad.at);
    Some(Hole { id: crate::id::ObjectId(0), name: String::new(), at, drill, pad: padd, net: None })
}

/// A footprint that cannot be a circuit component (KiCad's board-only footprints: not in the
/// netlist, or with a designator cadlab cannot use, such as stitching vias that all share one, or
/// `mouse-bite` hole patterns) made of round holes only: one board hole per pad (plated with its
/// pad and net, or non-plated), so no copper or drill is lost. Names are set by the caller.
fn board_only_holes(c: &Converted) -> Option<Vec<(Hole, Option<String>)>> {
    if c.fp.pads.is_empty() {
        return None;
    }
    let tf = transform(&c.placement);
    let mut out = Vec::new();
    for pad in &c.fp.pads {
        let round = match pad.shape {
            PadShape::Circle { d } => Some(d),
            PadShape::Oval { w, h } if w == h => Some(w),
            _ => None,
        };
        let (drill, padd) = match (pad.kind, round) {
            (PadKind::Npth { drill }, _) => (drill, None),
            (PadKind::Tht { drill }, Some(d)) if d > drill => (drill, Some(d)),
            _ => return None,
        };
        let net = padd.and(c.pads.iter().find(|n| n.number == pad.number).and_then(|n| n.net.clone()));
        out.push((
            Hole { id: crate::id::ObjectId(0), name: String::new(), at: tf(pad.at), drill, pad: padd, net: None },
            net,
        ));
    }
    Some(out)
}

/// A footprint without pads (logo, marking): its drawings become board graphics.
fn footprint_drawings(
    p: &Project,
    c: &Converted,
    graphics: &mut Vec<(BoardGraphic, Option<String>)>,
    notes: &mut Notes,
) {
    let _ = p;
    let side = c.placement.side;
    let mut n = 0;
    for g in &c.fp.graphics {
        let front = match g.layer {
            GraphicLayer::Silk => "F.SilkS",
            GraphicLayer::Fab => "F.Fab",
            GraphicLayer::Courtyard => "F.CrtYd",
        };
        let points = footprint::to_board(&c.placement, &g.geometry);
        if points.len() < 2 {
            continue;
        }
        graphics.push((
            BoardGraphic {
                id: crate::id::ObjectId(0),
                layer: side_layer(side, front),
                kind: GraphicKind::Line { points, width: g.width },
            },
            None,
        ));
        n += 1;
    }
    let what = if c.refdes.is_empty() { c.lib_id.clone() } else { c.refdes.clone() };
    if n > 0 {
        notes.agg(
            "import.footprint_as_graphics",
            "fp",
            Diagnostic::info(
                "import.footprint_as_graphics",
                format!("footprint `{what}` has no pads; its drawings become board graphics"),
            )
            .with_subject(ObjectRef::Name(what)),
        );
    } else {
        notes.not_imported(
            Diagnostic::warning(
                "import.footprint_empty",
                format!("footprint `{what}` has no pads and no drawings cadlab keeps; not imported"),
            )
            .with_subject(ObjectRef::Name(what))
            .at(c.placement.at)
            .with_hint("it may hold only texts or drawings on other layers; redraw what matters as board graphics"),
        );
    }
}

/// The project footprint for a converted one: the part's preferred footprint or a library
/// footprint of the same name when equivalent, else the converted footprint added under a free
/// name (`NAME`, `NAME_2`, ...).
fn choose_footprint(p: &mut Project, c: &Converted, preferred: Option<&str>, added: &mut Vec<String>) -> String {
    if let Some(name) = preferred
        && let Some(f) = p.library().footprints.get(name)
        && footprint::equivalent(f, &c.fp)
    {
        return name.to_string();
    }
    let base = c.fp.name.clone();
    for i in 1.. {
        let name = if i == 1 { base.clone() } else { format!("{base}_{i}") };
        let existing = p.library().find_footprint_ci(&name).map(String::from);
        match existing {
            Some(k) if footprint::equivalent(&p.library().footprints[&k], &c.fp) => return k,
            Some(_) => continue,
            None => {
                let mut fp = c.fp.clone();
                fp.name = name.clone();
                p.library_mut().footprints.insert(name.clone(), fp);
                added.push(name.clone());
                return name;
            }
        }
    }
    unreachable!("a free footprint name exists")
}

/// Matches footprints to the circuit's components; returns KiCad net → circuit net.
fn match_circuit(
    p: &mut Project,
    comps: &[&Converted],
    chosen: &mut BTreeMap<String, String>,
    report: &mut BoardImportReport,
    notes: &mut Notes,
) -> BTreeMap<String, Option<String>> {
    // Pin → net in the circuit.
    let mut pin_net: BTreeMap<(String, String), String> = BTreeMap::new();
    for (name, net) in &p.circuit().nets {
        for pin in &net.pins {
            pin_net.entry((pin.refdes.clone(), pin.pin.clone())).or_insert(name.clone());
        }
    }
    // KiCad net → circuit net → number of pads.
    let mut votes: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut pad_votes: Vec<(String, String, String, Option<String>)> = Vec::new();
    for c in comps {
        let refdes = c.refdes.trim().to_uppercase();
        let part_id = p.circuit().components[&refdes].part.clone();
        let part = p.library().parts.get(&part_id).cloned();
        let preferred = part.as_ref().and_then(|x| x.footprint()).map(|f| f.footprint.clone());
        let mut added = Vec::new();
        let name = choose_footprint(p, c, preferred.as_deref(), &mut added);
        report.library_footprints.extend(added);
        if let Some(part) = &part
            && part.footprints.is_empty()
            && let Some(lp) = p.library_mut().parts.get_mut(&part_id)
        {
            lp.footprints.push(FootprintRef::new(name.clone()));
            report.parts_given_footprint.push(part_id.clone());
        }
        chosen.insert(refdes.clone(), name.clone());
        let Some(part) = part else {
            notes.push(
                Diagnostic::warning(
                    "import.part_missing",
                    format!("`{refdes}` uses part `{part_id}`, which is not in the library"),
                )
                .with_subject(ObjectRef::Name(refdes.clone()))
                .with_hint("add the part (`part.create`) or replace it (`bom.replace`)"),
            );
            continue;
        };
        let fref = part.footprint().cloned().unwrap_or_else(|| FootprintRef::new(name.clone()));
        let fp_pads: BTreeSet<&str> = c.fp.pads.iter().map(|x| x.number.as_str()).collect();
        let missing: Vec<String> = part
            .symbol
            .pins
            .iter()
            .flat_map(|pin| fref.pads_for(&pin.number))
            .filter(|pad| !fp_pads.contains(pad.as_str()))
            .collect();
        if !missing.is_empty() {
            notes.push(
                Diagnostic::warning(
                    "import.pad_missing",
                    format!("the board footprint of `{refdes}` has no pad(s) {} for pins of part `{part_id}`", missing.join(", ")),
                )
                .with_subject(ObjectRef::Name(refdes.clone()))
                .with_hint("the board's footprint and the part number pads differently: fix the part's pin map (`part.set`) or the footprint"),
            );
        }
        for pn in &c.pads {
            let pins: Vec<&str> = part
                .symbol
                .pins
                .iter()
                .filter(|pin| fref.pads_for(&pin.number).contains(&pn.number))
                .map(|pin| pin.number.as_str())
                .collect();
            if pins.is_empty() {
                if pn.net.is_some() {
                    notes.push(
                        Diagnostic::warning(
                            "import.pad_not_in_part",
                            format!(
                                "pad {refdes}.{} has a net on the board but no pin of part `{part_id}` uses it",
                                pn.number
                            ),
                        )
                        .with_subject(ObjectRef::Pin { component: refdes.clone(), pin: pn.number.clone() })
                        .with_hint("fix the part's pin map (`part.set`) so the pad belongs to a pin"),
                    );
                }
                continue;
            }
            for pin in pins {
                let cnet = pin_net.get(&(refdes.clone(), pin.to_string())).cloned();
                if let (Some(k), Some(cn)) = (&pn.net, &cnet) {
                    *votes.entry(k.clone()).or_default().entry(cn.clone()).or_default() += 1;
                }
                pad_votes.push((refdes.clone(), pin.to_string(), pn.net.clone().unwrap_or_default(), cnet));
            }
        }
    }
    let mut map: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (k, v) in &votes {
        let best = v.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))).map(|(n, _)| n.clone());
        if v.len() > 1 {
            notes.push(
                Diagnostic::warning(
                    "import.net_conflict",
                    format!(
                        "board net `{k}` joins pins of circuit nets {}; its copper is given `{}`",
                        v.keys().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", "),
                        best.clone().unwrap_or_default()
                    ),
                )
                .with_subject(ObjectRef::Net(k.clone()))
                .with_hint("the board and the circuit disagree: fix the circuit (`net.connect`) or the board's copper, then run `drc.run`"),
            );
        }
        map.insert(k.clone(), best);
    }
    for (refdes, pin, k, cnet) in pad_votes {
        let mapped = if k.is_empty() { None } else { map.get(&k).cloned().flatten() };
        if !k.is_empty() && cnet.is_none() && !k.starts_with("unconnected-") {
            notes.push(
                Diagnostic::warning(
                    "import.net_mismatch",
                    format!("pin {refdes}.{pin} is on net `{k}` on the board but on no net in the circuit"),
                )
                .with_subject(ObjectRef::Pin { component: refdes, pin })
                .with_subject(ObjectRef::Net(k))
                .with_hint(
                    "connect it in the circuit (`net.connect`) or remove the copper; DRC reports the difference",
                ),
            );
        } else if let (Some(m), Some(c)) = (&mapped, &cnet)
            && m != c
        {
            notes.push(
                Diagnostic::warning(
                    "import.net_mismatch",
                    format!("pin {refdes}.{pin} is on `{c}` in the circuit, but its board net `{k}` became `{m}`"),
                )
                .with_subject(ObjectRef::Pin { component: refdes, pin })
                .with_subject(ObjectRef::Net(c.clone()))
                .with_hint("fix the circuit or the board's copper so they agree; DRC reports the short"),
            );
        }
    }
    // Pads on board nets whose pins are on no circuit net: the KiCad name.
    for k in raw_only_nets(comps) {
        map.entry(k).or_insert(None);
    }
    map.into_iter().map(|(k, v)| (k.clone(), v.or_else(|| default_net(&k)))).collect()
}

/// The cadlab net of a KiCad net found nowhere in the circuit: its name without the root
/// sheet prefix, or none for KiCad's single-pad `unconnected-(...)` nets.
fn default_net(k: &str) -> Option<String> {
    (!k.starts_with("unconnected-")).then(|| net_name(k, ""))
}

/// KiCad nets of pads not on any circuit pin (only used to complete the map).
fn raw_only_nets(comps: &[&Converted]) -> Vec<String> {
    let mut v: Vec<String> = comps.iter().flat_map(|c| c.pads.iter().filter_map(|p| p.net.clone())).collect();
    v.sort();
    v.dedup();
    v
}

/// Builds the circuit from the footprints through the netlist importer; returns KiCad net →
/// circuit net.
fn build_circuit(
    p: &mut Project,
    comps: &[&Converted],
    opts: &BoardImportOptions,
    chosen: &mut BTreeMap<String, String>,
    report: &mut BoardImportReport,
    notes: &mut Notes,
) -> Result<BTreeMap<String, Option<String>>, ImportError> {
    // Footprints first, so parts created from the netlist find them by name.
    for c in comps {
        let mut added = Vec::new();
        let name = choose_footprint(p, c, None, &mut added);
        report.library_footprints.extend(added);
        chosen.insert(c.refdes.trim().to_uppercase(), name);
    }
    let mut nl =
        KicadNetlist { source: Some(opts.file_name.clone()), tool: report.generator.clone(), ..Default::default() };
    let mut libparts: BTreeMap<(String, String), LibPart> = BTreeMap::new();
    let mut nets: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    for c in comps {
        let refdes = c.refdes.trim().to_uppercase();
        let fp_name = chosen[&refdes].clone();
        let symbol = format!("{fp_name}:{}", c.value);
        let prop = |k: &str| {
            c.props
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(k))
                .map(|(_, v)| v.trim().to_string())
                .filter(|v| !v.is_empty() && v != "~")
        };
        let fields: BTreeMap<String, String> = c
            .props
            .iter()
            .filter(|(k, v)| {
                let l = k.to_ascii_lowercase();
                !v.trim().is_empty()
                    && v.trim() != "~"
                    && !crate::netlist::import::STANDARD_FIELDS.contains(&l.as_str())
                    && !l.starts_with("ki_")
            })
            .map(|(k, v)| (k.clone(), v.trim().to_string()))
            .collect();
        nl.components.push(NetlistComponent {
            refdes: refdes.clone(),
            value: c.value.trim().to_string(),
            footprint: Some(format!("kicad_pcb:{fp_name}")),
            datasheet: prop("Datasheet"),
            description: prop("Description"),
            lib: Some("kicad_pcb".into()),
            part: Some(symbol.clone()),
            fields,
            dnp: c.attrs.contains("dnp"),
        });
        let lp = libparts.entry(("kicad_pcb".into(), symbol.clone())).or_insert_with(|| LibPart {
            lib: "kicad_pcb".into(),
            part: symbol.clone(),
            description: None,
            pins: Vec::new(),
        });
        let mut numbers: Vec<&str> = c.fp.pad_numbers();
        numbers.sort_by(|a, b| natural_cmp(a, b));
        for num in numbers {
            let info = c.pads.iter().find(|x| x.number == num);
            let name = info
                .and_then(|x| x.function.clone())
                .filter(|f| f != num && f != "~" && !f.is_empty())
                .unwrap_or_default();
            if let Some(existing) = lp.pins.iter_mut().find(|x| x.number == num) {
                if existing.name.is_empty() {
                    existing.name = name;
                }
                continue;
            }
            lp.pins.push(NetlistPin { number: num.to_string(), name, kind: info.and_then(|x| x.kind) });
        }
        let mut seen_pads: BTreeSet<&str> = BTreeSet::new();
        for pn in &c.pads {
            let Some(k) = &pn.net else { continue };
            if !seen_pads.insert(pn.number.as_str()) {
                continue;
            }
            nets.entry(k.clone()).or_default().push(Node {
                refdes: refdes.clone(),
                pin: pn.number.clone(),
                function: pn.function.clone(),
                kind: pn.kind,
            });
        }
    }
    nl.libparts = libparts.into_values().collect();
    let mut map: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (i, (k, nodes)) in nets.into_iter().enumerate() {
        let code = (i + 1).to_string();
        let skipped = k.starts_with("unconnected-") && nodes.len() <= 1;
        map.insert(k.clone(), if skipped { None } else { Some(net_name(&k, &code)) });
        nl.nets.push(NetlistNet { code, name: k, class: None, nodes });
    }
    if !nl.components.is_empty() {
        let o = crate::netlist::import::ImportOptions { replace: false, file_name: opts.file_name.clone() };
        let (r, d) = crate::netlist::import::import(p, &nl, &o)?;
        // A generic part's generated footprint is kept out when the board brought its own of
        // that name: that is the point, not news.
        let ours: BTreeSet<String> = chosen.values().map(|n| format!("`{n}`")).collect();
        for d in d {
            if d.code == "footprint.kept_existing" && ours.iter().any(|n| d.message.contains(n.as_str())) {
                continue;
            }
            notes.push(d);
        }
        report.netlist = Some(r);
    }
    Ok(map)
}

#[cfg(test)]
mod tests;
