//! Specctra DSN export and SES import: golden design, round trip through the reader, a
//! hand-written session, and the freerouting oracle (route our DSN, import the session, cadlab
//! DRC must be clean).

mod common;

use std::path::{Path, PathBuf};

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::board::BoardSide;
use cadlab::specctra::dsn::{Dsn, place_point};
use cadlab::specctra::export::{Options, export};
use cadlab::{Nm, Point};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn fail(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => panic!("{cmd} should fail: {}", serde_json::to_value(&o).unwrap()),
        Err(f) => serde_json::to_value(&f.error).unwrap(),
    }
}

fn new_project(r: &Registry, s: &mut Session, name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    exec(r, s, "project.new", json!({"path": dir.path().join(name), "name": name}));
    dir
}

fn mm(x: f64, y: f64) -> Value {
    json!([format!("{x}mm"), format!("{y}mm")])
}

fn drc_errors(s: &Session) -> Vec<String> {
    cadlab::drc::check(s.project.as_ref().unwrap())
        .into_iter()
        .filter(|d| d.severity == cadlab::Severity::Error)
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect()
}

/// The ATtiny85 board with parts on both sides (rotated), a power class, plated and
/// non-plated mounting holes, a cutout, a keep-out and a locked track.
fn attiny_board(r: &Registry, s: &mut Session) {
    common::boards::build_board(r, s);
    exec(r, s, "board.outline", json!({"width": "36mm", "height": "28mm", "corner_radius": "1mm"}));
    for (refdes, x, y, rot, side) in [
        ("J1", 4.0, 13.0, 0, "top"),
        ("U1", 10.0, 19.0, 0, "top"),
        ("C1", 7.0, 22.5, 90, "top"),
        ("C2", 13.5, 22.5, 90, "top"),
        ("U2", 18.0, 13.0, 0, "top"),
        ("C3", 18.0, 18.5, 0, "top"),
        ("R1", 23.0, 18.5, 0, "top"),
        ("J2", 29.0, 13.0, 90, "bottom"),
        ("R2", 14.0, 7.0, 90, "bottom"),
        ("D1", 19.0, 5.0, 30, "top"),
    ] {
        exec(r, s, "place.set", json!({"refdes": refdes, "at": mm(x, y), "rotation": rot, "side": side}));
    }
    exec(r, s, "place.lock", json!({"refdes": ["J1"]}));
    exec(r, s, "netclass.set", json!({"name": "power", "track_width": "0.4mm", "clearance": "0.25mm"}));
    exec(r, s, "net.set", json!({"nets": ["GND", "VBUS", "3V3"], "class": "power"}));
    exec(r, s, "board.hole", json!({"at": mm(3.0, 3.0), "drill": "1mm", "pad": "1.8mm", "net": "GND"}));
    exec(r, s, "board.hole", json!({"at": mm(33.0, 25.0), "drill": "1.2mm"}));
    exec(r, s, "board.cutout", json!({"rect": {"from": mm(30.0, 2.0), "to": mm(33.0, 5.0)}}));
    exec(
        r,
        s,
        "keepout.add",
        json!({"name": "antenna", "outline": {"rect": {"from": mm(24.0, 22.0), "to": mm(29.0, 26.0)}},
               "layers": ["F.Cu"], "no_tracks": true}),
    );
    // A hand-routed, locked piece of VBUS.
    exec(r, s, "track.add", json!({"layer": "F.Cu", "points": ["J1.1", mm(4.0, 17.0)]}));
    lock_last_track(s);
}

/// Locks the last track (no command does that yet).
fn lock_last_track(s: &mut Session) {
    s.project.as_mut().unwrap().board_mut().tracks.last_mut().unwrap().locked = true;
}

fn golden(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/specctra").join(name)
}

#[test]
fn dsn_golden_and_round_trip() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let dir = new_project(&r, &mut s, "tiny");
    attiny_board(&r, &mut s);
    let o = exec(&r, &mut s, "export.dsn", json!({}));
    let path = dir.path().join("tiny/out/route/tiny.dsn");
    assert_eq!(o["output"]["path"], path.display().to_string());
    assert_eq!(o["output"]["components"], 12, "10 parts and 2 holes");
    assert_eq!(o["output"]["protected"], 1);
    let text = std::fs::read_to_string(&path).unwrap();
    common::golden::assert_golden(&golden("tiny.dsn"), &text);

    // The reader gives back exactly what was written.
    let p = s.project.as_ref().unwrap();
    let written = export(p, "tiny", &Options::default()).dsn;
    let parsed = Dsn::parse(&text).unwrap();
    assert_eq!(parsed.layers, written.layers);
    assert_eq!(parsed.boundary, written.boundary);
    assert_eq!(parsed.keepouts, written.keepouts);
    assert_eq!(parsed.vias, written.vias);
    assert_eq!(parsed.rule, written.rule);
    assert_eq!(parsed.places, written.places);
    assert_eq!(parsed.images, written.images);
    assert_eq!(parsed.padstacks, written.padstacks);
    assert_eq!(parsed.nets, written.nets);
    assert_eq!(parsed.classes, written.classes);
    assert_eq!(parsed.wires, written.wires);
    assert_eq!(parsed.wire_vias, written.wire_vias);
    assert_eq!(parsed, written);
    assert_eq!(Dsn::parse(&parsed.write()).unwrap(), parsed);

    // The model: every copper pad is a pin at its board position, with its net.
    let pins = parsed.pin_positions();
    let pads: Vec<_> = cadlab::board::placed_pads(p).into_iter().filter(|pp| !pp.layers.is_empty()).collect();
    assert_eq!(pins.len(), pads.len());
    for pp in &pads {
        let key = format!("{}-{}", pp.refdes, pp.number);
        let at = pins.iter().find(|(k, _)| *k == key).unwrap_or_else(|| panic!("no pin {key}")).1;
        assert_eq!(at, pp.center, "{key}");
        if let Some(net) = &pp.net {
            let n = parsed.nets.iter().find(|n| &n.name == net).unwrap();
            assert!(n.pins.contains(&key), "{key} in {net}");
        }
    }
    // Bottom-side parts are mirrored: J2's pin 1 is on the other side of its origin than on top.
    let j2 = parsed.places.iter().find(|pl| pl.refdes == "J2").unwrap();
    assert_eq!(j2.side, BoardSide::Bottom);
    let img = parsed.images.iter().find(|i| i.name == j2.image).unwrap();
    let pin1 = img.pins.iter().find(|pin| pin.id == "1").unwrap();
    let top = cadlab::specctra::dsn::Place { side: BoardSide::Top, ..j2.clone() };
    assert_ne!(place_point(j2, pin1.at), place_point(&top, pin1.at));
    // Classes carry the rules, cutouts and keep-outs are there, the locked track is protected.
    let power = parsed.classes.iter().find(|c| c.name == "power").unwrap();
    assert_eq!(power.nets, ["3V3", "GND", "VBUS"]);
    assert_eq!(power.rule.width, Some(Nm::from_um(400)));
    assert_eq!(power.rule.clearance, Some(Nm::from_um(250)));
    assert!(parsed.keepouts.iter().any(|k| k.name == "cutout1"));
    assert!(parsed.keepouts.iter().any(|k| k.name == "antenna" && k.shape.layer() == "F.Cu"));
    assert!(parsed.wires.iter().all(|w| w.protect));
    assert!(parsed.places.iter().any(|pl| pl.refdes == "H1" && pl.locked));
    assert!(parsed.places.iter().find(|pl| pl.refdes == "J1").unwrap().locked);

    // Deterministic.
    exec(&r, &mut s, "export.dsn", json!({"path": "again"}));
    assert_eq!(std::fs::read_to_string(dir.path().join("tiny/again.dsn")).unwrap(), text);
    // Protecting everything.
    let o = exec(&r, &mut s, "export.dsn", json!({"path": "p.dsn", "protect_existing": true}));
    assert_eq!(o["output"]["protected"], 1);
    let f = fail(&r, &mut s, "export.dsn", json!({"resolution": 0}));
    assert_eq!(f["code"], "export.dsn_resolution");
}

/// The LDO + caps board (`tests/route.rs`) with an unlocked track to be replaced.
fn ldo_board(r: &Registry, s: &mut Session) {
    exec(
        r,
        s,
        "part.create",
        json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
        "pins": [{"number": "1", "name": "VIN", "kind": "power_in"}, {"number": "2", "name": "GND", "kind": "power_in"},
                 {"number": "3", "name": "EN", "kind": "input"}, {"number": "4", "name": "NC", "kind": "no_connect"},
                 {"number": "5", "name": "VOUT", "kind": "power_out"}]}),
    );
    exec(r, s, "circuit.add", json!({"part": "AP2112K-3.3TRG1"}));
    exec(r, s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 2}));
    exec(r, s, "net.connect", json!({"net": "VIN", "pins": ["U1.VIN", "U1.EN", "C1.1"]}));
    exec(r, s, "net.connect", json!({"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2"]}));
    exec(r, s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}));
    exec(r, s, "board.outline", json!({"width": "20mm", "height": "15mm", "corner_radius": "1mm"}));
    exec(r, s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7.5mm"]}));
    exec(r, s, "place.set", json!({"refdes": "C1", "at": ["5mm", "9mm"], "rotation": 90}));
    exec(r, s, "place.set", json!({"refdes": "C2", "at": ["15mm", "9mm"], "rotation": 90}));
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

#[test]
fn ses_import_hand_written() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s, "ldo");
    ldo_board(&r, &mut s);
    // An unlocked 3V3 track the session replaces, and a locked GND stub it repeats.
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": ["U1.VOUT", mm(13.0, 12.0), "C2.1"]}));
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": [mm(5.0, 11.0), mm(5.0, 9.4525)], "net": "GND"}));
    lock_last_track(&mut s);
    let o = exec(&r, &mut s, "route.import_ses", json!({"path": fixture("ldo.ses")}));
    let out = &o["output"];
    assert_eq!(out["nets"], 3);
    assert_eq!(out["tracks_removed"], 2, "the two segments of the unlocked 3V3 track");
    assert_eq!(out["duplicates"], 1, "the locked GND stub");
    assert_eq!(out["tracks_added"], 9);
    assert_eq!(out["vias_added"], 3);
    assert_eq!(out["unrouted"], 0);
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    let b = s.project.as_ref().unwrap().board();
    assert_eq!(b.tracks.len(), 10);
    assert!(b.tracks.iter().filter(|t| t.locked).count() == 1);
    // Exact unit conversion from 0.1 µm steps; via sizes from cadlab's padstack names or the
    // session's own padstacks (drill from the rules).
    let v = &b.vias[0];
    assert_eq!(v.at, Point::new(Nm(10_000_000), Nm(7_500_000)));
    assert_eq!((v.diameter, v.drill), (Nm::from_um(600), Nm::from_um(300)));
    assert_eq!((v.from.as_str(), v.to.as_str()), ("F.Cu", "B.Cu"));
    assert_eq!((b.vias[2].diameter, b.vias[2].drill), (Nm::from_um(800), Nm::from_um(300)));
    assert!(b.tracks.iter().any(|t| t.layer == "B.Cu" && t.width == Nm(254_000) && t.net.as_deref() == Some("GND")));
    assert!(b.tracks.iter().any(|t| t.start == Point::new(Nm(8_847_500), Nm(6_550_000))));
    assert_eq!(o["diagnostics"][0]["code"], "ses.placement_mismatch");
    assert_eq!(o["diagnostics"][0]["subjects"][0], "C2");
    assert_eq!(o["diagnostics"].as_array().unwrap().len(), 1);

    // Re-importing with keep_existing adds nothing new.
    let o = exec(&r, &mut s, "route.import_ses", json!({"path": fixture("ldo.ses"), "keep_existing": true}));
    assert_eq!(o["output"]["tracks_added"], 0);
    assert_eq!(o["output"]["vias_added"], 0);

    // Errors.
    let bad = tempfile::tempdir().unwrap();
    let f = bad.path().join("bad.ses");
    std::fs::write(
        &f,
        "(session x (routes (resolution um 10) (network_out (net NOPE (wire (path F.Cu 10 0 0 10 0))))))",
    )
    .unwrap();
    let e = fail(&r, &mut s, "route.import_ses", json!({"path": f}));
    assert_eq!(e["code"], "ses.unknown_net");
    std::fs::write(
        &f,
        "(session x (routes (resolution um 10) (network_out (net GND (wire (path In7.Cu 10 0 0 10 0))))))",
    )
    .unwrap();
    assert_eq!(fail(&r, &mut s, "route.import_ses", json!({"path": f}))["code"], "ses.unknown_layer");
    std::fs::write(&f, "(session x (routes (resolution um 10) (network_out (net GND (via Via0 0 0)))))").unwrap();
    assert_eq!(fail(&r, &mut s, "route.import_ses", json!({"path": f}))["code"], "ses.unknown_padstack");
    std::fs::write(&f, "(session x (routes").unwrap();
    assert_eq!(fail(&r, &mut s, "route.import_ses", json!({"path": f}))["code"], "ses.parse");
}

/// Runs freerouting (`CADLAB_ORACLE_FREEROUTING`: a jar, run with `java -jar`, or a launcher)
/// headless on `dsn`, writing `ses`.
fn freerouting(tool: &Path, dsn: &Path, ses: &Path) -> Option<u64> {
    let (program, mut args): (PathBuf, Vec<String>) = if tool.extension().is_some_and(|e| e == "jar") {
        (PathBuf::from("java"), vec!["-jar".into(), tool.display().to_string()])
    } else {
        (tool.to_path_buf(), vec![])
    };
    args.extend(
        [
            "-de",
            &dsn.display().to_string(),
            "-do",
            &ses.display().to_string(),
            "-mp",
            "20",
            "-mt",
            "1",
            "--gui.enabled=false",
        ]
        .map(String::from),
    );
    let out = std::process::Command::new(&program).args(&args).output().expect("running freerouting");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(ses.is_file(), "freerouting wrote no session:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    // freerouting's own count of connections it left open, from its statistics report.
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("\"incomplete_count\":"))
        .and_then(|v| v.trim().trim_end_matches(',').parse().ok())
}

#[test]
fn freerouting_oracle() {
    let Some(tool) = common::oracle::require(common::oracle::Oracle::Freerouting) else { return };
    let r = Registry::with_builtins();
    // Both sides, rotations, holes, cutout, keep-out, a protected track.
    let mut s = Session::new();
    let dir = new_project(&r, &mut s, "tiny");
    attiny_board(&r, &mut s);
    let o = route_with_freerouting(&tool, &r, &mut s, dir.path(), "tiny");
    assert!(o["output"]["duplicates"].as_u64().unwrap() >= 1, "the protected track comes back");
    assert!(s.project.as_ref().unwrap().board().tracks.iter().any(|t| t.locked));

    // A larger auto-placed four-layer board.
    let mut s = Session::new();
    let dir = new_project(&r, &mut s, "stm32");
    common::boards::build_stm32_board(&r, &mut s);
    exec(&r, &mut s, "board.setup", json!({"layers": 4}));
    exec(&r, &mut s, "board.outline", json!({"width": "60mm", "height": "45mm", "corner_radius": "2mm"}));
    exec(&r, &mut s, "place.auto", json!({"spacing": "1mm"}));
    route_with_freerouting(&tool, &r, &mut s, dir.path(), "stm32");
}

/// freerouting runs per board before giving up on a complete session.
const ATTEMPTS: usize = 5;

/// Routes the session's board with freerouting and imports the result: cadlab DRC must find no
/// clearance, short, edge or hole problem in what freerouting routed, and everything must be
/// connected. freerouting (2.1, multi-pass with optimization) now and then reports a complete
/// route but leaves a net out of its session; such a run is retried (after ripping the
/// imported routing), up to [`ATTEMPTS`] times. `CADLAB_SPECCTRA_KEEP=<dir>` keeps the DSN, SES and a
/// render of the last attempt.
fn route_with_freerouting(tool: &Path, r: &Registry, s: &mut Session, dir: &Path, name: &str) -> Value {
    let dsn = dir.join(format!("{name}.dsn"));
    let ses = dir.join(format!("{name}.ses"));
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            exec(r, s, "route.rip", json!({"all": true}));
        }
        exec(r, s, "export.dsn", json!({"path": dsn}));
        let _ = std::fs::remove_file(&ses);
        let open = freerouting(tool, &dsn, &ses);
        eprintln!("{name} (attempt {attempt}): freerouting reports {open:?} open connection(s)");
        let o = exec(r, s, "route.import_ses", json!({"path": ses}));
        eprintln!("{name} (attempt {attempt}): {}", o["summary"]);
        if let Some(keep) = std::env::var_os("CADLAB_SPECCTRA_KEEP") {
            let keep = Path::new(&keep);
            std::fs::copy(&dsn, keep.join(format!("{name}.dsn"))).unwrap();
            std::fs::copy(&ses, keep.join(format!("{name}.ses"))).unwrap();
            exec(r, s, "render.board", json!({"path": keep.join(format!("{name}.png")), "px_per_mm": 30}));
        }
        let errors: Vec<String> = drc_errors(s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
        assert_eq!(errors, Vec::<String>::new(), "{name}");
        if o["output"]["unrouted"] == 0 || attempt == ATTEMPTS {
            assert_eq!(drc_errors(s), Vec::<String>::new(), "{name}");
            return o;
        }
    }
    unreachable!()
}
