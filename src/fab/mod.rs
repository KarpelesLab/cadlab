//! Fab profiles: what a manufacturer can make and how it wants its files (DECISIONS D11, D12).
//! See `docs/MANUFACTURING.md`.
//!
//! A profile is a TOML data file. Built-in profiles live in `fab-profiles/` (embedded in the
//! binary); users add or override profiles with `*.toml` files in `<config dir>/fab-profiles/`
//! (`~/.config/cadlab/fab-profiles`). A user file whose `id` (or file stem) matches a built-in
//! profile is merged onto it: tables merge key by key, any other value (arrays included)
//! replaces the built-in one. Every value carries its source (`sources`, per-process `cite`)
//! and the date it was verified; values that could not be confirmed are listed in
//! `unverified`.
//!
//! Nothing here is stored in projects: profiles are applied by `fab.check`, `fab.compare` and
//! `fab.export`, and the choices made are recorded in `fab-lock.json` ([`export`]).

pub mod check;
pub mod export;
pub mod rules;
mod sha256;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::units::{Angle, Nm};

pub use sha256::sha256_hex;

/// Built-in profiles: (file name, TOML text).
const BUILTIN: &[(&str, &str)] = &[
    ("jlcpcb.toml", include_str!("../../fab-profiles/jlcpcb.toml")),
    ("pcbway.toml", include_str!("../../fab-profiles/pcbway.toml")),
    ("generic.toml", include_str!("../../fab-profiles/generic.toml")),
];

/// A manufacturer's capabilities and file conventions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FabProfile {
    /// Stable ID (`jlcpcb`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Home page.
    pub website: String,
    /// Date the values were last checked against the sources (`YYYY-MM-DD`).
    pub verified_at: String,
    /// Pages the values come from.
    pub sources: Vec<String>,
    /// Free-form notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// PCB processes offered, preferred first.
    #[serde(rename = "process")]
    pub processes: Vec<Process>,
    /// File conventions.
    #[serde(default)]
    pub output: Output,
    /// Assembly service, when offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assembly: Option<Assembly>,
}

/// A PCB process (e.g. standard 1–2 layer FR-4). Limits left out are not checked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Process {
    /// Process ID within the profile (`standard`, `multilayer`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Copper layer counts offered.
    pub layers: Vec<u8>,
    /// Finished board thickness options (empty: not checked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thickness: Vec<Nm>,
    /// Outer copper thickness options (35 µm = 1 oz; empty: not checked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outer_copper: Vec<Nm>,
    /// Inner copper thickness options (empty: not checked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inner_copper: Vec<Nm>,
    /// Minimum track width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_track: Option<Nm>,
    /// Minimum copper spacing (track to track, track to pad, pad to pad).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_space: Option<Nm>,
    /// Minimum mechanical drill (finished hole) for vias and plated holes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_drill: Option<Nm>,
    /// Maximum drill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_drill: Option<Nm>,
    /// Minimum non-plated hole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_npth: Option<Nm>,
    /// Minimum via annular ring ((pad − drill) / 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_via_ring: Option<Nm>,
    /// Minimum annular ring of plated through-hole pads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_pth_ring: Option<Nm>,
    /// Minimum hole-to-hole distance, edge to edge (any holes; vias in practice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hole_to_hole: Option<Nm>,
    /// Minimum distance between two component (pad) holes, edge to edge, when larger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_hole_to_hole: Option<Nm>,
    /// Minimum copper-to-board-edge distance (routed outline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copper_to_edge: Option<Nm>,
    /// Minimum silkscreen line width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_silk_width: Option<Nm>,
    /// Minimum silkscreen text height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_silk_height: Option<Nm>,
    /// Minimum silkscreen-to-pad distance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silk_to_pad: Option<Nm>,
    /// Minimum solder mask dam (web between openings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_dam: Option<Nm>,
    /// Via-in-pad (filled and capped vias) offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_in_pad: Option<bool>,
    /// Castellated holes offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub castellated: Option<bool>,
    /// Surface finishes (empty: not checked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finishes: Vec<String>,
    /// Solder mask colors (empty: not checked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mask_colors: Vec<String>,
    /// Silkscreen colors (empty: not checked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub silk_colors: Vec<String>,
    /// Largest board (either orientation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<[Nm; 2]>,
    /// Smallest board (either orientation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<[Nm; 2]>,
    /// Source URL per field, where it differs from the profile's first source.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cite: BTreeMap<String, String>,
    /// Fields whose value could not be confirmed on the sources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
}

/// Fields of [`Process`] that `cite` and `unverified` may name.
const PROCESS_FIELDS: &[&str] = &[
    "layers",
    "thickness",
    "outer_copper",
    "inner_copper",
    "min_track",
    "min_space",
    "min_drill",
    "max_drill",
    "min_npth",
    "min_via_ring",
    "min_pth_ring",
    "hole_to_hole",
    "pad_hole_to_hole",
    "copper_to_edge",
    "min_silk_width",
    "min_silk_height",
    "silk_to_pad",
    "mask_dam",
    "via_in_pad",
    "castellated",
    "finishes",
    "mask_colors",
    "silk_colors",
    "max_size",
    "min_size",
];

/// Kinds of fabrication file, as named in [`Output::names`] and [`Output::include`].
pub const FILE_KINDS: &[&str] = &[
    "copper_top",
    "copper_inner",
    "copper_bottom",
    "mask_top",
    "mask_bottom",
    "paste_top",
    "paste_bottom",
    "silk_top",
    "silk_bottom",
    "profile",
    "component_top",
    "component_bottom",
    "drill_pth",
    "drill_npth",
    "drill_span",
    "ipc356",
];

/// File conventions of the fab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// File kinds to produce (see [`FILE_KINDS`]); `copper_top` etc. Groups: `copper` (all
    /// copper layers), `mask`, `paste`, `silk`, `component`, `drill` (all drill files).
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    /// File name templates by kind; kinds left out use cadlab's generic names. Placeholders:
    /// `{project}`, `{layer}` (copper layer number, top = 1), `{n}` (inner layer number,
    /// 1-based), `{from}`/`{to}` (drill span layers).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, String>,
    /// Name of the zip archive holding the fabrication files (`{project}`, `{fab}`).
    #[serde(default = "default_archive")]
    pub archive: String,
    /// Drill file format, informative (`excellon`: XNC, metric, PTH and NPTH separate).
    #[serde(default = "default_drill_format")]
    pub drill_format: String,
    /// Source URL per field.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cite: BTreeMap<String, String>,
    /// Fields whose value could not be confirmed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
}

fn default_include() -> Vec<String> {
    ["copper", "mask", "paste", "silk", "profile", "drill"].map(String::from).to_vec()
}

fn default_archive() -> String {
    "{project}-{fab}.zip".into()
}

fn default_drill_format() -> String {
    "excellon".into()
}

impl Default for Output {
    fn default() -> Self {
        Output {
            include: default_include(),
            names: BTreeMap::new(),
            archive: default_archive(),
            drill_format: default_drill_format(),
            cite: BTreeMap::new(),
            unverified: Vec::new(),
        }
    }
}

const OUTPUT_FIELDS: &[&str] = &["include", "names", "archive", "drill_format"];

/// Board sides for assembly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Top.
    Top,
    /// Bottom.
    Bottom,
}

/// Assembly service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Assembly {
    /// Sides that can be assembled.
    pub sides: Vec<Side>,
    /// Smallest chip package assembled (imperial size code: `01005`, `0201`, `0402`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_package: Option<String>,
    /// Through-hole parts assembled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through_hole: Option<bool>,
    /// Part classes of the fab's catalog (informative, e.g. basic / extended).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub part_classes: Vec<String>,
    /// Supplier provider IDs whose SKUs the fab orders by (`lcsc`); their SKUs fill the `sku`
    /// BOM column. Empty: the fab sources by MPN.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catalog: Vec<String>,
    /// BOM file layout.
    pub bom: BomLayout,
    /// Placement (CPL / centroid) file layout.
    pub cpl: CplLayout,
    /// Per-package rotation offsets added to the placement rotation at export.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rotation_offsets: Vec<RotationOffset>,
    /// Source URL per field.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cite: BTreeMap<String, String>,
    /// Fields whose value could not be confirmed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
}

const ASSEMBLY_FIELDS: &[&str] =
    &["sides", "min_package", "through_hole", "part_classes", "catalog", "bom", "cpl", "rotation_offsets"];

/// Values a BOM column can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BomField {
    /// Line number (1-based).
    Line,
    /// Populated quantity.
    Quantity,
    /// Designators, comma separated.
    Designators,
    /// Value (`10k`, `AP2112K-3.3TRG1`).
    Value,
    /// Description.
    Description,
    /// Value and description.
    ValueDescription,
    /// Package (`0402`, `SOT-23-5`).
    Package,
    /// Footprint name.
    Footprint,
    /// Manufacturer of the chosen MPN.
    Manufacturer,
    /// Chosen MPN.
    Mpn,
    /// Fab catalog SKU (from the profile's `catalog` providers).
    Sku,
    /// `SMD` or `THT`.
    Mount,
    /// Notes.
    Notes,
    /// Always empty.
    Empty,
}

/// Values a placement column can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CplField {
    /// Designator.
    Designator,
    /// Value.
    Value,
    /// Package.
    Package,
    /// Footprint name.
    Footprint,
    /// X of the footprint origin.
    X,
    /// Y of the footprint origin.
    Y,
    /// Side, written with `side_names`.
    Side,
    /// Rotation in degrees, offsets applied.
    Rotation,
    /// Always empty.
    Empty,
}

/// One CSV column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Column<F> {
    /// Header text.
    pub header: String,
    /// Content.
    pub field: F,
}

/// BOM CSV layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BomLayout {
    /// File name template (`{project}`, `{fab}`).
    pub file: String,
    /// Text of the `mount` field for SMD and through-hole parts (default `SMD`, `THT`).
    #[serde(default = "default_mount_names")]
    pub mount_names: [String; 2],
    /// Columns in order.
    pub columns: Vec<Column<BomField>>,
}

fn default_mount_names() -> [String; 2] {
    ["SMD".into(), "THT".into()]
}

/// Placement origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Board coordinates, as in the Gerber files.
    #[default]
    Board,
    /// Lower-left corner of the outline's bounding box.
    OutlineLowerLeft,
}

/// Placement CSV layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CplLayout {
    /// File name template (`{project}`, `{fab}`).
    pub file: String,
    /// Columns in order.
    pub columns: Vec<Column<CplField>>,
    /// Text for the top and bottom sides.
    pub side_names: [String; 2],
    /// Suffix appended to coordinates (`mm`), or none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub coordinate_suffix: String,
    /// Origin of X/Y.
    #[serde(default)]
    pub origin: Origin,
}

/// A rotation offset for packages matching a pattern.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RotationOffset {
    /// Package or footprint name pattern; `*` matches any run of characters, case-insensitive.
    pub package: String,
    /// Added to the placement rotation (counter-clockwise).
    pub offset: Angle,
    /// Source of this offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl RotationOffset {
    /// Whether the pattern matches `name`.
    pub fn matches(&self, name: &str) -> bool {
        glob(&self.package.to_ascii_lowercase(), &name.to_ascii_lowercase())
    }
}

/// `*` wildcard match.
fn glob(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((head, rest)) => {
            let Some(s) = s.strip_prefix(head) else { return false };
            if rest.is_empty() {
                return true;
            }
            (0..=s.len()).filter(|&i| s.is_char_boundary(i)).any(|i| glob(rest, &s[i..]))
        }
    }
}

/// Error loading a profile.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// The file is not valid TOML or does not match the format.
    #[error("{source_name}: {message}")]
    Invalid {
        /// File or built-in name.
        source_name: String,
        /// Details.
        message: String,
    },
    /// The file could not be read.
    #[error("{0}: {1}")]
    Io(String, String),
}

impl FabProfile {
    /// Parses and validates a profile.
    pub fn parse(text: &str, source_name: &str) -> Result<FabProfile, ProfileError> {
        let v: toml::Table = toml::from_str(text).map_err(|e| invalid(source_name, e.to_string()))?;
        Self::from_table(v, source_name)
    }

    fn from_table(v: toml::Table, source_name: &str) -> Result<FabProfile, ProfileError> {
        let p: FabProfile = v.try_into().map_err(|e: toml::de::Error| invalid(source_name, e.to_string()))?;
        p.validate().map_err(|m| invalid(source_name, m))?;
        Ok(p)
    }

    /// Consistency checks beyond the types.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() || !self.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(format!("id `{}` must be lowercase letters, digits and dashes", self.id));
        }
        let d = self.verified_at.as_bytes();
        if d.len() != 10
            || d[4] != b'-'
            || d[7] != b'-'
            || !d.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        {
            return Err(format!("verified_at `{}` must be YYYY-MM-DD", self.verified_at));
        }
        if self.sources.is_empty() {
            return Err("sources must list at least one URL".into());
        }
        if self.processes.is_empty() {
            return Err("at least one [[process]] is required".into());
        }
        let mut ids = Vec::new();
        for p in &self.processes {
            if ids.contains(&&p.id) {
                return Err(format!("duplicate process `{}`", p.id));
            }
            ids.push(&p.id);
            if p.layers.is_empty() {
                return Err(format!("process `{}`: layers is empty", p.id));
            }
            fields_known(&format!("process `{}`", p.id), PROCESS_FIELDS, &p.cite, &p.unverified)?;
        }
        fields_known("output", OUTPUT_FIELDS, &self.output.cite, &self.output.unverified)?;
        for k in &self.output.include {
            if !FILE_KINDS.contains(&k.as_str()) && !GROUPS.contains(&k.as_str()) {
                return Err(format!("output.include: unknown file kind `{k}`"));
            }
        }
        for k in self.output.names.keys() {
            if !FILE_KINDS.contains(&k.as_str()) {
                return Err(format!("output.names: unknown file kind `{k}`"));
            }
        }
        if let Some(a) = &self.assembly {
            fields_known("assembly", ASSEMBLY_FIELDS, &a.cite, &a.unverified)?;
            if let Some(m) = &a.min_package
                && chip_size(m).is_none()
            {
                return Err(format!("assembly.min_package `{m}` is not a known chip size code"));
            }
            if a.bom.columns.is_empty() || a.cpl.columns.is_empty() {
                return Err("assembly: bom and cpl need at least one column".into());
            }
        }
        Ok(())
    }

    /// A process by ID.
    pub fn process(&self, id: &str) -> Option<&Process> {
        self.processes.iter().find(|p| p.id == id)
    }

    /// The process to use for a board with `layers` copper layers: the first that offers it.
    pub fn process_for(&self, layers: u8) -> Option<&Process> {
        self.processes.iter().find(|p| p.layers.contains(&layers))
    }

    /// Every value listed as unverified, as `process.<id>.<field>`, `output.<field>`, `assembly.<field>`.
    pub fn unverified(&self) -> Vec<String> {
        let mut v = Vec::new();
        for p in &self.processes {
            v.extend(p.unverified.iter().map(|f| format!("process.{}.{f}", p.id)));
        }
        v.extend(self.output.unverified.iter().map(|f| format!("output.{f}")));
        if let Some(a) = &self.assembly {
            v.extend(a.unverified.iter().map(|f| format!("assembly.{f}")));
        }
        v
    }
}

const GROUPS: &[&str] = &["copper", "mask", "paste", "silk", "component", "drill"];

fn fields_known(what: &str, known: &[&str], cite: &BTreeMap<String, String>, unv: &[String]) -> Result<(), String> {
    for f in cite.keys().chain(unv) {
        // `cpl.origin` names a sub-field of `cpl`.
        let head = f.split('.').next().unwrap_or_default();
        if !known.contains(&head) {
            return Err(format!("{what}: `{f}` in cite/unverified is not a field"));
        }
    }
    Ok(())
}

fn invalid(source_name: &str, message: String) -> ProfileError {
    ProfileError::Invalid { source_name: source_name.to_string(), message }
}

/// Nominal body size (length, width) of an imperial chip size code.
pub fn chip_size(code: &str) -> Option<(Nm, Nm)> {
    let um = |l: i64, w: i64| Some((Nm::from_um(l), Nm::from_um(w)));
    match code {
        "008004" => um(250, 125),
        "01005" => um(400, 200),
        "0201" => um(600, 300),
        "0402" => um(1000, 500),
        "0603" => um(1600, 800),
        "0805" => um(2000, 1250),
        "1206" => um(3200, 1600),
        _ => None,
    }
}

/// Where a profile came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSource {
    /// Shipped with cadlab.
    Builtin,
    /// A user file.
    User,
    /// A built-in profile with user overrides merged in.
    Merged,
}

/// The available profiles, sorted by ID.
#[derive(Clone, Debug, Default)]
pub struct Profiles {
    /// Profiles with their origin.
    pub profiles: BTreeMap<String, (FabProfile, ProfileSource)>,
    /// Problems with user files (the files are skipped).
    pub warnings: Vec<ProfileError>,
}

/// Directory of user profiles (`<config dir>/fab-profiles`).
pub fn user_dir() -> Option<PathBuf> {
    crate::supplier::config_dir().map(|d| d.join("fab-profiles"))
}

impl Profiles {
    /// Built-in profiles merged with the user's.
    pub fn load() -> Profiles {
        Self::load_from(user_dir().as_deref())
    }

    /// Built-in profiles only.
    pub fn builtin() -> Profiles {
        Self::load_from(None)
    }

    /// Built-in profiles merged with `*.toml` files in `dir` (sorted by name).
    pub fn load_from(dir: Option<&Path>) -> Profiles {
        let mut tables: BTreeMap<String, (toml::Table, ProfileSource, String)> = BTreeMap::new();
        for (name, text) in BUILTIN {
            let t: toml::Table = toml::from_str(text).expect("built-in fab profile is valid TOML");
            let id = t.get("id").and_then(|v| v.as_str()).expect("built-in fab profile has an id").to_string();
            tables.insert(id, (t, ProfileSource::Builtin, format!("built-in {name}")));
        }
        let mut out = Profiles::default();
        let mut files: Vec<PathBuf> = dir
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            let shown = f.display().to_string();
            let text = match std::fs::read_to_string(&f) {
                Ok(t) => t,
                Err(e) => {
                    out.warnings.push(ProfileError::Io(shown, e.to_string()));
                    continue;
                }
            };
            let t: toml::Table = match toml::from_str(&text) {
                Ok(t) => t,
                Err(e) => {
                    out.warnings.push(invalid(&shown, e.to_string()));
                    continue;
                }
            };
            let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let id = t.get("id").and_then(|v| v.as_str()).map(String::from).unwrap_or(stem);
            match tables.remove(&id) {
                Some((mut base, origin, _)) => {
                    merge(&mut base, t);
                    let origin = if origin == ProfileSource::Builtin { ProfileSource::Merged } else { origin };
                    tables.insert(id, (base, origin, shown));
                }
                None => {
                    let mut t = t;
                    t.entry("id").or_insert(toml::Value::String(id.clone()));
                    tables.insert(id, (t, ProfileSource::User, shown));
                }
            }
        }
        for (id, (t, origin, shown)) in tables {
            match FabProfile::from_table(t, &shown) {
                Ok(p) => {
                    out.profiles.insert(id, (p, origin));
                }
                Err(e) => out.warnings.push(e),
            }
        }
        out
    }

    /// A profile by ID.
    pub fn get(&self, id: &str) -> Option<&FabProfile> {
        self.profiles.get(id).map(|(p, _)| p)
    }

    /// Profile IDs.
    pub fn ids(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }
}

/// Deep merge: tables merge per key, other values replace.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse_and_validate() {
        let ps = Profiles::builtin();
        assert!(ps.warnings.is_empty(), "{:?}", ps.warnings);
        assert_eq!(ps.ids(), ["generic", "jlcpcb", "pcbway"]);
        for (p, _) in ps.profiles.values() {
            assert!(p.sources.iter().all(|s| s.starts_with("https://") || s.starts_with("docs/")), "{}", p.id);
            assert!(p.process_for(2).is_some(), "{} makes 2-layer boards", p.id);
        }
    }

    #[test]
    fn glob_patterns() {
        let r = RotationOffset { package: "SOT-23*".into(), offset: Angle::from_deg(180), source: None };
        assert!(r.matches("SOT-23-5"));
        assert!(r.matches("sot-23"));
        assert!(!r.matches("SOT-223"));
        assert!(glob("*qfn*", "vqfn-20"));
        assert!(!glob("a*b", "ac"));
    }

    #[test]
    fn validation_errors() {
        let base = include_str!("../../fab-profiles/generic.toml");
        let bad = base.replace("verified_at = \"2026-10-04\"", "verified_at = \"Oct 4\"");
        assert!(FabProfile::parse(&bad, "t").unwrap_err().to_string().contains("YYYY-MM-DD"));
        let bad = format!("{base}\nunknown_key = 1\n");
        assert!(FabProfile::parse(&bad, "t").is_err());
        let bad = base.replacen("[[process]]", "[[process]]\nunverified = [\"nope\"]", 1);
        assert!(FabProfile::parse(&bad, "t").unwrap_err().to_string().contains("nope"));
    }

    #[test]
    fn user_override_and_new_profile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("jlcpcb.toml"), "notes = \"mine\"\n").unwrap();
        std::fs::write(dir.path().join("broken.toml"), "id = \"broken\"\nname = 3\n").unwrap();
        let generic = include_str!("../../fab-profiles/generic.toml").replace("id = \"generic\"", "id = \"myfab\"");
        std::fs::write(dir.path().join("myfab.toml"), generic).unwrap();
        let ps = Profiles::load_from(Some(dir.path()));
        assert_eq!(ps.ids(), ["generic", "jlcpcb", "myfab", "pcbway"]);
        assert_eq!(ps.get("jlcpcb").unwrap().notes.as_deref(), Some("mine"));
        assert_eq!(ps.profiles["jlcpcb"].1, ProfileSource::Merged);
        assert_eq!(ps.profiles["myfab"].1, ProfileSource::User);
        assert_eq!(ps.warnings.len(), 1);
        assert!(ps.warnings[0].to_string().contains("broken.toml"));
    }
}
