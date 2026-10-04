//! Import of the user's KiCad libraries (DECISIONS D41): `footprint.import_kicad`,
//! `part.import_kicad_sym` and `lib.import_kicad`.
//!
//! - **Fixtures:** small hand-written `.kicad_mod` / `.kicad_sym` files
//!   (`tests/fixtures/kicad/`, cadlab's own, see its README) cover pad shapes, drills, layers,
//!   inheritance, multi-unit symbols, alternate pin functions and power symbols.
//! - **Round trips:** footprints cadlab writes into a `.kicad_pcb` and symbols it writes into a
//!   `.kicad_sch` are cut out into a `.pretty` and a `.kicad_sym`, imported, and compared with
//!   the originals.
//! - **Oracle** (`CADLAB_ORACLES=1`): `kicad-cli fp upgrade` / `sym upgrade` must read the
//!   fixtures and the round-trip libraries, and importing KiCad's rewrite must give the same
//!   footprints and parts; `kicad-cli sym export svg` must draw every symbol.

mod common;

use std::path::{Path, PathBuf};

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::kicad_import::library::same_land_pattern;
use cadlab::model::Project;
use cadlab::model::part::{Category, PinKind, Side};
use cadlab::sexpr::{self, Sexpr};
use common::boards::build_board;
use common::oracle::{self, Oracle};
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kicad")
}

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> (Value, Vec<String>) {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => (o.output, o.diagnostics.iter().map(|d| d.code.to_string()).collect()),
        Err(f) => panic!("{cmd} failed: {} ({:?})", f.error, f.error.diagnostic.hint),
    }
}

fn exec_err(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> cadlab::command::CommandError {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => panic!("{cmd} should fail, got {}", o.summary),
        Err(f) => f.error,
    }
}

fn new_project() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p"), "name": "libimport"}));
    (dir, r, s)
}

fn project(s: &Session) -> &Project {
    s.project.as_ref().unwrap()
}

fn changes(v: &Value, key: &str) -> Vec<(String, String)> {
    v[key]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| (i["name"].as_str().unwrap().to_string(), i["change"].as_str().unwrap().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn footprints_from_a_pretty_directory() {
    let (_d, r, mut s) = new_project();
    let pretty = fixtures().join("cadlab_test.pretty");
    let (o, codes) =
        exec(&r, &mut s, "footprint.import_kicad", json!({"path": pretty, "license": "MIT (cadlab test fixture)"}));
    assert_eq!(
        changes(&o, "footprints"),
        ["CUSTOM_TEST", "HDR_1x03_TEST", "R0603_TEST", "SOT23_TEST"].map(|n| (n.to_string(), "added".to_string()))
    );
    for c in [
        "import.footprint_layer",
        "import.pad_approximated",
        "import.footprint_model",
        "import.footprint_text",
        "import.footprint_arc",
    ] {
        assert!(codes.iter().any(|x| x == c), "missing {c} in {codes:?}");
    }
    // B.SilkS and Edge.Cuts drawings of CUSTOM_TEST (its F.Cu line is a copper drawing, D40).
    assert_eq!(o["not_imported"], 2);

    let lib = project(&s).library();
    use cadlab::model::footprint::{GraphicLayer, Mount, PadKind, PadShape, Paste};
    use cadlab::units::Nm;
    let mm = |v: &str| Nm::parse(&format!("{v}mm")).unwrap();
    let pt = |x: &str, y: &str| cadlab::geom::Point::new(mm(x), mm(y));

    let sot = &lib.footprints["SOT23_TEST"];
    assert_eq!(sot.mount, Mount::Smd);
    assert_eq!(sot.description, "Hand-written SOT-23 test footprint for cadlab");
    assert_eq!(sot.pads.len(), 3);
    // KiCad Y down → cadlab Y up; roundrect radius = ratio × shorter side.
    assert_eq!(sot.pads[0].at, pt("-1.1", "0.95"));
    assert_eq!(sot.pads[0].shape, PadShape::RoundRect { w: mm("1.06"), h: mm("0.65"), r: mm("0.1625") });
    assert_eq!(sot.pads[0].paste, None);
    assert_eq!(sot.courtyard, vec![pt("-1.9", "1.7"), pt("1.9", "1.7"), pt("1.9", "-1.7"), pt("-1.9", "-1.7")]);
    assert_eq!(sot.graphics.iter().filter(|g| g.layer == GraphicLayer::Silk).count(), 2);
    assert_eq!(sot.graphics.iter().filter(|g| g.layer == GraphicLayer::Fab).count(), 1);
    let prov = sot.provenance.as_ref().unwrap();
    assert_eq!(prov.detail.as_deref(), Some("KiCad footprint `SOT23_TEST` from cadlab_test.pretty"));
    assert_eq!(prov.license.as_deref(), Some("MIT (cadlab test fixture)"));

    // KiCad 8 format: courtyard from four lines, through-hole pads.
    let r0603 = &lib.footprints["R0603_TEST"];
    assert_eq!(r0603.courtyard.len(), 4);
    assert_eq!(r0603.pads[1].shape, PadShape::Rect { w: mm("0.8"), h: mm("0.95") });
    let hdr = &lib.footprints["HDR_1x03_TEST"];
    assert_eq!(hdr.mount, Mount::Tht);
    assert_eq!(hdr.pads[2].at, pt("0", "-5.08"));
    assert_eq!(hdr.pads[2].kind, PadKind::Tht { drill: mm("1") });
    assert_eq!(hdr.pads[2].paste, Some(Paste::None));

    // Every pad kind.
    let custom = &lib.footprints["CUSTOM_TEST"];
    assert_eq!(custom.mount, Mount::Tht);
    let pad = |n: &str| custom.pads.iter().find(|p| p.number == n).unwrap();
    assert_eq!(pad("1").kind, PadKind::Tht { drill: mm("0.8") });
    assert_eq!(pad("2").kind, PadKind::Tht { drill: mm("0.6") }, "the drill is the slot width");
    assert_eq!(pad("2").slot, Some((mm("0.6"), mm("1.6"))), "the oval hole is a slot");
    assert_eq!(pad("2").rotation, cadlab::units::Angle::parse("90").unwrap());
    assert_eq!(pad("").kind, PadKind::Npth { drill: mm("1.2") });
    assert_eq!(
        pad("3").shape,
        PadShape::Polygon { points: vec![pt("-1", "0.4"), pt("1", "0.4"), pt("0", "-0.6")] },
        "custom pad: anchor inside the triangle"
    );
    assert_eq!(pad("4").shape, PadShape::Rect { w: mm("1.2"), h: mm("0.6") }, "trapezoid as its bounding box");
    assert_eq!(pad("5").paste, Some(Paste::None), "no F.Paste layer");
    assert_eq!(pad("5").overrides.mask_margin, Some(mm("0.05")), "local mask margin kept");
    assert_eq!(custom.graphics.iter().filter(|g| g.layer == GraphicLayer::Copper).count(), 1, "copper drawing kept");
    assert!(custom.courtyard.len() > 16, "circular courtyard as a polygon");

    // The same files again: nothing changes.
    let (o, _) =
        exec(&r, &mut s, "footprint.import_kicad", json!({"path": pretty, "license": "MIT (cadlab test fixture)"}));
    assert!(changes(&o, "footprints").iter().all(|(_, c)| c == "unchanged"), "{o}");
    // Another license is another provenance: a conflict unless replaced.
    let e = exec_err(&r, &mut s, "footprint.import_kicad", json!({"path": pretty}));
    assert_eq!(e.diagnostic.code, "import.conflict");
}

#[test]
fn footprint_conflicts_selection_and_dry_run() {
    let (_d, r, mut s) = new_project();
    let pretty = fixtures().join("cadlab_test.pretty");
    // One footprint of the directory, case-insensitively.
    let (o, _) = exec(&r, &mut s, "footprint.import_kicad", json!({"path": pretty, "footprint": ["r0603_test"]}));
    assert_eq!(changes(&o, "footprints"), [("R0603_TEST".to_string(), "added".to_string())]);
    let e = exec_err(&r, &mut s, "footprint.import_kicad", json!({"path": pretty, "footprint": ["R0603_TST"]}));
    assert_eq!(e.diagnostic.code, "import.footprint_not_found");
    assert!(e.diagnostic.hint.as_deref().unwrap_or("").contains("R0603_TEST"), "{:?}", e.diagnostic);

    // A single file; a different footprint under the same name is a conflict.
    let file = fixtures().join("cadlab_test.pretty/SOT23_TEST.kicad_mod");
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("R0603_TEST.kicad_mod");
    std::fs::copy(&file, &other).unwrap();
    let e = exec_err(&r, &mut s, "footprint.import_kicad", json!({"path": other}));
    assert_eq!(e.diagnostic.code, "import.conflict");
    // Dry run: reported as replaced, nothing changes.
    let o = r
        .execute(
            &mut s,
            "footprint.import_kicad",
            json!({"path": other, "replace": true}),
            RunOptions { dry_run: true },
        )
        .unwrap();
    assert_eq!(changes(&o.output, "footprints"), [("R0603_TEST".to_string(), "replaced".to_string())]);
    assert_eq!(project(&s).library().footprints["R0603_TEST"].pads.len(), 2);
    let (o, _) = exec(&r, &mut s, "footprint.import_kicad", json!({"path": other, "replace": true}));
    assert_eq!(changes(&o, "footprints"), [("R0603_TEST".to_string(), "replaced".to_string())]);
    assert_eq!(project(&s).library().footprints["R0603_TEST"].pads.len(), 3);
    // Undo restores it.
    exec(&r, &mut s, "history.undo", json!({}));
    assert_eq!(project(&s).library().footprints["R0603_TEST"].pads.len(), 2);

    // Errors: missing path, not a footprint, KiCad 5 format.
    let e = exec_err(&r, &mut s, "footprint.import_kicad", json!({"path": dir.path().join("nope.kicad_mod")}));
    assert_eq!(e.diagnostic.code, "import.file_not_found");
    let bad = dir.path().join("BAD.kicad_mod");
    std::fs::write(&bad, "(kicad_pcb (version 20240108))").unwrap();
    let e = exec_err(&r, &mut s, "footprint.import_kicad", json!({"path": bad}));
    assert_eq!(e.diagnostic.code, "import.not_kicad_library");
    std::fs::write(&bad, "(module OLD (layer F.Cu) (tedit 5A02FF57))").unwrap();
    let e = exec_err(&r, &mut s, "footprint.import_kicad", json!({"path": bad}));
    assert_eq!(e.diagnostic.code, "import.kicad_version");
    assert!(e.diagnostic.hint.as_deref().unwrap().contains("kicad-cli fp upgrade"));
}

#[test]
fn symbols_become_parts() {
    let (_d, r, mut s) = new_project();
    let (o, codes) = exec(
        &r,
        &mut s,
        "part.import_kicad_sym",
        json!({"path": fixtures().join("cadlab_test.kicad_sym"), "footprints": fixtures().join("cadlab_test.pretty")}),
    );
    assert_eq!(
        changes(&o, "parts").into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
        ["TestLDO", "TestLDO_ADJ", "DualOpAmp", "MCU_Alt", "R_Test", "Conn_1x03_Test"]
    );
    assert_eq!(o["skipped"], json!([{"kicad_name": "+3V3_TEST", "reason": "import.symbol_power"}]));
    assert_eq!(o["footprints"].as_array().unwrap().len(), 4);
    for c in [
        "import.symbol_power",
        "import.symbol_footprint_missing",
        "import.symbol_hidden_power_pin",
        "import.symbol_supplier_field",
    ] {
        assert!(codes.iter().any(|x| x == c), "missing {c} in {codes:?}");
    }
    let lib = project(&s).library();

    let ldo = &lib.parts["TestLDO"];
    assert_eq!(ldo.category, Category::Ldo);
    assert_eq!(ldo.mpn.as_deref(), Some("TLDO-33"));
    assert_eq!(ldo.manufacturer.as_deref(), Some("Example Semi"));
    assert_eq!(ldo.description, "Test 3.3 V low-dropout regulator");
    assert_eq!(ldo.datasheet.as_deref(), Some("https://example.com/tldo.pdf"));
    assert_eq!(ldo.params.get("voltage_out").unwrap().to_string(), "3.3V");
    assert!(ldo.params.get("lcsc").is_none(), "supplier fields are not stored");
    assert_eq!(ldo.footprint().unwrap().footprint, "SOT23_TEST");
    let pins: Vec<(&str, &str, PinKind, Option<Side>)> =
        ldo.symbol.pins.iter().map(|p| (p.number.as_str(), p.name.as_str(), p.kind, p.side)).collect();
    assert_eq!(
        pins,
        [
            ("1", "VIN", PinKind::PowerIn, Some(Side::Left)),
            ("2", "GND", PinKind::PowerIn, Some(Side::Bottom)),
            ("3", "VOUT", PinKind::PowerOut, Some(Side::Right)),
        ]
    );
    assert!(ldo.symbol.pins.iter().all(|p| p.at.is_some()), "symbol generated");
    let prov = &ldo.provenance;
    assert_eq!(prov.origin, cadlab::model::part::Origin::Import);

    // Derived symbol: the parent's pins, its own fields (KiCad does not inherit user fields).
    let adj = &lib.parts["TestLDO_ADJ"];
    assert_eq!(adj.symbol, ldo.symbol);
    assert_eq!(adj.mpn.as_deref(), Some("TLDO-ADJ"));
    assert_eq!(adj.manufacturer, None);
    assert_eq!(adj.datasheet, None, "`~` is no datasheet");
    assert_eq!(adj.description, "Test adjustable low-dropout regulator");

    // Multi-unit: units 1 and 2 and the supply unit 3; the De Morgan body style adds nothing.
    let amp = &lib.parts["DualOpAmp"];
    let units: Vec<(&str, Option<u32>)> = amp.symbol.pins.iter().map(|p| (p.number.as_str(), p.unit)).collect();
    assert_eq!(
        units,
        [
            ("1", Some(1)),
            ("2", Some(1)),
            ("3", Some(1)),
            ("4", Some(3)),
            ("5", Some(2)),
            ("6", Some(2)),
            ("7", Some(2)),
            ("8", Some(3))
        ]
    );
    assert_eq!(amp.symbol.pins[0].name, "", "`~` is no name");
    assert!(amp.footprints.is_empty());

    // Alternate functions, stacked pins, `free` pins, missing footprint.
    let mcu = &lib.parts["MCU_Alt"];
    assert_eq!(mcu.category, Category::Mcu);
    assert_eq!(mcu.symbol.pins.len(), 6);
    let pa9 = mcu.symbol.pin("PA9").unwrap();
    let alts: Vec<(&str, PinKind)> = pa9.alternates.iter().map(|a| (a.name.as_str(), a.kind)).collect();
    assert_eq!(alts, [("TIM1_CH2", PinKind::Bidirectional), ("USART1_TX", PinKind::Output)]);
    assert_eq!(mcu.symbol.pin("BOOT0").unwrap().kind, PinKind::Passive);
    assert!(mcu.footprints.is_empty());

    let res = &lib.parts["R_Test"];
    assert_eq!(res.category, Category::Resistor);
    assert_eq!(res.value(), "10k");
    assert_eq!(res.params.get("tolerance").unwrap().to_string(), "1%");
    assert_eq!(res.footprint().unwrap().footprint, "R0603_TEST");
    assert_eq!(lib.parts["Conn_1x03_Test"].category, Category::Connector);

    // The parts work in a circuit.
    exec(&r, &mut s, "circuit.add", json!({"part": "TestLDO"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "R_Test"}));
    exec(&r, &mut s, "net.connect", json!({"net": "VOUT", "pins": ["U1.VOUT", "R1.1"]}));

    // Again: unchanged; one symbol only; unknown symbol.
    let (o, _) = exec(
        &r,
        &mut s,
        "part.import_kicad_sym",
        json!({"path": fixtures().join("cadlab_test.kicad_sym"), "symbol": ["R_Test"]}),
    );
    assert_eq!(changes(&o, "parts"), [("R_Test".to_string(), "unchanged".to_string())]);
    let e = exec_err(
        &r,
        &mut s,
        "part.import_kicad_sym",
        json!({"path": fixtures().join("cadlab_test.kicad_sym"), "symbol": ["TestLD0"]}),
    );
    assert_eq!(e.diagnostic.code, "import.symbol_not_found");
    // A category override changes the parts: a conflict without `replace`.
    let e = exec_err(
        &r,
        &mut s,
        "part.import_kicad_sym",
        json!({"path": fixtures().join("cadlab_test.kicad_sym"), "symbol": ["TestLDO"], "category": "regulator"}),
    );
    assert_eq!(e.diagnostic.code, "import.conflict");
    exec(
        &r,
        &mut s,
        "part.import_kicad_sym",
        json!({"path": fixtures().join("cadlab_test.kicad_sym"), "symbol": ["TestLDO"], "category": "regulator", "replace": true}),
    );
    assert_eq!(project(&s).library().parts["TestLDO"].category, Category::Regulator);
}

#[test]
fn import_into_a_shared_library() {
    let lib_dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let args = json!({
        "path": fixtures().join("cadlab_test.kicad_sym"),
        "footprints": fixtures().join("cadlab_test.pretty"),
        "library": lib_dir.path(),
    });
    // Without a project; a dry run writes nothing.
    let o = r.execute(&mut s, "lib.import_kicad", args.clone(), RunOptions { dry_run: true }).unwrap();
    assert_eq!(o.output["dry_run"], true);
    assert!(!lib_dir.path().join("parts").exists());
    let (o, _) = exec(&r, &mut s, "lib.import_kicad", args.clone());
    assert_eq!(o["footprints"].as_array().unwrap().len(), 4);
    assert_eq!(o["parts"].as_array().unwrap().len(), 6);
    assert!(lib_dir.path().join("parts/TestLDO.json").is_file());
    assert!(lib_dir.path().join("footprints/SOT23_TEST.json").is_file());
    let (o, _) = exec(&r, &mut s, "lib.import_kicad", args.clone());
    assert!(changes(&o, "parts").iter().chain(changes(&o, "footprints").iter()).all(|(_, c)| c == "unchanged"));

    // Symbols alone now find their footprints in the library.
    let (o, _) = exec(
        &r,
        &mut s,
        "lib.import_kicad",
        json!({"path": fixtures().join("cadlab_test.kicad_sym"), "library": lib_dir.path(), "name": ["R_Test"]}),
    );
    assert_eq!(o["parts"][0]["footprint"], "R0603_TEST");
    // Footprints alone, one of them.
    let (o, _) = exec(
        &r,
        &mut s,
        "lib.import_kicad",
        json!({"path": fixtures().join("cadlab_test.pretty"), "library": lib_dir.path(), "name": ["HDR_1x03_TEST"]}),
    );
    assert_eq!(changes(&o, "footprints"), [("HDR_1x03_TEST".to_string(), "unchanged".to_string())]);

    // And from the library into a project.
    let pdir = tempfile::tempdir().unwrap();
    exec(&r, &mut s, "project.new", json!({"path": pdir.path().join("p"), "name": "fromlib"}));
    exec(&r, &mut s, "lib.import", json!({"name": "TestLDO", "library": lib_dir.path()}));
    let p = project(&s);
    assert!(p.library().parts.contains_key("TestLDO"));
    assert!(p.library().footprints.contains_key("SOT23_TEST"));
}

// ---------------------------------------------------------------------------------------------
// Round trips through cadlab's own KiCad exports.

/// The ATtiny85 board with every component placed (some rotated, one on the bottom side).
fn placed_board() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p"), "name": "roundtrip"}));
    build_board(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "60mm", "height": "40mm"}));
    let places = [
        ("J1", "5mm", "5mm", 0, "top"),
        ("U1", "15mm", "10mm", 0, "top"),
        ("U2", "30mm", "20mm", 0, "top"),
        ("J2", "45mm", "20mm", 0, "top"),
        ("C1", "10mm", "30mm", 0, "top"),
        ("C2", "15mm", "30mm", 270, "bottom"),
        ("C3", "20mm", "30mm", 0, "top"),
        ("R1", "25mm", "30mm", 0, "top"),
        ("R2", "30mm", "30mm", 0, "bottom"),
        ("D1", "35mm", "30mm", 0, "top"),
    ];
    for (refdes, x, y, rot, side) in places {
        exec(&r, &mut s, "place.set", json!({"refdes": refdes, "at": [x, y], "rotation": rot, "side": side}));
    }
    (dir, r, s)
}

/// Cuts the footprints out of an exported board into `<dir>/rt.pretty`, one file per
/// footprint name (the first unrotated top-side placement of each: in a library file the footprint's
/// orientation is ignored, so a rotated placement would turn the pads).
fn footprints_to_pretty(pcb: &str, dir: &Path) -> PathBuf {
    let pretty = dir.join("rt.pretty");
    std::fs::create_dir_all(&pretty).unwrap();
    for span in child_lists(pcb, pcb.find("(kicad_pcb").unwrap()) {
        let text = &pcb[span.0..span.1];
        if !text.starts_with("(footprint ") {
            continue;
        }
        let fp = sexpr::parse(text).unwrap();
        let angle = fp.get("at").and_then(|a| a.items().get(3)).and_then(Sexpr::atom).unwrap_or("0");
        if fp.child_value("layer") != Some("F.Cu") || angle.parse::<f64>() != Ok(0.0) {
            continue;
        }
        let id = fp.value().unwrap();
        let name = id.rsplit_once(':').map_or(id, |(_, n)| n);
        let path = pretty.join(format!("{name}.kicad_mod"));
        if !path.exists() {
            std::fs::write(&path, format!("{text}\n")).unwrap();
        }
    }
    pretty
}

/// Byte spans of the child lists of the list opening at `open`, as written (strings and
/// escapes respected).
fn child_lists(text: &str, open: usize) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let (mut depth, mut i, mut in_str, mut start) = (0usize, open, false, 0);
    let mut out = Vec::new();
    while i < b.len() {
        match b[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'(' if !in_str => {
                depth += 1;
                if depth == 2 {
                    start = i;
                }
            }
            b')' if !in_str => {
                if depth == 2 {
                    out.push((start, i + 1));
                }
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Cuts the library symbols out of an exported schematic into a `.kicad_sym`, named without
/// their library nickname, as a symbol library stores them.
fn symbols_to_library(sch: &str, path: &Path) {
    let mut out = String::from("(kicad_symbol_lib (version 20231120) (generator \"cadlab_test\")\n");
    for (a, z) in child_lists(sch, sch.find("(lib_symbols").unwrap()) {
        let text = &sch[a..z];
        // `(symbol "lib:name"` → `(symbol "name"`.
        let q1 = text.find('"').unwrap();
        let q2 = q1 + 1 + text[q1 + 1..].find('"').unwrap();
        let name = &text[q1 + 1..q2];
        let short = name.rsplit_once(':').map_or(name, |(_, n)| n);
        out += &format!("  {}\"{short}\"{}\n", &text[..q1], &text[q2 + 1..]);
    }
    out += ")\n";
    std::fs::write(path, out).unwrap();
}

#[test]
fn round_trip_through_cadlab_exports() {
    let (dir, r, mut s) = placed_board();
    let original = project(&s).clone();
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let e = cadlab::kicad_pcb::export(&original, "board");
    let pretty = footprints_to_pretty(&e.pcb, &out);
    exec(&r, &mut s, "schematic.export", json!({"path": out.join("rt.kicad_sch")}));
    let sym = out.join("rt.kicad_sym");
    symbols_to_library(&std::fs::read_to_string(out.join("rt.kicad_sch")).unwrap(), &sym);

    let (_d2, r2, mut s2) = new_project();
    exec(&r2, &mut s2, "part.import_kicad_sym", json!({"path": sym, "footprints": pretty}));
    let imported = project(&s2).library();
    check_round_trip(&original, imported);
}

/// Footprints come back as the same land patterns, parts with the same symbol (pins, sides,
/// generated drawing), category, footprint, value, datasheet and description.
fn check_round_trip(original: &Project, imported: &cadlab::model::sections::Library) {
    let used: std::collections::BTreeSet<&str> =
        original.library().parts.values().filter_map(|p| p.footprint()).map(|f| f.footprint.as_str()).collect();
    assert_eq!(imported.footprints.len(), used.len());
    for name in used {
        let a = &original.library().footprints[name];
        let b = &imported.footprints[name];
        assert!(same_land_pattern(a, b), "footprint {name} differs:\n{a:?}\n{b:?}");
        assert_eq!(a.description, b.description);
    }
    let used_parts: std::collections::BTreeSet<&str> =
        original.circuit().components.values().map(|c| c.part.as_str()).collect();
    assert_eq!(imported.parts.len(), used_parts.len());
    for id in used_parts {
        let a = &original.library().parts[id];
        let b = imported.parts.get(id).unwrap_or_else(|| panic!("part {id} missing"));
        assert_eq!(b.category, a.category, "{id}");
        assert_eq!(b.symbol, a.symbol, "{id}");
        assert_eq!(b.footprint().map(|f| &f.footprint), a.footprint().map(|f| &f.footprint), "{id}");
        assert_eq!(b.value(), a.value(), "{id}");
        assert_eq!(b.datasheet, a.datasheet, "{id}");
        assert_eq!(b.description, a.description, "{id}");
    }
}

// ---------------------------------------------------------------------------------------------
// KiCad as the oracle.

/// Imports a symbol library (with footprints) into a fresh project; returns its library.
fn import_lib(sym: &Path, pretty: &Path) -> cadlab::model::sections::Library {
    let (_d, r, mut s) = new_project();
    exec(&r, &mut s, "part.import_kicad_sym", json!({"path": sym, "footprints": pretty}));
    project(&s).library().clone()
}

/// The two imports hold the same footprints (as land patterns) and the same parts (provenance
/// aside: it names the file).
fn assert_same_import(a: &cadlab::model::sections::Library, b: &cadlab::model::sections::Library) {
    assert_eq!(a.footprints.keys().collect::<Vec<_>>(), b.footprints.keys().collect::<Vec<_>>());
    for (k, fa) in &a.footprints {
        assert!(same_land_pattern(fa, &b.footprints[k]), "footprint {k}:\n{fa:?}\n{:?}", b.footprints[k]);
    }
    assert_eq!(a.parts.keys().collect::<Vec<_>>(), b.parts.keys().collect::<Vec<_>>());
    for (k, pa) in &a.parts {
        let mut pb = b.parts[k].clone();
        pb.provenance = pa.provenance.clone();
        assert_eq!(pa, &pb, "part {k}");
    }
}

/// `kicad-cli fp upgrade` + `sym upgrade` of a library into `out`; returns (symbols, pretty).
fn kicad_upgrade(kicad: &Path, sym: &Path, pretty: &Path, out: &Path) -> (PathBuf, PathBuf) {
    let (up_sym, up_pretty) = (out.join("up.kicad_sym"), out.join("up.pretty"));
    oracle::run(kicad, &["sym", "upgrade", "--force", sym.to_str().unwrap(), "-o", up_sym.to_str().unwrap()]);
    oracle::run(kicad, &["fp", "upgrade", "--force", pretty.to_str().unwrap(), "-o", up_pretty.to_str().unwrap()]);
    assert!(up_sym.is_file());
    // The footprint file names are the footprint names.
    let rename = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
    let mut a: Vec<String> = std::fs::read_dir(pretty).unwrap().flatten().map(|e| rename(&e.path())).collect();
    let mut b: Vec<String> = std::fs::read_dir(&up_pretty).unwrap().flatten().map(|e| rename(&e.path())).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "KiCad rewrote every footprint");
    (up_sym, up_pretty)
}

/// Symbols KiCad draws from a library (`sym export svg`).
fn kicad_svg_count(kicad: &Path, sym: &Path, out: &Path) -> usize {
    let svg = out.join("svg");
    oracle::run(kicad, &["sym", "export", "svg", sym.to_str().unwrap(), "-o", svg.to_str().unwrap()]);
    std::fs::read_dir(&svg).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "svg")).count()
}

#[test]
fn oracle_kicad_reads_the_fixtures_and_agrees() {
    let Some(kicad) = oracle::require(Oracle::KicadCli) else { return };
    let out = tempfile::tempdir().unwrap();
    let (sym, pretty) = (fixtures().join("cadlab_test.kicad_sym"), fixtures().join("cadlab_test.pretty"));
    let (up_sym, up_pretty) = kicad_upgrade(&kicad, &sym, &pretty, out.path());
    let text = std::fs::read_to_string(&up_sym).unwrap();
    assert!(text.contains("(extends \"TestLDO\")") && text.contains("(alternate \"USART1_TX\""), "KiCad kept them");
    // KiCad draws every symbol (one SVG per unit and body style; at least one per symbol).
    assert!(kicad_svg_count(&kicad, &sym, out.path()) >= 7);
    assert_same_import(&import_lib(&sym, &pretty), &import_lib(&up_sym, &up_pretty));
}

#[test]
fn oracle_kicad_reads_cadlab_round_trip_libraries() {
    let Some(kicad) = oracle::require(Oracle::KicadCli) else { return };
    let (dir, r, mut s) = placed_board();
    let original = project(&s).clone();
    let out = dir.path().join("out");
    if std::env::var("CADLAB_KEEP_TMP").is_ok_and(|v| v == "1") {
        eprintln!("keeping {}", dir.keep().display());
    }
    std::fs::create_dir_all(&out).unwrap();
    let pretty = footprints_to_pretty(&cadlab::kicad_pcb::export(&original, "board").pcb, &out);
    exec(&r, &mut s, "schematic.export", json!({"path": out.join("rt.kicad_sch")}));
    let sym = out.join("rt.kicad_sym");
    symbols_to_library(&std::fs::read_to_string(out.join("rt.kicad_sch")).unwrap(), &sym);
    let (up_sym, up_pretty) = kicad_upgrade(&kicad, &sym, &pretty, &out);
    let direct = import_lib(&sym, &pretty);
    let upgraded = import_lib(&up_sym, &up_pretty);
    assert_same_import(&direct, &upgraded);
    check_round_trip(&original, &upgraded);
}
