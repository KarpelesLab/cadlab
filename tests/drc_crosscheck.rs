//! Cross-check: cadlab's DRC against KiCad's on the same boards (docs/TESTING.md, "Cross-checks").
//!
//! Each board of `crosscheck::cases()` (a clean routed board with a pour, then one deliberate
//! violation per rule) is checked by `cadlab::drc::check` and by `kicad-cli pcb drc` on its
//! `.kicad_pcb` export, and the two sets are compared finding by finding with the matching of
//! `tests/common/drc_compare.rs` (rule, items through the exporter's UUID table, measured value,
//! item anchors). Every unmatched finding on either side must be covered by an allowlist entry
//! with a reason ([`drc_compare::ALLOW`] and [`CASE_ALLOW`]); every entry must be used. Runs only
//! with `CADLAB_ORACLES=1` (KiCad is an external process, DECISIONS D7). Reports are kept in
//! `$CARGO_TARGET_TMPDIR/drc_crosscheck/<case>/`.
//!
//! A second test checks the `.kicad_pcb` importer the same way: KiCad's DRC of each export
//! against cadlab's DRC of the board imported back from it into an empty project (D32).

mod common;
mod crosscheck;
#[path = "common/drc_compare.rs"]
mod drc_compare;

use std::collections::BTreeMap;
use std::path::Path;

use cadlab::kicad_pcb::Frame;
use cadlab::model::Project;
use common::oracle::{self, Oracle};
use drc_compare::{ALLOW, Allow, Side};
use serde_json::Value;

/// Allowances for single cross-check boards, on top of [`ALLOW`].
const CASE_ALLOW: &[Allow<'static>] = &[
    Allow {
        case: Some("netless_track"),
        side: Side::Cadlab,
        rule: "drc.short",
        when: None,
        max: None,
        involving: &[],
        reason: "copper without a net touching a net is a short for cadlab (the design must say what each track \
                 carries); KiCad gives such a track the net it touches when it loads the board",
    },
    Allow {
        case: Some("netless_track"),
        side: Side::Kicad,
        rule: "track_width",
        when: None,
        max: None,
        involving: &[],
        reason: "KiCad assigned the netless 0.25 mm track to GND (see the drc.short entry), whose power class asks \
                 for 0.4 mm; cadlab checks the class width of a track's own net, and it has none",
    },
];

fn allowlist() -> Vec<Allow<'static>> {
    ALLOW.iter().chain(CASE_ALLOW).cloned().collect()
}

/// Compares KiCad's DRC report on the export of a cross-check case with cadlab's DRC on `p`
/// (the case's project, or the project imported back from the export). `uuids` maps KiCad
/// UUIDs to `p`'s objects and `frame` KiCad coordinates to `p`'s.
#[allow(clippy::too_many_arguments)]
fn compare_case(
    case: &crosscheck::Case,
    p: &Project,
    report: &Value,
    uuids: &BTreeMap<String, String>,
    frame: &Frame,
    allow: &[Allow],
    used: &mut [usize],
    problems: &mut Vec<String>,
) {
    let b = drc_compare::Board { name: case.name, what: case.what, expect: case.expect, verbose: true };
    let ours = drc_compare::cadlab_findings(p);
    drc_compare::compare(&b, p, &ours, report, uuids, frame, allow, used, problems);
}

fn kicad_drc(cli: &Path, pcb: &Path) -> Value {
    drc_compare::kicad_drc(cli, pcb, &pcb.with_file_name("drc.json"), &[])
}

#[test]
fn kicad_and_cadlab_drc_agree() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let mut problems = Vec::new();
    let allow = allowlist();
    let mut used = vec![0; allow.len()];
    for case in crosscheck::cases() {
        let (_d, _r, s) = crosscheck::build(&case);
        let p = crosscheck::project(&s);
        let dir = crosscheck::keep_dir("drc_crosscheck", case.name);
        let export = cadlab::kicad_pcb::export(p, "board");
        let pcb = crosscheck::export_kicad(p, &dir);
        let report = kicad_drc(&cli, &pcb);
        compare_case(&case, p, &report, &export.uuids, &Frame::of(p), &allow, &mut used, &mut problems);
    }
    drc_compare::unused_entries(&allow, &used, &mut problems);
    assert!(problems.is_empty(), "DRC cross-check differences:\n{}", problems.join("\n"));
}

/// The same comparison with cadlab's side checked on the board imported back from the export
/// into an empty project (`board.import_kicad`, circuit built from the board, D32): KiCad's DRC
/// of the original export must match cadlab's DRC of the import, finding by finding, through
/// the import's UUID table.
#[test]
fn kicad_drc_of_export_matches_cadlab_drc_of_import() {
    use cadlab::kicad_import::{self, BoardImportOptions, rules};
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let mut problems = Vec::new();
    let allow = allowlist();
    let mut used = vec![0; allow.len()];
    for case in crosscheck::cases() {
        let (_d, _r, s) = crosscheck::build(&case);
        let p = crosscheck::project(&s);
        let dir = crosscheck::keep_dir("drc_crosscheck_import", case.name);
        let pcb = crosscheck::export_kicad(p, &dir);
        let report = kicad_drc(&cli, &pcb);
        let read = |ext: &str| std::fs::read_to_string(pcb.with_extension(ext)).ok();
        let (k, _) = rules::parse(read("kicad_pro").as_deref(), read("kicad_dru").as_deref()).unwrap();
        let opts = BoardImportOptions { rules: Some(k), ..Default::default() };
        let mut q = Project::new("imported");
        let (imported, _) = kicad_import::import(&mut q, &std::fs::read_to_string(&pcb).unwrap(), &opts).unwrap();
        let frame = Frame { origin: imported.origin };
        compare_case(&case, &q, &report, &imported.uuids, &frame, &allow, &mut used, &mut problems);
    }
    drc_compare::unused_entries(&allow, &used, &mut problems);
    assert!(problems.is_empty(), "DRC cross-check differences (imported boards):\n{}", problems.join("\n"));
}
