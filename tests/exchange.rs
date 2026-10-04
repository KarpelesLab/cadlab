//! Exchange outputs (M9): IPC-2581 rev C, STEP AP214, IDF 3.0 and IDX (EDMD). Golden files
//! (`CADLAB_BLESS=1`), structural checks that parse the files back (independent of the writers),
//! commands, and optional oracles: FreeCAD reads the STEP file (`CADLAB_ORACLE_FREECAD`), xmllint
//! validates the IPC-2581 and IDX files against schemas the user supplies (`CADLAB_IPC2581_XSD`,
//! `CADLAB_IDX_XSD`).

mod common;
mod fab_support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::fabout::{self, Options};
use cadlab::mcad::{self, Loop};
use common::oracle::{self, Oracle};
use fab_support::{exec, tiny_board};
use serde_json::json;

/// The tiny fab board plus a mounting hole, a rectangular cutout and a ground pour on the
/// bottom.
fn board() -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = tiny_board();
    exec(&r, &mut s, "board.hole", json!({"at": ["22mm", "12.5mm"], "drill": "2.2mm"}));
    exec(&r, &mut s, "board.cutout", json!({"rect": {"from": ["20mm", "2mm"], "to": ["23mm", "4mm"]}}));
    exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "gnd", "net": "GND", "layers": ["B.Cu"],
               "outline": [["1mm", "1mm"], ["10mm", "1mm"], ["10mm", "6mm"], ["1mm", "6mm"]]}),
    );
    (d, r, s)
}

fn options() -> Options {
    Options { version: "test".into(), ..Options::default() }
}

fn mcad_options() -> mcad::Options {
    mcad::Options { version: "test".into(), ..mcad::Options::default() }
}

fn golden(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden").join("exchange").join(name)
}

#[test]
fn golden_files() {
    let (_d, _r, s) = board();
    let p = s.project.as_ref().unwrap();
    let ipc = fabout::ipc2581::document(p, &options());
    assert_eq!(ipc.name, "tiny-ipc2581.xml");
    common::golden::assert_golden(&golden(&ipc.name), &ipc.content);
    let step = mcad::step::export(p, &mcad_options()).unwrap();
    common::golden::assert_golden(&golden("tiny.step"), &step.content);
    let idf = mcad::idf::export(p, &mcad_options()).unwrap();
    common::golden::assert_golden(&golden("tiny.emn"), &idf.board);
    common::golden::assert_golden(&golden("tiny.emp"), &idf.library);
}

#[test]
fn deterministic() {
    let (_d, _r, s) = board();
    let (_d2, _r2, s2) = board();
    let (p, p2) = (s.project.as_ref().unwrap(), s2.project.as_ref().unwrap());
    assert_eq!(fabout::ipc2581::document(p, &options()), fabout::ipc2581::document(p2, &options()));
    assert_eq!(
        mcad::step::export(p, &mcad_options()).unwrap().content,
        mcad::step::export(p2, &mcad_options()).unwrap().content
    );
    assert_eq!(
        mcad::idf::export(p, &mcad_options()).unwrap().board,
        mcad::idf::export(p2, &mcad_options()).unwrap().board
    );
}

// ---------------------------------------------------------------------------------------------
// A minimal XML reader: elements and attributes only (the writer emits no text content).

#[derive(Debug)]
struct El {
    name: String,
    attrs: BTreeMap<String, String>,
    children: Vec<El>,
    /// Text content (unescaped, untrimmed), empty when there is none.
    text: String,
}

impl El {
    fn attr(&self, k: &str) -> &str {
        self.attrs.get(k).map(String::as_str).unwrap_or_else(|| panic!("<{}> has no `{k}`", self.name))
    }

    fn kids<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a El> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    fn kid<'a>(&'a self, name: &'a str) -> &'a El {
        self.kids(name).next().unwrap_or_else(|| panic!("<{}> has no <{name}>", self.name))
    }

    fn walk<'a>(&'a self, out: &mut Vec<&'a El>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }

    fn all(&self) -> Vec<&El> {
        let mut v = Vec::new();
        self.walk(&mut v);
        v
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let j = rest[i..].find(';').expect("unterminated entity") + i;
        let ent = &rest[i + 1..j];
        match ent {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            e if e.starts_with('#') => out.push(char::from_u32(e[1..].parse().unwrap()).unwrap()),
            e => panic!("unknown entity &{e};"),
        }
        rest = &rest[j + 1..];
    }
    assert!(!rest.contains('<'), "raw < in attribute");
    out.push_str(rest);
    out
}

fn parse_xml(xml: &str) -> El {
    let xml = xml.strip_prefix("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n").expect("XML declaration");
    let mut stack: Vec<El> =
        vec![El { name: "#doc".into(), attrs: BTreeMap::new(), children: vec![], text: String::new() }];
    let mut rest = xml;
    loop {
        let Some(i) = rest.find('<') else {
            assert!(rest.trim().is_empty(), "text outside elements: {rest:?}");
            break;
        };
        if !rest[..i].trim().is_empty() {
            let top = stack.last_mut().unwrap();
            assert!(top.name != "#doc" && top.children.is_empty(), "mixed content: {:?}", &rest[..i]);
            top.text.push_str(&unescape(&rest[..i]));
        }
        let j = rest[i..].find('>').expect("unterminated tag") + i;
        let tag = &rest[i + 1..j];
        rest = &rest[j + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            let el = stack.pop().unwrap();
            assert_eq!(el.name, name, "mismatched end tag");
            stack.last_mut().unwrap().children.push(el);
            continue;
        }
        let (body, empty) = match tag.strip_suffix('/') {
            Some(b) => (b, true),
            None => (tag, false),
        };
        let name_end = body.find(' ').unwrap_or(body.len());
        let name = &body[..name_end];
        assert!(!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_:.".contains(c)), "{name}");
        let mut attrs = BTreeMap::new();
        let mut a = body[name_end..].trim_start();
        while !a.is_empty() {
            let eq = a.find("=\"").expect("attribute without value");
            let key = a[..eq].trim().to_string();
            let close = a[eq + 2..].find('"').unwrap() + eq + 2;
            let val = unescape(&a[eq + 2..close]);
            assert!(attrs.insert(key.clone(), val).is_none(), "duplicate attribute {key}");
            a = a[close + 1..].trim_start();
        }
        let el = El { name: name.to_string(), attrs, children: vec![], text: String::new() };
        if empty {
            stack.last_mut().unwrap().children.push(el);
        } else {
            stack.push(el);
        }
    }
    assert_eq!(stack.len(), 1, "unclosed elements");
    let mut doc = stack.pop().unwrap();
    assert_eq!(doc.children.len(), 1, "one root element");
    doc.children.pop().unwrap()
}

#[test]
fn ipc2581_structure() {
    let (_d, _r, s) = board();
    let p = s.project.as_ref().unwrap();
    let f = fabout::ipc2581::document(p, &options());
    assert_eq!(f.function, "IPC-2581C");
    let root = parse_xml(&f.content);
    assert_eq!(root.name, "IPC-2581");
    assert_eq!(root.attr("revision"), "C");
    assert_eq!(root.attr("xmlns"), "http://webstds.ipc.org/2581");
    assert!(root.all().iter().all(|e| e.text.is_empty()), "attributes only");
    let top: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(top, ["Content", "LogisticHeader", "Bom", "Ecad"]);

    let content = root.kid("Content");
    assert_eq!(content.kid("FunctionMode").attr("mode"), "ASSEMBLY");
    let order: Vec<&str> =
        content.children.iter().map(|c| c.name.as_str()).collect::<BTreeSet<_>>().into_iter().collect();
    assert_eq!(order, ["BomRef", "DictionaryLineDesc", "DictionaryStandard", "FunctionMode", "LayerRef", "StepRef"]);
    let std_ids: BTreeSet<&str> =
        content.kid("DictionaryStandard").kids("EntryStandard").map(|e| e.attr("id")).collect();
    let line_ids: BTreeSet<&str> =
        content.kid("DictionaryLineDesc").kids("EntryLineDesc").map(|e| e.attr("id")).collect();
    for e in content.kid("DictionaryStandard").kids("EntryStandard") {
        assert_eq!(e.children.len(), 1, "one primitive per entry");
    }

    let ecad = root.kid("Ecad");
    let cad = ecad.kid("CadData");
    let layers: Vec<&str> = cad.kids("Layer").map(|l| l.attr("name")).collect();
    let layer_refs: Vec<&str> = content.kids("LayerRef").map(|l| l.attr("name")).collect();
    assert_eq!(layers, layer_refs, "every layer is referenced from the content");
    assert_eq!(
        layers,
        ["F.SilkS", "F.Paste", "F.Mask", "F.Cu", "Dielectric1", "B.Cu", "B.Mask", "B.Paste", "B.SilkS", "DRILL_1_2"]
    );
    let drill = cad.kids("Layer").find(|l| l.attr("layerFunction") == "DRILL").unwrap();
    assert_eq!(drill.kid("Span").attr("fromLayer"), "F.Cu");
    assert_eq!(drill.kid("Span").attr("toLayer"), "B.Cu");

    // Stackup adds up to the board thickness.
    let stack = cad.kid("Stackup");
    assert_eq!(stack.attr("overallThickness"), "1.6");
    let sum: f64 =
        stack.kid("StackupGroup").kids("StackupLayer").map(|l| l.attr("thickness").parse::<f64>().unwrap()).sum();
    assert!((sum - 1.6).abs() < 1e-9, "{sum}");

    // References resolve.
    for el in root.all() {
        match el.name.as_str() {
            "StandardPrimitiveRef" => assert!(std_ids.contains(el.attr("id")), "{}", el.attr("id")),
            "LineDescRef" => assert!(line_ids.contains(el.attr("id")), "{}", el.attr("id")),
            "LayerFeature" => assert!(layers.contains(&el.attr("layerRef"))),
            _ => {}
        }
    }

    let step = cad.kid("Step");
    let names: Vec<&str> = step.children.iter().map(|c| c.name.as_str()).collect();
    let rank = |n: &str| {
        ["Datum", "Profile", "Package", "Component", "LogicalNet", "LayerFeature"].iter().position(|x| *x == n)
    };
    assert!(names.windows(2).all(|w| rank(w[0]).unwrap() <= rank(w[1]).unwrap()), "step order: {names:?}");
    // Profile: the outline with its four corner arcs, and the cutout.
    let profile = step.kid("Profile");
    assert_eq!(profile.kid("Polygon").kids("PolyStepCurve").count(), 4);
    assert_eq!(profile.kids("Cutout").count(), 1);
    for poly in profile.children.iter() {
        let first = &poly.children[0];
        let last = poly.children.last().unwrap();
        assert_eq!((first.attr("x"), first.attr("y")), (last.attr("x"), last.attr("y")), "closed");
    }

    // Packages cover every component; components are the placed footprints.
    let packages: BTreeSet<&str> = step.kids("Package").map(|pk| pk.attr("name")).collect();
    let comps: Vec<&El> = step.kids("Component").collect();
    assert_eq!(comps.len(), p.board().footprints.len());
    for c in &comps {
        assert!(packages.contains(c.attr("packageRef")));
    }
    let c2 = comps.iter().find(|c| c.attr("refDes") == "C2").unwrap();
    assert_eq!(c2.attr("layerRef"), "B.Cu");
    assert_eq!(c2.kid("Xform").attr("mirror"), "true");
    assert_eq!(c2.kid("Xform").attr("rotation"), "90");
    let sot = step.kids("Package").find(|pk| pk.kids("Pin").count() == 5).unwrap();
    assert_eq!(sot.kid("LandPattern").kids("Pad").count(), 5);
    assert_eq!(sot.attr("pinOne"), "1");

    // Logical nets: every placed component pad with a net.
    let pads = cadlab::board::placed_pads(p);
    let netted = pads.iter().filter(|pp| pp.net.is_some() && !pp.number.is_empty() && pp.refdes != "H1").count();
    let pin_refs: usize = step.kids("LogicalNet").map(|n| n.kids("PinRef").count()).sum();
    assert_eq!(pin_refs, netted);
    let nets: BTreeSet<&str> = step.kids("LogicalNet").map(|n| n.attr("name")).collect();
    assert_eq!(nets, ["3V3", "GND", "VIN"].into());

    // Copper features: every pad and via of the layer, tracks, the pour on the bottom.
    let feature = |layer: &str| step.kids("LayerFeature").find(|l| l.attr("layerRef") == layer).unwrap();
    let count = |el: &El, name: &str| el.all().iter().filter(|e| e.name == name).count();
    for (layer, tracks) in [("F.Cu", 1), ("B.Cu", 2)] {
        let lf = feature(layer);
        let on = pads.iter().filter(|pp| pp.layers.iter().any(|l| l == layer)).count();
        assert_eq!(count(lf, "Pad"), on + p.board().vias.len(), "{layer}");
        assert_eq!(count(lf, "Line") + count(lf, "Arc"), tracks, "{layer}");
        let vias: usize = lf
            .kids("Set")
            .filter(|s| s.attrs.get("padUsage").is_some_and(|u| u == "VIA"))
            .map(|s| s.kids("Pad").count())
            .sum();
        assert_eq!(vias, 2);
    }
    assert!(count(feature("B.Cu"), "Contour") >= 1, "zone fill");
    assert_eq!(count(feature("B.Cu"), "Arc"), 1, "the arc track");
    // Masks: SMD pads of the side plus every hole pad; paste: SMD pads.
    let top_smd = pads.iter().filter(|pp| pp.layers == ["F.Cu"]).count();
    let holes = pads.iter().filter(|pp| pp.hole.is_some()).count();
    assert_eq!(count(feature("F.Mask"), "Pad"), top_smd + holes);
    assert_eq!(count(feature("F.Paste"), "Pad"), top_smd);
    assert_eq!(count(feature("B.Paste"), "Pad"), 2);
    // Legend: reference designators and the board text.
    let silk = feature("F.SilkS");
    assert!(silk.kids("Set").any(|s| s.attrs.get("geometryUsage").is_some_and(|g| g == "TEXT")));
    // Drill: every hole, plating status by kind.
    let hs = fabout::holes(p);
    let drill_f = feature("DRILL_1_2");
    let holes_xml: Vec<&El> = drill_f.all().into_iter().filter(|e| e.name == "Hole").collect();
    assert_eq!(holes_xml.len(), hs.len());
    assert_eq!(holes_xml.iter().filter(|h| h.attr("platingStatus") == "VIA").count(), 2);
    assert_eq!(holes_xml.iter().filter(|h| h.attr("platingStatus") == "NONPLATED").count(), 1);

    // BOM: one item per BOM line, DNP not populated.
    let bom = root.kid("Bom");
    assert_eq!(bom.kids("BomItem").count(), cadlab::bom::rows(p).len());
    let c1 = bom.all().into_iter().find(|e| e.name == "RefDes" && e.attr("name") == "C1").unwrap();
    assert_eq!(c1.attr("populate"), "false");
    let u1 = bom.kids("BomItem").find(|i| i.kids("RefDes").any(|r| r.attr("name") == "U1")).unwrap();
    let mpn = u1.kid("Characteristics").kids("Textual").find(|t| t.attr("textualCharacteristicName") == "MPN").unwrap();
    assert_eq!(mpn.attr("textualCharacteristicValue"), "AP2112K-3.3TRG1");
}

/// Optional schema validation: the IPC-2581C XSD is published by IPC
/// (`http://webstds.ipc.org/2581/IPC-2581C.xsd`) but not redistributed here; point
/// `CADLAB_IPC2581_XSD` at a local copy.
#[test]
fn ipc2581_schema_oracle() {
    let Some(xsd) = std::env::var_os("CADLAB_IPC2581_XSD") else {
        eprintln!("skipping: set CADLAB_IPC2581_XSD to a local IPC-2581C.xsd");
        return;
    };
    let Some(xmllint) = oracle::optional(Oracle::Xmllint) else { return };
    let (d, _r, s) = board();
    let f = fabout::ipc2581::document(s.project.as_ref().unwrap(), &options());
    let path = d.path().join(&f.name);
    std::fs::write(&path, &f.content).unwrap();
    oracle::run(&xmllint, &["--noout", "--schema", &xsd.to_string_lossy(), &path.to_string_lossy()]);
}

// ---------------------------------------------------------------------------------------------
// STEP: a Part 21 reader for the DATA section.

/// Entities by id: (type name or complex body, full body).
fn parse_step(text: &str) -> BTreeMap<u64, String> {
    assert!(text.starts_with("ISO-10303-21;\nHEADER;\n"));
    assert!(text.ends_with("ENDSEC;\nEND-ISO-10303-21;\n"));
    assert!(text.contains("FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));"));
    let data = &text[text.find("DATA;\n").unwrap() + 6..text.rfind("ENDSEC;").unwrap()];
    // Split on ';' outside strings.
    let mut out = BTreeMap::new();
    let mut cur = String::new();
    let mut in_str = false;
    for c in data.chars() {
        if c == '\'' {
            in_str = !in_str;
        }
        if c == ';' && !in_str {
            let e = cur.trim();
            let (id, body) = e.split_once('=').unwrap();
            let id: u64 = id.trim().strip_prefix('#').unwrap().parse().unwrap();
            assert!(out.insert(id, body.trim().to_string()).is_none(), "duplicate #{id}");
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    assert!(cur.trim().is_empty());
    out
}

fn refs(body: &str) -> Vec<u64> {
    let mut v = Vec::new();
    let mut in_str = false;
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\'' {
            in_str = !in_str;
        } else if b[i] == b'#' && !in_str {
            let j = (i + 1..b.len()).find(|&j| !b[j].is_ascii_digit()).unwrap_or(b.len());
            v.push(body[i + 1..j].parse().unwrap());
            i = j;
            continue;
        }
        i += 1;
    }
    v
}

fn kind(body: &str) -> &str {
    &body[..body.find('(').unwrap()]
}

#[test]
fn step_structure() {
    let (_d, _r, s) = board();
    let p = s.project.as_ref().unwrap();
    let out = mcad::step::export(p, &mcad_options()).unwrap();
    let ents = parse_step(&out.content);
    for (id, body) in &ents {
        for r in refs(body) {
            assert!(ents.contains_key(&r), "#{id} references undefined #{r}");
        }
    }
    let of = |k: &str| ents.iter().filter(|(_, b)| kind(b) == k).map(|(i, _)| *i).collect::<Vec<_>>();
    // Bodies: U1, C2, J1 (C1 is DNP); C2 and J1... each footprint is its own part.
    let (bodies, missing) = mcad::bodies(p);
    assert!(missing.is_empty(), "{missing:?}");
    assert_eq!(bodies.iter().map(|b| b.refdes.as_str()).collect::<Vec<_>>(), ["C2", "J1", "U1"]);
    assert_eq!(out.bodies, 3);
    let footprints: BTreeSet<&str> = bodies.iter().map(|b| b.footprint.as_str()).collect();
    assert_eq!(of("MANIFOLD_SOLID_BREP").len(), 1 + footprints.len());
    assert_eq!(of("NEXT_ASSEMBLY_USAGE_OCCURRENCE").len(), 1 + bodies.len());
    assert_eq!(of("PRODUCT").len(), 1 + 1 + footprints.len(), "assembly, board, bodies");
    // Holes: J1's two pad holes and the mounting hole (vias are not drilled by default).
    assert_eq!(out.holes, 3);
    assert!(out.skipped_holes.is_empty());
    assert_eq!(of("CYLINDRICAL_SURFACE").len(), 3 + 4, "holes and outline corners");

    // Every shell is closed: each edge is used exactly twice, once in each direction.
    for shell in of("CLOSED_SHELL") {
        let mut uses: BTreeMap<u64, (u32, u32)> = BTreeMap::new();
        for face in refs(&ents[&shell]) {
            assert_eq!(kind(&ents[&face]), "ADVANCED_FACE");
            for bound in refs(&ents[&face]).into_iter().filter(|r| kind(&ents[r]).contains("BOUND")) {
                let lp = refs(&ents[&bound])[0];
                assert_eq!(kind(&ents[&lp]), "EDGE_LOOP");
                for oe in refs(&ents[&lp]) {
                    let body = &ents[&oe];
                    assert_eq!(kind(body), "ORIENTED_EDGE");
                    let edge = refs(body)[0];
                    let u = uses.entry(edge).or_default();
                    if body.ends_with(".T.)") { u.0 += 1 } else { u.1 += 1 }
                }
            }
        }
        for (edge, u) in uses {
            assert_eq!(u, (1, 1), "edge #{edge} in shell #{shell}");
        }
    }

    // Options: vias drilled, bodies left out.
    let o = mcad::Options { vias: true, components: false, ..mcad_options() };
    let out = mcad::step::export(p, &o).unwrap();
    assert_eq!((out.holes, out.bodies), (5, 0));
    assert_eq!(parse_step(&out.content).values().filter(|b| kind(b) == "MANIFOLD_SOLID_BREP").count(), 1);
}

/// Board volume expected from the outline, cutouts and holes (mm³).
fn board_volume(p: &cadlab::model::Project, vias: bool) -> f64 {
    let (outer, cutouts) = mcad::board_profile(p).unwrap();
    let mut area = outer.signed_area() + cutouts.iter().map(Loop::signed_area).sum::<f64>();
    let (holes, _) = mcad::cuttable_holes(&outer, &cutouts, mcad::drill_holes(p, vias));
    for h in holes {
        let r = h.diameter.0 as f64 / 2.0;
        area -= std::f64::consts::PI * r * r;
    }
    area * p.board().stackup.thickness.0 as f64 / 1e18
}

#[test]
fn step_freecad_oracle() {
    let Some(freecad) = oracle::optional(Oracle::FreeCad) else { return };
    let (d, _r, s) = board();
    let p = s.project.as_ref().unwrap();
    let out = mcad::step::export(p, &mcad_options()).unwrap();
    let path = d.path().join("tiny.step");
    std::fs::write(&path, &out.content).unwrap();
    let script = d.path().join("check.py");
    std::fs::write(
        &script,
        format!(
            "import Part\ns = Part.read(r'{}')\nprint('VALID', s.isValid())\nfor so in s.Solids:\n    print('SOLID', so.isValid(), so.isClosed(), '%.6f' % so.Volume)\n",
            path.display()
        ),
    )
    .unwrap();
    let stdout = oracle::run(&freecad, &["-c", &format!("exec(open(r'{}').read())", script.display())]);
    assert!(stdout.contains("VALID True"), "{stdout}");
    let solids: Vec<f64> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("SOLID "))
        .map(|l| {
            assert!(l.starts_with("True True "), "invalid solid: {l}");
            l.rsplit(' ').next().unwrap().parse().unwrap()
        })
        .collect();
    let (bodies, _) = mcad::bodies(p);
    assert_eq!(solids.len(), 1 + bodies.len(), "{stdout}");
    let expected = board_volume(p, false);
    assert!((solids[0] - expected).abs() < 1e-3, "board volume {} vs {expected}", solids[0]);
    let mut want: Vec<f64> =
        bodies.iter().map(|b| b.width.0 as f64 * b.length.0 as f64 * b.height.0 as f64 / 1e18).collect();
    let mut got = solids[1..].to_vec();
    want.sort_by(f64::total_cmp);
    got.sort_by(f64::total_cmp);
    for (g, w) in got.iter().zip(&want) {
        assert!((g - w).abs() < 1e-4, "body volume {g} vs {w}");
    }
}

// ---------------------------------------------------------------------------------------------
// IDF.

/// Sections of an IDF file: name → records (lines), in order.
fn idf_sections(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut open: Option<(String, Vec<String>)> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix(".END_") {
            let (n, recs) = open.take().expect("end without start");
            assert!(n.starts_with(name), "{n} closed by .END_{name}");
            out.push((name.to_string(), recs));
        } else if let Some(name) = line.strip_prefix('.') {
            assert!(open.is_none(), "nested section {name}");
            open = Some((name.to_string(), Vec::new()));
        } else {
            open.as_mut().expect("record outside a section").1.push(line.to_string());
        }
    }
    assert!(open.is_none());
    out
}

#[test]
fn idf_structure() {
    let (_d, _r, s) = board();
    let p = s.project.as_ref().unwrap();
    let out = mcad::idf::export(p, &mcad_options()).unwrap();
    let board = idf_sections(&out.board);
    let names: Vec<&str> = board.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["HEADER", "BOARD_OUTLINE", "DRILLED_HOLES", "PLACEMENT"]);
    assert!(board[0].1[0].starts_with("BOARD_FILE 3.0 \"cadlab test\" "));
    assert_eq!(board[0].1[1], "\"tiny\" MM");
    let outline = &board[1].1;
    assert_eq!(outline[0], "1.6");
    // Loops: 0 (outline, closed, four 90° arcs) and 1 (the cutout, closed).
    let pts: Vec<Vec<&str>> = outline[1..].iter().map(|l| l.split(' ').collect()).collect();
    for label in ["0", "1"] {
        let lp: Vec<&Vec<&str>> = pts.iter().filter(|r| r[0] == label).collect();
        assert_eq!(lp.first().unwrap()[1..3], lp.last().unwrap()[1..3], "loop {label} closed");
    }
    assert_eq!(pts.iter().filter(|r| r[3] == "90").count(), 4);
    // Holes: J1's pins and the mounting hole.
    let holes = &board[2].1;
    assert_eq!(holes.len(), 3);
    assert!(holes.iter().any(|h| h.ends_with("NPTH \"BOARD\" MTG ECAD")));
    assert_eq!(holes.iter().filter(|h| h.contains("PTH \"J1\" PIN")).count(), 2);
    // Placement: two records per populated, placed component with a body.
    let place = &board[3].1;
    assert_eq!(place.len(), 2 * 3);
    let c2 = place.iter().position(|l| l.ends_with("\"C2\"")).unwrap();
    assert_eq!(place[c2 + 1], "18 9 0 90 BOTTOM PLACED");
    // Library: one entry per geometry and part number, closed outline, height.
    let lib = idf_sections(&out.library);
    assert_eq!(lib[0].0, "HEADER");
    let entries: Vec<&(String, Vec<String>)> = lib.iter().filter(|(n, _)| n == "ELECTRICAL").collect();
    assert_eq!(entries.len(), 3);
    for (_, recs) in &entries {
        assert!(recs[0].ends_with(|c: char| c.is_ascii_digit()) && recs[0].contains(" MM "), "{}", recs[0]);
        assert_eq!(recs.len(), 1 + 5);
        assert_eq!(recs[1], recs[5]);
    }
    // Every placed geometry/part number pair is in the library.
    for i in (0..place.len()).step_by(2) {
        let key = place[i].rsplit_once(' ').unwrap().0;
        assert!(entries.iter().any(|(_, r)| r[0].starts_with(key)), "{key}");
    }
}

// ---------------------------------------------------------------------------------------------
// IDX (ProSTEP iViP EDMD).

/// The exchange board plus two keep-outs: one forbidding everything on all layers, one
/// forbidding top-side components only.
fn idx_board() -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = board();
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "mech", "outline": {"rect": {"from": ["20mm", "10mm"], "to": ["24mm", "14mm"]}}}),
    );
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "tall", "layers": ["F.Cu"], "no_footprints": true,
               "outline": [["14mm", "1mm"], ["18mm", "1mm"], ["18mm", "4mm"], ["14mm", "4mm"]]}),
    );
    (d, r, s)
}

/// Elements whose text is a reference (`xs:IDREF`) to another element's `id`.
const IDX_REFS: &[&str] = &[
    "foundation:System",
    "foundation:SystemScope",
    "foundation:GlobalUnitLength",
    "property:Unit",
    "pdm:Item",
    "pdm:Shape",
    "pdm:ShapeElement",
    "pdm:DefiningShape",
    "pdm:Stratum",
    "d2:Point",
    "d2:StartPoint",
    "d2:EndPoint",
    "d2:Center",
    "d2:Curve",
    "d2:DetailedGeometricModelElement",
];

#[test]
fn idx_golden_and_deterministic() {
    let (_d, _r, s) = idx_board();
    let (_d2, _r2, s2) = idx_board();
    let out = mcad::idx::export(s.project.as_ref().unwrap(), &mcad_options()).unwrap();
    common::golden::assert_golden(&golden("tiny.idx"), &out.content);
    assert_eq!(out.content, mcad::idx::export(s2.project.as_ref().unwrap(), &mcad_options()).unwrap().content);
}

#[test]
fn idx_structure() {
    let (_d, _r, s) = idx_board();
    let p = s.project.as_ref().unwrap();
    let out = mcad::idx::export(p, &mcad_options()).unwrap();
    let root = parse_xml(&out.content);
    assert_eq!(root.name, "foundation:EDMDDataSet");
    assert_eq!(root.attr("xmlns:foundation"), "http://www.prostep.org/ecad-mcad/edmd/4.0/foundation");
    let top: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(top, ["foundation:Header", "foundation:Body", "foundation:ProcessInstruction"]);
    assert_eq!(
        root.kid("foundation:ProcessInstruction").attr("xsi:type"),
        "computational:EDMDProcessInstructionSendInformation"
    );
    let header = root.kid("foundation:Header");
    assert_eq!(header.kid("foundation:CreationDateTime").text, "1970-01-01T00:00:00Z");
    assert_eq!(header.kid("foundation:Description").text, "tiny");

    // Ids are unique and every reference resolves.
    let all = root.all();
    let mut ids: BTreeMap<&str, &El> = BTreeMap::new();
    for e in &all {
        if let Some(id) = e.attrs.get("id") {
            assert!(ids.insert(id, e).is_none(), "duplicate id {id}");
        }
    }
    for e in &all {
        if IDX_REFS.contains(&e.name.as_str()) && e.children.is_empty() && e.name != "foundation:System" {
            assert!(ids.contains_key(e.text.as_str()), "<{}> {} does not resolve", e.name, e.text);
        }
    }
    assert!(ids.contains_key(header.kid("foundation:System").text.as_str()));
    let get = |id: &str| *ids.get(id).unwrap_or_else(|| panic!("no {id}"));
    let body = root.kid("foundation:Body");

    // Top-level occurrences by geometry type.
    let occ: Vec<&El> = body.kids("foundation:Item").filter(|i| i.attrs.contains_key("GeometryType")).collect();
    let mut by_type: BTreeMap<&str, usize> = BTreeMap::new();
    for o in &occ {
        *by_type.entry(o.attr("GeometryType")).or_default() += 1;
        assert_eq!(o.kid("pdm:ItemType").text, "assembly");
        assert_eq!(o.kids("pdm:ItemInstance").count(), 1);
        let single = get(&o.kid("pdm:ItemInstance").kid("pdm:Item").text);
        assert_eq!(single.kid("pdm:ItemType").text, "single");
        assert_eq!(single.kid("pdm:Identifier").kid("foundation:SystemScope").text, "CADLAB");
    }
    // Board; J1's two plated pin holes and the non-plated mounting hole; U1, C2, J1 (C1 is
    // DNP); "mech" forbids routing, vias, pours and components on both sides, "tall" top-side
    // components.
    assert_eq!(
        by_type,
        [
            ("BOARD_OUTLINE", 1),
            ("COMPONENT", 3),
            ("HOLE_NON_PLATED", 1),
            ("HOLE_PLATED", 2),
            ("KEEPOUT_AREA_COMPONENT", 3),
            ("KEEPOUT_AREA_OTHER", 1),
            ("KEEPOUT_AREA_ROUTING", 1),
            ("KEEPOUT_AREA_VIA", 1),
        ]
        .into()
    );
    assert_eq!((out.components, out.holes, out.keepouts), (3, 3, 6));
    let numbers: BTreeSet<&str> =
        all.iter().filter(|e| e.name == "pdm:Identifier").map(|e| e.kid("foundation:Number").text.as_str()).collect();
    assert_eq!(numbers.len(), all.iter().filter(|e| e.name == "pdm:Identifier").count(), "unique numbers");

    let curve_set = |shape_element: &El| get(&shape_element.kid("pdm:DefiningShape").text);
    let bound = |cs: &El, b: &str| cs.kids(b).next().map(|e| e.kid("property:Value").text.clone());
    fn instance(o: &El) -> &El {
        o.kid("pdm:ItemInstance")
    }
    let single_of = |o: &El| get(&instance(o).kid("pdm:Item").text);
    let prop = |e: &El, k: &str| {
        e.kids("foundation:UserProperty")
            .find(|u| u.kid("property:Key").kid("foundation:ObjectName").text == k)
            .map(|u| u.kid("property:Value").text.clone())
    };

    // Board: a stratum with the outline (four 90° arcs) and the inverted cutout, 1.6 mm thick.
    let board = occ.iter().find(|o| o.attr("GeometryType") == "BOARD_OUTLINE").unwrap();
    assert_eq!(prop(instance(board), "THICKNESS").as_deref(), Some("1.6"));
    let stratum = get(&single_of(board).kid("pdm:Shape").text);
    assert_eq!(stratum.name, "foundation:Stratum");
    let ses: Vec<&El> = stratum.kids("pdm:ShapeElement").map(|e| get(&e.text)).collect();
    assert_eq!(ses.iter().map(|s| s.kid("pdm:Inverted").text.as_str()).collect::<Vec<_>>(), ["false", "true"]);
    for se in &ses {
        let cs = curve_set(se);
        assert_eq!((bound(cs, "d2:LowerBound"), bound(cs, "d2:UpperBound")), (Some("0".into()), Some("1.6".into())));
    }
    let outline = get(&curve_set(ses[0]).kid("d2:DetailedGeometricModelElement").text);
    assert_eq!(outline.name, "foundation:CompositeCurve");
    let arcs: Vec<&El> =
        outline.kids("d2:Curve").map(|c| get(&c.text)).filter(|c| c.name == "foundation:Arc").collect();
    assert_eq!(arcs.len(), 4);
    for a in arcs {
        assert_eq!(a.kid("d2:IncludeAngle").kid("property:Value").text, "90", "counter-clockwise outline");
    }

    // Holes: circles of the drill diameter at their position, cut through the board.
    let holes = fabout::holes(p);
    for o in occ.iter().filter(|o| o.attr("GeometryType").starts_with("HOLE_")) {
        let t = instance(o).kid("pdm:Transformation");
        assert_eq!(t.kid("pdm:TransformationType").text, "d2");
        let (x, y) = (&t.kid("pdm:tx").kid("property:Value").text, &t.kid("pdm:ty").kid("property:Value").text);
        let isf = get(&single_of(o).kid("pdm:Shape").text);
        assert_eq!(isf.kid("pdm:Stratum").text, stratum.attr("id"));
        let plated = o.attr("GeometryType") == "HOLE_PLATED";
        assert_eq!(isf.kid("pdm:InterStratumFeatureType").text, if plated { "PlatedCutout" } else { "Cutout" });
        let se = get(&isf.kid("pdm:ShapeElement").text);
        assert_eq!(se.kid("pdm:Inverted").text, "true");
        let circle = get(&curve_set(se).kid("d2:DetailedGeometricModelElement").text);
        let d = &circle.kid("d2:Diameter").kid("property:Value").text;
        assert!(
            holes.iter().any(|h| fabout::gerber::mm(h.at.x.0) == *x
                && fabout::gerber::mm(h.at.y.0) == *y
                && fabout::gerber::mm(h.diameter.0) == *d
                && h.plated == plated),
            "hole at {x} {y}"
        );
    }

    // Keep-outs: routing/via/plane through the board, components from a surface outward.
    for o in occ.iter().filter(|o| o.attr("GeometryType").starts_with("KEEPOUT_")) {
        let ko = get(&single_of(o).kid("pdm:Shape").text);
        assert_eq!(ko.name, "foundation:KeepOut");
        let cs = curve_set(get(&ko.kid("pdm:ShapeElement").text));
        let bounds = (bound(cs, "d2:LowerBound"), bound(cs, "d2:UpperBound"));
        match (o.attr("GeometryType"), prop(instance(o), "SIDE").as_deref()) {
            ("KEEPOUT_AREA_COMPONENT", Some("TOP")) => {
                assert_eq!(ko.kid("pdm:Purpose").text, "ComponentPlacement");
                assert_eq!(bounds, (Some("1.6".into()), None));
            }
            ("KEEPOUT_AREA_COMPONENT", Some("BOTTOM")) => assert_eq!(bounds, (None, Some("0".into()))),
            (g, None) => {
                let purpose =
                    [("KEEPOUT_AREA_ROUTING", "Route"), ("KEEPOUT_AREA_VIA", "Via"), ("KEEPOUT_AREA_OTHER", "Plane")]
                        .iter()
                        .find(|(t, _)| *t == g)
                        .unwrap()
                        .1;
                assert_eq!(ko.kid("pdm:Purpose").text, purpose);
                assert_eq!(bounds, (Some("0".into()), Some("1.6".into())));
            }
            other => panic!("unexpected keep-out {other:?}"),
        }
    }

    // Components: body rectangle extruded to the package height, placed by a 3D transform.
    let (bodies, _) = mcad::bodies(p);
    for o in occ.iter().filter(|o| o.attr("GeometryType") == "COMPONENT") {
        let refdes = &instance(o).kid("foundation:Name").text;
        let b = bodies.iter().find(|b| &b.refdes == refdes).unwrap();
        assert_eq!(prop(instance(o), "REFDES").as_ref(), Some(refdes));
        let single = single_of(o);
        assert_eq!(prop(single, "PARTNUM").as_ref(), Some(&b.part_number));
        assert_eq!(single.kid("pdm:PackageName").kid("foundation:ObjectName").text, b.footprint);
        let ac = get(&single.kid("pdm:Shape").text);
        assert_eq!(ac.kid("pdm:AssemblyComponentType").text, "Physical");
        let cs = curve_set(get(&ac.kid("pdm:ShapeElement").text));
        let height = fabout::gerber::mm(b.height.0);
        assert_eq!(bound(cs, "d2:UpperBound"), Some(height.clone()));
        assert_eq!(prop(single, "HEIGHT"), Some(height));
        let rect = get(&cs.kid("d2:DetailedGeometricModelElement").text);
        let pts: Vec<&El> = rect.kids("d2:Point").map(|p| get(&p.text)).collect();
        assert_eq!(pts.len(), 5);
        let xs: BTreeSet<&str> = pts.iter().map(|p| p.kid("d2:X").kid("property:Value").text.as_str()).collect();
        let w = |s: &str| s.parse::<f64>().unwrap();
        let span = xs.iter().map(|s| w(s)).fold(f64::NEG_INFINITY, f64::max)
            - xs.iter().map(|s| w(s)).fold(f64::INFINITY, f64::min);
        assert!((span - b.width.0 as f64 / 1e6).abs() < 1e-9);
        let t = instance(o).kid("pdm:Transformation");
        assert_eq!(t.kid("pdm:TransformationType").text, "d3");
        let v = |k: &str| t.kid(k).text.clone();
        let tz = t.kid("pdm:tz").kid("property:Value").text.clone();
        if refdes == "C2" {
            // Bottom, 90°: turned over (X → -X, Z → -Z), then rotated.
            assert_eq!(prop(instance(o), "SIDE").as_deref(), Some("BOTTOM"));
            assert_eq!([v("pdm:xx"), v("pdm:xy"), v("pdm:yx"), v("pdm:yy"), v("pdm:zz")], ["0", "-1", "-1", "0", "-1"]);
            assert_eq!(tz, "0");
        } else {
            assert_eq!(v("pdm:zz"), "1");
            assert_eq!(tz, "1.6");
        }
    }
}

/// Optional schema validation: the IDX (EDMD) schema is published free of charge by the
/// prostep ivip Association (PSI 5 download, `PSI5_IDXv4.5_Schema.zip`) and not redistributed
/// here; point `CADLAB_IDX_XSD` at the directory holding its `foundation.xsd`.
#[test]
fn idx_schema_oracle() {
    let Some(dir) = std::env::var_os("CADLAB_IDX_XSD") else {
        eprintln!("skipping: set CADLAB_IDX_XSD to the directory of the IDX schema files");
        return;
    };
    let Some(xmllint) = oracle::optional(Oracle::Xmllint) else { return };
    let dir = std::path::PathBuf::from(dir);
    let (d, _r, s) = idx_board();
    let out = mcad::idx::export(s.project.as_ref().unwrap(), &mcad_options()).unwrap();
    let path = d.path().join("tiny.idx");
    std::fs::write(&path, &out.content).unwrap();
    // The process instruction types live in a schema `foundation.xsd` does not import: a
    // wrapper imports both.
    let ns = "http://www.prostep.org/ecad-mcad/edmd/4.0/";
    let mut wrapper = String::from(
        "<?xml version=\"1.0\"?>\n<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
         targetNamespace=\"urn:cadlab:idx-test\">\n",
    );
    for (n, file) in [("foundation", "foundation.xsd"), ("computational", "computational.xsd")] {
        let loc = dir.join(file);
        assert!(loc.is_file(), "{} missing", loc.display());
        let p = loc.to_string_lossy().replace('\\', "/");
        let url = if p.starts_with('/') { format!("file://{p}") } else { format!("file:///{p}") };
        wrapper.push_str(&format!("  <xs:import namespace=\"{ns}{n}\" schemaLocation=\"{url}\"/>\n"));
    }
    wrapper.push_str("</xs:schema>\n");
    let xsd = d.path().join("idx-wrapper.xsd");
    std::fs::write(&xsd, wrapper).unwrap();
    oracle::run(&xmllint, &["--noout", "--schema", &xsd.to_string_lossy(), &path.to_string_lossy()]);
}

#[test]
fn export_commands() {
    let (dir, r, mut s) = board();
    let root = dir.path().join("p");
    let o = exec(&r, &mut s, "export.ipc2581", json!({}));
    let path = o["output"]["files"][0]["path"].as_str().unwrap();
    assert!(Path::new(path).ends_with(Path::new("out").join("fab").join("tiny-ipc2581.xml")));
    assert!(Path::new(path).is_file());

    let o = exec(&r, &mut s, "export.step", json!({}));
    assert_eq!(o["output"]["bodies"], 3);
    assert_eq!(o["output"]["holes"], 3);
    assert!(root.join("out").join("mcad").join("tiny.step").is_file());
    let o = exec(&r, &mut s, "export.step", json!({"path": "m/board", "vias": true, "components": false}));
    assert_eq!((o["output"]["bodies"].as_u64(), o["output"]["holes"].as_u64()), (Some(0), Some(5)));
    assert!(root.join("m").join("board.step").is_file());

    let o = exec(&r, &mut s, "export.idf", json!({}));
    let files = o["output"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert!(root.join("out").join("mcad").join("tiny.emn").is_file());
    assert!(root.join("out").join("mcad").join("tiny.emp").is_file());
    assert_eq!(o["output"]["components"], 3);

    let o = exec(&r, &mut s, "export.idx", json!({}));
    assert_eq!((o["output"]["components"].as_u64(), o["output"]["holes"].as_u64()), (Some(3), Some(3)));
    assert_eq!(o["output"]["keepouts"], 0);
    let path = o["output"]["path"].as_str().unwrap();
    assert!(Path::new(path).ends_with(Path::new("out").join("mcad").join("tiny.idx")));
    assert!(root.join("out").join("mcad").join("tiny.idx").is_file());
    let o = exec(&r, &mut s, "export.idx", json!({"path": "m/collab", "vias": true, "components": false}));
    assert_eq!((o["output"]["components"].as_u64(), o["output"]["holes"].as_u64()), (Some(0), Some(5)));
    let text = std::fs::read_to_string(root.join("m").join("collab.idx")).unwrap();
    assert_eq!(text.matches("GeometryType=\"VIA\"").count(), 2);

    // export.all includes the IPC-2581 file.
    let o = exec(&r, &mut s, "export.all", json!({"dir": "all"}));
    assert!(o["output"]["files"].as_array().unwrap().iter().any(|f| f["function"] == "IPC-2581C"));

    // A component without a package body is reported, not modelled.
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "TP", "category": "connector", "package": "PinHeader 1x01",
        "pins": [{"number": "1", "name": "TP", "kind": "passive"}]}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "TP", "refdes": "TP1"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "TP1", "at": ["14mm", "3mm"]}));
    let fp = cadlab::board::footprint_for(s.project.as_ref().unwrap(), "TP1").unwrap().name.clone();
    s.project.as_mut().unwrap().library_mut().footprints.get_mut(&fp).unwrap().body = None;
    let v = serde_json::to_value(r.execute(&mut s, "export.step", json!({}), RunOptions::default()).unwrap()).unwrap();
    assert!(v.to_string().contains("export.no_body"), "{v}");

    // No outline: a conflict with a hint.
    s.project.as_mut().unwrap().board_mut().outline.contours.clear();
    for cmd in ["export.step", "export.idf", "export.idx"] {
        let e = r.execute(&mut s, cmd, json!({}), RunOptions::default()).unwrap_err();
        assert_eq!(e.error.diagnostic.code, "board.no_outline", "{cmd}");
        assert!(e.error.diagnostic.hint.is_some());
    }
}
