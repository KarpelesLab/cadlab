//! Component bodies generated from package dimensions: convex solids (boxes, prisms,
//! cylinders) in footprint coordinates (mm, IPC zero orientation, origin at the body center,
//! z up from the board surface).
//!
//! The package comes from the footprint's generator spec ([`PackageSpec`]); leads are placed
//! where the footprint's pads are, so missing pins and odd layouts follow the land pattern.
//! Footprints without a spec get a plain box from their body size or courtyard.

use crate::landpattern::{ChipKind, Dim, PackageSpec, SodLead};
use crate::model::footprint::{Footprint, Pad};
use crate::units::{LengthUnit, Nm};

/// Linear RGB-ish color in 0..1 (sRGB values, shaded linearly).
pub(crate) type Rgb = [f32; 3];

pub(crate) const fn rgb(hex: u32) -> Rgb {
    [((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0]
}

const MOLD: Rgb = rgb(0x2a2a2d);
const TIN: Rgb = rgb(0xc4c8cd);
const GOLD: Rgb = rgb(0xd6ae4c);
const DOT: Rgb = rgb(0x8c8c90);
const BAND: Rgb = rgb(0xc8c8c8);
const PLASTIC: Rgb = rgb(0x1f1f21);
const UNKNOWN: Rgb = rgb(0x6e6e72);

/// A convex solid: planar convex faces, any winding (oriented later from the centroid).
#[derive(Clone, Debug)]
pub(crate) struct Solid {
    pub faces: Vec<Vec<[f64; 3]>>,
    pub color: Rgb,
}

fn mm(v: Nm) -> f64 {
    v.to_f64(LengthUnit::Mm)
}

fn nom(d: &Dim) -> f64 {
    mm(d.nominal())
}

/// Axis-aligned box.
pub(crate) fn cuboid(a: [f64; 3], b: [f64; 3], color: Rgb) -> Solid {
    let (x0, x1) = (a[0].min(b[0]), a[0].max(b[0]));
    let (y0, y1) = (a[1].min(b[1]), a[1].max(b[1]));
    let (z0, z1) = (a[2].min(b[2]), a[2].max(b[2]));
    prism(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)], z0, z1, color)
}

/// Convex polygon extruded from `z0` to `z1`.
pub(crate) fn prism(poly: &[(f64, f64)], z0: f64, z1: f64, color: Rgb) -> Solid {
    let n = poly.len();
    let mut faces = vec![
        poly.iter().map(|&(x, y)| [x, y, z0]).collect::<Vec<_>>(),
        poly.iter().map(|&(x, y)| [x, y, z1]).collect::<Vec<_>>(),
    ];
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        faces.push(vec![[a.0, a.1, z0], [b.0, b.1, z0], [b.0, b.1, z1], [a.0, a.1, z1]]);
    }
    Solid { faces, color }
}

/// Regular polygon approximating a circle.
pub(crate) fn circle(c: (f64, f64), r: f64, n: usize) -> Vec<(f64, f64)> {
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * (i as f64 + 0.5) / n as f64;
            (c.0 + r * a.cos(), c.1 + r * a.sin())
        })
        .collect()
}

/// Cylinder along X from `x0` to `x1`, axis at (y = 0, z = `r`) so it rests on the board.
fn cylinder_x(x0: f64, x1: f64, r: f64, z_axis: f64, color: Rgb) -> Solid {
    let ring = circle((0.0, z_axis), r, 20);
    let mut faces = vec![
        ring.iter().map(|&(y, z)| [x0, y, z]).collect::<Vec<_>>(),
        ring.iter().map(|&(y, z)| [x1, y, z]).collect::<Vec<_>>(),
    ];
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        faces.push(vec![[x0, a.0, a.1], [x0, b.0, b.1], [x1, b.0, b.1], [x1, a.0, a.1]]);
    }
    Solid { faces, color }
}

/// Which edge of the body a lead leaves from: axis (0 = X, 1 = Y) and sign.
#[derive(Clone, Copy)]
struct Edge {
    axis: usize,
    sign: f64,
}

/// Box in lead coordinates (`u` outward from the body center along the edge's axis, `v`
/// along the edge) mapped to footprint coordinates.
fn lead_box(e: Edge, u: (f64, f64), v: (f64, f64), z: (f64, f64), color: Rgb) -> Solid {
    let (ua, ub) = (e.sign * u.0, e.sign * u.1);
    let (a, b) = if e.axis == 0 { ([ua, v.0, z.0], [ub, v.1, z.1]) } else { ([v.0, ua, z.0], [v.1, ub, z.1]) };
    cuboid(a, b, color)
}

/// A pad's edge and position along it, for a body of half sizes `hx`, `hy`. `None` for pads
/// under the body center (exposed pads).
fn pad_edge(p: &Pad, hx: f64, hy: f64) -> Option<(Edge, f64)> {
    let (x, y) = (mm(p.at.x), mm(p.at.y));
    if x.abs() < hx * 0.6 && y.abs() < hy * 0.6 {
        return None;
    }
    if x.abs() - hx >= y.abs() - hy {
        Some((Edge { axis: 0, sign: x.signum() }, y))
    } else {
        Some((Edge { axis: 1, sign: y.signum() }, x))
    }
}

/// Gull-wing lead: shoulder out of the body side, a leg down, a foot on the board ending at
/// `toe` (distance from the body center).
#[allow(clippy::too_many_arguments)]
fn gull_lead(out: &mut Vec<Solid>, e: Edge, at: f64, half_body: f64, toe: f64, foot: f64, width: f64, z_exit: f64) {
    let t = (width * 0.45).clamp(0.1, 0.2);
    let heel = (toe - foot).max(half_body + t);
    let v = (at - width / 2.0, at + width / 2.0);
    out.push(lead_box(e, (heel, toe.max(heel + t)), v, (0.0, t), TIN));
    out.push(lead_box(e, (heel, heel + t), v, (0.0, z_exit), TIN));
    out.push(lead_box(e, (half_body - 0.05, heel + t), v, (z_exit - t, z_exit), TIN));
}

/// Pin-1 dot on the top of a body (half sizes `hx`, `hy`, top at `z`), on the side of pad 1.
fn pin1_dot(out: &mut Vec<Solid>, fp: &Footprint, first: &str, hx: f64, hy: f64, z: f64) {
    let Some(p) = fp.pads.iter().find(|p| p.number == first) else { return };
    let r = (hx.min(hy) * 0.12).clamp(0.1, 0.5);
    let m = (r * 2.2).min(hx.min(hy) * 0.5);
    let c = (mm(p.at.x).clamp(-hx + m, hx - m), mm(p.at.y).clamp(-hy + m, hy - m));
    out.push(prism(&circle(c, r, 16), z, z + 0.02, DOT));
}

/// Band across the top of a two-terminal body near −X (cathode or + mark), between `x0` and `x1`.
fn band(out: &mut Vec<Solid>, x0: f64, x1: f64, hy: f64, z: f64, color: Rgb) {
    out.push(cuboid([x0, -hy * 0.98, z], [x1, hy * 0.98, z + 0.02], color));
}

fn chip_color(kind: ChipKind) -> (Rgb, Option<Rgb>) {
    match kind {
        ChipKind::Resistor => (rgb(0x1d1d1f), None),
        ChipKind::Capacitor => (rgb(0xb3976a), None),
        ChipKind::Inductor => (rgb(0x46474a), None),
        ChipKind::Led => (rgb(0xeeeae0), Some(rgb(0x2f9a40))),
        ChipKind::Diode => (rgb(0x222224), Some(BAND)),
        ChipKind::Fuse => (rgb(0x3a3a3c), None),
    }
}

/// Solids for one footprint, in footprint coordinates. `thickness` is the board thickness
/// (through-hole pins go through it).
pub(crate) fn solids(fp: &Footprint, thickness: f64) -> Vec<Solid> {
    let spec = fp.generator.as_ref().and_then(|v| serde_json::from_value::<PackageSpec>(v.clone()).ok());
    let mut out = Vec::new();
    match spec {
        Some(spec) => package(&mut out, fp, &spec, thickness),
        None => fallback(&mut out, fp),
    }
    out
}

fn smd_pads(fp: &Footprint) -> impl Iterator<Item = &Pad> {
    fp.pads.iter().filter(|p| !p.number.is_empty())
}

fn package(out: &mut Vec<Solid>, fp: &Footprint, spec: &PackageSpec, t: f64) {
    match spec {
        PackageSpec::Chip { kind, length, width, terminal, height } => {
            let (hl, hw, term, h) = (nom(length) / 2.0, nom(width) / 2.0, nom(terminal), mm(*height));
            let (body, mark) = chip_color(*kind);
            out.push(cuboid([-hl + term, -hw, 0.0], [hl - term, hw, h], body));
            for s in [-1.0, 1.0] {
                out.push(cuboid([s * hl, -hw * 1.01, 0.0], [s * (hl - term), hw * 1.01, h * 1.01], TIN));
            }
            if let Some(c) = mark {
                let x0 = -hl + term;
                band(out, x0, x0 + (2.0 * (hl - term)) * 0.25, hw, h, c);
            }
        }
        PackageSpec::GullWing { span, body_width, body_length, terminal, lead_width, height, .. } => {
            let (hx, hy, h) = (nom(body_width) / 2.0, nom(body_length) / 2.0, mm(*height));
            let stand = (h * 0.08).min(0.1);
            out.push(cuboid([-hx, -hy, stand], [hx, hy, h], MOLD));
            let z_exit = stand + (h - stand) * 0.4;
            for p in smd_pads(fp) {
                if let Some((e, at)) = pad_edge(p, hx, hy) {
                    let half = if e.axis == 0 { hx } else { hy };
                    gull_lead(out, e, at, half, nom(span) / 2.0, nom(terminal), nom(lead_width), z_exit);
                }
            }
            if fp.pads.len() >= 5 {
                pin1_dot(out, fp, "1", hx, hy, h);
            }
        }
        PackageSpec::Qfp { span_x, span_y, body_x, body_y, terminal, lead_width, height, .. } => {
            let (hx, hy, h) = (nom(body_x) / 2.0, nom(body_y) / 2.0, mm(*height));
            let stand = (h * 0.08).min(0.1);
            out.push(cuboid([-hx, -hy, stand], [hx, hy, h], MOLD));
            let z_exit = stand + (h - stand) * 0.4;
            for p in smd_pads(fp) {
                if let Some((e, at)) = pad_edge(p, hx, hy) {
                    let (half, toe) = if e.axis == 0 { (hx, nom(span_x) / 2.0) } else { (hy, nom(span_y) / 2.0) };
                    gull_lead(out, e, at, half, toe, nom(terminal), nom(lead_width), z_exit);
                }
            }
            pin1_dot(out, fp, "1", hx, hy, h);
        }
        PackageSpec::Dfn { body_width, body_length, terminal, lead_width, height, .. } => {
            no_lead(
                out,
                fp,
                nom(body_width) / 2.0,
                nom(body_length) / 2.0,
                nom(terminal),
                nom(lead_width),
                mm(*height),
            );
        }
        PackageSpec::Qfn { body_x, body_y, terminal, lead_width, height, .. } => {
            no_lead(out, fp, nom(body_x) / 2.0, nom(body_y) / 2.0, nom(terminal), nom(lead_width), mm(*height));
        }
        PackageSpec::Tab {
            span,
            body_width,
            body_length,
            terminal,
            lead_width,
            tab_width,
            tab_terminal,
            tab_protrusion,
            height,
            ..
        } => {
            let (hx, hy, h) = (nom(body_width) / 2.0, nom(body_length) / 2.0, mm(*height));
            let stand = (h * 0.05).min(0.1);
            out.push(cuboid([-hx, -hy, stand], [hx, hy, h], MOLD));
            let (left_toe, tab_end) = match tab_protrusion {
                Some(p) => {
                    let end = hx + nom(p);
                    (nom(span) - end, end)
                }
                None => (nom(span) / 2.0, nom(span) / 2.0),
            };
            let z_exit = stand + (h - stand) * 0.35;
            let left = Edge { axis: 0, sign: -1.0 };
            for p in smd_pads(fp).filter(|p| mm(p.at.x) < 0.0) {
                gull_lead(out, left, mm(p.at.y), hx, left_toe, nom(terminal), nom(lead_width), z_exit);
            }
            let right = Edge { axis: 0, sign: 1.0 };
            let tw = nom(tab_width);
            if tab_protrusion.is_some() {
                let th = (h * 0.22).clamp(0.3, 0.6);
                out.push(lead_box(right, (hx - 0.5, tab_end), (-tw / 2.0, tw / 2.0), (0.0, th), TIN));
            } else {
                gull_lead(out, right, 0.0, hx, tab_end, nom(tab_terminal), tw, z_exit);
            }
        }
        PackageSpec::Sod { lead, span, body_length, body_width, terminal, lead_width, height } => {
            let (hx, hy, h) = (nom(body_length) / 2.0, nom(body_width) / 2.0, mm(*height));
            let toe = nom(span) / 2.0;
            let w = nom(lead_width);
            match lead {
                SodLead::GullWing => {
                    out.push(cuboid([-hx, -hy, 0.08], [hx, hy, h], MOLD));
                    for s in [-1.0, 1.0] {
                        let e = Edge { axis: 0, sign: s };
                        gull_lead(out, e, 0.0, hx, toe, nom(terminal), w, h * 0.4);
                    }
                }
                SodLead::Flat => {
                    out.push(cuboid([-hx, -hy, 0.0], [hx, hy, h], MOLD));
                    for s in [-1.0, 1.0] {
                        out.push(cuboid([s * (hx - 0.1), -w / 2.0, 0.0], [s * toe, w / 2.0, 0.12], TIN));
                    }
                }
            }
            band(out, -hx * 0.75, -hx * 0.45, hy, h, BAND);
        }
        PackageSpec::Molded { kind, length, body_length, width, lead_width, height, .. } => {
            let (hx, hy, h) = (nom(body_length) / 2.0, nom(width) / 2.0, mm(*height));
            let end = (nom(length) / 2.0).max(hx + 0.12);
            out.push(cuboid([-hx, -hy, 0.05], [hx, hy, h], MOLD));
            let w = nom(lead_width).min(2.0 * hy);
            for s in [-1.0, 1.0] {
                out.push(cuboid([s * (hx - 0.3), -w / 2.0, 0.0], [s * end, w / 2.0, h * 0.55], TIN));
            }
            let c = if *kind == ChipKind::Capacitor { rgb(0x9a7a3a) } else { BAND };
            band(out, -hx * 0.8, -hx * 0.55, hy, h, c);
        }
        PackageSpec::Melf { kind, length, diameter, terminal } => {
            let (hl, r, term) = (nom(length) / 2.0, nom(diameter) / 2.0, nom(terminal));
            let body = match kind {
                ChipKind::Resistor => rgb(0x3c6ea8),
                ChipKind::Capacitor => rgb(0xb3976a),
                _ => rgb(0x9b3b3b),
            };
            out.push(cylinder_x(-hl + term, hl - term, r, r, body));
            for s in [-1.0, 1.0] {
                out.push(cylinder_x(s * hl, s * (hl - term), r * 1.03, r, TIN));
            }
            if matches!(kind, ChipKind::Diode | ChipKind::Led) {
                let x0 = -hl + term + 0.1;
                out.push(cylinder_x(x0, x0 + (hl - term) * 0.3, r * 1.01, r, rgb(0x161616)));
            }
        }
        PackageSpec::Dip { body_width, body_length, height, .. } => {
            let (hx, hy, h) = (nom(body_width) / 2.0, nom(body_length) / 2.0, mm(*height));
            let stand = 0.5_f64.min(h * 0.3);
            out.push(cuboid([-hx, -hy, stand], [hx, hy, h], MOLD));
            let z_exit = stand + (h - stand) * 0.45;
            for p in smd_pads(fp) {
                let (x, y) = (mm(p.at.x), mm(p.at.y));
                let e = Edge { axis: 0, sign: if x < 0.0 { -1.0 } else { 1.0 } };
                let u = x.abs();
                out.push(lead_box(e, (hx - 0.05, u + 0.13), (y - 0.75, y + 0.75), (z_exit - 0.25, z_exit), TIN));
                out.push(lead_box(e, (u - 0.13, u + 0.13), (y - 0.75, y + 0.75), (0.5, z_exit), TIN));
                out.push(lead_box(e, (u - 0.13, u + 0.13), (y - 0.25, y + 0.25), (-t - 1.0, 0.5), TIN));
            }
            pin1_dot(out, fp, "1", hx, hy, h);
            // Notch at the pin-1 end.
            out.push(prism(&circle((0.0, hy - 0.01), (hx * 0.18).min(0.8), 16), h, h + 0.02, rgb(0x141416)));
        }
        PackageSpec::Bga { ball, body_x, body_y, height, .. } => {
            let (hx, hy, h) = (nom(body_x) / 2.0, nom(body_y) / 2.0, mm(*height));
            let s = (nom(ball) * 0.6).min(h * 0.3);
            let sub = (s + 0.35).min(h * 0.6);
            out.push(cuboid([-hx, -hy, s], [hx, hy, sub], rgb(0x4b5e34)));
            let m = (hx.min(hy) * 0.04).min(0.3);
            out.push(cuboid([-hx + m, -hy + m, sub], [hx - m, hy - m, h], MOLD));
            pin1_dot(out, fp, "A1", hx - m, hy - m, h);
        }
        PackageSpec::PinHeader { pitch, height, .. } => {
            let pads: Vec<(f64, f64)> = smd_pads(fp).map(|p| (mm(p.at.x), mm(p.at.y))).collect();
            if pads.is_empty() {
                return;
            }
            let half = mm(*pitch) / 2.0;
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for &(x, y) in &pads {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
            let h = mm(*height);
            out.push(cuboid([x0 - half, y0 - half, 0.0], [x1 + half, y1 + half, h], PLASTIC));
            let pin = 0.32;
            for (x, y) in pads {
                out.push(cuboid([x - pin, y - pin, -t - 3.0], [x + pin, y + pin, h + 6.0], GOLD));
            }
        }
    }
}

/// DFN/QFN: body on the board, terminals showing at the body edges.
fn no_lead(out: &mut Vec<Solid>, fp: &Footprint, hx: f64, hy: f64, term: f64, width: f64, h: f64) {
    out.push(cuboid([-hx, -hy, 0.0], [hx, hy, h], MOLD));
    let th = (h * 0.25).min(0.2);
    for p in smd_pads(fp) {
        if let Some((e, at)) = pad_edge(p, hx, hy) {
            let half = if e.axis == 0 { hx } else { hy };
            out.push(lead_box(e, (half - term, half + 0.03), (at - width / 2.0, at + width / 2.0), (0.0, th), TIN));
        }
    }
    pin1_dot(out, fp, "1", hx, hy, h);
}

/// No generator spec: a box from the footprint body, or from the courtyard (1 mm high).
fn fallback(out: &mut Vec<Solid>, fp: &Footprint) {
    if let Some(b) = &fp.body {
        let (hx, hy) = (mm(b.width) / 2.0, mm(b.length) / 2.0);
        out.push(cuboid([-hx, -hy, 0.0], [hx, hy, mm(b.height).max(0.1)], MOLD));
        if fp.pads.len() >= 5 {
            pin1_dot(out, fp, "1", hx, hy, mm(b.height).max(0.1));
        }
        return;
    }
    let pts: Vec<(f64, f64)> = if fp.courtyard.is_empty() {
        fp.pads.iter().map(|p| (mm(p.at.x), mm(p.at.y))).collect()
    } else {
        fp.courtyard.iter().map(|q| (mm(q.x), mm(q.y))).collect()
    };
    let Some((x0, y0, x1, y1)) = pts.iter().fold(None, |b: Option<(f64, f64, f64, f64)>, &(x, y)| {
        Some(b.map_or((x, y, x, y), |b| (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y))))
    }) else {
        return;
    };
    let inset = 0.25_f64.min((x1 - x0).min(y1 - y0) / 4.0);
    if x1 - x0 > 2.0 * inset && y1 - y0 > 2.0 * inset {
        out.push(cuboid([x0 + inset, y0 + inset, 0.0], [x1 - inset, y1 - inset, 1.0], UNKNOWN));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::landpattern::{GenOptions, generate, packages};

    fn fp(name: &str) -> Footprint {
        generate(&packages::parse(name, ChipKind::Resistor).unwrap(), &GenOptions::default()).unwrap()
    }

    fn extent(s: &[Solid]) -> [f64; 6] {
        let mut e = [f64::MAX, f64::MAX, f64::MAX, f64::MIN, f64::MIN, f64::MIN];
        for v in s.iter().flat_map(|s| s.faces.iter().flatten()) {
            for i in 0..3 {
                e[i] = e[i].min(v[i]);
                e[i + 3] = e[i + 3].max(v[i]);
            }
        }
        e
    }

    #[test]
    fn bodies_follow_package_dimensions() {
        // SOIC-8: 8 leads of 3 boxes, a body and a pin-1 dot; toe-to-toe span ~6 mm.
        let s = solids(&fp("SOIC-8"), 1.6);
        assert_eq!(s.len(), 1 + 8 * 3 + 1);
        let e = extent(&s);
        assert!((e[3] - e[0] - 6.0).abs() < 0.3, "{e:?}");
        assert!(e[5] <= 1.8 && e[2] >= 0.0, "{e:?}");
        // 0402 resistor: body and two terminations, 1 x 0.5 mm.
        let s = solids(&fp("0402"), 1.6);
        assert_eq!(s.len(), 3);
        let e = extent(&s);
        assert!((e[3] - e[0] - 1.0).abs() < 0.1, "{e:?}");
        // LQFP-48: 48 leads on four sides.
        let s = solids(&fp("LQFP-48"), 1.6);
        assert_eq!(s.len(), 1 + 48 * 3 + 1);
        // Header pins go through the board.
        let s = solids(&fp("PinHeader 1x03"), 1.6);
        assert!(extent(&s)[2] < -1.6);
    }
}
