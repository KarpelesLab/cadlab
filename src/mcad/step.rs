//! STEP export: ISO 10303-21 clear text encoding, schema AP214 (`AUTOMOTIVE_DESIGN`).
//!
//! The file is an assembly product named after the project. It holds one instance of the board
//! part and one instance of a body part per placed, populated component (bodies are shared per
//! footprint: one part, several `NEXT_ASSEMBLY_USAGE_OCCURRENCE`s named after designators).
//!
//! Geometry is exact boundary representation (`MANIFOLD_SOLID_BREP` in an
//! `ADVANCED_BREP_SHAPE_REPRESENTATION`):
//!
//! - Board: the outline extruded from Z = 0 to the stackup thickness. Straight edges give
//!   planar side faces, arcs give cylindrical faces; outline cutouts and drilled holes are inner
//!   loops of the top and bottom faces with their own side faces. Holes that would touch the
//!   edge, a cutout or another hole are left out (reported by the caller).
//! - Bodies: the component's attached 3D model, or a box `width × length × height` centered
//!   on the footprint origin, standing on the board's top face, or hanging from its bottom face
//!   (bottom side: the part is turned over, which for a centered box equals cadlab's mirror).
//!   A model (a triangle mesh from oxideav-mesh3d, see [`crate::models3d`]) is written as a
//!   faceted B-rep: welded vertices, one planar `FACE_SURFACE` bounded by a `POLY_LOOP` per
//!   triangle. Each connected closed and consistently wound piece becomes a `FACETED_BREP`
//!   (a `CLOSED_SHELL`, turned outward) in a `FACETED_BREP_SHAPE_REPRESENTATION`; a model with
//!   any open or inconsistently wound piece is written as a surface model
//!   (`SHELL_BASED_SURFACE_MODEL` in a `MANIFOLD_SURFACE_SHAPE_REPRESENTATION`) instead.
//!   Colors: the most common color on the piece, plus face styles where faces differ.
//!
//! Units are millimeters (`SI_UNIT(.MILLI.,.METRE.)`), angles radians. The header has a fixed
//! time stamp so that output is byte-identical across runs.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::{DrillHole, Edge, Loop, Options, board_profile, body_set, cuttable_holes, drill_holes, radius};
use crate::fabout::gerber::mm;
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::BoardSide;
use crate::models3d::{Facets, ModelError3d};
use crate::units::Nm;

/// Result of a STEP export.
#[derive(Clone, Debug)]
pub struct StepOut {
    /// The file content.
    pub content: String,
    /// Holes that were not cut (crossing the edge, a cutout or another hole).
    pub skipped_holes: Vec<DrillHole>,
    /// Components without a package body (not modelled).
    pub no_body: Vec<String>,
    /// Components whose attached 3D model could not be read (their package box is used).
    pub model_errors: Vec<(String, ModelError3d)>,
    /// Bodies written from 3D models.
    pub model_bodies: usize,
    /// Model files written as open surfaces (not closed solids), by file name.
    pub open_models: Vec<String>,
    /// Bodies written.
    pub bodies: usize,
    /// Holes cut in the board solid.
    pub holes: usize,
}

/// Fixed header time stamp (deterministic output).
pub const TIME_STAMP: &str = "1970-01-01T00:00:00";

/// A STEP string literal (ISO 10303-21 7.3.3): `'` doubled, `\` doubled, non-ASCII as `\X2\`.
fn s(text: &str) -> String {
    let mut out = String::from("'");
    for c in text.chars() {
        match c {
            '\'' => out.push_str("''"),
            '\\' => out.push_str("\\\\"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                out.push_str("\\X2\\");
                for u in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "{u:04X}");
                }
                out.push_str("\\X0\\");
            }
        }
    }
    out.push('\'');
    out
}

/// A real from nanometers, in mm, exact.
fn nm(v: i64) -> String {
    let t = mm(v);
    if t.contains('.') { t } else { format!("{t}.") }
}

/// A real from a float (mm or unitless), 10 decimals, trailing zeros trimmed.
fn real(v: f64) -> String {
    let t = format!("{v:.10}");
    let t = t.trim_end_matches('0');
    let t = if t == "-." || t == "." { "0." } else { t };
    if t == "-0." { "0.".to_string() } else { t.to_string() }
}

/// nm (float) to a real in mm.
fn nmf(v: f64) -> String {
    real(v / 1e6)
}

/// Entity writer: numbers entities in order.
struct W {
    out: String,
    n: usize,
}

impl W {
    fn add(&mut self, e: impl AsRef<str>) -> usize {
        self.n += 1;
        let _ = writeln!(self.out, "#{}={};", self.n, e.as_ref());
        self.n
    }

    fn point(&mut self, x: String, y: String, z: String) -> usize {
        self.add(format!("CARTESIAN_POINT('',({x},{y},{z}))"))
    }

    fn dir(&mut self, x: String, y: String, z: String) -> usize {
        self.add(format!("DIRECTION('',({x},{y},{z}))"))
    }

    /// An axis placement at `(x, y, z)` with axis and reference directions.
    fn axis(
        &mut self,
        at: (String, String, String),
        axis: (String, String, String),
        refd: (String, String, String),
    ) -> usize {
        let p = self.point(at.0, at.1, at.2);
        let a = self.dir(axis.0, axis.1, axis.2);
        let r = self.dir(refd.0, refd.1, refd.2);
        self.add(format!("AXIS2_PLACEMENT_3D('',#{p},#{a},#{r})"))
    }

    fn origin(&mut self) -> usize {
        let z = || ("0.".to_string(), "0.".to_string(), "0.".to_string());
        self.axis(z(), ("0.".into(), "0.".into(), "1.".into()), ("1.".into(), "0.".into(), "0.".into()))
    }
}

fn t3(a: &str, b: &str, c: &str) -> (String, String, String) {
    (a.to_string(), b.to_string(), c.to_string())
}

/// Writes a solid: `loops` (first outer, counter-clockwise; others inner, clockwise) extruded
/// from `z0` to `z1` (nm). Returns the `MANIFOLD_SOLID_BREP` id.
fn extrude(w: &mut W, name: &str, loops: &[Loop], z0: i64, z1: i64) -> usize {
    let (zb, zt) = (nm(z0), nm(z1));
    let mut side_faces = Vec::new();
    let mut top_bounds = Vec::new();
    let mut bottom_bounds = Vec::new();
    for (li, lp) in loops.iter().enumerate() {
        let k = lp.edges.len();
        // Vertices, bottom and top.
        let mut vb = Vec::with_capacity(k);
        let mut vt = Vec::with_capacity(k);
        for i in 0..k {
            let v = lp.vertex(i);
            let pb = w.point(nm(v.x.0), nm(v.y.0), zb.clone());
            vb.push(w.add(format!("VERTEX_POINT('',#{pb})")));
            let pt = w.point(nm(v.x.0), nm(v.y.0), zt.clone());
            vt.push(w.add(format!("VERTEX_POINT('',#{pt})")));
        }
        // Vertical edges.
        let up = w.dir("0.".into(), "0.".into(), "1.".into());
        let mut vert = Vec::with_capacity(k);
        for i in 0..k {
            let v = lp.vertex(i);
            let p = w.point(nm(v.x.0), nm(v.y.0), zb.clone());
            let vec = w.add(format!("VECTOR('',#{up},{})", nm(z1 - z0)));
            let line = w.add(format!("LINE('',#{p},#{vec})"));
            vert.push(w.add(format!("EDGE_CURVE('',#{},#{},#{line},.T.)", vb[i], vt[i])));
        }
        // Horizontal edges at z, and the side surface of each edge.
        let horiz = |w: &mut W, z: &str, vs: &[usize]| -> Vec<usize> {
            (0..k)
                .map(|i| {
                    let a = lp.vertex(i);
                    let j = (i + 1) % k;
                    match lp.edges[i] {
                        Edge::Line { to } => {
                            let p = w.point(nm(a.x.0), nm(a.y.0), z.to_string());
                            let (dx, dy) = ((to.x.0 - a.x.0) as f64, (to.y.0 - a.y.0) as f64);
                            let len = (dx * dx + dy * dy).sqrt();
                            let d = w.dir(real(dx / len), real(dy / len), "0.".into());
                            let vec = w.add(format!("VECTOR('',#{d},{})", nmf(len)));
                            let line = w.add(format!("LINE('',#{p},#{vec})"));
                            w.add(format!("EDGE_CURVE('',#{},#{},#{line},.T.)", vs[i], vs[j]))
                        }
                        Edge::Arc { center, ccw, .. } => {
                            let r = radius(a, center);
                            let ax = w.axis(
                                (nmf(center.0), nmf(center.1), z.to_string()),
                                t3("0.", "0.", "1."),
                                t3("1.", "0.", "0."),
                            );
                            let c = w.add(format!("CIRCLE('',#{ax},{})", nmf(r)));
                            let sense = if ccw { ".T." } else { ".F." };
                            w.add(format!("EDGE_CURVE('',#{},#{},#{c},{sense})", vs[i], vs[j]))
                        }
                    }
                })
                .collect()
        };
        let eb = horiz(w, &zb, &vb);
        let et = horiz(w, &zt, &vt);
        for i in 0..k {
            let j = (i + 1) % k;
            let a = lp.vertex(i);
            let (surface, sense) = match lp.edges[i] {
                Edge::Line { to } => {
                    let (dx, dy) = ((to.x.0 - a.x.0) as f64, (to.y.0 - a.y.0) as f64);
                    let len = (dx * dx + dy * dy).sqrt();
                    // Material on the left of the travel direction: the outward normal is on the right.
                    let ax = w.axis(
                        (nm(a.x.0), nm(a.y.0), zb.clone()),
                        (real(dy / len), real(-dx / len), "0.".into()),
                        (real(dx / len), real(dy / len), "0.".into()),
                    );
                    (w.add(format!("PLANE('',#{ax})")), ".T.")
                }
                Edge::Arc { center, ccw, .. } => {
                    let r = radius(a, center);
                    let ax =
                        w.axis((nmf(center.0), nmf(center.1), zb.clone()), t3("0.", "0.", "1."), t3("1.", "0.", "0."));
                    // Counter-clockwise: material inside the cylinder (its normal points out).
                    (w.add(format!("CYLINDRICAL_SURFACE('',#{ax},{})", nmf(r))), if ccw { ".T." } else { ".F." })
                }
            };
            let o1 = w.add(format!("ORIENTED_EDGE('',*,*,#{},.T.)", eb[i]));
            let o2 = w.add(format!("ORIENTED_EDGE('',*,*,#{},.T.)", vert[j]));
            let o3 = w.add(format!("ORIENTED_EDGE('',*,*,#{},.F.)", et[i]));
            let o4 = w.add(format!("ORIENTED_EDGE('',*,*,#{},.F.)", vert[i]));
            let el = w.add(format!("EDGE_LOOP('',(#{o1},#{o2},#{o3},#{o4}))"));
            let b = w.add(format!("FACE_OUTER_BOUND('',#{el},.T.)"));
            side_faces.push(w.add(format!("ADVANCED_FACE('',(#{b}),#{surface},{sense})")));
        }
        let bound = if li == 0 { "FACE_OUTER_BOUND" } else { "FACE_BOUND" };
        let ots: Vec<String> =
            et.iter().map(|e| format!("#{}", w.add(format!("ORIENTED_EDGE('',*,*,#{e},.T.)")))).collect();
        let tl = w.add(format!("EDGE_LOOP('',({}))", ots.join(",")));
        top_bounds.push(w.add(format!("{bound}('',#{tl},.T.)")));
        let obs: Vec<String> =
            eb.iter().rev().map(|e| format!("#{}", w.add(format!("ORIENTED_EDGE('',*,*,#{e},.F.)")))).collect();
        let bl = w.add(format!("EDGE_LOOP('',({}))", obs.join(",")));
        bottom_bounds.push(w.add(format!("{bound}('',#{bl},.T.)")));
    }
    let tp = w.axis(("0.".into(), "0.".into(), zt.clone()), t3("0.", "0.", "1."), t3("1.", "0.", "0."));
    let tpl = w.add(format!("PLANE('',#{tp})"));
    let refs = |v: &[usize]| v.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(",");
    let top = w.add(format!("ADVANCED_FACE('',({}),#{tpl},.T.)", refs(&top_bounds)));
    let bp = w.axis(("0.".into(), "0.".into(), zb.clone()), t3("0.", "0.", "-1."), t3("1.", "0.", "0."));
    let bpl = w.add(format!("PLANE('',#{bp})"));
    let bottom = w.add(format!("ADVANCED_FACE('',({}),#{bpl},.T.)", refs(&bottom_bounds)));
    let mut faces = vec![top, bottom];
    faces.extend(side_faces);
    let shell = w.add(format!("CLOSED_SHELL('',({}))", refs(&faces)));
    w.add(format!("MANIFOLD_SOLID_BREP({},#{shell})", s(name)))
}

/// Shared context entities.
struct Ctx {
    product: usize,
    design: usize,
    geo: usize,
}

/// A part product with a B-rep shape; returns (product definition, shape representation,
/// origin placement in it).
fn part(w: &mut W, c: &Ctx, name: &str, brep: usize, color: (f64, f64, f64)) -> (usize, usize, usize) {
    part_rep(w, c, name, "ADVANCED_BREP_SHAPE_REPRESENTATION", &[brep], &[(brep, color)])
}

/// A part product whose shape representation (of type `kind`) holds `items`, with a color per
/// styled item.
fn part_rep(
    w: &mut W,
    c: &Ctx,
    name: &str,
    kind: &str,
    items: &[usize],
    styles: &[(usize, (f64, f64, f64))],
) -> (usize, usize, usize) {
    let pd = product(w, c, name);
    let pds = w.add(format!("PRODUCT_DEFINITION_SHAPE('','',#{pd})"));
    let o = w.origin();
    let list: Vec<String> = std::iter::once(o).chain(items.iter().copied()).map(|i| format!("#{i}")).collect();
    let rep = w.add(format!("{kind}({},({}),#{})", s(name), list.join(","), c.geo));
    w.add(format!("SHAPE_DEFINITION_REPRESENTATION(#{pds},#{rep})"));
    let mut psas: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    let mut styled = Vec::new();
    for &(item, color) in styles {
        let key = (real(color.0), real(color.1), real(color.2));
        let psa = match psas.get(&key) {
            Some(&psa) => psa,
            None => {
                let col = w.add(format!("COLOUR_RGB('',{},{},{})", key.0, key.1, key.2));
                let fc = w.add(format!("FILL_AREA_STYLE_COLOUR('',#{col})"));
                let fa = w.add(format!("FILL_AREA_STYLE('',(#{fc}))"));
                let ss = w.add(format!("SURFACE_STYLE_FILL_AREA(#{fa})"));
                let side = w.add(format!("SURFACE_SIDE_STYLE('',(#{ss}))"));
                let usage = w.add(format!("SURFACE_STYLE_USAGE(.BOTH.,#{side})"));
                let psa = w.add(format!("PRESENTATION_STYLE_ASSIGNMENT((#{usage}))"));
                psas.insert(key, psa);
                psa
            }
        };
        styled.push(format!("#{}", w.add(format!("STYLED_ITEM('color',(#{psa}),#{item})"))));
    }
    if !styled.is_empty() {
        w.add(format!("MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION_REPRESENTATION('',({}),#{})", styled.join(","), c.geo));
    }
    (pd, rep, o)
}

/// An RGB color (0..1) and its text form, the key that identifies it in the file.
type Rgb3 = (f64, f64, f64);
type ColorKey = (String, String, String);

/// A model written as faceted geometry: the representation type, its items and their styles.
struct Faceted {
    kind: &'static str,
    items: Vec<usize>,
    styles: Vec<(usize, (f64, f64, f64))>,
    closed: bool,
}

/// A color rounded for the file (sRGB, 4 decimals).
fn color3(c: [f32; 3]) -> (f64, f64, f64) {
    let r = |v: f32| (v as f64 * 1e4).round() / 1e4;
    (r(c[0]), r(c[1]), r(c[2]))
}

/// Writes a model's triangles (footprint coordinates, mm) as faceted B-rep pieces.
fn faceted(w: &mut W, name: &str, f: &Facets) -> Faceted {
    // Weld vertices on the nanometer grid; drop triangles that collapse.
    let mut index: BTreeMap<[i64; 3], u32> = BTreeMap::new();
    let mut verts: Vec<[i64; 3]> = Vec::new();
    let mut tris: Vec<([u32; 3], (f64, f64, f64))> = Vec::new();
    for t in &f.tris {
        let ids = t.v.map(|v| {
            let k = v.map(|x| (x * 1e6).round() as i64);
            *index.entry(k).or_insert_with(|| {
                verts.push(k);
                (verts.len() - 1) as u32
            })
        });
        if ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2] {
            tris.push((ids, color3(t.color)));
        }
    }
    // Connected pieces (triangles sharing an edge).
    let mut parent: Vec<usize> = (0..tris.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut by_edge: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for (ti, (t, _)) in tris.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let key = (a.min(b), a.max(b));
            match by_edge.get(&key) {
                Some(&other) => {
                    let (x, y) = (find(&mut parent, ti), find(&mut parent, other));
                    if x != y {
                        parent[x.max(y)] = x.min(y);
                    }
                }
                None => {
                    by_edge.insert(key, ti);
                }
            }
        }
    }
    let mut pieces: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for ti in 0..tris.len() {
        let r = find(&mut parent, ti);
        pieces.entry(r).or_default().push(ti);
    }
    // Closed: every directed edge once, and its reverse once. Closed pieces are turned outward.
    let mut pieces: Vec<(Vec<usize>, bool)> = pieces
        .into_values()
        .map(|ts| {
            let mut directed: BTreeMap<(u32, u32), u32> = BTreeMap::new();
            for &ti in &ts {
                let t = tris[ti].0;
                for k in 0..3 {
                    *directed.entry((t[k], t[(k + 1) % 3])).or_default() += 1;
                }
            }
            let closed = directed.iter().all(|(&(a, b), &n)| n == 1 && directed.get(&(b, a)) == Some(&1));
            (ts, closed)
        })
        .collect();
    let fv = |i: u32| verts[i as usize].map(|x| x as f64);
    for (ts, closed) in &pieces {
        if !closed {
            continue;
        }
        let vol: f64 = ts
            .iter()
            .map(|&ti| {
                let [a, b, c] = tris[ti].0.map(fv);
                a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                    + a[2] * (b[0] * c[1] - b[1] * c[0])
            })
            .sum();
        if vol < 0.0 {
            for &ti in ts {
                tris[ti].0.swap(1, 2);
            }
        }
    }
    let all_closed = pieces.iter().all(|(_, c)| *c);
    pieces.sort_by_key(|(ts, _)| ts[0]);

    let mut items = Vec::new();
    let mut styles = Vec::new();
    let mut shells = Vec::new();
    for (ts, closed) in &pieces {
        // Points of this piece, in first-use order.
        let mut pts: BTreeMap<u32, usize> = BTreeMap::new();
        let mut faces = Vec::new();
        let mut face_colors = Vec::new();
        for &ti in ts {
            let (t, color) = tris[ti];
            let ids = t.map(|v| {
                *pts.entry(v).or_insert_with(|| {
                    let k = verts[v as usize];
                    w.point(nm(k[0]), nm(k[1]), nm(k[2]))
                })
            });
            let [a, b, c] = t.map(fv);
            let (u, v) = (sub(b, a), sub(c, a));
            let n = unit(cross(u, v));
            let r = unit(u);
            let nd = w.dir(real(n[0]), real(n[1]), real(n[2]));
            let rd = w.dir(real(r[0]), real(r[1]), real(r[2]));
            let ax = w.add(format!("AXIS2_PLACEMENT_3D('',#{},#{nd},#{rd})", ids[0]));
            let plane = w.add(format!("PLANE('',#{ax})"));
            let lp = w.add(format!("POLY_LOOP('',(#{},#{},#{}))", ids[0], ids[1], ids[2]));
            let bound = w.add(format!("FACE_OUTER_BOUND('',#{lp},.T.)"));
            let face = w.add(format!("FACE_SURFACE('',(#{bound}),#{plane},.T.)"));
            faces.push(format!("#{face}"));
            face_colors.push((face, color));
        }
        let shell_kind = if *closed { "CLOSED_SHELL" } else { "OPEN_SHELL" };
        let shell = w.add(format!("{shell_kind}('',({}))", faces.join(",")));
        // Dominant color (most faces; ties: the smallest color).
        let mut counts: BTreeMap<ColorKey, (usize, Rgb3)> = BTreeMap::new();
        for (_, c) in &face_colors {
            counts.entry((real(c.0), real(c.1), real(c.2))).or_insert((0, *c)).0 += 1;
        }
        let dominant = counts.values().fold(None::<(usize, (f64, f64, f64))>, |best, &(n, c)| match best {
            Some((bn, _)) if bn >= n => best,
            _ => Some((n, c)),
        });
        let dominant = dominant.map_or((0.62, 0.62, 0.64), |(_, c)| c);
        let item = if all_closed {
            let brep = w.add(format!("FACETED_BREP({},#{shell})", s(name)));
            items.push(brep);
            brep
        } else {
            shells.push(shell);
            shell
        };
        styles.push((item, dominant));
        for (face, c) in face_colors {
            if real(c.0) != real(dominant.0) || real(c.1) != real(dominant.1) || real(c.2) != real(dominant.2) {
                styles.push((face, c));
            }
        }
    }
    if all_closed {
        Faceted { kind: "FACETED_BREP_SHAPE_REPRESENTATION", items, styles, closed: true }
    } else {
        let list: Vec<String> = shells.iter().map(|i| format!("#{i}")).collect();
        let sbsm = w.add(format!("SHELL_BASED_SURFACE_MODEL({},({}))", s(name), list.join(",")));
        // Piece styles were attached to shells; style the surface model as a whole too.
        let first = styles.first().map(|&(_, c)| c).unwrap_or((0.62, 0.62, 0.64));
        let mut st = vec![(sbsm, first)];
        st.extend(styles.into_iter().filter(|(i, _)| !shells.contains(i)));
        Faceted { kind: "MANIFOLD_SURFACE_SHAPE_REPRESENTATION", items: vec![sbsm], styles: st, closed: false }
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn unit(a: [f64; 3]) -> [f64; 3] {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    if l > 0.0 { a.map(|v| v / l) } else { [0.0, 0.0, 1.0] }
}

fn product(w: &mut W, c: &Ctx, name: &str) -> usize {
    let prod = w.add(format!("PRODUCT({0},{0},'',(#{1}))", s(name), c.product));
    w.add(format!("PRODUCT_RELATED_PRODUCT_CATEGORY('part',$,(#{prod}))"));
    let pdf = w.add(format!("PRODUCT_DEFINITION_FORMATION('','',#{prod})"));
    w.add(format!("PRODUCT_DEFINITION('design','',#{pdf},#{})", c.design))
}

/// Places a part instance in the assembly.
fn instance(w: &mut W, asm_pd: usize, asm_rep: usize, part: (usize, usize, usize), place: usize, name: &str) {
    let (pd, rep, origin) = part;
    let nauo = w.add(format!("NEXT_ASSEMBLY_USAGE_OCCURRENCE({0},{0},'',#{asm_pd},#{pd},$)", s(name)));
    let pds = w.add(format!("PRODUCT_DEFINITION_SHAPE('','',#{nauo})"));
    let idt = w.add(format!("ITEM_DEFINED_TRANSFORMATION('','',#{origin},#{place})"));
    let rr = w.add(format!(
        "(REPRESENTATION_RELATIONSHIP('','',#{rep},#{asm_rep})REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#{idt})SHAPE_REPRESENTATION_RELATIONSHIP())"
    ));
    w.add(format!("CONTEXT_DEPENDENT_SHAPE_REPRESENTATION(#{rr},#{pds})"));
}

/// A body part: footprint name, box size (nm) and the model it comes from.
type BodyKey = (String, i64, i64, i64, Option<crate::model::model3d::Model3d>);
/// A part's (product definition, shape representation, origin placement) entity ids.
type PartIds = (usize, usize, usize);

/// Board solid color (green) and body color (dark gray).
const BOARD_COLOR: (f64, f64, f64) = (0.0, 0.4, 0.15);
const BODY_COLOR: (f64, f64, f64) = (0.2, 0.2, 0.2);

/// The board (and component bodies) as a STEP AP214 assembly. `None` without a board outline.
pub fn export(p: &Project, o: &Options) -> Option<StepOut> {
    let (outer, cutouts) = board_profile(p)?;
    let name = p.manifest().name.clone();
    let thickness = p.board().stackup.thickness.0.max(1);
    let (holes, skipped) = cuttable_holes(&outer, &cutouts, drill_holes(p, o.vias));
    let set = if o.components { body_set(p) } else { Default::default() };
    let (bodies, no_body, model_errors) = (&set.bodies, set.no_body.clone(), set.model_errors.clone());

    let mut w = W { out: String::new(), n: 0 };
    let app = w.add("APPLICATION_CONTEXT('core data for automotive mechanical design processes')");
    w.add(format!("APPLICATION_PROTOCOL_DEFINITION('international standard','automotive_design',2000,#{app})"));
    let product_ctx = w.add(format!("PRODUCT_CONTEXT('',#{app},'mechanical')"));
    let design = w.add(format!("PRODUCT_DEFINITION_CONTEXT('part definition',#{app},'design')"));
    let len = w.add("(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.))");
    let ang = w.add("(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.))");
    let sol = w.add("(NAMED_UNIT(*)SI_UNIT($,.STERADIAN.)SOLID_ANGLE_UNIT())");
    let unc = w.add(format!(
        "UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-06),#{len},'distance_accuracy_value','confusion accuracy')"
    ));
    let geo = w.add(format!(
        "(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{unc}))GLOBAL_UNIT_ASSIGNED_CONTEXT((#{len},#{ang},#{sol}))REPRESENTATION_CONTEXT('3D','3D context with units and uncertainty'))"
    ));
    let c = Ctx { product: product_ctx, design, geo };

    // Assembly product.
    let asm_pd = product(&mut w, &c, &name);
    let asm_pds = w.add(format!("PRODUCT_DEFINITION_SHAPE('','',#{asm_pd})"));

    // Board part.
    let mut loops = vec![outer];
    loops.extend(cutouts);
    loops.extend(holes.iter().map(|h| Loop::circle(h.at, h.diameter, false)));
    let board_name = format!("{name}_board");
    let brep = extrude(&mut w, &board_name, &loops, 0, thickness);
    let board_part = part(&mut w, &c, &board_name, brep, BOARD_COLOR);

    // Body parts, one per footprint and size.
    let mut parts: BTreeMap<BodyKey, PartIds> = BTreeMap::new();
    let mut open_models = Vec::new();
    let key_of = |b: &super::BodyBox| (b.footprint.clone(), b.width.0, b.length.0, b.height.0, b.model.clone());
    for b in bodies {
        let key = key_of(b);
        if parts.contains_key(&key) {
            continue;
        }
        if let Some(m) = &b.model {
            let facets = &set.models[m];
            let pname = format!("{}_{}", b.footprint, m.file);
            let fc = faceted(&mut w, &pname, facets);
            if !fc.closed && !open_models.contains(&m.file) {
                open_models.push(m.file.clone());
            }
            let pr = part_rep(&mut w, &c, &pname, fc.kind, &fc.items, &fc.styles);
            parts.insert(key, pr);
            continue;
        }
        let (hw, hl) = (b.width.0 / 2, b.length.0 / 2);
        let pt = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
        let rect = Loop {
            start: pt(-hw, -hl),
            edges: vec![
                Edge::Line { to: pt(b.width.0 - hw, -hl) },
                Edge::Line { to: pt(b.width.0 - hw, b.length.0 - hl) },
                Edge::Line { to: pt(-hw, b.length.0 - hl) },
                Edge::Line { to: pt(-hw, -hl) },
            ],
        };
        let brep = extrude(&mut w, &b.footprint, &[rect], 0, b.height.0);
        let pr = part(&mut w, &c, &b.footprint, brep, BODY_COLOR);
        parts.insert(key, pr);
    }

    // Assembly representation: identity for the board, a placement per body.
    let asm_origin = w.origin();
    let board_place = w.origin();
    let mut places = Vec::new();
    for b in bodies {
        let (sin, cos) = match b.rotation.quarter_turns() {
            Some(q) => [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)][q as usize],
            None => b.rotation.to_rad_f64().sin_cos(),
        };
        let place = match b.side {
            BoardSide::Top => w.axis(
                (nm(b.at.x.0), nm(b.at.y.0), nm(thickness)),
                t3("0.", "0.", "1."),
                (real(cos), real(sin), "0.".into()),
            ),
            // Turned over (x → -x, z → -z), then rotated: the image of X is -(cos, sin).
            BoardSide::Bottom => w.axis(
                (nm(b.at.x.0), nm(b.at.y.0), "0.".into()),
                t3("0.", "0.", "-1."),
                (real(-cos), real(-sin), "0.".into()),
            ),
        };
        places.push(place);
    }
    let mut items = vec![format!("#{asm_origin}"), format!("#{board_place}")];
    items.extend(places.iter().map(|p| format!("#{p}")));
    let asm_rep = w.add(format!("SHAPE_REPRESENTATION({},({}),#{geo})", s(&name), items.join(",")));
    w.add(format!("SHAPE_DEFINITION_REPRESENTATION(#{asm_pds},#{asm_rep})"));
    instance(&mut w, asm_pd, asm_rep, board_part, board_place, &board_name);
    for (b, place) in bodies.iter().zip(&places) {
        let pr = parts[&key_of(b)];
        instance(&mut w, asm_pd, asm_rep, pr, *place, &b.refdes);
    }

    let mut out = String::new();
    out.push_str("ISO-10303-21;\nHEADER;\n");
    let _ = writeln!(out, "FILE_DESCRIPTION(({}),'2;1');", s(&format!("{name}: board and component bodies")));
    let _ = writeln!(
        out,
        "FILE_NAME({},'{TIME_STAMP}',(''),(''),{},{},'');",
        s(&format!("{name}.step")),
        s(&format!("cadlab {}", o.version)),
        s("cadlab")
    );
    out.push_str("FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\nENDSEC;\nDATA;\n");
    out.push_str(&w.out);
    out.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    let model_bodies = bodies.iter().filter(|b| b.model.is_some()).count();
    Some(StepOut {
        content: out,
        skipped_holes: skipped,
        no_body,
        model_errors,
        model_bodies,
        open_models,
        bodies: bodies.len(),
        holes: holes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        assert_eq!(s("it's"), "'it''s'");
        assert_eq!(s("Ω"), "'\\X2\\03A9\\X0\\'");
        assert_eq!(nm(1_500_000), "1.5");
        assert_eq!(nm(-2_000_000), "-2.");
        assert_eq!(real(-0.0), "0.");
        assert_eq!(real(1.0), "1.");
        assert_eq!(real(0.123456789012), "0.123456789");
    }

    #[test]
    fn box_topology() {
        let mut w = W { out: String::new(), n: 0 };
        let pt = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
        let rect = Loop {
            start: pt(0, 0),
            edges: vec![
                Edge::Line { to: pt(1_000_000, 0) },
                Edge::Line { to: pt(1_000_000, 1_000_000) },
                Edge::Line { to: pt(0, 1_000_000) },
                Edge::Line { to: pt(0, 0) },
            ],
        };
        extrude(&mut w, "b", &[rect, Loop::circle(pt(500_000, 500_000), Nm(200_000), false)], 0, 1_000_000);
        let count = |k: &str| w.out.lines().filter(|l| l.contains(&format!("={k}("))).count();
        assert_eq!(count("ADVANCED_FACE"), 2 + 4 + 1);
        assert_eq!(count("EDGE_CURVE"), 3 * 4 + 3);
        assert_eq!(count("VERTEX_POINT"), 2 * 5);
        assert_eq!(count("CYLINDRICAL_SURFACE"), 1);
    }
}
