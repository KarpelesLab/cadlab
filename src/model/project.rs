use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::model::ModelError;
use crate::model::footprint::Footprint;
use crate::model::format::to_canonical_string;
use crate::model::manifest::Manifest;
use crate::model::migrate::migrate;
use crate::model::model3d::{ModelData, valid_model_name};
use crate::model::part::Part;
use crate::model::raw::RawProject;
use crate::model::sections::{Board, Bom, Circuit, Library, Schematic};

/// Name of the manifest file that marks a project directory.
pub const MANIFEST_FILE: &str = "cadlab.toml";

/// Sections stored as `<name>.json`, in write order. `schematic` is optional.
const SECTION_FILES: &[&str] = &["bom", "circuit", "schematic", "board"];

/// Library subdirectories: parts and footprints, one JSON file per item.
const PARTS_DIR: &str = "library/parts";
const FOOTPRINTS_DIR: &str = "library/footprints";
/// 3D model files, stored as-is (binary), one file each.
const MODELS_DIR: &str = "library/models";

/// A cadlab project, in memory.
///
/// Sections are reference-counted and copy-on-write, so cloning a project is cheap. Undo
/// history and transactions rely on this: a snapshot is a clone.
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    manifest: Arc<Manifest>,
    library: Arc<Library>,
    bom: Arc<Bom>,
    circuit: Arc<Circuit>,
    schematic: Option<Arc<Schematic>>,
    board: Arc<Board>,
}

/// What [`Project::save`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveReport {
    /// Files written because their content changed.
    pub written: Vec<PathBuf>,
    /// Files removed (e.g. `schematic.json` after the schematic was cleared).
    pub removed: Vec<PathBuf>,
}

impl Project {
    /// A new, empty project.
    pub fn new(name: impl Into<String>) -> Self {
        Project {
            manifest: Arc::new(Manifest::new(name)),
            library: Arc::default(),
            bom: Arc::default(),
            circuit: Arc::default(),
            schematic: None,
            board: Arc::default(),
        }
    }

    /// Manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Mutable manifest (copy-on-write).
    pub fn manifest_mut(&mut self) -> &mut Manifest {
        Arc::make_mut(&mut self.manifest)
    }

    /// Part library.
    pub fn library(&self) -> &Library {
        &self.library
    }

    /// Mutable part library.
    pub fn library_mut(&mut self) -> &mut Library {
        Arc::make_mut(&mut self.library)
    }

    /// BOM overlay.
    pub fn bom(&self) -> &Bom {
        &self.bom
    }

    /// Mutable BOM overlay.
    pub fn bom_mut(&mut self) -> &mut Bom {
        Arc::make_mut(&mut self.bom)
    }

    /// Circuit.
    pub fn circuit(&self) -> &Circuit {
        &self.circuit
    }

    /// Mutable circuit.
    pub fn circuit_mut(&mut self) -> &mut Circuit {
        Arc::make_mut(&mut self.circuit)
    }

    /// Schematic hints, if any.
    pub fn schematic(&self) -> Option<&Schematic> {
        self.schematic.as_deref()
    }

    /// Mutable schematic hints, created on first use.
    pub fn schematic_mut(&mut self) -> &mut Schematic {
        Arc::make_mut(self.schematic.get_or_insert_with(Arc::default))
    }

    /// Drops schematic hints; the schematic will be fully regenerated.
    pub fn clear_schematic(&mut self) {
        self.schematic = None;
    }

    /// Board.
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// The shared board section (unchanged boards share it between snapshots).
    pub(crate) fn board_arc(&self) -> &Arc<Board> {
        &self.board
    }

    /// Mutable board.
    pub fn board_mut(&mut self) -> &mut Board {
        Arc::make_mut(&mut self.board)
    }

    /// Allocates a new object ID.
    pub fn alloc_id(&mut self) -> crate::ObjectId {
        self.manifest_mut().next_id.alloc()
    }

    /// Converts to untyped form.
    pub fn to_raw(&self) -> RawProject {
        let mut raw = RawProject::from_manifest(to_value(&*self.manifest));
        raw.sections.insert("bom".into(), to_value(&*self.bom));
        raw.sections.insert("circuit".into(), to_value(&*self.circuit));
        if let Some(s) = &self.schematic {
            raw.sections.insert("schematic".into(), to_value(&**s));
        }
        raw.sections.insert("board".into(), to_value(&*self.board));
        raw.parts = self.library.parts.iter().map(|(k, v)| (k.clone(), to_value(v))).collect();
        raw.footprints = self.library.footprints.iter().map(|(k, v)| (k.clone(), to_value(v))).collect();
        raw.models = self.library.models.clone();
        raw
    }

    /// Builds from untyped form, running migrations first.
    pub fn from_raw(mut raw: RawProject) -> Result<Self, ModelError> {
        migrate(&mut raw)?;
        let manifest: Manifest = from_value(MANIFEST_FILE, raw.manifest)?;
        let mut take = |name: &str| raw.sections.remove(name);
        let bom = section(&mut take, "bom")?.unwrap_or_default();
        let circuit = section(&mut take, "circuit")?.unwrap_or_default();
        let schematic = section(&mut take, "schematic")?;
        let board = section(&mut take, "board")?.unwrap_or_default();
        if let Some(extra) = raw.sections.keys().next() {
            return Err(ModelError::invalid(format!("{extra}.json"), "unknown project section"));
        }
        let mut library = Library::default();
        for (id, v) in raw.parts {
            let file = format!("{PARTS_DIR}/{id}.json");
            let p: Part = from_value(&file, v)?;
            if p.id != id {
                return Err(ModelError::invalid(file, format!("part id `{}` does not match the file name", p.id)));
            }
            library.parts.insert(id, p);
        }
        for (name, v) in raw.footprints {
            let file = format!("{FOOTPRINTS_DIR}/{name}.json");
            let f: Footprint = from_value(&file, v)?;
            if f.name != name {
                return Err(ModelError::invalid(
                    file,
                    format!("footprint name `{}` does not match the file name", f.name),
                ));
            }
            library.footprints.insert(name, f);
        }
        for (name, data) in raw.models {
            if !valid_model_name(&name) {
                return Err(ModelError::invalid(
                    format!("{MODELS_DIR}/{name}"),
                    "invalid model file name (letters, digits, `. _ + -`, with an extension)",
                ));
            }
            library.models.insert(name, data);
        }
        Ok(Project {
            manifest: Arc::new(manifest),
            library: Arc::new(library),
            bom: Arc::new(bom),
            circuit: Arc::new(circuit),
            schematic: schematic.map(Arc::new),
            board: Arc::new(board),
        })
    }

    /// The whole project as one canonical JSON document (`project pack`).
    pub fn to_packed_string(&self) -> String {
        to_canonical_string(&self.to_raw().to_packed())
    }

    /// Inverse of [`Project::to_packed_string`].
    pub fn from_packed_str(s: &str) -> Result<Self, ModelError> {
        let v: Value = serde_json::from_str(s).map_err(|e| ModelError::invalid("<packed>", e))?;
        Project::from_raw(RawProject::from_packed(v)?)
    }

    /// The file set this project saves to, as (relative path, content).
    pub fn to_files(&self) -> Vec<(String, String)> {
        let raw = self.to_raw();
        let mut files = Vec::new();
        files.push((MANIFEST_FILE.to_string(), manifest_toml(&self.manifest)));
        for name in SECTION_FILES {
            if let Some(v) = raw.sections.get(*name) {
                files.push((format!("{name}.json"), to_canonical_string(v)));
            }
        }
        for (id, v) in &raw.parts {
            files.push((format!("{PARTS_DIR}/{id}.json"), to_canonical_string(v)));
        }
        for (name, v) in &raw.footprints {
            files.push((format!("{FOOTPRINTS_DIR}/{name}.json"), to_canonical_string(v)));
        }
        files
    }

    /// Loads the project in `dir`.
    pub fn load(dir: &Path) -> Result<Self, ModelError> {
        let manifest_path = dir.join(MANIFEST_FILE);
        let text = match fs::read_to_string(&manifest_path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ModelError::NotAProject(dir.to_path_buf()));
            }
            Err(e) => return Err(ModelError::io(manifest_path, e)),
        };
        let manifest: Value = toml::from_str(&text).map_err(|e| ModelError::invalid(&manifest_path, e))?;
        let mut raw = RawProject::from_manifest(manifest);
        for name in SECTION_FILES {
            let path = dir.join(format!("{name}.json"));
            match fs::read_to_string(&path) {
                Ok(t) => {
                    let v = serde_json::from_str(&t).map_err(|e| ModelError::invalid(&path, e))?;
                    raw.sections.insert(name.to_string(), v);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(ModelError::io(path, e)),
            }
        }
        raw.parts = read_items(&dir.join(PARTS_DIR))?;
        raw.footprints = read_items(&dir.join(FOOTPRINTS_DIR))?;
        raw.models = read_models(&dir.join(MODELS_DIR))?;
        Project::from_raw(raw)
    }

    /// Saves into `dir`, creating it if needed. Only files whose content changed are written
    /// (atomically, via a temporary file and rename).
    pub fn save(&self, dir: &Path) -> Result<SaveReport, ModelError> {
        fs::create_dir_all(dir).map_err(|e| ModelError::io(dir, e))?;
        let mut report = SaveReport::default();
        let files = self.to_files();
        for (name, content) in &files {
            let path = dir.join(name);
            if fs::read_to_string(&path).is_ok_and(|old| &old == content) {
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| ModelError::io(parent, e))?;
            }
            write_atomic(&path, content)?;
            report.written.push(path);
        }
        for name in SECTION_FILES {
            let file = format!("{name}.json");
            if !files.iter().any(|(n, _)| *n == file) {
                let path = dir.join(&file);
                if path.exists() {
                    fs::remove_file(&path).map_err(|e| ModelError::io(&path, e))?;
                    report.removed.push(path);
                }
            }
        }
        // Model files (binary).
        let models = &self.library.models;
        for (name, data) in models {
            let path = dir.join(MODELS_DIR).join(name);
            if fs::read(&path).is_ok_and(|old| old == data.bytes()) {
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| ModelError::io(parent, e))?;
            }
            write_atomic_bytes(&path, data.bytes())?;
            report.written.push(path);
        }
        if let Ok(rd) = fs::read_dir(dir.join(MODELS_DIR)) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if valid_model_name(&name) && !models.contains_key(&name) && e.path().is_file() {
                    fs::remove_file(e.path()).map_err(|err| ModelError::io(e.path(), err))?;
                    report.removed.push(e.path());
                }
            }
        }
        // Library items that were removed.
        for sub in [PARTS_DIR, FOOTPRINTS_DIR] {
            let Ok(rd) = fs::read_dir(dir.join(sub)) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let rel = format!("{sub}/{name}");
                if name.ends_with(".json") && !files.iter().any(|(n, _)| *n == rel) {
                    fs::remove_file(e.path()).map_err(|err| ModelError::io(e.path(), err))?;
                    report.removed.push(e.path());
                }
            }
        }
        report.removed.sort();
        Ok(report)
    }
}

/// Walks up from `start` to find a directory containing `cadlab.toml`.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|d| d.join(MANIFEST_FILE).is_file()).map(Path::to_path_buf)
}

/// Reads every `<key>.json` in `dir` (missing directory = empty).
fn read_items(dir: &Path) -> Result<std::collections::BTreeMap<String, Value>, ModelError> {
    let mut out = std::collections::BTreeMap::new();
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(ModelError::io(dir, e)),
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(key) = name.strip_suffix(".json") else {
            continue;
        };
        let path = e.path();
        let text = fs::read_to_string(&path).map_err(|err| ModelError::io(&path, err))?;
        let v = serde_json::from_str(&text).map_err(|err| ModelError::invalid(&path, err))?;
        out.insert(key.to_string(), v);
    }
    Ok(out)
}

/// Reads every model file in `dir` (missing directory = empty). Files whose names are not valid
/// model names (hidden files, temporary files) are ignored.
fn read_models(dir: &Path) -> Result<std::collections::BTreeMap<String, ModelData>, ModelError> {
    let mut out = std::collections::BTreeMap::new();
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(ModelError::io(dir, e)),
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if !valid_model_name(&name) || name.ends_with(".tmp") || !path.is_file() {
            continue;
        }
        let bytes = fs::read(&path).map_err(|err| ModelError::io(&path, err))?;
        out.insert(name, ModelData::new(bytes));
    }
    Ok(out)
}

fn manifest_toml(m: &Manifest) -> String {
    // Serializing a plain struct of strings, numbers and maps to TOML cannot fail.
    toml::to_string(m).expect("manifest serializes to TOML")
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("model serializes to JSON")
}

fn from_value<T: DeserializeOwned>(file: &str, v: Value) -> Result<T, ModelError> {
    serde_json::from_value(v).map_err(|e| ModelError::invalid(file, e))
}

fn section<T: DeserializeOwned>(
    take: &mut impl FnMut(&str) -> Option<Value>,
    name: &str,
) -> Result<Option<T>, ModelError> {
    take(name).map(|v| from_value(&format!("{name}.json"), v)).transpose()
}

fn write_atomic_bytes(path: &Path, content: &[u8]) -> Result<(), ModelError> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    fs::write(&tmp, content).map_err(|e| ModelError::io(&tmp, e))?;
    fs::rename(&tmp, path).map_err(|e| ModelError::io(path, e))
}

fn write_atomic(path: &Path, content: &str) -> Result<(), ModelError> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    fs::write(&tmp, content).map_err(|e| ModelError::io(&tmp, e))?;
    fs::rename(&tmp, path).map_err(|e| ModelError::io(path, e))
}
