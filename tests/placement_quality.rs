//! Quality of spread automatic placement (DECISIONS D67): how `place.auto` uses a board larger
//! than its parts, measured by `cadlab::board::place::placement_stats` (empty windows,
//! unevenness, largest empty square), with the ratsnest length, decoupling distances and
//! connector edge distances as the costs that must not grow out of bounds.
//!
//! `placement_quality_report` (ignored; `cargo test --release --test placement_quality --
//! --ignored --nocapture`) prints the table for every board, and routes them with
//! `CADLAB_PLACE_ROUTE=<budget ms>` (`CADLAB_PLACE_VERBOSE` lists what stays unrouted);
//! `CADLAB_PLACE_ONLY=<name>` picks boards, `CADLAB_PLACEMENT_RENDER=<dir>` renders each placement.

#[path = "common/bigboard.rs"]
mod bigboard;
mod common;

use cadlab::board::place::{PlacementStats, placement_stats};
use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::Project;
use cadlab::{Nm, Point};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn project(s: &Session) -> &Project {
    s.project.as_ref().unwrap()
}

fn new_project() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    (dir, r, s)
}

fn outline(r: &Registry, s: &mut Session, w: f64, h: f64) {
    exec(r, s, "board.outline", json!({"width": format!("{w}mm"), "height": format!("{h}mm")}));
}

/// The ATtiny85 board of the schematic tests.
fn attiny(w: f64, h: f64) -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = new_project();
    common::boards::build_board(&r, &mut s);
    outline(&r, &mut s, w, h);
    (d, r, s)
}

/// The STM32 board (USB-C, LDO, crystal, headers, LEDs).
fn stm32(w: f64, h: f64) -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = new_project();
    common::boards::build_stm32_board(&r, &mut s);
    outline(&r, &mut s, w, h);
    (d, r, s)
}

/// Four dual op-amp channels, each with its own input connector, feedback network and
/// decoupling, and a power header: several groups of similar size and no single center.
fn channels(w: f64, h: f64) -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = new_project();
    let opa = json!([
        {"number": "1", "name": "OUTA", "kind": "output"}, {"number": "2", "name": "INA-", "kind": "input"},
        {"number": "3", "name": "INA+", "kind": "input"}, {"number": "4", "name": "V-", "kind": "power_in"},
        {"number": "5", "name": "INB+", "kind": "input"}, {"number": "6", "name": "INB-", "kind": "input"},
        {"number": "7", "name": "OUTB", "kind": "output"}, {"number": "8", "name": "V+", "kind": "power_in"}
    ]);
    exec(&r, &mut s, "part.create", json!({"id": "OPA", "category": "ic", "package": "SOIC-8", "pins": opa}));
    let hdr = json!([{"number":"1","name":"A"},{"number":"2","name":"B"},{"number":"3","name":"C"},{"number":"4","name":"D"}]);
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "HDR4", "category": "connector", "package": "PinHeader 1x04", "pins": hdr}),
    );
    let pwr = json!([{"number":"1","name":"VCC"},{"number":"2","name":"GND"}]);
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "PWR", "category": "connector", "package": "PinHeader 1x02", "pins": pwr}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "PWR", "refdes": "J9"}));
    let mut gnd = vec!["J9.GND".to_string()];
    let mut vcc = vec!["J9.VCC".to_string()];
    for k in 1..=4 {
        let (u, j) = (format!("U{k}"), format!("J{k}"));
        exec(&r, &mut s, "circuit.add", json!({"part": "OPA", "refdes": u}));
        exec(&r, &mut s, "circuit.add", json!({"part": "HDR4", "refdes": j}));
        let rr: Vec<String> = (1..=4).map(|i| format!("R{}", k * 10 + i)).collect();
        for x in &rr {
            exec(&r, &mut s, "circuit.add", json!({"part": "R 10k 1% 0402", "refdes": x}));
        }
        let c = format!("C{k}");
        exec(&r, &mut s, "circuit.add", json!({"part": "C 100nF 16V X7R 0402", "refdes": c}));
        let nets = [
            (format!("IN{k}A"), vec![format!("{j}.A"), format!("{}.1", rr[0])]),
            (format!("FB{k}A"), vec![format!("{u}.INA-"), format!("{}.2", rr[0]), format!("{}.1", rr[1])]),
            (format!("OUT{k}A"), vec![format!("{u}.OUTA"), format!("{}.2", rr[1]), format!("{j}.C")]),
            (format!("IN{k}B"), vec![format!("{j}.B"), format!("{}.1", rr[2])]),
            (format!("FB{k}B"), vec![format!("{u}.INB-"), format!("{}.2", rr[2]), format!("{}.1", rr[3])]),
            (format!("OUT{k}B"), vec![format!("{u}.OUTB"), format!("{}.2", rr[3]), format!("{j}.D")]),
        ];
        for (n, pins) in nets {
            exec(&r, &mut s, "net.connect", json!({"net": n, "pins": pins}));
        }
        gnd.extend([format!("{u}.V-"), format!("{u}.INA+"), format!("{u}.INB+"), format!("{c}.2")]);
        vcc.extend([format!("{u}.V+"), format!("{c}.1")]);
    }
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": gnd}));
    exec(&r, &mut s, "net.connect", json!({"net": "VCC", "pins": vcc}));
    outline(&r, &mut s, w, h);
    (d, r, s)
}

type Build = fn(f64, f64) -> (tempfile::TempDir, Registry, Session);

/// The boards measured: (name, builder, width, height, roomy).
fn boards() -> Vec<(&'static str, Build, f64, f64, bool)> {
    vec![
        ("attiny 40x30", attiny as Build, 40.0, 30.0, false),
        ("attiny 80x60", attiny, 80.0, 60.0, true),
        ("stm32 60x45", stm32, 60.0, 45.0, false),
        ("stm32 100x75", stm32, 100.0, 75.0, true),
        ("channels 60x45", channels, 60.0, 45.0, false),
        ("channels 100x80", channels, 100.0, 80.0, true),
    ]
}

fn mm(n: Nm) -> f64 {
    n.0 as f64 / 1e6
}

fn dist_mm(a: Point, b: Point) -> f64 {
    ((a.x.0 - b.x.0) as f64).hypot((a.y.0 - b.y.0) as f64) / 1e6
}

/// Placement costs that spreading must keep: the ratsnest, the farthest decoupling capacitor
/// (from its pad on the rail to the nearest IC pad of that rail) and the farthest connector
/// (courtyard to the board's box).
struct Costs {
    ratsnest: f64,
    decap: f64,
    connector: f64,
    /// Long connectors lying across their nearest edge rather than along it.
    across: Vec<String>,
}

fn costs(p: &Project, w: f64, h: f64) -> Costs {
    let ratsnest = mm(cadlab::board::place::ratsnest_length(p));
    let pads = cadlab::board::placed_pads(p);
    let lib = p.library();
    let c = p.circuit();
    let category = |r: &str| c.components.get(r).and_then(|x| lib.parts.get(&x.part)).map(|x| x.category);
    let pins = |r: &str| c.components.get(r).and_then(|x| lib.parts.get(&x.part)).map_or(0, |x| x.symbol.pins.len());
    let mut decap: f64 = 0.0;
    for pp in pads.iter().filter(|pp| pp.refdes.starts_with('C')) {
        let Some(net) = pp.net.as_deref() else { continue };
        if net == "GND" || !pads.iter().any(|q| q.refdes == pp.refdes && q.net.as_deref() == Some("GND")) {
            continue;
        }
        let near = pads
            .iter()
            .filter(|q| q.net.as_deref() == Some(net) && pins(&q.refdes) > 2)
            .filter(|q| category(&q.refdes) != Some(cadlab::model::part::Category::Connector))
            .map(|q| dist_mm(q.center, pp.center))
            .fold(f64::MAX, f64::min);
        if near < f64::MAX {
            decap = decap.max(near);
        }
    }
    let bx = cadlab::board::place::courtyard_box;
    let all: Vec<_> = p.board().footprints.keys().filter_map(|r| bx(p, r).map(|b| (r.clone(), b))).collect();
    let mut connector: f64 = 0.0;
    let mut across = Vec::new();
    for (r, b) in &all {
        if category(r) == Some(cadlab::model::part::Category::Connector) {
            let (x0, y0, x1, y1) = (b.x0 as f64 / 1e6, b.y0 as f64 / 1e6, b.x1 as f64 / 1e6, b.y1 as f64 / 1e6);
            connector = connector.max(x0.min(w - x1).min(y0).min(h - y1));
            // Long (at least twice as long as wide) and with an end, not a side, to its edge.
            let (dw, dh) = (x1 - x0, y1 - y0);
            let to_side = x0.min(w - x1);
            let to_end = y0.min(h - y1);
            if (dh >= 2.0 * dw && to_end < to_side) || (dw >= 2.0 * dh && to_side < to_end) {
                across.push(r.clone());
            }
        }
    }
    Costs { ratsnest, decap, connector, across }
}

fn place(r: &Registry, s: &mut Session, args: Value, w: f64, h: f64) -> (PlacementStats, Costs) {
    let o = exec(r, s, "place.auto", args);
    assert!(o["output"].get("unplaced").is_none(), "{o}");
    let p = project(s);
    (placement_stats(p).unwrap(), costs(p, w, h))
}

fn row(name: &str, st: &PlacementStats, c: &Costs) -> String {
    format!(
        "| {name} | {}/{} | {:.2} | {:.2} | {:.1} | {:.2} | {:.1} | {:.2} | {:.2} | {} |",
        st.empty_windows,
        st.windows,
        st.crowding,
        st.unevenness,
        mm(st.largest_empty),
        st.extent,
        c.ratsnest,
        c.decap,
        c.connector,
        c.across.len()
    )
}

/// Prints the stats of spread and compact placement on every board (and routes them with
/// `CADLAB_PLACE_ROUTE=1`).
#[test]
#[ignore]
fn placement_quality_report() {
    let route = std::env::var_os("CADLAB_PLACE_ROUTE").is_some();
    let budget: u64 = std::env::var("CADLAB_PLACE_ROUTE").ok().and_then(|v| v.parse().ok()).unwrap_or(30_000);
    println!(
        "| board | empty windows | crowding | unevenness | largest empty mm | extent | ratsnest mm | decap mm | connector mm | across |{}",
        if route { " routed | vias | track mm |" } else { "" }
    );
    let only = std::env::var("CADLAB_PLACE_ONLY").unwrap_or_default();
    for (name, build, w, h, _) in boards().into_iter().filter(|b| b.0.contains(only.as_str())) {
        for compact in [false, true] {
            let (_d, r, mut s) = build(w, h);
            let t = std::time::Instant::now();
            let (st, c) = place(&r, &mut s, if compact { json!({"compact": true}) } else { json!({}) }, w, h);
            let ms = t.elapsed().as_millis();
            let label = format!("{name}{} ({ms} ms)", if compact { " compact" } else { "" });
            let mut line = row(&label, &st, &c);
            if let Ok(dir) = std::env::var("CADLAB_PLACEMENT_RENDER") {
                let file = format!("{}{}.png", name.replace(' ', "_"), if compact { "_compact" } else { "" });
                let path = std::path::Path::new(&dir).join(file);
                exec(
                    &r,
                    &mut s,
                    "render.board",
                    json!({"path": path, "layers": ["F.Cu", "F.SilkS", "F.CrtYd", "Edge.Cuts"]}),
                );
            }
            if route {
                let o = exec(&r, &mut s, "route.all", json!({"budget_ms": budget, "seed": 1}));
                if std::env::var_os("CADLAB_PLACE_VERBOSE").is_some() {
                    for c in o["output"]["connections"].as_array().unwrap().iter().filter(|c| c["status"] != "routed") {
                        println!("  unrouted {} {} -> {}: {}", c["net"], c["from"], c["to"], c["reason"]);
                    }
                }
                let st = exec(&r, &mut s, "route.status", json!({}));
                let p = project(&s);
                let len: f64 = p.board().tracks.iter().map(|t| cadlab::board::track_length(t).0 as f64).sum();
                line += &format!(
                    " {}/{} | {} | {:.0} |",
                    st["output"]["connections"].as_u64().unwrap() - st["output"]["unrouted"].as_u64().unwrap(),
                    st["output"]["connections"],
                    p.board().vias.len(),
                    len / 1e6
                );
            }
            println!("{line}");
        }
    }
}

/// Courtyard center of a placed footprint.
fn center(p: &Project, r: &str) -> Point {
    cadlab::board::place::courtyard_box(p, r).unwrap().center()
}

/// DRC findings about placement (overlaps, outside the board, edges, silkscreen on pads).
fn placement_errors(p: &Project) -> Vec<String> {
    const CODES: &[&str] =
        &["drc.courtyard_overlap", "drc.footprint_outside", "drc.copper_to_edge", "drc.silk_over_pad", "drc.keepout"];
    cadlab::drc::check(p).into_iter().filter(|d| CODES.contains(&d.code.as_ref())).map(|d| d.message).collect()
}

/// A spread placement (its project) with its stats and costs, and those of the compact one.
type Compared = ((tempfile::TempDir, Session), PlacementStats, Costs, PlacementStats, Costs);

/// Spread placement on `build` (w × h), free of placement errors and the same when run again,
/// and the compact placement of the same board.
fn spread_and_compact(build: Build, w: f64, h: f64) -> Compared {
    let (d, r, mut s) = build(w, h);
    let (st, c) = place(&r, &mut s, json!({}), w, h);
    let errors = placement_errors(project(&s));
    assert!(errors.is_empty(), "{errors:#?}");
    let (_d2, r2, mut s2) = build(w, h);
    place(&r2, &mut s2, json!({}), w, h);
    assert_eq!(project(&s2).board().footprints, project(&s).board().footprints, "deterministic");
    let (_d3, r3, mut s3) = build(w, h);
    let (cst, cc) = place(&r3, &mut s3, json!({"compact": true}), w, h);
    ((d, s), st, c, cst, cc)
}

/// Four channels on a roomy board: each op-amp group goes to its own connector (before D67 the
/// largest IC was pinned at the center and the other groups clumped around it, their
/// connectors' nets crossing the board), with less ratsnest than even the packed placement.
#[test]
fn roomy_board_groups_go_to_their_connectors() {
    let ((_d, s), _st, c, _cst, cc) = spread_and_compact(channels, 100.0, 80.0);
    let p = project(&s);
    for k in 1..=4 {
        let d = dist_mm(center(p, &format!("U{k}")), center(p, &format!("J{k}")));
        assert!(d < 15.0, "U{k} {d:.1} mm from J{k}");
    }
    assert!(c.ratsnest < cc.ratsnest, "spread {:.1} mm vs compact {:.1} mm", c.ratsnest, cc.ratsnest);
    assert!(c.decap < 2.5, "decoupling {:.2} mm from its IC", c.decap);
    assert!(c.connector < 1.0, "a connector {:.2} mm from the edge", c.connector);
}

/// The STM32 board on an outline much larger than its parts: groups spread (less crowding and
/// fewer empty windows than packed), connectors at the edges, with less ratsnest than D55's
/// placement (1558 mm).
#[test]
fn roomy_board_spreads_without_clumping() {
    let (_s, st, c, cst, cc) = spread_and_compact(stm32, 100.0, 75.0);
    assert!(st.crowding < 0.5 * cst.crowding, "crowding {st:?} vs compact {cst:?}");
    assert!(st.empty_windows <= 5 && st.empty_windows < cst.empty_windows, "{st:?} vs compact {cst:?}");
    assert!(st.extent > 0.9, "{st:?}");
    assert!(c.ratsnest < 1500.0, "ratsnest {:.1} mm (compact {:.1} mm)", c.ratsnest, cc.ratsnest);
    assert!(c.connector < 1.5, "a connector {:.2} mm from the edge", c.connector);
}

/// The ATtiny board on a sparse outline: D55's group moves were all undone there (their
/// ratsnest overshot the budget); now the regulator sits between the power connector and the
/// MCU, and the parts spread wider and more evenly than packed.
#[test]
fn sparse_board_keeps_its_group_moves() {
    let ((_d, s), st, _c, cst, _cc) = spread_and_compact(attiny, 80.0, 60.0);
    let p = project(&s);
    let (j1, u1, u2) = (center(p, "J1"), center(p, "U1"), center(p, "U2"));
    let detour = dist_mm(j1, u1) + dist_mm(u1, u2) - dist_mm(j1, u2);
    assert!(detour < 6.0, "U1 off the way from J1 to U2 by {detour:.1} mm");
    assert!(st.unevenness < cst.unevenness && st.extent > 2.0 * cst.extent, "{st:?} vs compact {cst:?}");
}

/// Parts and circuit of a USB front: receptacle (a seven-pin header: one contact per net, so its
/// polarity is fixed), a flow-through ESD array, an MCU with PA11 and PA12 side by side, and
/// the pair declared.
fn usb_front(w: f64, h: f64) -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = new_project();
    let pins = |list: &[(&str, &str)]| -> Value {
        Value::Array(list.iter().map(|(n, m)| json!({"number": n, "name": m})).collect())
    };
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "USB", "category": "connector", "package": "PinHeader 1x07",
            "pins": pins(&[("1", "VBUS"), ("2", "CC1"), ("3", "DM"), ("4", "DP"), ("5", "CC2"), ("6", "GND"), ("7", "SHIELD")])}),
    );
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "ESD2", "category": "ic", "package": "SOT-23-6",
            "pins": pins(&[("1", "IO1"), ("2", "GND"), ("3", "IO2"), ("4", "IO2B"), ("5", "VBUS"), ("6", "IO1B")])}),
    );
    let mcu: Vec<(String, String)> = (1..=20)
        .map(|i| {
            let name = match i {
                17 => "PA11".to_string(),
                18 => "PA12".to_string(),
                15 => "VSS".to_string(),
                16 => "VDD".to_string(),
                _ => format!("P{i}"),
            };
            (i.to_string(), name)
        })
        .collect();
    let mcu: Vec<(&str, &str)> = mcu.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    exec(&r, &mut s, "part.create", json!({"id": "MCU", "category": "mcu", "package": "TSSOP-20", "pins": pins(&mcu)}));
    for (part, refdes) in [("USB", "J1"), ("ESD2", "U2"), ("MCU", "U1")] {
        exec(&r, &mut s, "circuit.add", json!({"part": part, "refdes": refdes}));
    }
    exec(&r, &mut s, "net.connect", json!({"net": "USB_DP", "pins": ["J1.DP", "U2.IO1", "U2.IO1B", "U1.PA12"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "USB_DM", "pins": ["J1.DM", "U2.IO2", "U2.IO2B", "U1.PA11"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "VBUS", "pins": ["J1.VBUS", "U2.VBUS"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["J1.GND", "J1.SHIELD", "U2.GND", "U1.VSS"]}));
    exec(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "USB_DM", "name": "USB"}));
    outline(&r, &mut s, w, h);
    (d, r, s)
}

/// The first automatic placement of a USB front does not leave the pair crossing at the ESD
/// array, whatever the board's shape, and the pair then routes.
#[test]
fn flow_through_part_keeps_its_pair_uncrossed() {
    let mut crossed = Vec::new();
    let mut unrouted = Vec::new();
    for (w, h) in [(30.0, 20.0), (40.0, 24.0), (40.0, 40.0), (50.0, 30.0), (60.0, 45.0), (80.0, 40.0), (100.0, 75.0)] {
        let (_d, r, mut s) = usb_front(w, h);
        let o = exec(&r, &mut s, "place.auto", json!({}));
        let warned = o["diagnostics"].as_array().unwrap().iter().any(|d| d["code"] == "place.pair_crossing");
        let c = cadlab::board::place::pair_crossings(project(&s));
        assert_eq!(warned, !c.is_empty(), "{w}x{h}: {o}");
        if !c.is_empty() {
            crossed.push(format!("{w}x{h}: {c:?}"));
        }
        let o = exec(&r, &mut s, "route.diffpair", json!({}));
        if o["output"]["pairs"][0]["status"] != "routed" {
            let at: Vec<String> = ["J1", "U2", "U1"]
                .iter()
                .map(|x| {
                    let f = &project(&s).board().footprints[*x];
                    format!("{x} ({}, {}) {}", f.at.x, f.at.y, f.rotation)
                })
                .collect();
            unrouted.push(format!("{w}x{h} {at:?}: {}", o["output"]["pairs"][0]["failures"]));
        }
    }
    assert!(crossed.is_empty(), "{crossed:#?}");
    assert!(unrouted.is_empty(), "{unrouted:#?}");
}

/// Long connectors go along an edge where that costs no ratsnest, however the board is shaped
/// (before D71: three across on the STM32 board on 100 × 75 mm, 1387 mm); `along_edges` lays them
/// all at the ratsnest's price.
#[test]
fn long_connectors_lie_along_their_edges() {
    let (_d, r, mut s) = stm32(100.0, 75.0);
    let (_st, c) = place(&r, &mut s, json!({}), 100.0, 75.0);
    assert!(c.across.len() <= 1, "{:?}", c.across);
    assert!(c.ratsnest < 1387.0, "ratsnest {:.1} mm", c.ratsnest);
    assert_eq!(placement_errors(project(&s)), Vec::<String>::new());
    for (w, h) in [(60.0, 45.0), (100.0, 80.0)] {
        let (_d, r, mut s) = channels(w, h);
        let (_st, c) = place(&r, &mut s, json!({}), w, h);
        assert!(c.across.is_empty(), "{w}x{h}: {:?}", c.across);
    }
    let plain = c.ratsnest;

    let (_d, r, mut s) = stm32(100.0, 75.0);
    let (_st, c) = place(&r, &mut s, json!({"along_edges": true}), 100.0, 75.0);
    assert!(c.across.is_empty(), "{:?}", c.across);
    assert!(c.connector < 2.0, "a connector {:.2} mm from the edge", c.connector);
    assert_eq!(placement_errors(project(&s)), Vec::<String>::new());
    println!("along_edges: {:.1} mm against {plain:.1} mm", c.ratsnest);

    let f = r.execute(
        &mut s,
        "place.auto",
        json!({"compact": true, "along_edges": true, "replace": true}),
        RunOptions::default(),
    );
    let f = f.expect_err("not with compact");
    assert_eq!(f.error.diagnostic.code, "place.along_edges_compact");
    assert!(f.error.diagnostic.hint.is_some());
}

/// What goes with an IC inherits its region: decoupling capacitors and other passives stay
/// inside it without a hint of their own; an own hint wins; a full region gives way with a note
/// rather than leaving a part unplaced.
#[test]
fn passives_inherit_their_ics_region() {
    let (_d, r, mut s) = channels(100.0, 80.0);
    // Channel 1 and its parts' IC on the left; U2 on the right, its capacitor hinted elsewhere.
    let left = json!({"from": ["0mm", "0mm"], "to": ["40mm", "80mm"]});
    let right = json!({"from": ["60mm", "0mm"], "to": ["100mm", "80mm"]});
    exec(&r, &mut s, "place.hint", json!({"refdes": ["U1"], "region": left}));
    exec(&r, &mut s, "place.hint", json!({"refdes": ["U2"], "region": right}));
    exec(&r, &mut s, "place.hint", json!({"refdes": ["C2"], "region": left}));
    let o = exec(&r, &mut s, "place.auto", json!({}));
    assert!(o["diagnostics"].as_array().unwrap().iter().all(|d| d["code"] != "place.region_full"), "{o}");
    let p = project(&s);
    let bx = cadlab::board::place::courtyard_box;
    for x in ["U1", "C1", "R11", "R12", "R13", "R14", "C2"] {
        let b = bx(p, x).unwrap();
        assert!(b.x1 <= 40_000_000, "{x} ends at {} mm, past the left region", b.x1 as f64 / 1e6);
    }
    for x in ["U2", "R21", "R22", "R23", "R24"] {
        let b = bx(p, x).unwrap();
        assert!(b.x0 >= 60_000_000, "{x} starts at {} mm, before the right region", b.x0 as f64 / 1e6);
    }

    // A region with room for the op-amp only: its passives give way, and say so.
    let (_d, r, mut s) = channels(100.0, 80.0);
    let tiny = json!({"from": ["10mm", "10mm"], "to": ["19mm", "17mm"]});
    exec(&r, &mut s, "place.hint", json!({"refdes": ["U1"], "region": tiny}));
    let o = exec(&r, &mut s, "place.auto", json!({}));
    assert!(o["output"].get("unplaced").is_none(), "{o}");
    let d = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "place.region_full");
    let d = d.unwrap_or_else(|| panic!("{o}"));
    assert!(d["hint"].as_str().is_some_and(|h| h.contains("region")), "{d}");
    assert!(!d["subjects"].as_array().unwrap().is_empty(), "{d}");
    let b = cadlab::board::place::courtyard_box(project(&s), "U1").unwrap();
    assert!(b.x1 <= 19_000_000, "the IC's own hint still holds");
}

/// The text about board use says what its numbers mean on a sparse board.
#[test]
fn sparse_boards_say_what_the_numbers_mean() {
    let (_d, r, mut s) = channels(100.0, 80.0);
    let o = exec(&r, &mut s, "place.auto", json!({}));
    let st = placement_stats(project(&s)).unwrap();
    assert!(st.sparse, "{st:?}");
    assert_eq!(st.crowding, 0.0, "{st:?}");
    assert!(st.fullest_window > 0.0, "the fullest window still says how dense the parts are: {st:?}");
    let text = o["summary"].as_str().unwrap();
    assert!(text.contains("sparse") && text.contains("judge by the ratsnest"), "{text}");

    let (_d, r, mut s) = stm32(60.0, 45.0);
    let o = exec(&r, &mut s, "place.auto", json!({}));
    assert!(!placement_stats(project(&s)).unwrap().sparse);
    assert!(!o["summary"].as_str().unwrap().contains("sparse"), "{o}");
}

/// Spread placement of the ~500-part synthetic board (`bigboard`, routing removed), timed:
/// `cargo test --release --test placement_quality -- --ignored --nocapture big_board`.
#[test]
#[ignore]
fn big_board_spread_timing() {
    let (_d, r, mut s) = bigboard::build(bigboard::Spec::default());
    {
        let b = s.project.as_mut().unwrap().board_mut();
        b.tracks.clear();
        b.vias.clear();
    }
    let t = std::time::Instant::now();
    let o = exec(&r, &mut s, "place.auto", json!({"replace": true}));
    let ms = t.elapsed().as_millis();
    let st = placement_stats(project(&s)).unwrap();
    println!(
        "place.auto (spread, {} parts): {ms} ms, ratsnest {}, {st:?}",
        o["output"]["placements"].as_object().unwrap().len(),
        o["output"]["ratsnest_length"]
    );
}
