//! M8 differential pairs and length tuning: coupled routing of a USB 2.0 pair at the width and
//! gap solved for 90 Ω, skew compensation, length groups tuned with meanders, DRC checks,
//! determinism.

mod common;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::{Nm, Point, Severity};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn fail(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> String {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => panic!("{cmd} should fail: {:?}", serde_json::to_value(&o).unwrap()),
        Err(f) => f.error.diagnostic.code.to_string(),
    }
}

fn new_project(r: &Registry, s: &mut Session) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    exec(r, s, "project.new", json!({"path": dir.path().join("p")}));
    dir
}

fn pins(list: &[(&str, &str)]) -> Value {
    Value::Array(list.iter().map(|(n, name)| json!({"number": n, "name": name, "kind": "passive"})).collect())
}

fn mm(v: &Value) -> f64 {
    v.as_str().unwrap().trim_end_matches("mm").parse().unwrap()
}

fn codes(o: &Value) -> Vec<String> {
    o["diagnostics"]
        .as_array()
        .map(|a| a.iter().map(|d| d["code"].as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

fn drc(s: &Session) -> Vec<cadlab::Diagnostic> {
    cadlab::drc::check(s.project.as_ref().unwrap())
}

fn drc_errors(s: &Session) -> Vec<String> {
    drc(s)
        .into_iter()
        .filter(|d| d.severity == Severity::Error && d.code != "drc.unrouted")
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect()
}

fn drc_codes(s: &Session, prefix: &str) -> Vec<String> {
    drc(s).into_iter().filter(|d| d.code.starts_with(prefix)).map(|d| format!("{}: {}", d.code, d.message)).collect()
}

/// Renders the board to `$CADLAB_ROUTE_RENDER/<name>.png` when that variable is set.
fn render(r: &Registry, s: &mut Session, name: &str) {
    if let Some(dir) = std::env::var_os("CADLAB_ROUTE_RENDER") {
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        exec(r, s, "render.board", json!({"path": path, "px_per_mm": 40}));
    }
}

/// A USB 2.0 device: USB-C receptacle (J1) → ESD array (U2) → MCU (U1, TSSOP-20), on a
/// four-layer board with a 0.2 mm prepreg under F.Cu; USB_DP/USB_DM in class `usb` solved
/// for 90 Ω with a 0.15 mm gap.
fn usb_board(r: &Registry, s: &mut Session) {
    exec(r, s, "board.setup", json!({"layers": 4}));
    exec(r, s, "board.dielectric", json!({"thickness": "0.2mm", "er": "4.4", "material": "prepreg"}));
    exec(r, s, "board.dielectric", json!({"gap": 2, "thickness": "1.065mm", "er": "4.6", "material": "core"}));
    exec(r, s, "board.outline", json!({"width": "40mm", "height": "24mm", "corner_radius": "1mm"}));
    exec(
        r,
        s,
        "part.create",
        json!({"id": "USB_C", "category": "connector", "description": "USB-C receptacle (USB 2.0)", "package": "PinHeader 1x07",
            "pins": pins(&[("1", "VBUS"), ("2", "CC1"), ("3", "DM"), ("4", "DP"), ("5", "CC2"), ("6", "GND"), ("7", "SHIELD")])}),
    );
    exec(
        r,
        s,
        "part.create",
        json!({"id": "ESD2", "category": "ic", "description": "2-line USB ESD array, flow-through", "package": "SOT-23-6",
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
    exec(r, s, "part.create", json!({"id": "MCU", "category": "mcu", "package": "TSSOP-20", "pins": pins(&mcu)}));
    exec(r, s, "circuit.add", json!({"part": "USB_C", "refdes": "J1"}));
    exec(r, s, "circuit.add", json!({"part": "ESD2", "refdes": "U2"}));
    exec(r, s, "circuit.add", json!({"part": "MCU", "refdes": "U1"}));
    exec(r, s, "net.connect", json!({"net": "USB_DP", "pins": ["J1.DP", "U2.IO1", "U2.IO1B", "U1.PA12"]}));
    exec(r, s, "net.connect", json!({"net": "USB_DM", "pins": ["J1.DM", "U2.IO2", "U2.IO2B", "U1.PA11"]}));
    exec(r, s, "net.connect", json!({"net": "VBUS", "pins": ["J1.VBUS", "U2.VBUS"]}));
    exec(r, s, "net.connect", json!({"net": "GND", "pins": ["J1.GND", "J1.SHIELD", "U2.GND", "U1.VSS"]}));
    for (refdes, at, rot) in [("J1", ["4mm", "12mm"], 0), ("U2", ["15mm", "12mm"], 180), ("U1", ["30mm", "12mm"], 180)]
    {
        exec(r, s, "place.set", json!({"refdes": refdes, "at": at, "rotation": rot}));
    }
    let o =
        exec(r, s, "impedance.solve", json!({"target": "90ohm", "gap": "0.15mm", "layer": "F.Cu", "netclass": "usb"}));
    assert!((o["output"]["zdiff"].as_str().unwrap().trim_end_matches('Ω').parse::<f64>().unwrap() - 90.0).abs() < 1.0);
    exec(r, s, "netclass.set", json!({"name": "usb", "clearance": "0.15mm"}));
    exec(r, s, "net.set", json!({"nets": ["USB_DP", "USB_DM"], "class": "usb"}));
}

fn tracks_of<'a>(s: &'a Session, net: &str) -> Vec<&'a cadlab::model::board::Track> {
    s.project.as_ref().unwrap().board().tracks.iter().filter(|t| t.net.as_deref() == Some(net)).collect()
}

#[test]
fn usb_pair_routes_coupled_at_the_solved_geometry() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    usb_board(&r, &mut s);
    let class = exec(&r, &mut s, "netclass.show", json!({"name": "usb"}));
    let (w, gap) = (class["output"]["diff_pair_width"].clone(), class["output"]["diff_pair_gap"].clone());
    assert_eq!(gap, "0.15mm");
    // Suggestion by name, then the pair.
    let o = exec(&r, &mut s, "diffpair.suggest", json!({}));
    assert_eq!(o["output"]["pairs"], json!([{"name": "USB_D", "p": "USB_DP", "n": "USB_DM", "rule": "DP/DM"}]));
    let o = exec(
        &r,
        &mut s,
        "diffpair.add",
        json!({"p": "USB_DP", "n": "USB_DM", "max_skew": "0.1mm", "max_uncoupled": "12mm"}),
    );
    assert_eq!(o["output"]["name"], "USB_D");
    assert_eq!(o["output"]["rules"]["width"], w);
    assert!(codes(&o).is_empty(), "{:?}", o["diagnostics"]);
    let o = exec(&r, &mut s, "route.diffpair", json!({}));
    render(&r, &mut s, "usb-pair");
    let pr = &o["output"]["pairs"][0];
    eprintln!("{}", serde_json::to_string_pretty(&o).unwrap());
    assert_eq!(pr["status"], "routed", "{pr}");
    assert_eq!(pr["connections"], 3, "J1 to U2, through U2 (flow-through), U2 to U1");
    assert_eq!(pr["width"], w);
    assert_eq!(pr["gap"], gap);
    assert!(mm(&pr["skew"]) <= 0.1, "{pr}");
    assert!(mm(&pr["coupled"]) > 10.0, "{pr}");
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    assert_eq!(drc_codes(&s, "drc.diffpair"), Vec::<String>::new());
    // Every track of the pair is at the solved width; coupled sections at the gap.
    let wn = Nm::parse(w.as_str().unwrap()).unwrap();
    for net in ["USB_DP", "USB_DM"] {
        assert!(tracks_of(&s, net).iter().all(|t| t.width == wn), "{net}");
    }
    let p = s.project.as_ref().unwrap();
    let d = &p.circuit().diffpairs["USB_D"];
    let cp = cadlab::lengths::coupling(p, d, Nm::parse("0.15mm").unwrap());
    assert!(!cp.sections.is_empty());
    for sec in &cp.sections {
        assert!((sec.gap.0 - 150_000).abs() <= 2, "{sec:?}");
    }
    assert!(cp.skew <= Nm::from_um(100));
    // Status: everything of the pair is connected.
    let st = exec(&r, &mut s, "route.status", json!({}));
    let by_net = &st["output"]["unrouted_by_net"];
    assert!(by_net.get("USB_DP").is_none() && by_net.get("USB_DM").is_none(), "{by_net}");
}

#[test]
fn route_all_routes_pairs_first_and_completes_the_board() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    usb_board(&r, &mut s);
    exec(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "USB_DM", "max_skew": "0.1mm"}));
    // The ESD array's GND and VBUS pins sit between its flow-through D+/D- pins: as a designer
    // would, drop their vias under the body first (the pair then passes outside them).
    let pads = cadlab::board::placed_pads(s.project.as_ref().unwrap());
    let center = Point::new(Nm::from_mm(15), Nm::from_mm(12));
    for (pin, net) in [("2", "GND"), ("5", "VBUS")] {
        let pp = pads.iter().find(|p| p.refdes == "U2" && p.number == pin).unwrap();
        let at =
            Point::new(Nm((pp.center.x.0 * 4 + center.x.0 * 6) / 10), Nm((pp.center.y.0 * 4 + center.y.0 * 6) / 10));
        exec(&r, &mut s, "via.add", json!({"at": at, "net": net}));
    }
    let o = exec(&r, &mut s, "route.all", json!({"seed": 1}));
    render(&r, &mut s, "usb-all");
    assert_eq!(o["output"]["stats"]["completion"], 100.0, "{}", o["output"]);
    let pr = &o["output"]["pairs"][0];
    assert_eq!(pr["status"], "routed", "{pr}");
    assert!(mm(&pr["skew"]) <= 0.1, "{pr}");
    assert!(drc(&s).iter().all(|d| d.severity != Severity::Error), "{:?}", drc_errors(&s));
    assert_eq!(drc_codes(&s, "drc.diffpair"), Vec::<String>::new());
    // The ordinary router never moves pair copper: routing again changes nothing.
    let before = s.project.as_ref().unwrap().board().tracks.clone();
    let o = exec(&r, &mut s, "route.all", json!({}));
    assert_eq!(o["output"]["stats"]["connections"], 0);
    assert_eq!(s.project.as_ref().unwrap().board().tracks, before);
}

/// Eight "DQ" nets between two rows of 0402 resistors, the far row staggered so the routed
/// lengths differ by 1.5 mm steps (about 20 to 31 mm), routed.
fn dq_board(r: &Registry, s: &mut Session) {
    exec(r, s, "board.outline", json!({"width": "50mm", "height": "36mm"}));
    exec(r, s, "circuit.add", json!({"part": "R 22R 1% 0402", "count": 16}));
    for k in 0..8 {
        let y = 4.0 + 3.5 * k as f64;
        exec(
            r,
            s,
            "net.connect",
            json!({"net": format!("DQ{k}"), "pins": [format!("R{}.2", k + 1), format!("R{}.1", k + 9)]}),
        );
        exec(r, s, "place.set", json!({"refdes": format!("R{}", k + 1), "at": ["6mm", format!("{y}mm")]}));
        let x = 26.0 + 1.5 * k as f64;
        exec(r, s, "place.set", json!({"refdes": format!("R{}", k + 9), "at": [format!("{x}mm"), format!("{y}mm")]}));
    }
    let o = exec(r, s, "route.all", json!({"seed": 1}));
    assert_eq!(o["output"]["stats"]["completion"], 100.0);
}

fn dq_members() -> Vec<String> {
    (0..8).map(|k| format!("DQ{k}")).collect()
}

fn tune_dq(style: &str, arcs: bool) {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    dq_board(&r, &mut s);
    let o = exec(&r, &mut s, "lengthgroup.set", json!({"name": "DQ", "members": dq_members(), "tolerance": "0.1mm"}));
    let st = &o["output"]["status"];
    let longest = mm(&st["target"]);
    assert!(longest > 25.0, "{st}");
    assert_eq!(drc_codes(&s, "drc.length_mismatch").len(), 7, "{st}");
    let o = exec(&r, &mut s, "route.tune", json!({"group": "DQ", "style": style, "arcs": arcs}));
    render(&r, &mut s, &format!("dq-{style}{}", if arcs { "-arcs" } else { "" }));
    assert!(codes(&o).is_empty(), "{:?}", o["diagnostics"]);
    for (k, m) in o["output"]["groups"][0]["members"].as_array().unwrap().iter().enumerate() {
        assert_eq!(m["error"], "0mm", "{style} DQ{k}: {m}");
        let after = mm(&m["after"]);
        assert!(longest - after <= 0.1 && after <= longest + 1e-4, "{m}");
        if k < 7 {
            assert!(m["meanders"].as_u64().unwrap() > 0, "{m}");
        }
    }
    assert!(drc(&s).iter().all(|d| d.severity != Severity::Error), "{style}: {:?}", drc_errors(&s));
    assert_eq!(drc_codes(&s, "drc.length"), Vec::<String>::new());
    if arcs {
        assert!(s.project.as_ref().unwrap().board().tracks.iter().any(|t| t.mid.is_some()));
    }
    // Measured lengths are the track lengths along arcs (no vias here).
    let p = s.project.as_ref().unwrap();
    for k in 0..8 {
        let net = format!("DQ{k}");
        let sum: i64 = p
            .board()
            .tracks
            .iter()
            .filter(|t| t.net.as_deref() == Some(net.as_str()))
            .map(|t| cadlab::board::track_length(t).0)
            .sum();
        assert!((sum - cadlab::lengths::net_length(p, &net).total.0).abs() <= 8);
    }
}

#[test]
fn length_group_tuned_with_trombones() {
    tune_dq("trombone", false);
}

#[test]
fn length_group_tuned_with_accordion_arcs() {
    tune_dq("accordion", true);
}

#[test]
fn length_group_tuned_with_sawtooth() {
    tune_dq("sawtooth", false);
}

#[test]
fn length_group_with_absolute_target_and_errors() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    dq_board(&r, &mut s);
    exec(
        &r,
        &mut s,
        "lengthgroup.set",
        json!({"name": "DQ", "members": dq_members(), "target": "33mm", "tolerance": "0.05mm"}),
    );
    let o = exec(&r, &mut s, "route.tune", json!({}));
    for m in o["output"]["groups"][0]["members"].as_array().unwrap() {
        assert!((mm(&m["after"]) - 33.0).abs() <= 0.05, "{m}");
    }
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    assert_eq!(drc_codes(&s, "drc.length"), Vec::<String>::new());
    let l = exec(&r, &mut s, "lengthgroup.list", json!({}));
    assert!(l["summary"].as_str().unwrap().contains("DQ: 33mm ±0.05mm"), "{}", l["summary"]);
    // A target below what is routed cannot be met by adding length.
    exec(&r, &mut s, "lengthgroup.set", json!({"name": "DQ", "members": dq_members(), "target": "10mm"}));
    let o = exec(&r, &mut s, "route.tune", json!({"group": "DQ"}));
    assert!(codes(&o).iter().all(|c| c == "route.tune_unmet"));
    assert_eq!(codes(&o).len(), 8);
    assert_eq!(drc_codes(&s, "drc.length_mismatch").len(), 8);
    // Errors.
    assert_eq!(fail(&r, &mut s, "route.tune", json!({"group": "nope"})), "lengthgroup.not_found");
    assert_eq!(fail(&r, &mut s, "lengthgroup.set", json!({"name": "X", "members": ["NOPE"]})), "net.not_found");
    assert_eq!(fail(&r, &mut s, "lengthgroup.set", json!({"name": "X", "members": ["DQ1"]})), "lengthgroup.no_target");
    assert_eq!(fail(&r, &mut s, "lengthgroup.remove", json!({"name": "X"})), "lengthgroup.not_found");
    assert_eq!(fail(&r, &mut s, "route.tune", json!({"spacing": "0mm"})), "route.invalid_meander");
    exec(&r, &mut s, "lengthgroup.remove", json!({"name": "DQ"}));
    assert_eq!(fail(&r, &mut s, "route.tune", json!({})), "route.nothing_to_tune");
}

#[test]
fn pair_in_a_length_group_meanders_as_a_pair() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    usb_board(&r, &mut s);
    exec(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "USB_DM", "max_skew": "0.1mm"}));
    exec(&r, &mut s, "route.diffpair", json!({}));
    let p = s.project.as_ref().unwrap();
    let before = cadlab::lengths::net_length(p, "USB_DP").total;
    let target = Nm(before.0 + 3_000_000);
    exec(
        &r,
        &mut s,
        "lengthgroup.set",
        json!({"name": "USB", "members": ["USB_D"], "target": target, "tolerance": "0.05mm"}),
    );
    let o = exec(&r, &mut s, "route.tune", json!({"group": "USB"}));
    render(&r, &mut s, "usb-tuned");
    let m = &o["output"]["groups"][0]["members"][0];
    assert_eq!(m["kind"], "pair");
    assert_eq!(m["error"], "0mm", "{m}");
    assert!(m["meanders"].as_u64().unwrap() > 0);
    let p = s.project.as_ref().unwrap();
    let d = &p.circuit().diffpairs["USB_D"];
    let cp = cadlab::lengths::coupling(p, d, Nm::from_um(150));
    assert!(cp.skew <= Nm::from_um(100), "{cp:?}");
    for sec in &cp.sections {
        assert!((sec.gap.0 - 150_000).abs() <= 3, "{sec:?}");
    }
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    assert_eq!(drc_codes(&s, "drc.diffpair"), Vec::<String>::new());
    assert_eq!(drc_codes(&s, "drc.length"), Vec::<String>::new());
}

#[test]
fn pair_definitions_and_checks() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    usb_board(&r, &mut s);
    assert_eq!(fail(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "USB_DP"})), "diffpair.same_net");
    assert_eq!(fail(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "NOPE"})), "net.not_found");
    assert_eq!(
        fail(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "USB_DM", "class": "x"})),
        "netclass.not_found"
    );
    assert_eq!(fail(&r, &mut s, "route.diffpair", json!({})), "diffpair.none");
    let o = exec(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "USB_DM", "name": "USB"}));
    assert_eq!(o["output"]["name"], "USB");
    assert_eq!(fail(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "VBUS"})), "diffpair.net_in_pair");
    // Updating keeps one pair; the suggestion skips paired nets.
    exec(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "USB_DM", "max_skew": "0.05mm"}));
    let l = exec(&r, &mut s, "diffpair.list", json!({}));
    assert_eq!(l["output"]["pairs"].as_array().unwrap().len(), 1);
    assert_eq!(l["output"]["pairs"][0]["max_skew"], "0.05mm");
    assert_eq!(exec(&r, &mut s, "diffpair.suggest", json!({}))["output"]["pairs"], json!([]));
    assert_eq!(fail(&r, &mut s, "route.diffpair", json!({"pairs": ["nope"]})), "diffpair.not_found");
    // A pair without class rules routes at the track width and clearance (with a note).
    exec(&r, &mut s, "net.set", json!({"nets": ["USB_DP", "USB_DM"], "class": ""}));
    let o = exec(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "USB_DM"}));
    assert_eq!(codes(&o), ["diffpair.no_class_rules"]);
    exec(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "USB_DM", "class": "usb"}));
    // Renaming a net follows into the pair; removing it removes the pair.
    exec(&r, &mut s, "net.rename", json!({"from": "USB_DM", "to": "USB_DN"}));
    let l = exec(&r, &mut s, "diffpair.list", json!({}));
    assert_eq!(l["output"]["pairs"][0]["n"], "USB_DN");
    // Routed pair: DRC skew check against a tight limit, gap check against a changed gap.
    exec(&r, &mut s, "route.diffpair", json!({"skew": false}));
    exec(
        &r,
        &mut s,
        "diffpair.add",
        json!({"p": "USB_DP", "n": "USB_DN", "max_skew": "0.001mm", "max_uncoupled": "1mm"}),
    );
    let p = s.project.as_ref().unwrap();
    let sk = cadlab::lengths::coupling(p, &p.circuit().diffpairs["USB"], Nm::from_um(150)).skew;
    if sk > Nm::from_um(1) {
        assert_eq!(drc_codes(&s, "drc.diffpair_skew").len(), 1);
    }
    assert_eq!(drc_codes(&s, "drc.diffpair_uncoupled").len(), 1);
    exec(&r, &mut s, "netclass.set", json!({"name": "usb", "diff_pair_gap": "0.2mm"}));
    let gaps = drc_codes(&s, "drc.diffpair_gap");
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("gap 0.15mm on F.Cu"), "{gaps:?}");
    // route.tune fixes the skew.
    exec(&r, &mut s, "netclass.set", json!({"name": "usb", "diff_pair_gap": "0.15mm"}));
    let o = exec(&r, &mut s, "route.tune", json!({}));
    assert!(mm(&o["output"]["pairs"][0]["after"]) <= 0.001, "{}", o["output"]);
    assert_eq!(drc_codes(&s, "drc.diffpair_skew"), Vec::<String>::new());
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    let o = exec(&r, &mut s, "net.remove", json!({"nets": ["USB_DN"]}));
    assert_eq!(codes(&o), ["diffpair.removed"]);
    assert!(exec(&r, &mut s, "diffpair.list", json!({}))["output"]["pairs"].as_array().unwrap().is_empty());
}

#[test]
fn identical_for_any_thread_count() {
    let r = Registry::with_builtins();
    let mut results = Vec::new();
    for threads in [1, 4] {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        let res = pool.install(|| {
            let mut s = Session::new();
            let _d = new_project(&r, &mut s);
            usb_board(&r, &mut s);
            exec(&r, &mut s, "diffpair.add", json!({"p": "USB_DP", "n": "USB_DM"}));
            let a = exec(&r, &mut s, "route.all", json!({"seed": 3}));
            exec(&r, &mut s, "lengthgroup.set", json!({"name": "U", "members": ["USB_D"], "target": "40mm"}));
            let b = exec(&r, &mut s, "route.tune", json!({"style": "accordion"}));
            let board = serde_json::to_value(s.project.as_ref().unwrap().board()).unwrap();
            (a["output"].clone(), b["output"].clone(), board)
        });
        results.push(res);
    }
    assert_eq!(results[0], results[1]);
}
