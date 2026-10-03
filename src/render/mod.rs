//! Rendering: a [`Scene`] of 2D primitives (millimeters, Y up) written to SVG or PNG.
//!
//! Text is converted to strokes with an embedded stroke font when added to the scene, so both
//! outputs come from exactly the same geometry. See `docs/RENDERING.md`.

pub mod board;
pub mod font;
#[cfg(feature = "png")]
mod png;
mod svg;

pub use font::{HAlign, VAlign};
#[cfg(feature = "png")]
pub use png::to_png;
pub use svg::to_svg;

/// An RGBA color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);

impl Color {
    /// Opaque color from `0xRRGGBB`.
    pub const fn hex(rgb: u32) -> Color {
        Color((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255)
    }

    /// Same color with alpha.
    pub const fn alpha(self, a: u8) -> Color {
        Color(self.0, self.1, self.2, a)
    }

    pub(crate) fn css(self) -> String {
        if self.3 == 255 {
            format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
        } else {
            format!("rgba({},{},{},{:.3})", self.0, self.1, self.2, self.3 as f64 / 255.0)
        }
    }
}

/// A drawing primitive. Coordinates in millimeters, Y up.
#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    /// Stroked polyline (closed if `closed`).
    Line {
        /// Vertices.
        pts: Vec<(f64, f64)>,
        /// Stroke width.
        width: f64,
        /// Color.
        color: Color,
        /// Close the path.
        closed: bool,
    },
    /// Filled polygon, optionally outlined.
    Polygon {
        /// Vertices.
        pts: Vec<(f64, f64)>,
        /// Fill.
        fill: Color,
        /// Outline width and color.
        stroke: Option<(f64, Color)>,
    },
    /// Circle.
    Circle {
        /// Center.
        c: (f64, f64),
        /// Radius.
        r: f64,
        /// Fill.
        fill: Option<Color>,
        /// Outline width and color.
        stroke: Option<(f64, Color)>,
    },
    /// Filled region of several closed rings (outer boundaries and holes), even-odd rule.
    Region {
        /// Rings.
        rings: Vec<Vec<(f64, f64)>>,
        /// Fill.
        fill: Color,
    },
}

/// A scene to render.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    /// Background.
    pub background: Color,
    /// Primitives, drawn in order.
    pub prims: Vec<Prim>,
}

impl Default for Scene {
    fn default() -> Self {
        Scene { background: Color::hex(0xffffff), prims: Vec::new() }
    }
}

impl Scene {
    /// A stroked line or polyline.
    pub fn line(&mut self, pts: Vec<(f64, f64)>, width: f64, color: Color) {
        self.prims.push(Prim::Line { pts, width, color, closed: false });
    }

    /// A stroked closed outline.
    pub fn outline(&mut self, pts: Vec<(f64, f64)>, width: f64, color: Color) {
        self.prims.push(Prim::Line { pts, width, color, closed: true });
    }

    /// A filled polygon.
    pub fn fill(&mut self, pts: Vec<(f64, f64)>, fill: Color, stroke: Option<(f64, Color)>) {
        self.prims.push(Prim::Polygon { pts, fill, stroke });
    }

    /// A filled region made of several rings (even-odd: holes are rings inside outer rings).
    pub fn region(&mut self, rings: Vec<Vec<(f64, f64)>>, fill: Color) {
        self.prims.push(Prim::Region { rings, fill });
    }

    /// A circle.
    pub fn circle(&mut self, c: (f64, f64), r: f64, fill: Option<Color>, stroke: Option<(f64, Color)>) {
        self.prims.push(Prim::Circle { c, r, fill, stroke });
    }

    /// Text drawn with the stroke font; `size` is the capital height.
    #[allow(clippy::too_many_arguments)]
    pub fn text(&mut self, s: &str, at: (f64, f64), size: f64, h: HAlign, v: VAlign, quarter_turns: u8, color: Color) {
        let width = (size * 0.12).max(0.05);
        for stroke in font::layout(s, at, size, h, v, quarter_turns) {
            if stroke.len() >= 2 {
                self.line(stroke, width, color);
            }
        }
    }

    /// Bounding box of everything drawn, including stroke widths: (min x, min y, max x, max y).
    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut b: Option<(f64, f64, f64, f64)> = None;
        let mut add = |x: f64, y: f64, pad: f64| {
            let r = b.get_or_insert((x - pad, y - pad, x + pad, y + pad));
            r.0 = r.0.min(x - pad);
            r.1 = r.1.min(y - pad);
            r.2 = r.2.max(x + pad);
            r.3 = r.3.max(y + pad);
        };
        for p in &self.prims {
            match p {
                Prim::Line { pts, width, .. } => pts.iter().for_each(|&(x, y)| add(x, y, width / 2.0)),
                Prim::Polygon { pts, stroke, .. } => {
                    let pad = stroke.map_or(0.0, |s| s.0 / 2.0);
                    pts.iter().for_each(|&(x, y)| add(x, y, pad));
                }
                Prim::Circle { c, r, stroke, .. } => add(c.0, c.1, r + stroke.map_or(0.0, |s| s.0 / 2.0)),
                Prim::Region { rings, .. } => rings.iter().flatten().for_each(|&(x, y)| add(x, y, 0.0)),
            }
        }
        b
    }
}

/// Output size and framing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Area shown: (min x, min y, max x, max y) in mm.
    pub area: (f64, f64, f64, f64),
    /// Pixels per millimeter (PNG); SVG uses millimeter units.
    pub px_per_mm: f64,
}

impl View {
    /// The scene's bounds plus a margin, at `px_per_mm`.
    pub fn fit(scene: &Scene, margin: f64, px_per_mm: f64) -> View {
        let (x0, y0, x1, y1) = scene.bounds().unwrap_or((0.0, 0.0, 10.0, 10.0));
        View { area: (x0 - margin, y0 - margin, x1 + margin, y1 + margin), px_per_mm }
    }

    /// Pixel size, rounded up.
    pub fn pixels(&self) -> (u32, u32) {
        let (x0, y0, x1, y1) = self.area;
        (((x1 - x0) * self.px_per_mm).ceil().max(1.0) as u32, ((y1 - y0) * self.px_per_mm).ceil().max(1.0) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_and_view() {
        let mut s = Scene::default();
        s.line(vec![(0.0, 0.0), (10.0, 5.0)], 1.0, Color::hex(0));
        assert_eq!(s.bounds(), Some((-0.5, -0.5, 10.5, 5.5)));
        let v = View::fit(&s, 1.0, 10.0);
        assert_eq!(v.pixels(), (130, 80));
        assert_eq!(Color::hex(0x1a7f37).css(), "#1a7f37");
    }
}
