//! KiCad DRC exclusions (`.kicad_pro` `board.design_settings.drc_exclusions`) as cadlab DRC
//! waivers (`drc.waive`, DECISIONS D58).
//!
//! KiCad records each excluded finding as one string, `type|x|y|item|item` (the violation type
//! as its DRC reports name it, the marker position in nanometers, Y down, and the UUIDs of the
//! items involved, the nil UUID for none; some types end with a layer name instead), and newer
//! files as a `[string, comment]` pair. Read from files KiCad 7 to 10 write; no KiCad code is
//! involved (DECISIONS D7).
//!
//! An exclusion becomes a waiver when its type has a cadlab counterpart ([`TYPE_CODES`]), every
//! item it names was imported (the import's UUID table, board edges left out), and cadlab's
//! DRC of the imported board has a finding of that code naming all those items: the waiver
//! then has that code and those items as subjects, and the KiCad comment as its reason. The
//! others are reported, grouped by type and cause, never dropped silently.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::diag::Diagnostic;
use crate::model::Project;
use crate::model::board::DrcWaiver;
use crate::refs::ObjectRef;

/// KiCad DRC violation type → the cadlab finding codes it corresponds to.
pub const TYPE_CODES: &[(&str, &[&str])] = &[
    ("clearance", &["drc.clearance"]),
    ("shorting_items", &["drc.short"]),
    ("tracks_crossing", &["drc.short"]),
    ("track_width", &["drc.track_width", "drc.track_width_class"]),
    ("annular_width", &["drc.via_annular_ring", "drc.pad_annular_ring"]),
    ("drill_out_of_range", &["drc.via_drill", "drc.pad_drill"]),
    ("hole_to_hole", &["drc.hole_to_hole"]),
    ("holes_co_located", &["drc.hole_to_hole"]),
    ("hole_clearance", &["drc.hole_clearance"]),
    ("missing_footprint", &["drc.unplaced"]),
    ("copper_edge_clearance", &["drc.copper_to_edge", "drc.outside_board"]),
    ("courtyards_overlap", &["drc.courtyard_overlap"]),
    ("items_not_allowed", &["drc.keepout", "drc.disallowed"]),
    ("silk_over_copper", &["drc.silk_over_pad", "drc.silk_over_mask_opening"]),
    ("unconnected_items", &["drc.unrouted"]),
];

/// One excluded KiCad finding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exclusion {
    /// KiCad violation type (`courtyards_overlap`).
    pub kind: String,
    /// UUIDs of the items involved (nil UUIDs left out).
    pub items: Vec<String>,
    /// Layer, for types that name one.
    pub layer: Option<String>,
    /// The comment KiCad keeps with the exclusion.
    pub comment: Option<String>,
    /// The entry as written.
    pub raw: String,
}

impl Exclusion {
    /// Reads one `drc_exclusions` entry: a string, or a `[string, comment]` pair.
    pub fn from_json(v: &Value) -> Option<Exclusion> {
        let (raw, comment) = match v {
            Value::String(s) => (s.as_str(), None),
            Value::Array(a) => (a.first()?.as_str()?, a.get(1).and_then(Value::as_str)),
            _ => return None,
        };
        Exclusion::parse(raw, comment)
    }

    /// Reads `type|x|y|item|item` (items: UUIDs, or a layer name).
    pub fn parse(raw: &str, comment: Option<&str>) -> Option<Exclusion> {
        let mut parts = raw.split('|');
        let kind = parts.next()?.trim();
        if kind.is_empty() {
            return None;
        }
        let rest: Vec<&str> = parts.skip(2).map(str::trim).collect();
        let mut x = Exclusion {
            kind: kind.to_string(),
            comment: comment.map(str::trim).filter(|c| !c.is_empty()).map(String::from),
            raw: raw.to_string(),
            ..Default::default()
        };
        for item in rest {
            if is_uuid(item) {
                if item.bytes().any(|b| b != b'0' && b != b'-') {
                    x.items.push(item.to_ascii_lowercase());
                }
            } else if !item.is_empty() {
                x.layer = Some(item.to_string());
            }
        }
        Some(x)
    }
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes()
            .enumerate()
            .all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() })
}

/// What became of a project's DRC exclusions.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ExclusionReport {
    /// Exclusions read from the project file.
    pub read: usize,
    /// Waivers added (several exclusions of one finding make one waiver).
    pub waived: usize,
    /// Exclusions that matched no cadlab finding (reported in the diagnostics).
    pub not_matched: usize,
}

/// Why an exclusion did not become a waiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Miss {
    /// cadlab has no check of that type.
    NoCounterpart,
    /// An item it names was not imported.
    ItemMissing,
    /// cadlab's DRC does not find the violation on the imported board.
    NoFinding,
}

/// The cadlab codes an exclusion may match and the subjects the finding must name.
type Wanted = (&'static [&'static str], Vec<String>);

/// Turns `exclusions` into waivers on the imported board `p`, given the import's UUID table
/// (KiCad UUID → cadlab object label). Runs cadlab's DRC when there is anything to match.
pub fn apply(
    p: &mut Project,
    exclusions: &[Exclusion],
    uuids: &BTreeMap<String, String>,
) -> (ExclusionReport, Vec<Diagnostic>) {
    let mut report = ExclusionReport { read: exclusions.len(), ..Default::default() };
    let mut diags = Vec::new();
    if exclusions.is_empty() {
        return (report, diags);
    }
    let codes = |kind: &str| TYPE_CODES.iter().find(|(k, _)| *k == kind).map(|(_, c)| *c);
    // Subjects per exclusion, or why there are none.
    let mut wanted: Vec<Result<Wanted, Miss>> = Vec::new();
    for x in exclusions {
        let Some(codes) = codes(&x.kind) else {
            wanted.push(Err(Miss::NoCounterpart));
            continue;
        };
        let labels: Option<Vec<String>> = x.items.iter().map(|u| uuids.get(u).cloned()).collect();
        match labels {
            Some(l) => wanted.push(Ok((codes, l.into_iter().filter(|l| l != "edge").collect()))),
            None => wanted.push(Err(Miss::ItemMissing)),
        }
    }
    let findings = if wanted.iter().any(Result::is_ok) { crate::drc::check(p) } else { Vec::new() };
    let printed: Vec<(String, Vec<String>)> =
        findings.iter().map(|d| (d.code.to_string(), d.subjects.iter().map(ToString::to_string).collect())).collect();
    let mut misses: BTreeMap<(String, Miss), Vec<&Exclusion>> = BTreeMap::new();
    for (x, w) in exclusions.iter().zip(wanted) {
        let (codes, subjects) = match w {
            Ok(w) => w,
            Err(m) => {
                misses.entry((x.kind.clone(), m)).or_default().push(x);
                continue;
            }
        };
        let probe =
            |code: &str| DrcWaiver { code: code.to_string(), subjects: subjects.clone(), reason: String::new() };
        let hit = printed.iter().find(|(c, s)| codes.contains(&c.as_str()) && probe(c).covers(c, s));
        let Some((code, _)) = hit else {
            misses.entry((x.kind.clone(), Miss::NoFinding)).or_default().push(x);
            continue;
        };
        let reason = match &x.comment {
            Some(c) => format!("KiCad DRC exclusion ({}): {c}", x.kind),
            None => format!("KiCad DRC exclusion ({}), imported with the board", x.kind),
        };
        let board = p.board_mut();
        if !board.drc_waivers.iter().any(|w| w.code == *code && w.subjects == subjects) {
            board.drc_waivers.push(DrcWaiver { code: code.clone(), subjects, reason });
            report.waived += 1;
        }
    }
    for ((kind, why), xs) in misses {
        report.not_matched += xs.len();
        let n = xs.len();
        let message = match why {
            Miss::NoCounterpart => {
                format!("{n} KiCad DRC exclusion(s) of `{kind}` not imported: cadlab has no check of that type")
            }
            Miss::ItemMissing => format!(
                "{n} KiCad DRC exclusion(s) of `{kind}` not imported: they name items the board import left out"
            ),
            Miss::NoFinding => {
                format!("{n} KiCad DRC exclusion(s) of `{kind}` not imported: cadlab's DRC does not find the violation")
            }
        };
        let mut d = Diagnostic::info("import.drc_exclusion", message).with_hint(
            "nothing to do; if `drc.run` reports a finding you accept on purpose, waive it with \
             `drc.waive <code> --subjects ... --reason ...`",
        );
        for x in &xs {
            for u in &x.items {
                if let Some(l) = uuids.get(u).filter(|l| *l != "edge")
                    && let Ok(r) = ObjectRef::parse(l)
                    && !d.subjects.contains(&r)
                {
                    d = d.with_subject(r);
                }
            }
        }
        diags.push(d);
    }
    (report, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries() {
        let x = Exclusion::parse(
            "courtyards_overlap|107972313|69250001|00000000-0000-0000-0000-00005ff50a4b|97020680-52fd-41ee-a656-0dc60c3ba581",
            None,
        )
        .unwrap();
        assert_eq!(x.kind, "courtyards_overlap");
        assert_eq!(x.items, ["00000000-0000-0000-0000-00005ff50a4b", "97020680-52fd-41ee-a656-0dc60c3ba581"]);
        let x = Exclusion::parse("copper_sliver|1|2|00000000-0000-0000-0000-000000000000|F.Cu", None).unwrap();
        assert!(x.items.is_empty());
        assert_eq!(x.layer.as_deref(), Some("F.Cu"));
        let v = serde_json::json!(["clearance|1|2|00000000-0000-0000-0000-0000000000a1", " on purpose "]);
        let x = Exclusion::from_json(&v).unwrap();
        assert_eq!(x.comment.as_deref(), Some("on purpose"));
        assert_eq!(x.items.len(), 1);
        assert!(Exclusion::from_json(&serde_json::json!(3)).is_none());
    }
}
