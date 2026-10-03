//! Shared user libraries (`lib.*`): publish from one project, import into another.
//!
//! In-process tests inject the library directories through `Session::libraries`; the binary test
//! isolates the environment (XDG directories) instead.

use std::path::{Path, PathBuf};
use std::process::Command;

use cadlab::command::{Failure, Registry, RunOptions, Session, Step};
use cadlab::userlib::{Libraries, UserLibrary};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {} ({:?})", f.error, f.error.diagnostic.hint),
    }
}

fn fail(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Failure {
    r.execute(s, cmd, args, RunOptions::default()).expect_err(cmd)
}

fn libs(root: &Path) -> Libraries {
    Libraries::new(vec![UserLibrary::new("user", root.join("user")), UserLibrary::new("team", root.join("team"))])
}

fn project(r: &Registry, root: &Path, name: &str) -> Session {
    let mut s = Session::new();
    s.libraries = Some(libs(root));
    exec(r, &mut s, "project.new", json!({"path": root.join(name), "name": name}));
    s
}

fn changes(o: &Value) -> Vec<String> {
    o["output"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            format!("{} {} {}", i["kind"].as_str().unwrap(), i["name"].as_str().unwrap(), i["change"].as_str().unwrap())
        })
        .collect()
}

fn pins(list: &[(&str, &str, &str)]) -> Value {
    Value::Array(list.iter().map(|(n, name, kind)| json!({"number": n, "name": name, "kind": kind})).collect())
}

fn batch(r: &Registry, s: &mut Session, steps: Vec<Value>) {
    let steps: Vec<Step> = serde_json::from_value(Value::Array(steps)).unwrap();
    r.execute_batch(s, steps, RunOptions::default()).unwrap_or_else(|f| panic!("step {:?}: {}", f.step, f.error));
}

fn attiny(r: &Registry, s: &mut Session) {
    batch(
        r,
        s,
        vec![
            json!({"cmd": "part.create", "args": {"category": "mcu", "manufacturer": "Microchip", "mpn": "ATTINY85-20SU", "package": "SOIC-8",
        "pins": pins(&[("1", "PB5", "bidirectional"), ("2", "PB3", "bidirectional"), ("3", "PB4", "bidirectional"), ("4", "GND", "power_in"),
                       ("5", "PB0", "bidirectional"), ("6", "PB1", "bidirectional"), ("7", "PB2", "bidirectional"), ("8", "VCC", "power_in")])}}),
        ],
    );
}

#[test]
fn publish_part_then_import_into_new_project() {
    let r = Registry::with_builtins();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut a = project(&r, root, "a");
    attiny(&r, &mut a);
    let part_id = exec(&r, &mut a, "part.show", json!({"id": "ATTINY85-20SU"}))["output"]["part"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let fp = exec(&r, &mut a, "part.show", json!({"id": part_id}))["output"]["part"]["footprints"][0]["footprint"]
        .as_str()
        .unwrap()
        .to_string();

    // Dry run writes nothing.
    let o = r.execute(&mut a, "lib.publish", json!({"part": "ATTINY85-20SU"}), RunOptions { dry_run: true }).unwrap();
    assert!(o.summary.contains("dry run"), "{}", o.summary);
    assert!(!root.join("user").exists());

    // Publishing a part brings its footprint.
    let o = exec(&r, &mut a, "lib.publish", json!({"part": "ATTINY85-20SU"}));
    assert_eq!(changes(&o), [format!("part {part_id} added"), format!("footprint {fp} added")]);
    assert!(root.join(format!("user/parts/{part_id}.json")).is_file());
    assert!(root.join(format!("user/footprints/{fp}.json")).is_file());
    assert!(root.join("user/library.toml").is_file());
    let o = exec(&r, &mut a, "lib.publish", json!({"part": part_id}));
    assert_eq!(changes(&o), [format!("part {part_id} unchanged"), format!("footprint {fp} unchanged")]);

    // Listing and searching need no project.
    let mut bare = Session::new();
    bare.libraries = Some(libs(root));
    let o = exec(&r, &mut bare, "lib.list", json!({}));
    assert_eq!(o["output"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(o["output"]["libraries"][1]["exists"], false);
    let o = exec(&r, &mut bare, "lib.list", json!({"query": "microchip", "kind": "part"}));
    assert_eq!(o["output"]["items"][0]["name"], part_id.as_str());
    assert_eq!(o["output"]["items"][0]["library"], "user");
    let o = exec(&r, &mut bare, "lib.show", json!({"name": fp}));
    assert_eq!(o["output"]["kind"], "footprint");
    assert_eq!(fail(&r, &mut bare, "lib.publish", json!({"part": "x"})).error.diagnostic.code, "project.none");

    // A fresh project does not see the library implicitly, but the error says how to import.
    let mut b = project(&r, root, "b");
    let f = fail(&r, &mut b, "circuit.add", json!({"part": "ATTINY85-20SU"}));
    assert_eq!(f.error.diagnostic.code, "part.not_found");
    let hint = f.error.diagnostic.hint.clone().unwrap();
    assert!(hint.contains(&format!("lib.import {part_id}")), "{hint}");
    let f = fail(&r, &mut b, "part.show", json!({"id": part_id}));
    assert!(f.error.diagnostic.hint.unwrap().contains("lib.import"));

    // Import by MPN: part and footprint come in, then circuit.add works.
    let o = exec(&r, &mut b, "lib.import", json!({"name": "attiny85-20su"}));
    assert_eq!(changes(&o), [format!("part {part_id} added"), format!("footprint {fp} added")]);
    let o = exec(&r, &mut b, "circuit.add", json!({"part": part_id}));
    assert_eq!(o["output"]["refdes"], json!(["U1"]));
    let o = exec(&r, &mut b, "lib.import", json!({"name": part_id}));
    assert_eq!(changes(&o), [format!("part {part_id} unchanged"), format!("footprint {fp} unchanged")]);
    assert!(!o["changed"].as_bool().unwrap());

    // The project stays self-contained on disk.
    b.save().unwrap();
    assert!(root.join(format!("b/library/parts/{part_id}.json")).is_file());
    assert!(root.join(format!("b/library/footprints/{fp}.json")).is_file());

    // Conflicts: the project's copy changes; publishing needs `replace`.
    let original = exec(&r, &mut b, "part.show", json!({"id": part_id}))["output"]["part"]["description"].clone();
    exec(&r, &mut a, "part.set", json!({"id": part_id, "description": "8-bit AVR"}));
    let f = fail(&r, &mut a, "lib.publish", json!({"part": part_id}));
    assert_eq!(f.error.diagnostic.code, "lib.conflict");
    assert!(f.error.diagnostic.message.contains(&format!("part {part_id}")), "{}", f.error.diagnostic.message);
    let o = exec(&r, &mut a, "lib.publish", json!({"part": part_id, "replace": true}));
    assert_eq!(changes(&o)[0], format!("part {part_id} replaced"));

    // ... and importing the new version into b needs `replace` too.
    let f = fail(&r, &mut b, "lib.import", json!({"name": part_id}));
    assert_eq!(f.error.diagnostic.code, "lib.conflict");
    let o = exec(&r, &mut b, "lib.import", json!({"name": part_id, "replace": true}));
    assert_eq!(changes(&o)[0], format!("part {part_id} replaced"));
    assert!(o["diagnostics"].as_array().unwrap().iter().any(|d| d["code"] == "lib.replaced_in_use"));
    assert_eq!(exec(&r, &mut b, "part.show", json!({"id": part_id}))["output"]["part"]["description"], "8-bit AVR");
    // Undo restores the previous project copy; the library is untouched by undo.
    exec(&r, &mut b, "history.undo", json!({}));
    assert_eq!(exec(&r, &mut b, "part.show", json!({"id": part_id}))["output"]["part"]["description"], original);

    // Another library, by name; a footprint used by a library part cannot be removed.
    let o = exec(&r, &mut a, "lib.publish", json!({"footprint": fp, "library": "team"}));
    assert_eq!(changes(&o), [format!("footprint {fp} added")]);
    let f = fail(&r, &mut bare, "lib.remove", json!({"name": fp}));
    assert_eq!(f.error.diagnostic.code, "lib.in_use");
    exec(&r, &mut bare, "lib.remove", json!({"name": fp, "library": "team"}));
    let f = fail(&r, &mut bare, "lib.show", json!({"name": "nope", "library": "tema"}));
    assert_eq!(f.error.diagnostic.code, "lib.unknown_library");
    let f = fail(&r, &mut bare, "lib.import", json!({"name": "ATTINY85-20SV"}));
    assert_eq!(f.error.diagnostic.code, "project.none");
    let f = fail(&r, &mut b, "lib.import", json!({"name": format!("{part_id}x")}));
    assert_eq!(f.error.diagnostic.code, "lib.not_found");
}

/// Board pieces from the ATtiny85 board of `tests/circuit.rs`: the LDO stage.
fn ldo_stage(r: &Registry, s: &mut Session) {
    batch(
        r,
        s,
        vec![
            json!({"cmd": "part.create", "args": {"category": "ldo", "manufacturer": "Diodes", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
                "pins": pins(&[("1", "VIN", "power_in"), ("2", "GND", "power_in"), ("3", "EN", "input"), ("4", "NC", "no_connect"), ("5", "VOUT", "power_out")])}}),
            json!({"cmd": "circuit.add", "args": {"part": "AP2112K-3.3TRG1"}}),
            json!({"cmd": "circuit.add", "args": {"part": "C 1uF 16V X5R 0402", "count": 2}}),
            json!({"cmd": "net.connect", "args": {"net": "VBUS", "pins": ["U1.VIN", "U1.EN", "C1.1"]}}),
            json!({"cmd": "net.connect", "args": {"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2"]}}),
            json!({"cmd": "net.connect", "args": {"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}}),
            json!({"cmd": "net.no_connect", "args": {"pins": ["U1.NC"]}}),
            json!({"cmd": "block.create", "args": {"name": "ldo_3v3", "components": ["U1", "C1", "C2"],
                "ports": ["VBUS", "GND", "3V3"], "description": "3.3 V LDO with input and output caps"}}),
        ],
    );
}

#[test]
fn publish_block_then_instantiate_in_fresh_project() {
    let r = Registry::with_builtins();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut a = project(&r, root, "a");
    ldo_stage(&r, &mut a);

    let o = exec(&r, &mut a, "lib.publish", json!({"block": "ldo_3v3"}));
    assert_eq!(changes(&o), ["block ldo_3v3 added"]);
    // The block file carries its parts and footprints; nothing else is written.
    let file = root.join("user/blocks/ldo_3v3.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(v["name"], "ldo_3v3");
    assert_eq!(v["parts"].as_object().unwrap().len(), 2);
    assert_eq!(v["footprints"].as_object().unwrap().len(), 2);
    assert!(!root.join("user/parts").exists());

    // Fresh project: import, instantiate, ERC sees connected pins.
    let mut c = project(&r, root, "c");
    let o = exec(&r, &mut c, "lib.import", json!({"name": "ldo_3v3", "kind": "block"}));
    let ch = changes(&o);
    assert_eq!(ch[0], "block ldo_3v3 added");
    assert_eq!(ch.len(), 5, "{ch:?}");
    assert!(ch.iter().skip(1).all(|c| c.ends_with(" added")));
    let o = exec(
        &r,
        &mut c,
        "block.instantiate",
        json!({"block": "ldo_3v3", "instance": "PWR", "connect": {"3V3": "VCC"}}),
    );
    assert_eq!(o["output"]["components"].as_object().unwrap().len(), 3);
    assert_eq!(o["output"]["nets"]["3V3"], "VCC");
    let o = exec(&r, &mut c, "net.list", json!({}));
    let text = o["summary"].as_str().unwrap();
    assert!(text.contains("VCC") && text.contains("VBUS"), "{text}");
    c.save().unwrap();
    let (c2, _) = Session::open(&root.join("c")).unwrap();
    assert_eq!(c2.project, c.project);

    // Conflicts on blocks: a different block of the same name in the project.
    exec(&r, &mut c, "block.create", json!({"name": "other", "components": ["C1"]}));
    let mut d = project(&r, root, "d");
    ldo_stage(&r, &mut d);
    exec(&r, &mut d, "block.remove", json!({"name": "ldo_3v3"}));
    exec(&r, &mut d, "block.create", json!({"name": "ldo_3v3", "components": ["U1", "C1"]}));
    let f = fail(&r, &mut d, "lib.import", json!({"name": "ldo_3v3"}));
    assert_eq!(f.error.diagnostic.code, "lib.conflict");
    assert!(f.error.diagnostic.message.contains("block ldo_3v3"));
    let f = fail(&r, &mut d, "lib.publish", json!({"block": "ldo_3v3"}));
    assert_eq!(f.error.diagnostic.code, "lib.conflict");
    exec(&r, &mut d, "lib.import", json!({"name": "ldo_3v3", "replace": true}));
    assert_eq!(d.project.as_ref().unwrap().circuit().blocks["ldo_3v3"].components.len(), 3);

    // Remove from the library.
    let mut bare = Session::new();
    bare.libraries = Some(libs(root));
    let o = r.execute(&mut bare, "lib.remove", json!({"name": "ldo_3v3"}), RunOptions { dry_run: true }).unwrap();
    assert!(o.summary.contains("dry run"));
    assert!(file.is_file());
    exec(&r, &mut bare, "lib.remove", json!({"name": "ldo_3v3"}));
    assert!(!file.exists());
    assert_eq!(exec(&r, &mut bare, "lib.list", json!({}))["output"]["items"], json!([]));
}

/// Runs the binary in `cwd` with every XDG directory under `home`.
fn cadlab(home: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cadlab"))
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env_remove("CADLAB_CATALOGS")
        .env_remove("DIGIKEY_CLIENT_ID")
        .env_remove("DIGIKEY_CLIENT_SECRET")
        .args(args)
        .output()
        .unwrap()
}

fn json_ok(o: &std::process::Output) -> Value {
    assert!(o.status.success(), "{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

/// The binary finds the user library under XDG_DATA_HOME and extra libraries from the settings.
#[test]
fn binary_uses_xdg_data_home_and_settings() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    let extra: PathBuf = cwd.join("shared/team-lib");
    std::fs::create_dir_all(cwd.join("config/cadlab")).unwrap();
    std::fs::write(cwd.join("config/cadlab/config.toml"), format!("libraries = [{:?}]\n", extra.display().to_string()))
        .unwrap();

    json_ok(&cadlab(cwd, cwd, &["project", "new", "p", "--json"]));
    let p = cwd.join("p");
    json_ok(&cadlab(cwd, &p, &["part", "generic", "R 10k 1% 0402", "--json"]));
    let v = json_ok(&cadlab(cwd, &p, &["lib", "publish", "--part", "R_10k_1pct_0402", "--json"]));
    assert_eq!(v["output"]["library"], "user", "{v}");
    assert!(cwd.join("data/cadlab/library/parts/R_10k_1pct_0402.json").is_file());
    let v =
        json_ok(&cadlab(cwd, &p, &["lib", "publish", "--part", "R_10k_1pct_0402", "--library", "team-lib", "--json"]));
    assert_eq!(v["output"]["path"], extra.display().to_string());

    // Outside any project.
    let v = json_ok(&cadlab(cwd, cwd, &["lib", "list", "10k", "--json"]));
    let libs: Vec<&str> =
        v["output"]["libraries"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert_eq!(libs, ["user", "team-lib"]);
    assert_eq!(v["output"]["items"].as_array().unwrap().len(), 2);

    json_ok(&cadlab(cwd, cwd, &["project", "new", "q", "--json"]));
    let q = cwd.join("q");
    let o = cadlab(cwd, &q, &["circuit", "add", "R_10k_1pct_0402", "--json"]);
    assert!(!o.status.success());
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["error"]["hint"].as_str().unwrap().contains("lib.import R_10k_1pct_0402"), "{v}");
    let v = json_ok(&cadlab(cwd, &q, &["lib", "import", "R_10k_1pct_0402", "--json"]));
    assert_eq!(v["output"]["items"][0]["change"], "added");
    let v = json_ok(&cadlab(cwd, &q, &["circuit", "add", "R_10k_1pct_0402", "--json"]));
    assert_eq!(v["output"]["refdes"], json!(["R1"]));
}
