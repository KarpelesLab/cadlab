//! Courtyard, fabrication outline and silkscreen for generated footprints.

use polyclip::{FillRule, Path, Ring, clip_paths};

use super::{GenError, GenOptions};
use crate::geom::{BBox, Point};
use crate::model::footprint::{Body, Footprint, Graphic, GraphicGeometry, GraphicLayer, Mount, Pad};
use crate::units::Nm;

/// Shortest silkscreen segment kept after clipping around pads.
const MIN_SILK_SEGMENT: Nm = Nm::from_um(200);

fn rect(min: Point, max: Point) -> Vec<Point> {
    vec![min, Point::new(max.x, min.y), max, Point::new(min.x, max.y)]
}

fn pad_box(p: &Pad, grow: Nm) -> BBox {
    let (w, h) = p.shape.size();
    let (w, h) = if p.rotation.quarter_turns().is_some_and(|q| q % 2 == 1) { (h, w) } else { (w, h) };
    BBox {
        min: Point::new(p.at.x - Nm(w.0 / 2) - grow, p.at.y - Nm(h.0 / 2) - grow),
        max: Point::new(p.at.x + Nm(w.0 / 2) + grow, p.at.y + Nm(h.0 / 2) + grow),
    }
}

fn body_box(body: &Body) -> BBox {
    BBox {
        min: Point::new(-Nm(body.width.0 / 2), -Nm(body.length.0 / 2)),
        max: Point::new(Nm(body.width.0 / 2), Nm(body.length.0 / 2)),
    }
}

fn snap_out(b: BBox, grid: Nm) -> BBox {
    let g = grid.0.max(1);
    let down = |v: Nm| Nm(v.0.div_euclid(g) * g);
    let up = |v: Nm| Nm(-(-v.0).div_euclid(g) * g);
    BBox { min: Point::new(down(b.min.x), down(b.min.y)), max: Point::new(up(b.max.x), up(b.max.y)) }
}

fn grow(b: BBox, d: Nm) -> BBox {
    BBox { min: Point::new(b.min.x - d, b.min.y - d), max: Point::new(b.max.x + d, b.max.y + d) }
}

fn union(a: BBox, b: BBox) -> BBox {
    let mut r = a;
    r.add_point(b.min);
    r.add_point(b.max);
    r
}

fn path_len(pts: &[Point]) -> i64 {
    pts.windows(2)
        .map(|w| {
            let (dx, dy) = ((w[1].x - w[0].x).0 as f64, (w[1].y - w[0].y).0 as f64);
            (dx * dx + dy * dy).sqrt() as i64
        })
        .sum()
}

/// Assembles the footprint: pads plus courtyard, fab outline and silkscreen.
pub(super) fn finish(
    name: String,
    mount: Mount,
    pads: Vec<Pad>,
    body: Body,
    courtyard_excess: Nm,
    pin1_marker: bool,
    opts: &GenOptions,
) -> Result<Footprint, GenError> {
    let bb = body_box(&body);
    let mut graphics = Vec::new();

    // Fabrication outline: the body, with a chamfered pin-1 corner on polarized packages.
    let fab = if pin1_marker {
        let c = Nm(body.width.0.min(body.length.0) / 4).min(Nm::from_um(1000));
        vec![
            Point::new(bb.min.x + c, bb.max.y),
            Point::new(bb.max.x, bb.max.y),
            Point::new(bb.max.x, bb.min.y),
            Point::new(bb.min.x, bb.min.y),
            Point::new(bb.min.x, bb.max.y - c),
        ]
    } else {
        rect(bb.min, bb.max)
    };
    graphics.push(Graphic {
        layer: GraphicLayer::Fab,
        width: opts.fab_width,
        geometry: GraphicGeometry::Polygon { points: fab },
    });

    // Silkscreen: the body outline (inner stroke edge on the body edge), minus keep-outs around pads.
    let half = Nm(opts.silk_width.0 / 2);
    let mut sb = grow(bb, half);
    // Every pad under the body (BGA): move the outline out to clear the pads rather than clipping
    // it into slivers between them.
    let inside = |b: BBox| b.min.x >= bb.min.x && b.min.y >= bb.min.y && b.max.x <= bb.max.x && b.max.y <= bb.max.y;
    if !pads.is_empty() && pads.iter().all(|p| inside(pad_box(p, Nm::ZERO))) {
        for p in &pads {
            sb = union(sb, pad_box(p, opts.silk_clearance + half * 2));
        }
    }
    let outline: Vec<Point> = {
        let mut r = rect(sb.min, sb.max);
        r.push(r[0]);
        r
    };
    let keepouts: Vec<Ring> = pads
        .iter()
        .map(|p| {
            let b = pad_box(p, opts.silk_clearance + half);
            Ring(rect(b.min, b.max).into_iter().map(Into::into).collect())
        })
        .collect();
    let path = Path(outline.iter().copied().map(Into::into).collect());
    let clipped =
        clip_paths(&vec![path], &keepouts, FillRule::NonZero).map_err(|e| GenError::Invalid(e.to_string()))?;
    let mut extent = union(bb, bb);
    for piece in &clipped.outside {
        let pts: Vec<Point> = piece.points.iter().copied().map(Into::into).collect();
        if path_len(&pts) < MIN_SILK_SEGMENT.0 {
            continue;
        }
        graphics.push(Graphic {
            layer: GraphicLayer::Silk,
            width: opts.silk_width,
            geometry: GraphicGeometry::Path { points: pts },
        });
    }

    // Pin-1 dot, left of pad 1 and clear of all pads.
    if pin1_marker && let Some(p1) = pads.iter().find(|p| p.number == "1" || p.number == "A1") {
        let b = pad_box(p1, Nm::ZERO);
        let r = opts.silk_width;
        let mut center = Point::new(b.min.x - opts.silk_clearance - r * 2, p1.at.y);
        // Keep it outside the silk outline too, so it stays visible on small parts.
        if center.x > sb.min.x - r * 2 {
            center.x = sb.min.x - r * 2;
        }
        // Not part of the courtyard: IPC courtyards cover pads and body only.
        graphics.push(Graphic {
            layer: GraphicLayer::Silk,
            width: Nm::ZERO,
            geometry: GraphicGeometry::Circle { center, radius: r, filled: true },
        });
    }

    // Courtyard: pads and body (and markers), plus the excess, on the courtyard grid.
    for p in &pads {
        extent = union(extent, pad_box(p, Nm::ZERO));
    }
    let cy = snap_out(grow(extent, courtyard_excess), opts.courtyard_grid);

    Ok(Footprint {
        name,
        description: String::new(),
        mount,
        pads,
        courtyard: rect(cy.min, cy.max),
        graphics,
        body: Some(body),
        generator: None,
        model: None,
    })
}
