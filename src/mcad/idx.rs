//! IDX (ProSTEP iViP PSI 5 "ECAD/MCAD Collaboration", EDMD schema) baseline export, written
//! from the published recommendation, implementation guidelines and schema (IDX V4.5; schema
//! namespaces `http://www.prostep.org/ecad-mcad/edmd/4.0/...`).
//!
//! The file is one `EDMDDataSet` with a `SendInformation` process instruction (a baseline:
//! the whole board, sent to start or reset a collaboration). Every feature follows the pattern
//! of the guidelines: an `Item` of type `assembly` with a `GeometryType` attribute (the IDX 4.0
//! "simplified" classification) and one `ItemInstance`, which references an `Item` of type
//! `single` whose shape is the "traditional" classification object (`Stratum`,
//! `InterStratumFeature`, `KeepOut`, `AssemblyComponent`) pointing at a `ShapeElement`, a
//! `CurveSet2d` (a curve extruded between a lower and an upper bound: "2.5D") and its curves.
//! Writing both classifications costs little and keeps readers of either method working.
//!
//! - Board (`BOARD_OUTLINE`): a `Stratum` (`DesignLayerStratum`, `PrimarySurface`) with the
//!   outline (Z 0 to the stackup thickness) and one inverted shape element per cutout.
//! - Holes (`HOLE_PLATED` / `HOLE_NON_PLATED`, and `VIA` when vias are requested): a circle of
//!   the finished diameter through the board, an `InterStratumFeature` (`PlatedCutout`,
//!   `Cutout`, `Via`) on the board stratum, placed by a 2D transformation; holes of the same
//!   kind and size share one padstack item.
//! - Keep-outs: one item per forbidden kind of a cadlab keep-out: tracks
//!   `KEEPOUT_AREA_ROUTING` (`Route`), vias `KEEPOUT_AREA_VIA` (`Via`), pours
//!   `KEEPOUT_AREA_OTHER` (`Plane`) through the board, footprints `KEEPOUT_AREA_COMPONENT`
//!   (`ComponentPlacement`) per side, from the board surface outward without an upper limit.
//! - Components (`COMPONENT`, `AssemblyComponent` of type `Physical`): the package body
//!   rectangle extruded to the body height, one item per footprint and part number, placed per
//!   designator with a 3D transformation: rotated about Z on the top face (Z = thickness), or
//!   turned over (X → -X, Z → -Z) then rotated on the bottom face (Z = 0), so bottom bodies
//!   hang below the board. Part number (`PARTNUM`) and `HEIGHT` are user properties of the
//!   component item; `REFDES` and `SIDE` of the instance.
//!
//! Coordinates are board coordinates in millimeters (Y up), Z = 0 at the board's bottom face
//! (the IDX convention: the bottom mounting surface). Transformation matrices are
//! `x' = xx·x + xy·y + xz·z + tx` (and likewise for y, z); arcs carry their included angle in
//! degrees, negative for clockwise. Identifiers (`SystemScope` `CADLAB`) are derived from
//! designators, hole and keep-out names, so a later export names the same objects the same
//! way. The header time stamps are fixed (`1970-01-01T00:00:00Z`): output is byte-identical
//! across runs.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::{BodyBox, Edge, Loop, Options, board_profile, bodies, radius, sweep_deg};
use crate::fabout::gerber::mm;
use crate::fabout::ipc2581::esc;
use crate::fabout::{self, HoleKind};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::BoardSide;
use crate::units::Nm;

/// Fixed creation/modification time stamp (deterministic output).
pub const TIME_STAMP: &str = "1970-01-01T00:00:00Z";

/// Namespace prefix of the EDMD 4.0 schema namespaces.
pub const NAMESPACE: &str = "http://www.prostep.org/ecad-mcad/edmd/4.0/";

/// The system scope of every identifier and name written.
const SCOPE: &str = "CADLAB";

/// Result of an IDX export.
#[derive(Clone, Debug)]
pub struct IdxOut {
    /// The file content.
    pub content: String,
    /// Components without a package body (left out).
    pub no_body: Vec<String>,
    /// Components written.
    pub components: usize,
    /// Holes written.
    pub holes: usize,
    /// Keep-out items written (one per forbidden kind and side).
    pub keepouts: usize,
}

/// Indented XML writer with text content.
struct Xml {
    out: String,
    depth: usize,
    /// Next number per id prefix.
    ids: BTreeMap<&'static str, usize>,
}

type Attrs<'a> = &'a [(&'a str, &'a str)];

impl Xml {
    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
    }

    fn start(&mut self, name: &str, attrs: Attrs<'_>) {
        self.indent();
        self.out.push('<');
        self.out.push_str(name);
        for (k, v) in attrs {
            let _ = write!(self.out, " {k}=\"{}\"", esc(v));
        }
    }

    fn open(&mut self, name: &str, attrs: Attrs<'_>) {
        self.start(name, attrs);
        self.out.push_str(">\n");
        self.depth += 1;
    }

    fn close(&mut self, name: &str) {
        self.depth -= 1;
        self.indent();
        let _ = writeln!(self.out, "</{name}>");
    }

    /// An element with text content.
    fn leaf(&mut self, name: &str, text: &str) {
        self.leaf_with(name, &[], text);
    }

    fn leaf_with(&mut self, name: &str, attrs: Attrs<'_>, text: &str) {
        self.start(name, attrs);
        let _ = writeln!(self.out, ">{}</{name}>", esc(text));
    }

    /// An element holding a `property:Value` (length, angle, logic properties).
    fn value(&mut self, name: &str, xsi: &str, v: &str) {
        self.open(name, &[("xsi:type", xsi)]);
        self.leaf("property:Value", v);
        self.close(name);
    }

    fn id(&mut self, prefix: &'static str) -> String {
        let n = self.ids.entry(prefix).or_insert(0);
        *n += 1;
        format!("{prefix}{n}")
    }

    /// A body element `foundation:<name>` of type `<xsi>` with a fresh id; returns the id.
    fn body_open(&mut self, name: &str, xsi: &str, prefix: &'static str) -> String {
        self.body_open_with(name, xsi, prefix, &[])
    }

    fn body_open_with(&mut self, name: &str, xsi: &str, prefix: &'static str, extra: Attrs<'_>) -> String {
        let id = self.id(prefix);
        let mut attrs: Vec<(&str, &str)> = vec![("xsi:type", xsi), ("id", &id)];
        attrs.extend_from_slice(extra);
        self.open(&format!("foundation:{name}"), &attrs);
        id
    }

    fn name(&mut self, el: &str, object: &str) {
        self.open(el, &[("xsi:type", "foundation:EDMDName")]);
        self.leaf("foundation:SystemScope", SCOPE);
        self.leaf("foundation:ObjectName", object);
        self.close(el);
    }

    fn identifier(&mut self, number: &str) {
        self.open("pdm:Identifier", &[("xsi:type", "foundation:EDMDIdentifier")]);
        self.leaf("foundation:SystemScope", SCOPE);
        self.leaf("foundation:Number", number);
        self.leaf("foundation:Version", "1");
        self.leaf("foundation:Revision", "0");
        self.leaf("foundation:Sequence", "0");
        self.close("pdm:Identifier");
    }

    fn user_property(&mut self, key: &str, value: &str, unit: Option<&str>) {
        self.open("foundation:UserProperty", &[("xsi:type", "property:EDMDUserSimpleProperty")]);
        self.name("property:Key", key);
        self.leaf("property:Value", value);
        if let Some(u) = unit {
            self.leaf("property:Unit", u);
        }
        self.close("foundation:UserProperty");
    }

    fn baseline(&mut self) {
        self.value("pdm:BaseLine", "property:EDMDLogicProperty", "true");
    }

    /// A Cartesian point (Z = 0); returns its id.
    fn point(&mut self, x: i64, y: i64) -> String {
        let id = self.body_open("CartesianPoint", "d2:EDMDCartesianPoint", "PT");
        self.value("d2:X", "property:EDMDLengthProperty", &mm(x));
        self.value("d2:Y", "property:EDMDLengthProperty", &mm(y));
        self.close("foundation:CartesianPoint");
        id
    }

    fn polyline(&mut self, points: &[String]) -> String {
        let id = self.body_open("PolyLine", "d2:EDMDPolyLine", "PL");
        for p in points {
            self.leaf("d2:Point", p);
        }
        self.close("foundation:PolyLine");
        id
    }

    fn circle(&mut self, center: (i64, i64), diameter: i64) -> String {
        let c = self.point(center.0, center.1);
        let id = self.body_open("CircleCenter", "d2:EDMDCircleCenter", "CC");
        self.leaf("d2:Center", &c);
        self.value("d2:Diameter", "property:EDMDLengthProperty", &mm(diameter));
        self.close("foundation:CircleCenter");
        id
    }

    /// A closed loop as a curve: a circle, a closed polyline, or a composite curve of polylines
    /// and arcs. Returns the curve id.
    fn closed_curve(&mut self, lp: &Loop) -> String {
        if let [Edge::Arc { to, center, .. }] = lp.edges.as_slice()
            && *to == lp.start
        {
            let d = (2.0 * radius(lp.start, *center)).round() as i64;
            return self.circle((center.0.round() as i64, center.1.round() as i64), d);
        }
        let lp = lp.split_circles();
        let n = lp.edges.len();
        let pts: Vec<String> = (0..n).map(|i| self.point(lp.vertex(i).x.0, lp.vertex(i).y.0)).collect();
        let at = |i: usize| pts[i % n].clone();
        let mut curves = Vec::new();
        let mut run: Vec<String> = Vec::new();
        for (i, e) in lp.edges.iter().enumerate() {
            match *e {
                Edge::Line { .. } => {
                    if run.is_empty() {
                        run.push(at(i));
                    }
                    run.push(at(i + 1));
                }
                Edge::Arc { to, center, ccw } => {
                    if !run.is_empty() {
                        curves.push(self.polyline(&std::mem::take(&mut run)));
                    }
                    let angle = sweep_deg(lp.vertex(i), to, center, ccw);
                    let id = self.body_open("Arc", "d2:EDMDArc", "ARC");
                    self.leaf("d2:StartPoint", &at(i));
                    self.leaf("d2:EndPoint", &at(i + 1));
                    self.value("d2:IncludeAngle", "property:EDMDAngleProperty", &real(angle));
                    self.close("foundation:Arc");
                    curves.push(id);
                }
            }
        }
        if !run.is_empty() {
            curves.push(self.polyline(&run));
        }
        if curves.len() == 1 {
            return curves.pop().unwrap_or_default();
        }
        let id = self.body_open("CompositeCurve", "d2:EDMDCompositeCurve", "CMP");
        for c in &curves {
            self.leaf("d2:Curve", c);
        }
        self.close("foundation:CompositeCurve");
        id
    }

    /// A curve set (the curve extruded from `lower` to `upper`, either unbounded when `None`)
    /// and its shape element; returns the shape element id.
    fn shape(&mut self, curve: &str, lower: Option<i64>, upper: Option<i64>, kind: &str, inverted: bool) -> String {
        let cs = self.body_open("CurveSet2d", "d2:EDMDCurveSet2d", "CS");
        self.leaf("pdm:ShapeDescriptionType", "GeometricModel");
        if let Some(l) = lower {
            self.value("d2:LowerBound", "property:EDMDLengthProperty", &mm(l));
        }
        if let Some(u) = upper {
            self.value("d2:UpperBound", "property:EDMDLengthProperty", &mm(u));
        }
        self.leaf("d2:DetailedGeometricModelElement", curve);
        self.close("foundation:CurveSet2d");
        let id = self.body_open("ShapeElement", "pdm:EDMDShapeElement", "SE");
        self.leaf("pdm:ShapeElementType", kind);
        self.leaf("pdm:Inverted", if inverted { "true" } else { "false" });
        self.leaf("pdm:DefiningShape", &cs);
        self.close("foundation:ShapeElement");
        id
    }

    /// The `single` item: the feature's definition. Returns its id.
    fn single(
        &mut self,
        name: &str,
        description: &str,
        number: &str,
        props: &[(&str, String)],
        package: Option<&str>,
        shape: &str,
    ) -> String {
        let id = self.body_open("Item", "pdm:EDMDItem", "ITEM");
        self.leaf("foundation:Name", name);
        self.leaf("foundation:Description", description);
        for (k, v) in props {
            self.user_property(k, v, None);
        }
        self.leaf("pdm:ItemType", "single");
        self.identifier(number);
        if let Some(pk) = package {
            self.name("pdm:PackageName", pk);
        }
        self.leaf("pdm:Shape", shape);
        self.baseline();
        self.close("foundation:Item");
        id
    }
}

/// An `assembly` item: one occurrence of a feature.
struct Occurrence<'a> {
    geometry: &'a str,
    name: &'a str,
    description: &'a str,
    number: &'a str,
    props: Vec<(&'a str, String, Option<&'a str>)>,
    transform: Option<Transform>,
    item: &'a str,
}

/// A transformation: 2D (rotation and translation in XY) or 3D (with Z).
struct Transform {
    xx: f64,
    xy: f64,
    yx: f64,
    yy: f64,
    zz: Option<f64>,
    t: (i64, i64, i64),
}

impl Xml {
    fn occurrence(&mut self, o: &Occurrence<'_>) {
        self.body_open_with("Item", "pdm:EDMDItem", "ITEM", &[("GeometryType", o.geometry)]);
        self.leaf("foundation:Name", o.name);
        self.leaf("foundation:Description", o.description);
        self.leaf("pdm:ItemType", "assembly");
        self.identifier(o.number);
        self.open("pdm:ItemInstance", &[("xsi:type", "pdm:EDMDItemInstance")]);
        self.leaf("foundation:Name", o.name);
        for (k, v, u) in &o.props {
            self.user_property(k, v, *u);
        }
        self.name("pdm:InstanceName", o.name);
        if let Some(t) = &o.transform {
            self.open("pdm:Transformation", &[("xsi:type", "pdm:EDMDTransformation")]);
            self.leaf("pdm:TransformationType", if t.zz.is_some() { "d3" } else { "d2" });
            self.leaf("pdm:xx", &real(t.xx));
            self.leaf("pdm:xy", &real(t.xy));
            if t.zz.is_some() {
                self.leaf("pdm:xz", "0");
            }
            self.leaf("pdm:yx", &real(t.yx));
            self.leaf("pdm:yy", &real(t.yy));
            if let Some(zz) = t.zz {
                self.leaf("pdm:yz", "0");
                self.leaf("pdm:zx", "0");
                self.leaf("pdm:zy", "0");
                self.leaf("pdm:zz", &real(zz));
            }
            self.value("pdm:tx", "property:EDMDLengthProperty", &mm(t.t.0));
            self.value("pdm:ty", "property:EDMDLengthProperty", &mm(t.t.1));
            if t.zz.is_some() {
                self.value("pdm:tz", "property:EDMDLengthProperty", &mm(t.t.2));
            }
            self.close("pdm:Transformation");
        }
        self.leaf("pdm:Item", o.item);
        self.close("pdm:ItemInstance");
        self.baseline();
        self.close("foundation:Item");
    }
}

/// A unitless real: 10 decimals, trailing zeros trimmed, no `-0`.
fn real(v: f64) -> String {
    let t = format!("{v:.10}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    if t == "-0" { "0".into() } else { t.to_string() }
}

/// The closed loop of a polygon.
fn polygon_loop(pts: &[Point]) -> Option<Loop> {
    let mut pts = pts.to_vec();
    pts.dedup();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    if pts.len() < 3 {
        return None;
    }
    let mut edges: Vec<Edge> = pts[1..].iter().map(|&to| Edge::Line { to }).collect();
    edges.push(Edge::Line { to: pts[0] });
    Some(Loop { start: pts[0], edges })
}

/// A hole as written: its name, kind and padstack.
struct IdxHole {
    number: String,
    name: String,
    at: Point,
    diameter: Nm,
    kind: HoleKind,
    plated: bool,
}

fn idx_holes(p: &Project, vias: bool) -> Vec<IdxHole> {
    let board_vias = &p.board().vias;
    let mut via_index = 0;
    let mut out = Vec::new();
    for h in fabout::holes(p) {
        let (number, name) = match (&h.pad, h.kind) {
            (_, HoleKind::Via) => {
                let id = board_vias.get(via_index).map_or(via_index as u64, |v| v.id.0);
                via_index += 1;
                if !vias {
                    continue;
                }
                (format!("VIA:{id}"), format!("via {id}"))
            }
            (Some((r, n)), _) if crate::board::holes::is_hole(p, r) || n.is_empty() => (format!("HOLE:{r}"), r.clone()),
            (Some((r, n)), _) => (format!("HOLE:{r}.{n}"), format!("{r}.{n}")),
            (None, _) => continue,
        };
        out.push(IdxHole { number, name, at: h.at, diameter: h.diameter, kind: h.kind, plated: h.plated });
    }
    out
}

/// Padstack of a hole: (geometry type, inter-stratum feature type, name).
fn padstack(h: &IdxHole) -> (&'static str, &'static str, String) {
    let d = mm(h.diameter.0);
    match (h.kind, h.plated) {
        (HoleKind::Via, _) => ("VIA", "Via", format!("VIA_{d}")),
        (_, true) => ("HOLE_PLATED", "PlatedCutout", format!("PTH_{d}")),
        (_, false) => ("HOLE_NON_PLATED", "Cutout", format!("NPTH_{d}")),
    }
}

/// The 3D placement of a body (see the module docs).
fn body_transform(b: &BodyBox, thickness: i64) -> Transform {
    let (sin, cos) = match b.rotation.quarter_turns() {
        Some(q) => [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)][q as usize],
        None => b.rotation.to_rad_f64().sin_cos(),
    };
    match b.side {
        BoardSide::Top => {
            Transform { xx: cos, xy: -sin, yx: sin, yy: cos, zz: Some(1.0), t: (b.at.x.0, b.at.y.0, thickness) }
        }
        // Rz(θ) · diag(-1, 1, -1).
        BoardSide::Bottom => {
            Transform { xx: -cos, xy: -sin, yx: -sin, yy: cos, zz: Some(-1.0), t: (b.at.x.0, b.at.y.0, 0) }
        }
    }
}

/// The IDX baseline (`SendInformation`) of the board. `None` without a board outline.
pub fn export(p: &Project, o: &Options) -> Option<IdxOut> {
    let (outer, cutouts) = board_profile(p)?;
    let name = p.manifest().name.clone();
    let thickness = p.board().stackup.thickness.0;
    let mut x = Xml { out: String::new(), depth: 0, ids: BTreeMap::new() };
    x.out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let ns = |m: &str| format!("{NAMESPACE}{m}");
    let (foundation, pdm, d2, property, computational) =
        (ns("foundation"), ns("pdm"), ns("geometry.d2"), ns("property"), ns("computational"));
    x.open(
        "foundation:EDMDDataSet",
        &[
            ("xmlns:foundation", &foundation),
            ("xmlns:pdm", &pdm),
            ("xmlns:d2", &d2),
            ("xmlns:property", &property),
            ("xmlns:computational", &computational),
            ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
        ],
    );
    x.open("foundation:Header", &[("xsi:type", "foundation:EDMDHeader")]);
    x.leaf("foundation:Description", &name);
    // Projects store no people or companies.
    x.leaf("foundation:CreatorName", "");
    x.leaf("foundation:CreatorCompany", "");
    x.leaf("foundation:CreatorSystem", "cadlab");
    x.leaf("foundation:PostProcessor", "cadlab");
    x.leaf("foundation:PostProcessorVersion", &o.version);
    x.leaf("foundation:System", SCOPE);
    x.leaf("foundation:CreationDateTime", TIME_STAMP);
    x.leaf("foundation:ModifiedDateTime", TIME_STAMP);
    x.leaf("foundation:GlobalUnitLength", "UNIT_MM");
    x.close("foundation:Header");

    x.open("foundation:Body", &[("xsi:type", "foundation:EDMDDataSetBody")]);
    x.open("foundation:System", &[("xsi:type", "foundation:EDMDSystem"), ("id", SCOPE)]);
    x.leaf("foundation:SystemType", "ECAD");
    x.leaf("foundation:GlobalName", "cadlab");
    x.leaf("foundation:Revision", &o.version);
    x.close("foundation:System");
    x.open("foundation:UnitLength", &[("xsi:type", "property:EDMDUnitLength"), ("id", "UNIT_MM")]);
    x.leaf("property:Fundamental", "mm");
    x.close("foundation:UnitLength");

    // Board: outline and cutouts through the whole thickness.
    let mut shapes = Vec::new();
    let c = x.closed_curve(&outer);
    shapes.push(x.shape(&c, Some(0), Some(thickness), "FeatureShapeElement", false));
    for cut in &cutouts {
        let c = x.closed_curve(cut);
        shapes.push(x.shape(&c, Some(0), Some(thickness), "FeatureShapeElement", true));
    }
    let stratum = x.body_open("Stratum", "pdm:EDMDStratum", "STRATUM");
    for s in &shapes {
        x.leaf("pdm:ShapeElement", s);
    }
    x.leaf("pdm:StratumType", "DesignLayerStratum");
    x.leaf("pdm:StratumSurfaceDesignation", "PrimarySurface");
    x.close("foundation:Stratum");
    let board = x.single(&name, "board geometry", "BOARD_GEOMETRY", &[], None, &stratum);
    x.occurrence(&Occurrence {
        geometry: "BOARD_OUTLINE",
        name: &name,
        description: "board",
        number: "BOARD",
        props: vec![("THICKNESS", mm(thickness), Some("UNIT_MM")), ("TYPE", "BOARDOUTLINE".into(), None)],
        transform: None,
        item: &board,
    });

    // Holes, padstacks shared by kind and diameter.
    let holes = idx_holes(p, o.vias);
    let mut stacks: BTreeMap<String, String> = BTreeMap::new();
    for h in &holes {
        let (geometry, feature, stack) = padstack(h);
        if !stacks.contains_key(&stack) {
            let c = x.circle((0, 0), h.diameter.0);
            let kind = if h.plated { "PartMountingFeature" } else { "FeatureShapeElement" };
            let se = x.shape(&c, Some(0), Some(thickness), kind, true);
            let isf = x.body_open("InterStratumFeature", "pdm:EDMDInterStratumFeature", "ISF");
            x.leaf("pdm:ShapeElement", &se);
            x.leaf("pdm:InterStratumFeatureType", feature);
            x.leaf("pdm:Stratum", &stratum);
            x.close("foundation:InterStratumFeature");
            let item = x.single(&stack, "padstack", &format!("PADSTACK:{stack}"), &[], Some(&stack), &isf);
            stacks.insert(stack.clone(), item);
        }
        x.occurrence(&Occurrence {
            geometry,
            name: &h.name,
            description: "drilled hole",
            number: &h.number,
            props: vec![("PADSTACK", stack.clone(), None)],
            transform: Some(Transform { xx: 1.0, xy: 0.0, yx: 0.0, yy: 1.0, zz: None, t: (h.at.x.0, h.at.y.0, 0) }),
            item: &stacks[&stack],
        });
    }

    // Keep-outs: one item per forbidden kind (and per side for footprints).
    let copper = p.board().stackup.copper_names();
    let (top, bottom) = (copper.first().cloned().unwrap_or_default(), copper.last().cloned().unwrap_or_default());
    let mut keepouts = 0;
    for k in &p.board().keepouts {
        let Some(lp) = polygon_loop(&k.outline) else { continue };
        let lp = lp.oriented(true);
        let layers = if k.layers.is_empty() { "all copper layers".to_string() } else { k.layers.join(", ") };
        let on = |l: &str| k.layers.is_empty() || k.layers.iter().any(|x| x == l);
        let mut sides = Vec::new();
        if on(&top) {
            sides.push(BoardSide::Top);
        }
        if copper.len() > 1 && on(&bottom) {
            sides.push(BoardSide::Bottom);
        }
        if sides.is_empty() {
            sides = vec![BoardSide::Top, BoardSide::Bottom];
        }
        let mut kinds: Vec<(&str, &str, &str, Option<BoardSide>)> = Vec::new();
        if k.no_tracks {
            kinds.push(("KEEPOUT_AREA_ROUTING", "Route", "ROUTING", None));
        }
        if k.no_vias {
            kinds.push(("KEEPOUT_AREA_VIA", "Via", "VIA", None));
        }
        if k.no_pours {
            kinds.push(("KEEPOUT_AREA_OTHER", "Plane", "PLANE", None));
        }
        if k.no_footprints {
            for s in &sides {
                kinds.push(("KEEPOUT_AREA_COMPONENT", "ComponentPlacement", "COMPONENT", Some(*s)));
            }
        }
        for (geometry, purpose, tag, side) in kinds {
            let (lower, upper) = match side {
                None => (Some(0), Some(thickness)),
                Some(BoardSide::Top) => (Some(thickness), None),
                Some(BoardSide::Bottom) => (None, Some(0)),
            };
            let c = x.closed_curve(&lp);
            let se = x.shape(&c, lower, upper, "FeatureShapeElement", false);
            let ko = x.body_open("KeepOut", "pdm:EDMDKeepOut", "KO");
            x.leaf("pdm:ShapeElement", &se);
            x.leaf("pdm:Purpose", purpose);
            x.close("foundation:KeepOut");
            let side_name = side.map(|s| if s == BoardSide::Top { "TOP" } else { "BOTTOM" });
            let number = match side_name {
                Some(s) => format!("KEEPOUT:{}:{tag}:{s}", k.name),
                None => format!("KEEPOUT:{}:{tag}", k.name),
            };
            let what = match tag {
                "ROUTING" => format!("no tracks on {layers}"),
                "VIA" => format!("no vias on {layers}"),
                "PLANE" => format!("no copper pours on {layers}"),
                _ => "no components".to_string(),
            };
            let item = x.single(&k.name, &what, &format!("{number}:GEOMETRY"), &[], None, &ko);
            let mut props = Vec::new();
            if let Some(s) = side_name {
                props.push(("SIDE", s.to_string(), None));
            }
            x.occurrence(&Occurrence {
                geometry,
                name: &k.name,
                description: &what,
                number: &number,
                props,
                transform: None,
                item: &item,
            });
            keepouts += 1;
        }
    }

    // Components: body rectangles extruded to their height, shared per footprint and part.
    let (bodies, no_body) = if o.components { bodies(p) } else { (Vec::new(), Vec::new()) };
    let mut packages: BTreeMap<(String, String), String> = BTreeMap::new();
    for b in &bodies {
        let key = (b.footprint.clone(), b.part_number.clone());
        if !packages.contains_key(&key) {
            let (hw, hl) = (b.width.0 / 2, b.length.0 / 2);
            let (w, l) = (b.width.0 - hw, b.length.0 - hl);
            let corners = [(-hw, -hl), (w, -hl), (w, l), (-hw, l)];
            let mut pts: Vec<String> = corners.iter().map(|&(px, py)| x.point(px, py)).collect();
            pts.push(pts[0].clone());
            let c = x.polyline(&pts);
            let se = x.shape(&c, Some(0), Some(b.height.0), "FeatureShapeElement", false);
            let ac = x.body_open("AssemblyComponent", "pdm:EDMDAssemblyComponent", "AC");
            x.leaf("pdm:ShapeElement", &se);
            x.leaf("pdm:AssemblyComponentType", "Physical");
            x.close("foundation:AssemblyComponent");
            let props = [("PARTNUM", b.part_number.clone()), ("HEIGHT", mm(b.height.0))];
            let number = format!("PACKAGE:{}:{}", b.footprint, b.part_number);
            let item = x.single(&b.footprint, "component body", &number, &props, Some(&b.footprint), &ac);
            packages.insert(key.clone(), item);
        }
        let side = if b.side == BoardSide::Top { "TOP" } else { "BOTTOM" };
        x.occurrence(&Occurrence {
            geometry: "COMPONENT",
            name: &b.refdes,
            description: &b.part_number,
            number: &format!("COMPONENT:{}", b.refdes),
            props: vec![("REFDES", b.refdes.clone(), None), ("SIDE", side.into(), None)],
            transform: Some(body_transform(b, thickness)),
            item: &packages[&key],
        });
    }
    x.close("foundation:Body");
    x.start("foundation:ProcessInstruction", &[("xsi:type", "computational:EDMDProcessInstructionSendInformation")]);
    x.out.push_str("/>\n");
    x.close("foundation:EDMDDataSet");
    Some(IdxOut { content: x.out, no_body, components: bodies.len(), holes: holes.len(), keepouts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Angle;

    fn pt(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    #[test]
    fn numbers_and_loops() {
        assert_eq!(real(-0.0), "0");
        assert_eq!(real(1.0), "1");
        assert_eq!(real(-0.12345678901234), "-0.123456789");
        assert!(polygon_loop(&[pt(0, 0), pt(1, 0)]).is_none());
        let lp = polygon_loop(&[pt(0, 0), pt(10, 0), pt(10, 10), pt(0, 0)]).unwrap();
        assert_eq!(lp.edges.len(), 3);

        let mut x = Xml { out: String::new(), depth: 0, ids: BTreeMap::new() };
        let c = x.closed_curve(&Loop::circle(pt(5_000_000, 5_000_000), Nm(2_000_000), true));
        assert_eq!(c, "CC1");
        assert!(x.out.contains("<property:Value>2</property:Value>"), "{}", x.out);
        let c = x.closed_curve(&lp);
        assert_eq!(c, "PL1", "a polygon is one closed polyline");
        assert_eq!(x.out.matches("<d2:Point>PT2</d2:Point>").count(), 2, "first point repeated");
    }

    #[test]
    fn transforms() {
        let b = BodyBox {
            refdes: "U1".into(),
            footprint: "f".into(),
            part_number: "p".into(),
            at: pt(1, 2),
            rotation: Angle::from_deg(90),
            side: BoardSide::Bottom,
            width: Nm(1),
            length: Nm(1),
            height: Nm(1),
        };
        let t = body_transform(&b, 1_600_000);
        // Local X (1, 0, 0) → mirrored to -X, rotated 90° → (0, -1).
        assert_eq!((t.xx, t.yx, t.zz, t.t.2), (-0.0, -1.0, Some(-1.0), 0));
        let top = body_transform(&BodyBox { side: BoardSide::Top, ..b }, 1_600_000);
        assert_eq!((top.xx, top.yx, top.t.2), (0.0, 1.0, 1_600_000));
    }
}
