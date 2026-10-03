//! KiCad schematic export (`.kicad_sch`).
//!
//! Written from KiCad's published file format documentation (dev-docs.kicad.org, "S-Expression
//! Format" and "Schematic File Format") and from files that `kicad-cli` reads and writes. No KiCad
//! code or library data is used (DECISIONS D7). The output targets file version `20231120`
//! (KiCad 8), which later versions read.
//!
//! What is written:
//! - one library symbol per used part, embedded in `lib_symbols` (`cadlab:<part id>`), drawn after
//!   cadlab's own symbols, with pin ends exactly where [`super::symbol::pin_ends`] puts them;
//! - one symbol instance per placed component;
//! - wires, local labels (net and wire labels), no-connect markers, junctions where needed;
//! - one power symbol per power/ground net (`cadlab_power:<net>`, a `power` symbol whose hidden
//!   `power_in` pin and value are the net name), placed at each power/ground label;
//! - a `PWR_FLAG` marker on nets that cadlab treats as supplied (marked driven, or ground) but that
//!   have no power output pin: KiCad requires a power output on every net with power inputs.
//!   It sits on one of the net's power symbols, drawn as a small diamond;
//! - a local label on wired nets that the layout left unnamed, so KiCad keeps cadlab's net names.
//! - block instance frames as dashed rectangles with their title as text.
//!
//! Coordinates: cadlab sheets are Y up from the bottom-left corner; KiCad sheets are Y down from
//! the top-left, so `y_kicad = height - y`. Library symbols are Y up in KiCad as in cadlab, so
//! symbol-local coordinates are written unchanged, and a placement rotated by `rot` quarter turns
//! counter-clockwise becomes angle `rot × 90`. Output is deterministic: UUIDs are hashes of the
//! project name and the object they identify.

use std::collections::{BTreeMap, BTreeSet};

use super::shapes::frame_title;
use super::symbol::{body_half, field_positions, pin_ends, symbol_of};
use super::{Dir, GRID, LabelKind, SheetLayout};
use crate::geom::Point;
use crate::model::Project;
use crate::model::circuit::PinRef;
use crate::model::part::{Part, PinKind, Side, Symbol, SymbolStyle};
use crate::render::{HAlign, VAlign};
use crate::symbolgen::{self, is_ground};
use crate::units::Nm;

/// Schematic file format version written (KiCad 8).
pub const FORMAT_VERSION: &str = "20231120";
/// Library nickname of the part symbols.
pub const PART_LIB: &str = "cadlab";
/// Library nickname of the power symbols.
pub const POWER_LIB: &str = "cadlab_power";

/// Export options.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// KiCad project name recorded in symbol instance data. KiCad matches it against the project
    /// (the file name without extension when opened on its own); defaults to the cadlab project
    /// name.
    pub project_name: Option<String>,
}

/// An exported schematic, with counts.
#[derive(Clone, Debug)]
pub struct KicadSchematic {
    /// File content.
    pub text: String,
    /// Component symbols.
    pub symbols: usize,
    /// Power symbols (including power flags).
    pub power_symbols: usize,
    /// Wires.
    pub wires: usize,
    /// Local labels.
    pub labels: usize,
    /// No-connect markers.
    pub no_connects: usize,
    /// Junctions.
    pub junctions: usize,
}

/// Writes a laid-out sheet as a KiCad schematic, with the cadlab project name as KiCad project.
pub fn to_kicad_sch(project: &Project, layout: &SheetLayout) -> String {
    export(project, layout, &Options::default()).text
}

/// S-expression node.
enum Sx {
    List(Vec<Sx>),
    Atom(String),
    Str(String),
}

fn a(s: impl Into<String>) -> Sx {
    Sx::Atom(s.into())
}

fn s(v: impl Into<String>) -> Sx {
    Sx::Str(v.into())
}

macro_rules! sx {
    ($head:expr $(, $item:expr)* $(,)?) => {
        Sx::List(vec![a($head) $(, Sx::from($item))*])
    };
}

impl From<&str> for Sx {
    fn from(v: &str) -> Sx {
        a(v)
    }
}

impl From<String> for Sx {
    fn from(v: String) -> Sx {
        a(v)
    }
}

impl Sx {
    fn push(&mut self, item: Sx) {
        if let Sx::List(v) = self {
            v.push(item);
        }
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            Sx::Atom(t) => out.push_str(t),
            Sx::Str(t) => {
                out.push('"');
                for c in t.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Sx::List(items) => {
                out.push('(');
                let nested = items.iter().any(|i| matches!(i, Sx::List(_)));
                let xy_run = items
                    .iter()
                    .skip(1)
                    .all(|i| matches!(i, Sx::List(v) if v.iter().all(|x| !matches!(x, Sx::List(_)))));
                let is_pts = matches!(items.first(), Some(Sx::Atom(h)) if h == "pts") && xy_run;
                for (k, item) in items.iter().enumerate() {
                    if k > 0 {
                        if nested && matches!(item, Sx::List(_)) && !is_pts {
                            out.push('\n');
                            out.extend(std::iter::repeat_n('\t', depth + 1));
                        } else {
                            out.push(' ');
                        }
                    }
                    item.write(out, depth + 1);
                }
                if nested && !is_pts {
                    out.push('\n');
                    out.extend(std::iter::repeat_n('\t', depth));
                }
                out.push(')');
            }
        }
    }
}

/// A length in millimeters, at KiCad's schematic resolution (0.1 µm).
fn mm(v: i64) -> String {
    num(v as f64 / 1e6)
}

/// A number with up to 4 decimals, no trailing zeros.
fn num(v: f64) -> String {
    let r = (v * 1e4).round() / 1e4;
    let t = format!("{r:.4}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    if t == "-0" { "0".into() } else { t.to_string() }
}

/// Deterministic UUID (version 4 layout) from a seed and a key: two FNV-1a 64-bit hashes.
fn uuid(seed: &str, key: &str) -> String {
    let h = |basis: u64| {
        let mut x = basis;
        for b in seed.bytes().chain([0u8]).chain(key.bytes()) {
            x ^= b as u64;
            x = x.wrapping_mul(0x0000_0100_0000_01b3);
        }
        x
    };
    let hi = h(0xcbf2_9ce4_8422_2325);
    let lo = h(0x6c62_272e_07bb_0142) ^ hi.rotate_left(17);
    let hi = (hi & 0xffff_ffff_ffff_0fff) | 0x4000;
    let lo = (lo & 0x3fff_ffff_ffff_ffff) | 0x8000_0000_0000_0000;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        hi >> 32,
        (hi >> 16) & 0xffff,
        hi & 0xffff,
        lo >> 48,
        lo & 0xffff_ffff_ffff
    )
}

fn xy(x: f64, y: f64) -> Sx {
    sx!("xy", num(x), num(y))
}

fn at(x: f64, y: f64, angle: u32) -> Sx {
    sx!("at", num(x), num(y), angle.to_string())
}

fn stroke(width: f64) -> Sx {
    sx!("stroke", sx!("width", num(width)), sx!("type", "default"))
}

fn fill(kind: &str) -> Sx {
    sx!("fill", sx!("type", kind))
}

/// Text effects: 1.27 mm font, optional justification and hiding.
fn effects(justify: &str, hide: bool) -> Sx {
    let mut e = sx!("effects", sx!("font", sx!("size", "1.27", "1.27")));
    if !justify.is_empty() {
        let mut j = sx!("justify");
        for w in justify.split(' ') {
            j.push(a(w));
        }
        e.push(j);
    }
    if hide {
        e.push(a("hide"));
    }
    e
}

fn property(key: &str, value: &str, x: f64, y: f64, justify: &str, hide: bool) -> Sx {
    sx!("property", s(key), s(value), at(x, y, 0), effects(justify, hide))
}

/// A visible field of a symbol instance rotated by `q` quarter turns, shown horizontal on the
/// sheet with the given justification. KiCad stores field angle and justification relative to
/// the symbol's orientation and keeps text readable (a 180° result is drawn at 0° with the
/// justification flipped), so both are compensated here (observed with kicad-cli SVG export).
fn sheet_field(key: &str, value: &str, x: f64, y: f64, justify: &str, q: u8, hide: bool) -> Sx {
    let angle = if q % 2 == 1 { 90 } else { 0 };
    let flip = q % 4 == 1 || q % 4 == 2;
    let justify: Vec<&str> = justify
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| match (w, flip) {
            ("left", true) => "right",
            ("right", true) => "left",
            ("top", true) => "bottom",
            ("bottom", true) => "top",
            (w, _) => w,
        })
        .collect();
    sx!("property", s(key), s(value), at(x, y, angle), effects(&justify.join(" "), hide))
}

fn polyline(pts: &[(f64, f64)], width: f64, fill_kind: &str) -> Sx {
    let mut p = sx!("pts");
    for &(x, y) in pts {
        p.push(xy(x, y));
    }
    sx!("polyline", p, stroke(width), fill(fill_kind))
}

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64, width: f64, fill_kind: &str) -> Sx {
    sx!("rectangle", sx!("start", num(x0), num(y0)), sx!("end", num(x1), num(y1)), stroke(width), fill(fill_kind))
}

fn f(v: Nm) -> f64 {
    v.0 as f64 / 1e6
}

fn pin_type(k: PinKind) -> &'static str {
    match k {
        PinKind::Input => "input",
        PinKind::Output => "output",
        PinKind::Bidirectional => "bidirectional",
        PinKind::TriState => "tri_state",
        PinKind::Passive => "passive",
        PinKind::PowerIn => "power_in",
        PinKind::PowerOut => "power_out",
        PinKind::OpenCollector => "open_collector",
        PinKind::OpenEmitter => "open_emitter",
        PinKind::NoConnect => "no_connect",
        PinKind::Unspecified => "unspecified",
    }
}

/// KiCad pin angle: the direction from the connection point toward the body.
fn pin_angle(side: Side) -> u32 {
    match side {
        Side::Left => 0,
        Side::Right => 180,
        Side::Top => 270,
        Side::Bottom => 90,
    }
}

/// Library symbol of a part.
fn lib_symbol(part: &Part, sym: &Symbol) -> Sx {
    let name = &part.id;
    let boxed = sym.style == SymbolStyle::Box;
    let w = 0.254;
    let mut out = sx!("symbol", s(format!("{PART_LIB}:{name}")));
    if boxed {
        out.push(sx!("pin_names", sx!("offset", "0.508")));
    } else {
        out.push(sx!("pin_numbers", "hide"));
        out.push(sx!("pin_names", sx!("offset", "0"), "hide"));
    }
    out.push(sx!("exclude_from_sim", "no"));
    out.push(sx!("in_bom", "yes"));
    out.push(sx!("on_board", "yes"));

    let mut gfx = sx!("symbol", s(format!("{name}_0_1")));
    let (top, bottom) = match (sym.style, sym.body) {
        (SymbolStyle::Box, body) => {
            let (bw, bh) = body.map_or((4.0 * 2.54, 4.0 * 2.54), |(x, y)| (f(x), f(y)));
            let (hw, hh) = (bw / 2.0, bh / 2.0);
            gfx.push(rectangle(-hw, hh, hw, -hh, w, "background"));
            (hh, -hh)
        }
        (style, _) => {
            two_terminal_graphics(&mut gfx, style, w);
            (if style == SymbolStyle::Led { 2.9 } else { 2.0 }, -2.0)
        }
    };
    let fp = part.footprint().map(|r| format!("{PART_LIB}:{}", r.footprint)).unwrap_or_default();
    let (rx, ry, vy, just) = if boxed { (0.0, top + 3.81, top + 1.27, "left bottom") } else { (0.0, top, bottom, "") };
    out.push(property("Reference", part.category.refdes_prefix(), rx, ry, just, false));
    out.push(property("Value", &part.value(), rx, vy, just, false));
    out.push(property("Footprint", &fp, 0.0, 0.0, "", true));
    out.push(property("Datasheet", part.datasheet.as_deref().unwrap_or(""), 0.0, 0.0, "", true));
    out.push(property("Description", &part.description, 0.0, 0.0, "", true));
    out.push(gfx);

    let mut pins = sx!("symbol", s(format!("{name}_1_1")));
    for p in &sym.pins {
        let (Some(pa), Some(side)) = (p.at, p.side) else { continue };
        let (x, y) = (f(pa.x), f(pa.y));
        let len = if boxed || y != 0.0 { f(symbolgen::PIN_LENGTH) } else { (x.abs() - body_half(sym.style)).max(0.0) };
        let pin_name = if p.name.is_empty() { "~" } else { p.name.as_str() };
        pins.push(sx!(
            "pin",
            pin_type(p.kind),
            "line",
            at(x, y, pin_angle(side)),
            sx!("length", num(len)),
            sx!("name", s(pin_name), effects("", false)),
            sx!("number", s(p.number.as_str()), effects("", false)),
        ));
    }
    out.push(pins);
    out
}

/// Drawing of a two-terminal symbol (pins at ±3.81 mm on X), after `symbol::draw`.
fn two_terminal_graphics(g: &mut Sx, style: SymbolStyle, w: f64) {
    match style {
        SymbolStyle::Resistor => g.push(rectangle(-2.0, 0.75, 2.0, -0.75, w, "none")),
        SymbolStyle::Fuse => {
            g.push(rectangle(-2.0, 0.6, 2.0, -0.6, w, "none"));
            g.push(polyline(&[(-2.0, 0.0), (2.0, 0.0)], w, "none"));
        }
        SymbolStyle::Capacitor | SymbolStyle::CapacitorPolarized => {
            g.push(polyline(&[(-0.5, -1.6), (-0.5, 1.6)], 2.0 * w, "none"));
            g.push(polyline(&[(0.5, -1.6), (0.5, 1.6)], 2.0 * w, "none"));
            if style == SymbolStyle::CapacitorPolarized {
                g.push(polyline(&[(-1.6, 1.2), (-1.0, 1.2)], w, "none"));
                g.push(polyline(&[(-1.3, 0.9), (-1.3, 1.5)], w, "none"));
            }
        }
        SymbolStyle::Inductor => {
            for k in 0..4 {
                let cx = -1.65 + 1.1 * k as f64;
                g.push(sx!(
                    "arc",
                    sx!("start", num(cx - 0.55), "0"),
                    sx!("mid", num(cx), "0.55"),
                    sx!("end", num(cx + 0.55), "0"),
                    stroke(w),
                    fill("none"),
                ));
            }
        }
        SymbolStyle::FerriteBead => g.push(rectangle(-2.2, 0.7, 2.2, -0.7, w, "outline")),
        SymbolStyle::Diode | SymbolStyle::Led => {
            g.push(polyline(&[(-1.27, -1.1), (-1.27, 1.1), (1.0, 0.0), (-1.27, -1.1)], w, "background"));
            g.push(polyline(&[(1.0, -1.1), (1.0, 1.1)], 1.5 * w, "none"));
            if style == SymbolStyle::Led {
                for dx in [-0.6, 0.4] {
                    g.push(polyline(&[(dx, 1.4), (dx + 1.0, 2.4)], w, "none"));
                    g.push(polyline(
                        &[(dx + 1.0, 2.4), (dx + 0.55, 2.25), (dx + 0.85, 1.95), (dx + 1.0, 2.4)],
                        w,
                        "outline",
                    ));
                }
            }
        }
        SymbolStyle::Crystal => {
            g.push(rectangle(-0.6, 1.2, 0.6, -1.2, w, "none"));
            g.push(polyline(&[(-1.2, -1.5), (-1.2, 1.5)], 1.5 * w, "none"));
            g.push(polyline(&[(1.2, -1.5), (1.2, 1.5)], 1.5 * w, "none"));
        }
        SymbolStyle::Switch => {
            for cx in [-1.3, 1.3] {
                g.push(sx!("circle", sx!("center", num(cx), "0"), sx!("radius", "0.35"), stroke(w), fill("none")));
            }
            g.push(polyline(&[(-1.9, 1.0), (1.9, 1.0)], w, "none"));
            g.push(polyline(&[(0.0, 1.0), (0.0, 2.0)], w, "none"));
            g.push(polyline(&[(-0.7, 2.0), (0.7, 2.0)], w, "none"));
        }
        _ => g.push(sx!("circle", sx!("center", "0", "0"), sx!("radius", "1"), stroke(w), fill("none"))),
    }
}

/// Power symbol kinds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PowerKind {
    Supply,
    Ground,
    Flag,
}

/// Library power symbol: pin at the origin, graphics toward +Y (supply, flag) or −Y (ground).
fn power_symbol(entry: &str, net: &str, kind: PowerKind) -> Sx {
    let w = 0.254;
    let mut out = sx!(
        "symbol",
        s(format!("{POWER_LIB}:{entry}")),
        sx!("power"),
        sx!("pin_numbers", "hide"),
        sx!("pin_names", sx!("offset", "0"), "hide"),
        sx!("exclude_from_sim", "no"),
        sx!("in_bom", "yes"),
        sx!("on_board", "yes"),
    );
    let (reference, value_y, value_hidden, description) = match kind {
        PowerKind::Supply => ("#PWR", 3.3, false, format!("Power net {net}")),
        PowerKind::Ground => ("#PWR", -3.81, false, format!("Ground net {net}")),
        PowerKind::Flag => ("#FLG", 1.27, true, "Marks a net as supplied (cadlab: driven or ground net)".into()),
    };
    out.push(property("Reference", reference, 0.0, 0.0, "", true));
    out.push(property("Value", net, 0.0, value_y, "", value_hidden));
    out.push(property("Footprint", "", 0.0, 0.0, "", true));
    out.push(property("Datasheet", "", 0.0, 0.0, "", true));
    out.push(property("Description", &description, 0.0, 0.0, "", true));
    let mut gfx = sx!("symbol", s(format!("{entry}_0_1")));
    let (pin_kind, angle) = match kind {
        PowerKind::Supply => {
            gfx.push(polyline(&[(0.0, 0.0), (0.0, 2.0)], w, "none"));
            gfx.push(polyline(&[(-1.0, 2.0), (1.0, 2.0)], 1.4 * w, "none"));
            ("power_in", 90)
        }
        PowerKind::Ground => {
            gfx.push(polyline(&[(0.0, 0.0), (0.0, -1.5)], w, "none"));
            for (i, half) in [1.3, 0.85, 0.4].iter().enumerate() {
                let y = -1.5 - 0.5 * i as f64;
                gfx.push(polyline(&[(-half, y), (*half, y)], 1.2 * w, "none"));
            }
            ("power_in", 270)
        }
        PowerKind::Flag => {
            gfx.push(polyline(&[(0.0, 0.0), (0.5, 0.5), (0.0, 1.0), (-0.5, 0.5), (0.0, 0.0)], w, "none"));
            ("power_out", 90)
        }
    };
    out.push(gfx);
    out.push(sx!(
        "symbol",
        s(format!("{entry}_1_1")),
        sx!(
            "pin",
            pin_kind,
            "line",
            at(0.0, 0.0, angle),
            sx!("length", "0"),
            "hide",
            sx!("name", s(net), effects("", false)),
            sx!("number", s("1"), effects("", false)),
        ),
    ));
    out
}

/// A library entry name made of characters KiCad accepts in library IDs.
fn entry_name(net: &str) -> String {
    net.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-') { c } else { '_' }).collect()
}

/// Sheet → KiCad coordinates (mm, Y down).
struct Frame {
    height: i64,
}

impl Frame {
    fn pt(&self, p: Point) -> (f64, f64) {
        (p.x.0 as f64 / 1e6, (self.height - p.y.0) as f64 / 1e6)
    }

    fn xy(&self, p: Point) -> Sx {
        sx!("xy", mm(p.x.0), mm(self.height - p.y.0))
    }

    fn at(&self, p: Point, angle: u32) -> Sx {
        sx!("at", mm(p.x.0), mm(self.height - p.y.0), angle.to_string())
    }
}

/// Angle of a direction (degrees, counter-clockwise from +X).
fn dir_angle(d: Dir) -> u32 {
    match d {
        Dir::Right => 0,
        Dir::Up => 90,
        Dir::Left => 180,
        Dir::Down => 270,
    }
}

fn shift(p: Point, d: Dir, len: i64) -> Point {
    let (dx, dy) = d.vec();
    Point::new(Nm(p.x.0 + dx * len), Nm(p.y.0 + dy * len))
}

/// Writes a laid-out sheet as a KiCad schematic.
pub fn export(project: &Project, layout: &SheetLayout, options: &Options) -> KicadSchematic {
    let c = project.circuit();
    let lib = project.library();
    let name = &project.manifest().name;
    let kproject = options.project_name.clone().unwrap_or_else(|| name.clone());
    let seed = name.as_str();
    let root = uuid(seed, "sheet:/");
    let frame = Frame { height: layout.size.1.0 };
    let pin_net = c.pin_index();

    let mut doc = sx!(
        "kicad_sch",
        sx!("version", FORMAT_VERSION),
        sx!("generator", s("cadlab")),
        sx!("generator_version", s(env!("CARGO_PKG_VERSION"))),
        sx!("uuid", s(root.as_str())),
    );
    if layout.paper == "User" {
        doc.push(sx!("paper", s("User"), mm(layout.size.0.0), mm(layout.size.1.0)));
    } else {
        doc.push(sx!("paper", s(layout.paper.as_str())));
    }
    let mut tb = sx!("title_block", sx!("title", s(name.as_str())));
    if let Some(rev) = project.manifest().metadata.get("rev") {
        tb.push(sx!("rev", s(rev.as_str())));
    }
    tb.push(sx!("comment", "1", s(format!("Generated by cadlab {}", env!("CARGO_PKG_VERSION")))));
    doc.push(tb);

    // Components with their parts and symbols.
    let mut comps: Vec<(&String, &Part, Symbol, crate::schematic::Placement)> = Vec::new();
    for (r, pl) in &layout.placements {
        let Some(part) = c.components.get(r).and_then(|comp| lib.parts.get(&comp.part)) else { continue };
        comps.push((r, part, symbol_of(part), *pl));
    }
    // Pin ends on the sheet → pin.
    let mut pin_at: BTreeMap<(i64, i64), PinRef> = BTreeMap::new();
    let mut pin_points: BTreeSet<(i64, i64)> = BTreeSet::new();
    for (r, _, sym, pl) in &comps {
        for (num, end, _) in pin_ends(sym, pl) {
            pin_at.entry((end.x.0, end.y.0)).or_insert_with(|| PinRef::new((*r).clone(), num));
            pin_points.insert((end.x.0, end.y.0));
        }
    }

    // Power symbols needed, by net.
    let mut power_nets: BTreeMap<&str, PowerKind> = BTreeMap::new();
    for l in &layout.labels {
        match l.kind {
            LabelKind::Power => power_nets.insert(l.net.as_str(), PowerKind::Supply),
            LabelKind::Ground => power_nets.insert(l.net.as_str(), PowerKind::Ground),
            _ => None,
        };
    }
    let mut entries: BTreeMap<&str, String> = BTreeMap::new();
    let mut taken: BTreeSet<String> = ["PWR_FLAG".to_string()].into();
    for net in power_nets.keys() {
        let base = entry_name(net);
        let mut e = base.clone();
        let mut k = 2;
        while taken.contains(&e) {
            e = format!("{base}_{k}");
            k += 1;
        }
        taken.insert(e.clone());
        entries.insert(net, e);
    }
    // Nets needing a power flag: supplied in cadlab's sense, but with no power output pin.
    let has_power_out = |net: &str| {
        c.nets.get(net).is_some_and(|n| {
            n.pins.iter().any(|p| {
                c.components
                    .get(&p.refdes)
                    .and_then(|comp| lib.parts.get(&comp.part))
                    .and_then(|pt| pt.symbol.pins.iter().find(|sp| sp.number == p.pin))
                    .is_some_and(|sp| sp.kind == PinKind::PowerOut)
            })
        })
    };
    let flagged: BTreeSet<&str> = power_nets
        .iter()
        .filter(|(net, kind)| {
            let driven = c.nets.get(**net).is_some_and(|n| n.driven);
            !has_power_out(net) && (driven || **kind == PowerKind::Ground || is_ground(net))
        })
        .map(|(net, _)| *net)
        .collect();

    // Library symbols.
    let mut libs = sx!("lib_symbols");
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut by_id: Vec<(&str, &Part, &Symbol)> = Vec::new();
    for (_, part, sym, _) in &comps {
        if seen.insert(part.id.as_str()) {
            by_id.push((part.id.as_str(), part, sym));
        }
    }
    by_id.sort_by(|x, y| x.0.cmp(y.0));
    for (_, part, sym) in &by_id {
        libs.push(lib_symbol(part, sym));
    }
    let mut power_libs: Vec<(String, Sx)> =
        power_nets.iter().map(|(net, kind)| (entries[net].clone(), power_symbol(&entries[net], net, *kind))).collect();
    if !flagged.is_empty() {
        power_libs.push(("PWR_FLAG".into(), power_symbol("PWR_FLAG", "PWR_FLAG", PowerKind::Flag)));
    }
    power_libs.sort_by(|x, y| x.0.cmp(&y.0));
    for (_, sx) in power_libs {
        libs.push(sx);
    }
    doc.push(libs);

    // Junctions, as drawn.
    let junctions: Vec<(i64, i64)> = super::junctions(project, layout).iter().map(|p| (p.x.0, p.y.0)).collect();
    for (x, y) in &junctions {
        let p = Point::new(Nm(*x), Nm(*y));
        doc.push(sx!(
            "junction",
            sx!("at", mm(p.x.0), mm(frame.height - p.y.0)),
            sx!("diameter", "0"),
            sx!("color", "0", "0", "0", "0"),
            sx!("uuid", s(uuid(seed, &format!("junction:{x},{y}")))),
        ));
    }

    // No-connect markers.
    for p in &layout.no_connects {
        let (x, y) = frame.pt(*p);
        doc.push(sx!(
            "no_connect",
            sx!("at", num(x), num(y)),
            sx!("uuid", s(uuid(seed, &format!("nc:{},{}", p.x.0, p.y.0))))
        ));
    }

    // Wires.
    for (i, (p, q)) in layout.wires.iter().enumerate() {
        doc.push(sx!(
            "wire",
            sx!("pts", frame.xy(*p), frame.xy(*q)),
            sx!("stroke", sx!("width", "0"), sx!("type", "default")),
            sx!("uuid", s(uuid(seed, &format!("wire:{i}:{},{}:{},{}", p.x.0, p.y.0, q.x.0, q.y.0)))),
        ));
    }

    // Local labels: the layout's net and wire labels, plus one on each wired net left unnamed.
    let mut labels: Vec<(String, Point, Dir)> = layout
        .labels
        .iter()
        .filter(|l| matches!(l.kind, LabelKind::Net | LabelKind::Wire))
        .map(|l| (l.net.clone(), l.at, l.dir))
        .collect();
    let named: BTreeSet<&str> = layout.labels.iter().map(|l| l.net.as_str()).collect();
    let mut added: BTreeSet<String> = BTreeSet::new();
    for (p, q) in &layout.wires {
        let net = [p, q].iter().filter_map(|e| pin_at.get(&(e.x.0, e.y.0))).find_map(|pin| pin_net.get(pin).copied());
        let Some(net) = net else { continue };
        if named.contains(net) || added.contains(net) {
            continue;
        }
        let d = if p.x == q.x {
            if q.y > p.y { Dir::Up } else { Dir::Down }
        } else if q.x > p.x {
            Dir::Right
        } else {
            Dir::Left
        };
        labels.push((net.to_string(), shift(*p, d, GRID.0 / 2), d));
        added.insert(net.to_string());
    }
    for (net, p, d) in &labels {
        let justify = match d {
            Dir::Right | Dir::Up => "left bottom",
            Dir::Left | Dir::Down => "right bottom",
        };
        doc.push(sx!(
            "label",
            s(net.as_str()),
            frame.at(*p, dir_angle(*d)),
            effects(justify, false),
            sx!("uuid", s(uuid(seed, &format!("label:{net}:{},{}", p.x.0, p.y.0)))),
        ));
    }

    // Component symbols.
    let instances = |reference: &str| {
        sx!(
            "instances",
            sx!(
                "project",
                s(kproject.as_str()),
                sx!("path", s(format!("/{root}")), sx!("reference", s(reference)), sx!("unit", "1"))
            )
        )
    };
    for (r, part, sym, pl) in &comps {
        let key = format!("symbol:{r}");
        let (x, y) = frame.pt(pl.at);
        let mut inst = sx!(
            "symbol",
            sx!("lib_id", s(format!("{PART_LIB}:{}", part.id))),
            at(x, y, pl.rot as u32 % 4 * 90),
            sx!("unit", "1"),
            sx!("exclude_from_sim", "no"),
            sx!("in_bom", "yes"),
            sx!("on_board", "yes"),
            sx!("dnp", "no"),
            sx!("uuid", s(uuid(seed, &key))),
        );
        // Designator and value placed as cadlab draws them (sheet coordinates, Y up).
        let value = part.value();
        let [rf, vf] = field_positions(sym, pl);
        let justify = |fp: &super::symbol::FieldPos| {
            let h = match fp.h {
                HAlign::Left => "left",
                HAlign::Right => "right",
                HAlign::Center => "",
            };
            let v = match fp.v {
                VAlign::Top => "top",
                VAlign::Bottom => "bottom",
                VAlign::Middle => "",
            };
            [h, v].iter().filter(|w| !w.is_empty()).copied().collect::<Vec<_>>().join(" ")
        };
        let kpt = |at: (f64, f64)| (at.0, frame.height as f64 / 1e6 - at.1);
        let (ref_pos, val_pos) = (kpt(rf.at), kpt(vf.at));
        let (ref_just, val_just) = (justify(&rf), justify(&vf));
        let fp = part.footprint().map(|r| format!("{PART_LIB}:{}", r.footprint)).unwrap_or_default();
        inst.push(sheet_field("Reference", r, ref_pos.0, ref_pos.1, &ref_just, pl.rot, false));
        inst.push(sheet_field("Value", &value, val_pos.0, val_pos.1, &val_just, pl.rot, false));
        inst.push(property("Footprint", &fp, x, y, "", true));
        inst.push(property("Datasheet", part.datasheet.as_deref().unwrap_or(""), x, y, "", true));
        inst.push(property("Description", &part.description, x, y, "", true));
        for p in &sym.pins {
            inst.push(sx!("pin", s(p.number.as_str()), sx!("uuid", s(uuid(seed, &format!("{key}:pin:{}", p.number))))));
        }
        inst.push(instances(r));
        doc.push(inst);
    }

    // Power symbols at power/ground labels, and power flags.
    let mut n_power = 0usize;
    let mut flags_done: BTreeSet<&str> = BTreeSet::new();
    for l in &layout.labels {
        let kind = match l.kind {
            LabelKind::Power => PowerKind::Supply,
            LabelKind::Ground => PowerKind::Ground,
            _ => continue,
        };
        let mut emit = |kind: PowerKind, entry: &str, value: &str, reference: String| {
            // The library drawing points up (supply, flag) or down (ground).
            let base = if kind == PowerKind::Ground { Dir::Down } else { Dir::Up };
            let q = (0..4u8).find(|q| base.rotated(*q) == l.dir).unwrap_or(0);
            let (x, y) = frame.pt(l.at);
            let key = format!("power:{reference}:{}:{},{}", l.net, l.at.x.0, l.at.y.0);
            let mut inst = sx!(
                "symbol",
                sx!("lib_id", s(format!("{POWER_LIB}:{entry}"))),
                at(x, y, q as u32 * 90),
                sx!("unit", "1"),
                sx!("exclude_from_sim", "no"),
                sx!("in_bom", "yes"),
                sx!("on_board", "yes"),
                sx!("dnp", "no"),
                sx!("uuid", s(uuid(seed, &key))),
            );
            let reach = if kind == PowerKind::Ground { 3.2 } else { 2.6 };
            let tip = shift(l.at, l.dir, (reach * 1e6) as i64);
            let (tx, ty) = frame.pt(tip);
            let just = match l.dir {
                Dir::Up => "bottom",
                Dir::Down => "top",
                Dir::Right => "left",
                Dir::Left => "right",
            };
            inst.push(property("Reference", &reference, x, y, "", true));
            inst.push(sheet_field("Value", value, tx, ty, just, q, kind == PowerKind::Flag));
            inst.push(property("Footprint", "", x, y, "", true));
            inst.push(property("Datasheet", "", x, y, "", true));
            inst.push(sx!("pin", s("1"), sx!("uuid", s(uuid(seed, &format!("{key}:pin"))))));
            inst.push(instances(&reference));
            doc.push(inst);
        };
        n_power += 1;
        emit(kind, &entries[l.net.as_str()], &l.net, format!("#PWR{n_power:02}"));
        if flagged.contains(l.net.as_str()) && flags_done.insert(l.net.as_str()) {
            emit(PowerKind::Flag, "PWR_FLAG", "PWR_FLAG", format!("#FLG{:02}", flags_done.len()));
        }
    }

    // Block instance frames: a dashed rectangle and its title.
    for fr in &layout.frames {
        let (x0, y0) = frame.pt(fr.min);
        let (x1, y1) = frame.pt(fr.max);
        doc.push(sx!(
            "rectangle",
            sx!("start", num(x0), num(y1)),
            sx!("end", num(x1), num(y0)),
            sx!("stroke", sx!("width", "0"), sx!("type", "dash")),
            fill("none"),
            sx!("uuid", s(uuid(seed, &format!("frame:{}", fr.title)))),
        ));
        let (title_at, size) = frame_title(fr);
        let (tx, ty) = (title_at.0, frame.height as f64 / 1e6 - title_at.1);
        let mut e = sx!("effects", sx!("font", sx!("size", num(size), num(size))));
        e.push(sx!("justify", "left", "top"));
        doc.push(sx!(
            "text",
            s(fr.title.as_str()),
            sx!("exclude_from_sim", "no"),
            at(tx, ty, 0),
            e,
            sx!("uuid", s(uuid(seed, &format!("frame-title:{}", fr.title)))),
        ));
    }

    doc.push(sx!("sheet_instances", sx!("path", s("/"), sx!("page", s("1")))));

    let mut text = String::new();
    doc.write(&mut text, 0);
    text.push('\n');
    KicadSchematic {
        text,
        symbols: comps.len(),
        power_symbols: n_power + flags_done.len(),
        wires: layout.wires.len(),
        labels: labels.len(),
        no_connects: layout.no_connects.len(),
        junctions: junctions.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_uuids() {
        assert_eq!(mm(2_540_000), "2.54");
        assert_eq!(mm(-1_270_000), "-1.27");
        assert_eq!(mm(0), "0");
        assert_eq!(num(-0.00001), "0");
        let u = uuid("p", "x");
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_eq!(u, uuid("p", "x"));
        assert_ne!(u, uuid("p", "y"));
        assert_ne!(uuid("ab", "c"), uuid("a", "bc"));
    }

    #[test]
    fn sexpr_writer() {
        let mut out = String::new();
        sx!("a", s("x\"y"), sx!("b", "1"), sx!("pts", sx!("xy", "0", "0"), sx!("xy", "1", "1"))).write(&mut out, 0);
        assert_eq!(out, "(a \"x\\\"y\"\n\t(b 1)\n\t(pts (xy 0 0) (xy 1 1))\n)");
        assert_eq!(entry_name("LED2/A b"), "LED2_A_b");
    }
}
