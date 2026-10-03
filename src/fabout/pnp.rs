//! Generic pick-and-place (component placement) CSV.
//!
//! One line per placed, populated component (DNP excluded), naturally sorted by designator:
//! `Designator,Value,Package,Footprint,X (mm),Y (mm),Rotation,Side`. Positions are the footprint
//! origin (the package center for generated IPC-7351 footprints) relative to the lower-left
//! corner of the outline's bounding box. Rotation is the placement rotation in degrees,
//! counter-clockwise seen from the top, from the footprint's IPC-7351 zero orientation; bottom
//! side parts are mirrored across their Y axis before rotation (docs/BOARD.md). Fab-specific
//! columns, origins and rotation offsets are applied by fab profiles, not here (DECISIONS D12).

use super::gerber::mm;
use super::{FileKind, OutFile, deg, file_name, outline_origin, populated};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::BoardSide;
use crate::model::sections::natural_cmp;

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

/// The pick-and-place file (RFC 4180, CRLF line endings).
pub fn pick_place(p: &Project) -> OutFile {
    let origin = outline_origin(p).unwrap_or(Point::ORIGIN);
    let info = populated(p);
    let board = p.board();
    let mut refs: Vec<&String> = board.footprints.keys().filter(|r| info.contains_key(*r)).collect();
    refs.sort_by(|a, b| natural_cmp(a, b));
    let mut s = String::from("Designator,Value,Package,Footprint,X (mm),Y (mm),Rotation,Side\r\n");
    for r in refs {
        let pf = &board.footprints[r];
        let ci = &info[r];
        let fields = [
            r.clone(),
            ci.value.clone(),
            ci.package.clone(),
            ci.footprint.clone(),
            mm(pf.at.x.0 - origin.x.0),
            mm(pf.at.y.0 - origin.y.0),
            deg(pf.rotation.normalized()),
            match pf.side {
                BoardSide::Top => "top".into(),
                BoardSide::Bottom => "bottom".into(),
            },
        ];
        s.push_str(&fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        s.push_str("\r\n");
    }
    OutFile { name: file_name(&p.manifest().name, &FileKind::PickPlace), function: "PickPlace".into(), content: s }
}
