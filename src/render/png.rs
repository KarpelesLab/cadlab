//! PNG rasterizer (tiny-skia), drawing the same primitives as the SVG writer.

use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

use super::{Color, Prim, Scene, View};

fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.0, c.1, c.2, c.3);
    p.anti_alias = true;
    p
}

/// Renders the scene to PNG bytes.
pub fn to_png(scene: &Scene, view: &View) -> Result<Vec<u8>, String> {
    let (w, h) = view.pixels();
    if w as u64 * h as u64 > 400_000_000 {
        return Err(format!("image too large ({w} x {h} px); lower the resolution"));
    }
    let mut pm = Pixmap::new(w, h).ok_or("cannot allocate image")?;
    let bg = scene.background;
    pm.fill(tiny_skia::Color::from_rgba8(bg.0, bg.1, bg.2, bg.3));
    let (x0, _, _, y1) = view.area;
    let k = view.px_per_mm as f32;
    // mm, Y up -> pixels, Y down.
    let tf = Transform::from_row(k, 0.0, 0.0, -k, -(x0 as f32) * k, (y1 as f32) * k);
    let path = |pts: &[(f64, f64)], closed: bool| {
        let mut b = PathBuilder::new();
        let mut it = pts.iter();
        let &(x, y) = it.next()?;
        b.move_to(x as f32, y as f32);
        for &(x, y) in it {
            b.line_to(x as f32, y as f32);
        }
        if closed {
            b.close();
        }
        b.finish()
    };
    let stroke =
        |w: f64| Stroke { width: w as f32, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
    for p in &scene.prims {
        match p {
            Prim::Line { pts, width, color, closed } => {
                if pts.len() == 1 || pts.windows(2).all(|w| w[0] == w[1]) {
                    // A dot.
                    if let Some(c) = PathBuilder::from_circle(pts[0].0 as f32, pts[0].1 as f32, (*width / 2.0) as f32) {
                        pm.fill_path(&c, &paint(*color), FillRule::Winding, tf, None);
                    }
                } else if let Some(pa) = path(pts, *closed) {
                    pm.stroke_path(&pa, &paint(*color), &stroke(*width), tf, None);
                }
            }
            Prim::Polygon { pts, fill, stroke: st } => {
                if let Some(pa) = path(pts, true) {
                    pm.fill_path(&pa, &paint(*fill), FillRule::Winding, tf, None);
                    if let Some((w, c)) = st {
                        pm.stroke_path(&pa, &paint(*c), &stroke(*w), tf, None);
                    }
                }
            }
            Prim::Circle { c, r, fill, stroke: st } => {
                if let Some(pa) = PathBuilder::from_circle(c.0 as f32, c.1 as f32, *r as f32) {
                    if let Some(f) = fill {
                        pm.fill_path(&pa, &paint(*f), FillRule::Winding, tf, None);
                    }
                    if let Some((w, col)) = st {
                        pm.stroke_path(&pa, &paint(*col), &stroke(*w), tf, None);
                    }
                }
            }
        }
    }
    pm.encode_png().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_pixels() {
        let mut s = Scene::default();
        s.fill(vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)], Color::hex(0xff0000), None);
        let v = View { area: (0.0, 0.0, 20.0, 10.0), px_per_mm: 2.0 };
        let png = to_png(&s, &v).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let pm = Pixmap::decode_png(&png).unwrap();
        assert_eq!((pm.width(), pm.height()), (40, 20));
        let at = |x: u32, y: u32| pm.pixel(x, y).unwrap();
        assert_eq!((at(5, 10).red(), at(5, 10).green()), (255, 0), "left half red");
        assert_eq!(at(35, 10).green(), 255, "right half white");
    }
}
