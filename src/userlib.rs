//! Shared user libraries: parts, footprints and blocks kept outside any project (DECISIONS D19).
//!
//! A library is a directory laid out like a project library, plus blocks:
//!
//! ```text
//! <library>/
//! ├── library.toml            schema_version (written on first publish)
//! ├── parts/<id>.json         a Part, same format as a project's library/parts/<id>.json
//! ├── footprints/<name>.json  a Footprint
//! ├── models/<file>           a 3D model file (`SOT-23-5.stl`), as-is, used by footprints/parts
//! └── blocks/<name>.json      a LibBlock: the block plus copies of every part, footprint and
//!                             model it uses
//! ```
//!
//! The **user library** lives in `$XDG_DATA_HOME/cadlab/library` (default
//! `~/.local/share/cadlab/library`; `%APPDATA%\cadlab\library` on Windows when `XDG_DATA_HOME` is
//! not set). More directories can be listed in the user settings (`libraries = [...]`); they are
//! searched after the user library, in order.
//!
//! Projects never read from these libraries implicitly: items are copied in with `lib.import`, so
//! a project stays self-contained (see `model::sections::Library`). Files are written with the
//! canonical JSON writer, so libraries diff well under version control.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Project;
use crate::model::circuit::Block;
use crate::model::footprint::Footprint;
use crate::model::format::to_canonical_string;
use crate::model::model3d::{ModelData, valid_model_name};
use crate::model::part::{Part, valid_id};

/// Marker file at the root of a library.
pub const LIBRARY_FILE: &str = "library.toml";

/// Library format version written by this build.
pub const LIBRARY_SCHEMA_VERSION: u32 = 1;

/// Name of the user library in listings and in the `library` argument of commands.
pub const USER_LIBRARY: &str = "user";

/// What a library item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// A part (`parts/<id>.json`).
    Part,
    /// A footprint (`footprints/<name>.json`).
    Footprint,
    /// A block with the parts and footprints it uses (`blocks/<name>.json`).
    Block,
    /// A 3D model file used by footprints and parts (`models/<file>`, stored as-is).
    Model,
}

impl ItemKind {
    /// Every kind, in listing order.
    pub const ALL: [ItemKind; 4] = [ItemKind::Part, ItemKind::Footprint, ItemKind::Block, ItemKind::Model];

    /// Subdirectory holding items of this kind.
    pub fn dir(self) -> &'static str {
        match self {
            ItemKind::Part => "parts",
            ItemKind::Footprint => "footprints",
            ItemKind::Block => "blocks",
            ItemKind::Model => "models",
        }
    }

    /// Lower-case label.
    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Part => "part",
            ItemKind::Footprint => "footprint",
            ItemKind::Block => "block",
            ItemKind::Model => "model",
        }
    }
}

/// A block as stored in a library: self-contained, so importing it into an empty project works
/// even if the library's own `parts/` and `footprints/` change later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LibBlock {
    /// Block name (the file name).
    pub name: String,
    /// The block definition, as in `circuit.json`.
    pub block: Block,
    /// Every part the block's components use, by ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parts: BTreeMap<String, Part>,
    /// Every footprint those parts reference, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub footprints: BTreeMap<String, Footprint>,
    /// Every 3D model file those parts and footprints reference, by file name (base64).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, ModelData>,
}

/// Model files referenced by `parts` (their own models) and `footprints`, sorted.
pub fn models_used<'a>(
    parts: impl IntoIterator<Item = &'a Part>,
    footprints: impl IntoIterator<Item = &'a Footprint>,
) -> Vec<String> {
    let mut out: Vec<String> =
        footprints.into_iter().filter_map(|f| f.model.as_ref().map(|m| m.file.clone())).collect();
    for p in parts {
        out.extend(p.footprints.iter().filter_map(|r| r.model.as_ref().map(|m| m.file.clone())));
    }
    out.sort();
    out.dedup();
    out
}

impl LibBlock {
    /// Bundles block `name` of `project` with its parts and their footprints. Footprints a part
    /// references but the project lacks are skipped and returned in the second value.
    pub fn from_project(project: &Project, name: &str) -> Option<(LibBlock, Vec<String>)> {
        let block = project.circuit().blocks.get(name)?.clone();
        let lib = project.library();
        let mut parts = BTreeMap::new();
        let mut footprints = BTreeMap::new();
        let mut missing = Vec::new();
        for bc in block.components.values() {
            let Some(part) = lib.parts.get(&bc.part) else {
                missing.push(format!("part {}", bc.part));
                continue;
            };
            for fr in &part.footprints {
                match lib.footprints.get(&fr.footprint) {
                    Some(f) => {
                        footprints.insert(f.name.clone(), f.clone());
                    }
                    None => missing.push(format!("footprint {}", fr.footprint)),
                }
            }
            parts.insert(part.id.clone(), part.clone());
        }
        let mut models = BTreeMap::new();
        for m in models_used(parts.values(), footprints.values()) {
            match lib.models.get(&m) {
                Some(d) => {
                    models.insert(m, d.clone());
                }
                None => missing.push(format!("model {m}")),
            }
        }
        missing.sort();
        missing.dedup();
        Some((LibBlock { name: name.to_string(), block, parts, footprints, models }, missing))
    }

    /// Checks internal consistency: every component's part is bundled.
    fn validate(&self) -> Result<(), String> {
        for (id, p) in &self.parts {
            if &p.id != id {
                return Err(format!("bundled part key `{id}` does not match its id `{}`", p.id));
            }
        }
        for (name, f) in &self.footprints {
            if &f.name != name {
                return Err(format!("bundled footprint key `{name}` does not match its name `{}`", f.name));
            }
        }
        for name in self.models.keys() {
            if !valid_model_name(name) {
                return Err(format!("bundled model `{name}` has an invalid file name"));
            }
        }
        for (r, bc) in &self.block.components {
            if !self.parts.contains_key(&bc.part) {
                return Err(format!("component {r} uses part `{}`, which the block file does not contain", bc.part));
            }
        }
        Ok(())
    }
}

/// One library item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// A part.
    Part(Part),
    /// A footprint.
    Footprint(Footprint),
    /// A block with its parts and footprints.
    Block(LibBlock),
    /// A 3D model file: name (with extension) and bytes.
    Model(String, ModelData),
}

impl Item {
    /// Kind.
    pub fn kind(&self) -> ItemKind {
        match self {
            Item::Part(_) => ItemKind::Part,
            Item::Footprint(_) => ItemKind::Footprint,
            Item::Block(_) => ItemKind::Block,
            Item::Model(..) => ItemKind::Model,
        }
    }

    /// ID or name (also the file name).
    pub fn name(&self) -> &str {
        match self {
            Item::Part(p) => &p.id,
            Item::Footprint(f) => &f.name,
            Item::Block(b) => &b.name,
            Item::Model(n, _) => n,
        }
    }

    /// JSON value as stored.
    pub fn to_value(&self) -> Value {
        let v = match self {
            Item::Part(p) => serde_json::to_value(p),
            Item::Footprint(f) => serde_json::to_value(f),
            Item::Block(b) => serde_json::to_value(b),
            Item::Model(n, d) => Ok(serde_json::json!({
                "name": n,
                "format": crate::models3d::format_of(n),
                "bytes": d.len(),
            })),
        };
        v.expect("library items serialize to JSON")
    }

    /// One-line description for listings.
    pub fn description(&self) -> String {
        match self {
            Item::Part(p) => {
                let mut s = p.category.label().to_string();
                if let Some(m) = &p.mpn {
                    s += &format!(" {m}");
                } else {
                    s += &format!(" {}", p.value());
                }
                if !p.description.is_empty() {
                    s += &format!(" — {}", p.description);
                }
                s
            }
            Item::Footprint(f) => format!("{} pads  {}", f.pads.len(), f.description).trim_end().to_string(),
            Item::Model(n, d) => {
                let f = crate::models3d::extension(n).to_ascii_uppercase();
                format!("{f} 3D model, {} bytes", d.len())
            }
            Item::Block(b) => {
                let mut s = format!("{} component(s), ports: {}", b.block.components.len(), b.block.ports.join(", "));
                if !b.block.description.is_empty() {
                    s += &format!(" — {}", b.block.description);
                }
                s
            }
        }
    }

    /// Whether `query` (case-insensitive substring) matches the name, description, or a part's
    /// manufacturer and MPN.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        let mut hay = format!("{} {}", self.name(), self.description());
        if let Item::Part(p) = self {
            hay += &format!(" {} {}", p.manufacturer.as_deref().unwrap_or(""), p.mpn.as_deref().unwrap_or(""));
        }
        hay.to_lowercase().contains(&q)
    }
}

/// How an item compares to what a library (or project) already holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Existing {
    /// Nothing under that name.
    Absent,
    /// Identical content.
    Same,
    /// Different content under the same name.
    Different,
    /// A file whose name differs only in case (the same file on case-insensitive filesystems).
    CaseVariant(String),
}

/// Error reading or writing a library.
#[derive(Debug, thiserror::Error)]
pub enum LibError {
    /// Filesystem error.
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    /// A file is not valid or does not match the expected structure.
    #[error("{0}: {1}")]
    Invalid(PathBuf, String),
    /// Written by a newer cadlab.
    #[error("{path}: library schema version {found} is newer than this cadlab supports ({supported}); upgrade cadlab")]
    NewerSchema {
        /// The library's marker file.
        path: PathBuf,
        /// Version found.
        found: u32,
        /// Highest supported version.
        supported: u32,
    },
    /// No home or data directory could be determined.
    #[error("cannot determine the user data directory (set HOME or XDG_DATA_HOME)")]
    NoDataDir,
    /// The user settings file could not be read.
    #[error("{0}")]
    Settings(String),
    /// Invalid item name.
    #[error("invalid {0} name `{1}` (letters, digits, `. _ + -`)")]
    InvalidName(&'static str, String),
}

/// `$XDG_DATA_HOME/cadlab`, else `%APPDATA%\cadlab` (Windows), else `~/.local/share/cadlab`.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|x| Path::new(x).is_absolute()) {
        return Some(PathBuf::from(x).join("cadlab"));
    }
    #[cfg(windows)]
    if let Some(a) = std::env::var_os("APPDATA").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(a).join("cadlab"));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".local/share/cadlab"))
}

/// The user library directory (`<data_dir>/library`).
pub fn user_library_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("library"))
}

/// Library marker file content.
#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    schema_version: u32,
}

/// One library directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UserLibrary {
    /// Short name: `user` for the user library, the directory name for the others.
    pub name: String,
    /// Directory.
    pub path: PathBuf,
}

impl UserLibrary {
    /// A library at `path`.
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        UserLibrary { name: name.into(), path: path.into() }
    }

    /// Whether the directory exists.
    pub fn exists(&self) -> bool {
        self.path.is_dir()
    }

    fn file(&self, kind: ItemKind, name: &str) -> PathBuf {
        match kind {
            ItemKind::Model => self.path.join(kind.dir()).join(name),
            _ => self.path.join(kind.dir()).join(format!("{name}.json")),
        }
    }

    /// Fails if the library was written by a newer cadlab.
    pub fn check_schema(&self) -> Result<(), LibError> {
        let p = self.path.join(LIBRARY_FILE);
        let text = match fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(LibError::Io(p, e)),
        };
        let m: Marker = toml::from_str(&text).map_err(|e| LibError::Invalid(p.clone(), e.to_string()))?;
        if m.schema_version > LIBRARY_SCHEMA_VERSION {
            return Err(LibError::NewerSchema { path: p, found: m.schema_version, supported: LIBRARY_SCHEMA_VERSION });
        }
        Ok(())
    }

    /// Names of the items of `kind`, sorted. A missing directory is empty.
    pub fn names(&self, kind: ItemKind) -> Result<Vec<String>, LibError> {
        let dir = self.path.join(kind.dir());
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(LibError::Io(dir, e)),
        };
        let mut out: Vec<String> = match kind {
            ItemKind::Model => rd
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| valid_model_name(n) && !n.ends_with(".tmp"))
                .collect(),
            _ => rd
                .flatten()
                .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string))
                .filter(|n| valid_id(n))
                .collect(),
        };
        out.sort();
        Ok(out)
    }

    /// The stored name of `name` (exact, then case-insensitive), if present.
    pub fn resolve(&self, kind: ItemKind, name: &str) -> Result<Option<String>, LibError> {
        let names = self.names(kind)?;
        if names.iter().any(|n| n == name) {
            return Ok(Some(name.to_string()));
        }
        Ok(names.into_iter().find(|n| n.eq_ignore_ascii_case(name)))
    }

    /// Reads item `name` (exact name, as returned by [`UserLibrary::resolve`]).
    pub fn read(&self, kind: ItemKind, name: &str) -> Result<Option<Item>, LibError> {
        self.check_schema()?;
        let path = self.file(kind, name);
        if kind == ItemKind::Model {
            return match fs::read(&path) {
                Ok(b) => Ok(Some(Item::Model(name.to_string(), ModelData::new(b)))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(LibError::Io(path, e)),
            };
        }
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(LibError::Io(path, e)),
        };
        let v: Value = serde_json::from_str(&text).map_err(|e| LibError::Invalid(path.clone(), e.to_string()))?;
        let item = match kind {
            ItemKind::Part => Item::Part(parse(&path, v)?),
            ItemKind::Footprint => Item::Footprint(parse(&path, v)?),
            ItemKind::Model => unreachable!("read above"),
            ItemKind::Block => {
                let b: LibBlock = parse(&path, v)?;
                b.validate().map_err(|e| LibError::Invalid(path.clone(), e))?;
                Item::Block(b)
            }
        };
        if item.name() != name {
            return Err(LibError::Invalid(
                path,
                format!("{} name `{}` does not match the file name", kind.label(), item.name()),
            ));
        }
        Ok(Some(item))
    }

    /// Every item of `kind`.
    pub fn items(&self, kind: ItemKind) -> Result<Vec<Item>, LibError> {
        let mut out = Vec::new();
        for n in self.names(kind)? {
            out.extend(self.read(kind, &n)?);
        }
        Ok(out)
    }

    /// Compares `item` with what the library holds under its name.
    pub fn compare(&self, item: &Item) -> Result<Existing, LibError> {
        match self.resolve(item.kind(), item.name())? {
            None => Ok(Existing::Absent),
            Some(n) if n != item.name() => Ok(Existing::CaseVariant(n)),
            Some(n) => match self.read(item.kind(), &n)? {
                Some(old) if &old == item => Ok(Existing::Same),
                Some(_) => Ok(Existing::Different),
                None => Ok(Existing::Absent),
            },
        }
    }

    /// Creates the library directory and its marker if needed.
    pub fn ensure(&self) -> Result<(), LibError> {
        fs::create_dir_all(&self.path).map_err(|e| LibError::Io(self.path.clone(), e))?;
        let marker = self.path.join(LIBRARY_FILE);
        if !marker.exists() {
            let text = toml::to_string(&Marker { schema_version: LIBRARY_SCHEMA_VERSION }).expect("marker serializes");
            write_atomic(&marker, &text)?;
        }
        self.check_schema()
    }

    /// Writes `item` (canonical JSON, atomically), replacing a file whose name differs only in
    /// case. Returns the file written.
    pub fn write(&self, item: &Item) -> Result<PathBuf, LibError> {
        let kind = item.kind();
        let name = item.name();
        let valid = if kind == ItemKind::Model { valid_model_name(name) } else { valid_id(name) };
        if !valid {
            return Err(LibError::InvalidName(kind.label(), name.to_string()));
        }
        self.ensure()?;
        if let Some(old) = self.resolve(kind, name)?
            && old != name
        {
            let p = self.file(kind, &old);
            fs::remove_file(&p).map_err(|e| LibError::Io(p, e))?;
        }
        let dir = self.path.join(kind.dir());
        fs::create_dir_all(&dir).map_err(|e| LibError::Io(dir.clone(), e))?;
        let path = self.file(kind, name);
        match item {
            Item::Model(_, d) => write_atomic_bytes(&path, d.bytes())?,
            _ => write_atomic(&path, &to_canonical_string(&item.to_value()))?,
        }
        Ok(path)
    }

    /// Deletes item `name` (exact). Returns whether it existed.
    pub fn remove(&self, kind: ItemKind, name: &str) -> Result<bool, LibError> {
        let p = self.file(kind, name);
        match fs::remove_file(&p) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(LibError::Io(p, e)),
        }
    }
}

/// The libraries searched, in order: the user library first, then the configured ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Libraries {
    /// Libraries in search order.
    pub list: Vec<UserLibrary>,
}

impl Libraries {
    /// Explicit libraries (tests, embedding applications). The first one is the default target
    /// of `lib.publish`.
    pub fn new(list: Vec<UserLibrary>) -> Self {
        Libraries { list }
    }

    /// The user library at `user`, then `extra` directories named after their last component
    /// (the full path when that name is taken).
    pub fn with_dirs(user: Option<PathBuf>, extra: &[PathBuf]) -> Self {
        let mut list: Vec<UserLibrary> = user.into_iter().map(|p| UserLibrary::new(USER_LIBRARY, p)).collect();
        for p in extra {
            if list.iter().any(|l| l.path == *p) {
                continue;
            }
            let base = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let name =
                if base.is_empty() || list.iter().any(|l| l.name == base) { p.display().to_string() } else { base };
            list.push(UserLibrary::new(name, p.clone()));
        }
        Libraries { list }
    }

    /// From the environment and the user settings file.
    pub fn from_settings() -> Result<Self, LibError> {
        let cfg = crate::config::UserConfig::load().map_err(|e| LibError::Settings(e.to_string()))?;
        Ok(Self::with_dirs(user_library_dir(), &cfg.library_paths()))
    }

    /// Finds a library by name, or by path when `spec` looks like one (contains a path
    /// separator or starts with `.` or `~`). Paths need not be configured.
    pub fn select(&self, spec: &str) -> Option<UserLibrary> {
        if let Some(l) = self.list.iter().find(|l| l.name == spec) {
            return Some(l.clone());
        }
        let looks_like_path =
            spec.contains('/') || spec.contains('\\') || spec.starts_with('.') || spec.starts_with('~');
        if !looks_like_path {
            return None;
        }
        let p = crate::config::expand_home(Path::new(spec));
        let p = std::path::absolute(&p).unwrap_or(p);
        Some(self.list.iter().find(|l| l.path == p).cloned().unwrap_or_else(|| UserLibrary::new(spec, p)))
    }

    /// Every library holding `name` (any kind in `kinds`), in search order, as (library, kind,
    /// stored name).
    pub fn find(&self, kinds: &[ItemKind], name: &str) -> Result<Vec<(UserLibrary, ItemKind, String)>, LibError> {
        let mut out = Vec::new();
        for l in &self.list {
            for &k in kinds {
                if let Some(n) = l.resolve(k, name)? {
                    out.push((l.clone(), k, n));
                }
            }
        }
        Ok(out)
    }

    /// Finds a part by MPN (case-insensitive) when no ID matches.
    pub fn find_part_by_mpn(&self, mpn: &str) -> Result<Option<(UserLibrary, Part)>, LibError> {
        for l in &self.list {
            for it in l.items(ItemKind::Part)? {
                if let Item::Part(p) = it
                    && p.mpn.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(mpn))
                {
                    return Ok(Some((l.clone(), p)));
                }
            }
        }
        Ok(None)
    }
}

fn parse<T: DeserializeOwned>(path: &Path, v: Value) -> Result<T, LibError> {
    serde_json::from_value(v).map_err(|e| LibError::Invalid(path.to_path_buf(), e.to_string()))
}

fn write_atomic_bytes(path: &Path, content: &[u8]) -> Result<(), LibError> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    fs::write(&tmp, content).map_err(|e| LibError::Io(tmp.clone(), e))?;
    fs::rename(&tmp, path).map_err(|e| LibError::Io(path.to_path_buf(), e))
}

fn write_atomic(path: &Path, content: &str) -> Result<(), LibError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).map_err(|e| LibError::Io(tmp.clone(), e))?;
    fs::rename(&tmp, path).map_err(|e| LibError::Io(path.to_path_buf(), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::circuit::{BlockComponent, PinRef};
    use crate::model::footprint::Mount;
    use crate::model::part::{Category, FootprintRef, Pin, PinKind, Symbol};

    fn footprint(name: &str) -> Footprint {
        Footprint {
            name: name.into(),
            description: "test".into(),
            mount: Mount::Smd,
            pads: vec![],
            courtyard: vec![],
            graphics: vec![],
            body: None,
            generator: None,
            model: None,
        }
    }

    fn part(id: &str, fp: &str) -> Part {
        Part {
            id: id.into(),
            category: Category::Resistor,
            description: "a resistor".into(),
            manufacturer: None,
            mpn: Some(format!("MPN-{id}")),
            params: Default::default(),
            symbol: Symbol {
                pins: vec![Pin::new("1", "A", PinKind::Passive), Pin::new("2", "B", PinKind::Passive)],
                ..Default::default()
            },
            footprints: vec![FootprintRef::new(fp)],
            datasheet: None,
            provenance: Default::default(),
        }
    }

    #[test]
    fn write_read_compare_remove() {
        let dir = tempfile::tempdir().unwrap();
        let lib = UserLibrary::new("user", dir.path().join("lib"));
        assert!(!lib.exists());
        assert!(lib.names(ItemKind::Part).unwrap().is_empty());
        let p = Item::Part(part("R1k", "RES0402"));
        assert_eq!(lib.compare(&p).unwrap(), Existing::Absent);
        let path = lib.write(&p).unwrap();
        assert_eq!(path, dir.path().join("lib/parts/R1k.json"));
        assert!(dir.path().join("lib/library.toml").is_file());
        // Canonical, deterministic output.
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, to_canonical_string(&p.to_value()));
        assert_eq!(lib.compare(&p).unwrap(), Existing::Same);
        assert_eq!(lib.read(ItemKind::Part, "R1k").unwrap(), Some(p.clone()));
        assert_eq!(lib.resolve(ItemKind::Part, "r1K").unwrap().as_deref(), Some("R1k"));

        let mut changed = part("R1k", "RES0402");
        changed.description = "changed".into();
        assert_eq!(lib.compare(&Item::Part(changed)).unwrap(), Existing::Different);
        assert_eq!(lib.compare(&Item::Part(part("r1k", "RES0402"))).unwrap(), Existing::CaseVariant("R1k".into()));
        lib.write(&Item::Part(part("r1k", "RES0402"))).unwrap();
        assert_eq!(lib.names(ItemKind::Part).unwrap(), ["r1k"]);

        assert!(Item::Part(part("r1k", "x")).matches("mpn-R1"));
        assert!(!Item::Part(part("r1k", "x")).matches("capacitor"));
        assert!(lib.remove(ItemKind::Part, "r1k").unwrap());
        assert!(!lib.remove(ItemKind::Part, "r1k").unwrap());
        assert!(lib.write(&Item::Part(part("a/b", "x"))).is_err());
    }

    #[test]
    fn invalid_files_and_schema() {
        let dir = tempfile::tempdir().unwrap();
        let lib = UserLibrary::new("user", dir.path());
        lib.write(&Item::Footprint(footprint("FP"))).unwrap();
        // File name and content disagree.
        fs::copy(dir.path().join("footprints/FP.json"), dir.path().join("footprints/OTHER.json")).unwrap();
        assert!(matches!(lib.read(ItemKind::Footprint, "OTHER"), Err(LibError::Invalid(..))));
        fs::write(dir.path().join(LIBRARY_FILE), "schema_version = 99\n").unwrap();
        assert!(matches!(lib.read(ItemKind::Footprint, "FP"), Err(LibError::NewerSchema { .. })));
    }

    #[test]
    fn blocks_are_self_contained() {
        let mut project = Project::new("t");
        project.library_mut().parts.insert("R1k".into(), part("R1k", "RES0402"));
        project.library_mut().footprints.insert("RES0402".into(), footprint("RES0402"));
        let block = Block {
            description: "divider".into(),
            components: [("R1".into(), BlockComponent { part: "R1k".into(), properties: Default::default() })].into(),
            nets: [("IN".into(), [PinRef::new("R1", "1")].into())].into(),
            ports: vec!["IN".into()],
            no_connect: Default::default(),
        };
        project.circuit_mut().blocks.insert("div".into(), block);
        let (lb, missing) = LibBlock::from_project(&project, "div").unwrap();
        assert!(missing.is_empty());
        assert_eq!(lb.parts.keys().collect::<Vec<_>>(), ["R1k"]);
        assert_eq!(lb.footprints.keys().collect::<Vec<_>>(), ["RES0402"]);

        let dir = tempfile::tempdir().unwrap();
        let lib = UserLibrary::new("user", dir.path());
        lib.write(&Item::Block(lb.clone())).unwrap();
        assert_eq!(lib.read(ItemKind::Block, "div").unwrap(), Some(Item::Block(lb.clone())));

        // A block file missing one of its parts is rejected.
        let mut broken = lb;
        broken.parts.clear();
        lib.write(&Item::Block(broken)).unwrap();
        assert!(matches!(lib.read(ItemKind::Block, "div"), Err(LibError::Invalid(..))));
    }

    #[test]
    fn search_order_and_selection() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let team = dir.path().join("team");
        let libs = Libraries::with_dirs(Some(a.clone()), &[team.clone(), a.clone(), dir.path().join("x/team")]);
        let names: Vec<&str> = libs.list.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names[..2], ["user", "team"]);
        assert_eq!(libs.list.len(), 3, "duplicate path skipped");
        assert!(libs.list[2].name.ends_with("x/team"), "name clash falls back to the path");

        libs.list[0].write(&Item::Part(part("P", "F"))).unwrap();
        libs.list[1].write(&Item::Part(part("P", "G"))).unwrap();
        libs.list[1].write(&Item::Footprint(footprint("P"))).unwrap();
        let found = libs.find(&ItemKind::ALL, "p").unwrap();
        let got: Vec<(&str, ItemKind)> = found.iter().map(|(l, k, _)| (l.name.as_str(), *k)).collect();
        assert_eq!(got, [("user", ItemKind::Part), ("team", ItemKind::Part), ("team", ItemKind::Footprint)]);
        assert_eq!(libs.find_part_by_mpn("mpn-p").unwrap().unwrap().0.name, "user");

        assert_eq!(libs.select("team").unwrap().path, team);
        assert!(libs.select("nope").is_none());
        let adhoc = libs.select(&dir.path().join("adhoc").display().to_string()).unwrap();
        assert_eq!(adhoc.path, dir.path().join("adhoc"));
    }
}
