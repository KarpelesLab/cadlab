//! Board-level holes (mounting holes) as pads: every consumer of [`super::placed_pads`] (DRC,
//! zone fill, rendering, Gerber/drill, IPC-D-356) sees them like footprint pads. A hole named
//! `H1` is a pad of "designator" `H1`: number `1` when plated (so it can carry a net), empty for
//! a non-plated hole.

use polyclip::{Circle, Polygon};

use super::{COPPER_TOL, PlacedPad};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::{BoardSide, Hole};
use crate::model::footprint::{Pad, PadKind, PadShape};

/// The footprint-like pad of a hole (centered at the origin).
pub fn hole_pad(h: &Hole) -> Pad {
    let (number, kind, d) = match h.pad {
        Some(d) => ("1".to_string(), PadKind::Tht { drill: h.drill }, d),
        None => (String::new(), PadKind::Npth { drill: h.drill }, h.drill),
    };
    Pad { number, at: Point::ORIGIN, rotation: Default::default(), shape: PadShape::Circle { d }, kind, paste: None }
}

/// Every board hole as a placed pad, in board order.
pub fn hole_pads(p: &Project) -> Vec<PlacedPad> {
    let board = p.board();
    let copper = board.stackup.copper_names();
    board
        .holes
        .iter()
        .map(|h| {
            let pad = hole_pad(h);
            let d = h.diameter();
            let ring = Circle { center: h.at.into(), radius: d.0 / 2 }.to_ring(COPPER_TOL).unwrap_or_default();
            PlacedPad {
                refdes: h.name.clone(),
                number: pad.number.clone(),
                net: if h.pad.is_some() { h.net.clone() } else { None },
                center: h.at,
                shape: Polygon::new(ring, vec![]),
                layers: if h.pad.is_some() { copper.clone() } else { Vec::new() },
                hole: Some((h.drill, h.pad.is_some())),
                pad,
                side: BoardSide::Top,
            }
        })
        .collect()
}

/// Whether `name` is a board hole (not a component).
pub fn is_hole(p: &Project, name: &str) -> bool {
    p.board().holes.iter().any(|h| h.name == name)
}
