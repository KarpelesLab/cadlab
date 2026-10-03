//! SVG writer: millimeter units, Y flipped to SVG's downward axis, fixed precision for
//! deterministic output.

use std::fmt::Write as _;

use super::{Prim, Scene, View};

fn n(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

/// Writes the scene as an SVG document sized in millimeters.
pub fn to_svg(scene: &Scene, view: &View) -> String {
    let (x0, y0, x1, y1) = view.area;
    let (w, h) = (x1 - x0, y1 - y0);
    // Scene Y is up; SVG Y is down: y' = y1 - y.
    let px = |x: f64| n(x - x0);
    let py = |y: f64| n(y1 - y);
    let pts = |v: &[(f64, f64)]| v.iter().map(|&(x, y)| format!("{},{}", px(x), py(y))).collect::<Vec<_>>().join(" ");
    let mut s = String::new();
    let _ = writeln!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}mm" height="{}mm" viewBox="0 0 {} {}">"#,
        n(w),
        n(h),
        n(w),
        n(h)
    );
    let _ = writeln!(s, r#"<rect width="100%" height="100%" fill="{}"/>"#, scene.background.css());
    s += "<g stroke-linecap=\"round\" stroke-linejoin=\"round\">\n";
    for p in &scene.prims {
        match p {
            Prim::Line { pts: v, width, color, closed } => {
                let tag = if *closed { "polygon" } else { "polyline" };
                let _ = writeln!(
                    s,
                    r#"<{tag} points="{}" fill="none" stroke="{}" stroke-width="{}"/>"#,
                    pts(v),
                    color.css(),
                    n(*width)
                );
            }
            Prim::Polygon { pts: v, fill, stroke } => {
                let st = stroke
                    .map(|(w, c)| format!(r#" stroke="{}" stroke-width="{}""#, c.css(), n(w)))
                    .unwrap_or_default();
                let _ = writeln!(s, r#"<polygon points="{}" fill="{}"{st}/>"#, pts(v), fill.css());
            }
            Prim::Circle { c, r, fill, stroke } => {
                let f = fill.map_or("none".to_string(), |c| c.css());
                let st = stroke
                    .map(|(w, c)| format!(r#" stroke="{}" stroke-width="{}""#, c.css(), n(w)))
                    .unwrap_or_default();
                let _ = writeln!(s, r#"<circle cx="{}" cy="{}" r="{}" fill="{f}"{st}/>"#, px(c.0), py(c.1), n(*r));
            }
        }
    }
    s += "</g>\n</svg>\n";
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Color;

    #[test]
    fn writes_flipped_coordinates() {
        let mut sc = Scene::default();
        sc.line(vec![(0.0, 0.0), (10.0, 5.0)], 0.25, Color::hex(0x112233));
        let v = View { area: (0.0, 0.0, 10.0, 5.0), px_per_mm: 1.0 };
        let s = to_svg(&sc, &v);
        assert!(
            s.contains(r##"<polyline points="0,5 10,0" fill="none" stroke="#112233" stroke-width="0.25"/>"##),
            "{s}"
        );
        assert!(s.starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10mm" height="5mm""#));
    }
}
