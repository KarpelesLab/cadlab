//! Drawing a laid-out sheet.

use super::symbol::{self, STYLE, symbol_of};
use super::{Dir, LabelKind, SheetLayout};
use crate::model::Project;
use crate::model::part::Part;
use crate::render::{Color, HAlign, Scene, VAlign, font};
use crate::units::{LengthUnit, Nm};

const WIRE: Color = Color::hex(0x1a7f37);
const LABEL: Color = Color::hex(0x0550ae);
const POWER: Color = Color::hex(0xc4302b);
const FRAME: Color = Color::hex(0x8c959f);
const NC: Color = Color::hex(0x0550ae);

fn mm(v: Nm) -> f64 {
    v.to_f64(LengthUnit::Mm)
}

/// Draws the sheet: frame, title block, symbols, wires, labels.
pub fn draw(p: &Project, l: &SheetLayout) -> Scene {
    let mut s = Scene::default();
    let (w, h) = (mm(l.size.0), mm(l.size.1));
    let ts = STYLE.text_size;

    // Frame and title block.
    s.outline(vec![(5.0, 5.0), (w - 5.0, 5.0), (w - 5.0, h - 5.0), (5.0, h - 5.0)], 0.2, FRAME);
    let (tx0, ty1) = (w - 5.0 - 90.0, 5.0 + 14.0);
    s.outline(vec![(tx0, 5.0), (w - 5.0, 5.0), (w - 5.0, ty1), (tx0, ty1)], 0.2, FRAME);
    s.line(vec![(tx0, 5.0 + 7.0), (w - 5.0, 5.0 + 7.0)], 0.15, FRAME);
    s.text(&p.manifest().name, (tx0 + 2.0, ty1 - 2.0), 2.5, HAlign::Left, VAlign::Top, 0, STYLE.text);
    let rev = p.manifest().metadata.get("rev").map(|r| format!("rev {r}  ")).unwrap_or_default();
    s.text(
        &format!("{rev}{} / cadlab {}", l.paper, env!("CARGO_PKG_VERSION")),
        (tx0 + 2.0, 5.0 + 5.0),
        ts,
        HAlign::Left,
        VAlign::Top,
        0,
        FRAME,
    );

    // Wires.
    for (a, b) in &l.wires {
        s.line(vec![(mm(a.x), mm(a.y)), (mm(b.x), mm(b.y))], 0.25, WIRE);
    }

    // Symbols.
    for (r, pl) in &l.placements {
        let Some(comp) = p.circuit().components.get(r) else {
            continue;
        };
        let Some(part) = p.library().parts.get(&comp.part) else {
            continue;
        };
        let sym = symbol_of(part);
        symbol::draw(&mut s, &sym, pl, r, &Part::value(part));
    }

    // Labels and power symbols.
    for lb in &l.labels {
        let (x, y) = (mm(lb.at.x), mm(lb.at.y));
        let (dx, dy) = lb.dir.vec();
        let (dx, dy) = (dx as f64, dy as f64);
        let q = if lb.dir.horizontal() { 0 } else { 1 };
        match lb.kind {
            LabelKind::Net => {
                // Flag: point at the pin, text inside.
                let len = font::width(&lb.net, ts) + 2.0;
                let (px, py) = (-dy, dx); // perpendicular
                let hw = 1.0;
                let pts = vec![
                    (x, y),
                    (x + dx * 1.0 + px * hw, y + dy * 1.0 + py * hw),
                    (x + dx * len + px * hw, y + dy * len + py * hw),
                    (x + dx * len - px * hw, y + dy * len - py * hw),
                    (x + dx * 1.0 - px * hw, y + dy * 1.0 - py * hw),
                ];
                s.outline(pts, 0.2, LABEL);
                let (h, v) = (HAlign::Center, VAlign::Middle);
                s.text(&lb.net, (x + dx * (len / 2.0 + 0.5), y + dy * (len / 2.0 + 0.5)), ts, h, v, q, LABEL);
            }
            LabelKind::Wire => {
                // Text above (or left of) the wire, starting at the anchor point.
                let (h, v) = match lb.dir {
                    Dir::Right => (HAlign::Left, VAlign::Bottom),
                    Dir::Left => (HAlign::Right, VAlign::Bottom),
                    Dir::Up => (HAlign::Left, VAlign::Bottom),
                    Dir::Down => (HAlign::Right, VAlign::Bottom),
                };
                s.text(
                    &lb.net,
                    (x, y + if q == 0 { 0.4 } else { 0.0 } - if q == 1 { 0.4 } else { 0.0 }),
                    ts,
                    h,
                    v,
                    q,
                    LABEL,
                );
            }
            LabelKind::Power => {
                let stub = 2.0;
                let (ex, ey) = (x + dx * stub, y + dy * stub);
                s.line(vec![(x, y), (ex, ey)], 0.25, POWER);
                let (px, py) = (-dy, dx);
                s.line(vec![(ex - px * 1.0, ey - py * 1.0), (ex + px * 1.0, ey + py * 1.0)], 0.35, POWER);
                let at = (ex + dx * 0.6, ey + dy * 0.6);
                let (h, v) = match lb.dir {
                    Dir::Up => (HAlign::Center, VAlign::Bottom),
                    Dir::Down => (HAlign::Center, VAlign::Top),
                    Dir::Right => (HAlign::Left, VAlign::Middle),
                    Dir::Left => (HAlign::Right, VAlign::Middle),
                };
                s.text(&lb.net, at, ts, h, v, 0, POWER);
            }
            LabelKind::Ground => {
                let stub = 1.5;
                let (ex, ey) = (x + dx * stub, y + dy * stub);
                s.line(vec![(x, y), (ex, ey)], 0.25, POWER);
                let (px, py) = (-dy, dx);
                for (i, half) in [1.3, 0.85, 0.4].iter().enumerate() {
                    let (cx, cy) = (ex + dx * 0.5 * i as f64, ey + dy * 0.5 * i as f64);
                    s.line(vec![(cx - px * half, cy - py * half), (cx + px * half, cy + py * half)], 0.3, POWER);
                }
            }
        }
    }

    // No-connect markers.
    for p in &l.no_connects {
        let (x, y) = (mm(p.x), mm(p.y));
        let d = 0.8;
        s.line(vec![(x - d, y - d), (x + d, y + d)], 0.25, NC);
        s.line(vec![(x - d, y + d), (x + d, y - d)], 0.25, NC);
    }
    s
}
