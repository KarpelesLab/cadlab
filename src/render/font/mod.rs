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

/// Distance between the baselines of two lines of a text, in cap heights (KiCad's, as its
/// plots give it: imported texts keep their lines, DECISIONS D83).
pub const LINE_PITCH: f64 = 1.609944;
/// Slant of italic text: how far a point moves to the right per unit above the baseline.
pub const ITALIC_SLANT: f64 = 0.125;

/// A text block as polylines around its anchor at the origin (X to the right, Y up): lines
/// separated by `\n`, capitals `height` high, characters scaled to the nominal width `width`
/// (equal to `height`: the font's own proportions), each line placed by `h`, the block by `v`
/// (`Top`: the capitals of the first line end at the anchor; `Bottom`: the last line's baseline
/// is on it; `Middle`: halfway between the two), slanted when `italic`.
pub fn layout_block(text: &str, height: f64, width: f64, h: HAlign, v: VAlign, italic: bool) -> Vec<Vec<(f64, f64)>> {
    let k = height / CAP_HEIGHT;
    let kx = width / CAP_HEIGHT;
    let lines: Vec<&str> = text.split('\n').collect();
    let pitch = LINE_PITCH * height;
    let below = (lines.len() - 1) as f64 * pitch;
    // Baseline of the first line.
    let y0 = match v {
        VAlign::Top => -height,
        VAlign::Middle => (below - height) / 2.0,
        VAlign::Bottom => below,
    };
    let mut out = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let w: f64 = line.chars().map(|c| glyph(c).right - glyph(c).left).sum::<f64>() * kx;
        let dx = match h {
            HAlign::Left => 0.0,
            HAlign::Center => -w / 2.0,
            HAlign::Right => -w,
        };
        let base = y0 - n as f64 * pitch;
        let mut pen = 0.0;
        for c in line.chars() {
            let g = glyph(c);
            for s in &g.strokes {
                out.push(
                    s.iter()
                        .map(|&(x, y)| {
                            let up = (BASELINE - y) * k;
                            let slant = if italic { up * ITALIC_SLANT * (kx / k) } else { 0.0 };
                            (dx + pen + (x - g.left) * kx + slant, base + up)
                        })
                        .collect(),
                );
            }
            pen += (g.right - g.left) * kx;
        }
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

    #[test]
    fn blocks() {
        let ext = |l: &Vec<Vec<(f64, f64)>>| {
            let (xs, ys): (Vec<f64>, Vec<f64>) = l.iter().flatten().copied().unzip();
            let f = |v: &[f64], m: fn(f64, f64) -> f64, z: f64| v.iter().copied().fold(z, m);
            (
                f(&xs, f64::min, f64::MAX),
                f(&xs, f64::max, f64::MIN),
                f(&ys, f64::min, f64::MAX),
                f(&ys, f64::max, f64::MIN),
            )
        };
        // One centered line is `layout`'s.
        let a = layout_block("Hi", 2.1, 2.1, HAlign::Center, VAlign::Middle, false);
        let b = layout("Hi", (0.0, 0.0), 2.1, HAlign::Center, VAlign::Middle, 0);
        assert_eq!(a.len(), b.len());
        for (p, q) in a.iter().flatten().zip(b.iter().flatten()) {
            assert!((p.0 - q.0).abs() < 1e-9 && (p.1 - q.1).abs() < 1e-9);
        }
        // Left starts at the anchor, right ends there; top hangs below, bottom stands on it.
        let (x0, _, y0, y1) = ext(&layout_block("H", 2.0, 2.0, HAlign::Left, VAlign::Bottom, false));
        assert!(x0 >= 0.0 && y0.abs() < 1e-9 && (y1 - 2.0).abs() < 1e-9);
        let (_, x1, y0, y1) = ext(&layout_block("H", 2.0, 2.0, HAlign::Right, VAlign::Top, false));
        assert!(x1 <= 0.0 && (y0 + 2.0).abs() < 1e-9 && y1.abs() < 1e-9);
        // Two lines: one pitch apart, the block centered.
        let (_, _, y0, y1) = ext(&layout_block("H\nH", 1.0, 1.0, HAlign::Center, VAlign::Middle, false));
        assert!((y1 - y0 - (1.0 + LINE_PITCH)).abs() < 1e-9 && (y0 + y1).abs() < 1e-9);
        // Twice as wide; italic leans to the right.
        let (a0, a1, ..) = ext(&layout_block("H", 1.0, 1.0, HAlign::Left, VAlign::Bottom, false));
        let (b0, b1, ..) = ext(&layout_block("H", 1.0, 2.0, HAlign::Left, VAlign::Bottom, false));
        assert!(((b1 - b0) - 2.0 * (a1 - a0)).abs() < 1e-9);
        let (_, c1, ..) = ext(&layout_block("H", 1.0, 1.0, HAlign::Left, VAlign::Bottom, true));
        assert!((c1 - a1 - ITALIC_SLANT).abs() < 1e-9);
    }
}
