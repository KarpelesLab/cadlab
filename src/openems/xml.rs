//! The CSXCAD XML geometry file (a `<ContinuousStructure>` document, the geometry-only file
//! openEMS's tools read and write), written from openEMS's documentation: properties holding
//! primitives, a rectilinear grid; the dispersive (Debye) material and the conducting sheet with
//! the parameter names of CSXCAD's documented `SetMaterialProperty` (`Epsilon`, `Kappa`,
//! `EpsilonDelta_<n>`, `EpsilonRelaxTime_<n>`) and `AddConductingSheet` (conductivity,
//! thickness) interfaces (DECISIONS D68).

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
const PRIO_METAL: u32 = 10;
const PRIO_SHEET: u32 = 11;
const PRIO_NPTH: u32 = 20;

pub(super) struct Geometry<'a> {
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

/// Opens the material property of a dielectric: a plain `Material` (`er_text`) when lossless,
/// else a `DebyeMaterial` holding the Djordjevic–Sarkar model (at its reference frequency, or
/// anchored at f₀) fitted over the band as the built-in solver does: ε∞, a conductivity and Δε
/// on the shared relaxation times `taus` (terms of zero weight left out). Returns the closing
/// tag.
fn open_material(
    out: &mut String,
    id: usize,
    name: &str,
    er_text: &str,
    d: &Dispersive,
    g: &Geometry<'_>,
    taus: &[f64],
) -> &'static str {
    if d.tan_d <= 0.0 {
        let _ = writeln!(out, "      <Material ID=\"{id}\" Name=\"{name}\">");
        let _ = writeln!(out, "        <Property Epsilon=\"{er_text}\"/>");
        return "Material";
    }
    let f = fit::debye_fit(&d.model(g.f0 as f64), taus, g.band.0, g.band.1);
    let mut terms = String::new();
    let mut n = 0;
    for (delta, tau) in f.delta.iter().zip(taus).filter(|(d, _)| **d > 0.0) {
        n += 1;
        let _ = write!(terms, " EpsilonDelta_{n}=\"{}\" EpsilonRelaxTime_{n}=\"{}\"", num(*delta), num(*tau));
    }
    let (e0, t0) = d.at(g.f0 as f64);
    let _ = writeln!(
        out,
        "      <!-- Djordjevic–Sarkar as {n} Debye terms fitted from {} to {} Hz; at {} Hz: εr {e0:.6}, tan δ {t0:.6} -->",
        g.band.0, g.band.1, g.f0
    );
    let _ = writeln!(out, "      <DebyeMaterial ID=\"{id}\" Name=\"{name}\">");
    let kappa = if f.sigma > 0.0 { format!(" Kappa=\"{}\"", num(f.sigma)) } else { String::new() };
    let _ = writeln!(out, "        <Property Epsilon=\"{}\"{kappa}{terms}/>", num(f.eps_inf));
    "DebyeMaterial"
}

fn xml_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace("--", "- -")
}

/// Writes one hole-free ring as a primitive spanning `z0..z1`: a box when it is an
/// axis-aligned rectangle, else an extruded polygon (`LinPoly`), or a flat `Polygon` sheet.
fn primitive(out: &mut String, r: &Ring, z0: i64, z1: i64, prio: u32) {
    let pts = &r.0;
    if let Some(b) = r.bbox() {
        let rect = 2 * (b.max.x - b.min.x) as i128 * (b.max.y - b.min.y) as i128;
        let distinct: BTreeSet<_> = pts.iter().collect();
        if distinct.len() == 4 && ring_area2(r).unsigned_abs() as i128 == rect {
            let _ = writeln!(
                out,
                "          <Box Priority=\"{prio}\">\n            <P1 X=\"{}\" Y=\"{}\" Z=\"{}\"/>\n            <P2 X=\"{}\" Y=\"{}\" Z=\"{}\"/>\n          </Box>",
                mm(b.min.x),
                mm(b.min.y),
                mm(z0),
                mm(b.max.x),
                mm(b.max.y),
                mm(z1)
            );
            return;
        }
    }
    if z0 == z1 {
        let _ = writeln!(
            out,
            "          <Polygon Priority=\"{prio}\" Elevation=\"{}\" NormDir=\"2\" CoordSystem=\"0\" QtyVertices=\"{}\">",
            mm(z0),
            pts.len()
        );
    } else {
        let _ = writeln!(
            out,
            "          <LinPoly Priority=\"{prio}\" Elevation=\"{}\" Length=\"{}\" NormDir=\"2\" CoordSystem=\"0\" QtyVertices=\"{}\">",
            mm(z0),
            mm(z1 - z0),
            pts.len()
        );
    }
    for p in pts {
        let _ = writeln!(out, "            <Vertex X1=\"{}\" X2=\"{}\"/>", mm(p.x), mm(p.y));
    }
    let _ = writeln!(out, "          </{}>", if z0 == z1 { "Polygon" } else { "LinPoly" });
}

fn cylinder(out: &mut String, h: &Hole, z0: i64, z1: i64, prio: u32) {
    match &h.slot {
        Some(s) => {
            let mut rings = Vec::new();
            hole_free(std::slice::from_ref(s), &mut rings);
            for r in &rings {
                primitive(out, r, z0, z1, prio);
            }
        }
        None => {
            let (x, y) = (mm(h.center.x.0), mm(h.center.y.0));
            let _ = writeln!(
                out,
                "          <Cylinder Priority=\"{prio}\" Radius=\"{}\">\n            <P1 X=\"{x}\" Y=\"{y}\" Z=\"{}\"/>\n            <P2 X=\"{x}\" Y=\"{y}\" Z=\"{}\"/>\n          </Cylinder>",
                mm(h.radius),
                mm(z0),
                mm(z1)
            );
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
    let mut id = 0;
    let mut board_rings = Vec::new();
    hole_free(g.board, &mut board_rings);
    let taus = fit::relaxation_times(g.band.0, g.band.1);
    let f0 = g.f0 as f64;
    for d in g.diels {
        let er = d.d.er;
        // Unset: FR-4's typical value, as the loss calculations assume (DECISIONS D52).
        let tan = d.d.loss_tangent.unwrap_or(crate::model::board::DEFAULT_LOSS_TANGENT);
        let model = Dispersive { er: er.to_f64(), tan_d: tan.to_f64(), ref_freq: crate::electrical::ref_freq(&d.d) };
        let _ = writeln!(
            out,
            "      <!-- gap {}: {} thick, εr {}, tan δ {}{}{}{} -->",
            d.gap,
            mm(d.d.thickness.0),
            er,
            tan.decimal_in(0),
            if d.d.loss_tangent.is_none() { " assumed" } else { "" },
            d.d.ref_freq.filter(|_| model.ref_freq.is_some()).map(|f| format!(" at {f}")).unwrap_or_default(),
            d.d.material.as_deref().map(|m| format!(", {}", xml_text(m))).unwrap_or_default()
        );
        let tag = open_material(&mut out, id, &format!("dielectric_{}", d.gap), &er.to_string(), &model, g, &taus);
        id += 1;
        let _ = writeln!(out, "        <Primitives>");
        for r in &board_rings {
            primitive(&mut out, r, d.bottom, d.top, PRIO_DIELECTRIC);
        }
        let _ = writeln!(out, "        </Primitives>");
        let _ = writeln!(out, "      </{tag}>");
    }
    if !g.masks.is_empty() {
        let (er, tan) = g.mask_material;
        let _ = writeln!(out, "      <!-- solder mask: εr {er}, tan δ {tan} -->");
        let tag = open_material(&mut out, id, "solder_mask", &er.to_string(), &Dispersive::constant(er, tan), g, &taus);
        id += 1;
        let _ = writeln!(out, "        <Primitives>");
        for m in g.masks {
            let (a, b) = (m.laminate.0.min(m.laminate.1), m.laminate.0.max(m.laminate.1));
            for r in &board_rings {
                primitive(&mut out, r, a, b, PRIO_MASK);
            }
            let (a, b) = (m.copper.0.min(m.copper.1), m.copper.0.max(m.copper.1));
            for c in g.copper.iter().filter(|c| c.layer == m.layer) {
                let mut rings = Vec::new();
                hole_free(&c.shape, &mut rings);
                for r in &rings {
                    primitive(&mut out, r, a, b, PRIO_MASK_COPPER);
                }
            }
        }
        let _ = writeln!(out, "        </Primitives>");
        let _ = writeln!(out, "      </{tag}>");
        let _ = writeln!(out, "      <!-- air over the copper beyond its mask, and in the pad openings -->");
        let _ = writeln!(out, "      <Material ID=\"{id}\" Name=\"mask_openings\">");
        id += 1;
        let _ = writeln!(out, "        <Property Epsilon=\"1\"/>");
        let _ = writeln!(out, "        <Primitives>");
        for m in g.masks {
            let (lo, hi) = (m.span.0.min(m.span.1), m.span.0.max(m.span.1));
            // Above the copper, between its mask and the mask over the laminate.
            let (cu, over) = m.copper;
            let (c0, c1) = if over > cu { (over, hi) } else { (lo, over) };
            if c1 > c0 {
                for c in g.copper.iter().filter(|c| c.layer == m.layer) {
                    let mut rings = Vec::new();
                    hole_free(&c.shape, &mut rings);
                    for r in &rings {
                        primitive(&mut out, r, c0, c1, PRIO_MASK_CLEAR);
                    }
                }
            }
            let mut rings = Vec::new();
            hole_free(&m.openings, &mut rings);
            for r in &rings {
                primitive(&mut out, r, lo, hi, PRIO_MASK_OPENING);
            }
        }
        let _ = writeln!(out, "        </Primitives>");
        let _ = writeln!(out, "      </Material>");
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
        if let Some(n) = net {
            let _ = writeln!(out, "      <!-- net {} -->", xml_text(n));
        }
        let copper: Vec<&Copper> = g.copper.iter().filter(|c| c.net.as_ref() == net).collect();
        let holes: Vec<&Hole> = g.holes.iter().filter(|h| h.plated && h.net.as_ref() == net).collect();
        // With a conductivity the copper is all in the conducting sheets: a perfect conductor
        // between the two sheets of solid copper took 40 % of their loss in openEMS (D68).
        let volumes: Vec<&&Copper> = copper.iter().filter(|_| sheet_sigma.is_none()).collect();
        if !volumes.is_empty() || !holes.is_empty() {
            let _ = writeln!(out, "      <Metal ID=\"{id}\" Name=\"{}\">", unique(base.clone()));
            id += 1;
            let _ = writeln!(out, "        <Primitives>");
            for c in volumes {
                let l = &g.layers[c.layer];
                let mut rings = Vec::new();
                hole_free(&c.shape, &mut rings);
                for r in &rings {
                    primitive(&mut out, r, l.bottom, l.top, PRIO_METAL);
                }
            }
            for h in holes {
                cylinder(&mut out, h, g.layers[h.span.1].bottom, g.layers[h.span.0].top, PRIO_METAL);
            }
            let _ = writeln!(out, "        </Primitives>");
            let _ = writeln!(out, "      </Metal>");
        }
        let Some((sigma, k)) = sheet_sigma else { continue };
        for c in copper {
            let l = &g.layers[c.layer];
            if k != 1.0 {
                let _ = writeln!(
                    out,
                    "      <!-- {}: roughness factor {k:.4} at {} Hz in the conductivity -->",
                    xml_text(&l.name),
                    g.f0
                );
            }
            let _ = writeln!(
                out,
                "      <ConductingSheet ID=\"{id}\" Name=\"{}\" Conductivity=\"{}\" Thickness=\"{}\">",
                unique(format!("{base}_{}", ident(&l.name))),
                num(sigma),
                num(l.thickness as f64 * 1e-9)
            );
            id += 1;
            let _ = writeln!(out, "        <Primitives>");
            let mut rings = Vec::new();
            hole_free(&c.shape, &mut rings);
            // Both surfaces of solid copper.
            let faces: &[i64] = if l.top > l.bottom { &[l.bottom, l.top] } else { &[l.bottom] };
            for &z in faces {
                for r in &rings {
                    primitive(&mut out, r, z, z, PRIO_SHEET);
                }
            }
            let _ = writeln!(out, "        </Primitives>");
            let _ = writeln!(out, "      </ConductingSheet>");
        }
    }
    if g.holes.iter().any(|h| !h.plated) {
        let _ = writeln!(out, "      <!-- non-plated holes: air through the board -->");
        let _ = writeln!(out, "      <Material ID=\"{id}\" Name=\"npth\">");
        let _ = writeln!(out, "        <Property Epsilon=\"1\"/>");
        let _ = writeln!(out, "        <Primitives>");
        let z0 = g.layers.last().map_or(0, |l| l.bottom).min(g.diels.iter().map(|d| d.bottom).min().unwrap_or(0));
        let z1 = g.layers.first().map_or(0, |l| l.top);
        for h in g.holes.iter().filter(|h| !h.plated) {
            cylinder(&mut out, h, z0, z1, PRIO_NPTH);
        }
        let _ = writeln!(out, "        </Primitives>");
        let _ = writeln!(out, "      </Material>");
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
