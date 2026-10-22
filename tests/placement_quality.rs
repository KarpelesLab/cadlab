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
    for (r, b) in &all {
        if category(r) == Some(cadlab::model::part::Category::Connector) {
            let (x0, y0, x1, y1) = (b.x0 as f64 / 1e6, b.y0 as f64 / 1e6, b.x1 as f64 / 1e6, b.y1 as f64 / 1e6);
            connector = connector.max(x0.min(w - x1).min(y0).min(h - y1));
        }
    }
    Costs { ratsnest, decap, connector }
}

fn place(r: &Registry, s: &mut Session, args: Value, w: f64, h: f64) -> (PlacementStats, Costs) {
    let o = exec(r, s, "place.auto", args);
    assert!(o["output"].get("unplaced").is_none(), "{o}");
    let p = project(s);
    (placement_stats(p).unwrap(), costs(p, w, h))
}

fn row(name: &str, st: &PlacementStats, c: &Costs) -> String {
    format!(
        "| {name} | {}/{} | {:.2} | {:.2} | {:.1} | {:.2} | {:.1} | {:.2} | {:.2} |",
        st.empty_windows,
        st.windows,
        st.crowding,
        st.unevenness,
        mm(st.largest_empty),
        st.extent,
        c.ratsnest,
        c.decap,
        c.connector
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
        "| board | empty windows | crowding | unevenness | largest empty mm | extent | ratsnest mm | decap mm | connector mm |{}",
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
