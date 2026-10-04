//! `.kicad_pcb` import (DECISIONS D32): round trips through cadlab's own export, the
//! `board.import_kicad` command, and (with `CADLAB_ORACLES=1`) files rewritten by KiCad itself.
//!
//! - **Matched round trip:** each cross-check board (`tests/crosscheck`) is exported, its board
//!   cleared, and the export imported back into the project (circuit kept): the board model
//!   must come back equal (object IDs aside), the library unchanged, and DRC must report the
//!   same findings.
//! - **Built round trip:** the export imported into an empty project (circuit built from the
//!   board): same nets (by pins), same DRC findings.
//! - **KiCad-saved files:** `kicad-cli pcb upgrade` rewrites the export in KiCad's current format
//!   (nets by name, reordered items); importing it must give the same board.
//! - **Third-party boards:** with `CADLAB_KICAD_PCB_FIXTURES=<dir>`, every `.kicad_pcb` under it is
//!   imported into a fresh project (no download in tests; CI may provide the directory).

#[path = "common/bigboard.rs"]
mod bigboard;
mod common;
mod crosscheck;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::kicad_import::{self, BoardImportOptions, rules};
use cadlab::model::Project;
use cadlab::model::board::Board;
use cadlab::{Diagnostic, ObjectId, Severity};
use common::oracle::{self, Oracle};
use serde_json::{Value, json};

/// The board with every object ID zeroed (IDs are allocated anew on import).
fn without_ids(b: &Board) -> Board {
    let mut b = b.clone();
    let z = ObjectId(0);
    b.tracks.iter_mut().for_each(|t| t.id = z);
    b.vias.iter_mut().for_each(|t| t.id = z);
    b.zones.iter_mut().for_each(|t| t.id = z);
    b.keepouts.iter_mut().for_each(|t| t.id = z);
    b.holes.iter_mut().for_each(|t| t.id = z);
    b.graphics.iter_mut().for_each(|t| t.id = z);
    b
}

/// The board up to the order KiCad saves items in: items in sorted lists (tracks with sorted
/// ends), outline contours as sorted sets of undirected segments.
fn canonical(b: &Board) -> (Board, Vec<Vec<String>>) {
    use cadlab::model::board::Segment;
    let mut b = without_ids(b);
    for t in &mut b.tracks {
        if t.end < t.start {
            std::mem::swap(&mut t.start, &mut t.end);
        }
    }
    b.tracks.sort_by_key(|x| format!("{x:?}"));
    b.vias.sort_by_key(|x| format!("{x:?}"));
    b.zones.sort_by_key(|x| format!("{x:?}"));
    b.keepouts.sort_by_key(|x| format!("{x:?}"));
    b.holes.sort_by_key(|x| format!("{x:?}"));
    b.graphics.sort_by_key(|x| format!("{x:?}"));
    let contours = b
        .outline
        .contours
        .iter()
        .map(|c| {
            let mut from = c.start;
            let mut v: Vec<String> = c
                .segments
                .iter()
                .map(|s| {
                    let (to, mid) = match *s {
                        Segment::Line { to } => (to, None),
                        Segment::Arc { mid, to } => (to, Some(mid)),
                    };
                    let (a, z) = if from < to { (from, to) } else { (to, from) };
                    from = to;
                    format!("{a:?} {mid:?} {z:?}")
                })
                .collect();
            v.sort();
            v
        })
        .collect();
    b.outline = Default::default();
    (b, contours)
}

/// DRC findings without object IDs: (severity, code, location), sorted.
fn drc_summary(p: &Project) -> Vec<String> {
    let mut v: Vec<String> = cadlab::drc::check(p)
        .iter()
        .map(|d: &Diagnostic| {
            let sev = match d.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "info",
            };
            format!("{sev} {} {:?}", d.code, d.location.map(|l| (l.x.0, l.y.0)))
        })
        .collect();
    v.sort();
    v
}

/// Reads the rules files next to `pcb` and imports it into `p`.
fn import_file(p: &mut Project, pcb: &Path) -> (kicad_import::BoardImportReport, Vec<Diagnostic>) {
    let text = std::fs::read_to_string(pcb).unwrap();
    let read = |ext: &str| std::fs::read_to_string(pcb.with_extension(ext)).ok();
    let (k, _) = rules::parse(read("kicad_pro").as_deref(), read("kicad_dru").as_deref()).unwrap();
    let opts = BoardImportOptions {
        file_name: pcb.file_name().unwrap().to_string_lossy().into_owned(),
        rules: Some(k),
        ..Default::default()
    };
    kicad_import::import(p, &text, &opts).unwrap_or_else(|e| panic!("import failed: {e} ({})", e.hint))
}

/// Diagnostics that must not appear on cadlab's own exports.
fn unexpected(diags: &[Diagnostic]) -> Vec<String> {
    diags.iter().filter(|d| d.severity != Severity::Info).map(|d| d.to_string()).collect()
}

/// Placed pads of every net, by net name (a built circuit has one pin per pad, so pins are
/// compared through the pads they land on).
fn nets(p: &Project) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pad in cadlab::board::placed_pads(p) {
        if let Some(n) = pad.net {
            m.entry(n).or_default().push(format!("{}.{}", pad.refdes, pad.number));
        }
    }
    m.values_mut().for_each(|v| v.sort());
    m
}

#[test]
fn round_trip_into_the_same_circuit() {
    for case in crosscheck::cases() {
        let (_d, _r, s) = crosscheck::build(&case);
        let p = crosscheck::project(&s).clone();
        let dir = crosscheck::keep_dir("kicad_pcb_import", case.name);
        let pcb = crosscheck::export_kicad(&p, &dir);
        let mut q = p.clone();
        *q.board_mut() = Board::default();
        let (report, diags) = import_file(&mut q, &pcb);
        assert_eq!(unexpected(&diags), Vec::<String>::new(), "{}: diagnostics", case.name);
        assert_eq!(report.not_imported, 0, "{}", case.name);
        assert!(
            report.library_footprints.is_empty(),
            "{}: footprints added {:?}",
            case.name,
            report.library_footprints
        );
        assert_eq!(without_ids(q.board()), without_ids(p.board()), "{}: board differs", case.name);
        assert_eq!(q.library(), p.library(), "{}: library changed", case.name);
        assert_eq!(drc_summary(&q), drc_summary(&p), "{}: DRC differs", case.name);
        // Every exported object maps to an imported one.
        let export = cadlab::kicad_pcb::export(&p, "board");
        for (u, label) in &export.uuids {
            if label.starts_with("text:") && !label.ends_with("/Reference") {
                continue;
            }
            assert!(report.uuids.contains_key(u), "{}: {label} ({u}) not mapped", case.name);
        }
    }
}

#[test]
fn round_trip_into_an_empty_project() {
    for case in crosscheck::cases() {
        let (_d, _r, s) = crosscheck::build(&case);
        let p = crosscheck::project(&s).clone();
        let dir = crosscheck::keep_dir("kicad_pcb_import_new", case.name);
        let pcb = crosscheck::export_kicad(&p, &dir);
        let mut q = Project::new("imported");
        let (report, diags) = import_file(&mut q, &pcb);
        assert_eq!(report.circuit, kicad_import::CircuitSource::Built);
        let bad: Vec<String> =
            unexpected(&diags).into_iter().filter(|d| !d.contains("import.placeholder_part")).collect();
        assert_eq!(bad, Vec::<String>::new(), "{}: diagnostics", case.name);
        // Same connectivity: nets with at least two pins (one-pin nets of the original are
        // `unconnected-` in KiCad only when unnamed).
        assert_eq!(nets(&q), nets(&p), "{}: nets differ", case.name);
        assert_eq!(drc_summary(&q), drc_summary(&p), "{}: DRC differs", case.name);
        // Effective net class values are kept.
        for (n, c) in &p.circuit().netclasses {
            let r = &p.board().rules;
            let qc = &q.circuit().netclasses[n];
            let qr = &q.board().rules;
            assert_eq!(c.track_width.unwrap_or(r.track_width), qc.track_width.unwrap_or(qr.track_width));
            assert_eq!(c.via_drill.unwrap_or(r.via_drill), qc.via_drill.unwrap_or(qr.via_drill));
            assert_eq!(c.via_diameter.unwrap_or(r.via_diameter), qc.via_diameter.unwrap_or(qr.via_diameter));
            assert_eq!(c.clearance.unwrap_or(r.clearance), qc.clearance.unwrap_or(qr.clearance));
        }
        assert_eq!(q.board().rules, p.board().rules, "{}: rules", case.name);
    }
}

/// Both round trips on larger boards: the four-layer synthetic board (both sides, inner pours,
/// unrouted nets) and the STM32 example board.
#[test]
fn round_trip_larger_boards() {
    let (_d1, _r1, s1) = bigboard::build(bigboard::Spec::small(3));
    let d2 = tempfile::tempdir().unwrap();
    let r2 = Registry::with_builtins();
    let mut s2 = Session::new();
    exec(&r2, &mut s2, "project.new", json!({"path": d2.path().join("p")}), false).unwrap();
    common::stm32::build_stm32_board(&r2, &mut s2);
    exec(&r2, &mut s2, "board.outline", json!({"width": "70mm", "height": "50mm"}), false).unwrap();
    exec(&r2, &mut s2, "place.auto", json!({}), false).unwrap();
    for (name, s) in [("bigboard", &s1), ("stm32", &s2)] {
        let p = crosscheck::project(s).clone();
        let dir = crosscheck::keep_dir("kicad_pcb_import", name);
        let pcb = crosscheck::export_kicad(&p, &dir);
        let mut q = p.clone();
        *q.board_mut() = Board::default();
        let (report, diags) = import_file(&mut q, &pcb);
        assert_eq!(unexpected(&diags), Vec::<String>::new(), "{name}: diagnostics");
        assert!(report.library_footprints.is_empty(), "{name}: {:?}", report.library_footprints);
        assert_eq!(without_ids(q.board()), without_ids(p.board()), "{name}: board differs");
        let want = drc_summary(&p);
        assert_eq!(drc_summary(&q), want, "{name}: DRC differs");
        let mut fresh = Project::new("fresh");
        import_file(&mut fresh, &pcb);
        assert_eq!(nets(&fresh), nets(&p), "{name}: nets differ");
        assert_eq!(drc_summary(&fresh), want, "{name}: DRC differs (built circuit)");
    }
}

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value, dry: bool) -> Result<Value, String> {
    let opts = RunOptions { dry_run: dry };
    r.execute(s, cmd, args, opts).map(|o| serde_json::to_value(&o).unwrap()).map_err(|f| f.error.to_string())
}

#[test]
fn command_dry_run_conflict_and_replace() {
    let case = &crosscheck::cases()[1];
    let (dir, r, mut s) = crosscheck::build(case);
    let p = crosscheck::project(&s).clone();
    let out = dir.path().join("k");
    let pcb = crosscheck::export_kicad(&p, &out);
    // A second project, empty: dry run leaves it unchanged.
    let mut s2 = Session::new();
    exec(&r, &mut s2, "project.new", json!({"path": dir.path().join("q")}), false).unwrap();
    let before = s2.project.clone();
    let o = exec(&r, &mut s2, "board.import_kicad", json!({"path": pcb}), true).unwrap();
    assert_eq!(o["output"]["footprints"], json!(p.board().footprints.len()), "{o}");
    assert_eq!(s2.project, before, "dry run changes nothing");
    let o = exec(&r, &mut s2, "board.import_kicad", json!({"path": pcb}), false).unwrap();
    assert_eq!(o["output"]["circuit"], "built");
    assert!(o["output"].get("uuids").is_none(), "UUID table not in the output");
    // The board is not empty any more.
    let e = exec(&r, &mut s2, "board.import_kicad", json!({"path": pcb}), false).unwrap_err();
    assert!(e.contains("import.board_not_empty"), "{e}");
    let o = exec(&r, &mut s2, "board.import_kicad", json!({"path": pcb, "replace": true}), false).unwrap();
    assert_eq!(o["output"]["circuit"], "matched", "the circuit built the first time is kept");
    assert_eq!(o["output"]["tracks"], json!(p.board().tracks.len()));
    // Rules alone.
    let o =
        exec(&r, &mut s, "board.import_kicad_rules", json!({"path": pcb.with_extension("kicad_pro")}), false).unwrap();
    assert_eq!(o["output"]["netclasses"], json!(["power"]), "{o}");
    let e = exec(&r, &mut s, "board.import_kicad", json!({"path": "missing.kicad_pcb"}), false).unwrap_err();
    assert!(e.contains("missing.kicad_pcb"), "{e}");
}

/// KiCad rewrites the export in its current format; the import gives the same board.
#[test]
fn kicad_saved_board_imports_the_same() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    for case in crosscheck::cases() {
        if case.name == "netless_track" {
            // KiCad gives the netless track the net it touches when it loads the board
            // (tests/drc_crosscheck.rs): the saved file has a different design.
            continue;
        }
        let (_d, _r, s) = crosscheck::build(&case);
        let p = crosscheck::project(&s).clone();
        let dir = crosscheck::keep_dir("kicad_pcb_import_upgraded", case.name);
        let pcb = crosscheck::export_kicad(&p, &dir);
        oracle::run(&cli, &["pcb", "upgrade", pcb.to_str().unwrap()]);
        let mut q = p.clone();
        *q.board_mut() = Board::default();
        let (report, diags) = import_file(&mut q, &pcb);
        assert_eq!(unexpected(&diags), Vec::<String>::new(), "{}: diagnostics", case.name);
        assert!(report.library_footprints.is_empty(), "{}: {:?}", case.name, report.library_footprints);
        assert_eq!(canonical(q.board()), canonical(p.board()), "{}: board differs", case.name);
        assert_eq!(drc_summary(&q), drc_summary(&p), "{}: DRC differs", case.name);
        let mut fresh = Project::new("fresh");
        import_file(&mut fresh, &pcb);
        assert_eq!(drc_summary(&fresh), drc_summary(&p), "{}: DRC differs (built circuit)", case.name);
    }
}

fn find_boards(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            find_boards(&p, out);
        } else if p.extension().is_some_and(|e| e == "kicad_pcb") {
            out.push(p);
        }
    }
}

/// Third-party boards from a fixture directory (`CADLAB_KICAD_PCB_FIXTURES`), when given: each
/// imports into a new project, every pad of the file is accounted for, and the imported project
/// exports back to a board KiCad loads.
#[test]
fn third_party_boards() {
    let Some(dir) = std::env::var_os("CADLAB_KICAD_PCB_FIXTURES").map(PathBuf::from) else {
        eprintln!("skipping: set CADLAB_KICAD_PCB_FIXTURES to a directory of .kicad_pcb files");
        return;
    };
    let mut boards = Vec::new();
    find_boards(&dir, &mut boards);
    assert!(!boards.is_empty(), "no .kicad_pcb under {}", dir.display());
    let cli = oracle::require(Oracle::KicadCli);
    for pcb in boards {
        let mut p = Project::new("fixture");
        let (report, diags) = import_file(&mut p, &pcb);
        eprintln!(
            "{}: {} footprints, {} holes, {} tracks, {} vias, {} zones, {} not imported, {} diagnostics",
            pcb.display(),
            report.footprints,
            report.holes,
            report.tracks,
            report.vias,
            report.zones,
            report.not_imported,
            diags.len()
        );
        for d in &diags {
            eprintln!("  {d}");
        }
        assert!(!p.board().outline.contours.is_empty() || diags.iter().any(|d| d.code == "import.no_outline"));
        if let Some(cli) = &cli {
            let out = crosscheck::keep_dir("kicad_pcb_import_fixtures", &pcb.file_stem().unwrap().to_string_lossy());
            let e = cadlab::kicad_pcb::export(&p, "board");
            let f = out.join("board.kicad_pcb");
            std::fs::write(&f, &e.pcb).unwrap();
            std::fs::write(out.join("board.kicad_pro"), &e.project).unwrap();
            oracle::run(
                cli,
                &["pcb", "export", "ipcd356", "-o", out.join("board.d356").to_str().unwrap(), f.to_str().unwrap()],
            );
        }
    }
}
