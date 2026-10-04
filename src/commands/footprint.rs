//! `footprint.*`: land patterns in the project library.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util;
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::geom::BBox;
use crate::landpattern::{self, ChipKind, Density, GenOptions, PackageSpec};
use crate::model::Project;
use crate::model::footprint::{Footprint, Mount};
use crate::model::model3d::{MAX_MODEL_BYTES, Model3d, ModelData, ModelUnit, ModelUp, valid_model_name};
use crate::model::part::valid_id;
use crate::models3d::{self, ModelError3d};
use crate::refs::ObjectRef;
use crate::suggest::did_you_mean;
use crate::units::{Angle, Nm, Scale};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Generate>()
        .register::<List>()
        .register::<Show>()
        .register::<Remove>()
        .register::<ModelSet>()
        .register::<ModelClear>()
        .register::<ModelList>();
}

/// Short description of a footprint.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FootprintSummary {
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// SMD or THT.
    pub mount: Mount,
    /// Number of pads.
    pub pads: usize,
    /// Courtyard size (width, height).
    pub courtyard: (Nm, Nm),
    /// Parts using it.
    pub used_by: Vec<String>,
    /// Attached 3D model file, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl FootprintSummary {
    fn of(p: &Project, f: &Footprint) -> Self {
        let cy = BBox::of_points(f.courtyard.iter().copied())
            .map(|b| (b.width(), b.height()))
            .unwrap_or((Nm::ZERO, Nm::ZERO));
        let used_by = p
            .library()
            .parts
            .values()
            .filter(|part| part.footprints.iter().any(|r| r.footprint == f.name))
            .map(|part| part.id.clone())
            .collect();
        FootprintSummary {
            name: f.name.clone(),
            description: f.description.clone(),
            mount: f.mount,
            pads: f.pads.len(),
            courtyard: cy,
            used_by,
            model: f.model.as_ref().map(|m| m.file.clone()),
        }
    }

    fn line(&self) -> String {
        let mut s = format!(
            "{}  {} pads, courtyard {} x {}  {}",
            self.name, self.pads, self.courtyard.0, self.courtyard.1, self.description
        );
        if !self.used_by.is_empty() {
            s += &format!("  used by {}", self.used_by.join(", "));
        }
        if let Some(m) = &self.model {
            s += &format!("  model {m}");
        }
        s
    }
}

/// Generate a footprint (IPC-7351B) from a package name or datasheet dimensions.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Generate {
    /// Package name: "0402", "SOT-23-5", "SOIC-8", "TSSOP-20", "LQFP-48",
    /// "QFN-32 5x5mm P0.5mm EP3.1mm", "PinHeader 1x04", "SOT-223", "DPAK", "SOD-123", "SMA", "MINIMELF",
    /// "DIP-8", "BGA-64 8x8 P0.8mm 6x6mm".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Explicit dimensions from the datasheet, instead of `package`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<PackageSpec>,
    /// Body type for two-terminal chips (affects name and height): resistor, capacitor, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ChipKind>,
    /// IPC density level (default: nominal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<Density>,
    /// Store under this name instead of the IPC name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Overwrite an existing footprint with the same name.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

/// Result of `footprint.generate`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct GenerateResult {
    /// The footprint.
    #[serde(flatten)]
    pub footprint: FootprintSummary,
    /// `created`, `replaced` or `unchanged`.
    pub status: String,
}

impl Command for Generate {
    const NAME: &'static str = "footprint.generate";
    const SUMMARY: &'static str = "Generate an IPC-7351B footprint from a package name or datasheet dimensions";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["package"];
    type Output = GenerateResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<GenerateResult, CommandError> {
        let spec = match (&self.package, self.spec) {
            (Some(name), None) => landpattern::packages::parse(name, self.kind.unwrap_or_default())
                .map_err(|e| CommandError::invalid_args("footprint.unknown_package", e))?,
            (None, Some(spec)) => spec,
            _ => {
                return Err(CommandError::invalid_args("footprint.missing_package", "give either `package` or `spec`"));
            }
        };
        let opts = GenOptions { density: self.density.unwrap_or_default(), ..Default::default() };
        let mut fp = landpattern::generate(&spec, &opts)
            .map_err(|e| CommandError::invalid_args("footprint.invalid", e.to_string()))?;
        if let Some(n) = self.name {
            if !valid_id(&n) {
                return Err(CommandError::invalid_args(
                    "footprint.invalid_name",
                    format!("invalid footprint name `{n}`"),
                ));
            }
            fp.name = n;
        }
        let p = ctx.project_mut()?;
        let existing = p.library().find_footprint_ci(&fp.name).map(String::from);
        // Regenerating keeps an attached 3D model.
        if let Some(name) = &existing {
            fp.model = p.library().footprints[name].model.clone();
        }
        let status = match existing {
            Some(name) if p.library().footprints[&name] == fp => "unchanged",
            Some(name) if !self.replace => {
                return Err(CommandError::conflict(
                    "footprint.exists",
                    format!("footprint `{name}` already exists with different content"),
                )
                .with_hint("pass `replace: true` to overwrite it, or `name` to store under another name"));
            }
            Some(name) => {
                p.library_mut().footprints.remove(&name);
                p.library_mut().footprints.insert(fp.name.clone(), fp.clone());
                "replaced"
            }
            None => {
                p.library_mut().footprints.insert(fp.name.clone(), fp.clone());
                "created"
            }
        };
        let p = ctx.project()?;
        Ok(GenerateResult {
            footprint: FootprintSummary::of(p, &p.library().footprints[&fp.name]),
            status: status.into(),
        })
    }

    fn summarize(o: &GenerateResult) -> String {
        format!("{}: {}", o.status, o.footprint.line())
    }
}

/// List footprints in the project library.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {}

/// Footprints.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FootprintList {
    /// Footprints.
    pub footprints: Vec<FootprintSummary>,
}

impl Command for List {
    const NAME: &'static str = "footprint.list";
    const SUMMARY: &'static str = "List footprints in the project library";
    const KIND: CommandKind = CommandKind::Query;
    type Output = FootprintList;

    fn run(self, ctx: &mut Context<'_>) -> Result<FootprintList, CommandError> {
        let p = ctx.project()?;
        Ok(FootprintList { footprints: p.library().footprints.values().map(|f| FootprintSummary::of(p, f)).collect() })
    }

    fn summarize(o: &FootprintList) -> String {
        if o.footprints.is_empty() {
            return "no footprints".into();
        }
        o.footprints.iter().map(FootprintSummary::line).collect::<Vec<_>>().join("\n")
    }
}

/// Show a footprint in full: pads and graphics.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Show {
    /// Footprint name.
    pub name: String,
}

impl Command for Show {
    const NAME: &'static str = "footprint.show";
    const SUMMARY: &'static str = "Show a footprint: pads, courtyard, graphics";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Footprint;

    fn run(self, ctx: &mut Context<'_>) -> Result<Footprint, CommandError> {
        Ok(util::footprint(ctx.project()?, &self.name)?.clone())
    }

    fn summarize(f: &Footprint) -> String {
        let mut s = format!("{} ({:?}): {}", f.name, f.mount, f.description);
        for p in &f.pads {
            let (w, h) = p.shape.size();
            s += &format!("\n  pad {:>3} at ({}, {}) {} x {}", p.number, p.at.x, p.at.y, w, h);
        }
        s
    }
}

/// Remove an unused footprint.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Remove {
    /// Footprint name.
    pub name: String,
}

/// Result of `footprint.remove`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Removed {
    /// Removed footprint.
    pub name: String,
}

impl Command for Remove {
    const NAME: &'static str = "footprint.remove";
    const SUMMARY: &'static str = "Remove a footprint no part uses";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Removed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Removed, CommandError> {
        let p = ctx.project()?;
        let f = util::footprint(p, &self.name)?;
        let summary = FootprintSummary::of(p, f);
        if !summary.used_by.is_empty() {
            return Err(CommandError::conflict(
                "footprint.in_use",
                format!("footprint `{}` is used by {}", f.name, summary.used_by.join(", ")),
            ));
        }
        let name = f.name.clone();
        let lib = ctx.project_mut()?.library_mut();
        lib.footprints.remove(&name);
        lib.prune_models();
        Ok(Removed { name })
    }

    fn summarize(o: &Removed) -> String {
        format!("removed footprint {}", o.name)
    }
}

// ---------------------------------------------------------------------------------------------
// 3D models (DECISIONS D36).

/// Attach a 3D model to a footprint (or to one part's use of it) for 3D renders and STEP/IDF
/// exports. The file is copied into the project library (`library/models/`); formats: STL,
/// OBJ, glTF/GLB, USDZ (STEP and VRML once the oxideav decoders are published).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelSet {
    /// Footprint name.
    pub footprint: String,
    /// Model file: a path (relative to the project) or the name of a model already in the
    /// project library (`footprint.model_list`).
    pub file: String,
    /// Attach to this part's use of the footprint instead of the footprint itself (a part model
    /// overrides the footprint's for components of that part).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// Store the file in the library under this name (with its extension). Default: the file
    /// name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Offset of the model origin: X, Y in footprint coordinates, Z up from the board
    /// (`["0mm", "0mm", "0.1mm"]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<[Nm; 3]>,
    /// Rotation about X, Y and Z in degrees, applied in that order (`[90, 0, 180]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<[Angle; 3]>,
    /// Scale: one factor for all axes or one per axis (`[2.54]` or `[1, 1, 0.5]`), applied after
    /// the unit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scale: Vec<Scale>,
    /// Unit of the model's coordinates when the file's own is wrong (STL is read as mm, glTF
    /// and OBJ as m).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<ModelUnit>,
    /// Model axis pointing away from the board when the file's own is wrong (STL: z; glTF,
    /// OBJ: y).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up: Option<ModelUp>,
    /// Overwrite a different library model with the same name.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

/// Facts about a model file, from decoding it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ModelFacts {
    /// Format id (`stl`, `gltf`, ...), when this build can decode the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// File size in bytes.
    pub bytes: usize,
    /// Triangles, when decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triangles: Option<usize>,
    /// Placed extent in footprint coordinates (min X, Y, Z), when decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<[Nm; 3]>,
    /// Placed extent (max X, Y, Z), when decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<[Nm; 3]>,
    /// Why it could not be decoded (diagnostic code and message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn nm3(v: [f64; 3]) -> [Nm; 3] {
    v.map(|x| Nm((x * 1e6).round() as i64))
}

impl ModelFacts {
    /// Decodes the model `m` in project `p`.
    fn of(p: &Project, m: &Model3d) -> ModelFacts {
        let bytes = p.library().models.get(&m.file).map_or(0, |d| d.len());
        let format = models3d::format_of(&m.file);
        match models3d::load(p, m) {
            Ok(f) => ModelFacts {
                format,
                bytes,
                triangles: Some(f.tris.len()),
                min: Some(nm3(f.min)),
                max: Some(nm3(f.max)),
                error: None,
            },
            Err(e) => ModelFacts { format, bytes, error: Some(format!("{}: {e}", e.code())), ..Default::default() },
        }
    }

    fn text(&self) -> String {
        let mut s = format!("{}, {} bytes", self.format.as_deref().unwrap_or("?"), self.bytes);
        if let Some(t) = self.triangles {
            s += &format!(", {t} triangles");
        }
        if let (Some(a), Some(b)) = (self.min, self.max) {
            s += &format!(", {} x {} x {} (z {}..{})", b[0] - a[0], b[1] - a[1], b[2] - a[2], a[2], b[2]);
        }
        if let Some(e) = &self.error {
            s += &format!(" [{e}]");
        }
        s
    }
}

/// Result of `footprint.model_set` / `footprint.model_clear`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ModelAttached {
    /// Footprint.
    pub footprint: String,
    /// Part, when the model is the part's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// The reference as stored (absent after `model_clear`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model3d>,
    /// The model file, decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<ModelFacts>,
    /// Library model files removed because nothing uses them any more.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pruned: Vec<String>,
}

impl ModelAttached {
    fn target(&self) -> String {
        match &self.part {
            Some(p) => format!("part {p} on {}", self.footprint),
            None => format!("footprint {}", self.footprint),
        }
    }
}

fn model_error(e: &ModelError3d) -> CommandError {
    CommandError::invalid_args(e.code(), e.to_string()).with_hint(e.hint())
}

/// The model slot of a footprint or of a part's reference to it.
fn model_slot<'a>(
    p: &'a mut Project,
    footprint: &str,
    part: Option<&str>,
) -> Result<&'a mut Option<Model3d>, CommandError> {
    match part {
        None => Ok(&mut p.library_mut().footprints.get_mut(footprint).expect("resolved").model),
        Some(id) => {
            let part = p.library_mut().parts.get_mut(id).expect("resolved");
            let pid = part.id.clone();
            let listed: Vec<String> = part.footprints.iter().map(|r| r.footprint.clone()).collect();
            match part.footprints.iter_mut().find(|r| r.footprint == footprint) {
                Some(r) => Ok(&mut r.model),
                None => Err(CommandError::invalid_args(
                    "model.part_footprint",
                    format!("part `{pid}` does not use footprint `{footprint}` (it uses {})", listed.join(", ")),
                )
                .with_subject(ObjectRef::Part { scheme: "local".into(), id: pid })
                .with_hint("give one of the part's footprints, or leave out `part` to attach to the footprint")),
            }
        }
    }
}

/// Resolves the footprint and optional part names (exact keys).
fn model_target(p: &Project, footprint: &str, part: Option<&str>) -> Result<(String, Option<String>), CommandError> {
    let fp = util::footprint(p, footprint)?.name.clone();
    let part = part.map(|id| util::part(p, id).map(|x| x.id.clone())).transpose()?;
    Ok((fp, part))
}

/// Reads a model file from disk, or takes it from the project library: (default name, bytes).
fn model_source(ctx: &Context<'_>, file: &str) -> Result<(String, ModelData), CommandError> {
    let path = match ctx.session.root() {
        Some(root) if Path::new(file).is_relative() => root.join(file),
        _ => PathBuf::from(file),
    };
    let io = |e| CommandError::from(crate::model::ModelError::Io { path: path.clone(), source: e });
    if path.is_file() {
        let len = std::fs::metadata(&path).map_err(io)?.len();
        if len > MAX_MODEL_BYTES as u64 {
            return Err(CommandError::invalid_args(
                "model.too_large",
                format!("`{file}` is {} MiB; the limit is {} MiB", len >> 20, MAX_MODEL_BYTES >> 20),
            )
            .with_hint("simplify the model (fewer triangles) or use a binary format (binary STL, GLB)"));
        }
        let bytes = std::fs::read(&path).map_err(io)?;
        let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok((name, ModelData::new(bytes)));
    }
    let lib = ctx.project()?.library();
    if let Some(n) = lib.find_model_ci(file) {
        return Ok((n.to_string(), lib.models[n].clone()));
    }
    Err(CommandError::not_found(
        "model.file_not_found",
        format!("no file `{file}` (looked for {} and in the project's library/models)", path.display()),
    )
    .with_suggestions(&did_you_mean(file, lib.models.keys().map(String::as_str), 3))
    .with_hint_if_none("give a path to an STL, OBJ, glTF/GLB or USDZ file (relative to the project)"))
}

impl Command for ModelSet {
    const NAME: &'static str = "footprint.model_set";
    const SUMMARY: &'static str = "Attach a 3D model (STL, OBJ, glTF/GLB, USDZ) to a footprint or a part's footprint";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["footprint", "file"];
    type Output = ModelAttached;

    fn run(self, ctx: &mut Context<'_>) -> Result<ModelAttached, CommandError> {
        let (fp_name, part) = model_target(ctx.project()?, &self.footprint, self.part.as_deref())?;
        let scale = match self.scale.as_slice() {
            [] => [Scale::ONE; 3],
            [s] => [*s; 3],
            [x, y, z] => [*x, *y, *z],
            _ => {
                return Err(CommandError::invalid_args("model.bad_scale", "give one scale factor or three (X, Y, Z)")
                    .with_hint("e.g. `scale: [2.54]` or `scale: [1, 1, 0.5]`"));
            }
        };
        if scale.iter().any(|s| s.0 <= 0) {
            return Err(CommandError::invalid_args("model.bad_scale", "scale factors must be positive")
                .with_hint("turn the model with `rotation` instead of mirroring it"));
        }
        // Check the format from the name first: a STEP file gets its diagnostic before any I/O.
        let given = self.name.clone().unwrap_or_else(|| {
            Path::new(&self.file).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
        });
        models3d::check_format(&given).map_err(|e| model_error(&e))?;
        let (default_name, data) = model_source(ctx, &self.file)?;
        let name = self.name.clone().unwrap_or(default_name);
        if !valid_model_name(&name) {
            return Err(CommandError::invalid_args(
                "model.invalid_name",
                format!("invalid model file name `{name}` (letters, digits, `. _ + -`, with an extension)"),
            )
            .with_hint("pass `name`, e.g. `SOT-23-5.stl`"));
        }
        let model = Model3d {
            file: name.clone(),
            offset: self.offset.unwrap_or([Nm::ZERO; 3]),
            rotation: self.rotation.unwrap_or([Angle::ZERO; 3]),
            scale,
            unit: self.unit,
            up: self.up,
        };

        let p = ctx.project_mut()?;
        if let Some(old) = p.library().models.get(&name)
            && *old != data
            && !self.replace
        {
            let users = p.library().model_users(&name);
            return Err(CommandError::conflict(
                "model.exists",
                format!("the library already has a different model `{name}` (used by {})", users.join(", ")),
            )
            .with_hint(
                "pass `name` to store it under another name, or `replace: true` to overwrite it for every user",
            ));
        }
        p.library_mut().models.insert(name.clone(), data);
        // Validate by decoding (the transaction rolls the library back on error).
        if cfg!(feature = "models3d")
            && let Err(e) = models3d::load(p, &model)
        {
            return Err(model_error(&e));
        }
        let facts = ModelFacts::of(p, &model);
        *model_slot(p, &fp_name, part.as_deref())? = Some(model.clone());
        let pruned = p.library_mut().prune_models();

        // A model far from the footprint's body size is usually a unit mistake.
        if let (Some(body), Some(a), Some(b)) = (p.library().footprints[&fp_name].body, facts.min, facts.max) {
            let size = (b[0] - a[0]).0.max((b[1] - a[1]).0);
            let want = body.width.0.max(body.length.0);
            if size > 0 && want > 0 && !(0.1..=10.0).contains(&(size as f64 / want as f64)) {
                ctx.report(
                    crate::Diagnostic::warning(
                        "model.size_mismatch",
                        format!(
                            "model `{name}` spans {} but the footprint body is {} x {}",
                            Nm(size),
                            body.width,
                            body.length
                        ),
                    )
                    .with_hint("set `unit` (mm, in, m, ...) or `scale` so the model matches the package"),
                );
            }
        }
        Ok(ModelAttached { footprint: fp_name, part, model: Some(model), facts: Some(facts), pruned })
    }

    fn summarize(o: &ModelAttached) -> String {
        let m = o.model.as_ref().map_or("", |m| m.file.as_str());
        let mut s = format!("{}: model {m}", o.target());
        if let Some(f) = &o.facts {
            s += &format!(" ({})", f.text());
        }
        if !o.pruned.is_empty() {
            s += &format!("; removed unused {}", o.pruned.join(", "));
        }
        s
    }
}

/// Detach the 3D model from a footprint (or from a part's use of it); renders and exports go
/// back to generated bodies. Model files nothing uses any more leave the project library.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelClear {
    /// Footprint name.
    pub footprint: String,
    /// The part whose own model to remove (default: the footprint's model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
}

impl Command for ModelClear {
    const NAME: &'static str = "footprint.model_clear";
    const SUMMARY: &'static str = "Detach the 3D model from a footprint or a part's footprint";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["footprint"];
    type Output = ModelAttached;

    fn run(self, ctx: &mut Context<'_>) -> Result<ModelAttached, CommandError> {
        let (fp_name, part) = model_target(ctx.project()?, &self.footprint, self.part.as_deref())?;
        let p = ctx.project_mut()?;
        let old = model_slot(p, &fp_name, part.as_deref())?.take();
        if old.is_none() {
            let what = match &part {
                Some(id) => format!("part `{id}` has no model of its own on footprint `{fp_name}`"),
                None => format!("footprint `{fp_name}` has no model"),
            };
            return Err(CommandError::not_found("model.not_set", what)
                .with_hint("list attached models with `footprint.model_list`"));
        }
        let pruned = p.library_mut().prune_models();
        Ok(ModelAttached { footprint: fp_name, part, model: None, facts: None, pruned })
    }

    fn summarize(o: &ModelAttached) -> String {
        let mut s = format!("{}: model removed", o.target());
        if !o.pruned.is_empty() {
            s += &format!("; removed unused {}", o.pruned.join(", "));
        }
        s
    }
}

/// List 3D models: the files in the project library, what uses them, and the formats this
/// build reads.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelList {}

/// A model file in the project library.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ModelFileInfo {
    /// File name (`library/models/<name>`).
    pub name: String,
    /// Decoded facts, placed as its first user places it.
    #[serde(flatten)]
    pub facts: ModelFacts,
    /// Footprints and parts using it.
    pub used_by: Vec<String>,
}

/// One attachment.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ModelUse {
    /// Footprint.
    pub footprint: String,
    /// Part, for a part's own model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// The reference.
    pub model: Model3d,
}

/// Result of `footprint.model_list`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ModelListing {
    /// Model files.
    pub files: Vec<ModelFileInfo>,
    /// Attachments: footprints, then parts.
    pub uses: Vec<ModelUse>,
    /// Formats this build decodes.
    pub formats: Vec<models3d::FormatInfo>,
    /// Formats announced upstream (oxideav) but not available yet.
    pub pending: Vec<String>,
}

impl Command for ModelList {
    const NAME: &'static str = "footprint.model_list";
    const SUMMARY: &'static str = "List 3D model files, the footprints and parts using them, and readable formats";
    const KIND: CommandKind = CommandKind::Query;
    type Output = ModelListing;

    fn run(self, ctx: &mut Context<'_>) -> Result<ModelListing, CommandError> {
        let p = ctx.project()?;
        let lib = p.library();
        let mut uses = Vec::new();
        for f in lib.footprints.values() {
            if let Some(m) = &f.model {
                uses.push(ModelUse { footprint: f.name.clone(), part: None, model: m.clone() });
            }
        }
        for part in lib.parts.values() {
            for r in &part.footprints {
                if let Some(m) = &r.model {
                    uses.push(ModelUse {
                        footprint: r.footprint.clone(),
                        part: Some(part.id.clone()),
                        model: m.clone(),
                    });
                }
            }
        }
        let files = lib
            .models
            .keys()
            .map(|name| {
                let m =
                    uses.iter().find(|u| &u.model.file == name).map_or_else(|| Model3d::new(name), |u| u.model.clone());
                ModelFileInfo { name: name.clone(), facts: ModelFacts::of(p, &m), used_by: lib.model_users(name) }
            })
            .collect();
        let pending =
            models3d::PENDING_FORMATS.iter().map(|(f, exts)| format!("{f} (.{})", exts.join(", ."))).collect();
        Ok(ModelListing { files, uses, formats: models3d::formats(), pending })
    }

    fn summarize(o: &ModelListing) -> String {
        let mut lines: Vec<String> = o
            .files
            .iter()
            .map(|f| {
                let users = if f.used_by.is_empty() { "unused".into() } else { f.used_by.join(", ") };
                format!("{}  {}  used by {users}", f.name, f.facts.text())
            })
            .collect();
        if lines.is_empty() {
            lines.push("no 3D models".into());
        }
        let fmts: Vec<String> = o.formats.iter().flat_map(|f| f.extensions.iter().map(|e| format!(".{e}"))).collect();
        lines.push(format!(
            "readable: {}; pending upstream: {}",
            if fmts.is_empty() { "none (feature `models3d` off)".to_string() } else { fmts.join(", ") },
            o.pending.join(", ")
        ));
        lines.join("\n")
    }
}
