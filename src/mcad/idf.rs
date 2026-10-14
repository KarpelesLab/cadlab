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
//! Y, mounting offset 0, rotation, `TOP`/`BOTTOM`, `PLACED`). Between the outline and the
//! holes:
//!
//! - `.ROUTE_OUTLINE ECAD` (`ALL` layers) and `.PLACE_OUTLINE ECAD` (`BOTH` sides, no height
//!   field: no height restriction): the board's outer contour shrunk by the copper-to-edge
//!   clearance, the area copper and courtyards keep to (one section each per piece; the exact
//!   outline when the clearance is zero).
//! - Per keep-out (a cadlab keep-out can give several): `.ROUTE_KEEPOUT ECAD` for `no_tracks`
//!   with the routing layers `ALL`, `TOP`, `BOTTOM`, `BOTH` and/or `INNER` (one section per
//!   keyword: IDF 3.0 cannot name single inner layers, so any inner layer gives `INNER`);
//!   `.VIA_KEEPOUT ECAD` for `no_vias`; `.PLACE_KEEPOUT ECAD` for `no_footprints`, on the side
//!   its copper layers touch (`TOP`, `BOTTOM` or `BOTH`) with height 0 (every component
//!   excluded). Pour keep-outs and rule areas (keep-outs that forbid nothing) have no IDF 3.0
//!   counterpart and are left out (IDX carries pour keep-outs).
//!
//! In these sections the loop label is the direction (0: counter-clockwise), each section is
//! one closed loop.
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

use super::{
    BodyBox, DrillHole, Edge, Loop, Options, board_profile, body_set, drill_holes, inset_outer, keepout_sides,
    polygon_loop, sweep_deg,
};
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
    /// Keep-out sections written (`.ROUTE_KEEPOUT`, `.VIA_KEEPOUT`, `.PLACE_KEEPOUT`).
    pub keepouts: usize,
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

/// IDF routing layer keywords covering a keep-out's copper layers (`layers` empty: all):
/// `ALL`, else `TOP`, `BOTTOM` or `BOTH` for the outer layers and `INNER` when any inner layer
/// is included (IDF 3.0 cannot name single inner layers).
fn routing_layers(copper: &[String], layers: &[String]) -> Vec<&'static str> {
    let on = |l: &String| layers.is_empty() || layers.contains(l);
    let n = copper.len();
    let (top, bottom) = (n > 0 && on(&copper[0]), n > 1 && on(&copper[n - 1]));
    let inner: Vec<bool> = copper.iter().skip(1).take(n.saturating_sub(2)).map(on).collect();
    let all_inner = inner.iter().all(|&b| b);
    if layers.is_empty() || (n == 1 && top) || (top && bottom && all_inner) {
        return vec!["ALL"];
    }
    let mut out = Vec::new();
    match (top, bottom) {
        (true, true) => out.push("BOTH"),
        (true, false) => out.push("TOP"),
        (false, true) => out.push("BOTTOM"),
        (false, false) => {}
    }
    if inner.iter().any(|&b| b) {
        out.push("INNER");
    }
    out
}

/// One keep-out or outline section: `.NAME ECAD`, an optional second record, one loop
/// (label 0: counter-clockwise), `.END_NAME`.
fn loop_section(out: &mut String, name: &str, record: Option<&str>, lp: &Loop) {
    let _ = writeln!(out, ".{name} ECAD");
    if let Some(r) = record {
        let _ = writeln!(out, "{r}");
    }
    write_loop(out, 0, lp);
    let _ = writeln!(out, ".END_{name}");
}

/// Routing and placement outlines and keep-outs; returns the number of keep-out sections.
fn outlines_and_keepouts(out: &mut String, p: &Project, outer: &Loop) -> usize {
    let b = p.board();
    // Routing and placement confined to the board shrunk by the copper-to-edge clearance.
    for lp in inset_outer(p, outer, b.rules.copper_to_edge) {
        loop_section(out, "ROUTE_OUTLINE", Some("ALL"), &lp);
        loop_section(out, "PLACE_OUTLINE", Some("BOTH"), &lp);
    }
    let copper = b.stackup.copper_names();
    let mut n = 0;
    for k in &b.keepouts {
        let Some(lp) = polygon_loop(&k.outline) else { continue };
        let lp = lp.oriented(true);
        if k.no_tracks {
            for layers in routing_layers(&copper, &k.layers) {
                loop_section(out, "ROUTE_KEEPOUT", Some(layers), &lp);
                n += 1;
            }
        }
        if k.no_vias {
            loop_section(out, "VIA_KEEPOUT", None, &lp);
            n += 1;
        }
        if k.no_footprints {
            let side = match keepout_sides(p, &k.layers).as_slice() {
                [BoardSide::Top] => "TOP",
                [BoardSide::Bottom] => "BOTTOM",
                _ => "BOTH",
            };
            // Height 0: every component is excluded.
            loop_section(out, "PLACE_KEEPOUT", Some(&format!("{side} 0")), &lp);
            n += 1;
        }
    }
    n
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
    let keepouts = outlines_and_keepouts(&mut b, p, &outer);
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
    Some(IdfOut { board: b, library: l, no_body, model_errors, placed: bodies.len(), holes: holes.len(), keepouts })
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

    #[test]
    fn routing_layer_keywords() {
        let four: Vec<String> = ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"].map(String::from).to_vec();
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(routing_layers(&four, &[]), ["ALL"]);
        assert_eq!(routing_layers(&four, &l(&["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"])), ["ALL"]);
        assert_eq!(routing_layers(&four, &l(&["F.Cu", "B.Cu"])), ["BOTH"]);
        assert_eq!(routing_layers(&four, &l(&["B.Cu"])), ["BOTTOM"]);
        assert_eq!(routing_layers(&four, &l(&["F.Cu", "In2.Cu"])), ["TOP", "INNER"]);
        assert_eq!(routing_layers(&four, &l(&["In1.Cu"])), ["INNER"]);
        let two: Vec<String> = ["F.Cu", "B.Cu"].map(String::from).to_vec();
        assert_eq!(routing_layers(&two, &l(&["F.Cu", "B.Cu"])), ["ALL"]);
        assert_eq!(routing_layers(&two, &l(&["F.Cu"])), ["TOP"]);
        assert_eq!(routing_layers(&["F.Cu".to_string()], &l(&["F.Cu"])), ["ALL"]);
    }
}
