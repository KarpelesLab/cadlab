//! Schematic layout quality (M3): grouping by block instance, packing, multi-sheet output and
//! the absence of overlapping drawn elements, on several circuits built through commands.
//!
//! Set `CADLAB_SCHEMATIC_OUT=/some/dir` to also write the rendered sheets there for review.

mod common;

use cadlab::command::{Registry, RunOptions, Session};
use common::boards::{build_board, build_stm32_board};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn project(name: &str) -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p"), "name": name}));
    (dir, r, s)
}

fn attiny() -> (tempfile::TempDir, Registry, Session) {
    let (dir, r, mut s) = project("attiny-blinky");
    build_board(&r, &mut s);
    exec(&r, &mut s, "net.set", json!({"nets": ["VBUS"], "driven": true}));
    exec(&r, &mut s, "net.no_connect", json!({"pins": ["U2.PB4"]}));
    (dir, r, s)
}

fn stm32() -> (tempfile::TempDir, Registry, Session) {
    let (dir, r, mut s) = project("stm32-board");
    build_stm32_board(&r, &mut s);
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    assert_eq!(o["output"], json!({"errors": 0, "warnings": 0}), "{:?}", o["diagnostics"]);
    (dir, r, s)
}

fn ldo() -> (tempfile::TempDir, Registry, Session) {
    let (dir, r, mut s) = project("ldo");
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
        "pins": [{"number": "1", "name": "VIN", "kind": "power_in"}, {"number": "2", "name": "GND", "kind": "power_in"},
                 {"number": "3", "name": "EN", "kind": "input"}, {"number": "4", "name": "NC", "kind": "no_connect"},
                 {"number": "5", "name": "VOUT", "kind": "power_out"}]}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "AP2112K-3.3TRG1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 2}));
    exec(&r, &mut s, "circuit.add", json!({"part": "R 10k 1% 0402"}));
    exec(&r, &mut s, "net.connect", json!({"net": "VIN", "pins": ["U1.VIN", "U1.EN", "C1.1", "R1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}));
    (dir, r, s)
}

/// Renders every sheet into `CADLAB_SCHEMATIC_OUT` when set.
fn review(r: &Registry, s: &mut Session, name: &str) {
    let Ok(out) = std::env::var("CADLAB_SCHEMATIC_OUT") else { return };
    let path = std::path::Path::new(&out).join(format!("{name}.png"));
    exec(r, s, "render.schematic", json!({"path": path}));
}

/// The STM32 board with many more block instances: more than one A3 sheet.
fn big() -> (tempfile::TempDir, Registry, Session) {
    let (dir, r, mut s) = project("big-board");
    build_stm32_board(&r, &mut s);
    for i in 2..=12 {
        let nets = json!({"USB_DP": format!("USB{i}_DP"), "USB_DM": format!("USB{i}_DM"), "VBUS": format!("VBUS{i}")});
        exec(
            &r,
            &mut s,
            "block.instantiate",
            json!({"block": "usb_in", "instance": format!("usb{i}"), "connect": nets}),
        );
        let nets = json!({"VBUS": format!("VBUS{i}"), "3V3": format!("3V3_{i}")});
        exec(
            &r,
            &mut s,
            "block.instantiate",
            json!({"block": "ldo_3v3", "instance": format!("power{i}"), "connect": nets}),
        );
        let nets = json!({"LED_IN": format!("3V3_{i}")});
        exec(
            &r,
            &mut s,
            "block.instantiate",
            json!({"block": "led_ind", "instance": format!("led{i}"), "connect": nets}),
        );
    }
    (dir, r, s)
}

type Build = fn() -> (tempfile::TempDir, Registry, Session);

fn circuits() -> Vec<(&'static str, Build)> {
    vec![("attiny", attiny), ("stm32", stm32), ("ldo", ldo), ("big", big)]
}

/// Paper choice: the ATtiny and STM32 boards fit on one A4 sheet; the big board continues on
/// several A3 sheets with each block instance whole on one sheet, rendered as `name-N.png`.
#[test]
fn sheets_and_multi_sheet_rendering() {
    for (build, papers) in [(attiny as Build, vec!["A4"]), (stm32, vec!["A4"])] {
        let (_dir, _r, s) = build();
        let sheets = cadlab::schematic::layout_sheets(s.project.as_ref().unwrap(), &Default::default());
        assert_eq!(sheets.iter().map(|s| s.paper.as_str()).collect::<Vec<_>>(), papers);
    }
    let (dir, r, mut s) = big();
    let p = s.project.as_ref().unwrap();
    let sheets = cadlab::schematic::layout_sheets(p, &Default::default());
    assert!(sheets.len() >= 2, "{} sheet(s)", sheets.len());
    for (i, sh) in sheets.iter().enumerate() {
        assert_eq!((sh.paper.as_str(), sh.sheet as usize, sh.sheets as usize), ("A3", i + 1, sheets.len()));
    }
    // A block instance is never split across sheets.
    let c = p.circuit();
    for inst in c.instances.keys() {
        let on: Vec<u32> = sheets
            .iter()
            .filter(|sh| {
                c.components
                    .iter()
                    .any(|(r, comp)| comp.block.as_deref() == Some(inst) && sh.placements.contains_key(r))
            })
            .map(|sh| sh.sheet)
            .collect();
        assert_eq!(on.len(), 1, "{inst} on sheets {on:?}");
        assert_eq!(
            sheets.iter().flat_map(|sh| &sh.frames).filter(|f| f.title.starts_with(&format!("{inst} "))).count(),
            1
        );
    }
    // The single-sheet layout (KiCad export) takes a larger paper instead.
    assert_ne!(cadlab::schematic::layout(p, &Default::default()).paper, "A3");

    let o = exec(&r, &mut s, "render.schematic", json!({"path": "out/big.png", "px_per_mm": 2}));
    let pages = o["output"]["pages"].as_array().unwrap();
    assert_eq!(pages.len(), sheets.len());
    for k in 1..=sheets.len() {
        assert!(dir.path().join(format!("p/out/big-{k}.png")).is_file());
    }
    let o = exec(&r, &mut s, "render.schematic", json!({"path": "one.svg", "sheet": 2}));
    assert!(o["output"]["pages"].is_null());
    assert!(std::fs::read_to_string(dir.path().join("p/one.svg")).unwrap().contains("viewBox=\"0 0 420 297\""));
    let f = r.execute(&mut s, "render.schematic", json!({"sheet": 99}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "render.no_sheet");
}

#[test]
fn review_renders() {
    for (name, build) in circuits() {
        let (_dir, r, mut s) = build();
        review(&r, &mut s, name);
    }
}

/// No two drawn elements overlap: symbol bodies, designators/values, labels and power symbols
/// (boxes from the stroke font's text extents), wires and block frames; on every sheet and on
/// the single-sheet layout used for the KiCad export.
#[test]
fn no_overlapping_elements() {
    for (name, build) in circuits() {
        let (_dir, _r, s) = build();
        let p = s.project.as_ref().unwrap();
        let mut sheets = cadlab::schematic::layout_sheets(p, &Default::default());
        sheets.push(cadlab::schematic::layout(p, &Default::default()));
        for sheet in &sheets {
            let o = cadlab::schematic::overlaps(p, sheet);
            assert!(o.is_empty(), "{name} sheet {} ({}):\n{}", sheet.sheet, sheet.paper, o.join("\n"));
            let (w, h) = (sheet.size.0.0, sheet.size.1.0);
            for (r, pl) in &sheet.placements {
                assert!(pl.at.x.0 > 0 && pl.at.x.0 < w && pl.at.y.0 > 0 && pl.at.y.0 < h, "{name}: {r} off the sheet");
            }
        }
        // Every component on exactly one sheet.
        let placed: usize = sheets[..sheets.len() - 1].iter().map(|s| s.placements.len()).sum();
        assert_eq!(placed, p.circuit().components.len(), "{name}");
        assert_eq!(sheets.last().unwrap().placements.len(), p.circuit().components.len(), "{name}");
    }
}
