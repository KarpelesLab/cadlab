//! End-to-end tests of the `cadlab` binary.

use std::path::Path;
use std::process::{Command, Output};

mod common;

use common::golden::assert_golden_dir;
use serde_json::{Value, json};

fn cadlab(cwd: &Path, args: &[&str]) -> Output {
    cadlab_env(cwd, args, &[])
}

/// Runs the binary isolated from the user's configuration (no catalogs unless given).
fn cadlab_env(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cadlab"))
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", cwd.join(".no-config"))
        .env("XDG_CACHE_HOME", cwd.join(".no-cache"))
        .env("XDG_DATA_HOME", cwd.join(".no-data"))
        .env_remove("CADLAB_CATALOGS")
        .env_remove("DIGIKEY_CLIENT_ID")
        .env_remove("DIGIKEY_CLIENT_SECRET")
        .envs(env.iter().copied())
        .args(args)
        .output()
        .unwrap()
}

fn json_of(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

#[test]
fn new_then_info_json() {
    let dir = tempfile::tempdir().unwrap();
    let o = cadlab(dir.path(), &["project", "new", "demo", "--targets", "jlcpcb,pcbway", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v = json_of(&o);
    assert_eq!(v["ok"], true);
    assert_eq!(v["output"]["unsaved_changes"], false);

    let o = cadlab(&dir.path().join("demo"), &["project", "info", "--json"]);
    assert!(o.status.success());
    let v = json_of(&o);
    assert_eq!(v["output"]["name"], "demo");
    assert_eq!(v["output"]["targets"], json!(["jlcpcb", "pcbway"]));

    // -p works from anywhere.
    let o = cadlab(dir.path(), &["-p", "demo", "project", "info", "--json"]);
    assert_eq!(json_of(&o)["output"]["name"], "demo");
}

#[test]
fn new_project_matches_golden_files() {
    let dir = tempfile::tempdir().unwrap();
    let o = cadlab(dir.path(), &["project", "new", "demo", "--description", "Golden test project"]);
    assert!(o.status.success());
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/new-project");
    assert_golden_dir(&dir.path().join("demo"), &golden, &[".cadlab"]);
}

#[test]
fn set_undo_redo_across_invocations() {
    let dir = tempfile::tempdir().unwrap();
    cadlab(dir.path(), &["project", "new", "p"]);
    let p = dir.path().join("p");
    assert!(cadlab(&p, &["project", "set", "--metadata", "rev=A", "--metadata", "author=me"]).status.success());
    assert!(cadlab(&p, &["project", "set", "--metadata", "rev="]).status.success());
    let info = json_of(&cadlab(&p, &["project", "info", "--json"]));
    assert_eq!(info["output"]["metadata"], json!({"author": "me"}));

    assert!(cadlab(&p, &["undo"]).status.success());
    let info = json_of(&cadlab(&p, &["project", "info", "--json"]));
    assert_eq!(info["output"]["metadata"], json!({"author": "me", "rev": "A"}));

    assert!(cadlab(&p, &["redo"]).status.success());
    let info = json_of(&cadlab(&p, &["project", "info", "--json"]));
    assert_eq!(info["output"]["metadata"], json!({"author": "me"}));

    let hist = json_of(&cadlab(&p, &["history", "--json"]));
    assert_eq!(hist["output"]["undo"].as_array().unwrap().len(), 2);
}

#[test]
fn dry_run_does_not_write() {
    let dir = tempfile::tempdir().unwrap();
    cadlab(dir.path(), &["project", "new", "p"]);
    let p = dir.path().join("p");
    let before = std::fs::read_to_string(p.join("cadlab.toml")).unwrap();
    let o = cadlab(&p, &["project", "set", "--name", "other", "--dry-run", "--json"]);
    assert_eq!(json_of(&o)["output"]["name"], "other");
    assert_eq!(std::fs::read_to_string(p.join("cadlab.toml")).unwrap(), before);
}

#[test]
fn call_and_batch() {
    let dir = tempfile::tempdir().unwrap();
    cadlab(dir.path(), &["project", "new", "p"]);
    let p = dir.path().join("p");
    let o = cadlab(&p, &["call", "project.set", r#"{"description": "via call"}"#, "--json"]);
    assert_eq!(json_of(&o)["output"]["description"], "via call");

    std::fs::write(
        p.join("steps.jsonl"),
        "# a comment\n{\"cmd\": \"project.set\", \"args\": {\"name\": \"batched\"}}\n\n{\"cmd\": \"project.info\"}\n",
    )
    .unwrap();
    let o = cadlab(&p, &["batch", "steps.jsonl", "--json"]);
    let v = json_of(&o);
    assert_eq!(v["results"][1]["output"]["name"], "batched");

    // A failing batch changes nothing and reports the step.
    std::fs::write(p.join("bad.jsonl"), "{\"cmd\": \"project.set\", \"args\": {\"name\": \"x\"}}\n{\"cmd\": \"project.set\", \"args\": {\"bogus\": 1}}\n").unwrap();
    let o = cadlab(&p, &["batch", "bad.jsonl", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(json_of(&o)["step"], 1);
    let info = json_of(&cadlab(&p, &["project", "info", "--json"]));
    assert_eq!(info["output"]["name"], "batched");
}

#[test]
fn errors_and_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    // No project here.
    let o = cadlab(dir.path(), &["project", "info", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    let v = json_of(&o);
    assert_eq!(v["error"]["code"], "project.none");
    assert!(v["error"]["hint"].as_str().unwrap().contains("project.new"));

    // Usage errors.
    assert_eq!(cadlab(dir.path(), &["project", "nope"]).status.code(), Some(2));
    assert_eq!(cadlab(dir.path(), &["call", "project.info", "{not json"]).status.code(), Some(2));

    // Unknown command through `call` suggests the right one.
    let o = cadlab(dir.path(), &["call", "project.inf", "--json"]);
    assert_eq!(json_of(&o)["error"]["hint"], "did you mean `project.info`?");

    // Existing project.
    cadlab(dir.path(), &["project", "new", "p"]);
    let o = cadlab(dir.path(), &["project", "new", "p", "--json"]);
    assert_eq!(json_of(&o)["error"]["code"], "project.exists");
}

#[test]
fn describe_lists_commands() {
    let dir = tempfile::tempdir().unwrap();
    let v = json_of(&cadlab(dir.path(), &["describe", "--json"]));
    let names: Vec<&str> = v.as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    insta::assert_json_snapshot!("commands", names);
    let v = json_of(&cadlab(dir.path(), &["describe", "project.new", "--json"]));
    insta::assert_json_snapshot!("project_new_schema", v);
}

#[test]
fn rules_presets_and_substitutes_from_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    cadlab(dir.path(), &["project", "new", "p"]);
    let p = dir.path().join("p");
    let o = cadlab(&p, &["board", "rules", "--preset", "ipc3", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    assert_eq!(json_of(&o)["output"]["via_diameter"], "0.8mm");
    let o = cadlab(&p, &["board", "rules", "--fab", "jlcpcb", "--margin", "tightest", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    let v = json_of(&o);
    assert_eq!(v["output"]["min_track_width"], "0.1mm");
    assert_eq!(v["output"]["derived_from"], "jlcpcb two-layer");
    let o = cadlab(&p, &["board", "rules"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("IPC class 3"), "{}", String::from_utf8_lossy(&o.stdout));

    // Substitutes for a generic line from the test catalog: 1 % or better, cheapest first.
    let catalog = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/catalog.json");
    let env = [("CADLAB_CATALOGS", catalog.to_str().unwrap())];
    cadlab_env(&p, &["circuit", "add", "R 10k 1% 0402"], &env);
    let o = cadlab_env(&p, &["bom", "substitutes", "--json"], &env);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    let v = json_of(&o);
    let mpns: Vec<&str> = v["output"]["lines"][0]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["offer"]["mpn"].as_str().unwrap())
        .collect();
    assert_eq!(mpns, ["0402WGF1002TCE", "RC0402FR-0710KL"]);
    let o = cadlab_env(&p, &["fab", "substitute", "generic", "R_10k_1pct_0402", "--json"], &env);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    assert_eq!(json_of(&o)["output"]["substitution"]["mpn"], "0402WGF1002TCE");
    assert!(p.join("out/fab/generic/fab-lock.json").exists());
}

#[test]
fn catalogs_from_environment() {
    let dir = tempfile::tempdir().unwrap();
    cadlab(dir.path(), &["project", "new", "p"]);
    let p = dir.path().join("p");
    let catalog = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/catalog.json");
    let env = [("CADLAB_CATALOGS", catalog.to_str().unwrap())];
    let o = cadlab_env(
        &p,
        &["part", "search", "LDO", "--package", "SOT-23-5", "--params", "current_out=>=500mA", "--in-stock", "--json"],
        &env,
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    let v = json_of(&o);
    let mpns: Vec<&str> =
        v["output"]["candidates"].as_array().unwrap().iter().map(|c| c["mpn"].as_str().unwrap()).collect();
    assert_eq!(mpns, ["ME6211C33M5G-N", "AP2112K-3.3TRG1"]);

    // Without catalogs: a clear error.
    let o = cadlab(&p, &["part", "search", "LDO", "--json"]);
    assert_eq!(json_of(&o)["error"]["code"], "supplier.none");

    // bom check exits with 3 when checks fail (generic line without MPN).
    cadlab_env(&p, &["circuit", "add", "LED red 0603"], &env);
    let o = cadlab_env(&p, &["bom", "check"], &env);
    assert_eq!(o.status.code(), Some(3), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn user_settings_for_digikey() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_home = dir.path().join("cfg");
    let env = [("XDG_CONFIG_HOME", cfg_home.to_str().unwrap())];
    let run = |args: &[&str], stdin: &str| {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_cadlab"))
            .current_dir(dir.path())
            .envs(env.iter().copied())
            .env_remove("DIGIKEY_CLIENT_ID")
            .env_remove("DIGIKEY_CLIENT_SECRET")
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    };
    let o = run(&["config", "digikey", "--client-id", "my-id", "--no-verify", "--json"], "my-secret-value\n");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(json_of(&o)["client_secret"], "my-s…alue");
    let stored = std::fs::read_to_string(cfg_home.join("cadlab/config.toml")).unwrap();
    assert!(stored.contains("client_secret = \"my-secret-value\""));
    let o = run(&["config", "show", "--json"], "");
    let v = json_of(&o);
    assert_eq!(v["digikey"]["client_id"], "my-id");
    assert_eq!(v["digikey"]["client_secret"], "my-s…alue");
    // No secret on stdin: the stored one is kept.
    let o = run(&["config", "digikey", "--client-id", "x", "--no-verify"], "");
    assert!(o.status.success(), "keeps the stored secret when none is given");
    let o = run(&["config", "remove", "digikey"], "");
    assert!(o.status.success());
    assert_eq!(json_of(&run(&["config", "show", "--json"], ""))["digikey"], Value::Null);
}
