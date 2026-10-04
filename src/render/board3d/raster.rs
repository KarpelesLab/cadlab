//! Z-buffer triangle rasterizer with supersampling.
//!
//! Triangles arrive in pixel coordinates with a depth per vertex (larger = closer). The image
//! is cut into horizontal bands rendered independently (in parallel); each band keeps an
//! `S × S` supersampled color and depth buffer and is box-filtered down. Triangles are drawn
//! in input order and a sample is replaced only by a strictly closer one, so the result does
//! not depend on the thread count.

use super::bodies::Rgb;

/// Samples per pixel along each axis.
pub(crate) const SS: usize = 3;
/// Output rows per band.
const BAND: usize = 32;

/// A board-face texture: color and coverage (outline minus holes), in board millimeters.
pub(crate) struct Texture {
    /// Premultiplied RGBA8 (opaque).
    pub rgba: Vec<u8>,
    /// Coverage, 0..255.
    pub mask: Vec<u8>,
    pub w: usize,
    pub h: usize,
    /// Left edge (board X, mm) and top edge (board Y, mm).
    pub x0: f64,
    pub y1: f64,
    /// Pixels per millimeter.
    pub k: f64,
}

impl Texture {
    /// Bilinear color at a board point, `None` where the board is not (outside, holes).
    fn sample(&self, x: f64, y: f64) -> Option<Rgb> {
        let u = (x - self.x0) * self.k - 0.5;
        let v = (self.y1 - y) * self.k - 0.5;
        let (fu, fv) = (u.floor(), v.floor());
        let (ax, ay) = ((u - fu) as f32, (v - fv) as f32);
        let clamp = |i: f64, n: usize| (i.max(0.0) as usize).min(n - 1);
        let (i0, j0) = (clamp(fu, self.w), clamp(fv, self.h));
        let (i1, j1) = (clamp(fu + 1.0, self.w), clamp(fv + 1.0, self.h));
        let wts = [(1.0 - ax) * (1.0 - ay), ax * (1.0 - ay), (1.0 - ax) * ay, ax * ay];
        let idx = [j0 * self.w + i0, j0 * self.w + i1, j1 * self.w + i0, j1 * self.w + i1];
        let cover: f32 = idx.iter().zip(wts).map(|(&i, w)| self.mask[i] as f32 * w).sum();
        if cover < 127.5 {
            return None;
        }
        let mut c = [0.0f32; 3];
        for (&i, w) in idx.iter().zip(wts) {
            for (k, ck) in c.iter_mut().enumerate() {
                *ck += self.rgba[i * 4 + k] as f32 * w;
            }
        }
        Some(c.map(|v| v / 255.0))
    }
}

/// How a triangle is colored.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Fill {
    /// Flat color, already shaded.
    Flat(Rgb),
    /// Texture `index`, multiplied by `shade`; vertex board coordinates in `uv`.
    Tex { index: usize, shade: f32, uv: [[f64; 2]; 3] },
}

/// A projected triangle: pixel coordinates and depth per vertex.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PTri {
    pub p: [[f64; 2]; 3],
    pub depth: [f64; 3],
    pub fill: Fill,
}

/// Background color of an output row (vertical gradient).
pub(crate) fn background(row: f64, height: f64) -> Rgb {
    let t = (row / height.max(1.0)) as f32;
    let (a, b) = (super::bodies::rgb(0x3d424a), super::bodies::rgb(0x1c1e22));
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Rasterizes the triangles into a `w × h` RGBA8 image.
pub(crate) fn rasterize(tris: &[PTri], textures: &[Texture], w: usize, h: usize) -> Vec<u8> {
    let bands = h.div_ceil(BAND);
    // Bin triangles by band (input order kept within each bin).
    let mut bins: Vec<Vec<u32>> = vec![Vec::new(); bands];
    for (i, t) in tris.iter().enumerate() {
        let y0 = t.p.iter().map(|p| p[1]).fold(f64::MAX, f64::min);
        let y1 = t.p.iter().map(|p| p[1]).fold(f64::MIN, f64::max);
        if !(y0.is_finite() && y1.is_finite()) || y1 < 0.0 || y0 > h as f64 {
            continue;
        }
        let b0 = (y0.max(0.0) as usize / BAND).min(bands - 1);
        let b1 = (y1.max(0.0) as usize / BAND).min(bands - 1);
        for bin in &mut bins[b0..=b1] {
            bin.push(i as u32);
        }
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, bands.max(1));
    let mut out = vec![0u8; w * h * 4];
    let results: Vec<Vec<(usize, Vec<u8>)>> = std::thread::scope(|sc| {
        let jobs: Vec<_> = (0..threads)
            .map(|t| {
                let bins = &bins;
                sc.spawn(move || {
                    (t..bands)
                        .step_by(threads)
                        .map(|b| (b, band(tris, textures, &bins[b], w, h, b * BAND, ((b + 1) * BAND).min(h))))
                        .collect()
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().expect("raster thread")).collect()
    });
    for (b, px) in results.into_iter().flatten() {
        let start = b * BAND * w * 4;
        out[start..start + px.len()].copy_from_slice(&px);
    }
    out
}

/// Renders output rows `r0..r1`.
fn band(tris: &[PTri], textures: &[Texture], bin: &[u32], w: usize, h: usize, r0: usize, r1: usize) -> Vec<u8> {
    let sw = w * SS;
    let sh = (r1 - r0) * SS;
    let mut color: Vec<Rgb> = Vec::with_capacity(sw * sh);
    for j in 0..sh {
        let bg = background((r0 * SS + j) as f64 / SS as f64, h as f64);
        color.extend(std::iter::repeat_n(bg, sw));
    }
    let mut depth = vec![f64::NEG_INFINITY; sw * sh];
    let k = SS as f64;
    let sy0 = (r0 * SS) as f64;
    for &ti in bin {
        let t = &tris[ti as usize];
        // Sample space: sample (i, j) of the band is at ((i + 0.5) / S, r0 + (j + 0.5) / S).
        let p = t.p.map(|q| [q[0] * k, q[1] * k - sy0]);
        let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
        if area.abs() < 1e-9 {
            continue;
        }
        let minx = p.iter().map(|q| q[0]).fold(f64::MAX, f64::min);
        let maxx = p.iter().map(|q| q[0]).fold(f64::MIN, f64::max);
        let miny = p.iter().map(|q| q[1]).fold(f64::MAX, f64::min);
        let maxy = p.iter().map(|q| q[1]).fold(f64::MIN, f64::max);
        let i0 = (minx - 0.5).ceil().max(0.0) as usize;
        let i1 = ((maxx - 0.5).floor().min(sw as f64 - 1.0)).max(-1.0);
        let j0 = (miny - 0.5).ceil().max(0.0) as usize;
        let j1 = ((maxy - 0.5).floor().min(sh as f64 - 1.0)).max(-1.0);
        if i1 < 0.0 || j1 < 0.0 {
            continue;
        }
        let (i1, j1) = (i1 as usize, j1 as usize);
        let inv = 1.0 / area;
        let eps = 1e-9;
        for j in j0..=j1 {
            let y = j as f64 + 0.5;
            for i in i0..=i1 {
                let x = i as f64 + 0.5;
                let w0 = ((p[2][0] - p[1][0]) * (y - p[1][1]) - (p[2][1] - p[1][1]) * (x - p[1][0])) * inv;
                let w1 = ((p[0][0] - p[2][0]) * (y - p[2][1]) - (p[0][1] - p[2][1]) * (x - p[2][0])) * inv;
                let w2 = 1.0 - w0 - w1;
                if w0 < -eps || w1 < -eps || w2 < -eps {
                    continue;
                }
                let d = w0 * t.depth[0] + w1 * t.depth[1] + w2 * t.depth[2];
                let at = j * sw + i;
                if d <= depth[at] {
                    continue;
                }
                let c = match t.fill {
                    Fill::Flat(c) => c,
                    Fill::Tex { index, shade, uv } => {
                        let u = w0 * uv[0][0] + w1 * uv[1][0] + w2 * uv[2][0];
                        let v = w0 * uv[0][1] + w1 * uv[1][1] + w2 * uv[2][1];
                        match textures[index].sample(u, v) {
                            Some(c) => c.map(|x| x * shade),
                            None => continue,
                        }
                    }
                };
                depth[at] = d;
                color[at] = c;
            }
        }
    }
    // Box filter down to output pixels.
    let n = (SS * SS) as f32;
    let mut px = Vec::with_capacity(w * (r1 - r0) * 4);
    for row in 0..r1 - r0 {
        for col in 0..w {
            let mut acc = [0.0f32; 3];
            for dj in 0..SS {
                let base = (row * SS + dj) * sw + col * SS;
                for c in &color[base..base + SS] {
                    acc[0] += c[0];
                    acc[1] += c[1];
                    acc[2] += c[2];
                }
            }
            for a in acc {
                px.push(((a / n).clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            px.push(255);
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearer_triangle_wins_regardless_of_order() {
        let red = Fill::Flat([1.0, 0.0, 0.0]);
        let blue = Fill::Flat([0.0, 0.0, 1.0]);
        let tri = |d: f64, fill| PTri { p: [[0.0, 0.0], [40.0, 0.0], [0.0, 40.0]], depth: [d; 3], fill };
        for order in [[tri(1.0, red), tri(2.0, blue)], [tri(2.0, blue), tri(1.0, red)]] {
            let img = rasterize(&order, &[], 40, 40);
            let at = |x: usize, y: usize| &img[(y * 40 + x) * 4..(y * 40 + x) * 4 + 3];
            assert_eq!(at(5, 5), &[0, 0, 255], "blue is closer");
            // Outside: background; on the diagonal edge: blended.
            assert_ne!(at(35, 35), &[0, 0, 255]);
        }
    }
}
