//! Shared by the fab output tests: a tiny board and structural checkers for Gerber, XNC and
//! IPC-D-356A files (independent of the writers: they parse the text back).

#![allow(dead_code, missing_docs)]

use std::collections::{BTreeMap, BTreeSet};

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::geom::Point;
use cadlab::id::ObjectId;
use cadlab::model::board::{BoardGraphic, GraphicKind};
use cadlab::units::{Angle, Nm};
use serde_json::{Value, json};

pub fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn mm(v: f64) -> String {
    format!("{v}mm")
}

/// LDO + two caps (as in tests/board.rs) plus a 2-pin through-hole header: a 25×15 mm board
/// with rounded corners, a part at 45°, a bottom-side part, tracks (one an arc), two vias and a
/// silkscreen text. C1 is DNP.
pub fn tiny_board() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p"), "name": "tiny"}));
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "manufacturer": "Diodes", "package": "SOT-23-5",
        "pins": [{"number": "1", "name": "VIN", "kind": "power_in"}, {"number": "2", "name": "GND", "kind": "power_in"},
                 {"number": "3", "name": "EN", "kind": "input"}, {"number": "4", "name": "NC", "kind": "no_connect"},
                 {"number": "5", "name": "VOUT", "kind": "power_out"}]}),
    );
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "PWR_HDR", "category": "connector", "description": "2-pin power header",
        "package": "PinHeader 1x02", "pins": [{"number": "1", "name": "VIN", "kind": "passive"}, {"number": "2", "name": "GND", "kind": "passive"}]}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "AP2112K-3.3TRG1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 2}));
    exec(&r, &mut s, "circuit.add", json!({"part": "PWR_HDR", "refdes": "J1"}));
    exec(&r, &mut s, "net.connect", json!({"net": "VIN", "pins": ["U1.VIN", "U1.EN", "C1.1", "J1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2", "J1.2"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "25mm", "height": "15mm", "corner_radius": "1mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": [mm(12.0), mm(7.5)]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": [mm(7.0), mm(10.5)], "rotation": 45}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": [mm(18.0), mm(9.0)], "rotation": 90, "side": "bottom"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": [mm(3.0), mm(6.0)], "rotation": 90}));
    exec(&r, &mut s, "bom.dnp", json!({"refdes": ["C1"]}));
    // 3V3: U1.VOUT on top to a via, then bottom to C2.1.
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": ["U1.VOUT", [mm(15.5), mm(9.0)]]}));
    exec(&r, &mut s, "via.add", json!({"at": [mm(15.5), mm(9.0)], "net": "3V3"}));
    exec(&r, &mut s, "track.add", json!({"layer": "B.Cu", "points": [[mm(15.5), mm(9.0)], "C2.1"], "net": "3V3"}));
    // GND: a via near the header and an arc track on the bottom.
    exec(&r, &mut s, "via.add", json!({"at": [mm(6.0), mm(3.0)], "net": "GND"}));
    let o = exec(
        &r,
        &mut s,
        "track.add",
        json!({"layer": "B.Cu", "points": [[mm(6.0), mm(3.0)], [mm(10.0), mm(3.0)]], "net": "GND"}),
    );
    let id = o["output"]["tracks"][0]["id"].as_u64().unwrap();
    let p = s.project.as_mut().unwrap();
    let b = p.board_mut();
    let t = b.tracks.iter_mut().find(|t| t.id.0 == id).unwrap();
    t.mid = Some(Point::new(Nm::from_mm(8), Nm::from_mm(4)));
    b.graphics.push(BoardGraphic {
        id: ObjectId(9000),
        layer: "F.SilkS".into(),
        kind: GraphicKind::Text {
            text: "cadlab".into(),
            at: Point::new(Nm(20_000_000), Nm(3_000_000)),
            size: Nm(1_500_000),
            rotation: Angle::ZERO,
        },
    });
    // A silkscreen line across U1's pins 1 and 5: clipped at the mask openings.
    b.graphics.push(BoardGraphic {
        id: ObjectId(9001),
        layer: "F.SilkS".into(),
        kind: GraphicKind::Line {
            points: vec![Point::new(Nm(9_000_000), Nm(8_450_000)), Point::new(Nm(15_000_000), Nm(8_450_000))],
            width: Nm(150_000),
        },
    });
    (dir, r, s)
}

/// A flash: position (nm), D-code and the object attributes in effect.
pub type Flash = ((i64, i64), u32, BTreeMap<String, String>);

/// What a structural Gerber check found.
#[derive(Debug, Default)]
pub struct GerberInfo {
    /// File attributes (`.FileFunction` → value).
    pub file_attrs: BTreeMap<String, String>,
    /// D-code → (template, .AperFunction).
    pub apertures: BTreeMap<u32, (String, Option<String>)>,
    /// Flash count per D-code.
    pub flashes: BTreeMap<u32, usize>,
    /// Flash positions (nm) with the object attributes in effect.
    pub flash_list: Vec<Flash>,
    /// Number of draws/arcs.
    pub draws: usize,
    /// Number of arcs.
    pub arcs: usize,
    /// Number of regions.
    pub regions: usize,
    /// Object attribute names ever used.
    pub object_attrs: BTreeSet<String>,
}

/// Parses a Gerber file and checks its structure: header attributes before graphics, format
/// 4.6 mm, macros and apertures defined before use, balanced and closed regions with no
/// flashes inside, G75 before arcs, explicit operation codes, M02 at the end.
pub fn check_gerber(name: &str, text: &str) -> GerberInfo {
    let mut info = GerberInfo::default();
    let fail = |msg: String| -> ! { panic!("{name}: {msg}") };
    // Split into commands: extended `%...%` blocks and word commands ending with `*`.
    let mut cmds: Vec<(bool, String)> = Vec::new();
    let mut rest = text;
    while !rest.trim_start().is_empty() {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix('%') {
            let end = r.find('%').unwrap_or_else(|| fail("unterminated extended command".into()));
            cmds.push((true, r[..end].to_string()));
            rest = &r[end + 1..];
        } else {
            let end = rest.find('*').unwrap_or_else(|| fail(format!("word without `*`: {rest:.40}")));
            cmds.push((false, rest[..end].trim().to_string()));
            rest = &rest[end + 1..];
        }
    }
    let (mut fs, mut mo, mut g75, mut in_region, mut ended) = (false, false, false, false, false);
    let mut macros = BTreeSet::new();
    let mut ta: Option<String> = None;
    let mut cur: Option<u32> = None;
    let mut mode = "G01";
    let mut pos: Option<(i64, i64)> = None;
    let mut contour_start: Option<(i64, i64)> = None;
    let mut obj: BTreeMap<String, String> = BTreeMap::new();
    let mut graphics_started = false;
    let coord = |s: &str, k: char| -> Option<i64> {
        let i = s.find(k)?;
        let t: String = s[i + 1..].chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
        if t.trim_start_matches('-').len() > 10 {
            fail(format!("coordinate exceeds format 4.6: {s}"));
        }
        Some(t.parse().unwrap_or_else(|_| fail(format!("bad coordinate in {s}"))))
    };
    let close_contour = |pos: Option<(i64, i64)>, start: Option<(i64, i64)>| {
        if let (Some(p), Some(s)) = (pos, start)
            && p != s
        {
            fail(format!("region contour not closed: ends at {p:?}, started at {s:?}"));
        }
    };
    for (ext, c) in cmds {
        if ended {
            fail("data after M02".into());
        }
        if ext {
            for part in c.split('*').map(str::trim).filter(|s| !s.is_empty()) {
                if part == "FSLAX46Y46" {
                    fs = true;
                } else if part.starts_with("FS") {
                    fail(format!("unexpected format {part}"));
                } else if part == "MOMM" {
                    mo = true;
                } else if let Some(a) = part.strip_prefix("TF") {
                    if graphics_started {
                        fail(format!("file attribute after graphics: {part}"));
                    }
                    let (k, v) = a.split_once(',').unwrap_or((a, ""));
                    info.file_attrs.insert(k.to_string(), v.to_string());
                } else if let Some(a) = part.strip_prefix("TA.AperFunction,") {
                    ta = Some(a.to_string());
                } else if part == "TD.AperFunction" {
                    ta = None;
                } else if part == "TD" {
                    ta = None;
                    obj.clear();
                } else if let Some(a) = part.strip_prefix("TO") {
                    let (k, v) = a.split_once(',').unwrap_or((a, ""));
                    info.object_attrs.insert(k.to_string());
                    obj.insert(k.to_string(), v.to_string());
                } else if let Some(k) = part.strip_prefix("TD") {
                    obj.remove(k);
                } else if let Some(m) = part.strip_prefix("AM") {
                    macros.insert(m.to_string());
                    break; // the rest are primitives
                } else if let Some(d) = part.strip_prefix("ADD") {
                    let n: String = d.chars().take_while(|c| c.is_ascii_digit()).collect();
                    let tpl = &d[n.len()..];
                    let num: u32 = n.parse().unwrap();
                    assert!(num >= 10, "{name}: aperture number {num} < 10");
                    let shape = tpl.split(',').next().unwrap();
                    if !["C", "R", "O", "P"].contains(&shape) && !macros.contains(shape) {
                        fail(format!("aperture D{num} uses undefined macro {shape}"));
                    }
                    if info.apertures.insert(num, (tpl.to_string(), ta.clone())).is_some() {
                        fail(format!("D{num} defined twice"));
                    }
                } else if part == "LPD" {
                } else {
                    fail(format!("unexpected extended command {part}"));
                }
            }
            continue;
        }
        if c.starts_with("G04") {
            continue;
        }
        match c.as_str() {
            "G01" | "G02" | "G03" => {
                mode = if c == "G01" { "G01" } else { "arc" };
                continue;
            }
            "G75" => {
                g75 = true;
                continue;
            }
            "G36" => {
                assert!(!in_region, "{name}: nested G36");
                in_region = true;
                contour_start = None;
                info.regions += 1;
                continue;
            }
            "G37" => {
                assert!(in_region, "{name}: G37 without G36");
                close_contour(pos, contour_start);
                in_region = false;
                continue;
            }
            "M02" => {
                ended = true;
                continue;
            }
            _ => {}
        }
        if let Some(d) = c.strip_prefix('D')
            && d.chars().all(|ch| ch.is_ascii_digit())
        {
            let n: u32 = d.parse().unwrap();
            if !info.apertures.contains_key(&n) {
                fail(format!("D{n} selected before definition"));
            }
            cur = Some(n);
            continue;
        }
        // Operations.
        assert!(fs && mo, "{name}: coordinates before FS/MO");
        for k in [".FileFunction", ".FilePolarity", ".GenerationSoftware", ".Part"] {
            assert!(info.file_attrs.contains_key(k), "{name}: missing %TF{k}");
        }
        graphics_started = true;
        let op = &c[c.len() - 3..];
        let p = (
            coord(&c, 'X').unwrap_or_else(|| fail(format!("no X in {c}"))),
            coord(&c, 'Y').unwrap_or_else(|| fail(format!("no Y in {c}"))),
        );
        match op {
            "D02" => {
                if in_region {
                    close_contour(pos, contour_start);
                    contour_start = Some(p);
                }
            }
            "D01" => {
                assert!(pos.is_some(), "{name}: D01 without current point");
                if mode == "arc" {
                    assert!(g75, "{name}: arc before G75");
                    assert!(c.contains('I') && c.contains('J'), "{name}: arc without I/J: {c}");
                    info.arcs += 1;
                }
                if !in_region {
                    assert!(cur.is_some(), "{name}: draw without aperture");
                    info.draws += 1;
                }
            }
            "D03" => {
                assert!(!in_region, "{name}: flash inside a region");
                let d = cur.unwrap_or_else(|| fail("flash without aperture".into()));
                *info.flashes.entry(d).or_default() += 1;
                info.flash_list.push((p, d, obj.clone()));
            }
            _ => fail(format!("missing operation code: {c}")),
        }
        pos = Some(p);
    }
    assert!(ended, "{name}: no M02");
    assert!(!in_region, "{name}: unterminated region");
    info
}

/// Checks an XNC drill file: header with METRIC and the tool table, `%`, G05, tools selected
/// after declaration, decimal coordinates, M30 last; no spaces outside comments. Returns
/// (tool → diameter mm, hits per tool, attribute comments).
pub fn check_xnc(name: &str, text: &str) -> (BTreeMap<String, f64>, BTreeMap<String, usize>, Vec<String>) {
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.first(), Some(&"M48"), "{name}: must start with M48");
    assert_eq!(lines.last(), Some(&"M30"), "{name}: must end with M30");
    let mut tools = BTreeMap::new();
    let mut hits: BTreeMap<String, usize> = BTreeMap::new();
    let mut attrs = Vec::new();
    let (mut header, mut metric, mut cur) = (true, false, None::<String>);
    for l in &lines[1..lines.len() - 1] {
        if let Some(c) = l.strip_prefix(';') {
            if let Some(a) = c.strip_prefix(" #@! ") {
                attrs.push(a.to_string());
            }
            continue;
        }
        assert!(!l.contains(' '), "{name}: space outside comment: {l}");
        if header {
            match *l {
                "METRIC" => metric = true,
                "%" => header = false,
                t if t.starts_with('T') => {
                    assert!(metric, "{name}: tool before unit");
                    let (num, d) = t[1..].split_once('C').unwrap();
                    assert_eq!(num.len(), 2, "{name}: tool number must have 2 digits");
                    tools.insert(format!("T{num}"), d.parse::<f64>().unwrap());
                }
                _ => panic!("{name}: unexpected header line {l}"),
            }
            continue;
        }
        if *l == "G05" {
            continue;
        }
        if l.starts_with('T') {
            assert!(tools.contains_key(*l), "{name}: undeclared tool {l}");
            cur = Some(l.to_string());
            continue;
        }
        let (x, y) =
            l.strip_prefix('X').and_then(|r| r.split_once('Y')).unwrap_or_else(|| panic!("{name}: bad line {l}"));
        assert!(x.contains('.') && y.contains('.'), "{name}: coordinates need a decimal point: {l}");
        x.parse::<f64>().unwrap();
        y.parse::<f64>().unwrap();
        *hits.entry(cur.clone().expect("hit before tool")).or_default() += 1;
    }
    assert!(!header, "{name}: no end of header");
    (tools, hits, attrs)
}

/// Checks IPC-D-356A records: ≤ 80 columns, known record types, `999` last. Returns the test
/// records (op, net, refdes, pin, access).
pub fn check_ipc356(text: &str) -> Vec<(String, String, String, String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.last(), Some(&"999"));
    let mut out = Vec::new();
    for l in &lines[..lines.len() - 1] {
        assert!(l.len() <= 80 && l.is_ascii(), "record too long: {l}");
        match &l[..1] {
            "C" | "P" => continue,
            "3" => {
                let col = |a: usize, b: usize| l.get(a - 1..b.min(l.len())).unwrap_or("").trim().to_string();
                assert!(["317", "327", "367"].contains(&&l[..3]), "unknown record {l}");
                assert_eq!(&l[26..27], "-", "{l}");
                assert_eq!(&l[41..42], "X", "{l}");
                assert_eq!(&l[49..50], "Y", "{l}");
                assert_eq!(&l[72..73], "S", "{l}");
                out.push((col(1, 3), col(4, 17), col(21, 26), col(28, 31), col(39, 41)));
            }
            _ => panic!("unknown record {l}"),
        }
    }
    out
}
