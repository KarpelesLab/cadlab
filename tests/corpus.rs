//! Open-source KiCad projects as import and oracle inputs (docs/TESTING.md, "Open-source corpus";
//! DECISIONS D35).
//!
//! The projects of `tests/corpus/projects.toml` are pinned commits of third-party repositories,
//! fetched by `scripts/fetch-corpus.sh` into a directory outside the tracked files and found
//! through `CADLAB_CORPUS_DIR`; this test never downloads anything and skips when the variable is
//! unset. Per project:
//!
//! 1. **Netlist:** `kicad-cli sch export netlist` of the root schematic, read with
//!    `circuit.import` into a new project (D13, D27). Board-only projects skip this; their
//!    circuit is built from the board.
//! 2. **Board:** `board.import_kicad` of the `.kicad_pcb` (with its `.kicad_pro` / `.kicad_dru`)
//!    into that project, cadlab's origin on KiCad's auxiliary origin, so cadlab coordinates are
//!    the ones KiCad plots.
//! 3. **DRC of the original:** cadlab's DRC of the import against `kicad-cli pcb drc` of the
//!    original board, finding by finding (`tests/common/drc_compare.rs`), items mapped through
//!    the importer's UUID table.
//! 4. **Re-export:** `board.export_kicad` of the import; KiCad must load it, and its DRC of the
//!    re-export is compared with cadlab's the same way (through the exporter's UUID table).
//! 5. **Gerbers:** cadlab's Gerbers and drill files of the import against KiCad's of the
//!    original board, rendered by gerbv and XORed (`tests/common/gerber_compare.rs`).
//! 6. **Re-route** (`CADLAB_CORPUS_ROUTE=1`, informative): tracks and vias ripped up and routed
//!    again by cadlab's router; completion is reported, never a failure.
//!
//! Differences without an allowance (global ones in `drc_compare::ALLOW` and [`CORPUS_ALLOW`],
//! board-specific `known` entries in the manifest, each with a reason) fail the test. A summary
//! table goes to stderr and a JSON report to `$CARGO_TARGET_TMPDIR/corpus/report.json`, next to
//! every intermediate file. Without `CADLAB_ORACLES=1`, only the board import (circuit built from
//! the board), cadlab's DRC and the internal re-export round trip run.
//!
//! `CADLAB_CORPUS_ONLY=name,name` limits the run to some projects. Example:
//! `scripts/fetch-corpus.sh && CADLAB_CORPUS_DIR=target/corpus CADLAB_ORACLES=1 cargo test --release
//! --test corpus -- --nocapture`.
#![cfg(feature = "png")]

mod common;
#[path = "common/drc_compare.rs"]
mod drc_compare;
#[path = "common/gerber_compare.rs"]
mod gerber_compare;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use cadlab::kicad_import::{self, BoardImportOptions, OriginMode, rules};
use cadlab::kicad_pcb::Frame;
use cadlab::model::Project;
use cadlab::netlist::import as netlist_import;
use cadlab::{Diagnostic, Severity};
use common::oracle::{self, Oracle};
use drc_compare::{Allow, Side};
use gerber_compare::{LayerSpec, Mode, Window};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MANIFEST: &str = include_str!("corpus/projects.toml");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    project: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    repo: String,
    sha: String,
    sparse: Vec<String>,
    pcb: String,
    #[serde(default)]
    sch: Option<String>,
    license: String,
    layers: u8,
    kicad: u8,
    notes: String,
    #[serde(default)]
    known: Vec<Known>,
    #[serde(default)]
    gerber_known: Vec<GerberKnown>,
}

/// A Gerber layer (or `drill`) known to differ on a board: reported, not failed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GerberKnown {
    layer: String,
    reason: String,
}

/// A board-specific expected difference.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Known {
    side: String,
    rule: String,
    max: usize,
    reason: String,
    /// `original` or `reexport` (absent: both comparisons).
    #[serde(default)]
    stage: Option<String>,
}

fn manifest() -> Manifest {
    toml::from_str(MANIFEST).expect("tests/corpus/projects.toml")
}

/// Rules only one tool has (cadlab checks a subset of KiCad's rules and has a few of its own,
/// docs/BOARD.md): reported by that tool only, on any board.
const ONE_SIDED_RULES: &[(Side, &str, &str)] = &[
    (Side::Kicad, "silk_edge_clearance", "cadlab has no silkscreen-to-board-edge rule"),
    (
        Side::Kicad,
        "text_height",
        "cadlab has no minimum text height rule (KiCad's text_height checks board and footprint texts)",
    ),
    (Side::Kicad, "text_thickness", "cadlab has no minimum text stroke rule"),
    (
        Side::Kicad,
        "solder_mask_bridge",
        "cadlab has no solder mask web/bridge rule (mask openings of different nets merging)",
    ),
    (Side::Kicad, "footprint_type_mismatch", "cadlab has no footprint attribute (SMD/THT) consistency rule"),
    (Side::Kicad, "starved_thermal", "cadlab has no minimum thermal spoke count rule"),
    (Side::Kicad, "copper_sliver", "cadlab has no copper sliver rule"),
    (Side::Kicad, "nonmirrored_text_on_back_layer", "cadlab has no text mirroring rule"),
    (Side::Kicad, "npth_inside_courtyard", "cadlab has no rule for holes inside courtyards"),
    (Side::Kicad, "pth_inside_courtyard", "cadlab has no rule for holes inside courtyards"),
    (
        Side::Kicad,
        "hole_clearance",
        "cadlab has no hole-to-copper clearance rule (its clearance is copper to copper, hole_to_hole hole to hole)",
    ),
    (
        Side::Cadlab,
        "drc.footprint_outside",
        "KiCad has no rule for a courtyard extending beyond the board edge (edge connectors, castellated \
         modules, overhanging USB receptacles); its copper_edge_clearance checks copper only",
    ),
];

/// Allowances for the comparison with KiCad's DRC of the original board only (on the re-export,
/// cadlab's `.kicad_pro` / `.kicad_dru` make KiCad check the same).
const ORIGINAL_ONLY_RULES: &[(Side, &str, &str)] = &[(
    Side::Cadlab,
    "drc.track_width_class",
    "KiCad uses net class track widths only as defaults for new tracks and never checks existing ones; cadlab \
     warns when a track is narrower than its class asks for (its KiCad export adds a .kicad_dru rule for it)",
)];

/// Allowances for the comparison with KiCad's DRC of the re-export only.
const REEXPORT_ONLY_RULES: &[(Side, &str, &str)] = &[(
    Side::Cadlab,
    "drc.unplaced",
    "the re-export has no schematic, so KiCad cannot report components missing from the board; on the original \
     board, KiCad's schematic parity check (missing_footprint) is compared with drc.unplaced",
)];

/// Allowances for third-party boards, on top of [`drc_compare::ALLOW`].
fn corpus_allow() -> Vec<Allow<'static>> {
    let mut v: Vec<Allow<'static>> = ONE_SIDED_RULES
        .iter()
        .chain(ORIGINAL_ONLY_RULES)
        .chain(REEXPORT_ONLY_RULES)
        .map(|&(side, rule, reason)| Allow { case: None, side, rule, when: None, max: None, involving: &[], reason })
        .collect();
    v.push(Allow {
        case: None,
        side: Side::Kicad,
        rule: "silk_over_copper",
        when: Some(names_board_text),
        max: None,
        involving: &[],
        reason: "cadlab's silk_over_pad checks silkscreen drawings, not texts (board texts have no outline in \
                 cadlab's DRC; their font is the writer's)",
    });
    v
}

/// The finding names a board text (`PCB text 'GND' on F.Silkscreen`).
fn names_board_text(f: &drc_compare::Finding) -> bool {
    f.text.contains("PCB text '")
}

/// Whether an entry of [`corpus_allow`] applies to one comparison only: `Some(true)` the original
/// board's, `Some(false)` the re-export's.
fn stage_of(a: &Allow) -> Option<bool> {
    let is = |list: &[(Side, &str, &str)]| list.iter().any(|&(s, r, _)| s == a.side && r == a.rule);
    if is(ORIGINAL_ONLY_RULES) {
        Some(true)
    } else if is(REEXPORT_ONLY_RULES) {
        Some(false)
    } else {
        None
    }
}

/// cadlab code of each KiCad DRC type, for rules a project sets to `ignore`.
fn cadlab_codes(kicad_type: &str) -> &'static [&'static str] {
    drc_compare::TYPE_MAP.iter().find(|(t, _)| *t == kicad_type).map(|(_, c)| *c).unwrap_or(&[])
}

/// Directory kept after the run (`$CARGO_TARGET_TMPDIR/corpus/<name>`), emptied first.
fn keep_dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("corpus").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Diagnostics counted by code.
fn by_code(diags: &[Diagnostic]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for d in diags {
        *m.entry(d.code.to_string()).or_default() += 1;
    }
    m
}

/// One project's results, for the summary table and the JSON report.
#[derive(Debug, Default, Serialize)]
struct Row {
    name: String,
    license: String,
    layers: u8,
    kicad: u8,
    status: String,
    netlist: Option<Value>,
    import: Option<Value>,
    import_diagnostics: BTreeMap<String, usize>,
    drc_original: Option<Value>,
    drc_reexport: Option<Value>,
    gerbers: Option<Value>,
    drill: Option<Value>,
    route: Option<Value>,
    seconds: f64,
    problems: Vec<String>,
}

fn outcome_json(o: &drc_compare::Outcome) -> Value {
    let flat = |m: &BTreeMap<(&'static str, String), usize>| -> BTreeMap<String, usize> {
        m.iter().map(|((s, r), n)| (format!("{s}:{r}"), *n)).collect()
    };
    json!({
        "kicad": o.totals.0,
        "cadlab": o.totals.1,
        "matched": o.matched,
        "matched_by_rule": o.matched_by_rule,
        "same_defect_per_segment": o.per_segment,
        "beyond_kicad_report_cap": o.beyond_cap,
        "allowed": flat(&o.allowed),
        "unexplained": flat(&o.unexplained),
    })
}

/// Rule severities the project sets to `ignore` (`board.design_settings.rule_severities`).
fn ignored_rules(pro: Option<&str>) -> Vec<String> {
    let Some(v) = pro.and_then(|t| serde_json::from_str::<Value>(t).ok()) else { return Vec::new() };
    let Some(m) = v["board"]["design_settings"]["rule_severities"].as_object() else { return Vec::new() };
    m.iter().filter(|(_, s)| s.as_str() == Some("ignore")).map(|(k, _)| k.clone()).collect()
}

/// Copies the checked-out files (not `.git`) into `dir`; KiCad then works on the copy, so the
/// fetched checkout stays untouched.
fn copy_tree(src: &Path, dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let (p, name) = (e.path(), e.file_name());
        if name == ".git" || name == ".cadlab-corpus" {
            continue;
        }
        if p.is_dir() {
            copy_tree(&p, &dir.join(&name));
        } else {
            std::fs::copy(&p, dir.join(&name)).unwrap();
        }
    }
}

/// The board text without its zone fills (`(filled_polygon ...)` and older `(fill_segments
/// ...)` lists): KiCad then plots the zones empty unless asked to refill them.
fn strip_zone_fills(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let next = ["(filled_polygon", "(fill_segments"].iter().filter_map(|m| rest.find(m)).min();
        let Some(i) = next else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..i]);
        // Skip the balanced expression (no parentheses inside these lists' strings).
        let mut depth = 0usize;
        let mut end = rest.len();
        for (j, c) in rest[i..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + j + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &rest[end..];
    }
}

struct Tools {
    cli: PathBuf,
    gerbv: PathBuf,
}

fn run_project(
    e: &Entry,
    root: &Path,
    tools: Option<&Tools>,
    base_allow: &[Allow<'static>],
    used: &mut [usize],
) -> Row {
    let started = Instant::now();
    let mut row = Row {
        name: e.name.clone(),
        license: e.license.clone(),
        layers: e.layers,
        kicad: e.kicad,
        ..Default::default()
    };
    let src = root.join(&e.name);
    if !src.is_dir() {
        row.status = "missing".into();
        row.problems.push(format!("not in {} (run scripts/fetch-corpus.sh)", root.display()));
        return row;
    }
    let marker = std::fs::read_to_string(src.join(".cadlab-corpus")).unwrap_or_default();
    if marker.split_whitespace().next() != Some(e.sha.as_str()) {
        row.status = "stale".into();
        row.problems.push(format!("checkout is not at {} (run scripts/fetch-corpus.sh)", e.sha));
        return row;
    }
    let out = keep_dir(&e.name);
    copy_tree(&src, &out.join("original"));
    let pcb = out.join("original").join(&e.pcb);
    let mut p = Project::new(&e.name);
    eprintln!("\n##### {} ({}, {} layers, KiCad {})", e.name, e.license, e.layers, e.kicad);

    // 1. Netlist.
    if let (Some(sch), Some(t)) = (&e.sch, tools) {
        let net = out.join("netlist.net");
        oracle::run(
            &t.cli,
            &[
                "sch",
                "export",
                "netlist",
                "-o",
                net.to_str().unwrap(),
                out.join("original").join(sch).to_str().unwrap(),
            ],
        );
        let nl = netlist_import::parse_kicad(&std::fs::read_to_string(&net).unwrap());
        match nl.and_then(|nl| {
            netlist_import::import(
                &mut p,
                &nl,
                &netlist_import::ImportOptions { replace: false, file_name: sch.clone() },
            )
        }) {
            Ok((r, diags)) => {
                let mut res: BTreeMap<String, usize> = BTreeMap::new();
                for part in &r.parts {
                    *res.entry(format!("{:?}", part.resolution).to_lowercase()).or_default() += part.refdes.len();
                }
                row.netlist = Some(json!({
                    "components": r.components, "nets": r.nets, "parts": r.parts.len(), "resolution": res,
                    "power_symbols": r.power_symbols, "unconnected_skipped": r.unconnected_skipped,
                    "diagnostics": by_code(&diags),
                }));
                eprintln!("netlist: {}", row.netlist.as_ref().unwrap());
            }
            Err(err) => {
                row.status = "netlist failed".into();
                row.problems.push(format!("circuit.import: {} ({}): {}", err.code, err.message, err.hint));
                return row;
            }
        }
    }

    // 2. Board import.
    let read = |ext: &str| std::fs::read_to_string(pcb.with_extension(ext)).ok();
    let (pro, dru) = (read("kicad_pro"), read("kicad_dru"));
    let (k, rule_diags) = match rules::parse(pro.as_deref(), dru.as_deref()) {
        Ok(r) => r,
        Err(err) => {
            row.status = "rules failed".into();
            row.problems.push(format!("rules: {} ({})", err.code, err.message));
            return row;
        }
    };
    let opts =
        BoardImportOptions { file_name: e.pcb.clone(), rules: Some(k), origin: OriginMode::Aux, ..Default::default() };
    let t0 = Instant::now();
    let imported = kicad_import::import(&mut p, &std::fs::read_to_string(&pcb).unwrap(), &opts);
    let (report, diags) = match imported {
        Ok(r) => r,
        Err(err) => {
            row.status = "import failed".into();
            row.problems.push(format!("board.import_kicad: {} ({}): {}", err.code, err.message, err.hint));
            return row;
        }
    };
    let diags: Vec<Diagnostic> = rule_diags.into_iter().chain(diags).collect();
    row.import_diagnostics = by_code(&diags);
    row.import = Some(json!({
        "circuit": report.circuit, "footprints": report.footprints, "library_footprints": report.library_footprints.len(),
        "holes": report.holes, "tracks": report.tracks, "vias": report.vias, "zones": report.zones,
        "keepouts": report.keepouts, "graphics": report.graphics, "contours": report.contours,
        "not_imported": report.not_imported, "seconds": t0.elapsed().as_secs_f64(),
    }));
    eprintln!("import: {}", row.import.as_ref().unwrap());
    for d in diags.iter().filter(|d| d.severity != Severity::Info) {
        eprintln!("  {d}");
    }

    // Internal round trip: the export imports back to the same placements and copper.
    let export = cadlab::kicad_pcb::export(&p, "board");
    let mut q = Project::new("reimport");
    let back = kicad_import::import(&mut q, &export.pcb, &BoardImportOptions::default());
    match back {
        Ok((r2, _)) => {
            if (r2.footprints + r2.holes, r2.tracks, r2.vias, r2.zones)
                != (report.footprints + report.holes, report.tracks, report.vias, report.zones)
            {
                row.problems.push(format!(
                    "re-export imports back with {} footprints + holes, {} tracks, {} vias, {} zones (was {}, {}, {}, {})",
                    r2.footprints + r2.holes,
                    r2.tracks,
                    r2.vias,
                    r2.zones,
                    report.footprints + report.holes,
                    report.tracks,
                    report.vias,
                    report.zones
                ));
            }
        }
        Err(err) => row.problems.push(format!("re-export does not import: {} ({})", err.code, err.message)),
    }

    // The imported project, kept for inspection.
    if let Err(err) = p.save(&out.join("project")) {
        row.problems.push(format!("the imported project does not save: {err}"));
    }

    // cadlab DRC (once; both comparisons use it).
    let t0 = Instant::now();
    let cad_diags = cadlab::drc::check(&p);
    std::fs::write(out.join("drc-cadlab.json"), serde_json::to_string_pretty(&cad_diags).unwrap()).unwrap();
    let ours = drc_compare::findings_of(&cad_diags);
    eprintln!("cadlab DRC: {} findings in {:.1} s", ours.len(), t0.elapsed().as_secs_f64());

    let Some(t) = tools else {
        row.drc_original = Some(json!({"cadlab": ours.len()}));
        row.status = if row.problems.is_empty() { "ok (no oracles)".into() } else { "differences".into() };
        row.seconds = started.elapsed().as_secs_f64();
        return row;
    };

    // Allowances: global, rules the project ignores, footprints given a courtyard by the import,
    // and the manifest's known differences. Some apply to one comparison only (`stage`:
    // `Some(true)` the original board's, `Some(false)` the re-export's); each comparison gets the
    // whole list with the other's entries disabled (`max: Some(0)`), so indices stay aligned.
    let mut allow: Vec<Allow> = base_allow.to_vec();
    let mut stage: Vec<Option<bool>> = base_allow.iter().map(stage_of).collect();
    let ignored = ignored_rules(pro.as_deref());
    let ignore_reason = "the project sets this KiCad rule's severity to `ignore` in its .kicad_pro, so KiCad never \
                         reports it; cadlab's DRC has no per-project severities";
    for r in &ignored {
        for code in cadlab_codes(r) {
            allow.push(Allow {
                case: None,
                side: Side::Cadlab,
                rule: code,
                when: None,
                max: None,
                involving: &[],
                reason: ignore_reason,
            });
            stage.push(Some(true));
        }
    }
    // Footprints without a courtyard in KiCad get one from the import (0.25 mm around the pads):
    // cadlab checks their overlaps, KiCad has nothing to check.
    let generated: Vec<String> = diags
        .iter()
        .filter(|d| d.code == "import.courtyard_generated")
        .flat_map(|d| d.subjects.iter().map(|s| s.to_string()))
        .collect();
    allow.push(Allow {
        case: None,
        side: Side::Cadlab,
        rule: "drc.courtyard_overlap",
        when: None,
        max: None,
        involving: &generated,
        reason: "one of the footprints has no courtyard in KiCad (nothing to check there); the import gave it one \
                 0.25 mm around its pads (import.courtyard_generated)",
    });
    stage.push(Some(true));
    for kd in &e.known {
        let side = if kd.side == "kicad" { Side::Kicad } else { Side::Cadlab };
        allow.push(Allow {
            case: Some(&e.name),
            side,
            rule: &kd.rule,
            when: None,
            max: Some(kd.max),
            involving: &[],
            reason: &kd.reason,
        });
        stage.push(match kd.stage.as_deref() {
            Some("original") => Some(true),
            Some("reexport") => Some(false),
            _ => None,
        });
    }
    let mut allow_rx = allow.clone();
    for ((a, rx), s) in allow.iter_mut().zip(allow_rx.iter_mut()).zip(&stage) {
        match s {
            Some(true) => rx.max = Some(0),
            Some(false) => a.max = Some(0),
            None => {}
        }
    }

    let mut local_used = vec![0usize; allow.len()];

    // 3. DRC of the original board (with schematic parity when there is a schematic: KiCad then
    // reports the components missing on the board, as cadlab's drc.unplaced does).
    let parity: &[&str] = if e.sch.is_some() { &["--schematic-parity"] } else { &[] };
    let report_orig = drc_compare::kicad_drc(&t.cli, &pcb, &out.join("drc-original.json"), parity);
    let frame = Frame { origin: report.origin };
    let b = drc_compare::Board { name: &e.name, what: "original board", expect: &[], verbose: false };
    let mut probs = Vec::new();
    let o =
        drc_compare::compare(&b, &p, &ours, &report_orig, &report.uuids, &frame, &allow, &mut local_used, &mut probs);
    row.drc_original = Some(outcome_json(&o));
    row.problems.extend(probs.into_iter().map(|s| format!("DRC original: {s}")));

    // 4. Re-export: KiCad loads it; its DRC against cadlab's.
    let rx = out.join("reexport");
    std::fs::create_dir_all(&rx).unwrap();
    let rpcb = rx.join("board.kicad_pcb");
    std::fs::write(&rpcb, &export.pcb).unwrap();
    std::fs::write(rx.join("board.kicad_pro"), &export.project).unwrap();
    std::fs::write(rx.join("board.kicad_dru"), &export.rules).unwrap();
    let report_rx = drc_compare::kicad_drc(&t.cli, &rpcb, &rx.join("drc.json"), &[]);
    let b = drc_compare::Board { name: &e.name, what: "re-export", expect: &[], verbose: false };
    let mut probs = Vec::new();
    let mut rx_used = vec![0usize; allow.len()];
    let o = drc_compare::compare(
        &b,
        &p,
        &ours,
        &report_rx,
        &export.uuids,
        &Frame::of(&p),
        &allow_rx,
        &mut rx_used,
        &mut probs,
    );
    row.drc_reexport = Some(outcome_json(&o));
    row.problems.extend(probs.into_iter().map(|s| format!("DRC re-export: {s}")));
    if !export.warnings.is_empty() {
        row.drc_reexport.as_mut().unwrap()["export_warnings"] = json!(export.warnings);
    }
    for (u, n) in local_used.iter_mut().zip(&rx_used) {
        *u += n;
    }
    for (u, n) in used.iter_mut().zip(&local_used) {
        *u += n;
    }
    // A known difference that no longer shows up is stale: the manifest must follow fixes.
    let first_known = allow.len() - e.known.len();
    for (kd, n) in e.known.iter().zip(&local_used[first_known..]) {
        if *n == 0 {
            row.problems.push(format!(
                "known difference {} {} no longer seen: remove it from tests/corpus/projects.toml",
                kd.side, kd.rule
            ));
        }
    }

    // 5. Gerbers and drill files. Copper is compared twice: without pours (KiCad's zone fills
    // removed from a copy of the board, cadlab's zones removed), which must agree, and with pours
    // (KiCad refills them), informative since the two fill algorithms differ. Silkscreen is
    // informative too (different fonts; cadlab writes its own reference designators).
    if p.board().outline.contours.is_empty() {
        row.problems.push("no outline: Gerbers not compared".into());
    } else {
        let gdir = out.join("gerbers");
        let ours_dir = gerber_compare::cadlab_outputs(&p, &gdir);
        let theirs_dir = gdir.join("kicad");
        let copper = p.board().stackup.copper_names();
        let mut layers: Vec<String> = copper.clone();
        layers.extend(
            ["F.Mask", "B.Mask", "F.Paste", "B.Paste", "F.SilkS", "B.SilkS", "Edge.Cuts"].iter().map(|s| s.to_string()),
        );
        gerber_compare::kicad_outputs(
            &t.cli,
            &pcb,
            &theirs_dir,
            &layers.join(","),
            &["--no-protel-ext", "--check-zones"],
        );
        let file_of = |l: &str| match l {
            "F.SilkS" => ("-F_SilkS.gbr".to_string(), "-F_Silkscreen.gbr".to_string(), Mode::Silk),
            "B.SilkS" => ("-B_SilkS.gbr".to_string(), "-B_Silkscreen.gbr".to_string(), Mode::Silk),
            "Edge.Cuts" => ("-Edge_Cuts.gbr".to_string(), "-Edge_Cuts.gbr".to_string(), Mode::Profile),
            _ => {
                let s = format!("-{}.gbr", l.replace('.', "_"));
                (s.clone(), s, Mode::Exact)
            }
        };
        let mut gprobs: Vec<(String, String)> = Vec::new();
        let specs_in = |ours: &Path, theirs: &Path, ls: &[String], gprobs: &mut Vec<(String, String)>| {
            let mut specs = Vec::new();
            for l in ls {
                let (a, b, mode) = file_of(l);
                match (gerber_compare::try_find(ours, &a), gerber_compare::try_find(theirs, &b)) {
                    (Some(o), Some(t)) => specs.push(LayerSpec { layer: l.clone(), ours: o, theirs: t, mode }),
                    (x, y) => gprobs.push((
                        l.clone(),
                        format!("{l}: file missing (cadlab {}, KiCad {})", x.is_some(), y.is_some()),
                    )),
                }
            }
            // Masks first: the silkscreen comparison skips mask openings.
            specs.sort_by_key(|s| !s.layer.ends_with(".Mask"));
            specs
        };
        let win = Window::of(&p);
        let mut g = serde_json::Map::new();
        // Without pours.
        let bare_dir = gdir.join("no-pours");
        let mut bare = p.clone();
        bare.board_mut().zones.clear();
        let ours_bare = gerber_compare::cadlab_outputs(&bare, &bare_dir);
        let theirs_bare = bare_dir.join("kicad");
        let stripped = bare_dir.join(pcb.file_name().unwrap());
        std::fs::write(&stripped, strip_zone_fills(&std::fs::read_to_string(&pcb).unwrap())).unwrap();
        gerber_compare::kicad_outputs(&t.cli, &stripped, &theirs_bare, &copper.join(","), &["--no-protel-ext"]);
        eprintln!("  copper without pours:");
        let specs = specs_in(&ours_bare, &theirs_bare, &copper, &mut gprobs);
        for r in gerber_compare::compare_layers(&t.gerbv, &bare, &win, &bare_dir, &specs, false) {
            g.insert(
                format!("{} (no pours)", r.layer),
                json!({"area": [r.area.0, r.area.1], "xor": r.xor, "differences": r.core, "where": r.bbox_mm}),
            );
            gprobs.extend(r.problems().into_iter().map(|m| (r.layer.clone(), format!("without pours: {m}"))));
        }
        // Everything, pours included.
        eprintln!("  all layers, pours refilled by each tool:");
        let specs = specs_in(&ours_dir, &theirs_dir, &layers, &mut gprobs);
        for r in gerber_compare::compare_layers(&t.gerbv, &p, &win, &gdir, &specs, false) {
            g.insert(
                r.layer.clone(),
                json!({"area": [r.area.0, r.area.1], "xor": r.xor, "differences": r.core, "where": r.bbox_mm}),
            );
            if r.mode != Mode::Silk && !copper.contains(&r.layer) {
                gprobs.extend(r.problems().into_iter().map(|m| (r.layer.clone(), m)));
            }
        }
        g.insert("window_mm".into(), json!([win.x0, win.y0, win.w, win.h]));
        g.insert("origin_kicad_mm".into(), json!([report.origin.0.0 as f64 / 1e6, report.origin.1.0 as f64 / 1e6]));
        // Layers the manifest knows to differ are reported, not failed.
        let mut known_layers = Vec::new();
        let mut known_used: BTreeSet<&str> = BTreeSet::new();
        for (layer, msg) in gprobs {
            match e.gerber_known.iter().find(|k| k.layer == layer) {
                Some(k) => {
                    known_used.insert(k.layer.as_str());
                    known_layers.push(format!("{msg} (known: {})", k.reason));
                }
                None => row.problems.push(format!("Gerber {msg}")),
            }
        }
        if !known_layers.is_empty() {
            g.insert("known_differences".into(), json!(known_layers));
        }
        row.gerbers = Some(Value::Object(g));

        // Drill: same hits; slots the import drills as round holes are counted apart.
        let (ha, hb) = (gerber_compare::all_hits(&ours_dir), gerber_compare::all_hits(&theirs_dir));
        let (only_a, only_b) = gerber_compare::unmatched_hits(&ha, &hb, true);
        let slot_center = |s: &gerber_compare::Hit| gerber_compare::Hit { slot: false, ..s.clone() };
        let (kicad_slots, only_b): (Vec<_>, Vec<_>) = only_b.into_iter().partition(|h| h.slot);
        let as_round: Vec<gerber_compare::Hit> = kicad_slots.iter().map(slot_center).collect();
        let (only_a, unmatched_slots) = gerber_compare::unmatched_hits(&only_a, &as_round, true);
        row.drill = Some(json!({
            "cadlab": ha.len(), "kicad": hb.len(), "only_cadlab": only_a.len(), "only_kicad": only_b.len(),
            "kicad_slots_drilled_round": kicad_slots.len() - unmatched_slots.len(),
        }));
        eprintln!(
            "  drill     {} / {} hits, {} / {} unmatched, {} slots drilled round",
            ha.len(),
            hb.len(),
            only_a.len(),
            only_b.len() + unmatched_slots.len(),
            kicad_slots.len() - unmatched_slots.len()
        );
        if !only_a.is_empty() || !only_b.is_empty() || !unmatched_slots.is_empty() {
            let msg = format!(
                "drill hits differ: cadlab only {} {:?}, KiCad only {} {:?}",
                only_a.len(),
                &only_a[..only_a.len().min(3)],
                only_b.len() + unmatched_slots.len(),
                only_b.iter().chain(&unmatched_slots).take(3).collect::<Vec<_>>()
            );
            match e.gerber_known.iter().any(|k| k.layer == "drill") {
                true => {
                    known_used.insert("drill");
                    row.gerbers.as_mut().unwrap()["known_drill_difference"] = json!(msg);
                }
                false => row.problems.push(msg),
            }
        }
        // A known layer difference that no longer shows up is stale.
        for k in &e.gerber_known {
            if !known_used.contains(k.layer.as_str()) {
                row.problems.push(format!(
                    "known Gerber difference on {} no longer seen: remove it from tests/corpus/projects.toml",
                    k.layer
                ));
            }
        }
    }

    // 6. Re-route (informative).
    if std::env::var("CADLAB_CORPUS_ROUTE").is_ok_and(|v| v == "1") {
        row.route = Some(reroute(&p));
        eprintln!("route: {}", row.route.as_ref().unwrap());
    }

    row.status = if row.problems.is_empty() { "ok".into() } else { "differences".into() };
    row.seconds = started.elapsed().as_secs_f64();
    row
}

/// Rips up every track and via (zones stay) and routes the board again with a time budget
/// (`CADLAB_CORPUS_ROUTE_BUDGET` seconds, default 60).
fn reroute(p: &Project) -> Value {
    use cadlab::router::{self, Hooks, RouteOptions, Scope};
    let mut q = p.clone();
    let (tracks, vias) = (q.board().tracks.len(), q.board().vias.len());
    q.board_mut().tracks.clear();
    q.board_mut().vias.clear();
    let secs: u64 = std::env::var("CADLAB_CORPUS_ROUTE_BUDGET").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let opts = RouteOptions { budget: Some(std::time::Duration::from_secs(secs)), ..Default::default() };
    let t0 = Instant::now();
    match router::route(&q, &Scope::All, &opts, &Hooks::none()) {
        Ok(r) => json!({
            "ripped_tracks": tracks, "ripped_vias": vias,
            "connections": r.stats.connections, "routed": r.stats.routed, "completion": r.stats.completion,
            "tracks": r.stats.tracks, "vias": r.stats.vias, "budget_exhausted": r.stats.budget_exhausted,
            "seconds": t0.elapsed().as_secs_f64(),
        }),
        Err(e) => json!({"error": e.to_string()}),
    }
}

fn summary(rows: &[Row]) -> String {
    let n = |v: &Option<Value>, path: &[&str]| -> String {
        let mut x = match v {
            Some(x) => x,
            None => return "-".into(),
        };
        for k in path {
            x = &x[*k];
        }
        match x {
            Value::Null => "-".into(),
            Value::Number(n) => n.to_string(),
            Value::Object(m) => m.values().filter_map(Value::as_u64).sum::<u64>().to_string(),
            v => v.to_string(),
        }
    };
    let mut s = String::new();
    s.push_str(&format!(
        "{:<20} {:>2} {:>3} {:>5} {:>6} {:>6} {:>5} {:>13} {:>13} {:>8} {:>7} {:>6} {:<12}\n",
        "project",
        "L",
        "fps",
        "trk",
        "comps",
        "diags",
        "notim",
        "DRC k/c/match",
        "rexp k/c/m",
        "unexpl",
        "route%",
        "secs",
        "status"
    ));
    for r in rows {
        let unexpl =
            |v: &Option<Value>| v.as_ref().map(|v| n(&Some(v.clone()), &["unexplained"])).unwrap_or("-".into());
        let drc = |v: &Option<Value>| format!("{}/{}/{}", n(v, &["kicad"]), n(v, &["cadlab"]), n(v, &["matched"]));
        s.push_str(&format!(
            "{:<20} {:>2} {:>3} {:>5} {:>6} {:>6} {:>5} {:>13} {:>13} {:>8} {:>7} {:>6.0} {:<12}\n",
            r.name,
            r.layers,
            n(&r.import, &["footprints"]),
            n(&r.import, &["tracks"]),
            n(&r.netlist, &["components"]),
            r.import_diagnostics.values().sum::<usize>(),
            n(&r.import, &["not_imported"]),
            drc(&r.drc_original),
            drc(&r.drc_reexport),
            format!("{}+{}", unexpl(&r.drc_original), unexpl(&r.drc_reexport)),
            r.route.as_ref().and_then(|v| v["completion"].as_f64()).map(|c| format!("{c:.1}")).unwrap_or("-".into()),
            r.seconds,
            r.status
        ));
    }
    s
}

#[test]
fn manifest_is_valid() {
    let m = manifest();
    assert!(m.project.len() >= 8, "the corpus has at least 8 projects");
    let mut names = std::collections::BTreeSet::new();
    for e in &m.project {
        assert!(names.insert(e.name.as_str()), "duplicate project {}", e.name);
        assert!(
            e.name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
            "{}: names are directory names",
            e.name
        );
        assert!(
            e.sha.len() == 40 && e.sha.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{}",
            e.name
        );
        assert!(e.repo.starts_with("https://github.com/"), "{}: repo", e.name);
        assert!(!e.license.is_empty() && !e.notes.is_empty(), "{}: license and notes", e.name);
        assert!(e.pcb.ends_with(".kicad_pcb"), "{}", e.name);
        assert!(e.sch.as_ref().is_none_or(|s| s.ends_with(".kicad_sch")), "{}", e.name);
        assert!([2, 4, 6, 8, 10, 12].contains(&e.layers), "{}: layers", e.name);
        assert!((6..=10).contains(&e.kicad), "{}: KiCad 6 or later", e.name);
        assert!(!e.sparse.is_empty(), "{}: sparse patterns", e.name);
        for k in &e.known {
            assert!(k.side == "kicad" || k.side == "cadlab", "{}: known side {}", e.name, k.side);
            assert!(k.max > 0 && !k.rule.is_empty() && k.reason.len() > 20, "{}: known {}", e.name, k.rule);
            assert!(
                k.stage.as_deref().is_none_or(|s| s == "original" || s == "reexport"),
                "{}: known stage {:?}",
                e.name,
                k.stage
            );
        }
        for g in &e.gerber_known {
            let layers = ["F.Mask", "B.Mask", "F.Paste", "B.Paste", "Edge.Cuts", "drill"];
            assert!(
                layers.contains(&g.layer.as_str()) || g.layer.ends_with(".Cu"),
                "{}: gerber_known layer {}",
                e.name,
                g.layer
            );
            assert!(g.reason.len() > 20, "{}: gerber_known {} needs a reason", e.name, g.layer);
        }
    }
    // The fetch script reads the manifest with a line-based parser: one-line string arrays and
    // `key = "value"` lines only.
    for line in MANIFEST.lines().filter(|l| l.starts_with("sparse")) {
        assert!(line.trim_end().ends_with(']'), "sparse arrays stay on one line: {line}");
    }
}

#[test]
fn corpus() {
    let Some(root) = std::env::var_os("CADLAB_CORPUS_DIR").map(PathBuf::from) else {
        eprintln!("skipping: set CADLAB_CORPUS_DIR to the directory filled by scripts/fetch-corpus.sh");
        return;
    };
    let only: Option<Vec<String>> =
        std::env::var("CADLAB_CORPUS_ONLY").ok().map(|v| v.split(',').map(|s| s.trim().to_string()).collect());
    let tools = oracle::require(Oracle::KicadCli)
        .map(|cli| Tools { cli, gerbv: oracle::require(Oracle::Gerbv).expect("gerbv") });
    let m = manifest();
    let base: Vec<Allow<'static>> = drc_compare::ALLOW.iter().cloned().chain(corpus_allow()).collect();
    let mut used = vec![0usize; base.len()];
    let mut rows = Vec::new();
    for e in &m.project {
        if only.as_ref().is_some_and(|o| !o.contains(&e.name)) {
            continue;
        }
        let mut u = vec![0usize; base.len() + 64];
        let row = run_project(e, &root, tools.as_ref(), &base, &mut u);
        for (a, b) in used.iter_mut().zip(&u) {
            *a += b;
        }
        rows.push(row);
    }
    let table = summary(&rows);
    eprintln!("\n{table}");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("corpus");
    std::fs::create_dir_all(&dir).unwrap();
    let report = json!({
        "kicad_cli": tools.as_ref().map(|t| oracle::run(&t.cli, &["version"]).trim().to_string()),
        "projects": rows,
    });
    std::fs::write(dir.join("report.json"), serde_json::to_string_pretty(&report).unwrap()).unwrap();
    std::fs::write(dir.join("summary.txt"), &table).unwrap();
    let mut problems: Vec<String> =
        rows.iter().flat_map(|r| r.problems.iter().map(move |p| format!("{}: {p}", r.name))).collect();
    if tools.is_some() && only.is_none() {
        // Corpus-specific allowances must all be needed somewhere in the corpus.
        let corpus_only = &base[drc_compare::ALLOW.len()..];
        drc_compare::unused_entries(corpus_only, &used[drc_compare::ALLOW.len()..], &mut problems);
    }
    let shown: Vec<&String> = problems.iter().take(200).collect();
    assert!(
        problems.is_empty(),
        "{} corpus differences (first {} below; report in {}):\n{}",
        problems.len(),
        shown.len(),
        dir.display(),
        shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
    );
}
