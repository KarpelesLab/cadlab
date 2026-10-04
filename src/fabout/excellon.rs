//! Excellon drill files in the XNC profile (Ucamco "XNC Format Specification", revision
//! 2021.11): metric, decimal-point coordinates, a tool table in the header, one file per plating
//! and layer span. Holes are drill hits; slots (oval holes) are routed with the slot's width as
//! the tool (`G00` to one end, `M15` tool down, `G01` to the other end, `M16` tool up, `G05`
//! back to drill mode). X2 attributes are carried in standard comments
//! (`; #@! TF.FileFunction,...`, `; #@! TA.AperFunction,...`, spec section 4).

use std::fmt::Write as _;

use super::gerber::{field, mm};
use super::layers::{drill_function, drill_groups, drill_tools};
use super::{FileKind, Options, OutFile, file_name};
use crate::model::Project;

/// Largest number of tools XNC allows in one file (two-digit tool numbers).
pub const MAX_TOOLS: usize = 99;

/// Whether some drill file would need more than [`MAX_TOOLS`] tools (the export commands
/// refuse such boards).
pub fn too_many_tools(p: &Project) -> bool {
    drill_groups(p).iter().any(|(_, hs)| drill_tools(hs).len() > MAX_TOOLS)
}

/// One XNC file per drill span and plating (plated through holes first), one tool per
/// (function, diameter). See [`too_many_tools`] for the 99-tool limit.
pub fn drills(p: &Project, o: &Options) -> Vec<OutFile> {
    let n = p.board().stackup.copper_names().len();
    let name = p.manifest().name.clone();
    let mut out = Vec::new();
    for ((plated, a, b), hs) in drill_groups(p) {
        let (function, through) = drill_function(plated, a, b, n);
        let tools = drill_tools(&hs);
        let mut s = String::from("M48\n");
        let _ = writeln!(s, "; #@! TF.GenerationSoftware,cadlab,cadlab,{}", field(&o.version));
        s.push_str("; #@! TF.SameCoordinates\n");
        let _ = writeln!(s, "; #@! TF.FileFunction,{function},Drill");
        s.push_str("; #@! TF.FilePolarity,Positive\n; #@! TF.Part,Single\nMETRIC\n");
        let mut cur = None;
        for (i, (kind, d)) in tools.iter().enumerate() {
            if cur != Some(*kind) {
                let _ = writeln!(s, "; #@! TA.AperFunction,{}", kind.function());
                cur = Some(*kind);
            }
            let _ = writeln!(s, "T{:02}C{}", i + 1, xnc_num(d.0));
        }
        s.push_str("%\nG05\n");
        for (i, (kind, d)) in tools.iter().enumerate() {
            let _ = writeln!(s, "T{:02}", i + 1);
            let of_tool = || hs.iter().filter(|h| h.kind == *kind && h.diameter == *d);
            for h in of_tool().filter(|h| h.slot.is_none()) {
                let _ = writeln!(s, "X{}Y{}", xnc_num(h.at.x.0), xnc_num(h.at.y.0));
            }
            // Slots in route mode: move to one end, tool down, route to the other, tool up,
            // back to drill mode.
            for h in of_tool() {
                let Some((a, b)) = h.slot else { continue };
                let _ = writeln!(s, "G00X{}Y{}", xnc_num(a.x.0), xnc_num(a.y.0));
                s.push_str("M15\n");
                let _ = writeln!(s, "G01X{}Y{}", xnc_num(b.x.0), xnc_num(b.y.0));
                s.push_str("M16\nG05\n");
            }
        }
        s.push_str("M30\n");
        out.push(OutFile {
            name: file_name(&name, &FileKind::Drill { plated, from: a, to: b, through }),
            function,
            content: s,
        });
    }
    out
}

/// A millimeter value with an explicit decimal point (XNC 2.6 allows any decimals; the point
/// avoids the zero-suppression ambiguity of legacy Excellon).
fn xnc_num(nm: i64) -> String {
    let s = mm(nm);
    if s.contains('.') { s } else { format!("{s}.0") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(xnc_num(0), "0.0");
        assert_eq!(xnc_num(300_000), "0.3");
        assert_eq!(xnc_num(-12_345_678), "-12.345678");
    }
}
