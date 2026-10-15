//! Agent session 4 (docs/TESTING.md, "Agent sessions"): a KiCad migration driven through the
//! MCP server (`cadlab mcp`, JSON-RPC over stdio) instead of the CLI, replayed as a regression
//! test.
//!
//! Prompt: "Take an open-source KiCad project (2 layers), import its netlist and board, inspect
//! it (summaries, DRC, BOM), make a design change (swap a part for a different package, add an
//! LED indicator with its resistor on a power rail, add a mounting hole, re-pour zones, rip up
//! and reroute the affected nets), pass DRC, and export fab outputs for two fabs plus a KiCad
//! re-export."
//!
//! Two variants:
//! - [`migration_of_a_cadlab_board`] always runs: the "KiCad project" is one cadlab writes
//!   first (board and netlist exported through MCP), migrated into a fresh project.
//! - [`migration_of_the_lumenpnp_ring_light`] runs on the corpus's `lumenpnp-ringlight`
//!   (fetched by `scripts/fetch-corpus.sh`, found through `CADLAB_CORPUS_DIR`; skipped when
//!   unset, like `tests/corpus.rs`). Its netlist comes from `kicad-cli` when oracle tests are
//!   enabled, else the circuit is built from the board.
//!
//! Every call is a `tools/call` as the agent sent it, after the session's friction was fixed:
//! the MCP-specific fixes (default actions, structured errors, hints with JSON arguments,
//! argument paths in type errors, `describe` without output schemas, DRC filters) are checked
//! along the way.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

mod common;

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    /// Starts `cadlab mcp` isolated from the user's configuration (no suppliers) and
    /// initializes the session.
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
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "session4", "version": "0"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "cadlab");
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// A request and its response line (with its size: what the agent's context pays).
    fn request_sized(&mut self, method: &str, params: Value) -> (Value, usize) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return (v, line.len());
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.request_sized(method, params).0
    }

    /// A tool call's result (successful or not).
    fn call(&mut self, tool: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        assert!(r.get("error").is_none(), "protocol error for {tool} {args}: {r}");
        r["result"].clone()
    }

    /// A tool call that must succeed: its structured content.
    fn ok(&mut self, tool: &str, args: Value) -> Value {
        let r = self.call(tool, args.clone());
        assert_eq!(r["isError"], false, "{tool} {args} failed: {}", text(&r));
        r["structuredContent"].clone()
    }

    /// A tool call that must fail: its structured error.
    fn err(&mut self, tool: &str, args: Value) -> Value {
        let r = self.call(tool, args.clone());
        assert_eq!(r["isError"], true, "{tool} {args} should fail: {}", text(&r));
        r["structuredContent"]["error"].clone()
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

/// Diagnostics of a result with this code.
fn diags<'a>(out: &'a Value, code: &str) -> Vec<&'a Value> {
    out["diagnostics"].as_array().into_iter().flatten().filter(|d| d["code"] == code).collect()
}

fn codes(out: &Value) -> Vec<String> {
    out["diagnostics"].as_array().into_iter().flatten().filter_map(|d| d["code"].as_str().map(String::from)).collect()
}

/// The first command written as `group.action {json}` in a hint: (tool, action, args), as the
/// MCP server rewrites hints.
fn hint_command(hint: &str) -> Option<(String, String, Value)> {
    for span in hint.split('`').skip(1).step_by(2) {
        let Some((name, args)) = span.split_once(' ') else { continue };
        let Ok(args) = serde_json::from_str::<Value>(args) else { continue };
        let (group, action) = name.split_once('.')?;
        return Some((group.to_string(), action.to_string(), args));
    }
    None
}

/// The MCP surface itself: tool list size and shape, default actions, error shapes, hints,
/// argument errors, `describe`.
fn check_mcp_surface(c: &mut Client) {
    let (list, size) = c.request_sized("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    // One tool per group plus describe, call and batch, small enough to keep in context.
    assert!(tools.len() < 40, "{} tools", tools.len());
    assert!(size < 64 * 1024, "tools/list is {size} bytes");
    for t in tools {
        assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "{t}");
        for (k, p) in t["inputSchema"]["properties"].as_object().unwrap() {
            assert!(p["description"].as_str().is_some(), "{}.{k} has no description", t["name"]);
        }
    }
    let drc = tools.iter().find(|t| t["name"] == "drc").unwrap();
    assert!(drc["inputSchema"].get("required").is_none(), "drc runs without `action`");
    assert!(drc["description"].as_str().unwrap().contains("Without `action`: `run`"));
    let place = tools.iter().find(|t| t["name"] == "place").unwrap();
    assert_eq!(place["inputSchema"]["required"], json!(["action"]));

    // `history` alone lists steps, as the description and the CLI say.
    c.ok("history", json!({}));
    // A missing action is an error shaped like every other: code, message, hint.
    let e = c.err("place", json!({"args": {"refdes": "U1"}}));
    assert_eq!(e["code"], "mcp.missing_action");
    assert!(e["hint"].as_str().unwrap().contains("set, move"), "{e}");
    // `group.action` as the action works too.
    c.ok("board", json!({"action": "board.info"}));
    // Unknown tools suggest the right one.
    let r = c.request("tools/call", json!({"name": "boards", "arguments": {"action": "info"}}));
    assert!(r["error"]["message"].as_str().unwrap().contains("did you mean `board`"), "{r}");
    // `describe` of a meta tool or an unknown command: structured errors with hints.
    let e = c.err("describe", json!({"command": "batch"}));
    assert_eq!(e["code"], "describe.meta_tool");
    let e = c.err("describe", json!({"command": "place.nearby"}));
    assert_eq!(e["code"], "command.unknown");
    assert!(e["hint"].as_str().unwrap().contains("place.near"), "{e}");
    // `describe` leaves the (large) output schema out unless asked, and names the tool.
    let d = c.ok("describe", json!({"command": "board.rules"}));
    assert!(d["commands"][0].get("output_schema").is_none());
    assert!(d["commands"][0]["input_schema"]["properties"]["clearance"].is_object());
    let r = c.call("describe", json!({"command": "board.rules"}));
    assert!(text(&r).contains("tool `board`, action `rules`"), "{}", text(&r));
    assert!(text(&r).contains("returns: {"), "{}", text(&r));
    let d = c.ok("describe", json!({"command": "board.rules", "output_schema": true}));
    assert!(d["commands"][0]["output_schema"].is_object());
}

/// Argument errors say which argument and why (a length without its unit).
fn check_argument_errors(c: &mut Client) {
    let e = c.err("board", json!({"action": "hole", "args": {"at": [1, 2], "drill": "3mm"}}));
    let m = e["message"].as_str().unwrap();
    assert!(m.contains("`at[0]`") && m.contains("units"), "{m}");
    let e = c.err("net", json!({"action": "set", "args": {"net": "VBUS", "driven": true}}));
    assert!(e["message"].as_str().unwrap().contains("unknown field `net`"), "{e}");
}

/// The source design: a small 2-layer board written by cadlab as a KiCad board and netlist.
fn write_source_project(c: &mut Client, dir: &Path) {
    c.ok("project", json!({"action": "new", "args": {"path": "source", "name": "blinky"}}));
    let pins = |list: &[(&str, &str, &str)]| -> Value {
        list.iter().map(|(n, name, kind)| json!({"number": n, "name": name, "kind": kind})).collect()
    };
    let steps = json!([
        {"cmd": "part.create", "args": {"id": "PWR_HDR", "category": "connector", "description": "2-pin power header",
            "package": "PinHeader 1x02", "pins": pins(&[("1", "VBUS", "passive"), ("2", "GND", "passive")])}},
        {"cmd": "part.create", "args": {"category": "ldo", "manufacturer": "Diodes", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
            "pins": pins(&[("1", "VIN", "power_in"), ("2", "GND", "power_in"), ("3", "EN", "input"), ("4", "NC", "no_connect"), ("5", "VOUT", "power_out")])}},
        {"cmd": "circuit.add", "args": {"part": "PWR_HDR", "refdes": "J1"}},
        {"cmd": "circuit.add", "args": {"part": "AP2112K-3.3TRG1", "refdes": "U1"}},
        {"cmd": "circuit.add", "args": {"part": "C 1uF 16V X5R 0805", "count": 2}},
        {"cmd": "circuit.add", "args": {"part": "R 1k 1% 0805", "refdes": "R1"}},
        {"cmd": "circuit.add", "args": {"part": "LED red 0805", "refdes": "D1"}},
        {"cmd": "net.connect", "args": {"net": "VBUS", "pins": ["J1.VBUS", "U1.VIN", "U1.EN", "C1.1"]}},
        {"cmd": "net.connect", "args": {"net": "GND", "pins": ["J1.GND", "U1.GND", "C1.2", "C2.2", "D1.K"]}},
        {"cmd": "net.connect", "args": {"net": "3V3", "pins": ["U1.VOUT", "C2.1", "R1.1"]}},
        {"cmd": "net.connect", "args": {"net": "LED_A", "pins": ["R1.2", "D1.A"]}},
        {"cmd": "net.set", "args": {"nets": ["VBUS"], "driven": true}},
        {"cmd": "board.setup", "args": {"layers": 2}},
        {"cmd": "board.outline", "args": {"width": "30mm", "height": "22mm"}},
        {"cmd": "board.rules", "args": {"fab": "jlcpcb"}},
        {"cmd": "place.auto", "args": {}},
        {"cmd": "zone.add", "args": {"name": "GND_B", "net": "GND", "layers": ["B.Cu"], "outline": "board"}},
    ]);
    c.ok("batch", json!({"steps": steps}));
    let r = c.ok("route", json!({"action": "all", "args": {"budget": "30s"}}));
    assert_eq!(r["output"]["stats"]["failed"], 0, "{r}");
    c.ok("zone", json!({"action": "fill"}));
    let d = c.ok("drc", json!({}));
    assert_eq!(d["output"]["errors"], 0, "{d}");
    c.ok("board", json!({"action": "export_kicad", "args": {"path": "../kicad/blinky.kicad_pcb"}}));
    c.ok("circuit", json!({"action": "export", "args": {"path": "../kicad/blinky.net", "format": "kicad"}}));
    assert!(dir.join("kicad/blinky.kicad_pcb").is_file() && dir.join("kicad/blinky.net").is_file());
}

/// The part of `refdes`, from `circuit.list`.
fn part_of(c: &mut Client, refdes: &str) -> String {
    let l = c.ok("circuit", json!({"action": "list"}));
    let comp = l["output"]["components"].as_array().unwrap().iter().find(|x| x["refdes"] == refdes).unwrap().clone();
    comp["part"].as_str().unwrap().to_string()
}

/// The footprint `refdes` is placed with, from `place.list` (its own choice) or the part.
fn placed_footprint(c: &mut Client, refdes: &str) -> String {
    let l = c.ok("place", json!({"action": "list"}));
    match l["output"]["placements"][refdes]["footprint"].as_str() {
        Some(f) => f.to_string(),
        None => {
            let l = c.ok("circuit", json!({"action": "list"}));
            let comp =
                l["output"]["components"].as_array().unwrap().iter().find(|x| x["refdes"] == refdes).unwrap().clone();
            comp["footprint"].as_str().unwrap().to_string()
        }
    }
}

/// Swaps `refdes` to `spec` (another package), checks the warning about its routing and
/// follows the hint's commands: rip up and reroute the nets it names.
fn swap_package(c: &mut Client, refdes: &str, spec: &str, new_footprint: &str) {
    let from = part_of(c, refdes);
    let before = placed_footprint(c, refdes);
    let r = c.ok("bom", json!({"action": "replace", "args": {"from": from, "to": spec}}));
    let w = diags(&r, "board.footprint_changed_routed");
    assert_eq!(w.len(), 1, "{r}");
    assert_eq!(placed_footprint(c, refdes), new_footprint, "the placement follows the new part (was {before})");
    // The hint's commands come with JSON arguments, ready for the `route` tool.
    let hint = w[0]["hint"].as_str().unwrap();
    let (tool, action, args) = hint_command(hint).unwrap_or_else(|| panic!("no command in {hint}"));
    assert_eq!((tool.as_str(), action.as_str()), ("route", "rip"), "{hint}");
    let nets = args["nets"].clone();
    assert!(!nets.as_array().unwrap().is_empty(), "{hint}");
    c.ok("route", json!({"action": "rip", "args": {"nets": nets}}));
    let r = c.ok("route", json!({"action": "nets", "args": {"nets": nets}}));
    assert_eq!(r["output"]["stats"]["failed"], 0, "{r}");
}

/// Routes what is left, fills the zones again, until everything is connected.
fn finish_routing(c: &mut Client, nets: &[&str]) {
    c.ok("route", json!({"action": "nets", "args": {"nets": nets}}));
    for _ in 0..3 {
        c.ok("zone", json!({"action": "fill"}));
        let s = c.ok("route", json!({"action": "status"}));
        if s["output"]["unrouted"] == 0 {
            return;
        }
        // Zone islands cut by new parts: route what the fill left open.
        c.ok("route", json!({"action": "all", "args": {"budget": "20s"}}));
    }
    let s = c.ok("route", json!({"action": "status"}));
    assert_eq!(s["output"]["unrouted"], 0, "{s}");
}

/// Exports for two fabs and the KiCad re-export; checks the files.
fn export_all(c: &mut Client, project: &Path, name: &str) {
    for fab in ["jlcpcb", "pcbway"] {
        let r = c.ok("fab", json!({"action": "export", "args": {"fab": fab, "dir": format!("out/{fab}")}}));
        let files = r["output"]["files"].as_array().unwrap();
        assert!(files.len() >= 10, "{fab}: {r}");
        assert!(project.join(format!("out/{fab}/fab-lock.json")).is_file());
    }
    let r = c.ok("board", json!({"action": "export_kicad", "args": {"path": format!("out/kicad/{name}.kicad_pcb")}}));
    assert!(project.join(format!("out/kicad/{name}.kicad_pcb")).is_file(), "{r}");
}

#[test]
fn migration_of_a_cadlab_board() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path());
    write_source_project(&mut c, dir.path());
    check_mcp_surface(&mut c);

    // Migration into a new project: netlist, then board.
    c.ok(
        "project",
        json!({"action": "new", "args": {"path": "migrated", "name": "blinky2", "targets": ["jlcpcb", "pcbway"]}}),
    );
    check_argument_errors(&mut c);
    let r = c.ok("circuit", json!({"action": "import", "args": {"path": "../kicad/blinky.net"}}));
    assert_eq!(r["output"]["components"], 6, "{r}");
    let r = c.ok("board", json!({"action": "import_kicad", "args": {"path": "../kicad/blinky.kicad_pcb"}}));
    assert_eq!(r["output"]["footprints"], 6, "{r}");
    // Inspect: summaries, BOM, ERC, DRC (errors only first).
    c.ok("circuit", json!({"action": "summary"}));
    c.ok("bom", json!({"action": "list"}));
    c.ok("board", json!({"action": "info"}));
    let erc = c.ok("circuit", json!({"action": "erc"}));
    // The netlist does not carry "driven": the hint says how, in JSON, and works as written.
    let undriven = diags(&erc, "erc.power_not_driven");
    assert_eq!(undriven.len(), 1, "{erc}");
    let (tool, action, args) = hint_command(undriven[0]["hint"].as_str().unwrap()).unwrap();
    assert_eq!((tool.as_str(), action.as_str(), &args), ("net", "set", &json!({"nets": ["VBUS"], "driven": true})));
    c.ok(&tool, json!({"action": action, "args": args}));
    let d = c.ok("drc", json!({"args": {"severity": "error"}}));
    assert_eq!(d["output"]["errors"], 0, "{d}");

    // The change: R1 from 0805 to 0603, rip up and reroute its nets.
    swap_package(&mut c, "R1", "R 1k 1% 0603", "RESC1608X55N");
    // A power LED on VBUS with its resistor, a mounting hole, placed and routed around the
    // existing copper.
    for spec in ["LED green 0603", "R 2.2k 1% 0603"] {
        c.ok("part", json!({"action": "generic", "args": {"spec": spec}}));
    }
    c.ok("circuit", json!({"action": "add", "args": {"part": "LED_green_0603", "refdes": "D2"}}));
    c.ok("circuit", json!({"action": "add", "args": {"part": "R_2.2k_1pct_0603", "refdes": "R2"}}));
    c.ok("net", json!({"action": "connect", "args": {"net": "VBUS", "pins": ["R2.1"]}}));
    c.ok("net", json!({"action": "connect", "args": {"net": "PWR_LED", "pins": ["R2.2", "D2.A"]}}));
    c.ok("net", json!({"action": "connect", "args": {"net": "GND", "pins": ["D2.K"]}}));
    let erc = c.ok("circuit", json!({"action": "erc", "args": {"lint": true}}));
    assert!(codes(&erc).iter().all(|c| !c.starts_with("erc.")), "{erc}");
    c.ok("board", json!({"action": "hole", "args": {"at": ["2.5mm", "2.5mm"], "drill": "2.2mm", "name": "MH1"}}));
    c.ok("place", json!({"action": "auto"}));
    let d =
        c.ok("drc", json!({"args": {"codes": ["drc.clearance", "drc.short", "drc.hole_clearance", "drc.courtyard"]}}));
    assert_eq!(d["output"]["listed"].as_u64().unwrap_or(0), 0, "new parts sit clear of the existing copper: {d}");
    finish_routing(&mut c, &["PWR_LED", "VBUS", "GND"]);

    // DRC: clean; filters and counts for long reports; waivers.
    let d = c.ok("drc", json!({}));
    assert_eq!(d["output"]["errors"], 0, "{d}");
    let d = c.ok("drc", json!({"args": {"codes": ["drc.clearance"], "limit": 1}}));
    assert!(d["output"]["listed"].as_u64().unwrap_or(0) <= 1, "{d}");
    // A waiver matching nothing says so.
    c.ok("drc", json!({"action": "waive", "args": {"code": "drc.footprint_outside", "subjects": ["J1"], "reason": "edge connector"}}));
    let d = c.ok("drc", json!({}));
    assert_eq!(diags(&d, "drc.waiver_unused").len(), 1, "{d}");
    c.ok("drc", json!({"action": "unwaive", "args": {"code": "drc.footprint_outside"}}));
    let e = c.err("drc", json!({"action": "waive", "args": {"code": "drc.clearance", "reason": " "}}));
    assert_eq!(e["code"], "drc.waiver_reason");

    // `net:` subjects, as findings print them, highlight in renders.
    c.ok(
        "render",
        json!({"action": "board", "args": {"path": "out/gnd.png", "highlight": ["net:GND", "U1.1"], "px_per_mm": 10}}),
    );

    export_all(&mut c, &dir.path().join("migrated"), "blinky2");
    // Undo history: one step per change, as the agent made them.
    let h = c.ok("history", json!({}));
    assert!(h["output"]["undo"].as_array().unwrap().len() > 10, "{h}");
}

/// The lumenpnp ring light from the corpus (skipped without `CADLAB_CORPUS_DIR`).
#[test]
fn migration_of_the_lumenpnp_ring_light() {
    let Some(root) = std::env::var_os("CADLAB_CORPUS_DIR").map(PathBuf::from) else {
        eprintln!("skipping: set CADLAB_CORPUS_DIR to the directory filled by scripts/fetch-corpus.sh");
        return;
    };
    let src = root.join("lumenpnp-ringlight/pnp/pcb/ring-light");
    if !src.join("ringLight.kicad_pcb").is_file() {
        eprintln!("skipping: lumenpnp-ringlight is not in {}", root.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path());
    c.ok(
        "project",
        json!({"action": "new", "args": {"path": "ring", "name": "ringlight", "targets": ["jlcpcb", "pcbway"]}}),
    );

    // The netlist from the schematic (kicad-cli, an oracle), else the circuit from the board.
    let netlist = common::oracle::require(common::oracle::Oracle::KicadCli).map(|cli| {
        let net = dir.path().join("ringLight.net");
        let sch = src.join("ringLight.kicad_sch");
        common::oracle::run(&cli, &["sch", "export", "netlist", "-o", net.to_str().unwrap(), sch.to_str().unwrap()]);
        net
    });
    if let Some(net) = &netlist {
        let r = c.ok("circuit", json!({"action": "import", "args": {"path": net}}));
        // One placeholder warning per part, not per component.
        let fid: Vec<&Value> = diags(&r, "import.placeholder_part")
            .into_iter()
            .filter(|d| d["message"].as_str().unwrap().contains("Fiducial"))
            .collect();
        assert_eq!(fid.len(), 1, "{r}");
    }
    let pcb = src.join("ringLight.kicad_pcb");
    let r = c.ok("board", json!({"action": "import_kicad", "args": {"path": pcb}}));
    // Fiducials (symbols without pins) neither lose pads nor stay placeholders once the board
    // gave them their footprint.
    let cs = codes(&r);
    assert!(!cs.contains(&"import.pad_missing".to_string()), "{cs:?}");
    assert!(!cs.contains(&"import.placeholder_part".to_string()), "{cs:?}");
    let erc = c.ok("circuit", json!({"action": "erc"}));
    assert!(!diags(&erc, "erc.unconnected").iter().any(|d| d["message"].as_str().unwrap().contains("FID")), "{erc}");

    // Inspect. The kibuzzard labels are filled silkscreen polygons: no zero-width lines.
    c.ok("circuit", json!({"action": "summary"}));
    c.ok("bom", json!({"action": "list"}));
    let d = c.ok("drc", json!({"args": {"codes": ["fab.silk_width"]}}));
    assert!(!d.to_string().contains(" is 0mm wide"), "{d}");
    // The connectors' courtyards reach over the tab's edge, by how much is in the message.
    let d = c.ok("drc", json!({"args": {"severity": "error"}}));
    let outside = diags(&d, "drc.footprint_outside");
    assert_eq!(outside.len(), 2, "{d}");
    assert!(outside.iter().all(|x| x["message"].as_str().unwrap().contains("by up to")));
    // The design's own copper-to-edge is 0.025 mm; the fabs want more.
    c.ok("board", json!({"action": "rules", "args": {"copper_to_edge": "0.3mm", "silk_to_pad": "0.15mm", "min_silk_width": "0.15mm"}}));

    // The schematic has C8 where the board has C10: place it where C10 was.
    if netlist.is_some() {
        c.ok(
            "place",
            json!({"action": "set", "args": {"refdes": "C8", "at": ["28.671751mm", "34.971751mm"], "rotation": 225}}),
        );
    }
    swap_package(&mut c, "R1", "R 330 1% 0603", "RESC1608X55N");

    // Power LED on +5V, a mounting hole, placed clear of the existing routing.
    c.ok("part", json!({"action": "generic", "args": {"spec": "LED red 0603"}}));
    c.ok("part", json!({"action": "generic", "args": {"spec": "R 1k 1% 0603"}}));
    c.ok("circuit", json!({"action": "add", "args": {"part": "LED_red_0603", "refdes": "D9"}}));
    c.ok("circuit", json!({"action": "add", "args": {"part": "R_1k_1pct_0603", "refdes": "R2"}}));
    c.ok("net", json!({"action": "connect", "args": {"net": "+5V", "pins": ["R2.1"]}}));
    c.ok("net", json!({"action": "connect", "args": {"net": "PWR_LED", "pins": ["R2.2", "D9.A"]}}));
    c.ok("net", json!({"action": "connect", "args": {"net": "GND", "pins": ["D9.K"]}}));
    let erc = c.ok("circuit", json!({"action": "erc"}));
    if let Some(d) = diags(&erc, "erc.power_not_driven").first() {
        let (tool, action, args) = hint_command(d["hint"].as_str().unwrap()).unwrap();
        c.ok(&tool, json!({"action": action, "args": args}));
    }
    c.ok("board", json!({"action": "hole", "args": {"at": ["8.9mm", "24.6mm"], "drill": "2.2mm", "name": "MH1"}}));
    c.ok("place", json!({"action": "auto"}));
    let d = c.ok("drc", json!({"args": {"codes": ["drc.clearance", "drc.short", "drc.hole_clearance"]}}));
    assert_eq!(d["output"]["listed"].as_u64().unwrap_or(0), 0, "new parts sit clear of the existing copper: {d}");
    finish_routing(&mut c, &["PWR_LED", "+5V", "GND"]);

    // DRC: the connectors over the edge are as designed; waive them (and the silkscreen the
    // fab clips) with reasons, and the check passes.
    let d = c.ok("drc", json!({"args": {"severity": "error"}}));
    let errors: Vec<String> = codes(&d);
    assert!(errors.iter().all(|c| c == "drc.footprint_outside"), "{d}");
    for r in ["J1", "J2"] {
        let hint = diags(&d, "drc.footprint_outside")
            .into_iter()
            .find(|x| x["subjects"].as_array().unwrap().contains(&json!(r)))
            .unwrap()["hint"]
            .as_str()
            .unwrap()
            .to_string();
        let (tool, action, mut args) = hint_command(&hint).unwrap();
        assert_eq!((tool.as_str(), action.as_str()), ("drc", "waive"), "{hint}");
        args["reason"] = json!("JST XH connector flush with the tab edge, as in the original design");
        c.ok("drc", json!({"action": "waive", "args": args}));
    }
    for code in ["fab.silk_width", "drc.silk_over_pad", "fab.silk_to_pad"] {
        c.ok(
            "drc",
            json!({"action": "waive", "args": {"code": code, "reason": "original design; the fab clips or thins it"}}),
        );
    }
    let d = c.ok("drc", json!({}));
    assert_eq!((d["output"]["errors"].clone(), d["output"]["warnings"].clone()), (json!(0), json!(0)), "{d}");
    assert!(d["output"]["waived"].as_u64().unwrap() >= 2, "{d}");

    let project = dir.path().join("ring");
    export_all(&mut c, &project, "ringlight");
    // KiCad reads the re-export: no unconnected pads after refilling, no shorts or clearance
    // errors.
    if let Some(cli) = common::oracle::optional(common::oracle::Oracle::KicadCli) {
        let report = project.join("out/kicad/drc.json");
        common::oracle::run(
            &cli,
            &[
                "pcb",
                "drc",
                "--refill-zones",
                "--format",
                "json",
                "-o",
                report.to_str().unwrap(),
                project.join("out/kicad/ringlight.kicad_pcb").to_str().unwrap(),
            ],
        );
        let r: Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
        assert_eq!(r["unconnected_items"].as_array().map_or(0, Vec::len), 0, "{r}");
        let bad: Vec<&Value> = r["violations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| matches!(v["type"].as_str(), Some("clearance" | "shorting_items" | "hole_clearance")))
            .collect();
        assert!(bad.is_empty(), "{bad:#?}");
    }
}
