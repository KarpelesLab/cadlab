//! A fill that takes parts of earlier fills where nothing they depend on has changed
//! (`board::zones::fill_zones`: the reliefs of pads, docs/BOARD.md "Zone fill") is the fill
//! made from scratch (`fill_zones_uncached`), point for point, on real boards: every board of
//! the open-source corpus (`tests/corpus/projects.toml`, fetched by `scripts/fetch-corpus.sh`,
//! found through `CADLAB_CORPUS_DIR`; skipped without it), as routed by its project and then
//! in a sequence of boards that differ from it by a few tracks, by many tracks and vias, and
//! by all of them, which is what an autorouter and an interactive session ask of the fill.

use std::path::PathBuf;

use cadlab::board::{self, zones};
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

fn same(p: &Project, what: &str) {
    let base = board::base_copper_items(p);
    let (reused, fresh) = (zones::fill_zones(p, &base), zones::fill_zones_uncached(p, &base));
    assert_eq!(reused.len(), fresh.len(), "{what}");
    for (a, b) in reused.iter().zip(&fresh) {
        assert_eq!((a.zone, &a.layer, &a.error), (b.zone, &b.layer, &b.error), "{what}");
        assert!(a.fill == b.fill, "{what}: the fill of zone {} on {} differs", a.name, a.layer);
    }
}

#[test]
fn corpus_refills_are_the_fills_from_scratch() {
    let Some(dir) = std::env::var_os("CADLAB_CORPUS_DIR").map(PathBuf::from) else {
        eprintln!("skipping: set CADLAB_CORPUS_DIR to the directory filled by scripts/fetch-corpus.sh");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest: Manifest =
        toml::from_str(&std::fs::read_to_string(root.join("tests/corpus/projects.toml")).unwrap()).unwrap();
    let mut boards = 0;
    for e in &manifest.project {
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
        same(&p, &format!("{} as routed", e.name));
        // The last tracks out, a few at a time: a refill near them alone.
        for n in [1usize, 5, 40] {
            let mut q = p.clone();
            let len = q.board().tracks.len();
            q.board_mut().tracks.truncate(len.saturating_sub(n));
            same(&q, &format!("{} without its last {n} track(s)", e.name));
        }
        // Tracks and vias out all over the board, more each time, down to none.
        for (kt, kv) in [(7usize, 5usize), (3, 2), (1, 1)] {
            let mut q = p.clone();
            let mut i = 0;
            q.board_mut().tracks.retain(|_| {
                i += 1;
                i % kt != 0
            });
            let mut i = 0;
            q.board_mut().vias.retain(|_| {
                i += 1;
                i % kv != 0
            });
            same(&q, &format!("{} without every {kt}th track and every {kv}th via", e.name));
        }
        // And as routed once more, after all of those: the whole fill is no longer kept (the
        // last few are), its reliefs are.
        same(&p, &format!("{} as routed, after the others", e.name));
    }
    assert!(boards > 0, "no board of the corpus with zones in {}", dir.display());
}
