//! The M4 exit (docs/ROADMAP.md): the target demo board, a two-layer board with an MCU, USB-C,
//! an LDO and LEDs designed entirely through commands, passes cadlab's DRC, and the same
//! unmodified project exports a JLCPCB bundle and a PCBWay bundle.
//!
//! The board is `examples/demo-board/board.jsonl`, agent session 1's design (docs/TESTING.md) as
//! a batch: the file a user runs with `cadlab batch` is the file these tests run.
//!
//! - [`demo_board_passes_drc_and_exports_for_both_fabs`]: through the command registry.
//! - [`documented_commands_write_the_same_bundles`]: the command lines of
//!   docs/MANUFACTURING.md ("Demo board") with the `cadlab` binary give the same archives.
//! - [`kicad_drc_clean_demo_board`]: KiCad's DRC on the board's `.kicad_pcb` export. An oracle
//!   test: it runs only with `CADLAB_ORACLES=1` (CI's `oracles` job, DECISIONS D7).
//!
//! Not covered here, and not automatable: the fabs' own online checks of the uploaded archives
//! (docs/MANUFACTURING.md, "Demo board", has the checklist).

mod common;
#[path = "common/drc_compare.rs"]
mod drc_compare;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use cadlab::command::{Registry, RunOptions, Session, Step};
use cadlab::kicad_pcb::Frame;
use cadlab::supplier::Suppliers;
use common::oracle::{self, Oracle};
use drc_compare::{ALLOW, Allow};
use serde_json::{Value, json};

/// Both fabs of the exit, with the names their profiles give the fabrication files, in the
/// same order: top and bottom copper, mask, paste and silkscreen, the outline, the plated drill.
const FABS: [(&str, [&str; 10]); 2] = [
    (
        "jlcpcb",
        [
            "demo.GTL",
            "demo.GBL",
            "demo.GTS",
            "demo.GBS",
            "demo.GTP",
            "demo.GBP",
            "demo.GTO",
            "demo.GBO",
            "demo.GKO",
            "demo-PTH.drl",
        ],
    ),
    (
        "pcbway",
        [
            "demo-F_Cu.gbr",
            "demo-B_Cu.gbr",
            "demo-F_Mask.gbr",
            "demo-B_Mask.gbr",
            "demo-F_Paste.gbr",
            "demo-B_Paste.gbr",
            "demo-F_SilkS.gbr",
            "demo-B_SilkS.gbr",
            "demo-Edge_Cuts.gbr",
            "demo-PTH.drl",
        ],
    ),
];

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/demo-board/board.jsonl")
}

/// The batch's steps, read as `cadlab batch` reads them (one JSON step per line, `#` comments).
fn steps() -> Vec<Step> {
    let text = std::fs::read_to_string(script()).unwrap();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn codes(o: &Value) -> Vec<&str> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap()).collect()
}

/// The demo project, built through the registry and saved: `project.new` with both fabs as
/// compatibility targets, then the batch. No supplier is configured.
fn build() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    s.suppliers = Suppliers::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("demo"), "targets": ["jlcpcb", "pcbway"]}));
    let done = r
        .execute_batch(&mut s, steps(), RunOptions::default())
        .unwrap_or_else(|f| panic!("step {:?}: {}", f.step, f.error));
    for o in &done {
        assert!(o.diagnostics.is_empty(), "{}: {:?}", o.command, o.diagnostics);
    }
    s.save().unwrap();
    (dir, r, s)
}

/// Every file under `dir` by relative path, without the directories in `skip`.
fn files(dir: &Path, skip: &[&str]) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, skip: &[&str], out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if skip.contains(&rel.as_str()) {
                continue;
            }
            if p.is_dir() {
                walk(root, &p, skip, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, skip, &mut out);
    out
}

/// The members of a zip archive, by name.
fn unzip(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    use fstool::fs::Filesystem;
    let mut dev = fstool::block::MemoryBackend::from_bytes(bytes.to_vec());
    let mut z = fstool::fs::archive::zip::ZipFs::open(&mut dev).unwrap();
    let mut out = BTreeMap::new();
    for e in z.list(&mut dev, Path::new("/")).unwrap() {
        assert_eq!(e.kind, fstool::fs::EntryKind::Regular, "{} is not a flat file", e.name);
        let mut buf = Vec::new();
        z.read_file(&mut dev, Path::new(&format!("/{}", e.name))).unwrap().read_to_end(&mut buf).unwrap();
        out.insert(e.name, buf);
    }
    out
}

/// M4 exit, the automated part: the demo board is what the roadmap names, it passes ERC, lint
/// and cadlab's DRC (which, with both fabs as targets, includes what either cannot make), and
/// `fab.export` writes a complete bundle for JLCPCB and for PCBWay from the same project, which
/// neither export changes and which names no fab outside its list of targets.
#[test]
fn demo_board_passes_drc_and_exports_for_both_fabs() {
    let (dir, r, mut s) = build();
    let root = dir.path().join("demo");

    // The board the roadmap names: two layers, MCU + USB-C + LDO + LEDs, placed and routed.
    let p = s.project.as_ref().unwrap();
    assert_eq!(p.board().stackup.copper_layers, 2);
    let part_of = |refdes: &str| p.circuit().components[refdes].part.clone();
    assert_eq!(
        (part_of("U2").as_str(), part_of("J1").as_str(), part_of("U1").as_str()),
        ("ATMEGA328P-AU", "USB-C-6P", "AP2112K-3.3TRG1")
    );
    assert!(part_of("D2").starts_with("LED_") && part_of("D3").starts_with("LED_"));
    let components = p.circuit().components.len();
    assert_eq!(components, 24);
    assert_eq!(p.board().footprints.len(), components, "every component is placed");

    let erc = exec(&r, &mut s, "circuit.erc", json!({}));
    assert_eq!(erc["output"], json!({"errors": 0, "warnings": 0}), "{:?}", erc["diagnostics"]);
    let lint = exec(&r, &mut s, "circuit.lint", json!({}));
    assert!(codes(&lint).is_empty(), "{:?}", lint["diagnostics"]);
    let status = exec(&r, &mut s, "route.status", json!({}));
    assert_eq!(status["output"]["unrouted"], 0, "{status}");

    // cadlab DRC: no error, no warning; the targets add each fab's own limits to the run.
    let drc = exec(&r, &mut s, "drc.run", json!({}));
    assert_eq!(drc["output"], json!({"errors": 0, "warnings": 0}), "{:?}", drc["diagnostics"]);
    assert!(codes(&drc).is_empty(), "{:?}", drc["diagnostics"]);

    // Both exports, from the project as saved; nothing but the missing supplier is reported
    // (`fab.export` refuses a board the fab check finds errors on).
    let before = files(&root, &["out", ".cadlab"]);
    let mut bundles = Vec::new();
    for (fab, names) in FABS {
        let check = exec(&r, &mut s, "fab.check", json!({"fab": fab}));
        assert_eq!(codes(&check), ["fab.no_suppliers"], "{fab}: {:?}", check["diagnostics"]);
        let o = exec(&r, &mut s, "fab.export", json!({"fab": fab}));
        assert_eq!(codes(&o), ["fab.no_suppliers"], "{fab}: {:?}", o["diagnostics"]);
        assert_eq!(o["output"]["dir"], format!("out/fab/{fab}"));

        let out = files(&root.join("out/fab").join(fab), &[]);
        let mut expected: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        for n in ["demo-BOM-{}.csv", "demo-CPL-{}.csv", "demo-{}.zip"] {
            expected.push(n.replace("{}", fab));
        }
        expected.push("fab-lock.json".into());
        expected.sort();
        assert_eq!(out.keys().cloned().collect::<Vec<_>>(), expected, "{fab}");

        // The archive to upload holds exactly the fabrication files.
        let zip = unzip(&out[&format!("demo-{fab}.zip")]);
        let mut fab_files: Vec<&str> = names.to_vec();
        fab_files.sort();
        assert_eq!(zip.keys().map(String::as_str).collect::<Vec<_>>(), fab_files, "{fab}");
        for (name, content) in &zip {
            assert!(content == &out[name], "{fab}: {name} differs in the archive");
            assert!(!content.is_empty(), "{fab}: {name} is empty");
        }

        // The lock says which fab and process the bundle is for, and lists every file.
        let lock: Value = serde_json::from_slice(&out["fab-lock.json"]).unwrap();
        assert_eq!((&lock["profile"]["id"], &lock["process"]["layers"]), (&json!(fab), &json!(2)));
        assert_eq!(lock["process"]["thickness"], "1.6mm");
        assert_eq!(lock["files"].as_array().unwrap().len(), expected.len() - 1, "all but the lock itself");

        // Assembly files: every component placed once, every BOM line with its designators.
        let cpl = String::from_utf8(out[&format!("demo-CPL-{fab}.csv")].clone()).unwrap();
        assert_eq!(cpl.lines().count(), 1 + components, "{fab}:\n{cpl}");
        let bom = String::from_utf8(out[&format!("demo-BOM-{fab}.csv")].clone()).unwrap();
        for refdes in p_refs(&s) {
            assert!(
                cpl.lines().any(|l| l.split(',').any(|f| f.trim_matches('"') == refdes)),
                "{fab}: {refdes} not in CPL"
            );
            assert!(bom.contains(&refdes), "{fab}: {refdes} not in BOM");
        }
        bundles.push(out);
    }

    // One design, two bundles: the fabrication data is the same, byte for byte, under each
    // fab's file names.
    for (a, b) in FABS[0].1.iter().zip(FABS[1].1) {
        assert!(bundles[0][*a] == bundles[1][b], "{a} (jlcpcb) differs from {b} (pcbway)");
    }

    // The same unmodified project: saving after both exports leaves every project file as it was.
    s.save().unwrap();
    assert!(before == files(&root, &["out", ".cadlab"]), "an export changed the project");
    // ... and it is tied to no fab: outside the manifest's list of targets (a hint for checks,
    // D12), no project file names one.
    for (name, content) in &before {
        let text = String::from_utf8_lossy(content).to_lowercase();
        let named = ["jlcpcb", "pcbway", "lcsc"].iter().any(|f| text.contains(f));
        assert_eq!(named, name == "cadlab.toml", "{name}");
    }
}

fn p_refs(s: &Session) -> Vec<String> {
    s.project.as_ref().unwrap().circuit().components.keys().cloned().collect()
}

/// Runs the binary isolated from the user's configuration, like a fresh machine.
fn cadlab(cwd: &Path, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_cadlab"))
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", cwd.join(".no-config"))
        .env("XDG_CACHE_HOME", cwd.join(".no-cache"))
        .env("XDG_DATA_HOME", cwd.join(".no-data"))
        .env("HOME", cwd.join(".no-home"))
        .env_remove("CADLAB_CATALOGS")
        .env_remove("DIGIKEY_CLIENT_ID")
        .env_remove("DIGIKEY_CLIENT_SECRET")
        .env_remove("MOUSER_API_KEY")
        .env_remove("NEXAR_CLIENT_ID")
        .env_remove("NEXAR_CLIENT_SECRET")
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "`cadlab {}` exited with {:?}\nstdout: {}\nstderr: {}",
        args.join(" "),
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// The command lines of docs/MANUFACTURING.md ("Demo board"), as a user types them: each
/// succeeds (`cadlab drc` exits 0 only without errors), both archives land where the checklist
/// says, and they are byte for byte the archives of the registry build: the bundles to upload
/// are reproducible, whoever builds them and however.
#[test]
fn documented_commands_write_the_same_bundles() {
    let tmp = tempfile::tempdir().unwrap();
    let script = script();
    cadlab(tmp.path(), &["project", "new", "demo", "--targets", "jlcpcb,pcbway"]);
    cadlab(tmp.path(), &["-p", "demo", "batch", script.to_str().unwrap()]);
    let drc: Value = serde_json::from_str(&cadlab(tmp.path(), &["-p", "demo", "--json", "drc"])).unwrap();
    assert_eq!(drc["output"], json!({"errors": 0, "warnings": 0}), "{drc}");
    cadlab(tmp.path(), &["-p", "demo", "fab", "export", "jlcpcb"]);
    cadlab(tmp.path(), &["-p", "demo", "fab", "export", "pcbway"]);

    let (dir, r, mut s) = build();
    for (fab, _) in FABS {
        exec(&r, &mut s, "fab.export", json!({"fab": fab}));
        let zip = format!("demo/out/fab/{fab}/demo-{fab}.zip");
        let cli = std::fs::read(tmp.path().join(&zip)).unwrap_or_else(|e| panic!("{zip}: {e}"));
        assert!(cli == std::fs::read(dir.path().join(&zip)).unwrap(), "{zip} differs between the two builds");
        let lock = format!("demo/out/fab/{fab}/fab-lock.json");
        assert!(
            std::fs::read(tmp.path().join(&lock)).unwrap() == std::fs::read(dir.path().join(&lock)).unwrap(),
            "{lock} differs between the two builds"
        );
    }
}

/// M4 exit, the KiCad DRC oracle: `kicad-cli pcb drc` (every severity, zones refilled) on the
/// demo board's `.kicad_pcb` export reports nothing, as cadlab's own DRC does, except what the
/// allowances of `tests/common/drc_compare.rs` explain as conventions of the two tools. The
/// allowances for dangling track ends and vias are left out: the demo board is routed by
/// cadlab and has no deliberate stub, so KiCad finding one is a defect to look at. The report
/// and the export are kept in `$CARGO_TARGET_TMPDIR/demo_board/`.
#[test]
fn kicad_drc_clean_demo_board() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let (_dir, _r, s) = build();
    let p = s.project.as_ref().unwrap();
    let ours = drc_compare::cadlab_findings(p);
    assert!(ours.is_empty(), "cadlab DRC: {ours:?}");

    let keep = Path::new(env!("CARGO_TARGET_TMPDIR")).join("demo_board");
    let _ = std::fs::remove_dir_all(&keep);
    std::fs::create_dir_all(&keep).unwrap();
    let export = cadlab::kicad_pcb::export(p, "demo");
    assert!(export.warnings.is_empty(), "export warnings: {:?}", export.warnings);
    let pcb = keep.join("demo.kicad_pcb");
    std::fs::write(&pcb, &export.pcb).unwrap();
    std::fs::write(keep.join("demo.kicad_pro"), &export.project).unwrap();
    std::fs::write(keep.join("demo.kicad_dru"), &export.rules).unwrap();
    let report = drc_compare::kicad_drc(&cli, &pcb, &keep.join("drc.json"), &[]);

    let allow: Vec<Allow> =
        ALLOW.iter().filter(|a| !["track_dangling", "via_dangling"].contains(&a.rule)).cloned().collect();
    let mut used = vec![0; allow.len()];
    let mut problems = Vec::new();
    let board = drc_compare::Board {
        name: "demo_board",
        what: "the M4 demo board (ATmega328P, USB-C, LDO, LEDs), autorouted",
        expect: &[],
        verbose: true,
    };
    drc_compare::compare(&board, p, &ours, &report, &export.uuids, &Frame::of(p), &allow, &mut used, &mut problems);
    assert!(problems.is_empty(), "KiCad DRC on the demo board:\n{}", problems.join("\n"));
}
