use cadlab::command::{ErrorKind, Registry, RunOptions, Session, Step, run};
use cadlab::commands::project;
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => o.output,
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn new_project(r: &Registry) -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    let out = exec(
        r,
        &mut s,
        "project.new",
        json!({"path": dir.path().join("demo"), "targets": ["JLCPCB", "pcbway", "jlcpcb"]}),
    );
    assert_eq!(out["name"], "demo");
    assert_eq!(out["targets"], json!(["jlcpcb", "pcbway"]));
    s.save().unwrap();
    (dir, s)
}

#[test]
fn create_set_undo_redo_persisted() {
    let r = Registry::with_builtins();
    let (dir, mut s) = new_project(&r);
    let root = dir.path().join("demo");

    exec(&r, &mut s, "project.set", json!({"description": "first"}));
    exec(&r, &mut s, "project.set", json!({"metadata": {"rev": "A"}}));
    s.save().unwrap();

    // A new session (as the next CLI invocation would) sees the history.
    let (mut s2, warnings) = Session::open(&root).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(s2.history.undo_len(), 2);
    let out = exec(&r, &mut s2, "history.undo", json!({}));
    assert_eq!(out["steps"], json!(["project.set"]));
    assert_eq!(s2.project.as_ref().unwrap().manifest().metadata.get("rev"), None);
    assert_eq!(s2.project.as_ref().unwrap().manifest().description.as_deref(), Some("first"));
    s2.save().unwrap();

    let (mut s3, _) = Session::open(&root).unwrap();
    assert_eq!((s3.history.undo_len(), s3.history.redo_len()), (1, 1));
    exec(&r, &mut s3, "history.redo", json!({}));
    assert_eq!(s3.project.as_ref().unwrap().manifest().metadata.get("rev").map(String::as_str), Some("A"));

    // A new change clears redo.
    exec(&r, &mut s3, "history.undo", json!({"steps": 5}));
    assert_eq!(s3.history.undo_len(), 0);
    exec(&r, &mut s3, "project.set", json!({"name": "renamed"}));
    assert_eq!(s3.history.redo_len(), 0);

    let e = r.execute(&mut s3, "history.redo", json!({}), RunOptions::default()).unwrap_err();
    assert_eq!(e.error.diagnostic.code, "history.empty");

    // Operation log records mutations and session steps.
    s3.save().unwrap();
    let log = std::fs::read_to_string(root.join(".cadlab/oplog.jsonl")).unwrap();
    let cmds: Vec<String> = log.lines().map(|l| serde_json::from_str::<Step>(l).unwrap().cmd).collect();
    assert_eq!(
        cmds,
        ["project.new", "project.set", "project.set", "history.undo", "history.redo", "history.undo", "project.set"]
    );
    assert_eq!(std::fs::read_to_string(root.join(".cadlab/.gitignore")).unwrap(), "*\n");
}

#[test]
fn dry_run_and_errors_leave_state_unchanged() {
    let r = Registry::with_builtins();
    let (_dir, mut s) = new_project(&r);
    let before = s.project.clone();

    let o = r.execute(&mut s, "project.set", json!({"name": "x"}), RunOptions { dry_run: true }).unwrap();
    assert_eq!(o.output["name"], "x");
    assert!(!o.changed);
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo_len(), 0);

    let f = r
        .execute(&mut s, "project.set", json!({"name": "ok", "targets": ["bad fab!"]}), RunOptions::default())
        .unwrap_err();
    assert_eq!(f.error.kind, ErrorKind::InvalidArgs);
    assert_eq!(s.project, before);

    let f = r.execute(&mut s, "project.set", json!({"nmae": "typo"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "args.invalid");
    assert!(f.error.diagnostic.message.contains("nmae"), "{}", f.error.diagnostic.message);

    // A no-op mutation does not create an undo step.
    let name = s.project.as_ref().unwrap().manifest().name.clone();
    let o = r.execute(&mut s, "project.set", json!({"name": name}), RunOptions::default()).unwrap();
    assert!(!o.changed);
    assert_eq!(s.history.undo_len(), 0);
}

#[test]
fn unknown_command_suggests() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let f = r.execute(&mut s, "project.inf", json!({}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.kind, ErrorKind::NotFound);
    assert_eq!(f.error.diagnostic.hint.as_deref(), Some("did you mean `project.info`?"));

    let f = r.execute(&mut s, "project.info", json!({}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.kind, ErrorKind::NoProject);
}

#[test]
fn batch_is_atomic() {
    let r = Registry::with_builtins();
    let (_dir, mut s) = new_project(&r);
    let before = s.project.clone();

    let steps: Vec<Step> = serde_json::from_value(json!([
        {"cmd": "project.set", "args": {"name": "a"}},
        {"cmd": "project.set", "args": {"targets": ["no good"]}}
    ]))
    .unwrap();
    let f = r.execute_batch(&mut s, steps, RunOptions::default()).unwrap_err();
    assert_eq!(f.step, Some(1));
    assert_eq!(s.project, before);

    let steps: Vec<Step> = serde_json::from_value(json!([
        {"cmd": "project.set", "args": {"name": "a"}},
        {"cmd": "project.set", "args": {"description": "b"}},
        {"cmd": "project.info"}
    ]))
    .unwrap();
    let outs = r.execute_batch(&mut s, steps, RunOptions::default()).unwrap();
    assert_eq!(outs.len(), 3);
    assert_eq!(outs[2].output["name"], "a");
    assert_eq!(s.history.undo_len(), 1);
    assert_eq!(s.history.undo_items()[0].label, "batch (3 commands)");

    let steps = vec![Step { cmd: "history.undo".into(), args: json!({}) }];
    let f = r.execute_batch(&mut s, steps, RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "batch.session_command");
}

#[test]
fn typed_api() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    let (info, _) = run(
        &mut s,
        project::New { path: dir.path().join("t"), name: Some("typed".into()), description: None, targets: vec![] },
        RunOptions::default(),
    )
    .unwrap();
    assert_eq!(info.settings.name, "typed");
    let (settings, _) =
        run(&mut s, project::Set { description: Some("d".into()), ..Default::default() }, RunOptions::default())
            .unwrap();
    assert_eq!(settings.description.as_deref(), Some("d"));
    assert_eq!(s.history.undo_len(), 1);
    assert!(s.is_dirty());
    s.save().unwrap();
    assert!(dir.path().join("t/cadlab.toml").is_file());
}

#[test]
fn new_refuses_existing_project() {
    let r = Registry::with_builtins();
    let (dir, _s) = new_project(&r);
    let mut s = Session::new();
    let f =
        r.execute(&mut s, "project.new", json!({"path": dir.path().join("demo")}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "project.exists");
}

#[test]
fn schemas_are_self_contained() {
    let r = Registry::with_builtins();
    for e in r.iter() {
        let s = e.input_schema.to_string();
        assert!(!s.contains("$ref"), "{} input schema has $ref: {s}", e.name);
        assert_eq!(e.input_schema["type"], "object", "{}", e.name);
    }
}
