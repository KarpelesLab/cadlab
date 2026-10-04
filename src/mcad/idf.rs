//! IDF 3.0 export: the board file (`.emn`) and the component library file (`.emp`), written
//! from the published "Intermediate Data Format, Version 3.0" specification.
//!
//! Board file sections: `.HEADER` (`BOARD_FILE 3.0`, board name, units `MM`),
//! `.BOARD_OUTLINE ECAD` (thickness, then loops of points: label 0 is the outline,
//! counter-clockwise; labels 1, 2, ... are cutouts, clockwise; each point carries the included
//! angle of the arc from the previous point, positive counter-clockwise, 0 for a line, and a
//! circle is its center followed by a point on it with angle 360), `.DRILLED_HOLES` (diameter,
//! X, Y, `PTH`/`NPTH`, associated part — the designator, or `BOARD` — and hole type `PIN`,
//! `VIA` or `MTG`, owner `ECAD`) and `.PLACEMENT` (geometry name, part number, designator; X,
//! Y, mounting offset 0, rotation, `TOP`/`BOTTOM`, `PLACED`).
//!
//! Library file: one `.ELECTRICAL` section per (geometry, part number) with the body height and
//! its outline, counter-clockwise: the package body rectangle centered on the footprint origin,
//! or, for a component with an attached 3D model, the model's bounding rectangle and its top as
//! the height. A bottom-side component is the library outline mirrored about its Y axis, then
//! rotated counter-clockwise (seen from the top) by the placement angle: cadlab's own
//! convention (the same as for pads), which for centered rectangles equals any other mirror
//! axis.
//!
//! Strings are always double-quoted. The header date is fixed so that output is
//! byte-identical across runs.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::{BodyBox, DrillHole, Edge, Loop, Options, board_profile, body_set, drill_holes, sweep_deg};
use crate::fabout::HoleKind;
use crate::fabout::gerber::mm;
use crate::model::Project;
use crate::model::board::BoardSide;

/// Fixed header date (deterministic output).
pub const DATE: &str = "1970/01/01.00:00:00";

/// Result of an IDF export.
#[derive(Clone, Debug)]
pub struct IdfOut {
    /// Board file (`.emn`).
    pub board: String,
    /// Library file (`.emp`).
    pub library: String,
    /// Components without a package body (left out of the placement).
    pub no_body: Vec<String>,
    /// Components whose attached 3D model could not be read (their package box is used).
    pub model_errors: Vec<(String, crate::models3d::ModelError3d)>,
    /// Components placed.
    pub placed: usize,
    /// Drilled holes listed.
    pub holes: usize,
}

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "'"))
}

fn angle(a: f64) -> String {
    let t = format!("{a:.3}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    if t == "-0" { "0".into() } else { t.to_string() }
}

fn write_loop(out: &mut String, label: usize, lp: &Loop) {
    // A loop that is one full circle: center, then a point on it with angle 360.
    if let [Edge::Arc { to, center, .. }] = lp.edges.as_slice()
        && *to == lp.start
    {
        let (cx, cy) = (center.0.round() as i64, center.1.round() as i64);
        let _ = writeln!(out, "{label} {} {} 0", mm(cx), mm(cy));
        let _ = writeln!(out, "{label} {} {} 360", mm(lp.start.x.0), mm(lp.start.y.0));
        return;
    }
    let lp = lp.split_circles();
    let _ = writeln!(out, "{label} {} {} 0", mm(lp.start.x.0), mm(lp.start.y.0));
    for (i, e) in lp.edges.iter().enumerate() {
        let to = e.to();
        let a = match *e {
            Edge::Line { .. } => "0".to_string(),
            Edge::Arc { center, ccw, .. } => angle(sweep_deg(lp.vertex(i), to, center, ccw)),
        };
        let _ = writeln!(out, "{label} {} {} {a}", mm(to.x.0), mm(to.y.0));
    }
}

fn hole_record(h: &DrillHole) -> String {
    let (assoc, kind) = match h.kind {
        HoleKind::Via => ("BOARD".to_string(), "VIA"),
        HoleKind::Component => (h.refdes.clone().unwrap_or_else(|| "BOARD".into()), "PIN"),
        HoleKind::Mechanical => match &h.refdes {
            Some(r) => (r.clone(), "PIN"),
            None => ("BOARD".into(), "MTG"),
        },
    };
    format!(
        "{} {} {} {} {} {kind} ECAD",
        mm(h.diameter.0),
        mm(h.at.x.0),
        mm(h.at.y.0),
        if h.plated { "PTH" } else { "NPTH" },
        q(&assoc)
    )
}

fn library_entry(out: &mut String, b: &BodyBox) {
    let _ = writeln!(out, ".ELECTRICAL");
    let _ = writeln!(out, "{} {} MM {}", q(&b.footprint), q(&b.part_number), mm(b.height.0));
    let (hw, hl) = (b.width.0 / 2, b.length.0 / 2);
    let (w, l) = (b.width.0 - hw, b.length.0 - hl);
    let (ox, oy) = (b.offset.x.0, b.offset.y.0);
    for (x, y) in [(-hw, -hl), (w, -hl), (w, l), (-hw, l), (-hw, -hl)] {
        let _ = writeln!(out, "0 {} {} 0", mm(ox + x), mm(oy + y));
    }
    let _ = writeln!(out, ".END_ELECTRICAL");
}

/// The IDF 3.0 board and library files. `None` without a board outline.
pub fn export(p: &Project, o: &Options) -> Option<IdfOut> {
    let (outer, cutouts) = board_profile(p)?;
    let name = p.manifest().name.clone();
    let source = q(&format!("cadlab {}", o.version));
    let holes = drill_holes(p, o.vias);
    let set = if o.components { body_set(p) } else { Default::default() };
    let (bodies, no_body, model_errors) = (set.bodies, set.no_body, set.model_errors);

    let mut b = String::new();
    let _ = writeln!(b, ".HEADER\nBOARD_FILE 3.0 {source} {DATE} 1\n{} MM\n.END_HEADER", q(&name));
    let _ = writeln!(b, ".BOARD_OUTLINE ECAD\n{}", mm(p.board().stackup.thickness.0));
    write_loop(&mut b, 0, &outer);
    for (i, c) in cutouts.iter().enumerate() {
        write_loop(&mut b, i + 1, c);
    }
    let _ = writeln!(b, ".END_BOARD_OUTLINE");
    if !holes.is_empty() {
        let _ = writeln!(b, ".DRILLED_HOLES");
        for h in &holes {
            let _ = writeln!(b, "{}", hole_record(h));
        }
        let _ = writeln!(b, ".END_DRILLED_HOLES");
    }
    if !bodies.is_empty() {
        let _ = writeln!(b, ".PLACEMENT");
        for c in &bodies {
            let side = if c.side == BoardSide::Top { "TOP" } else { "BOTTOM" };
            let _ = writeln!(b, "{} {} {}", q(&c.footprint), q(&c.part_number), q(&c.refdes));
            let _ = writeln!(
                b,
                "{} {} 0 {} {side} PLACED",
                mm(c.at.x.0),
                mm(c.at.y.0),
                crate::fabout::deg(c.rotation.normalized())
            );
        }
        let _ = writeln!(b, ".END_PLACEMENT");
    }

    let mut l = String::new();
    let _ = writeln!(l, ".HEADER\nLIBRARY_FILE 3.0 {source} {DATE} 1\n.END_HEADER");
    let mut seen: BTreeMap<(&str, &str), &BodyBox> = BTreeMap::new();
    for c in &bodies {
        seen.entry((c.footprint.as_str(), c.part_number.as_str())).or_insert(c);
    }
    for c in seen.values() {
        library_entry(&mut l, c);
    }
    Some(IdfOut { board: b, library: l, no_body, model_errors, placed: bodies.len(), holes: holes.len() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;
    use crate::units::Nm;

    #[test]
    fn loops() {
        let mut s = String::new();
        write_loop(&mut s, 1, &Loop::circle(Point::new(Nm(5_000_000), Nm(5_000_000)), Nm(2_000_000), false));
        assert_eq!(s, "1 5 5 0\n1 6 5 360\n");
        assert_eq!(angle(-0.0000001), "0");
        assert_eq!(angle(90.0), "90");
        assert_eq!(angle(-89.5), "-89.5");
    }
}
