//! Stroke font (Hershey Simplex Roman, see `NOTICE`): text becomes polylines, so SVG and PNG
//! output look identical on every machine and need no font files.

use std::sync::OnceLock;

const DATA: &str = include_str!("futural.jhf");

/// Height of capital letters in font units.
const CAP_HEIGHT: f64 = 21.0;
/// Baseline in font units (Hershey Y grows downward).
const BASELINE: f64 = 9.0;

struct Glyph {
    left: f64,
    right: f64,
    strokes: Vec<Vec<(f64, f64)>>,
}

fn glyphs() -> &'static Vec<Glyph> {
    static G: OnceLock<Vec<Glyph>> = OnceLock::new();
    G.get_or_init(|| {
        DATA.lines()
            .filter(|l| l.len() >= 10)
            .map(|l| {
                let b = l.as_bytes();
                let coord = |c: u8| c as f64 - b'R' as f64;
                let (left, right) = (coord(b[8]), coord(b[9]));
                let mut strokes = vec![Vec::new()];
                for pair in b[10..].chunks(2) {
                    if pair.len() < 2 {
                        break;
                    }
                    if pair == b" R" {
                        strokes.push(Vec::new());
                    } else {
                        strokes.last_mut().expect("non-empty").push((coord(pair[0]), coord(pair[1])));
                    }
                }
                strokes.retain(|s| !s.is_empty());
                Glyph { left, right, strokes }
            })
            .collect()
    })
}

fn glyph(c: char) -> &'static Glyph {
    let g = glyphs();
    let c = match c {
        'µ' | 'μ' => 'u',
        'Ω' => 'R',
        '°' => 'o',
        '±' => '+',
        c => c,
    };
    let i = if (' '..='~').contains(&c) { c as usize - 32 } else { '?' as usize - 32 };
    &g[i.min(g.len() - 1)]
}

/// Horizontal alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HAlign {
    /// Text starts at the anchor.
    Left,
    /// Text is centered on the anchor.
    Center,
    /// Text ends at the anchor.
    Right,
}

/// Vertical alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VAlign {
    /// Anchor at the top of capitals.
    Top,
    /// Anchor at mid-height of capitals.
    Middle,
    /// Anchor on the baseline.
    Bottom,
}

/// Width of `text` at cap height `size` (same units).
pub fn width(text: &str, size: f64) -> f64 {
    let k = size / CAP_HEIGHT;
    text.chars().map(|c| glyph(c).right - glyph(c).left).sum::<f64>() * k
}

/// Text as polylines: cap height `size`, anchored at `at` (Y up), rotated by `quarter_turns` × 90°
/// counter-clockwise around the anchor.
pub fn layout(text: &str, at: (f64, f64), size: f64, h: HAlign, v: VAlign, quarter_turns: u8) -> Vec<Vec<(f64, f64)>> {
    let k = size / CAP_HEIGHT;
    let w = width(text, size);
    let dx = match h {
        HAlign::Left => 0.0,
        HAlign::Center => -w / 2.0,
        HAlign::Right => -w,
    };
    let dy = match v {
        VAlign::Top => -size,
        VAlign::Middle => -size / 2.0,
        VAlign::Bottom => 0.0,
    };
    let rot = |x: f64, y: f64| match quarter_turns % 4 {
        0 => (x, y),
        1 => (-y, x),
        2 => (-x, -y),
        _ => (y, -x),
    };
    let mut out = Vec::new();
    let mut pen = 0.0;
    for c in text.chars() {
        let g = glyph(c);
        for s in &g.strokes {
            out.push(
                s.iter()
                    .map(|&(x, y)| {
                        let lx = dx + pen + (x - g.left) * k;
                        let ly = dy + (BASELINE - y) * k;
                        let (rx, ry) = rot(lx, ly);
                        (at.0 + rx, at.1 + ry)
                    })
                    .collect(),
            );
        }
        pen += (g.right - g.left) * k;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_glyphs() {
        assert_eq!(glyphs().len(), 96);
        assert!(glyph('A').strokes.len() >= 3);
        assert!(glyph(' ').strokes.is_empty());
        // Capital H spans the cap height exactly.
        let lines = layout("H", (0.0, 0.0), 2.1, HAlign::Left, VAlign::Bottom, 0);
        let ys: Vec<f64> = lines.iter().flatten().map(|p| p.1).collect();
        let (lo, hi) = (ys.iter().cloned().fold(f64::MAX, f64::min), ys.iter().cloned().fold(f64::MIN, f64::max));
        assert!((lo - 0.0).abs() < 1e-9 && (hi - 2.1).abs() < 1e-9, "{lo} {hi}");
        assert!(width("HH", 2.1) > width("H", 2.1));
        assert_eq!(glyph('µ') as *const _, glyph('u') as *const _);
    }
}
