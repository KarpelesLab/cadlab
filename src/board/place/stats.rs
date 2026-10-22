//! How a placement uses the board ([`placement_stats`]): a measure of spread placement's goal
//! (no side of a roomy board left empty while the parts clump elsewhere), reported by
//! `place.auto` and used by the placement tests and benchmarks.
//!
//! The free area (outline shrunk by the edge margin, minus cutouts and holes) is rasterized on
//! a grid of about 40 000 cells; a cell is covered when its center lies in the courtyard box of
//! a placed footprint (either side). Over that raster:
//!
//! - **windows**: the free area's box is cut into about 16 near-square windows; a window
//!   counts when at least a quarter of it is free area, and is *empty* when its covered share
//!   is below a quarter of the board's average (parts somewhere else, nothing here);
//! - **crowding**: the share of the courtyard area beyond what windows may hold at the
//!   default density (a window holding more than 1 / 0.6 times the board's average share, and
//!   more than a quarter of its area, is crowded): parts clumped while the board has room;
//! - **unevenness**: the coefficient of variation of the windows' covered shares (each window
//!   weighted by its free area): 0 when every window holds the same share of parts;
//! - **largest empty square**: the side of the largest square of free, uncovered cells;
//! - **extent**: the share of the free area's box the box around all courtyards covers.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Rect, courtyard_box, free_area};
use crate::model::Project;
use crate::model::board::BoardSide;
use crate::units::Nm;

/// Windows per board of the stats (about).
const WINDOWS: f64 = 16.0;
/// Raster cells per board (about).
const CELLS: f64 = 40_000.0;

/// How a placement uses the board ([`placement_stats`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlacementStats {
    /// Windows with at least a quarter of their area free to place in.
    pub windows: usize,
    /// Windows among those holding less than a quarter of the average share of courtyards.
    pub empty_windows: usize,
    /// Share of the courtyard area beyond what windows may hold at the default density
    /// ([`super::SPREAD_DENSITY`]: 1 / 0.6 times the board's average, at least a quarter of the
    /// window): how clumped the parts are.
    pub crowding: f64,
    /// Coefficient of variation of the windows' courtyard shares (0: even).
    pub unevenness: f64,
    /// Side of the largest square of the free area clear of courtyards.
    pub largest_empty: Nm,
    /// Share of the free area's box covered by the box around all courtyards.
    pub extent: f64,
    /// Share of the free area covered by courtyards.
    pub fill: f64,
}

/// How the placed footprints use the board (`None` without an outline or footprints).
pub fn placement_stats(p: &Project) -> Option<PlacementStats> {
    let b = p.board();
    let refs: Vec<&String> = b.footprints.keys().collect();
    let boxes: Vec<Rect> = refs.iter().filter_map(|r| courtyard_box(p, r)).collect();
    if boxes.is_empty() {
        return None;
    }
    let all: BTreeSet<String> = refs.iter().map(|r| r.to_string()).collect();
    let edge = b.rules.copper_to_edge.0.max(0);
    let free = free_area(p, BoardSide::Top, 0, edge, &all, false)?;
    let fb = polyclip::Geometry::bbox(&free)?;
    let fr = Rect { x0: fb.min.x, y0: fb.min.y, x1: fb.max.x, y1: fb.max.y };
    if fr.w() <= 0 || fr.h() <= 0 {
        return None;
    }
    let s = ((fr.w() as f64 * fr.h() as f64 / CELLS).sqrt() as i64).max(50_000);
    let (nx, ny) = ((fr.w() / s).max(1) as usize, (fr.h() / s).max(1) as usize);
    let prepared = polyclip::Prepared::new(&free);
    // Per cell: 0 outside the free area, 1 free, 2 covered.
    let mut cells = vec![0u8; nx * ny];
    for j in 0..ny {
        let y = fr.y0 + j as i64 * s + s / 2;
        for i in 0..nx {
            let x = fr.x0 + i as i64 * s + s / 2;
            if prepared.locate(polyclip::Point::new(x, y)) != polyclip::Location::Inside {
                continue;
            }
            let covered = boxes.iter().any(|r| r.x0 <= x && x <= r.x1 && r.y0 <= y && y <= r.y1);
            cells[j * nx + i] = if covered { 2 } else { 1 };
        }
    }
    let free_cells = cells.iter().filter(|&&c| c > 0).count();
    let covered_cells = cells.iter().filter(|&&c| c == 2).count();
    if free_cells == 0 {
        return None;
    }
    let rho = covered_cells as f64 / free_cells as f64;

    // Windows: about WINDOWS near-square ones over the free area's box.
    let side = ((fr.w() as f64 * fr.h() as f64) / WINDOWS).sqrt();
    let wx = ((fr.w() as f64 / side).round() as usize).max(1);
    let wy = ((fr.h() as f64 / side).round() as usize).max(1);
    let mut win = vec![(0usize, 0usize); wx * wy];
    for j in 0..ny {
        for i in 0..nx {
            let c = cells[j * nx + i];
            if c == 0 {
                continue;
            }
            let w = &mut win[(j * wy / ny) * wx + i * wx / nx];
            w.0 += 1;
            w.1 += usize::from(c == 2);
        }
    }
    let full = (nx * ny) as f64 / (wx * wy) as f64;
    let counted: Vec<(f64, f64)> =
        win.iter().filter(|w| w.0 as f64 >= full / 4.0).map(|&(f, c)| (f as f64, c as f64 / f as f64)).collect();
    let weight: f64 = counted.iter().map(|w| w.0).sum();
    let unevenness = if rho > 0.0 && weight > 0.0 {
        (counted.iter().map(|&(f, d)| f * (d - rho).powi(2)).sum::<f64>() / weight).sqrt() / rho
    } else {
        0.0
    };
    let empty_windows = counted.iter().filter(|w| w.1 < rho / 4.0).count();
    // Courtyard area beyond what the default density lets a window hold.
    let cap = (rho / super::SPREAD_DENSITY).max(super::CROWD_FLOOR);
    let excess: f64 = counted.iter().map(|&(f, d)| f * (d - cap).max(0.0)).sum();
    let crowding = if covered_cells > 0 { excess / covered_cells as f64 } else { 0.0 };

    // Largest empty square (dynamic programming over the raster).
    let mut run = vec![0u32; nx * ny];
    let mut best = 0u32;
    for j in 0..ny {
        for i in 0..nx {
            if cells[j * nx + i] != 1 {
                continue;
            }
            let v = if i == 0 || j == 0 {
                1
            } else {
                1 + run[j * nx + i - 1].min(run[(j - 1) * nx + i]).min(run[(j - 1) * nx + i - 1])
            };
            run[j * nx + i] = v;
            best = best.max(v);
        }
    }

    let ext = boxes.iter().copied().reduce(|a, b| Rect {
        x0: a.x0.min(b.x0),
        y0: a.y0.min(b.y0),
        x1: a.x1.max(b.x1),
        y1: a.y1.max(b.y1),
    })?;
    let clip = |a: i64, lo: i64, hi: i64| a.clamp(lo, hi);
    let ew = (clip(ext.x1, fr.x0, fr.x1) - clip(ext.x0, fr.x0, fr.x1)) as f64;
    let eh = (clip(ext.y1, fr.y0, fr.y1) - clip(ext.y0, fr.y0, fr.y1)) as f64;
    let round = |v: f64| (v * 1000.0).round() / 1000.0;
    Some(PlacementStats {
        windows: counted.len(),
        empty_windows,
        crowding: round(crowding),
        unevenness: round(unevenness),
        largest_empty: Nm(best as i64 * s),
        extent: round(ew * eh / (fr.w() as f64 * fr.h() as f64)),
        fill: round(rho),
    })
}
