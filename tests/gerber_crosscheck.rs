//! Cross-check: cadlab's Gerber and drill files against KiCad's for the same boards
//! (docs/TESTING.md, "Cross-checks").
//!
//! For each board of `crosscheck::cases()`, cadlab's Gerbers (`fabout`) and KiCad's
//! (`kicad-cli pcb export gerbers/drill` on the `.kicad_pcb` export, zones refilled by KiCad,
//! coordinates relative to the auxiliary origin, which the export puts at cadlab's origin) are
//! rendered by gerbv over the same window and compared pixel by pixel, layer by layer
//! (`tests/common/gerber_compare.rs`). Drill files are compared hit by hit. Runs only with
//! `CADLAB_ORACLES=1`; KiCad and gerbv are external processes (DECISIONS D7). Files and renders
//! are kept in `$CARGO_TARGET_TMPDIR/gerber_crosscheck/<case>/`.
#![cfg(feature = "png")]

mod common;
mod crosscheck;
#[path = "common/gerber_compare.rs"]
mod gerber_compare;

use std::path::{Path, PathBuf};

use cadlab::model::Project;
use common::oracle::{self, Oracle};
use gerber_compare::{LayerSpec, Mode, Window, find};

/// Exports the board for KiCad and writes KiCad's Gerbers and drill files into `dir/kicad`.
fn kicad_outputs(cli: &Path, p: &Project, dir: &Path) -> PathBuf {
    let pcb = crosscheck::export_kicad(p, &dir.join("kicad_pcb"));
    let out = dir.join("kicad");
    gerber_compare::kicad_outputs(
        cli,
        &pcb,
        &out,
        "F.Cu,B.Cu,F.Mask,B.Mask,F.Paste,B.Paste,F.SilkS,B.SilkS,Edge.Cuts",
        // KiCad refills the zones with its own algorithm before plotting.
        &["--check-zones"],
    );
    out
}

/// (layer, cadlab file suffix, KiCad file suffix, comparison).
const COMPARED: &[(&str, &str, &str, Mode)] = &[
    ("F.Cu", "-F_Cu.gbr", "-F_Cu.gtl", Mode::Exact),
    ("B.Cu", "-B_Cu.gbr", "-B_Cu.gbl", Mode::Exact),
    ("F.Mask", "-F_Mask.gbr", "-F_Mask.gts", Mode::Exact),
    ("B.Mask", "-B_Mask.gbr", "-B_Mask.gbs", Mode::Exact),
    ("F.Paste", "-F_Paste.gbr", "-F_Paste.gtp", Mode::Exact),
    ("B.Paste", "-B_Paste.gbr", "-B_Paste.gbp", Mode::Exact),
    ("F.SilkS", "-F_SilkS.gbr", "-F_Silkscreen.gto", Mode::Silk),
    ("B.SilkS", "-B_SilkS.gbr", "-B_Silkscreen.gbo", Mode::Silk),
    ("Edge.Cuts", "-Edge_Cuts.gbr", "-Edge_Cuts.gm1", Mode::Profile),
];

#[test]
fn kicad_and_cadlab_gerbers_agree() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let Some(gerbv) = oracle::require(Oracle::Gerbv) else { return };
    let mut problems = Vec::new();
    for case in crosscheck::cases() {
        let (_d, _r, s) = crosscheck::build(&case);
        let p = crosscheck::project(&s);
        let dir = crosscheck::keep_dir("gerber_crosscheck", case.name);
        let ours = gerber_compare::cadlab_outputs(p, &dir);
        let theirs = kicad_outputs(&cli, p, &dir);
        let win = Window::of(p);
        eprintln!("=== {}", case.name);
        let specs: Vec<LayerSpec> = COMPARED
            .iter()
            .map(|(layer, a, b, mode)| LayerSpec {
                layer: layer.to_string(),
                ours: find(&ours, a),
                theirs: find(&theirs, b),
                mode: *mode,
            })
            .collect();
        for r in gerber_compare::compare_layers(&gerbv, p, &win, &dir, &specs, true) {
            for msg in r.problems() {
                problems.push(format!("{}: {msg} (renders in {})", case.name, dir.display()));
            }
        }
        // Drill files: same hits, diameters, plating and functions.
        let (ha, hb) = (gerber_compare::all_hits(&ours), gerber_compare::all_hits(&theirs));
        eprintln!("  drill     {} / {} hits", ha.len(), hb.len());
        if !gerber_compare::same_hits(&ha, &hb) {
            problems.push(format!("{}: drill hits differ:\n  cadlab {ha:?}\n  KiCad  {hb:?}", case.name));
        }
    }
    assert!(problems.is_empty(), "Gerber cross-check differences:\n{}", problems.join("\n"));
}
