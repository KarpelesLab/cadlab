//! KiCad schematic export (M3), checked with `kicad-cli` as an external oracle (DECISIONS D7):
//! KiCad's ERC must find no errors, and the netlist KiCad extracts from our file must have the
//! same connectivity as cadlab's circuit. Oracle tests run only with `CADLAB_ORACLES=1`
//! (`CADLAB_ORACLE_KICAD_CLI=/path/to/kicad-cli` if it is not on `PATH`). Set `CADLAB_KEEP_TMP=1`
//! to keep the exported files for inspection.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cadlab::command::{Registry, RunOptions, Session};
use common::boards::build_board;
use common::oracle::{self, Oracle};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

/// The ERC-clean ATtiny85 board of `tests/circuit.rs`, in a fresh project.
fn board() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p"), "name": "attiny-blinky"}));
    build_board(&r, &mut s);
    exec(&r, &mut s, "net.set", json!({"nets": ["VBUS"], "driven": true}));
    exec(&r, &mut s, "net.no_connect", json!({"pins": ["U2.PB4"]}));
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    assert_eq!(o["output"], json!({"errors": 0, "warnings": 0}), "{:?}", o["diagnostics"]);
    (dir, r, s)
}

fn keep(dir: tempfile::TempDir) {
    if std::env::var("CADLAB_KEEP_TMP").is_ok_and(|v| v == "1") {
        eprintln!("kept {}", dir.keep().display());
    }
}

/// Minimal S-expression tree, for reading what we and KiCad write.
#[derive(Debug, Clone, PartialEq)]
enum Sx {
    Atom(String),
    List(Vec<Sx>),
}

impl Sx {
    fn head(&self) -> Option<&str> {
        match self {
            Sx::List(v) => match v.first() {
                Some(Sx::Atom(a)) => Some(a),
                _ => None,
            },
            Sx::Atom(_) => None,
        }
    }

    fn items(&self) -> &[Sx] {
        match self {
            Sx::List(v) => v,
            Sx::Atom(_) => &[],
        }
    }

    /// Children lists with the given head.
    fn all<'a>(&'a self, head: &'a str) -> impl Iterator<Item = &'a Sx> + 'a {
        self.items().iter().filter(move |c| c.head() == Some(head))
    }

    fn get(&self, head: &str) -> Option<&Sx> {
        self.items().iter().find(|c| c.head() == Some(head))
    }

    /// First atom after the head.
    fn value(&self) -> Option<&str> {
        match self.items().get(1) {
            Some(Sx::Atom(a)) => Some(a),
            _ => None,
        }
    }
}

/// Parses one S-expression (strings unquoted into atoms); panics on malformed input.
fn parse(text: &str) -> Sx {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut stack: Vec<Vec<Sx>> = Vec::new();
    let mut done: Option<Sx> = None;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '(' => stack.push(Vec::new()),
            ')' => {
                let list = Sx::List(stack.pop().expect("unbalanced `)`"));
                match stack.last_mut() {
                    Some(parent) => parent.push(list),
                    None => {
                        assert!(done.is_none(), "more than one top-level expression");
                        done = Some(list);
                    }
                }
            }
            '"' => {
                let mut s = String::new();
                i += 1;
                while chars[i] != '"' {
                    if chars[i] == '\\' {
                        i += 1;
                        s.push(match chars[i] {
                            'n' => '\n',
                            c => c,
                        });
                    } else {
                        s.push(chars[i]);
                    }
                    i += 1;
                }
                stack.last_mut().expect("string outside a list").push(Sx::Atom(s));
            }
            c if c.is_whitespace() => {}
            _ => {
                let start = i;
                while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '(' && chars[i] != ')' {
                    i += 1;
                }
                stack.last_mut().expect("atom outside a list").push(Sx::Atom(chars[start..i].iter().collect()));
                continue;
            }
        }
        i += 1;
    }
    assert!(stack.is_empty(), "unbalanced `(`");
    done.expect("empty input")
}

fn export(r: &Registry, s: &mut Session, root: &Path) -> (PathBuf, Value) {
    let path = root.join("kicad/attiny-blinky.kicad_sch");
    let o = exec(r, s, "schematic.export", json!({"path": path}));
    (path, o["output"].clone())
}

/// Not an oracle test: the export is deterministic and structurally sound.
#[test]
fn kicad_export_is_deterministic_and_well_formed() {
    let (dir, r, mut s) = board();
    let (path, out) = export(&r, &mut s, dir.path());
    assert_eq!(out["symbols"], 10);
    let text = std::fs::read_to_string(&path).unwrap();
    // Same output from a second export, and from the library function.
    exec(&r, &mut s, "schematic.export", json!({"path": "again.kicad_sch"}));
    let again = std::fs::read_to_string(dir.path().join("p/again.kicad_sch")).unwrap();
    let project = s.project.as_ref().unwrap();
    let layout = cadlab::schematic::layout(project, &Default::default());
    // Only the KiCad project name (from the file name) differs.
    assert_eq!(text.replace("\"attiny-blinky\"", "\"again\""), again.replace("\"attiny-blinky\"", "\"again\""));
    assert_eq!(cadlab::schematic::kicad::to_kicad_sch(project, &layout), text);

    let doc = parse(&text);
    assert_eq!(doc.head(), Some("kicad_sch"));
    let heads: BTreeSet<&str> = doc.items().iter().filter_map(Sx::head).collect();
    for h in [
        "version",
        "generator",
        "uuid",
        "paper",
        "title_block",
        "lib_symbols",
        "wire",
        "label",
        "symbol",
        "no_connect",
        "sheet_instances",
    ] {
        assert!(heads.contains(h), "missing ({h} ...)");
    }
    assert_eq!(doc.get("generator").unwrap().value(), Some("cadlab"));
    assert_eq!(doc.get("paper").unwrap().value(), Some("A4"));
    // Every instance refers to an embedded library symbol, and UUIDs are unique.
    let libs: BTreeSet<&str> = doc.get("lib_symbols").unwrap().all("symbol").filter_map(Sx::value).collect();
    let mut refs = BTreeSet::new();
    for sym in doc.all("symbol") {
        let lib_id = sym.get("lib_id").and_then(Sx::value).unwrap();
        assert!(libs.contains(lib_id), "{lib_id} not embedded");
        let reference = sym.all("property").find(|p| p.value() == Some("Reference")).unwrap();
        let Sx::Atom(name) = &reference.items()[2] else { panic!() };
        refs.insert(name.clone());
    }
    for r in ["C1", "C2", "C3", "D1", "J1", "J2", "R1", "R2", "U1", "U2"] {
        assert!(refs.contains(r), "{r} missing");
    }
    let mut uuids = Vec::new();
    collect_uuids(&doc, &mut uuids);
    let unique: BTreeSet<&String> = uuids.iter().collect();
    assert_eq!(unique.len(), uuids.len(), "duplicate UUIDs");
    keep(dir);
}

fn collect_uuids(x: &Sx, out: &mut Vec<String>) {
    if x.head() == Some("uuid") {
        out.push(x.value().unwrap().to_string());
    }
    for c in x.items() {
        collect_uuids(c, out);
    }
}

/// ERC warnings that are KiCad conventions rather than design problems, with the reason.
const ERC_ALLOWED: &[(&str, &str)] = &[
    (
        "lib_symbol_issues",
        "symbols are embedded in the schematic; the `cadlab`/`cadlab_power` libraries are not in KiCad's symbol \
         library table (and must not be: cadlab never ships KiCad libraries)",
    ),
    ("footprint_link_issues", "footprints are cadlab's own (`cadlab:<name>`), not in a KiCad footprint library table"),
];

/// Oracle: `kicad-cli sch erc` finds no errors and only allowlisted warnings.
#[test]
fn kicad_erc_clean() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let (dir, r, mut s) = board();
    let (path, _) = export(&r, &mut s, dir.path());
    let report = dir.path().join("erc.json");
    oracle::run(
        &cli,
        &["sch", "erc", "--format", "json", "--severity-all", "-o", report.to_str().unwrap(), path.to_str().unwrap()],
    );
    let erc: Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    let mut problems = Vec::new();
    for sheet in erc["sheets"].as_array().unwrap() {
        for v in sheet["violations"].as_array().unwrap() {
            let (sev, kind) = (v["severity"].as_str().unwrap(), v["type"].as_str().unwrap());
            if sev == "error" || !ERC_ALLOWED.iter().any(|(k, _)| *k == kind) {
                problems.push(format!("{sev} {kind}: {} {}", v["description"], v["items"]));
            }
        }
    }
    assert!(problems.is_empty(), "KiCad ERC ({}):\n{}", erc["kicad_version"], problems.join("\n"));
    keep(dir);
}

/// Oracle: the netlist KiCad extracts has exactly cadlab's nets.
#[test]
fn kicad_netlist_matches() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let (dir, r, mut s) = board();
    check_netlist(&cli, &r, &mut s, dir.path());
    keep(dir);
}

/// Oracle: rotated box symbols (fixed placements) keep their connectivity.
#[test]
fn kicad_netlist_matches_rotated_symbols() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let (dir, r, mut s) = board();
    for (refdes, at, rot) in
        [("U2", ["60mm", "120mm"], 1), ("J2", ["150mm", "120mm"], 2), ("U1", ["220mm", "120mm"], 3)]
    {
        exec(&r, &mut s, "schematic.place", json!({"refdes": refdes, "at": at, "rot": rot}));
    }
    check_netlist(&cli, &r, &mut s, dir.path());
    keep(dir);
}

fn check_netlist(cli: &Path, r: &Registry, s: &mut Session, root: &Path) {
    let (path, _) = export(r, s, root);
    let net_path = root.join("kicad.net");
    oracle::run(cli, &["sch", "export", "netlist", "-o", net_path.to_str().unwrap(), path.to_str().unwrap()]);
    let doc = parse(&std::fs::read_to_string(&net_path).unwrap());

    // KiCad: net name → (ref, pin) set, without power symbols and flags (`#` references).
    let mut kicad: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for net in doc.get("nets").unwrap().all("net") {
        let name = net.get("name").and_then(Sx::value).unwrap();
        // Local labels on the root sheet get the sheet path prefix `/`.
        let name = name.strip_prefix('/').unwrap_or(name).to_string();
        let nodes: BTreeSet<(String, String)> = net
            .all("node")
            .map(|n| {
                let r = n.get("ref").and_then(Sx::value).unwrap().to_string();
                (r, n.get("pin").and_then(Sx::value).unwrap().to_string())
            })
            .filter(|(r, _)| !r.starts_with('#'))
            .collect();
        if !nodes.is_empty() {
            kicad.insert(name, nodes);
        }
    }
    let c = s.project.as_ref().unwrap().circuit();
    let cadlab: BTreeMap<String, BTreeSet<(String, String)>> = c
        .nets
        .iter()
        .map(|(n, net)| (n.clone(), net.pins.iter().map(|p| (p.refdes.clone(), p.pin.clone())).collect()))
        .collect();
    let connected: BTreeSet<&(String, String)> = cadlab.values().flatten().collect();
    // KiCad also lists every unconnected pin as a one-node net (`unconnected-(U1-NC-Pad4)`):
    // those must be pins that are on no cadlab net.
    for (name, nodes) in &kicad {
        if !cadlab.contains_key(name) {
            assert!(
                name.starts_with("unconnected-") && nodes.len() == 1 && !nodes.iter().any(|n| connected.contains(n)),
                "KiCad net `{name}` {nodes:?} is not a cadlab net"
            );
        }
    }
    for (name, nodes) in &cadlab {
        assert_eq!(kicad.get(name), Some(nodes), "net `{name}`");
    }
}
