//! The CSXCAD XML geometry file (a `<ContinuousStructure>` document, the geometry-only file
//! openEMS's tools read and write), written from openEMS's documentation: properties holding
//! primitives, a rectilinear grid; the dispersive (Debye) material and the conducting sheet with
//! the parameter names of CSXCAD's documented `SetMaterialProperty` (`Epsilon`, `Kappa`,
//! `EpsilonDelta_<n>`, `EpsilonRelaxTime_<n>`) and `AddConductingSheet` (conductivity,
//! thickness) interfaces (DECISIONS D68).
//!
//! The region first becomes a list of properties with primitives ([`properties`]); the file is
//! that list written out, and the ems solver (`crate::fdtd::ems`, D86) runs the same list.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::{Copper, Diel, Hole, Layer, MaskLayer, hole_free, mm};
use crate::electrical::dispersion::Dielectric as Dispersive;
use crate::electrical::loss::{Roughness, skin_depth};
use crate::fdtd::fit;
use crate::geom::poly::{Polygon, Rect, Ring, ring_area2};

/// Priorities: dielectrics below the solder mask (over the laminate, air above the copper where
/// the mask over it is thinner, the mask over the copper, air in the pad openings), all below
/// metal; the conducting sheets on the copper surfaces above the copper volumes; non-plated
/// holes above everything.
const PRIO_DIELECTRIC: u32 = 0;
const PRIO_MASK: u32 = 5;
const PRIO_MASK_CLEAR: u32 = 6;
const PRIO_MASK_COPPER: u32 = 7;
const PRIO_MASK_OPENING: u32 = 8;
const PRIO_PART: u32 = 9;
const PRIO_METAL: u32 = 10;
const PRIO_SHEET: u32 = 11;
const PRIO_NPTH: u32 = 20;

pub(crate) struct Geometry<'a> {
    /// Board parts as lumped elements.
    pub parts: &'a [crate::fdtd::parts::PartBox],
    pub title: &'a str,
    pub layers: &'a [Layer],
    pub diels: &'a [Diel],
    pub copper: &'a [Copper],
    pub holes: &'a [Hole],
    pub board: &'a [Polygon],
    pub masks: &'a [MaskLayer],
    /// Solder mask εr and tan δ.
    pub mask_material: (f64, f64),
    /// The pulse's center (Hz): the anchor of the Djordjevic–Sarkar model of dielectrics
    /// without a reference frequency, and where the roughness factor is taken.
    pub f0: u64,
    /// Frequencies of the result (Hz): the band of the Debye fits.
    pub band: (f64, f64),
    /// Copper conductivity (S/m; `None`: perfect conductors).
    pub conductivity: Option<f64>,
    /// Copper roughness.
    pub roughness: Roughness,
    pub lines: [&'a [i64]; 3],
    pub window: Rect,
}

/// A property name: letters, digits and `_.+-`, others replaced by `_`.
fn ident(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || "_.+-".contains(c) { c } else { '_' }).collect()
}

/// A computed number in the XML: seven significant digits (beyond the fits' accuracy, and
/// short of the last bits that could differ between platforms' math libraries).
fn num(v: f64) -> String {
    format!("{v:.6e}")
}

/// What a property is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    /// A lossless dielectric: εr, and the text it is written as.
    Material { er: f64, text: String },
    /// A Debye material: ε∞, conductivity (S/m), and (Δε, relaxation time in s) terms.
    Debye { eps_inf: f64, sigma: f64, terms: Vec<(f64, f64)> },
    /// A perfect conductor.
    Metal,
    /// A thin lossy conductor: conductivity (S/m) and thickness (m).
    Sheet { conductivity: f64, thickness: f64 },
    /// A lumped element along `axis` (0 x, 1 y, 2 z): a resistance alone is a plain element,
    /// anything with a reactance a series one (`series`).
    Lumped { axis: usize, r: f64, l: f64, c: Option<f64>, series: bool },
}

/// A primitive's shape (nm).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Shape {
    /// An axis-aligned box (flat, or a line, when corners share coordinates).
    Box { lo: [i64; 3], hi: [i64; 3] },
    /// A hole-free polygon in the xy plane from `z0` to `z1` (a flat sheet when equal).
    Prism { ring: Ring, z0: i64, z1: i64 },
    /// A cylinder along z.
    Cylinder { x: i64, y: i64, radius: i64, z0: i64, z1: i64 },
}

/// A primitive with its priority.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Prim {
    pub(crate) shape: Shape,
    pub(crate) priority: u32,
}

/// A property with its primitives: one element of the geometry file, and one property of the
/// model the ems solver runs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Prop {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    /// Comment lines written before it.
    pub(crate) comments: Vec<String>,
    pub(crate) prims: Vec<Prim>,
}

/// A dielectric's property kind: a plain material (`er_text`) when lossless, else a Debye
/// material holding the Djordjevic–Sarkar model (at its reference frequency, or anchored at
/// f₀) fitted over the band as the built-in solver does: ε∞, a conductivity and Δε on the
/// shared relaxation times `taus` (terms of zero weight left out). With the comment naming the
/// fit.
fn material(er_text: &str, d: &Dispersive, g: &Geometry<'_>, taus: &[f64]) -> (Kind, Option<String>) {
    if d.tan_d <= 0.0 {
        return (Kind::Material { er: d.er, text: er_text.to_string() }, None);
    }
    let f = fit::debye_fit(&d.model(g.f0 as f64), taus, g.band.0, g.band.1);
    let terms: Vec<(f64, f64)> = f.delta.iter().zip(taus).filter(|(d, _)| **d > 0.0).map(|(d, t)| (*d, *t)).collect();
    let (e0, t0) = d.at(g.f0 as f64);
    let comment = format!(
        "Djordjevic–Sarkar as {} Debye terms fitted from {} to {} Hz; at {} Hz: εr {e0:.6}, tan δ {t0:.6}",
        terms.len(),
        g.band.0,
        g.band.1,
        g.f0
    );
    (Kind::Debye { eps_inf: f.eps_inf, sigma: f.sigma, terms }, Some(comment))
}

fn xml_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace("--", "- -")
}

/// One hole-free ring spanning `z0..z1`: a box when it is an axis-aligned rectangle, else a
/// polygon (extruded, or a flat sheet).
fn ring_prim(r: &Ring, z0: i64, z1: i64, priority: u32) -> Prim {
    if let Some(b) = r.bbox() {
        let rect = 2 * (b.max.x - b.min.x) as i128 * (b.max.y - b.min.y) as i128;
        let distinct: BTreeSet<_> = r.0.iter().collect();
        if distinct.len() == 4 && ring_area2(r).unsigned_abs() as i128 == rect {
            return Prim { shape: Shape::Box { lo: [b.min.x, b.min.y, z0], hi: [b.max.x, b.max.y, z1] }, priority };
        }
    }
    Prim { shape: Shape::Prism { ring: r.clone(), z0, z1 }, priority }
}

fn rings(shape: &[Polygon], z0: i64, z1: i64, priority: u32, out: &mut Vec<Prim>) {
    let mut rs = Vec::new();
    hole_free(shape, &mut rs);
    out.extend(rs.iter().map(|r| ring_prim(r, z0, z1, priority)));
}

fn hole_prims(h: &Hole, z0: i64, z1: i64, priority: u32, out: &mut Vec<Prim>) {
    match &h.slot {
        Some(s) => rings(std::slice::from_ref(s), z0, z1, priority, out),
        None => out.push(Prim {
            shape: Shape::Cylinder { x: h.center.x.0, y: h.center.y.0, radius: h.radius, z0, z1 },
            priority,
        }),
    }
}

/// The region as properties with primitives, in the order of the file.
pub(crate) fn properties(g: &Geometry<'_>) -> Vec<Prop> {
    let mut props = Vec::new();
    let mut board_rings = Vec::new();
    hole_free(g.board, &mut board_rings);
    let taus = fit::relaxation_times(g.band.0, g.band.1);
    let f0 = g.f0 as f64;
    for d in g.diels {
        let er = d.d.er;
        // Unset: FR-4's typical value, as the loss calculations assume (DECISIONS D52).
        let tan = d.d.loss_tangent.unwrap_or(crate::model::board::DEFAULT_LOSS_TANGENT);
        let model = Dispersive { er: er.to_f64(), tan_d: tan.to_f64(), ref_freq: crate::electrical::ref_freq(&d.d) };
        let mut comments = vec![format!(
            "gap {}: {} thick, εr {}, tan δ {}{}{}{}",
            d.gap,
            mm(d.d.thickness.0),
            er,
            tan.decimal_in(0),
            if d.d.loss_tangent.is_none() { " assumed" } else { "" },
            d.d.ref_freq.filter(|_| model.ref_freq.is_some()).map(|f| format!(" at {f}")).unwrap_or_default(),
            d.d.material.as_deref().map(|m| format!(", {}", xml_text(m))).unwrap_or_default()
        )];
        let (kind, note) = material(&er.to_string(), &model, g, &taus);
        comments.extend(note);
        props.push(Prop {
            name: format!("dielectric_{}", d.gap),
            kind,
            comments,
            prims: board_rings.iter().map(|r| ring_prim(r, d.bottom, d.top, PRIO_DIELECTRIC)).collect(),
        });
    }
    if !g.masks.is_empty() {
        let (er, tan) = g.mask_material;
        let mut comments = vec![format!("solder mask: εr {er}, tan δ {tan}")];
        let (kind, note) = material(&er.to_string(), &Dispersive::constant(er, tan), g, &taus);
        comments.extend(note);
        let mut prims = Vec::new();
        for m in g.masks {
            let (a, b) = (m.laminate.0.min(m.laminate.1), m.laminate.0.max(m.laminate.1));
            prims.extend(board_rings.iter().map(|r| ring_prim(r, a, b, PRIO_MASK)));
            let (a, b) = (m.copper.0.min(m.copper.1), m.copper.0.max(m.copper.1));
            for c in g.copper.iter().filter(|c| c.layer == m.layer) {
                rings(&c.shape, a, b, PRIO_MASK_COPPER, &mut prims);
            }
        }
        props.push(Prop { name: "solder_mask".into(), kind, comments, prims });
        let mut prims = Vec::new();
        for m in g.masks {
            let (lo, hi) = (m.span.0.min(m.span.1), m.span.0.max(m.span.1));
            // Above the copper, between its mask and the mask over the laminate.
            let (cu, over) = m.copper;
            let (c0, c1) = if over > cu { (over, hi) } else { (lo, over) };
            if c1 > c0 {
                for c in g.copper.iter().filter(|c| c.layer == m.layer) {
                    rings(&c.shape, c0, c1, PRIO_MASK_CLEAR, &mut prims);
                }
            }
            rings(&m.openings, lo, hi, PRIO_MASK_OPENING, &mut prims);
        }
        props.push(Prop {
            name: "mask_openings".into(),
            kind: Kind::Material { er: 1.0, text: "1".into() },
            comments: vec!["air over the copper beyond its mask, and in the pad openings".into()],
            prims,
        });
    }
    // Lossy copper: the skin-effect surface resistance through a conductivity, with the
    // roughness factor at f₀ folded in (Rs K = √(πfμ₀/(σ/K²))).
    let sheet_sigma = g.conductivity.map(|sigma| {
        let k = g.roughness.factor(skin_depth(f0, sigma));
        (sigma / (k * k), k)
    });
    // One metal property per net: copper on every layer (top to bottom) and plated holes; with a
    // conductivity, a conducting sheet per net and layer on the copper's surfaces.
    let mut nets: Vec<Option<&String>> = g.copper.iter().map(|c| c.net.as_ref()).collect();
    nets.extend(g.holes.iter().filter(|h| h.plated).map(|h| h.net.as_ref()));
    nets.sort_by(|a, b| (a.is_none(), a).cmp(&(b.is_none(), b)));
    nets.dedup();
    let mut used = BTreeSet::new();
    let mut unique = |base: String| {
        let mut name = base.clone();
        let mut k = 2;
        while !used.insert(name.clone()) {
            name = format!("{base}_{k}");
            k += 1;
        }
        name
    };
    for net in nets {
        let base = net.map_or("copper_nonet".to_string(), |n| format!("copper_{}", ident(n)));
        // The net's comment goes before its first property.
        let mut comments: Vec<String> = net.iter().map(|n| format!("net {}", xml_text(n))).collect();
        let copper: Vec<&Copper> = g.copper.iter().filter(|c| c.net.as_ref() == net).collect();
        let holes: Vec<&Hole> = g.holes.iter().filter(|h| h.plated && h.net.as_ref() == net).collect();
        // With a conductivity the copper is all in the conducting sheets: a perfect conductor
        // between the two sheets of solid copper took 40 % of their loss in openEMS (D68).
        let volumes: Vec<&&Copper> = copper.iter().filter(|_| sheet_sigma.is_none()).collect();
        if !volumes.is_empty() || !holes.is_empty() {
            let mut prims = Vec::new();
            for c in volumes {
                let l = &g.layers[c.layer];
                rings(&c.shape, l.bottom, l.top, PRIO_METAL, &mut prims);
            }
            for h in holes {
                let (z0, z1) = (g.layers[h.span.1].bottom, g.layers[h.span.0].top);
                hole_prims(h, z0, z1, PRIO_METAL, &mut prims);
                if h.slot.is_none() {
                    // A barrel between mesh lines holds no mesh edge, so it conducts nothing (a
                    // via read open in openEMS, D74): its nearest node column, as the built-in
                    // solver adds it (D59). On mesh coordinates: a line between them is dropped.
                    let near = |ls: &[i64], v: i64| ls.iter().copied().min_by_key(|l| (l - v).abs()).unwrap_or(v);
                    let (x, y) = (near(g.lines[0], h.center.x.0), near(g.lines[1], h.center.y.0));
                    prims.push(Prim { shape: Shape::Box { lo: [x, y, z0], hi: [x, y, z1] }, priority: PRIO_METAL });
                }
            }
            props.push(Prop {
                name: unique(base.clone()),
                kind: Kind::Metal,
                comments: std::mem::take(&mut comments),
                prims,
            });
        }
        let Some((sigma, k)) = sheet_sigma else { continue };
        for c in copper {
            let l = &g.layers[c.layer];
            let mut comments = std::mem::take(&mut comments);
            if k != 1.0 {
                comments.push(format!(
                    "{}: roughness factor {k:.4} at {} Hz in the conductivity",
                    xml_text(&l.name),
                    g.f0
                ));
            }
            let mut prims = Vec::new();
            // Both surfaces of solid copper.
            let faces: &[i64] = if l.top > l.bottom { &[l.bottom, l.top] } else { &[l.bottom] };
            for &z in faces {
                rings(&c.shape, z, z, PRIO_SHEET, &mut prims);
            }
            props.push(Prop {
                name: unique(format!("{base}_{}", ident(&l.name))),
                kind: Kind::Sheet { conductivity: sigma, thickness: l.thickness as f64 * 1e-9 },
                comments,
                prims,
            });
        }
    }
    // Board parts: `LumpedElement`s (openEMS's documented property), in the gap between their pads.
    // Only a resistance is a plain element; anything with a reactance a series one. 0 Ω is a short.
    for b in g.parts {
        let kind = if b.r == 0.0 && b.l == 0.0 && b.c.is_none() {
            Kind::Metal
        } else {
            Kind::Lumped { axis: b.axis, r: b.r, l: b.l, c: b.c, series: b.l > 0.0 || b.c.is_some() }
        };
        props.push(Prop {
            name: unique(format!("part_{}", ident(&b.info.refdes))),
            kind,
            comments: vec![xml_text(&b.info.pads.join(" to "))],
            prims: vec![Prim { shape: Shape::Box { lo: b.lo, hi: b.hi }, priority: PRIO_PART }],
        });
    }
    if g.holes.iter().any(|h| !h.plated) {
        let z0 = g.layers.last().map_or(0, |l| l.bottom).min(g.diels.iter().map(|d| d.bottom).min().unwrap_or(0));
        let z1 = g.layers.first().map_or(0, |l| l.top);
        let mut prims = Vec::new();
        for h in g.holes.iter().filter(|h| !h.plated) {
            hole_prims(h, z0, z1, PRIO_NPTH, &mut prims);
        }
        props.push(Prop {
            name: "npth".into(),
            kind: Kind::Material { er: 1.0, text: "1".into() },
            comments: vec!["non-plated holes: air through the board".into()],
            prims,
        });
    }
    props
}

fn write_prim(out: &mut String, p: &Prim) {
    let prio = p.priority;
    match &p.shape {
        Shape::Box { lo, hi } => {
            let _ = writeln!(
                out,
                "          <Box Priority=\"{prio}\">\n            <P1 X=\"{}\" Y=\"{}\" Z=\"{}\"/>\n            <P2 X=\"{}\" Y=\"{}\" Z=\"{}\"/>\n          </Box>",
                mm(lo[0]),
                mm(lo[1]),
                mm(lo[2]),
                mm(hi[0]),
                mm(hi[1]),
                mm(hi[2])
            );
        }
        Shape::Prism { ring, z0, z1 } => {
            let pts = &ring.0;
            let tag = if z0 == z1 { "Polygon" } else { "LinPoly" };
            if z0 == z1 {
                let _ = writeln!(
                    out,
                    "          <Polygon Priority=\"{prio}\" Elevation=\"{}\" NormDir=\"2\" CoordSystem=\"0\" QtyVertices=\"{}\">",
                    mm(*z0),
                    pts.len()
                );
            } else {
                let _ = writeln!(
                    out,
                    "          <LinPoly Priority=\"{prio}\" Elevation=\"{}\" Length=\"{}\" NormDir=\"2\" CoordSystem=\"0\" QtyVertices=\"{}\">",
                    mm(*z0),
                    mm(z1 - z0),
                    pts.len()
                );
            }
            for p in pts {
                let _ = writeln!(out, "            <Vertex X1=\"{}\" X2=\"{}\"/>", mm(p.x), mm(p.y));
            }
            let _ = writeln!(out, "          </{tag}>");
        }
        Shape::Cylinder { x, y, radius, z0, z1 } => {
            let (x, y) = (mm(*x), mm(*y));
            let _ = writeln!(
                out,
                "          <Cylinder Priority=\"{prio}\" Radius=\"{}\">\n            <P1 X=\"{x}\" Y=\"{y}\" Z=\"{}\"/>\n            <P2 X=\"{x}\" Y=\"{y}\" Z=\"{}\"/>\n          </Cylinder>",
                mm(*radius),
                mm(*z0),
                mm(*z1)
            );
        }
    }
}

/// The opening tag of a property (with its `<Property>` element for materials) and its tag name.
fn open_tag(id: usize, p: &Prop) -> (String, &'static str) {
    let name = &p.name;
    match &p.kind {
        Kind::Material { text, .. } => {
            (format!("<Material ID=\"{id}\" Name=\"{name}\">\n        <Property Epsilon=\"{text}\"/>"), "Material")
        }
        Kind::Debye { eps_inf, sigma, terms } => {
            let mut t = String::new();
            for (n, (delta, tau)) in terms.iter().enumerate() {
                let n = n + 1;
                let _ = write!(t, " EpsilonDelta_{n}=\"{}\" EpsilonRelaxTime_{n}=\"{}\"", num(*delta), num(*tau));
            }
            let kappa = if *sigma > 0.0 { format!(" Kappa=\"{}\"", num(*sigma)) } else { String::new() };
            (
                format!(
                    "<DebyeMaterial ID=\"{id}\" Name=\"{name}\">\n        <Property Epsilon=\"{}\"{kappa}{t}/>",
                    num(*eps_inf)
                ),
                "DebyeMaterial",
            )
        }
        Kind::Metal => (format!("<Metal ID=\"{id}\" Name=\"{name}\">"), "Metal"),
        Kind::Sheet { conductivity, thickness } => (
            format!(
                "<ConductingSheet ID=\"{id}\" Name=\"{name}\" Conductivity=\"{}\" Thickness=\"{}\">",
                num(*conductivity),
                num(*thickness)
            ),
            "ConductingSheet",
        ),
        Kind::Lumped { axis, r, l, c, series } => {
            let mut values = String::new();
            if *r > 0.0 {
                let _ = write!(values, " R=\"{}\"", num(*r));
            }
            if let Some(c) = c {
                let _ = write!(values, " C=\"{}\"", num(*c));
            }
            if *l > 0.0 {
                let _ = write!(values, " L=\"{}\"", num(*l));
            }
            let series = if *series { " LEtype=\"1\"" } else { "" };
            (
                format!("<LumpedElement ID=\"{id}\" Name=\"{name}\" Direction=\"{axis}\" Caps=\"1\"{values}{series}>"),
                "LumpedElement",
            )
        }
    }
}

pub(super) fn write(g: &Geometry<'_>) -> String {
    let mut out = String::new();
    let w = g.window;
    let _ = writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    let _ = writeln!(
        out,
        "<!-- Generated by cadlab export.openems{}: CSXCAD geometry and mesh, lengths in mm (DeltaUnit 1e-3).\n     Board region x {} .. {}, y {} .. {}; z 0 is the bottom of {}. Ports, excitation and boundaries are in the driver script. -->",
        if g.title.is_empty() { String::new() } else { format!(" ({})", xml_text(g.title)) },
        mm(w.min.x),
        mm(w.max.x),
        mm(w.min.y),
        mm(w.max.y),
        g.layers.last().map_or("the board", |l| l.name.as_str())
    );
    let _ = writeln!(out, "<ContinuousStructure CoordSystem=\"0\">");
    let _ = writeln!(out, "  <Properties>");
    for (id, p) in properties(g).iter().enumerate() {
        for c in &p.comments {
            let _ = writeln!(out, "      <!-- {c} -->");
        }
        let (open, tag) = open_tag(id, p);
        let _ = writeln!(out, "      {open}");
        let _ = writeln!(out, "        <Primitives>");
        for q in &p.prims {
            write_prim(&mut out, q);
        }
        let _ = writeln!(out, "        </Primitives>");
        let _ = writeln!(out, "      </{tag}>");
    }
    let _ = writeln!(out, "  </Properties>");
    let _ = writeln!(out, "  <RectilinearGrid DeltaUnit=\"0.001\" CoordSystem=\"0\">");
    for (tag, lines) in ["XLines", "YLines", "ZLines"].iter().zip(g.lines) {
        let v: Vec<String> = lines.iter().map(|&x| mm(x)).collect();
        let _ = writeln!(out, "    <{tag}>{}</{tag}>", v.join(","));
    }
    let _ = writeln!(out, "  </RectilinearGrid>");
    let _ = writeln!(out, "</ContinuousStructure>");
    out
}
