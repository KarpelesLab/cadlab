use cadlab::model::{MANIFEST_FILE, ModelError, Project, find_project_root};

#[test]
fn new_project_files() {
    let mut p = Project::new("demo");
    p.manifest_mut().description = Some("A test board".into());
    p.manifest_mut().targets = vec!["jlcpcb".into(), "pcbway".into()];
    let files = p.to_files();
    let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["cadlab.toml", "bom.json", "circuit.json", "board.json"]);
    assert_eq!(
        files[0].1,
        "schema_version = 1\nname = \"demo\"\ndescription = \"A test board\"\ndisplay_units = \"mm\"\ntargets = [\"jlcpcb\", \"pcbway\"]\nnext_id = 1\n"
    );
    assert_eq!(files[1].1, "{}\n");
}

#[test]
fn save_load_roundtrip_and_determinism() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Project::new("demo");
    p.manifest_mut().metadata.insert("author".into(), "someone".into());
    p.alloc_id();
    p.schematic_mut();

    let r1 = p.save(dir.path()).unwrap();
    assert_eq!(r1.written.len(), 5);
    let loaded = Project::load(dir.path()).unwrap();
    assert_eq!(loaded, p);

    // Saving again writes nothing: identical bytes.
    let r2 = loaded.save(dir.path()).unwrap();
    assert!(r2.written.is_empty() && r2.removed.is_empty());

    // Clearing the schematic removes its file.
    let mut q = loaded.clone();
    q.clear_schematic();
    let r3 = q.save(dir.path()).unwrap();
    assert_eq!(r3.removed, vec![dir.path().join("schematic.json")]);
    assert_eq!(Project::load(dir.path()).unwrap(), q);
}

#[test]
fn packed_roundtrip() {
    let mut p = Project::new("packed");
    p.alloc_id();
    let s = p.to_packed_string();
    assert_eq!(Project::from_packed_str(&s).unwrap(), p);
}

#[test]
fn errors() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(Project::load(dir.path()), Err(ModelError::NotAProject(_))));

    std::fs::write(dir.path().join(MANIFEST_FILE), "schema_version = 999\nname = \"x\"\n").unwrap();
    assert!(matches!(Project::load(dir.path()), Err(ModelError::NewerSchema { found: 999, .. })));

    std::fs::write(dir.path().join(MANIFEST_FILE), "schema_version = 1\nname = \"x\"\ntypo = 1\n").unwrap();
    let e = Project::load(dir.path()).unwrap_err().to_string();
    assert!(e.contains("unknown field `typo`"), "{e}");

    std::fs::write(dir.path().join(MANIFEST_FILE), "schema_version = 1\nname = \"x\"\n").unwrap();
    std::fs::write(dir.path().join("board.json"), "{not json").unwrap();
    let e = Project::load(dir.path()).unwrap_err().to_string();
    assert!(e.contains("board.json"), "{e}");
}

#[test]
fn finds_root() {
    let dir = tempfile::tempdir().unwrap();
    Project::new("x").save(dir.path()).unwrap();
    let sub = dir.path().join("a/b");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(find_project_root(&sub).as_deref(), Some(dir.path()));
}
