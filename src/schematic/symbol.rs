//! Symbol geometry: pin positions on the sheet, and symbol drawings.

use super::{Dir, GRID, Placement};
use crate::geom::Point;
use crate::model::part::{Part, Side, Symbol, SymbolStyle};
use crate::render::{Color, HAlign, Scene, VAlign};
use crate::symbolgen;
use crate::units::Nm;

/// Symbol colors and sizes.
pub struct Style {
    /// Outline color.
    pub body: Color,
    /// Box fill.
    pub fill: Color,
    /// Pin color.
    pub pin: Color,
    /// Text color.
    pub text: Color,
    /// Line width (mm).
    pub width: f64,
    /// Text size (mm).
    pub text_size: f64,
}

/// Default symbol style.
pub const STYLE: Style = Style {
    body: Color::hex(0x9a2a2a),
    fill: Color::hex(0xfff8e1),
    pin: Color::hex(0x9a2a2a),
    text: Color::hex(0x1f2328),
    width: 0.25,
    text_size: 1.27,
};

/// The part's symbol with pin positions (generated if the stored symbol has none).
pub fn symbol_of(part: &Part) -> Symbol {
    if part.symbol.pins.iter().all(|p| p.at.is_some() && p.side.is_some()) && !part.symbol.pins.is_empty() {
        part.symbol.clone()
    } else {
        symbolgen::generate(part.category, part.symbol.pins.clone())
    }
}

fn side_dir(s: Side) -> Dir {
    match s {
        Side::Left => Dir::Left,
        Side::Right => Dir::Right,
        Side::Top => Dir::Up,
        Side::Bottom => Dir::Down,
    }
}

/// Rotates a point by quarter turns.
pub fn rot(p: (i64, i64), q: u8) -> (i64, i64) {
    match q % 4 {
        0 => p,
        1 => (-p.1, p.0),
        2 => (-p.0, -p.1),
        _ => (p.1, -p.0),
    }
}

/// Sheet position of a symbol-local point.
pub fn place(local: Point, pl: &Placement) -> Point {
    let (x, y) = rot((local.x.0, local.y.0), pl.rot);
    Point::new(pl.at.x + Nm(x), pl.at.y + Nm(y))
}

/// Pins on the sheet: (number, connection point, outward direction).
pub fn pin_ends(sym: &Symbol, pl: &Placement) -> Vec<(String, Point, Dir)> {
    sym.pins
        .iter()
        .map(|p| {
            let at = p.at.unwrap_or(Point::ORIGIN);
            let dir = side_dir(p.side.unwrap_or(Side::Left)).rotated(pl.rot);
            (p.number.clone(), place(at, pl), dir)
        })
        .collect()
}

/// Local bounding box of the drawn symbol body and pins (mm), before rotation.
pub fn local_extent(sym: &Symbol) -> (f64, f64, f64, f64) {
    let g = GRID.to_f64(crate::units::LengthUnit::Mm);
    let (mut x0, mut y0, mut x1, mut y1) = match sym.body {
        Some((w, h)) => {
            let (w, h) = (w.to_f64(crate::units::LengthUnit::Mm) / 2.0, h.to_f64(crate::units::LengthUnit::Mm) / 2.0);
            (-w, -h, w, h)
        }
        None => (-g, -g * 0.6, g, g * 0.6),
    };
    for p in &sym.pins {
        if let Some(at) = p.at {
            let (x, y) = (at.x.to_f64(crate::units::LengthUnit::Mm), at.y.to_f64(crate::units::LengthUnit::Mm));
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
    }
    (x0, y0, x1, y1)
}

fn mm(v: Nm) -> f64 {
    v.to_f64(crate::units::LengthUnit::Mm)
}

/// Draws a symbol with its designator and value.
pub fn draw(scene: &mut Scene, sym: &Symbol, pl: &Placement, refdes: &str, value: &str) {
    let s = &STYLE;
    let t = |x: f64, y: f64| -> (f64, f64) {
        let p = place(Point::new(Nm((x * 1e6).round() as i64), Nm((y * 1e6).round() as i64)), pl);
        (mm(p.x), mm(p.y))
    };
    let tl = |pts: &[(f64, f64)]| pts.iter().map(|&(x, y)| t(x, y)).collect::<Vec<_>>();
    let g = mm(GRID);
    let w = s.width;

    match sym.style {
        SymbolStyle::Box => {
            let (bw, bh) = sym.body.map_or((4.0 * g, 4.0 * g), |(a, b)| (mm(a), mm(b)));
            let (hw, hh) = (bw / 2.0, bh / 2.0);
            scene.fill(tl(&[(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]), s.fill, Some((w, s.body)));
            for p in &sym.pins {
                let (Some(at), Some(side)) = (p.at, p.side) else {
                    continue;
                };
                let (px, py) = (mm(at.x), mm(at.y));
                let d = side_dir(side).vec();
                let len = mm(symbolgen::PIN_LENGTH);
                let inner = (px - d.0 as f64 * len, py - d.1 as f64 * len);
                scene.line(tl(&[(px, py), inner]), w, s.pin);
                // Name inside the body, number above the pin line; text stays readable.
                let name = p.label();
                let sheet_dir = side_dir(side).rotated(pl.rot);
                let inner_s = t(inner.0, inner.1);
                let mid = t((px + inner.0) / 2.0, (py + inner.1) / 2.0);
                let (ts, ns) = (s.text_size, s.text_size * 0.8);
                match sheet_dir {
                    Dir::Left => {
                        if !p.name.is_empty() {
                            scene.text(name, (inner_s.0 + 0.6, inner_s.1), ts, HAlign::Left, VAlign::Middle, 0, s.text);
                        }
                        scene.text(&p.number, (mid.0, mid.1 + 0.3), ns, HAlign::Center, VAlign::Bottom, 0, s.pin);
                    }
                    Dir::Right => {
                        if !p.name.is_empty() {
                            scene.text(
                                name,
                                (inner_s.0 - 0.6, inner_s.1),
                                ts,
                                HAlign::Right,
                                VAlign::Middle,
                                0,
                                s.text,
                            );
                        }
                        scene.text(&p.number, (mid.0, mid.1 + 0.3), ns, HAlign::Center, VAlign::Bottom, 0, s.pin);
                    }
                    Dir::Up => {
                        if !p.name.is_empty() {
                            scene.text(
                                name,
                                (inner_s.0, inner_s.1 - 0.6),
                                ts,
                                HAlign::Right,
                                VAlign::Middle,
                                1,
                                s.text,
                            );
                        }
                        scene.text(&p.number, (mid.0 - 0.3, mid.1), ns, HAlign::Center, VAlign::Bottom, 1, s.pin);
                    }
                    Dir::Down => {
                        if !p.name.is_empty() {
                            scene.text(name, (inner_s.0, inner_s.1 + 0.6), ts, HAlign::Left, VAlign::Middle, 1, s.text);
                        }
                        scene.text(&p.number, (mid.0 - 0.3, mid.1), ns, HAlign::Center, VAlign::Bottom, 1, s.pin);
                    }
                }
            }
            draw_fields(scene, sym, pl, refdes, value);
        }
        style => {
            // Two-terminal symbols: pins at ±1.5 grid on X.
            let e = 1.5 * g;
            let body = body_half(style);
            scene.line(tl(&[(-e, 0.0), (-body, 0.0)]), w, s.pin);
            scene.line(tl(&[(body, 0.0), (e, 0.0)]), w, s.pin);
            match style {
                SymbolStyle::Resistor => {
                    scene.outline(tl(&[(-2.0, -0.75), (2.0, -0.75), (2.0, 0.75), (-2.0, 0.75)]), w, s.body);
                }
                SymbolStyle::Fuse => {
                    scene.outline(tl(&[(-2.0, -0.6), (2.0, -0.6), (2.0, 0.6), (-2.0, 0.6)]), w, s.body);
                    scene.line(tl(&[(-2.0, 0.0), (2.0, 0.0)]), w, s.body);
                }
                SymbolStyle::Capacitor | SymbolStyle::CapacitorPolarized => {
                    scene.line(tl(&[(-0.5, -1.6), (-0.5, 1.6)]), w * 2.0, s.body);
                    scene.line(tl(&[(0.5, -1.6), (0.5, 1.6)]), w * 2.0, s.body);
                    if style == SymbolStyle::CapacitorPolarized {
                        scene.line(tl(&[(-1.6, 1.2), (-1.0, 1.2)]), w, s.body);
                        scene.line(tl(&[(-1.3, 0.9), (-1.3, 1.5)]), w, s.body);
                    }
                }
                SymbolStyle::Inductor => {
                    let mut pts = Vec::new();
                    for k in 0..4 {
                        let cx = -1.65 + 1.1 * k as f64;
                        for i in 0..=8 {
                            let a = std::f64::consts::PI * (1.0 - i as f64 / 8.0);
                            pts.push((cx + 0.55 * a.cos(), 0.55 * a.sin()));
                        }
                    }
                    scene.line(tl(&pts), w, s.body);
                }
                SymbolStyle::FerriteBead => {
                    scene.fill(tl(&[(-2.2, -0.7), (2.2, -0.7), (2.2, 0.7), (-2.2, 0.7)]), s.body, None);
                }
                SymbolStyle::Diode | SymbolStyle::Led => {
                    // Anode left, cathode right.
                    scene.fill(tl(&[(-1.27, -1.1), (-1.27, 1.1), (1.0, 0.0)]), s.fill, Some((w, s.body)));
                    scene.line(tl(&[(1.0, -1.1), (1.0, 1.1)]), w * 1.5, s.body);
                    scene.line(tl(&[(-1.27, 0.0), (1.0, 0.0)]), w, s.body);
                    if style == SymbolStyle::Led {
                        for dx in [-0.6, 0.4] {
                            scene.line(tl(&[(dx, 1.4), (dx + 1.0, 2.4)]), w, s.body);
                            scene.fill(tl(&[(dx + 1.0, 2.4), (dx + 0.55, 2.25), (dx + 0.85, 1.95)]), s.body, None);
                        }
                    }
                }
                SymbolStyle::Crystal => {
                    scene.outline(tl(&[(-0.6, -1.2), (0.6, -1.2), (0.6, 1.2), (-0.6, 1.2)]), w, s.body);
                    scene.line(tl(&[(-1.2, -1.5), (-1.2, 1.5)]), w * 1.5, s.body);
                    scene.line(tl(&[(1.2, -1.5), (1.2, 1.5)]), w * 1.5, s.body);
                }
                SymbolStyle::Switch => {
                    for cx in [-1.3, 1.3] {
                        scene.circle(t(cx, 0.0), 0.35, None, Some((w, s.body)));
                    }
                    scene.line(tl(&[(-1.9, 1.0), (1.9, 1.0)]), w, s.body);
                    scene.line(tl(&[(0.0, 1.0), (0.0, 2.0)]), w, s.body);
                    scene.line(tl(&[(-0.7, 2.0), (0.7, 2.0)]), w, s.body);
                }
                _ => {
                    scene.circle(t(0.0, 0.0), 1.0, None, Some((w, s.body)));
                }
            }
            draw_fields(scene, sym, pl, refdes, value);
        }
    }
}

fn draw_fields(scene: &mut Scene, sym: &Symbol, pl: &Placement, refdes: &str, value: &str) {
    let [r, v] = field_positions(sym, pl);
    let ts = STYLE.text_size;
    scene.text(refdes, r.at, ts, r.h, r.v, 0, STYLE.text);
    scene.text(value, v.at, ts, v.h, v.v, 0, STYLE.text);
}

/// Half-length of the body of a two-terminal drawing along its axis (mm); the pin lines run from
/// there to the pin ends at ±1.5 grid.
pub fn body_half(style: SymbolStyle) -> f64 {
    match style {
        SymbolStyle::Resistor | SymbolStyle::Fuse => 2.0,
        SymbolStyle::Inductor | SymbolStyle::FerriteBead => 2.2,
        SymbolStyle::Crystal => 1.2,
        SymbolStyle::Capacitor | SymbolStyle::CapacitorPolarized => 0.5,
        SymbolStyle::Switch => 1.65,
        _ => 1.27,
    }
}

/// Extent of a two-terminal drawing across its axis (mm): (below, above) in symbol coordinates.
pub fn body_across(style: SymbolStyle) -> (f64, f64) {
    match style {
        SymbolStyle::Resistor => (0.75, 0.75),
        SymbolStyle::Fuse => (0.6, 0.6),
        SymbolStyle::Capacitor | SymbolStyle::CapacitorPolarized => (1.6, 1.6),
        SymbolStyle::Inductor => (0.1, 0.6),
        SymbolStyle::FerriteBead => (0.7, 0.7),
        SymbolStyle::Diode => (1.1, 1.1),
        SymbolStyle::Led => (1.1, 2.4),
        SymbolStyle::Crystal => (1.5, 1.5),
        SymbolStyle::Switch => (0.4, 2.0),
        _ => (1.0, 1.0),
    }
}

/// Where a symbol's designator or value is drawn: anchor on the sheet (mm, Y up) and alignment.
/// Text is always horizontal on the sheet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldPos {
    /// Anchor (mm).
    pub at: (f64, f64),
    /// Horizontal alignment.
    pub h: HAlign,
    /// Vertical alignment.
    pub v: VAlign,
}

/// Positions of the designator and value of a placed symbol, in that order. Box symbols carry
/// them outside the top-right corner (clear of top pins); two-terminal symbols above and below a
/// horizontal body, or beside a vertical one on the side of the symbol's −Y.
pub fn field_positions(sym: &Symbol, pl: &Placement) -> [FieldPos; 2] {
    let ts = STYLE.text_size;
    let t = |x: f64, y: f64| -> (f64, f64) {
        let p = place(Point::new(Nm((x * 1e6).round() as i64), Nm((y * 1e6).round() as i64)), pl);
        (mm(p.x), mm(p.y))
    };
    if sym.style == SymbolStyle::Box {
        let g = mm(GRID);
        let (bw, bh) = sym.body.map_or((4.0 * g, 4.0 * g), |(a, b)| (mm(a), mm(b)));
        let (hw, hh) = (bw / 2.0, bh / 2.0);
        let (_, _, x1, y1) = sheet_box(&[t(-hw, -hh), t(hw, hh)]);
        return [
            FieldPos { at: (x1 + 0.6, y1 + 0.6 + ts * 1.6), h: HAlign::Left, v: VAlign::Bottom },
            FieldPos { at: (x1 + 0.6, y1 + 0.6), h: HAlign::Left, v: VAlign::Bottom },
        ];
    }
    let c = t(0.0, 0.0);
    let (below, above) = body_across(sym.style);
    if pl.rot % 2 == 1 {
        // Vertical: beside the body, on the sheet side of the symbol's −Y (away from LED arrows
        // and switch plungers, which are drawn toward +Y).
        let dx = below.max(1.6) + 0.6;
        if rot((0, -1), pl.rot).0 > 0 {
            [
                FieldPos { at: (c.0 + dx, c.1 + 0.3), h: HAlign::Left, v: VAlign::Bottom },
                FieldPos { at: (c.0 + dx, c.1 - 0.3), h: HAlign::Left, v: VAlign::Top },
            ]
        } else {
            [
                FieldPos { at: (c.0 - dx, c.1 + 0.3), h: HAlign::Right, v: VAlign::Bottom },
                FieldPos { at: (c.0 - dx, c.1 - 0.3), h: HAlign::Right, v: VAlign::Top },
            ]
        }
    } else {
        let uy = rot((0, 1), pl.rot).1;
        let (up, down) = if uy > 0 { (above, below) } else { (below, above) };
        [
            FieldPos { at: (c.0, c.1 + up.max(1.5) + 0.5), h: HAlign::Center, v: VAlign::Bottom },
            FieldPos { at: (c.0, c.1 - down.max(1.5) - 0.5), h: HAlign::Center, v: VAlign::Top },
        ]
    }
}

fn sheet_box(pts: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    let xs = pts.iter().map(|p| p.0);
    let ys = pts.iter().map(|p| p.1);
    (
        xs.clone().fold(f64::MAX, f64::min),
        ys.clone().fold(f64::MAX, f64::min),
        xs.fold(f64::MIN, f64::max),
        ys.fold(f64::MIN, f64::max),
    )
}
