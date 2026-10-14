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

use super::xml::{self, Element};
use super::{BodyBox, Edge, Loop, Options, board_profile, bodies, keepout_sides, polygon_loop, radius, sweep_deg};
use crate::fabout::gerber::mm;
use crate::fabout::ipc2581::esc;
use crate::fabout::{self, HoleKind};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::BoardSide;
use crate::units::{Angle, Nm};

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
    /// Items are marked as baseline items (`SendInformation`), not in change messages.
    baseline: bool,
    /// Sequence number written in the next identifiers.
    seq: u64,
}

type Attrs<'a> = &'a [(&'a str, &'a str)];

impl Xml {
    fn new(baseline: bool) -> Xml {
        Xml { out: String::new(), depth: 0, ids: BTreeMap::new(), baseline, seq: 0 }
    }

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
        self.leaf("foundation:Sequence", &self.seq.to_string());
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
        if !self.baseline {
            return;
        }
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

/// Starts a data set: the XML declaration, the root, the header, and the body with the system
/// and the length unit. With an `actor`, the administration namespace and an `EDMDPerson`
/// (id `ACTOR`) for the change process instruction.
fn open_dataset(x: &mut Xml, name: &str, version: &str, actor: Option<&str>) {
    x.out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let ns = |m: &str| format!("{NAMESPACE}{m}");
    let (foundation, pdm, d2, property, computational, administration) =
        (ns("foundation"), ns("pdm"), ns("geometry.d2"), ns("property"), ns("computational"), ns("administration"));
    let mut attrs: Vec<(&str, &str)> = vec![
        ("xmlns:foundation", &foundation),
        ("xmlns:pdm", &pdm),
        ("xmlns:d2", &d2),
        ("xmlns:property", &property),
        ("xmlns:computational", &computational),
        ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
    ];
    if actor.is_some() {
        attrs.insert(5, ("xmlns:administration", &administration));
    }
    x.open("foundation:EDMDDataSet", &attrs);
    x.open("foundation:Header", &[("xsi:type", "foundation:EDMDHeader")]);
    x.leaf("foundation:Description", name);
    // Projects store no people or companies.
    x.leaf("foundation:CreatorName", "");
    x.leaf("foundation:CreatorCompany", "");
    x.leaf("foundation:CreatorSystem", "cadlab");
    x.leaf("foundation:PostProcessor", "cadlab");
    x.leaf("foundation:PostProcessorVersion", version);
    x.leaf("foundation:System", SCOPE);
    x.leaf("foundation:CreationDateTime", TIME_STAMP);
    x.leaf("foundation:ModifiedDateTime", TIME_STAMP);
    x.leaf("foundation:GlobalUnitLength", "UNIT_MM");
    x.close("foundation:Header");

    x.open("foundation:Body", &[("xsi:type", "foundation:EDMDDataSetBody")]);
    x.open("foundation:System", &[("xsi:type", "foundation:EDMDSystem"), ("id", SCOPE)]);
    x.leaf("foundation:SystemType", "ECAD");
    x.leaf("foundation:GlobalName", "cadlab");
    x.leaf("foundation:Revision", version);
    x.close("foundation:System");
    x.open("foundation:UnitLength", &[("xsi:type", "property:EDMDUnitLength"), ("id", "UNIT_MM")]);
    x.leaf("property:Fundamental", "mm");
    x.close("foundation:UnitLength");
    if let Some(a) = actor {
        x.open("foundation:Person", &[("xsi:type", "administration:EDMDPerson"), ("id", "ACTOR")]);
        x.leaf("administration:SurName", a);
        x.close("foundation:Person");
    }
}

/// The IDX baseline (`SendInformation`) of the board. `None` without a board outline.
pub fn export(p: &Project, o: &Options) -> Option<IdxOut> {
    let (outer, cutouts) = board_profile(p)?;
    let name = p.manifest().name.clone();
    let thickness = p.board().stackup.thickness.0;
    let mut x = Xml::new(true);
    open_dataset(&mut x, &name, &o.version, None);

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
    let mut keepouts = 0;
    for k in &p.board().keepouts {
        let Some(lp) = polygon_loop(&k.outline) else { continue };
        let lp = lp.oriented(true);
        let layers = if k.layers.is_empty() { "all copper layers".to_string() } else { k.layers.join(", ") };
        let sides = keepout_sides(p, &k.layers);
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
            let item = package_item(&mut x, b);
            packages.insert(key.clone(), item);
        }
        component(&mut x, b, &packages[&key], thickness);
    }
    x.close("foundation:Body");
    x.start("foundation:ProcessInstruction", &[("xsi:type", "computational:EDMDProcessInstructionSendInformation")]);
    x.out.push_str("/>\n");
    x.close("foundation:EDMDDataSet");
    Some(IdxOut { content: x.out, no_body, components: bodies.len(), holes: holes.len(), keepouts })
}

/// The package item of a body: its rectangle extruded to the body height. Returns its id.
fn package_item(x: &mut Xml, b: &BodyBox) -> String {
    let (hw, hl) = (b.width.0 / 2, b.length.0 / 2);
    let (w, l) = (b.width.0 - hw, b.length.0 - hl);
    // Model bodies are centered on their bounding box, not on the footprint origin.
    let (ox, oy) = (b.offset.x.0, b.offset.y.0);
    let corners = [(-hw, -hl), (w, -hl), (w, l), (-hw, l)];
    let mut pts: Vec<String> = corners.iter().map(|&(px, py)| x.point(ox + px, oy + py)).collect();
    pts.push(pts[0].clone());
    let c = x.polyline(&pts);
    let se = x.shape(&c, Some(0), Some(b.height.0), "FeatureShapeElement", false);
    let ac = x.body_open("AssemblyComponent", "pdm:EDMDAssemblyComponent", "AC");
    x.leaf("pdm:ShapeElement", &se);
    x.leaf("pdm:AssemblyComponentType", "Physical");
    x.close("foundation:AssemblyComponent");
    let props = [("PARTNUM", b.part_number.clone()), ("HEIGHT", mm(b.height.0))];
    x.single(&b.footprint, "component body", &package_number(b), &props, Some(&b.footprint), &ac)
}

fn package_number(b: &BodyBox) -> String {
    format!("PACKAGE:{}:{}", b.footprint, b.part_number)
}

/// The item number of a component's placement.
fn component_number(refdes: &str) -> String {
    format!("COMPONENT:{refdes}")
}

/// A component occurrence placing the package item `item`.
fn component(x: &mut Xml, b: &BodyBox, item: &str, thickness: i64) {
    let side = if b.side == BoardSide::Top { "TOP" } else { "BOTTOM" };
    x.occurrence(&Occurrence {
        geometry: "COMPONENT",
        name: &b.refdes,
        description: &b.part_number,
        number: &component_number(&b.refdes),
        props: vec![("REFDES", b.refdes.clone(), None), ("SIDE", side.into(), None)],
        transform: Some(body_transform(b, thickness)),
        item,
    });
}

// ---------------------------------------------------------------------------------------------
// Change and response messages.

/// A component placement as an IDX message states it: the instance name (designator), the
/// package item number and the transformation `[xx, xy, yx, yy, zz, tx, ty, tz]` (lengths in
/// mm; `zz` is -1 for a part turned over onto the bottom side).
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    /// Designator (the instance name).
    pub refdes: String,
    /// Number of the package item placed (empty when the message does not say).
    pub package: String,
    /// Transformation.
    pub matrix: [f64; 8],
}

impl Placement {
    fn of(b: &BodyBox, thickness: i64) -> Placement {
        let t = body_transform(b, thickness);
        let m = |v: i64| v as f64 / 1e6;
        Placement {
            refdes: b.refdes.clone(),
            package: package_number(b),
            matrix: [t.xx, t.xy, t.yx, t.yy, t.zz.unwrap_or(1.0), m(t.t.0), m(t.t.1), m(t.t.2)],
        }
    }

    fn same_place(&self, o: &Placement) -> bool {
        self.matrix.iter().zip(&o.matrix).all(|(a, b)| (a - b).abs() < 1e-6)
    }

    /// The board placement this states: the footprint origin, the rotation (to 0.001°) and the
    /// side (bottom when turned over, `zz` < 0).
    pub fn board_placement(&self) -> (Point, Angle, BoardSide) {
        let [xx, _, yx, _, zz, tx, ty, _] = self.matrix;
        let (side, deg) = if zz < 0.0 {
            (BoardSide::Bottom, (-yx).atan2(-xx).to_degrees())
        } else {
            (BoardSide::Top, yx.atan2(xx).to_degrees())
        };
        let at = Point::new(Nm((tx * 1e6).round() as i64), Nm((ty * 1e6).round() as i64));
        (at, Angle((deg * 1000.0).round() as i32).normalized(), side)
    }
}

/// Millimeters per unit of a length unit name (`mm`, `cm`, `m`, `inch`, `mil`, `um`), 1 when
/// unknown.
fn unit_factor(name: &str) -> f64 {
    match name.trim().to_ascii_lowercase().as_str() {
        "cm" | "centimeter" | "centimetre" => 10.0,
        "m" | "meter" | "metre" => 1000.0,
        "in" | "inch" => 25.4,
        "mil" | "thou" => 0.0254,
        "um" | "micron" | "micrometer" | "micrometre" => 0.001,
        _ => 1.0,
    }
}

/// A parsed message: its root, elements by `id`, and the length factor to mm.
struct Message<'a> {
    root: &'a Element,
    ids: BTreeMap<&'a str, &'a Element>,
    factor: f64,
}

impl<'a> Message<'a> {
    fn new(root: &'a Element) -> Message<'a> {
        let mut all = Vec::new();
        root.walk(&mut all);
        let ids: BTreeMap<&str, &Element> = all.iter().filter_map(|e| e.attr("id").map(|i| (i, *e))).collect();
        let factor = root
            .kid("Header")
            .and_then(|h| h.value("GlobalUnitLength"))
            .and_then(|u| ids.get(u))
            .and_then(|u| u.value("Fundamental"))
            .map_or(1.0, unit_factor);
        Message { root, ids, factor }
    }

    /// Body items.
    fn items(&self) -> impl Iterator<Item = &'a Element> + 'a {
        self.root.kids("Body").flat_map(|b| b.kids("Item"))
    }

    /// The placement an assembly item states (an instance with a transformation).
    fn placement(&self, item: &Element) -> Option<Placement> {
        if item.value("ItemType") != Some("assembly") {
            return None;
        }
        let inst = item.kid("ItemInstance")?;
        let t = inst.kid("Transformation")?;
        let refdes = inst
            .kid("InstanceName")
            .and_then(|n| n.value("ObjectName"))
            .or_else(|| inst.value("Name"))
            .or_else(|| item.value("Name"))?
            .to_string();
        let num = |k: &str, default: f64| t.value(k).and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(default);
        let package = inst
            .value("Item")
            .and_then(|i| self.ids.get(i))
            .and_then(|i| identifier(i))
            .map(|(n, _)| n)
            .unwrap_or_default();
        let f = self.factor;
        let matrix = [
            num("xx", 1.0),
            num("xy", 0.0),
            num("yx", 0.0),
            num("yy", 1.0),
            num("zz", 1.0),
            num("tx", 0.0) * f,
            num("ty", 0.0) * f,
            num("tz", 0.0) * f,
        ];
        Some(Placement { refdes, package, matrix })
    }

    /// The body item with this identifier.
    fn item(&self, number: &str, sequence: u64) -> Option<&'a Element> {
        self.items().find(|i| identifier(i).is_some_and(|(n, s)| n == number && s == sequence))
    }

    /// The transactions of the `SendChanges` process instructions.
    fn transactions(&self) -> Vec<&'a Element> {
        self.root
            .kids("ProcessInstruction")
            .filter(|pi| pi.attr("xsi:type").is_some_and(|t| xml::local(t) == "EDMDProcessInstructionSendChanges"))
            .flat_map(|pi| pi.kids("Changes"))
            .collect()
    }
}

/// Number and sequence of an item's (first) identifier.
fn identifier(item: &Element) -> Option<(String, u64)> {
    ident(item.kid("Identifier")?)
}

/// Number and sequence of an `EDMDIdentifier` element (a missing sequence is 0).
fn ident(id: &Element) -> Option<(String, u64)> {
    let n = id.value("Number")?.to_string();
    Some((n, id.value("Sequence").and_then(|s| s.trim().parse().ok()).unwrap_or(0)))
}

/// Whether an item carries `Accept` false (a rejected change in a response).
fn rejected(item: &Element) -> bool {
    item.kids("Accept").any(|a| a.kid("Value").map_or(a.text.as_str(), |v| v.text.as_str()).trim() == "false")
}

/// The changes of an `EDMDTransaction` (or of a single change).
fn changes_of(t: &Element) -> Vec<&Element> {
    if t.kid("NewItem").is_some() { vec![t] } else { t.kids("Change").collect() }
}

fn data_set(text: &str) -> Result<Element, String> {
    let root = xml::parse(text)?;
    if root.local() != "EDMDDataSet" {
        return Err(format!("not an IDX data set (root element `{}`)", root.name));
    }
    Ok(root)
}

/// Component placements established by earlier messages (a baseline, then change and response
/// messages, read in order): per component item number, its states by sequence (`None`:
/// deleted). Items a response rejects are dropped again, so the previous state applies.
#[derive(Clone, Debug, Default)]
pub struct History {
    states: BTreeMap<String, BTreeMap<u64, Option<Placement>>>,
    /// Highest sequence seen per number (a new item version must not reuse one).
    max: BTreeMap<String, u64>,
}

impl History {
    /// Reads one message (baseline, change or response) into the history.
    pub fn read(&mut self, text: &str) -> Result<(), String> {
        let root = data_set(text)?;
        let m = Message::new(&root);
        for item in m.items() {
            let Some((number, seq)) = identifier(item) else { continue };
            if !number.starts_with("COMPONENT:") {
                continue;
            }
            self.seen(&number, seq);
            let versions = self.states.entry(number).or_default();
            if rejected(item) {
                versions.remove(&seq);
            } else if let Some(pl) = m.placement(item) {
                versions.insert(seq, Some(pl));
            }
        }
        for change in m.transactions().into_iter().flat_map(changes_of) {
            let Some((number, seq)) = change.kid("NewItem").and_then(ident) else { continue };
            if !number.starts_with("COMPONENT:") {
                continue;
            }
            self.seen(&number, seq);
            if change.kid("DeletedInstanceName").is_some() && m.item(&number, seq).is_none() {
                self.states.entry(number).or_default().insert(seq, None);
            }
        }
        Ok(())
    }

    fn seen(&mut self, number: &str, seq: u64) {
        let e = self.max.entry(number.to_string()).or_insert(seq);
        *e = (*e).max(seq);
    }

    /// The current state of an item: its sequence and placement (`None`: deleted).
    pub fn current(&self, number: &str) -> Option<(u64, Option<&Placement>)> {
        self.states.get(number)?.iter().next_back().map(|(s, p)| (*s, p.as_ref()))
    }

    /// The sequence of the next version of an item.
    fn next(&self, number: &str) -> u64 {
        self.max.get(number).map_or(0, |m| m + 1)
    }

    /// Whether no component placement is known.
    pub fn is_empty(&self) -> bool {
        self.states.values().all(|v| v.values().all(Option::is_none))
    }
}

/// What changed for one component (its designator).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComponentChange {
    /// Moved, rotated or flipped to the other side.
    Moved(String),
    /// Placed with another package (footprint or part number).
    Changed(String),
    /// New since the history.
    Added(String),
    /// Gone (removed, unplaced, made DNP or left without a body).
    Removed(String),
}

impl ComponentChange {
    /// The designator.
    pub fn refdes(&self) -> &str {
        match self {
            ComponentChange::Moved(r)
            | ComponentChange::Changed(r)
            | ComponentChange::Added(r)
            | ComponentChange::Removed(r) => r,
        }
    }

    fn describe(&self) -> String {
        match self {
            ComponentChange::Moved(r) => format!("{r} moved"),
            ComponentChange::Changed(r) => format!("{r} placed with another package"),
            ComponentChange::Added(r) => format!("{r} added"),
            ComponentChange::Removed(r) => format!("{r} removed"),
        }
    }
}

/// Result of a change export.
#[derive(Clone, Debug)]
pub struct IdxChanges {
    /// The `SendChanges` message, or `None` when nothing changed.
    pub content: Option<String>,
    /// The changes, one transaction each, in designator order.
    pub changes: Vec<ComponentChange>,
    /// Components without a package body (left out).
    pub no_body: Vec<String>,
}

/// A change to write: what, the body (none for a removal), the item number, the new sequence
/// and the predecessor's.
type Pending<'a> = (ComponentChange, Option<&'a BodyBox>, String, u64, Option<u64>);

/// An IDX change message (`SendChanges`) for the component placements that differ from
/// `history`: one transaction per component. A moved or changed component is the next version
/// (sequence + 1) of its assembly item with the new transformation; an added one a new item; a
/// removed one the next version of its item, not defined in the body, with its instance name
/// as `DeletedInstanceName`. The body holds the new item versions and the package items they
/// place; `actor` is the surname of the `EDMDPerson` the instruction refers to. `None` without
/// a board outline.
pub fn changes(p: &Project, o: &Options, history: &History, actor: &str) -> Option<IdxChanges> {
    board_profile(p)?;
    let thickness = p.board().stackup.thickness.0;
    let (bodies, no_body) = bodies(p);
    let mut list: Vec<Pending<'_>> = Vec::new();
    let mut current: BTreeMap<String, &BodyBox> = BTreeMap::new();
    for b in &bodies {
        let number = component_number(&b.refdes);
        current.insert(number.clone(), b);
        let now = Placement::of(b, thickness);
        let next = history.next(&number);
        match history.current(&number) {
            Some((s, Some(prev))) => {
                let kind = if !prev.same_place(&now) {
                    ComponentChange::Moved(b.refdes.clone())
                } else if !prev.package.is_empty() && prev.package != now.package {
                    ComponentChange::Changed(b.refdes.clone())
                } else {
                    continue;
                };
                list.push((kind, Some(b), number, next, Some(s)));
            }
            _ => list.push((ComponentChange::Added(b.refdes.clone()), Some(b), number, next, None)),
        }
    }
    for (number, versions) in &history.states {
        if current.contains_key(number) {
            continue;
        }
        if let Some((&s, Some(prev))) = versions.iter().next_back() {
            let next = history.next(number);
            list.push((ComponentChange::Removed(prev.refdes.clone()), None, number.clone(), next, Some(s)));
        }
    }
    list.sort_by(|a, b| crate::model::sections::natural_cmp(a.0.refdes(), b.0.refdes()));
    let changes: Vec<ComponentChange> = list.iter().map(|c| c.0.clone()).collect();
    if list.is_empty() {
        return Some(IdxChanges { content: None, changes, no_body });
    }

    let name = p.manifest().name.clone();
    let mut x = Xml::new(false);
    open_dataset(&mut x, &name, &o.version, Some(actor));
    let mut packages: BTreeMap<String, String> = BTreeMap::new();
    for (_, b, _, seq, _) in &list {
        let Some(b) = b else { continue };
        let key = package_number(b);
        if !packages.contains_key(&key) {
            x.seq = 0;
            let item = package_item(&mut x, b);
            packages.insert(key.clone(), item);
        }
        x.seq = *seq;
        component(&mut x, b, &packages[&key], thickness);
    }
    x.seq = 0;
    x.close("foundation:Body");
    x.open("foundation:ProcessInstruction", &[("xsi:type", "computational:EDMDProcessInstructionSendChanges")]);
    x.leaf("computational:Actor", "ACTOR");
    for (kind, _, number, seq, pred) in &list {
        x.open("computational:Changes", &[("xsi:type", "computational:EDMDTransaction")]);
        x.leaf("foundation:Description", &kind.describe());
        x.open("computational:Change", &[("xsi:type", "computational:EDMDChange")]);
        let ident = |x: &mut Xml, el: &str, s: u64| {
            x.open(el, &[("xsi:type", "foundation:EDMDIdentifier")]);
            x.leaf("foundation:SystemScope", SCOPE);
            x.leaf("foundation:Number", number);
            x.leaf("foundation:Version", "1");
            x.leaf("foundation:Revision", "0");
            x.leaf("foundation:Sequence", &s.to_string());
            x.close(el);
        };
        ident(&mut x, "computational:NewItem", *seq);
        if let Some(s) = pred {
            ident(&mut x, "computational:PredecessorItem", *s);
        }
        if let ComponentChange::Removed(r) = kind {
            x.name("computational:DeletedInstanceName", r);
        }
        x.close("computational:Change");
        x.close("computational:Changes");
    }
    x.close("foundation:ProcessInstruction");
    x.close("foundation:EDMDDataSet");
    Some(IdxChanges { content: Some(x.out), changes, no_body })
}

/// One proposed change of a received change message.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposed {
    /// Number of the new item version.
    pub number: String,
    /// Its sequence.
    pub sequence: u64,
    /// Sequence of the predecessor, if any.
    pub predecessor: Option<u64>,
    /// Instance names deleted.
    pub deleted: Vec<String>,
    /// The placement the new item states, when it is a placed assembly item in the body.
    pub placement: Option<Placement>,
    /// The new item is defined in the body.
    pub in_body: bool,
}

/// A transaction of a received change message, accepted or rejected as a whole.
#[derive(Clone, Debug, PartialEq)]
pub struct Transaction {
    /// Its description.
    pub description: String,
    /// Its changes.
    pub changes: Vec<Proposed>,
}

/// The transactions of a change message (`SendChanges`).
pub fn read_changes(text: &str) -> Result<Vec<Transaction>, String> {
    let root = data_set(text)?;
    let m = Message::new(&root);
    if !root
        .kids("ProcessInstruction")
        .any(|pi| pi.attr("xsi:type").is_some_and(|t| xml::local(t) == "EDMDProcessInstructionSendChanges"))
    {
        return Err("not a change message (no SendChanges process instruction)".into());
    }
    let mut out = Vec::new();
    for t in m.transactions() {
        let mut changes = Vec::new();
        for c in changes_of(t) {
            let Some((number, sequence)) = c.kid("NewItem").and_then(ident) else { continue };
            let item = m.item(&number, sequence);
            changes.push(Proposed {
                predecessor: c.kid("PredecessorItem").and_then(ident).map(|(_, s)| s),
                deleted: c
                    .kids("DeletedInstanceName")
                    .filter_map(|d| d.value("ObjectName"))
                    .map(String::from)
                    .collect(),
                placement: item.and_then(|i| m.placement(i)),
                in_body: item.is_some(),
                number,
                sequence,
            });
        }
        out.push(Transaction { description: t.value("Description").unwrap_or_default().to_string(), changes });
    }
    Ok(out)
}

/// The answer to one transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// Accepted (else rejected).
    pub accept: bool,
    /// Comment, written as the transaction's description.
    pub comment: Option<String>,
}

/// The response to a change message: the message itself with `Accept` on every body item a
/// change of the transaction names as its new item, and the comment as the transaction's
/// `Description` (the response file of the IDX implementation guidelines: same header, body
/// and `SendChanges` instruction). `answers` are by transaction, in [`read_changes`] order;
/// a missing answer rejects.
pub fn respond(text: &str, answers: &[Answer]) -> Result<String, String> {
    let root = data_set(text)?;
    let m = Message::new(&root);
    // A prefix for a namespace, declared on the element when the root does not bind one.
    let prefix = |ns: &str, fallback: &str| -> (String, String) {
        match root.prefix_of(&format!("{NAMESPACE}{ns}")) {
            Some(p) => (format!("{p}:"), String::new()),
            None => (format!("{fallback}:"), format!(" xmlns:{fallback}=\"{NAMESPACE}{ns}\"")),
        }
    };
    let (prop, prop_decl) = prefix("property", "cadlabprop");
    let (found, found_decl) = prefix("foundation", "cadlabfnd");
    // (from, to, replacement), applied from the end of the text.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for (i, t) in m.transactions().into_iter().enumerate() {
        let answer = answers.get(i).cloned().unwrap_or(Answer { accept: false, comment: None });
        let value = if answer.accept { "true" } else { "false" };
        for c in changes_of(t) {
            let Some((number, seq)) = c.kid("NewItem").and_then(ident) else { continue };
            let Some(item) = m.item(&number, seq) else { continue };
            let pdm = item.kid("ItemType").map(|e| e.prefix()).filter(|p| !p.is_empty());
            let pdm = pdm.map(|p| format!("{p}:")).unwrap_or_default();
            let accept = format!("<{pdm}Accept{prop_decl}><{prop}Value>{value}</{prop}Value></{pdm}Accept>");
            let existing: Vec<&Element> = item.kids("Accept").collect();
            if let Some(first) = existing.first() {
                edits.push((first.start, first.end, accept));
                for e in &existing[1..] {
                    edits.push((e.start, e.end, String::new()));
                }
            } else {
                // Schema order: after the instances, shape and transformation.
                let later = ["ClearanceState", "BaseLine", "ReferenceName", "AssembleToName", "PackagePin"];
                let next = item.children.iter().find(|k| later.contains(&k.local()));
                let at = next.map_or(item.content_end, |k| k.start);
                // Keep the file's indentation: on its own line, one level in before the end tag.
                let line = &text[text[..at].rfind('\n').map_or(0, |n| n + 1)..at];
                let edit = if line.chars().all(char::is_whitespace) {
                    let deeper = if next.is_none() { "  " } else { "" };
                    format!("{deeper}{accept}\n{line}")
                } else {
                    accept
                };
                edits.push((at, at, edit));
            }
        }
        if let Some(comment) = &answer.comment {
            let text = esc(comment);
            let element = format!("<{found}Description{found_decl}>{text}</{found}Description>");
            match t.kid("Description") {
                Some(d) if d.content_end > d.content_start || d.end > d.content_end => {
                    edits.push((d.content_start, d.content_end, text))
                }
                Some(d) => edits.push((d.start, d.end, element)),
                None => {
                    let at = t.kid("Name").map_or(t.content_start, |n| n.end);
                    edits.push((at, at, element));
                }
            }
        }
    }
    edits.sort_by_key(|e| std::cmp::Reverse((e.0, e.1)));
    let mut out = text.to_string();
    for (from, to, s) in edits {
        out.replace_range(from..to, &s);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let mut x = Xml::new(true);
        let c = x.closed_curve(&Loop::circle(pt(5_000_000, 5_000_000), Nm(2_000_000), true));
        assert_eq!(c, "CC1");
        assert!(x.out.contains("<property:Value>2</property:Value>"), "{}", x.out);
        let c = x.closed_curve(&lp);
        assert_eq!(c, "PL1", "a polygon is one closed polyline");
        assert_eq!(x.out.matches("<d2:Point>PT2</d2:Point>").count(), 2, "first point repeated");
    }

    #[test]
    fn foreign_change_messages() {
        // Other prefixes, inch units, a transaction with a Name but no Description, no
        // property namespace on the root.
        let msg = r#"<?xml version="1.0"?>
<f:EDMDDataSet xmlns:f="http://www.prostep.org/ecad-mcad/edmd/4.0/foundation" xmlns:p="http://www.prostep.org/ecad-mcad/edmd/4.0/pdm" xmlns:c="http://www.prostep.org/ecad-mcad/edmd/4.0/computational" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <f:Header><f:GlobalUnitLength>U</f:GlobalUnitLength></f:Header>
  <f:Body>
    <f:UnitLength id="U"><Fundamental>inch</Fundamental></f:UnitLength>
    <f:Item id="I1">
      <p:ItemType>assembly</p:ItemType>
      <p:Identifier><f:Number>COMPONENT:R1</f:Number><f:Sequence>3</f:Sequence></p:Identifier>
      <p:ItemInstance>
        <p:InstanceName><f:ObjectName>R1</f:ObjectName></p:InstanceName>
        <p:Transformation><p:xx>0</p:xx><p:xy>1</p:xy><p:yx>1</p:yx><p:yy>0</p:yy><p:zz>-1</p:zz>
          <p:tx><Value>1</Value></p:tx><p:ty><Value>0.5</Value></p:ty></p:Transformation>
      </p:ItemInstance>
      <p:BaseLine><Value>false</Value></p:BaseLine>
    </f:Item>
  </f:Body>
  <f:ProcessInstruction xsi:type="c:EDMDProcessInstructionSendChanges">
    <c:Actor>A</c:Actor>
    <c:Changes><f:Name>t</f:Name><c:Change><c:NewItem><f:Number>COMPONENT:R1</f:Number><f:Sequence>3</f:Sequence></c:NewItem></c:Change></c:Changes>
  </f:ProcessInstruction>
</f:EDMDDataSet>
"#;
        let t = read_changes(msg).unwrap();
        assert_eq!(t.len(), 1);
        let pl = t[0].changes[0].placement.clone().unwrap();
        let (at, rot, side) = pl.board_placement();
        // Bottom: xx = -cos, yx = -sin → 270°.
        assert_eq!((at, rot, side), (pt(25_400_000, 12_700_000), Angle::DEG_270, BoardSide::Bottom));
        let out = respond(msg, &[Answer { accept: true, comment: Some("ok <3".into()) }]).unwrap();
        let ns = "http://www.prostep.org/ecad-mcad/edmd/4.0/property";
        assert!(out.contains(&format!(
            "      <p:Accept xmlns:cadlabprop=\"{ns}\"><cadlabprop:Value>true</cadlabprop:Value></p:Accept>\n      <p:BaseLine>"
        )));
        assert!(out.contains("<f:Name>t</f:Name><f:Description>ok &lt;3</f:Description>"), "{out}");
        assert!(xml::parse(&out).is_ok());
        assert!(read_changes("<a/>").is_err());
        let mut h = History::default();
        h.read(msg).unwrap();
        assert_eq!(h.current("COMPONENT:R1").map(|c| c.0), Some(3));
        assert_eq!(h.next("COMPONENT:R1"), 4);
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
            offset: pt(0, 0),
            model: None,
        };
        let t = body_transform(&b, 1_600_000);
        // Local X (1, 0, 0) → mirrored to -X, rotated 90° → (0, -1).
        assert_eq!((t.xx, t.yx, t.zz, t.t.2), (-0.0, -1.0, Some(-1.0), 0));
        let top = body_transform(&BodyBox { side: BoardSide::Top, ..b }, 1_600_000);
        assert_eq!((top.xx, top.yx, top.t.2), (0.0, 1.0, 1_600_000));
    }
}
