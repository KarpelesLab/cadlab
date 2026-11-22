//! `zone.relieve` on real boards (`board::thermal::relieve`, docs/BOARD.md "Thermal relief";
//! DECISIONS D119): on every board of the open-source corpus (`tests/corpus/projects.toml`,
//! fetched by `scripts/fetch-corpus.sh`, found through `CADLAB_CORPUS_DIR`; skipped without
//! it), as routed by its project, the repair that takes zone layers, reliefs and joints from
//! the fills and readings before where everything they were made from is as it was leaves
//! the board, the report and the joints of the repair that makes each from scratch
//! (`relieve_uncached`). It prints, per board, the time of both, the pads before and after
//! and, for the pads left short of spokes, what stands in the way by whose it is to move.
//! Run in release: `CADLAB_CORPUS_DIR=... cargo test --release --test zone_relieve -- --nocapture`;
//! `CADLAB_RELIEVE_ONLY=a,b` takes those boards alone, `CADLAB_RELIEVE_VERBOSE=1` lists every pad left
//! short with what was tried and what is in its way, every pad on a single neck and what the repair did.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use cadlab::board::{self, thermal, zones};
use cadlab::kicad_import::{self, BoardImportOptions, OriginMode, rules};
use cadlab::model::Project;

#[derive(serde::Deserialize)]
struct Manifest {
    project: Vec<Entry>,
}

#[derive(serde::Deserialize)]
struct Entry {
    name: String,
    pcb: String,
}

#[test]
fn corpus_relieve_is_the_relieve_from_scratch() {
    let Some(dir) = std::env::var_os("CADLAB_CORPUS_DIR").map(PathBuf::from) else {
        eprintln!("skipping: set CADLAB_CORPUS_DIR to the directory filled by scripts/fetch-corpus.sh");
        return;
    };
    let only: Vec<String> =
        std::env::var("CADLAB_RELIEVE_ONLY").map(|v| v.split(',').map(str::to_string).collect()).unwrap_or_default();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest: Manifest =
        toml::from_str(&std::fs::read_to_string(root.join("tests/corpus/projects.toml")).unwrap()).unwrap();
    let mut boards = 0;
    for e in manifest.project.iter().filter(|e| only.is_empty() || only.contains(&e.name)) {
        let pcb = dir.join(&e.name).join(&e.pcb);
        let Ok(text) = std::fs::read_to_string(&pcb) else {
            eprintln!("{}: not in {}", e.name, dir.display());
            continue;
        };
        let read = |ext: &str| std::fs::read_to_string(pcb.with_extension(ext)).ok();
        let (pro, dru) = (read("kicad_pro"), read("kicad_dru"));
        let mut p = Project::new(&e.name);
        let k = rules::parse(pro.as_deref(), dru.as_deref()).map(|r| r.0).ok();
        let opts =
            BoardImportOptions { file_name: e.pcb.clone(), rules: k, origin: OriginMode::Aux, ..Default::default() };
        if kicad_import::import(&mut p, &text, &opts).is_err() || p.board().zones.is_empty() {
            continue;
        }
        boards += 1;
        let max = p.board().rules.thermal_max_contact;
        // A fill of the board as it stands, from nothing, and its pads: what a command that
        // looks at the board has done before the repair begins.
        zones::clear_fill_cache();
        let t = Instant::now();
        let before = thermal::joints(&p, &board::copper_items(&p));
        let fill = t.elapsed();
        let census = thermal::census(&p, &before, max);
        let (mut kept, mut scratch) = (p.clone(), p.clone());
        let t = Instant::now();
        let (a, ja) = thermal::relieve_from(&mut kept, None, false, Some(&before));
        let t_kept = t.elapsed();
        let t = Instant::now();
        let (b, jb) = thermal::relieve_uncached(&mut scratch, None, false);
        let t_scratch = t.elapsed();
        assert_eq!(a, b, "{}: the report", e.name);
        assert_eq!(ja, jb, "{}: the joints", e.name);
        assert!(kept == scratch, "{}: the board", e.name);
        // The joints kept are those of the board read from nothing.
        zones::clear_fill_cache();
        assert_eq!(thermal::joints(&kept, &board::copper_items(&kept)), ja, "{}: read again", e.name);
        let after = thermal::census(&kept, &ja, max);
        // What is in the way of the pads left short, by whose it is to move.
        let mut classes: BTreeMap<String, usize> = BTreeMap::new();
        for v in a.blocked.values() {
            let mut of: Vec<&str> = v.iter().map(thermal::Blocker::class).collect();
            of.sort_unstable();
            of.dedup();
            *classes.entry(of.join("+")).or_default() += 1;
        }
        let unexplained = after.starved.saturating_sub(a.blocked.len());
        // Short of spokes counted by the separate pieces of copper alone, as before D119.
        let min = kept.board().rules.min_thermal_spokes;
        let pieces = |js: &[thermal::Joint]| {
            js.iter()
                .filter(|j| {
                    j.class.solder()
                        && j.in_pour()
                        && j.style == Some(cadlab::model::board::PadConnection::Thermal)
                        && j.solid(max).is_none()
                        && j.necks < min.min(j.possible.max(1))
                })
                .count()
        };
        eprintln!(
            "{}: fill and read {fill:.2?}; relieve {t_kept:.2?}, from scratch {t_scratch:.2?}\n  before: {}\n  after:  {}\n  \
             short by separate pieces {} -> {}; {} fitted, {} track(s), {} kept solid, {} pad(s) left short; in the way \
             by pad and layer: {classes:?}, {unexplained} with nothing named",
            e.name,
            census.text(),
            after.text(),
            pieces(&before),
            pieces(&ja),
            a.fitted.len(),
            a.tracks.len(),
            a.solid.len(),
            a.reasons.len(),
        );
        if std::env::var_os("CADLAB_RELIEVE_VERBOSE").is_some() {
            for (k, v) in &a.reasons {
                eprintln!("  left {k}: {v}");
            }
            for j in ja.iter().filter(|j| j.class.solder() && j.in_pour() && j.necks < 2) {
                eprintln!(
                    "  thin {}.{} on {}: {} neck(s), worth {}, contact {} of {}, spoke {}, possible {}, starved {}",
                    j.refdes,
                    j.number,
                    j.layer,
                    j.necks,
                    j.worth(),
                    j.contact,
                    j.perimeter,
                    j.spoke,
                    j.possible,
                    j.starved(min, max)
                );
            }
            for l in a.fitted.iter().chain(&a.tracks).chain(&a.solid).chain(&a.cut) {
                eprintln!("  did {l}");
            }
        }
    }
    assert!(boards > 0, "no board of the corpus with zones in {}", dir.display());
}
