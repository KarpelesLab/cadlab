//! Agent session 6 (docs/TESTING.md, "Agent sessions"): a 2.4 GHz wireless sensor node, the
//! electrical analysis side, designed through the MCP server (`cadlab mcp`, JSON-RPC over stdio)
//! and replayed as a regression test.
//!
//! Prompt: "A 2.4 GHz wireless sensor node: module-less RF SoC with its crystal, the matching
//! network into a 50 Ω microstrip feeding a U.FL connector, a battery input with a buck converter,
//! an I2C sensor and SWD, on four layers. Use the electrical tools as a designer would: stackup and
//! impedance targets, the field solver, net loss, `sim.sparams` on the RF feed with the matching
//! parts, Touchstone, openEMS export, then DRC and fab outputs."
//!
//! Part data and where it comes from (read 2026-11-01):
//! - nRF52832-QFAA: pin table (Nordic nRF52832 Product Specification, docs.nordicsemi.com "Pin
//!   assignments"), QFN48 6 x 6 mm, e 0.4, D2/E2 4.6 ("Mechanical specifications"), reference circuit
//!   and BOM (PS v1.0 chapter 53 figure 167 and table 139, internal LDO setup): ANT, shunt C3 0.8 pF,
//!   series L1 3.9 nH, RF. The land pattern is cadlab's IPC-7351B one, not Nordic's reference layout.
//!   The specification states no load impedance for ANT, so no match can be claimed here.
//! - TPS62205: pins, 2.5 V fixed, 2.7 to 6 V in, 4.7 µF / 10 µH / 10 µF (TI SLVS417F).
//! - BME280: pins, I2C strapping, landing pattern (Bosch BST-BME280-DS002-15, fig. 21, table 35).
//! - U.FL-R-SMT-1: recommended PCB mounting pattern (Hirose U.FL series catalog), read from its
//!   drawing; the signal pad's offset is our reading of it.
//! - Not from a datasheet: the 32 MHz crystal is a generic 2016 part per Nordic's BOM values (no MPN,
//!   so the land pattern is generated and its pad pitch is ours), and generic passives have no ESL
//!   or Q (`sim.parts_ideal` says so). Stackup εr is JLCPCB's published value; the loss tangent 0.02
//!   at 1 GHz is a nominal FR-4 value we chose, not the fab's.
//!
//! Friction fixed (DECISIONS D70): mesh slivers, `sim.sparams --plan`, part values per simulation,
//! results at the frequency of interest with impedance and VSWR, passivity and reciprocity checks,
//! notes on what a port and an ideal part mean, openEMS export warns about its missing parts,
//! `net.loss` with solder mask, controlled impedance lines in `fab-lock.json`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

mod common;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent_session6")
}

fn fixture(name: &str) -> Value {
    let p = fixtures().join(name);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    /// Starts `cadlab mcp` isolated from the user's configuration (no suppliers).
    fn start(cwd: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cadlab"))
            .arg("mcp")
            .current_dir(cwd)
            .env("XDG_CONFIG_HOME", cwd.join(".no-config"))
            .env("XDG_CACHE_HOME", cwd.join(".no-cache"))
            .env("XDG_DATA_HOME", cwd.join(".no-data"))
            .env_remove("CADLAB_CATALOGS")
            .env_remove("DIGIKEY_CLIENT_ID")
            .env_remove("DIGIKEY_CLIENT_SECRET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Client { child, stdin, stdout, next_id: 1 };
        let init = c.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "session6", "version": "0"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "cadlab");
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        assert!(r.get("error").is_none(), "protocol error for {tool} {args}: {r}");
        r["result"].clone()
    }

    /// `tool` with `action` and `args`, which must succeed: the structured content.
    fn ok(&mut self, tool: &str, action: &str, args: Value) -> Value {
        let r = self.call(tool, json!({"action": action, "args": args}));
        assert_eq!(r["isError"], false, "{tool}.{action} {args} failed: {}", text(&r));
        r["structuredContent"].clone()
    }

    /// `tool` with `action` and `args`, which must fail: the structured error.
    fn err(&mut self, tool: &str, action: &str, args: Value) -> Value {
        let r = self.call(tool, json!({"action": action, "args": args}));
        assert_eq!(r["isError"], true, "{tool}.{action} {args} should fail: {}", text(&r));
        r["structuredContent"]["error"].clone()
    }

    /// Several commands as one transaction.
    fn batch(&mut self, steps: Vec<Value>) -> Value {
        let r = self.call("batch", json!({"steps": steps}));
        assert_eq!(r["isError"], false, "batch failed: {}", text(&r));
        r["structuredContent"].clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(r: &Value) -> String {
    r["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")
}

fn diags<'a>(out: &'a Value, code: &str) -> Vec<&'a Value> {
    out["diagnostics"].as_array().into_iter().flatten().filter(|d| d["code"] == code).collect()
}

fn pins(list: &[(&str, &str, &str)]) -> Value {
    list.iter().map(|(n, name, kind)| json!({"number": n, "name": name, "kind": kind})).collect()
}

fn step(cmd: &str, args: Value) -> Value {
    json!({"cmd": cmd, "args": args})
}

fn mm(v: &Value) -> f64 {
    v.as_str().unwrap().trim_end_matches("mm").parse::<f64>().unwrap()
}

fn parts(c: &mut Client) {
    let mut steps = vec![
        step(
            "part.create",
            json!({"category": "mcu", "mpn": "nRF52832-QFAA", "manufacturer": "Nordic Semiconductor",
                "description": "2.4 GHz BLE SoC, Cortex-M4F", "package": "QFN-48 6x6mm P0.4mm EP4.6mm",
                "pins": fixture("nrf52832_pins.json")}),
        ),
        step(
            "part.create",
            json!({"category": "regulator", "mpn": "TPS62205DBV", "manufacturer": "TI",
                "description": "2.5 V 300 mA step-down converter", "package": "SOT-23-5",
                "pins": pins(&[("1", "VI", "power_in"), ("2", "GND", "power_in"), ("3", "EN", "input"), ("4", "FB", "input"), ("5", "SW", "power_out")])}),
        ),
        step(
            "footprint.create",
            json!({"name": "BME280_LGA8", "pads": fixture("bme280_pads.json"), "body": {"width": "2.5mm", "length": "2.5mm", "height": "0.93mm"}}),
        ),
        step(
            "part.create",
            json!({"category": "ic", "mpn": "BME280", "manufacturer": "Bosch Sensortec",
                "description": "Pressure, humidity, temperature; I2C", "footprint": "BME280_LGA8",
                "pins": pins(&[("1", "GND", "power_in"), ("2", "CSB", "input"), ("3", "SDI", "bidirectional"), ("4", "SCK", "input"),
                    ("5", "SDO", "bidirectional"), ("6", "VDDIO", "power_in"), ("7", "GND", "power_in"), ("8", "VDD", "power_in")])}),
        ),
        step(
            "footprint.create",
            json!({"name": "UFL_R_SMT1", "pads": fixture("ufl_r_smt1_pads.json"), "body": {"width": "2.6mm", "length": "3.1mm", "height": "1.25mm"}}),
        ),
        step(
            "part.create",
            json!({"category": "connector", "mpn": "U.FL-R-SMT-1", "manufacturer": "Hirose", "description": "U.FL coaxial receptacle",
                "footprint": "UFL_R_SMT1", "pins": pins(&[("1", "SIG", "passive"), ("2", "GND", "passive"), ("3", "GND", "passive")])}),
        ),
        step(
            "part.create",
            json!({"category": "crystal", "id": "XTAL_32MHz_2016", "description": "32 MHz crystal, CL 8 pF, 2016 (per Nordic's BOM; no MPN)",
                "params": {"frequency": "32MHz"}, "package": "DFN-4 2.0x1.6mm P1.2mm",
                "pins": pins(&[("1", "X1", "passive"), ("2", "GND", "passive"), ("3", "X2", "passive"), ("4", "GND", "passive")])}),
        ),
        step(
            "part.create",
            json!({"category": "connector", "id": "SWD_2x05_1.27", "description": "Cortex debug header", "package": "PinHeader 2x05 P1.27mm",
                "pins": pins(&[("1", "VTREF", "passive"), ("2", "SWDIO", "passive"), ("3", "GND", "passive"), ("4", "SWCLK", "passive"), ("5", "GND", "passive"),
                    ("6", "SWO", "passive"), ("7", "KEY", "no_connect"), ("8", "NC", "no_connect"), ("9", "GNDD", "passive"), ("10", "NRST", "passive")])}),
        ),
        step(
            "part.create",
            json!({"category": "connector", "id": "BATT_1x02", "description": "Li-Po battery header", "package": "PinHeader 1x02 P2.54mm",
                "pins": pins(&[("1", "BAT+", "passive"), ("2", "BAT-", "passive")])}),
        ),
    ];
    for spec in [
        "C 12pF 50V C0G 0402",
        "C 0.8pF 50V C0G 0402",
        "C 100nF 16V X7R 0402",
        "C 100pF 50V C0G 0402",
        "C 4.7uF 16V X5R 0603",
        "C 1uF 16V X7R 0603",
        "C 10uF 16V X5R 0805",
        "L 3.9nH 0402",
        "L 10uH 0805",
        "R 4.7k 1% 0402",
    ] {
        steps.push(step("part.generic", json!({"spec": spec})));
    }
    c.batch(steps);
    // A capacitor below 1 pF keeps the unit a BOM uses (it was `C_800fF_...`, 800fF in the BOM).
    let s = c.ok("part", "show", json!({"id": "C_0.8pF_50V_C0G_0402"}));
    assert_eq!(s["output"]["part"]["params"]["capacitance"], "0.8pF", "{s}");
}

fn circuit(c: &mut Client) {
    let mut steps = Vec::new();
    for (part, r) in [
        ("nRF52832-QFAA", "U1"),
        ("TPS62205DBV", "U2"),
        ("BME280", "U3"),
        ("U.FL-R-SMT-1", "J1"),
        ("BATT_1x02", "J2"),
        ("SWD_2x05_1.27", "J3"),
        ("XTAL_32MHz_2016", "X1"),
        ("L_3.9nH_0402", "L1"),
        ("L_10uH_0805", "L2"),
        ("C_12pF_50V_C0G_0402", "C1"),
        ("C_12pF_50V_C0G_0402", "C2"),
        ("C_0.8pF_50V_C0G_0402", "C3"),
        ("C_100nF_16V_X7R_0402", "C4"),
        ("C_100nF_16V_X7R_0402", "C5"),
        ("C_100pF_50V_C0G_0402", "C7"),
        ("C_100nF_16V_X7R_0402", "C8"),
        ("C_4.7uF_16V_X5R_0603", "C9"),
        ("C_1uF_16V_X7R_0603", "C10"),
        ("C_4.7uF_16V_X5R_0603", "C20"),
        ("C_10uF_16V_X5R_0805", "C21"),
        ("C_100nF_16V_X7R_0402", "C22"),
        ("C_100nF_16V_X7R_0402", "C23"),
        ("R_4.7k_1pct_0402", "R1"),
        ("R_4.7k_1pct_0402", "R2"),
    ] {
        steps.push(step("circuit.add", json!({"part": part, "refdes": r})));
    }
    c.batch(steps);
    #[rustfmt::skip]
    let nets: &[(&str, &[&str])] = &[
        ("VBAT", &["J2.BAT+", "U2.VI", "U2.EN", "C20.1"]),
        ("SW", &["U2.SW", "L2.1"]),
        ("VDD", &["L2.2", "U2.FB", "C21.1", "U1.13", "U1.36", "U1.48", "C5.1", "C8.1", "C9.1", "U3.VDD", "U3.VDDIO", "U3.CSB",
            "C22.1", "C23.1", "R1.1", "R2.1", "J3.VTREF"]),
        ("GND", &["J2.BAT-", "U2.GND", "C20.2", "C21.2", "U1.31", "U1.45", "U1.49", "C1.2", "C2.2", "C3.2", "C4.2", "C5.2", "C7.2",
            "C8.2", "C9.2", "C10.2", "C22.2", "C23.2", "U3.1", "U3.7", "U3.SDO", "J1.2", "J1.3", "J3.3", "J3.5", "J3.GNDD", "X1.2", "X1.4"]),
        ("DEC1", &["U1.DEC1", "C4.1"]), ("DEC3", &["U1.DEC3", "C7.1"]), ("DEC4", &["U1.DEC4", "C10.1"]),
        ("XC1", &["U1.XC1", "X1.X1", "C1.1"]), ("XC2", &["U1.XC2", "X1.X2", "C2.1"]),
        ("ANT", &["U1.ANT", "C3.1", "L1.1"]), ("RF_FEED", &["L1.2", "J1.SIG"]),
        ("I2C_SDA", &["U1.P0.26", "U3.SDI", "R1.2"]), ("I2C_SCL", &["U1.P0.27", "U3.SCK", "R2.2"]),
        ("SWDIO", &["U1.SWDIO", "J3.SWDIO"]), ("SWCLK", &["U1.SWDCLK", "J3.SWCLK"]), ("SWO", &["U1.P0.18", "J3.SWO"]),
        ("NRST", &["U1.P0.21", "J3.NRST"]),
    ];
    let mut steps: Vec<Value> = nets.iter().map(|(n, p)| step("net.connect", json!({"net": n, "pins": p}))).collect();
    #[rustfmt::skip]
    let nc = ["U1.DEC2", "U1.DCC", "J3.KEY", "J3.NC", "U1.P0.00", "U1.P0.01", "U1.P0.02", "U1.P0.03", "U1.P0.04", "U1.P0.05", "U1.P0.06",
        "U1.P0.07", "U1.P0.08", "U1.P0.09", "U1.P0.10", "U1.P0.11", "U1.P0.12", "U1.P0.13", "U1.P0.14", "U1.P0.15", "U1.P0.16",
        "U1.P0.17", "U1.P0.19", "U1.P0.20", "U1.P0.22", "U1.P0.23", "U1.P0.24", "U1.P0.25", "U1.P0.28", "U1.P0.29", "U1.P0.30", "U1.P0.31"];
    steps.push(step("net.no_connect", json!({"pins": nc})));
    steps.push(step("net.set", json!({"nets": ["VBAT"], "driven": true, "voltage": "3.7V"})));
    steps.push(step("net.set", json!({"nets": ["VDD"], "voltage": "2.5V"})));
    c.batch(steps);
    let e = c.ok("circuit", "erc", json!({"lint": true}));
    assert_eq!((e["output"]["errors"].clone(), e["output"]["warnings"].clone()), (json!(0), json!(0)), "{e}");
}

/// Stackup, impedance, width of the RF feed.
fn stackup_and_width(c: &mut Client) {
    c.batch(vec![
        step("board.setup", json!({"layers": 4})),
        step("board.outline", json!({"width": "40mm", "height": "25mm", "corner_radius": "1.5mm"})),
        step("board.rules", json!({"fab": "jlcpcb"})),
    ]);
    let list = c.ok("board", "fab_stackup", json!({"fab": "jlcpcb"}));
    assert!(list.to_string().contains("JLC04161H-7628"), "{list}");
    c.ok("board", "fab_stackup", json!({"fab": "jlcpcb", "stackup": "JLC04161H-7628"}));

    // Closed form against field solver on the bare line: the two agree.
    let bare = c.ok("impedance", "solve", json!({"target": "50ohm", "layer": "F.Cu"}));
    let field = c.ok("impedance", "solve", json!({"target": "50ohm", "layer": "F.Cu", "method": "field_solver"}));
    let (b, f) = (mm(&bare["output"]["width"]), mm(&field["output"]["width"]));
    assert!((b - f).abs() < 0.005, "closed form {b}, field solver {f}");
    // The mask needs the field solver and narrows the line by what designers expect.
    let r = c.call(
        "impedance",
        json!({"action": "solve", "args": {"target": "50ohm", "layer": "F.Cu", "mask": "25um", "netclass": "RF50"}}),
    );
    assert_eq!(r["isError"], false, "{}", text(&r));
    assert_eq!(r["structuredContent"]["output"]["width"], "0.347mm", "{r}");
    assert!(text(&r).contains("net.set <nets> --class RF50"), "says how to use the class: {}", text(&r));
    // A coplanar solve says so: it used to read as a plain microstrip.
    let r = c.call(
        "impedance",
        json!({"action": "solve", "args": {"target": "50ohm", "layer": "F.Cu", "mask": "25um", "coplanar_gap": "0.25mm"}}),
    );
    assert!(text(&r).contains("coplanar grounds at 0.25mm"), "{}", text(&r));
    // The assumed loss tangent reads as a number, not as "20m".
    let l = c.ok("impedance", "calc", json!({"width": "0.347mm", "layer": "F.Cu", "freq": ["2.44GHz"]}));
    let w = diags(&l, "impedance.loss_tangent_assumed");
    assert!(w[0]["message"].as_str().unwrap().contains("assuming 0.02 "), "{l}");
    // Nominal FR-4 value, chosen by us: JLCPCB publishes εr but not tan δ for this stackup.
    c.ok("board", "dielectric", json!({"loss_tangent": "0.02", "ref_freq": "1GHz"}));
}

/// Board: the RF path placed and routed by hand, the rest automatic.
fn layout(c: &mut Client) {
    c.ok("net", "set", json!({"nets": ["ANT", "RF_FEED"], "class": "RF50"}));
    c.ok("place", "set", json!({"refdes": "U1", "at": ["18mm", "13mm"]}));
    c.ok("place", "set", json!({"refdes": "C3", "at": ["22.2mm", "12.3475mm"], "rotation": 270}));
    c.ok("place", "set", json!({"refdes": "L1", "at": ["23.9mm", "12.8mm"]}));
    c.ok("place", "set", json!({"refdes": "J1", "at": ["35.025mm", "12.8mm"], "rotation": 270}));
    c.ok("place", "lock", json!({"refdes": ["U1", "C3", "L1", "J1"]}));
    c.ok("track", "add", json!({"layer": "F.Cu", "points": ["U1.30", "C3.1", "L1.1"]}));
    c.ok("track", "add", json!({"layer": "F.Cu", "points": ["L1.2", "J1.1"]}));
    c.ok("track", "add", json!({"layer": "F.Cu", "points": ["C3.2", ["22.2mm", "11.2mm"]]}));
    c.ok("via", "add", json!({"at": ["22.2mm", "11.2mm"], "net": "GND"}));
    c.ok("via", "add", json!({"at": ["35.025mm", "9.9mm"], "net": "GND"}));
    c.ok("via", "add", json!({"at": ["35.025mm", "15.7mm"], "net": "GND"}));
    c.ok(
        "track",
        "add",
        json!({"layer": "F.Cu", "points": ["J1.2", ["35.025mm", "14.275mm"], ["35.025mm", "15.7mm"]]}),
    );
    c.ok("track", "add", json!({"layer": "F.Cu", "points": ["J1.3", ["35.025mm", "11.325mm"], ["35.025mm", "9.9mm"]]}));
    // One keep-out around the feed, which the feed net itself may cross (D76; the session had to
    // draw two rectangles, one each side of the track).
    let k = c.ok(
        "keepout",
        "add",
        json!({"name": "RF", "outline": {"rect": {"from": ["24.9mm", "10.8mm"], "to": ["32.7mm", "14.8mm"]}},
            "layers": ["F.Cu"], "no_tracks": true, "no_vias": true, "no_pours": true, "no_footprints": true,
            "allow_nets": ["RF_FEED"]}),
    );
    assert_eq!(k["output"]["keepouts"][0]["allow_nets"], json!(["RF_FEED"]), "{k}");
    c.ok("place", "hint", json!({"refdes": ["J2"], "edge": "left"}));
    c.ok("place", "hint", json!({"refdes": ["J3"], "edge": "bottom"}));
    let pl = c.ok("place", "auto", json!({}));
    assert!(pl["output"]["unplaced"].as_array().is_none_or(Vec::is_empty), "{pl}");
    for (n, net, layer) in [("GND_IN1", "GND", "In1.Cu"), ("VDD_IN2", "VDD", "In2.Cu"), ("GND_B", "GND", "B.Cu")] {
        c.ok("zone", "add", json!({"name": n, "net": net, "layers": [layer], "outline": "board"}));
    }
    let r = c.ok("route", "all", json!({"budget": "60s"}));
    assert_eq!(r["output"]["stats"]["failed"], 0, "{r}");
    c.ok("zone", "fill", json!({}));
    let s = c.ok("route", "status", json!({}));
    assert_eq!(s["output"]["unrouted"], 0, "{s}");
    let d = c.call("drc", json!({}));
    assert_eq!(d["structuredContent"]["output"]["errors"], 0, "{}", text(&d));
}

/// The same setup a designer ends up with: small region, thin absorbing layer, a mesh whose
/// timestep is not set by slivers.
fn sim_args(extra: Value) -> Value {
    let mut a = json!({"f_min": "1GHz", "f_max": "6GHz", "nets": ["ANT", "RF_FEED"], "only_selected": true,
        "ports": [{"at": "U1.30"}, {"at": "J1.1"}], "margin": "3mm", "copper": "sheet", "max_cell": "0.4mm",
        "min_cell": "50um", "air": "3mm", "pml_cells": 6});
    for (k, v) in extra.as_object().unwrap() {
        a[k] = v.clone();
    }
    a
}

fn analysis(c: &mut Client, project: &Path) {
    // Net loss: the mask adds what the field solver says, and is stated like everywhere else.
    let bare = c.ok("net", "loss", json!({"net": "RF_FEED", "freq": ["2.44GHz"]}));
    let masked = c.ok("net", "loss", json!({"net": "RF_FEED", "freq": ["2.44GHz"], "mask": "25um"}));
    let db =
        |v: &Value| v["output"]["points"][0]["loss"].as_str().unwrap().trim_end_matches("dB").parse::<f64>().unwrap();
    assert!((0.07..0.12).contains(&db(&bare)), "{bare}");
    assert!(db(&masked) > db(&bare), "{bare} {masked}");
    assert_eq!(masked["output"]["method"], "field_solver", "{masked}");

    // The first attempt a designer makes: plan it. Margin of 2 mm, default mesh.
    let first = c.ok(
        "sim",
        "sparams",
        json!({"plan": true, "f_min": "1GHz", "f_max": "6GHz", "nets": ["ANT", "RF_FEED"], "only_selected": true,
            "ports": [{"at": "U1.30"}, {"at": "J1.1"}], "margin": "2mm"}),
    );
    assert_eq!(first["output"]["planned"], true, "{first}");
    assert!(first["output"]["runs"].as_array().unwrap().is_empty());
    assert!(!project.join("out/sim").exists(), "a plan writes nothing");
    // The warning says how much margin is missing.
    let w = diags(&first, "openems.port_in_pml");
    assert_eq!(w.len(), 1, "{first}");
    assert!(w[0]["hint"].as_str().unwrap().contains("grow `margin` by at least"), "{first}");
    // Merged mesh lines keep the minimum spacing (a 5 µm sliver in x made the timestep 4× too small).
    let min_cell = &first["output"]["mesh"]["min_cell"];
    let largest = mm(&first["output"]["mesh"]["max_cell"]);
    assert!(mm(&min_cell[0]) >= largest / 20.0 - 1e-6, "{min_cell}");
    assert!(mm(&min_cell[1]) >= largest / 20.0 - 1e-6, "{min_cell}");

    let plan = c.ok("sim", "sparams", sim_args(json!({"plan": true})));
    // Parts on the RF nets are lumped elements, those that merely touch a kept ground are a note.
    let modeled: Vec<&str> =
        plan["output"]["parts"].as_array().unwrap().iter().map(|q| q["refdes"].as_str().unwrap()).collect();
    assert_eq!(modeled, ["C3", "L1"], "{plan}");
    assert!(diags(&plan, "sim.part_not_modeled").is_empty(), "{plan}");
    assert_eq!(diags(&plan, "sim.parts_outside_selection").len(), 1, "{plan}");
    // What the result will not mean is said before the run.
    let ideal = diags(&plan, "sim.parts_ideal");
    assert!(ideal[0]["message"].as_str().unwrap().contains("C3, L1"), "{plan}");
    let ports = diags(&plan, "sim.port_replaces_part");
    assert_eq!(ports.len(), 2, "{plan}");
    assert!(ports[0]["message"].as_str().unwrap().contains("U1 itself is not simulated"), "{plan}");
    // Reference plane under the feed is the GND pour on In1.
    let p0 = &plan["output"]["ports"][0];
    assert_eq!((p0["layer"].as_str(), p0["reference"].as_str()), (Some("F.Cu"), Some("In1.Cu")), "{plan}");
    assert!(plan["output"]["mesh"]["cells"].as_u64().unwrap() < 300_000, "{plan}");
    let est = &plan["output"]["estimate"];
    assert!(est["pulse_steps"].as_u64().unwrap() > 1000, "{plan}");
    assert!(est["thread_minutes"].as_f64().unwrap() > 0.0, "{plan}");

    // Bad requests fail before any run.
    let e = c.err("sim", "sparams", sim_args(json!({"part_values": {"C33": {"capacitance": "1pF"}}})));
    assert_eq!(e["code"], "sim.unknown_part", "{e}");
    assert!(e["hint"].as_str().unwrap().contains("C3"), "did you mean: {e}");
    let e = c.err("sim", "sparams", sim_args(json!({"part_values": {"C3": {"capacitance": "1nH"}}})));
    assert_eq!(e["code"], "value.wrong_unit", "{e}");
    let e = c.err("sim", "sparams", sim_args(json!({"part_values": {"C3": {"q": "40"}}})));
    assert_eq!(e["code"], "sim.bad_part_value", "{e}");
    let e = c.err("sim", "sparams", sim_args(json!({"frequencies": ["9GHz"]})));
    assert_eq!(e["code"], "touchstone.out_of_band", "{e}");
    let e = c.err("sim", "sparams", sim_args(json!({"band": ["2.5GHz", "2.4GHz"]})));
    assert_eq!(e["code"], "touchstone.out_of_band", "{e}");

    // An override that does nothing is said so.
    let p = c.ok("sim", "sparams", sim_args(json!({"plan": true, "part_values": {"R1": {"resistance": "1k"}}})));
    assert_eq!(diags(&p, "sim.override_unused").len(), 1, "{p}");

    // A run truncated inside the pulse says the result means nothing (it used to read
    // "decayed to -inf dB"), and carries its provenance. Twice: byte-identical.
    let short = sim_args(json!({"timesteps": 30, "points": 5, "name": "short", "frequencies": ["2.44GHz"],
        "band": ["2.4GHz", "2.4835GHz"], "part_values": {"C3": {"capacitance": "1pF"}}}));
    let r = c.ok("sim", "sparams", short.clone());
    assert_eq!(diags(&r, "sim.stopped_in_pulse").len(), 2, "{r}");
    assert!(diags(&r, "sim.not_converged").is_empty(), "{r}");
    let path = project.join("out/sim/short.s2p");
    let first = std::fs::read_to_string(&path).unwrap();
    assert!(first.contains("! port 1: U1.30 on F.Cu over In1.Cu, 50Ω"), "{first}");
    assert!(first.contains("! lumped C3: 1pF (part_values)"), "{first}");
    assert!(first.contains("! lumped L1: 3.9nH\n"), "{first}");
    c.ok("sim", "sparams", short);
    assert_eq!(first, std::fs::read_to_string(&path).unwrap(), "same run, same bytes");
    // The reader skips the header.
    c.ok("touchstone", "summary", json!({"path": "out/sim/short.s2p"}));

    // openEMS: same region, the matching parts as lumped elements (D74).
    let x = c.ok(
        "export",
        "openems",
        json!({"name": "rf", "f_min": "1GHz", "f_max": "6GHz", "nets": ["ANT", "RF_FEED"], "only_selected": true,
            "margin": "3mm", "max_cell": "0.4mm", "air": "3mm", "ports": [{"at": "U1.30"}, {"at": "J1.1"}], "copper": "sheet"}),
    );
    let xml = std::fs::read_to_string(project.join("out/openems/rf.xml")).unwrap();
    assert_eq!(xml.matches("<LumpedElement").count(), 2, "{x}");
    assert!(project.join("out/openems/rf.py").exists());
}

/// A Touchstone file of the ideal network, computed here: the summary must read what the circuit
/// says. Shunt C and series L between 50 Ω lines, no line loss.
#[test]
fn touchstone_summary_reads_an_ideal_network() {
    let (s11, s21) = ideal_chain(2.44e9, 0.8e-12, 3.9e-9);
    let dir = tempfile::tempdir().unwrap();
    let mut body = String::from("! ideal\n# Hz S RI R 50\n");
    for f in [1.0e9, 2.4e9, 2.44e9, 2.4835e9, 6.0e9] {
        let (a, b) = ideal_chain(f, 0.8e-12, 3.9e-9);
        body += &format!("{f} {} {} {} {} {} {} {} {}\n", a.0, a.1, b.0, b.1, b.0, b.1, a.0, a.1);
    }
    std::fs::write(dir.path().join("ideal.s2p"), body).unwrap();
    let mut c = Client::start(dir.path());
    let r = c.ok(
        "touchstone",
        "summary",
        json!({"path": dir.path().join("ideal.s2p"), "frequencies": ["2.44GHz"], "band": ["2.4GHz", "2.4835GHz"]}),
    );
    let o = &r["output"];
    let param = |n: &str| o["params"].as_array().unwrap().iter().find(|p| p["name"] == n).unwrap().clone();
    let (s11_p, s21_p) = (param("S11"), param("S21"));
    let at = &s11_p["at"][0];
    let mag = (s11.0 * s11.0 + s11.1 * s11.1).sqrt();
    assert!((at["mag"].as_f64().unwrap() - mag).abs() < 1e-4, "{at}");
    // Impedance looking in, from Γ by hand.
    let d = (1.0 - s11.0).powi(2) + s11.1 * s11.1;
    let (zr, zi) = (50.0 * (1.0 - mag * mag) / d, 50.0 * 2.0 * s11.1 / d);
    assert!((at["reflection"]["z_re"].as_f64().unwrap() - zr).abs() < 0.02, "{at} {zr}");
    assert!((at["reflection"]["z_im"].as_f64().unwrap() - zi).abs() < 0.02, "{at} {zi}");
    assert!((at["reflection"]["vswr"].as_f64().unwrap() - (1.0 + mag) / (1.0 - mag)).abs() < 0.01, "{at}");
    // Lossless network: everything |S21| loses is the mismatch.
    let tx = &s21_p["at"][0];
    let m21 = (s21.0 * s21.0 + s21.1 * s21.1).sqrt();
    assert!((tx["mag"].as_f64().unwrap() - m21).abs() < 1e-4, "{tx}");
    assert!(tx["available_loss_db"].as_f64().unwrap().abs() < 0.01, "{tx}");
    // The extremes stay in the band of interest.
    assert_eq!(o["over"], json!([2.4e9, 2.4835e9]), "{o}");
    assert!(s11_p["max"]["freq"].as_f64().unwrap() <= 2.4835e9, "{s11_p}");
    assert!(o["checks"]["max_power"].as_f64().unwrap() <= 1.0001, "{o}");
    assert_eq!(o["checks"]["max_asymmetry"], 0.0, "{o}");
    assert!(diags(&r, "touchstone.not_passive").is_empty(), "{r}");
    let t = c.call(
        "touchstone",
        json!({"action": "summary", "args": {"path": dir.path().join("ideal.s2p"), "frequencies": ["2.44GHz"]}}),
    );
    assert!(text(&t).contains("VSWR") && text(&t).contains("without the mismatch"), "{}", text(&t));
}

/// (S11, S21) of: 1.2675 mm line, shunt C, 1.2475 mm line, series L, 9.1475 mm line, on a 50 Ω line
/// of εeff `ER_EFF`, lossless (the RF path of this board between its ports' reference planes),
/// as (re, im) pairs.
fn ideal_chain(f: f64, cap: f64, ind: f64) -> ((f64, f64), (f64, f64)) {
    chain(f, cap, ind, 0.0)
}

/// The field solver's εeff of the bare 0.347 mm line (Hammerstad-Jensen agrees within 0.2 %).
const ER_EFF: f64 = 3.19;

type C = (f64, f64);
fn mul(a: C, b: C) -> C {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}
fn add(a: C, b: C) -> C {
    (a.0 + b.0, a.1 + b.1)
}
fn div(a: C, b: C) -> C {
    let d = b.0 * b.0 + b.1 * b.1;
    ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
}
type Abcd = [C; 4];
fn cascade(x: Abcd, y: Abcd) -> Abcd {
    [
        add(mul(x[0], y[0]), mul(x[1], y[2])),
        add(mul(x[0], y[1]), mul(x[1], y[3])),
        add(mul(x[2], y[0]), mul(x[3], y[2])),
        add(mul(x[2], y[1]), mul(x[3], y[3])),
    ]
}
/// A 50 Ω line of `len` meters with `db_per_m` attenuation.
fn line(f: f64, len: f64, db_per_m: f64) -> Abcd {
    let beta = 2.0 * std::f64::consts::PI * f * ER_EFF.sqrt() / 299_792_458.0 * len;
    let alpha = db_per_m / 8.685_9 * len;
    let (ch, sh) = (
        (alpha.cosh() * beta.cos(), alpha.sinh() * beta.sin()),
        (alpha.sinh() * beta.cos(), alpha.cosh() * beta.sin()),
    );
    [ch, (sh.0 * 50.0, sh.1 * 50.0), (sh.0 / 50.0, sh.1 / 50.0), ch]
}
fn chain(f: f64, cap: f64, ind: f64, db_per_m: f64) -> (C, C) {
    let w = 2.0 * std::f64::consts::PI * f;
    let one = (1.0, 0.0);
    let zero = (0.0, 0.0);
    let shunt: Abcd = [one, zero, (0.0, w * cap), one];
    let series: Abcd = [one, (0.0, w * ind), zero, one];
    let m = cascade(line(f, 1.2675e-3, db_per_m), shunt);
    let m = cascade(m, line(f, 1.2475e-3, db_per_m));
    let m = cascade(m, series);
    let m = cascade(m, line(f, 9.1475e-3, db_per_m));
    let (a, b, c, d) = (m[0], (m[1].0 / 50.0, m[1].1 / 50.0), (m[2].0 * 50.0, m[2].1 * 50.0), m[3]);
    let den = add(add(a, b), add(c, d));
    let s11 = div(add(add(a, b), (-c.0 - d.0, -c.1 - d.1)), den);
    let s21 = div((2.0, 0.0), den);
    (s11, s21)
}

fn outputs(c: &mut Client, project: &Path) {
    let r = c.ok("fab", "export", json!({"fab": "jlcpcb", "dir": "out/jlcpcb"}));
    assert_eq!(r["output"]["stackup"], "JLC04161H-7628", "{r}");
    assert!(diags(&r, "fab.impedance_without_stackup").is_empty(), "{r}");
    // The order states the controlled line.
    let line = &r["output"]["impedance"][0];
    assert_eq!(
        (line["target"].as_str(), line["layer"].as_str(), line["width"].as_str()),
        (Some("50Ω"), Some("F.Cu"), Some("0.347mm")),
        "{r}"
    );
    assert_eq!(line["nets"], json!(["ANT", "RF_FEED"]), "{r}");
    let lock: Value =
        serde_json::from_str(&std::fs::read_to_string(project.join("out/jlcpcb/fab-lock.json")).unwrap()).unwrap();
    assert_eq!(lock["process"]["impedance"][0]["class"], "RF50", "{lock}");
    let bom = std::fs::read_to_string(
        std::fs::read_dir(project.join("out/jlcpcb"))
            .unwrap()
            .flatten()
            .find(|f| f.file_name().to_string_lossy().contains("BOM"))
            .unwrap()
            .path(),
    )
    .unwrap();
    assert!(bom.contains("0.8pF") && !bom.contains("800fF"), "{bom}");
    c.ok("render", "board", json!({"path": "out/board.png", "px_per_mm": 8}));
    c.ok("render", "schematic", json!({"path": "out/schematic.svg"}));
}

#[test]
fn wireless_node_through_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path());
    c.ok("project", "new", json!({"path": "node", "name": "ble-sensor-node", "targets": ["jlcpcb"]}));
    let project = dir.path().join("node");
    parts(&mut c);
    circuit(&mut c);
    stackup_and_width(&mut c);
    layout(&mut c);
    // The legend reads (D82): `place auto` moved the reference designators off the board edge,
    // the pads and each other; none had to be left out.
    let board = cadlab::model::Project::load(&project).unwrap();
    common::silk::assert_legible(&board);
    assert_eq!(common::silk::hidden(&board), Vec::<String>::new());
    analysis(&mut c, &project);
    outputs(&mut c, &project);
}

/// The full-wave run: `cargo test --release --test agent_session6 -- --ignored`. Minutes per run.
///
/// Compared with the ideal chain of `chain`: the layout's own pads and gaps are the difference, so
/// agreement bounds the setup, not the RF performance of a real board: the parts here are ideal,
/// the nRF52832 and the connector are not simulated, and nothing is measured.
#[test]
#[ignore]
fn rf_path_full_wave() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path());
    c.ok("project", "new", json!({"path": "node", "name": "ble-sensor-node", "targets": ["jlcpcb"]}));
    parts(&mut c);
    circuit(&mut c);
    stackup_and_width(&mut c);
    layout(&mut c);
    let r = c.ok(
        "sim",
        "sparams",
        sim_args(json!({"name": "rf", "points": 101, "end_criteria": "50dB", "frequencies": ["2.44GHz"],
            "band": ["2.4GHz", "2.4835GHz"]})),
    );
    assert!(r["output"]["runs"].as_array().unwrap().iter().all(|x| x["converged"] == true), "{r}");
    assert!(diags(&r, "sim.not_passive").is_empty() && diags(&r, "sim.not_reciprocal").is_empty(), "{r}");
    let param = |n: &str| r["output"]["params"].as_array().unwrap().iter().find(|p| p["name"] == n).unwrap().clone();
    let (s11, s21) = chain(2.44e9, 0.8e-12, 3.9e-9, 10.6);
    let abs = |c: (f64, f64)| (c.0 * c.0 + c.1 * c.1).sqrt();
    let got11 = param("S11")["at"][0]["mag"].as_f64().unwrap();
    let got21 = param("S21")["at"][0]["mag"].as_f64().unwrap();
    eprintln!("|S11| {got11} (ideal chain {}), |S21| {got21} (ideal chain {})", abs(s11), abs(s21));
    assert!((got21 - abs(s21)).abs() < S21_TOLERANCE, "|S21| {got21}, ideal chain {}", abs(s21));
    assert!((got11 - abs(s11)).abs() < S11_TOLERANCE, "|S11| {got11}, ideal chain {}", abs(s11));
}

/// Set from the session's run (docs/TESTING.md, session 6).
const S21_TOLERANCE: f64 = 0.1;
const S11_TOLERANCE: f64 = 0.15;
