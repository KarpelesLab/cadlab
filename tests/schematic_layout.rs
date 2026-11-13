//! Schematic layout quality (M3): grouping by block instance, packing, multi-sheet output and
//! the absence of overlapping drawn elements, on several circuits built through commands.
//!
//! Set `CADLAB_SCHEMATIC_OUT=/some/dir` to also write the rendered sheets there for review.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use cadlab::Point;
use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::Project;
use cadlab::model::circuit::PinRef;
use cadlab::render::{Prim, Scene, View};
use cadlab::schematic::SheetLayout;
use cadlab::schematic::symbol::{pin_ends, symbol_of};
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

/// Bounding box (min x, min y, max x, max y) of a drawing primitive, stroke width included.
fn prim_box(p: &Prim) -> (f64, f64, f64, f64) {
    let mut one = Scene::default();
    one.prims.push(p.clone());
    one.bounds().unwrap()
}

/// A group of pins drawn as one net, with the net names drawn on it.
type DrawnNet = (BTreeSet<String>, BTreeSet<PinRef>);

/// The nets a reader recovers from a drawn sheet: pins joined by wires (a pin or wire end on a
/// wire connects to it) and by labels and power symbols carrying the same name.
fn drawn_nets(p: &Project, l: &SheetLayout) -> Vec<DrawnNet> {
    let on_wire = |pt: Point, (a, b): (Point, Point)| {
        let (x, y) = (pt.x.0, pt.y.0);
        let within = |v: i64, s: i64, e: i64| v >= s.min(e) && v <= s.max(e);
        (a.x == b.x && x == a.x.0 && within(y, a.y.0, b.y.0)) || (a.y == b.y && y == a.y.0 && within(x, a.x.0, b.x.0))
    };
    // Nodes: wires first, then pins, then labels.
    let mut pins: Vec<(PinRef, Point)> = Vec::new();
    for (r, pl) in &l.placements {
        let part = &p.library().parts[&p.circuit().components[r].part];
        for (number, end, _) in pin_ends(&symbol_of(part), pl) {
            pins.push((PinRef::new(r.clone(), number), end));
        }
    }
    let (nw, np) = (l.wires.len(), pins.len());
    let mut parent: Vec<usize> = (0..nw + np + l.labels.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut join = |a: usize, b: usize| {
        let (a, b) = (find(&mut parent, a), find(&mut parent, b));
        parent[a.max(b)] = a.min(b);
    };
    for (i, w) in l.wires.iter().enumerate() {
        assert!(w.0.x == w.1.x || w.0.y == w.1.y, "wire {i} is neither horizontal nor vertical");
        for (j, v) in l.wires.iter().enumerate().skip(i + 1) {
            if [v.0, v.1].into_iter().any(|e| on_wire(e, *w)) || [w.0, w.1].into_iter().any(|e| on_wire(e, *v)) {
                join(i, j);
            }
        }
        for (k, (_, at)) in pins.iter().enumerate() {
            if on_wire(*at, *w) {
                join(i, nw + k);
            }
        }
        for (k, lb) in l.labels.iter().enumerate() {
            if on_wire(lb.at, *w) {
                join(i, nw + np + k);
            }
        }
    }
    for (k, (_, at)) in pins.iter().enumerate() {
        for (m, (_, other)) in pins.iter().enumerate().skip(k + 1) {
            if at == other {
                join(nw + k, nw + m);
            }
        }
        for (m, lb) in l.labels.iter().enumerate() {
            if lb.at == *at {
                join(nw + k, nw + np + m);
            }
        }
    }
    for (k, lb) in l.labels.iter().enumerate() {
        for (m, other) in l.labels.iter().enumerate().skip(k + 1) {
            if lb.net == other.net {
                join(nw + np + k, nw + np + m);
            }
        }
    }
    let mut groups: BTreeMap<usize, DrawnNet> = BTreeMap::new();
    for (k, (pin, _)) in pins.iter().enumerate() {
        groups.entry(find(&mut parent, nw + k)).or_default().1.insert(pin.clone());
    }
    for (k, lb) in l.labels.iter().enumerate() {
        let root = find(&mut parent, nw + np + k);
        let g = groups.get_mut(&root).unwrap_or_else(|| panic!("label {} at {:?} names no pin", lb.net, lb.at));
        g.0.insert(lb.net.clone());
    }
    groups.into_values().collect()
}

/// M3 exit: `render.schematic` writes a readable schematic of the M2 board (the ERC-clean ATtiny85
/// board of `tests/circuit.rs`) as PNG and as SVG. Readable, measured:
///
/// - every component is drawn, once, with its designator and value;
/// - the drawing says what the circuit says: the nets read back from the sheet (wires, and
///   labels or power symbols sharing a name) are the circuit's nets, pin for pin, and every
///   label names the net of the pins it sits on (so a net that is not wired from end to end
///   carries its name at each piece);
/// - no two drawn elements overlap, and nothing but the title block is in the title block;
/// - everything drawn, text included (the stroke font's lines), is inside the sheet frame;
/// - the PNG is the same sheet (size, ink at every symbol and label) and both outputs are
///   deterministic: byte-identical from a second, separately built project, and the SVG equal
///   to the golden file (without the title block, whose text carries the crate version).
#[test]
fn attiny_schematic_png_and_svg_are_readable() {
    let (dir, r, mut s) = attiny();
    let erc = exec(&r, &mut s, "circuit.erc", json!({}));
    assert_eq!(erc["output"], json!({"errors": 0, "warnings": 0}), "the M2 board, ERC clean");

    let png = exec(&r, &mut s, "render.schematic", json!({"path": "sch.png"}));
    let svg = exec(&r, &mut s, "render.schematic", json!({"path": "sch.svg"}));
    assert_eq!((&png["output"]["format"], &svg["output"]["format"]), (&json!("png"), &json!("svg")));
    assert!(png["output"]["pages"].is_null() && svg["output"]["pages"].is_null(), "one sheet");
    let png_bytes = std::fs::read(dir.path().join("p/sch.png")).unwrap();
    let svg_text = std::fs::read_to_string(dir.path().join("p/sch.svg")).unwrap();

    let p = s.project.as_ref().unwrap();
    let c = p.circuit();
    let sheets = cadlab::schematic::layout_sheets(p, &Default::default());
    assert_eq!(sheets.len(), 1);
    let sheet = &sheets[0];
    let (w, h) = (sheet.size.0.0 as f64 / 1e6, sheet.size.1.0 as f64 / 1e6);
    assert_eq!((sheet.paper.as_str(), w, h), ("A4", 297.0, 210.0));

    // Every component, once, with a designator and a value to print.
    assert_eq!(sheet.placements.keys().collect::<Vec<_>>(), c.components.keys().collect::<Vec<_>>());
    for (refdes, comp) in &c.components {
        assert!(!refdes.is_empty() && !p.library().parts[&comp.part].value().is_empty(), "{refdes}");
    }

    // The drawing reads back as the circuit.
    let drawn = drawn_nets(p, sheet);
    let mut whole: BTreeSet<&str> = BTreeSet::new();
    for (names, pins) in &drawn {
        let nets: Vec<&str> =
            c.nets.iter().filter(|(_, n)| !n.pins.is_disjoint(pins)).map(|(k, _)| k.as_str()).collect();
        match nets[..] {
            [] => assert!(names.is_empty() && pins.len() == 1, "unconnected pins drawn joined or named: {pins:?}"),
            [net] => {
                assert!(names.iter().all(|n| n == net), "pins of {net} carry the label(s) {names:?}");
                assert_eq!(*pins, c.nets[net].pins, "{net} as drawn (labels {names:?}) against the circuit");
                whole.insert(net);
            }
            _ => panic!("pins of {nets:?} are drawn joined: {pins:?}"),
        }
    }
    assert_eq!(whole.into_iter().collect::<Vec<_>>(), c.nets.keys().collect::<Vec<_>>(), "every net is drawn");
    for pin in &c.no_connect {
        let (names, pins) = drawn.iter().find(|(_, pins)| pins.contains(pin)).expect("no-connect pin drawn");
        assert!(names.is_empty() && pins.len() == 1, "{pin:?} is marked no-connect but drawn connected");
    }
    assert_eq!(sheet.no_connects.len(), c.no_connect.len(), "one marker per no-connect pin");

    // No overlapping elements.
    let o = cadlab::schematic::overlaps(p, sheet);
    assert!(o.is_empty(), "{}", o.join("\n"));

    // Everything inside the frame; the title block holds only its own text. The frame and title
    // block are the primitives an empty sheet has, drawn first.
    let scene = cadlab::schematic::draw(p, sheet);
    let empty = SheetLayout { paper: sheet.paper.clone(), size: sheet.size, sheet: 1, sheets: 1, ..Default::default() };
    let title = cadlab::schematic::draw(p, &empty);
    assert!(scene.prims.len() > title.prims.len() && scene.prims[..title.prims.len()] == title.prims[..]);
    let pad = 0.11; // half the frame's line
    let block = (w - 5.0 - 90.0, 5.0, w - 5.0, 5.0 + 14.0);
    for (i, prim) in scene.prims.iter().enumerate() {
        let (x0, y0, x1, y1) = prim_box(prim);
        assert!(x0 >= 5.0 - pad && y0 >= 5.0 - pad && x1 <= w - 5.0 + pad && y1 <= h - 5.0 + pad, "outside: {prim:?}");
        if i >= title.prims.len() {
            let apart = x1 < block.0 || x0 > block.2 || y1 < block.1 || y0 > block.3;
            assert!(apart, "drawn over the title block: {prim:?}");
        }
    }

    // The PNG is that sheet at 10 px/mm, with ink at every symbol and label.
    let pm = tiny_skia::Pixmap::decode_png(&png_bytes).unwrap();
    assert_eq!((pm.width(), pm.height()), (2970, 2100));
    assert_eq!((png["output"]["width"].as_f64(), png["output"]["height"].as_f64()), (Some(2970.0), Some(2100.0)));
    let ink = |at: Point, reach: f64| {
        let (cx, cy) = (at.x.0 as f64 / 1e5, (h * 1e6 - at.y.0 as f64) / 1e5);
        let px = |v: f64, max: u32| (v.max(0.0) as u32).min(max - 1);
        let (x0, x1) = (px(cx - reach * 10.0, pm.width()), px(cx + reach * 10.0, pm.width()));
        let (y0, y1) = (px(cy - reach * 10.0, pm.height()), px(cy + reach * 10.0, pm.height()));
        (y0..=y1).flat_map(|y| (x0..=x1).map(move |x| (x, y))).any(|(x, y)| {
            let c = pm.pixel(x, y).unwrap();
            (c.red(), c.green(), c.blue()) != (255, 255, 255)
        })
    };
    for (refdes, pl) in &sheet.placements {
        assert!(ink(pl.at, 2.0), "{refdes}: nothing drawn at its place in the PNG");
    }
    for lb in &sheet.labels {
        assert!(ink(lb.at, 1.0), "label {}: nothing drawn at its place in the PNG", lb.net);
    }

    // Deterministic: a second project built from scratch gives the same bytes.
    let (dir2, r2, mut s2) = attiny();
    exec(&r2, &mut s2, "render.schematic", json!({"path": "sch.png"}));
    exec(&r2, &mut s2, "render.schematic", json!({"path": "sch.svg"}));
    assert!(png_bytes == std::fs::read(dir2.path().join("p/sch.png")).unwrap(), "PNG differs between runs");
    assert_eq!(svg_text, std::fs::read_to_string(dir2.path().join("p/sch.svg")).unwrap());

    // Golden SVG. The command's file is the scene on the sheet; the golden file leaves out the
    // title block's lines (the crate version is drawn there).
    let view = View { area: (0.0, 0.0, w, h), px_per_mm: 10.0 };
    assert!(svg_text == cadlab::render::to_svg(&scene, &view), "render.schematic writes the sheet's scene");
    let mut body = scene.clone();
    body.prims.drain(..title.prims.len());
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/attiny85/schematic.svg");
    common::golden::assert_golden(&golden, &cadlab::render::to_svg(&body, &view));
}
