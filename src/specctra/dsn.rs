//! The Specctra design file (`.dsn`) as data, with its writer and reader.
//!
//! Lengths are stored in nanometers. The writer gives coordinates in micrometers with as many
//! decimals as needed, so nothing is rounded; the resolution only tells the router how fine
//! its own grid must be. The reader accepts every dimension unit and both the `unit` and
//! `resolution` declarations, and ignores what it does not know.

use super::sexpr::{self, Sx, SyntaxError};
use super::{Scale, Unit, fmt_angle, parse_angle};
use crate::geom::Point;
use crate::model::board::BoardSide;
use crate::units::{Angle, Nm};

/// Layer name meaning "every signal layer" in shapes.
pub const ALL_SIGNAL: &str = "signal";
/// Layer name of the board boundary.
pub const PCB: &str = "pcb";

/// A routing layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    /// Name (`F.Cu`).
    pub name: String,
    /// Layer type (`signal`, `power`, `mixed`, `jumper`).
    pub kind: String,
}

/// A shape on a layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Circle of diameter `diameter` centered at `at`.
    Circle {
        /// Layer.
        layer: String,
        /// Diameter.
        diameter: Nm,
        /// Center.
        at: Point,
    },
    /// Axis-aligned rectangle between two corners.
    Rect {
        /// Layer.
        layer: String,
        /// Lower-left corner.
        a: Point,
        /// Upper-right corner.
        b: Point,
    },
    /// Closed polygon (first vertex not repeated) drawn with an aperture.
    Polygon {
        /// Layer.
        layer: String,
        /// Aperture width (0: sharp).
        width: Nm,
        /// Vertices.
        points: Vec<Point>,
    },
    /// Open path drawn with a round aperture.
    Path {
        /// Layer.
        layer: String,
        /// Aperture width.
        width: Nm,
        /// Vertices.
        points: Vec<Point>,
    },
}

impl Shape {
    /// Layer of the shape.
    pub fn layer(&self) -> &str {
        match self {
            Shape::Circle { layer, .. }
            | Shape::Rect { layer, .. }
            | Shape::Polygon { layer, .. }
            | Shape::Path { layer, .. } => layer,
        }
    }
}

/// What a keep-out forbids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeepoutKind {
    /// Wires and vias (`keepout`).
    All,
    /// Vias only (`via_keepout`).
    Via,
    /// Wires only (`wire_keepout`).
    Wire,
}

impl KeepoutKind {
    fn keyword(self) -> &'static str {
        match self {
            KeepoutKind::All => "keepout",
            KeepoutKind::Via => "via_keepout",
            KeepoutKind::Wire => "wire_keepout",
        }
    }
}

/// A keep-out area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keepout {
    /// What it forbids.
    pub kind: KeepoutKind,
    /// Name (may be empty).
    pub name: String,
    /// Area.
    pub shape: Shape,
}

/// Width and clearance rules.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rule {
    /// Wire width.
    pub width: Option<Nm>,
    /// Clearance.
    pub clearance: Option<Nm>,
}

/// A padstack: copper shapes per layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Padstack {
    /// Name.
    pub name: String,
    /// Shapes, one per layer.
    pub shapes: Vec<Shape>,
    /// Whether vias may be attached (placed on it).
    pub attach: bool,
}

/// A pin of an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImagePin {
    /// Padstack.
    pub padstack: String,
    /// Pin rotation.
    pub rotation: Angle,
    /// Pin ID, unique in the image.
    pub id: String,
    /// Position in the image.
    pub at: Point,
}

/// A component image (footprint).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Name.
    pub name: String,
    /// Outline drawings.
    pub outlines: Vec<Shape>,
    /// Pins.
    pub pins: Vec<ImagePin>,
    /// Keep-outs (non-plated holes).
    pub keepouts: Vec<Keepout>,
}

/// A placed component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// Image.
    pub image: String,
    /// Reference designator.
    pub refdes: String,
    /// Image origin on the board.
    pub at: Point,
    /// Side.
    pub side: BoardSide,
    /// Rotation, counter-clockwise as seen from the top.
    pub rotation: Angle,
    /// Locked in position.
    pub locked: bool,
    /// Part number.
    pub part: Option<String>,
}

/// A net and its pins (`U1-3`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Net {
    /// Name.
    pub name: String,
    /// Pins as `<component>-<pin>`.
    pub pins: Vec<String>,
}

/// A net class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Class {
    /// Name.
    pub name: String,
    /// Nets.
    pub nets: Vec<String>,
    /// Via padstack to use.
    pub via: Option<String>,
    /// Rules.
    pub rule: Rule,
}

/// A wire (routed path).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    /// Layer.
    pub layer: String,
    /// Width.
    pub width: Nm,
    /// Vertices.
    pub points: Vec<Point>,
    /// Net.
    pub net: Option<String>,
    /// Protected: the router must not change it.
    pub protect: bool,
}

/// A via in the wiring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireVia {
    /// Padstack.
    pub padstack: String,
    /// Position.
    pub at: Point,
    /// Net.
    pub net: Option<String>,
    /// Protected.
    pub protect: bool,
}

/// A Specctra design.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dsn {
    /// Design name.
    pub name: String,
    /// Resolution: unit and steps per unit.
    pub resolution: (Unit, u32),
    /// Routing layers, top to bottom.
    pub layers: Vec<Layer>,
    /// Board boundary (closed, first vertex not repeated).
    pub boundary: Vec<Point>,
    /// Keep-outs (cutouts, keep-out areas).
    pub keepouts: Vec<Keepout>,
    /// Via padstacks the router may use.
    pub vias: Vec<String>,
    /// Default rules.
    pub rule: Rule,
    /// Component placement.
    pub places: Vec<Place>,
    /// Images.
    pub images: Vec<Image>,
    /// Padstacks.
    pub padstacks: Vec<Padstack>,
    /// Nets.
    pub nets: Vec<Net>,
    /// Net classes.
    pub classes: Vec<Class>,
    /// Existing wires.
    pub wires: Vec<Wire>,
    /// Existing vias.
    pub wire_vias: Vec<WireVia>,
}

/// Coordinates written: micrometers.
const OUT: Scale = Scale { unit: Unit::Um, divisor: 1 };

fn n(v: Nm) -> Sx {
    Sx::atom(OUT.fmt(v))
}

fn coords(points: &[Point]) -> impl Iterator<Item = Sx> + '_ {
    points.iter().flat_map(|p| [n(p.x), n(p.y)])
}

fn shape_sx(s: &Shape) -> Sx {
    match s {
        Shape::Circle { layer, diameter, at } => {
            let mut v = vec![Sx::string(layer), n(*diameter)];
            if *at != Point::ORIGIN {
                v.extend([n(at.x), n(at.y)]);
            }
            Sx::list("circle", v)
        }
        Shape::Rect { layer, a, b } => Sx::list("rect", [Sx::string(layer), n(a.x), n(a.y), n(b.x), n(b.y)]),
        Shape::Polygon { layer, width, points } => {
            Sx::list("polygon", [Sx::string(layer), n(*width)].into_iter().chain(coords(points)))
        }
        Shape::Path { layer, width, points } => {
            Sx::list("path", [Sx::string(layer), n(*width)].into_iter().chain(coords(points)))
        }
    }
}

fn keepout_sx(k: &Keepout) -> Sx {
    Sx::list(k.kind.keyword(), [Sx::string(&k.name), shape_sx(&k.shape)])
}

fn rule_sx(r: &Rule) -> Sx {
    let mut v = Vec::new();
    if let Some(w) = r.width {
        v.push(Sx::list("width", [n(w)]));
    }
    if let Some(c) = r.clearance {
        v.push(Sx::list("clearance", [n(c)]));
    }
    Sx::list("rule", v)
}

fn side_word(s: BoardSide) -> &'static str {
    match s {
        BoardSide::Top => "front",
        BoardSide::Bottom => "back",
    }
}

impl Dsn {
    /// The design as an S-expression tree.
    pub fn to_sx(&self) -> Sx {
        let mut pcb = vec![Sx::string(&self.name)];
        pcb.push(Sx::list(
            "parser",
            [
                Sx::list("string_quote", [Sx::atom("\"")]),
                Sx::list("space_in_quoted_tokens", [Sx::atom("on")]),
                Sx::list("host_cad", [Sx::string("cadlab")]),
            ],
        ));
        pcb.push(Sx::list(
            "resolution",
            [Sx::atom(self.resolution.0.keyword()), Sx::atom(self.resolution.1.to_string())],
        ));
        pcb.push(Sx::list("unit", [Sx::atom(OUT.unit.keyword())]));

        let mut st = Vec::new();
        for (i, l) in self.layers.iter().enumerate() {
            st.push(Sx::list(
                "layer",
                [
                    Sx::string(&l.name),
                    Sx::list("type", [Sx::atom(&l.kind)]),
                    Sx::list("property", [Sx::list("index", [Sx::atom(i.to_string())])]),
                ],
            ));
        }
        if !self.boundary.is_empty() {
            let mut closed = self.boundary.clone();
            closed.push(self.boundary[0]);
            st.push(Sx::list(
                "boundary",
                [shape_sx(&Shape::Path { layer: PCB.into(), width: Nm::ZERO, points: closed })],
            ));
        }
        st.extend(self.keepouts.iter().map(keepout_sx));
        if !self.vias.is_empty() {
            st.push(Sx::list("via", self.vias.iter().map(Sx::string)));
        }
        st.push(rule_sx(&self.rule));
        st.push(Sx::list("control", [Sx::list("via_at_smd", [Sx::atom("off")])]));
        pcb.push(Sx::list("structure", st));

        // Placement grouped by image, in image order.
        let mut placement = Vec::new();
        for img in &self.images {
            let places: Vec<&Place> = self.places.iter().filter(|p| p.image == img.name).collect();
            if places.is_empty() {
                continue;
            }
            let mut c = vec![Sx::string(&img.name)];
            for p in places {
                let mut v = vec![
                    Sx::string(&p.refdes),
                    n(p.at.x),
                    n(p.at.y),
                    Sx::atom(side_word(p.side)),
                    Sx::atom(fmt_angle(p.rotation)),
                ];
                if p.locked {
                    v.push(Sx::list("lock_type", [Sx::atom("position")]));
                }
                if let Some(pn) = &p.part {
                    v.push(Sx::list("PN", [Sx::string(pn)]));
                }
                c.push(Sx::list("place", v));
            }
            placement.push(Sx::list("component", c));
        }
        pcb.push(Sx::list("placement", placement));

        let mut lib = Vec::new();
        for img in &self.images {
            let mut v = vec![Sx::string(&img.name)];
            v.extend(img.outlines.iter().map(|s| Sx::list("outline", [shape_sx(s)])));
            for pin in &img.pins {
                let mut p = vec![Sx::string(&pin.padstack)];
                if pin.rotation != Angle::ZERO {
                    p.push(Sx::list("rotate", [Sx::atom(fmt_angle(pin.rotation))]));
                }
                p.extend([Sx::string(&pin.id), n(pin.at.x), n(pin.at.y)]);
                v.push(Sx::list("pin", p));
            }
            v.extend(img.keepouts.iter().map(keepout_sx));
            lib.push(Sx::list("image", v));
        }
        for ps in &self.padstacks {
            let mut v = vec![Sx::string(&ps.name)];
            v.extend(ps.shapes.iter().map(|s| Sx::list("shape", [shape_sx(s)])));
            v.push(Sx::list("attach", [Sx::atom(if ps.attach { "on" } else { "off" })]));
            lib.push(Sx::list("padstack", v));
        }
        pcb.push(Sx::list("library", lib));

        let mut net = Vec::new();
        for nt in &self.nets {
            net.push(Sx::list("net", [Sx::string(&nt.name), Sx::list("pins", nt.pins.iter().map(Sx::string))]));
        }
        for c in &self.classes {
            let mut v = vec![Sx::string(&c.name)];
            v.extend(c.nets.iter().map(Sx::string));
            if let Some(via) = &c.via {
                v.push(Sx::list("circuit", [Sx::list("use_via", [Sx::string(via)])]));
            }
            v.push(rule_sx(&c.rule));
            net.push(Sx::list("class", v));
        }
        pcb.push(Sx::list("network", net));

        let mut wiring = Vec::new();
        for w in &self.wires {
            let mut v =
                vec![shape_sx(&Shape::Path { layer: w.layer.clone(), width: w.width, points: w.points.clone() })];
            if let Some(net) = &w.net {
                v.push(Sx::list("net", [Sx::string(net)]));
            }
            if w.protect {
                v.push(Sx::list("type", [Sx::atom("protect")]));
            }
            wiring.push(Sx::list("wire", v));
        }
        for via in &self.wire_vias {
            let mut v = vec![Sx::string(&via.padstack), n(via.at.x), n(via.at.y)];
            if let Some(net) = &via.net {
                v.push(Sx::list("net", [Sx::string(net)]));
            }
            if via.protect {
                v.push(Sx::list("type", [Sx::atom("protect")]));
            }
            wiring.push(Sx::list("via", v));
        }
        pcb.push(Sx::list("wiring", wiring));
        Sx::list("pcb", pcb)
    }

    /// The design file text.
    pub fn write(&self) -> String {
        sexpr::write(&self.to_sx())
    }

    /// Reads a design file.
    pub fn parse(text: &str) -> Result<Dsn, ParseError> {
        let root = sexpr::parse(text).map_err(ParseError::Syntax)?;
        if root.head() != Some("pcb") {
            return Err(ParseError::Invalid("the file is not a Specctra design (no `pcb` list)".into()));
        }
        let name = root.args().first().and_then(Sx::text).unwrap_or_default().to_string();
        let resolution = match root.child("resolution") {
            Some(r) => read_resolution(r)?,
            None => (Unit::Inch, 1000),
        };
        let unit = match root.child("unit").and_then(|u| u.args().first()).and_then(Sx::text) {
            Some(u) => Unit::parse(u).ok_or_else(|| ParseError::Invalid(format!("unknown unit `{u}`")))?,
            None => resolution.0,
        };
        let sc = Scale { unit, divisor: 1 };
        let mut d = Dsn {
            name,
            resolution,
            layers: Vec::new(),
            boundary: Vec::new(),
            keepouts: Vec::new(),
            vias: Vec::new(),
            rule: Rule::default(),
            places: Vec::new(),
            images: Vec::new(),
            padstacks: Vec::new(),
            nets: Vec::new(),
            classes: Vec::new(),
            wires: Vec::new(),
            wire_vias: Vec::new(),
        };
        if let Some(st) = root.child("structure") {
            for l in st.children("layer") {
                let name = arg_text(l, 0)?;
                let kind = l.child("type").and_then(|t| t.args().first()).and_then(Sx::text).unwrap_or("signal");
                d.layers.push(Layer { name, kind: kind.to_string() });
            }
            let boundaries: Vec<Shape> = st
                .children("boundary")
                .filter_map(|b| b.args().iter().find(|a| matches!(a, Sx::List(_))))
                .map(|s| read_shape(s, sc))
                .collect::<Result<_, _>>()?;
            if let Some(b) = boundaries.iter().find(|s| s.layer() == PCB).or(boundaries.first()) {
                d.boundary = shape_points(b);
            }
            d.keepouts = read_keepouts(st, sc)?;
            for v in st.children("via") {
                d.vias.extend(v.args().iter().filter_map(Sx::text).map(str::to_string));
            }
            if let Some(r) = st.child("rule") {
                d.rule = read_rule(r, sc)?;
            }
        }
        if let Some(pl) = root.child("placement") {
            d.places = read_placement(pl, sc)?;
        }
        if let Some(lib) = root.child("library") {
            for img in lib.children("image") {
                let mut image = Image {
                    name: arg_text(img, 0)?,
                    outlines: Vec::new(),
                    pins: Vec::new(),
                    keepouts: read_keepouts(img, sc)?,
                };
                for o in img.children("outline") {
                    if let Some(s) = o.args().first() {
                        image.outlines.push(read_shape(s, sc)?);
                    }
                }
                for p in img.children("pin") {
                    let padstack = arg_text(p, 0)?;
                    let rotation = match p.child("rotate") {
                        Some(r) => angle(&arg_text(r, 0)?)?,
                        None => Angle::ZERO,
                    };
                    let atoms: Vec<&str> = p.args()[1..].iter().filter_map(Sx::text).collect();
                    let [id, x, y] = atoms[..] else {
                        return Err(ParseError::Invalid(format!("pin of image `{}` has no position", image.name)));
                    };
                    image.pins.push(ImagePin {
                        padstack,
                        rotation,
                        id: id.to_string(),
                        at: Point::new(len(sc, x)?, len(sc, y)?),
                    });
                }
                d.images.push(image);
            }
            d.padstacks = read_padstacks(lib, sc)?;
        }
        if let Some(net) = root.child("network") {
            for nt in net.children("net") {
                let pins = nt.child("pins").map(|p| p.args().iter().filter_map(Sx::text).map(str::to_string).collect());
                d.nets.push(Net { name: arg_text(nt, 0)?, pins: pins.unwrap_or_default() });
            }
            for c in net.children("class") {
                let atoms: Vec<String> = c.args().iter().filter_map(Sx::text).map(str::to_string).collect();
                let Some((name, nets)) = atoms.split_first() else {
                    return Err(ParseError::Invalid("class without a name".into()));
                };
                let via = c
                    .child("circuit")
                    .and_then(|ci| ci.child("use_via"))
                    .and_then(|u| u.args().first())
                    .and_then(Sx::text)
                    .map(str::to_string);
                let rule = match c.child("rule") {
                    Some(r) => read_rule(r, sc)?,
                    None => Rule::default(),
                };
                d.classes.push(Class {
                    name: name.clone(),
                    nets: nets.iter().filter(|n| !n.is_empty()).cloned().collect(),
                    via,
                    rule,
                });
            }
        }
        if let Some(w) = root.child("wiring") {
            let (wires, vias) = read_wiring(w.args(), sc, None)?;
            d.wires = wires;
            d.wire_vias = vias;
        }
        Ok(d)
    }

    /// Pin positions on the board by `<component>-<pin>`, from placement and images, using the
    /// Specctra placement transform ([`place_point`]).
    pub fn pin_positions(&self) -> Vec<(String, Point)> {
        let mut out = Vec::new();
        for p in &self.places {
            let Some(img) = self.images.iter().find(|i| i.name == p.image) else { continue };
            for pin in &img.pins {
                out.push((format!("{}-{}", p.refdes, pin.id), place_point(p, pin.at)));
            }
        }
        out
    }
}

/// Image coordinates → board coordinates for a placed component. Components on the back are
/// seen through the board: the image is mirrored across its Y axis (x → −x), then rotated
/// counter-clockwise by the placement rotation, as on the front. This matches cadlab's footprint
/// transform, so placement rotations are written unchanged.
pub fn place_point(p: &Place, q: Point) -> Point {
    let q = if p.side == BoardSide::Bottom { Point::new(-q.x, q.y) } else { q };
    q.rotated(p.rotation) + p.at
}

/// A problem reading a Specctra file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Not well-formed S-expressions.
    Syntax(SyntaxError),
    /// Well-formed but not a valid design or session.
    Invalid(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Syntax(e) => write!(f, "line {}: {}", e.line, e.message),
            ParseError::Invalid(m) => f.write_str(m),
        }
    }
}

pub(crate) fn arg_text(s: &Sx, i: usize) -> Result<String, ParseError> {
    s.args()
        .get(i)
        .and_then(Sx::text)
        .map(str::to_string)
        .ok_or_else(|| ParseError::Invalid(format!("`{}` is missing an argument", s.head().unwrap_or("list"))))
}

pub(crate) fn len(sc: Scale, s: &str) -> Result<Nm, ParseError> {
    sc.to_nm(s).ok_or_else(|| ParseError::Invalid(format!("`{s}` is not a number")))
}

fn angle(s: &str) -> Result<Angle, ParseError> {
    parse_angle(s).ok_or_else(|| ParseError::Invalid(format!("`{s}` is not an angle")))
}

pub(crate) fn read_resolution(r: &Sx) -> Result<(Unit, u32), ParseError> {
    let u = arg_text(r, 0)?;
    let unit = Unit::parse(&u).ok_or_else(|| ParseError::Invalid(format!("unknown unit `{u}`")))?;
    let steps = arg_text(r, 1)?;
    let steps: u32 = steps
        .parse()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| ParseError::Invalid(format!("resolution `{steps}` is not a positive integer")))?;
    Ok((unit, steps))
}

fn points(sc: Scale, atoms: &[&Sx]) -> Result<Vec<Point>, ParseError> {
    let v: Vec<Nm> = atoms.iter().filter_map(|a| a.text()).map(|t| len(sc, t)).collect::<Result<_, _>>()?;
    if !v.len().is_multiple_of(2) {
        return Err(ParseError::Invalid("odd number of coordinates".into()));
    }
    Ok(v.chunks(2).map(|c| Point::new(c[0], c[1])).collect())
}

/// Reads a shape (`circle`, `rect`, `polygon`, `path`).
pub(crate) fn read_shape(s: &Sx, sc: Scale) -> Result<Shape, ParseError> {
    let layer = arg_text(s, 0)?;
    let atoms: Vec<&Sx> = s.args()[1..].iter().filter(|a| a.text().is_some()).collect();
    match s.head() {
        Some("circle") => {
            let diameter = len(sc, atoms.first().and_then(|a| a.text()).unwrap_or("0"))?;
            let at = match atoms.get(1..3) {
                Some(xy) => points(sc, xy)?[0],
                None => Point::ORIGIN,
            };
            Ok(Shape::Circle { layer, diameter, at })
        }
        Some("rect") => {
            let p = points(sc, &atoms)?;
            let [a, b] = p[..] else { return Err(ParseError::Invalid("rect needs two corners".into())) };
            Ok(Shape::Rect {
                layer,
                a: Point::new(a.x.min(b.x), a.y.min(b.y)),
                b: Point::new(a.x.max(b.x), a.y.max(b.y)),
            })
        }
        Some(kind @ ("polygon" | "path")) => {
            let (w, rest) = atoms.split_first().ok_or_else(|| ParseError::Invalid(format!("{kind} has no width")))?;
            let width = len(sc, w.text().unwrap_or("0"))?;
            let mut pts = points(sc, rest)?;
            if kind == "polygon" {
                if pts.len() > 1 && pts.first() == pts.last() {
                    pts.pop();
                }
                Ok(Shape::Polygon { layer, width, points: pts })
            } else {
                Ok(Shape::Path { layer, width, points: pts })
            }
        }
        other => Err(ParseError::Invalid(format!("unsupported shape `{}`", other.unwrap_or("?")))),
    }
}

/// Vertices of an area shape (a closed path loses its repeated last vertex).
pub(crate) fn shape_points(s: &Shape) -> Vec<Point> {
    match s {
        Shape::Rect { a, b, .. } => vec![*a, Point::new(b.x, a.y), *b, Point::new(a.x, b.y)],
        Shape::Polygon { points, .. } => points.clone(),
        Shape::Path { points, .. } => {
            let mut v = points.clone();
            if v.len() > 1 && v.first() == v.last() {
                v.pop();
            }
            v
        }
        Shape::Circle { at, .. } => vec![*at],
    }
}

fn read_keepouts(parent: &Sx, sc: Scale) -> Result<Vec<Keepout>, ParseError> {
    let mut out = Vec::new();
    for k in parent.args() {
        let kind = match k.head() {
            Some("keepout") => KeepoutKind::All,
            Some("via_keepout") => KeepoutKind::Via,
            Some("wire_keepout") => KeepoutKind::Wire,
            _ => continue,
        };
        let name = k.args().first().and_then(Sx::text).unwrap_or_default().to_string();
        let Some(shape) = k.args().iter().find(|a| matches!(a, Sx::List(_)) && a.head() != Some("sequence_number"))
        else {
            continue;
        };
        out.push(Keepout { kind, name, shape: read_shape(shape, sc)? });
    }
    Ok(out)
}

fn read_rule(r: &Sx, sc: Scale) -> Result<Rule, ParseError> {
    let mut rule = Rule::default();
    if let Some(w) = r.child("width") {
        rule.width = Some(len(sc, &arg_text(w, 0)?)?);
    }
    // The plain clearance (without a `type` qualifier).
    if let Some(c) = r.children("clearance").find(|c| c.child("type").is_none()) {
        rule.clearance = Some(len(sc, &arg_text(c, 0)?)?);
    }
    Ok(rule)
}

/// Reads `(placement (component image (place ...)...)...)`; places without coordinates are
/// skipped.
pub(crate) fn read_placement(pl: &Sx, sc: Scale) -> Result<Vec<Place>, ParseError> {
    let mut out = Vec::new();
    for c in pl.children("component") {
        let image = arg_text(c, 0)?;
        for p in c.children("place") {
            let atoms: Vec<&str> = p.args().iter().filter_map(Sx::text).collect();
            let [refdes, x, y, side, rot, ..] = atoms[..] else { continue };
            out.push(Place {
                image: image.clone(),
                refdes: refdes.to_string(),
                at: Point::new(len(sc, x)?, len(sc, y)?),
                side: if side == "back" { BoardSide::Bottom } else { BoardSide::Top },
                rotation: angle(rot)?,
                locked: p.child("lock_type").is_some(),
                part: p.child("PN").and_then(|n| n.args().first()).and_then(Sx::text).map(str::to_string),
            });
        }
    }
    Ok(out)
}

/// Reads the padstacks of a `library` (or `library_out`).
pub(crate) fn read_padstacks(lib: &Sx, sc: Scale) -> Result<Vec<Padstack>, ParseError> {
    let mut out = Vec::new();
    for ps in lib.children("padstack") {
        let mut shapes = Vec::new();
        for s in ps.children("shape") {
            if let Some(inner) = s.args().first() {
                shapes.push(read_shape(inner, sc)?);
            }
        }
        let attach = ps.child("attach").and_then(|a| a.args().first()).and_then(Sx::text) != Some("off");
        out.push(Padstack { name: arg_text(ps, 0)?, shapes, attach });
    }
    Ok(out)
}

/// Reads `wire` and `via` items; `net` is the enclosing net in a session's `network_out`.
/// Wires with shapes other than paths (polygons, arcs) are skipped.
pub(crate) fn read_wiring(items: &[Sx], sc: Scale, net: Option<&str>) -> Result<(Vec<Wire>, Vec<WireVia>), ParseError> {
    let (mut wires, mut vias) = (Vec::new(), Vec::new());
    let net_of = |x: &Sx| {
        x.child("net").and_then(|n| n.args().first()).and_then(Sx::text).map(str::to_string).or(net.map(str::to_string))
    };
    let protect_of = |x: &Sx| {
        x.child("type").and_then(|t| t.args().first()).and_then(Sx::text).is_some_and(|t| t == "protect" || t == "fix")
    };
    for x in items {
        match x.head() {
            Some("wire") => {
                let Some(path) = x.child("path") else { continue };
                let Shape::Path { layer, width, points } = read_shape(path, sc)? else { unreachable!() };
                wires.push(Wire { layer, width, points, net: net_of(x), protect: protect_of(x) });
            }
            Some("via") => {
                let atoms: Vec<&str> = x.args().iter().filter_map(Sx::text).collect();
                let [padstack, xs, ys, ..] = atoms[..] else {
                    return Err(ParseError::Invalid("via without a position".into()));
                };
                vias.push(WireVia {
                    padstack: padstack.to_string(),
                    at: Point::new(len(sc, xs)?, len(sc, ys)?),
                    net: net_of(x),
                    protect: protect_of(x),
                });
            }
            _ => {}
        }
    }
    Ok((wires, vias))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A design in the style of other writers: mil units, a rect boundary, a rotated pin,
    /// qualified clearances, string quote declared, unknown sections.
    #[test]
    fn reads_other_writers() {
        let text = r#"(pcb "other board"
  (parser (string_quote ") (space_in_quoted_tokens on) (host_cad "X") (host_version "1"))
  (resolution mil 10)
  (unit mil)
  (structure
    (layer Top (type signal) (property (index 0)))
    (layer Bottom (type signal))
    (boundary (rect pcb 0 0 1000 500))
    (via "Via[0-1]_24:12_mil")
    (rule (width 10) (clearance 8) (clearance 8 (type smd_smd)))
    (keepout "" (circle signal 100 500 250))
    (autoroute_settings (fanout off))
  )
  (placement
    (component "R_0603"
      (place R1 100 100 back 270 (PN "10k"))
      (place R2)
    )
  )
  (library
    (image "R_0603"
      (pin Rect[T]Pad_40x40_mil (rotate 90) 1 -30 0)
      (pin Rect[T]Pad_40x40_mil 2 30 0)
    )
    (padstack Rect[T]Pad_40x40_mil (shape (rect Top -20 -20 20 20)) (attach off))
  )
  (network
    (net "N 1" (pins R1-1 R1-2))
    (class kicad_default "" "N 1" (circuit (use_via "Via[0-1]_24:12_mil")) (rule (width 10) (clearance 8)))
  )
  (wiring
    (wire (path Top 10 100 100 200 100) (net "N 1") (type route))
    (via "Via[0-1]_24:12_mil" 200 100 (net "N 1") (type protect))
  )
)"#;
        let d = Dsn::parse(text).unwrap();
        let mil = |v: i64| Nm(v * 25_400);
        assert_eq!(d.name, "other board");
        assert_eq!(d.resolution, (Unit::Mil, 10));
        assert_eq!(d.layers.len(), 2);
        assert_eq!(
            d.boundary,
            vec![
                Point::ORIGIN,
                Point::new(mil(1000), Nm(0)),
                Point::new(mil(1000), mil(500)),
                Point::new(Nm(0), mil(500))
            ]
        );
        assert_eq!(d.rule, Rule { width: Some(mil(10)), clearance: Some(mil(8)) });
        assert_eq!(d.keepouts.len(), 1);
        assert_eq!(d.places.len(), 1, "a place without coordinates is skipped");
        let r1 = &d.places[0];
        assert_eq!((r1.side, r1.rotation, r1.part.as_deref()), (BoardSide::Bottom, Angle(270_000), Some("10k")));
        assert_eq!(d.images[0].pins[0].rotation, Angle::DEG_90);
        assert_eq!(d.images[0].pins[1].at, Point::new(mil(30), Nm(0)));
        assert_eq!(d.classes[0].nets, ["N 1"]);
        assert_eq!(d.classes[0].via.as_deref(), Some("Via[0-1]_24:12_mil"));
        assert_eq!(d.wires[0].net.as_deref(), Some("N 1"));
        assert!(!d.wires[0].protect);
        assert!(d.wire_vias[0].protect);
        // Back side, 270°: pin 2 at image (30, 0) mirrors to (-30, 0), then turns to (0, 30).
        assert_eq!(d.pin_positions()[1], ("R1-2".to_string(), Point::new(mil(100), mil(130))));
        assert!(matches!(Dsn::parse("(session x)"), Err(ParseError::Invalid(_))));
        assert!(matches!(Dsn::parse("(pcb x (unit furlong))"), Err(ParseError::Invalid(_))));
    }
}
