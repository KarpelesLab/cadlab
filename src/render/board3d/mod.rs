//! 3D view of the assembled board: an orthographic (isometric by default) software render.
//!
//! - The board is its outline extruded to the stackup thickness, with cutout and drill-hole
//!   walls; its top and bottom faces are textured with the realistic 2D render of that side
//!   ([`super::board::realistic_scene`]), and holes are see-through.
//! - Components are their attached 3D model (decoded through oxideav-mesh3d, see
//!   [`crate::models3d`]), else convex solids built from their package dimensions (see
//!   `bodies`); bottom-side parts are mirrored under the board.
//! - Hidden surfaces: a supersampled z-buffer (`raster`); flat shading with one directional
//!   light fixed relative to the camera.
//!
//! Output is deterministic: no randomness, fixed draw order, results independent of the thread
//! count. See `docs/RENDERING.md` ("3D view").

mod bodies;
mod raster;

use tiny_skia::{FillRule, IntSize, Mask, PathBuilder, Pixmap, Transform};

use crate::board as geo;
use crate::geom::poly::{ArcTol, Side};
use crate::model::Project;
use crate::model::board::BoardSide;
use crate::models3d::{self, ModelError3d};
use crate::units::{LengthUnit, Nm};

use super::View;
use bodies::{Rgb, Solid, rgb};
use raster::{Fill, PTri, Texture};

/// True isometric elevation: atan(1/√2), in degrees.
pub const ISO_ELEVATION: f64 = 35.264_389_682_754_654;

/// What to render.
#[derive(Clone, Debug, PartialEq)]
pub struct Options3d {
    /// Side facing the viewer. `Bottom` turns the board over (as the realistic bottom view,
    /// mirrored left to right).
    pub side: BoardSide,
    /// Where the viewer stands around the board, in degrees clockwise (seen from the side
    /// viewed) from the front edge (−Y): 0 = front, 45 = front-left (the default isometric
    /// view), 90 = left.
    pub azimuth: f64,
    /// Viewer height above the board plane in degrees, 5..=90 (90 = straight down).
    pub elevation: f64,
    /// Draw component bodies.
    pub components: bool,
    /// Designators drawn in the highlight color.
    pub highlight: Vec<String>,
    /// Longer image side in pixels, when `px_per_mm` is not given.
    pub size: u32,
    /// Resolution in pixels per millimeter (of the projection plane); overrides `size`.
    pub px_per_mm: Option<f64>,
}

impl Default for Options3d {
    fn default() -> Self {
        Options3d {
            side: BoardSide::Top,
            azimuth: 45.0,
            elevation: ISO_ELEVATION,
            components: true,
            highlight: Vec::new(),
            size: 1600,
            px_per_mm: None,
        }
    }
}

/// An orthographic camera mapping millimeters (board X, Y; Z up from the board's top surface)
/// to pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    right: [f64; 3],
    up: [f64; 3],
    toward: [f64; 3],
    /// Pixels per millimeter.
    scale: f64,
    /// Projected coordinates of the image's top-left corner.
    origin: (f64, f64),
}

impl Camera {
    fn new(o: &Options3d) -> Camera {
        let (sa, ca) = o.azimuth.to_radians().sin_cos();
        let (se, ce) = o.elevation.clamp(5.0, 90.0).to_radians().sin_cos();
        let flip = if o.side == BoardSide::Bottom { -1.0 } else { 1.0 };
        let toward = [-sa * ce, -ca * ce, flip * se];
        let right = [flip * ca, -flip * sa, 0.0];
        let up = cross(toward, right);
        Camera { right, up, toward, scale: 1.0, origin: (0.0, 0.0) }
    }

    fn plane(&self, p: [f64; 3]) -> (f64, f64) {
        (dot(p, self.right), dot(p, self.up))
    }

    /// Pixel position (x right, y down) of a point in millimeters (board X, Y; Z from the top
    /// surface of the board, negative below it).
    pub fn project(&self, p: [f64; 3]) -> (f64, f64) {
        let (u, v) = self.plane(p);
        ((u - self.origin.0) * self.scale, (self.origin.1 - v) * self.scale)
    }

    fn depth(&self, p: [f64; 3]) -> f64 {
        dot(p, self.toward)
    }
}

/// A rendered 3D view.
#[derive(Clone, Debug)]
pub struct Image3d {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA8 pixels, row-major, opaque.
    pub rgba: Vec<u8>,
    /// The projection used.
    pub camera: Camera,
    /// Components whose attached 3D model could not be used (drawn with a generated body).
    pub model_errors: Vec<(String, ModelError3d)>,
}

impl Image3d {
    /// RGB of a pixel.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
    }

    /// PNG bytes.
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let size = IntSize::from_wh(self.width, self.height).ok_or("empty image")?;
        let pm = Pixmap::from_vec(self.rgba.clone(), size).ok_or("bad image buffer")?;
        pm.encode_png().map_err(|e| e.to_string())
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let l = dot(a, a).sqrt();
    if l > 0.0 { a.map(|v| v / l) } else { a }
}

fn mm(v: Nm) -> f64 {
    v.to_f64(LengthUnit::Mm)
}

/// Color of a surface: flat or the board texture of one side.
#[derive(Clone, Copy, Debug)]
enum Surface {
    Flat(Rgb),
    /// Texture index; vertices carry board coordinates.
    Tex(usize),
}

/// A world-space triangle with its outward normal. `cull`: skip when facing away.
#[derive(Clone, Copy, Debug)]
struct Tri {
    v: [[f64; 3]; 3],
    n: [f64; 3],
    surface: Surface,
    cull: bool,
}

const FR4_EDGE: Rgb = rgb(0xbdb38a);
const HIGHLIGHT: Rgb = rgb(0xff8a1e);

/// Board rings in mm: the outer edge (counter-clockwise) and cutouts (clockwise). Without an
/// outline, the extent of everything placed.
fn board_rings(p: &Project) -> Vec<Vec<(f64, f64)>> {
    let mut rings: Vec<Vec<(f64, f64)>> = p
        .board()
        .outline
        .contours
        .iter()
        .map(|c| {
            geo::contour_ring(c, ArcTol::new(5_000, Side::Outside))
                .iter()
                .map(|q| (q.x as f64 / 1e6, q.y as f64 / 1e6))
                .collect::<Vec<_>>()
        })
        .filter(|r| r.len() >= 3)
        .collect();
    if rings.is_empty()
        && let Some((x0, y0, x1, y1)) = super::board::extent(p)
    {
        rings.push(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]);
    }
    for (i, r) in rings.iter_mut().enumerate() {
        let a: f64 = (0..r.len()).map(|k| r[k].0 * r[(k + 1) % r.len()].1 - r[(k + 1) % r.len()].0 * r[k].1).sum();
        if (i == 0) != (a > 0.0) {
            r.reverse();
        }
    }
    rings
}

/// Drill holes: center, diameter (mm) and plated.
fn drill_holes(p: &Project) -> Vec<((f64, f64), f64, bool)> {
    let mut v: Vec<((f64, f64), f64, bool)> = geo::placed_pads(p)
        .iter()
        .filter_map(|pp| pp.hole.map(|(d, plated)| ((mm(pp.center.x), mm(pp.center.y)), mm(d), plated)))
        .collect();
    v.extend(p.board().vias.iter().map(|vi| ((mm(vi.at.x), mm(vi.at.y)), mm(vi.drill), true)));
    v
}

fn hole_ring(c: (f64, f64), d: f64) -> Vec<(f64, f64)> {
    let n = ((d * 12.0).ceil() as usize).clamp(12, 48);
    let mut r = bodies::circle(c, d / 2.0, n);
    r.reverse(); // clockwise, like a cutout
    r
}

/// Finish color of plated hole walls, from the board preferences.
fn finish_color(p: &Project) -> Rgb {
    let f = p.board().stackup.finish.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    if f.contains("hasl") {
        rgb(0xc6cbd1)
    } else if f.contains("osp") || f.contains("bare") {
        rgb(0xc8783c)
    } else {
        rgb(0xd9b35b)
    }
}

/// Board solid: walls of every ring and hole, and the two textured faces (as rectangles over
/// the board's extent; the texture's coverage cuts out the shape).
fn board_tris(p: &Project, t: f64, rings: &[Vec<(f64, f64)>], holes: &[((f64, f64), f64, bool)]) -> Vec<Tri> {
    let mut out = Vec::new();
    let mut wall = |r: &[(f64, f64)], color: Rgb| {
        for k in 0..r.len() {
            let (a, b) = (r[k], r[(k + 1) % r.len()]);
            let n = normalize([b.1 - a.1, a.0 - b.0, 0.0]);
            let (a0, b0, a1, b1) = ([a.0, a.1, -t], [b.0, b.1, -t], [a.0, a.1, 0.0], [b.0, b.1, 0.0]);
            let s = Surface::Flat(color);
            out.push(Tri { v: [a0, b0, b1], n, surface: s, cull: true });
            out.push(Tri { v: [a0, b1, a1], n, surface: s, cull: true });
        }
    };
    for r in rings {
        wall(r, FR4_EDGE);
    }
    let plated = finish_color(p);
    for &(c, d, pl) in holes {
        wall(&hole_ring(c, d), if pl { plated } else { FR4_EDGE });
    }
    if let Some((x0, y0, x1, y1)) = bounds(rings.iter().flatten().copied()) {
        for (z, n, tex) in [(0.0, [0.0, 0.0, 1.0], 0), (-t, [0.0, 0.0, -1.0], 1)] {
            let q = [[x0, y0, z], [x1, y0, z], [x1, y1, z], [x0, y1, z]];
            let s = Surface::Tex(tex);
            out.push(Tri { v: [q[0], q[1], q[2]], n, surface: s, cull: true });
            out.push(Tri { v: [q[0], q[2], q[3]], n, surface: s, cull: true });
        }
    }
    out
}

fn bounds(pts: impl Iterator<Item = (f64, f64)>) -> Option<(f64, f64, f64, f64)> {
    pts.fold(None, |b, (x, y)| {
        Some(match b {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        })
    })
}

/// Component solids in world coordinates: the attached 3D model's triangles, else convex
/// solids generated from the package. Also returns the models that could not be used.
fn component_tris(p: &Project, t: f64, highlight: &[String]) -> (Vec<Tri>, Vec<(String, ModelError3d)>) {
    let mut out = Vec::new();
    let mut failed = Vec::new();
    let mut cache = models3d::Cache::default();
    for (refdes, pf) in &p.board().footprints {
        let Some(fp) = geo::footprint_for(p, refdes) else { continue };
        let (s, c) = match pf.rotation.quarter_turns() {
            Some(q) => [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)][q as usize],
            None => pf.rotation.to_rad_f64().sin_cos(),
        };
        let bottom = pf.side == BoardSide::Bottom;
        let (ox, oy) = (mm(pf.at.x), mm(pf.at.y));
        let place = |v: [f64; 3]| -> [f64; 3] {
            let x = if bottom { -v[0] } else { v[0] };
            let z = if bottom { -t - v[2] } else { v[2] };
            [ox + x * c - v[1] * s, oy + x * s + v[1] * c, z]
        };
        let lit = highlight.iter().any(|h| h == refdes);
        if let Some(m) = models3d::model_for(p, refdes) {
            match cache.get(p, m) {
                Ok(facets) => {
                    for f in &facets.tris {
                        let v = f.v.map(place);
                        let n = normalize(cross(sub(v[1], v[0]), sub(v[2], v[0])));
                        let color = if lit { mix(f.color, HIGHLIGHT, 0.65) } else { f.color };
                        // Meshes are drawn double-sided: their winding is not trusted.
                        out.push(Tri { v, n, surface: Surface::Flat(color), cull: false });
                    }
                    continue;
                }
                Err(e) => failed.push((refdes.clone(), e)),
            }
        }
        for solid in bodies::solids(fp, t) {
            let Solid { faces, color } = solid;
            let color = if lit { mix(color, HIGHLIGHT, 0.65) } else { color };
            let faces: Vec<Vec<[f64; 3]>> = faces.iter().map(|f| f.iter().map(|&v| place(v)).collect()).collect();
            let all: Vec<[f64; 3]> = faces.iter().flatten().copied().collect();
            let k = all.len().max(1) as f64;
            let center = all.iter().fold([0.0; 3], |a, v| [a[0] + v[0] / k, a[1] + v[1] / k, a[2] + v[2] / k]);
            for f in faces.iter().filter(|f| f.len() >= 3) {
                let mut n = normalize(cross(sub(f[1], f[0]), sub(f[2], f[0])));
                let fc = f.iter().fold([0.0; 3], |a, v| [a[0] + v[0], a[1] + v[1], a[2] + v[2]]);
                let fc = fc.map(|x| x / f.len() as f64);
                if dot(n, sub(fc, center)) < 0.0 {
                    n = n.map(|x| -x);
                }
                for i in 1..f.len() - 1 {
                    out.push(Tri { v: [f[0], f[i], f[i + 1]], n, surface: Surface::Flat(color), cull: true });
                }
            }
        }
    }
    (out, failed)
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Renders one side's realistic view as a texture covering `area` (mm), with coverage from
/// the outline rings and drill holes.
fn texture(
    p: &Project,
    side: BoardSide,
    area: (f64, f64, f64, f64),
    k: f64,
    rings: &[Vec<(f64, f64)>],
    holes: &[((f64, f64), f64, bool)],
) -> Result<Texture, String> {
    let scene = super::board::realistic_scene(p, side);
    let view = View { area, px_per_mm: k };
    let pm = super::png::to_pixmap(&scene, &view)?;
    let (w, h) = (pm.width(), pm.height());
    let mut mask = Mask::new(w, h).ok_or("cannot allocate texture mask")?;
    let mut b = PathBuilder::new();
    let ring = |b: &mut PathBuilder, r: &[(f64, f64)]| {
        b.move_to(r[0].0 as f32, r[0].1 as f32);
        for q in &r[1..] {
            b.line_to(q.0 as f32, q.1 as f32);
        }
        b.close();
    };
    for r in rings {
        ring(&mut b, r);
    }
    for &(c, d, _) in holes {
        ring(&mut b, &hole_ring(c, d));
    }
    if let Some(path) = b.finish() {
        let kf = k as f32;
        let tf = Transform::from_row(kf, 0.0, 0.0, -kf, -(area.0 as f32) * kf, (area.3 as f32) * kf);
        mask.fill_path(&path, FillRule::EvenOdd, true, tf);
    }
    Ok(Texture {
        rgba: pm.data().to_vec(),
        mask: mask.data().to_vec(),
        w: w as usize,
        h: h as usize,
        x0: area.0,
        y1: area.3,
        k,
    })
}

/// Renders the board in 3D. Errors: an empty board (no outline, nothing placed) or an image
/// too large.
pub fn render(p: &Project, o: &Options3d) -> Result<Image3d, String> {
    let t = mm(p.board().stackup.thickness).max(0.1);
    let rings = board_rings(p);
    if rings.is_empty() {
        return Err("the board has no outline and nothing placed".into());
    }
    let holes = drill_holes(p);
    let mut tris = board_tris(p, t, &rings, &holes);
    let mut model_errors = Vec::new();
    if o.components {
        let (c, failed) = component_tris(p, t, &o.highlight);
        tris.extend(c);
        model_errors = failed;
    }
    let mut cam = Camera::new(o);
    // Fit: projected extent of everything.
    let (u0, v0, u1, v1) =
        bounds(tris.iter().flat_map(|tr| tr.v.iter().map(|&q| cam.plane(q)))).ok_or("nothing to draw")?;
    let (ew, eh) = ((u1 - u0).max(1e-3), (v1 - v0).max(1e-3));
    let longest = ew.max(eh);
    let (scale, margin) = match o.px_per_mm {
        Some(k) => {
            let k = k.clamp(1.0, 400.0);
            (k, (longest * k * 0.03).max(8.0))
        }
        None => {
            let size = o.size.clamp(64, 8000) as f64;
            let margin = (size * 0.03).max(8.0);
            ((size - 2.0 * margin) / longest, margin)
        }
    };
    let (w, h) = ((ew * scale + 2.0 * margin).round() as u32, (eh * scale + 2.0 * margin).round() as u32);
    if w as u64 * h as u64 > 64_000_000 {
        return Err(format!("image too large ({w} x {h} px); lower the resolution"));
    }
    cam.scale = scale;
    cam.origin = (u0 - margin / scale, v1 + margin / scale);

    // Light from the upper left, fixed relative to the camera.
    let light = normalize({
        let (r, u, c) = (cam.right, cam.up, cam.toward);
        [
            -0.45 * r[0] + 0.6 * u[0] + 0.75 * c[0],
            -0.45 * r[1] + 0.6 * u[1] + 0.75 * c[1],
            -0.45 * r[2] + 0.6 * u[2] + 0.75 * c[2],
        ]
    });
    let shade = |n: [f64; 3]| (0.42 + 0.58 * dot(n, light).max(0.0)) as f32;

    // Textures: only of faces that can be seen.
    let board_area = bounds(rings.iter().flatten().copied()).expect("rings are not empty");
    let pad = 0.5;
    let area = (board_area.0 - pad, board_area.1 - pad, board_area.2 + pad, board_area.3 + pad);
    let longest = (area.2 - area.0).max(area.3 - area.1);
    let k = (scale * 1.5).clamp(4.0, 80.0).min(6000.0 / longest);
    let top_visible = cam.toward[2] > 0.0;
    let tex = texture(p, if top_visible { BoardSide::Top } else { BoardSide::Bottom }, area, k, &rings, &holes)?;

    let mut ptris = Vec::with_capacity(tris.len());
    for tr in &tris {
        if tr.cull && dot(tr.n, cam.toward) <= 1e-9 {
            continue;
        }
        // Double-sided faces are shaded on the side facing the viewer.
        let n = if !tr.cull && dot(tr.n, cam.toward) < 0.0 { tr.n.map(|x| -x) } else { tr.n };
        let fill = match tr.surface {
            Surface::Flat(c) => Fill::Flat(c.map(|x| x * shade(n))),
            Surface::Tex(i) => {
                if (i == 0) != top_visible {
                    continue;
                }
                Fill::Tex { index: 0, shade: shade(tr.n), uv: tr.v.map(|q| [q[0], q[1]]) }
            }
        };
        let pr = tr.v.map(|q| {
            let (x, y) = cam.project(q);
            [x, y]
        });
        ptris.push(PTri { p: pr, depth: tr.v.map(|q| cam.depth(q)), fill });
    }
    let rgba = raster::rasterize(&ptris, std::slice::from_ref(&tex), w as usize, h as usize);
    Ok(Image3d { width: w, height: h, rgba, camera: cam, model_errors })
}
