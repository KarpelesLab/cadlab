//! `export.*`: generic manufacturing outputs (Gerber X2/X3, XNC drill, pick-and-place,
//! IPC-D-356A, IPC-2581), mechanical CAD outputs (STEP, IDF, IDX), the Specctra DSN design for
//! external autorouters, simulation inputs (SPICE netlist, openEMS full-wave model), and `place.import_idx` (answering IDX change messages). Fab-specific bundles come with fab profiles (`export fab`, DECISIONS D12).

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::fabout::{self, Options, OutFile};
use crate::geom::Point;
use crate::model::Project;
use crate::refs::ObjectRef;
use crate::units::Nm;
use crate::value::{Quantity, Unit};
use crate::{mcad, openems};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Gerber>()
        .register::<Drill>()
        .register::<Pnp>()
        .register::<Ipc356>()
        .register::<Ipc2581>()
        .register::<All>()
        .register::<Step>()
        .register::<Idf>()
        .register::<Idx>()
        .register::<IdxChanges>()
        .register::<PlaceImportIdx>()
        .register::<ExportDsn>()
        .register::<Spice>()
        .register::<OpenEms>();
}

/// Default output directory, relative to the project.
const DEFAULT_DIR: &str = "out/fab";

/// A file written by an export.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WrittenFile {
    /// Path written.
    pub path: String,
    /// What it contains (Gerber `.FileFunction`, `PickPlace`, `TestNetlist`).
    pub function: String,
}

/// Result of an export.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Exported {
    /// Files written, in output order.
    pub files: Vec<WrittenFile>,
}

fn summary(o: &Exported) -> String {
    let mut s = format!("wrote {} file(s)", o.files.len());
    for f in &o.files {
        s.push_str(&format!("\n  {} ({})", f.path, f.function));
    }
    s
}

pub(crate) fn resolve(ctx: &Context<'_>, path: Option<&Path>, default: PathBuf) -> PathBuf {
    let p = path.map(Path::to_path_buf).unwrap_or(default);
    match ctx.session.root() {
        Some(root) if p.is_relative() => root.join(p),
        _ => p,
    }
}

fn io(path: &Path, e: std::io::Error) -> CommandError {
    crate::model::ModelError::Io { path: path.to_path_buf(), source: e }.into()
}

pub(crate) fn write(files: Vec<(PathBuf, OutFile)>) -> Result<Exported, CommandError> {
    let mut out = Vec::new();
    for (path, f) in files {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        }
        std::fs::write(&path, f.content.as_bytes()).map_err(|e| io(&path, e))?;
        out.push(WrittenFile { path: path.display().to_string(), function: f.function });
    }
    Ok(Exported { files: out })
}

fn write_dir(ctx: &Context<'_>, dir: Option<&Path>, files: Vec<OutFile>) -> Result<Exported, CommandError> {
    let dir = resolve(ctx, dir, PathBuf::from(DEFAULT_DIR));
    write(files.into_iter().map(|f| (dir.join(&f.name), f)).collect())
}

fn options(mask_expansion: Option<Nm>) -> Options {
    Options { mask_expansion, ..Options::default() }
}

/// Warnings shared by board outputs.
fn check_board(ctx: &mut Context<'_>) -> Result<(), CommandError> {
    let p = ctx.project()?;
    let no_outline = p.board().outline.contours.is_empty();
    if no_outline {
        ctx.report(
            Diagnostic::warning("export.no_outline", "the board has no outline; the profile layer is empty")
                .with_hint("set one with `board.outline`"),
        );
    }
    Ok(())
}

fn check_drills(p: &Project) -> Result<(), CommandError> {
    if fabout::excellon::too_many_tools(p) {
        return Err(CommandError::invalid_args(
            "export.too_many_tools",
            format!("a drill file would need more than {} tools", fabout::excellon::MAX_TOOLS),
        )
        .with_hint("use fewer distinct drill diameters (via and pad drills)"));
    }
    Ok(())
}

fn check_placed(ctx: &mut Context<'_>) -> Result<(), CommandError> {
    let p = ctx.project()?;
    let dnp = &p.bom().dnp;
    let unplaced: Vec<String> = p
        .circuit()
        .components
        .keys()
        .filter(|r| !dnp.contains(*r) && !p.board().footprints.contains_key(*r))
        .cloned()
        .collect();
    for r in unplaced {
        ctx.report(
            Diagnostic::warning("export.unplaced", format!("`{r}` is not placed and is missing from assembly outputs"))
                .with_subject(ObjectRef::Name(r))
                .with_hint("place it with `place.set` or `place.auto`, or mark it DNP with `bom.dnp`"),
        );
    }
    Ok(())
}

/// Write Gerber X2 layers: copper, solder mask, paste, legend, profile, plus X3 component layers.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Gerber {
    /// Output directory (default out/fab, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Solder mask opening growth per side for this export, instead of the board's `mask_expansion` (pads and footprints with their own margin keep it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_expansion: Option<Nm>,
}

impl Command for Gerber {
    const NAME: &'static str = "export.gerber";
    const SUMMARY: &'static str = "Write Gerber X2 layers (copper, mask, paste, silk, outline) and X3 component layers";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["dir"];
    type Output = Exported;

    fn run(self, ctx: &mut Context<'_>) -> Result<Exported, CommandError> {
        check_board(ctx)?;
        let files = fabout::gerbers(ctx.project()?, &options(self.mask_expansion));
        write_dir(ctx, self.dir.as_deref(), files)
    }

    fn summarize(o: &Exported) -> String {
        summary(o)
    }
}

/// Write Excellon (XNC) drill files, plated and non-plated separately.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Drill {
    /// Output directory (default out/fab, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Also write Gerber X2 drill files.
    #[serde(default)]
    pub gerber: bool,
}

impl Command for Drill {
    const NAME: &'static str = "export.drill";
    const SUMMARY: &'static str = "Write Excellon/XNC drill files (PTH and NPTH), optionally Gerber X2 drill files";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["dir"];
    type Output = Exported;

    fn run(self, ctx: &mut Context<'_>) -> Result<Exported, CommandError> {
        let p = ctx.project()?;
        check_drills(p)?;
        let o = Options::default();
        let mut files = fabout::excellon::drills(p, &o);
        if self.gerber {
            files.extend(fabout::drill_gerbers(p, &o));
        }
        write_dir(ctx, self.dir.as_deref(), files)
    }

    fn summarize(o: &Exported) -> String {
        summary(o)
    }
}

/// Write the pick-and-place CSV (populated parts; mm from the outline's lower-left corner).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pnp {
    /// Output file (default `out/fab/<project>-pos.csv`, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl Command for Pnp {
    const NAME: &'static str = "export.pnp";
    const SUMMARY: &'static str = "Write the pick-and-place CSV (DNP excluded)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = Exported;

    fn run(self, ctx: &mut Context<'_>) -> Result<Exported, CommandError> {
        check_placed(ctx)?;
        let f = fabout::pnp::pick_place(ctx.project()?);
        let path = resolve(ctx, self.path.as_deref(), Path::new(DEFAULT_DIR).join(&f.name));
        write(vec![(path, f)])
    }

    fn summarize(o: &Exported) -> String {
        summary(o)
    }
}

/// Write the IPC-D-356A bare-board test netlist.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ipc356 {
    /// Output file (default `out/fab/<project>.d356`, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl Command for Ipc356 {
    const NAME: &'static str = "export.ipc356";
    const SUMMARY: &'static str = "Write the IPC-D-356A bare-board electrical test netlist";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = Exported;

    fn run(self, ctx: &mut Context<'_>) -> Result<Exported, CommandError> {
        let f = fabout::ipc356::netlist(ctx.project()?, &Options::default());
        let path = resolve(ctx, self.path.as_deref(), Path::new(DEFAULT_DIR).join(&f.name));
        write(vec![(path, f)])
    }

    fn summarize(o: &Exported) -> String {
        summary(o)
    }
}

/// Write the IPC-2581 revision C XML: BOM, stackup, layers, outline, packages, components,
/// nets and every copper, mask, paste, legend and drill feature in one file.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ipc2581 {
    /// Output file (default `out/fab/<project>-ipc2581.xml`, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Solder mask opening growth per side for this export, instead of the board's `mask_expansion` (pads and footprints with their own margin keep it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_expansion: Option<Nm>,
}

impl Command for Ipc2581 {
    const NAME: &'static str = "export.ipc2581";
    const SUMMARY: &'static str = "Write the IPC-2581 revision C XML (fab and assembly data in one file)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = Exported;

    fn run(self, ctx: &mut Context<'_>) -> Result<Exported, CommandError> {
        check_board(ctx)?;
        check_placed(ctx)?;
        let f = fabout::ipc2581::document(ctx.project()?, &options(self.mask_expansion));
        let path = resolve(ctx, self.path.as_deref(), Path::new(DEFAULT_DIR).join(&f.name));
        write(vec![(path, f)])
    }

    fn summarize(o: &Exported) -> String {
        summary(o)
    }
}

/// Write every generic manufacturing output: Gerber X2/X3, XNC drill, pick-and-place, IPC-D-356A,
/// IPC-2581.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct All {
    /// Output directory (default out/fab, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Solder mask opening growth per side for this export, instead of the board's `mask_expansion` (pads and footprints with their own margin keep it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_expansion: Option<Nm>,
}

impl Command for All {
    const NAME: &'static str = "export.all";
    const SUMMARY: &'static str =
        "Write all generic fab outputs (Gerber X2/X3, drill, pick-and-place, IPC-D-356A, IPC-2581)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["dir"];
    type Output = Exported;

    fn run(self, ctx: &mut Context<'_>) -> Result<Exported, CommandError> {
        check_board(ctx)?;
        check_placed(ctx)?;
        check_drills(ctx.project()?)?;
        let o = options(self.mask_expansion);
        let p = ctx.project()?;
        let mut files = fabout::all(p, &o);
        files.push(fabout::ipc2581::document(p, &o));
        write_dir(ctx, self.dir.as_deref(), files)
    }

    fn summarize(o: &Exported) -> String {
        summary(o)
    }
}

/// Default directory of mechanical CAD outputs, relative to the project.
fn mcad_dir() -> PathBuf {
    Path::new("out").join("mcad")
}

fn mcad_options(vias: bool, components: Option<bool>) -> mcad::Options {
    mcad::Options { vias, components: components.unwrap_or(true), ..mcad::Options::default() }
}

fn no_outline() -> CommandError {
    CommandError::conflict("board.no_outline", "the board has no outline")
        .with_hint("set one with `board.outline` before exporting a 3D or MCAD model")
}

fn report_no_body(ctx: &mut Context<'_>, refdes: Vec<String>) {
    for r in refdes {
        ctx.report(
            Diagnostic::warning(
                "export.no_body",
                format!("`{r}` has neither a 3D model nor package body dimensions and is left out"),
            )
            .with_subject(ObjectRef::Name(r))
            .with_hint(
                "attach a 3D model (`footprint.model_set`), or use a footprint generated from a package (`footprint.generate`), which records the body size and height",
            ),
        );
    }
}

fn report_model_errors(ctx: &mut Context<'_>, errors: &[(String, crate::models3d::ModelError3d)]) {
    for (refdes, e) in errors {
        ctx.report(e.warning(refdes));
    }
}

/// Appends `.ext` unless the path already has that extension.
fn with_ext(path: PathBuf, ext: &str) -> PathBuf {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
        return path;
    }
    let mut s = path.into_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// Write the board as a STEP (AP214) assembly for MCAD: the board solid (outline extruded to
/// the stackup thickness, minus cutouts and drilled holes) and component bodies: attached 3D
/// models as faceted B-rep, other parts as exact solids generated from the package (body with
/// chamfered top edges, leads, terminations, cylinders for MELF bodies and pins), as in the 3D
/// view.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Output file (default `out/mcad/<project>.step`, relative to the project; `.step` is
    /// appended if missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Also drill the vias (default: component and mounting holes only).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vias: bool,
    /// Include component bodies (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<bool>,
}

/// Result of `export.step`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StepExported {
    /// File written.
    pub path: String,
    /// Component bodies written.
    pub bodies: usize,
    /// Of those, bodies from attached 3D models (faceted).
    #[serde(default)]
    pub model_bodies: usize,
    /// Solids written for generated package bodies (body, leads, marks; once per footprint).
    #[serde(default)]
    pub generated_solids: usize,
    /// Holes cut in the board.
    pub holes: usize,
    /// Holes left out (crossing the edge, a cutout or another hole).
    pub skipped_holes: usize,
}

impl Command for Step {
    const NAME: &'static str = "export.step";
    const SUMMARY: &'static str = "Write the board and component bodies as a STEP AP214 assembly";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = StepExported;

    fn run(self, ctx: &mut Context<'_>) -> Result<StepExported, CommandError> {
        check_placed(ctx)?;
        let p = ctx.project()?;
        let stem = fabout::file_stem(&p.manifest().name);
        let out = mcad::step::export(p, &mcad_options(self.vias, self.components)).ok_or_else(no_outline)?;
        let path = resolve(ctx, self.path.as_deref(), mcad_dir().join(format!("{stem}.step")));
        let path = with_ext(path, "step");
        let file = OutFile { name: String::new(), function: "STEP".into(), content: out.content };
        let written = write(vec![(path, file)])?;
        for h in &out.skipped_holes {
            ctx.report(
                Diagnostic::warning(
                    "export.step_hole_skipped",
                    format!(
                        "a {} hole crosses the board edge, a cutout or another hole and is not cut in the 3D model",
                        h.diameter
                    ),
                )
                .at(h.at)
                .with_hint("move the hole inside the outline, clear of cutouts and other holes (`drc.run` reports it)"),
            );
        }
        report_no_body(ctx, out.no_body);
        report_model_errors(ctx, &out.model_errors);
        for m in &out.open_models {
            ctx.report(
                Diagnostic::info(
                    "export.step_open_model",
                    format!("model `{m}` is not a closed, consistently oriented mesh: written as surfaces, not a solid"),
                )
                .with_hint("MCAD tools show it but cannot compute its volume; repair the mesh (close holes, fix normals) for a solid"),
            );
        }
        Ok(StepExported {
            path: written.files[0].path.clone(),
            bodies: out.bodies,
            model_bodies: out.model_bodies,
            generated_solids: out.generated_solids,
            holes: out.holes,
            skipped_holes: out.skipped_holes.len(),
        })
    }

    fn summarize(o: &StepExported) -> String {
        let mut s = format!("wrote {} ({} bodies, {} holes)", o.path, o.bodies, o.holes);
        if o.skipped_holes > 0 {
            s.push_str(&format!(", {} hole(s) not cut", o.skipped_holes));
        }
        s
    }
}

/// Write IDF 3.0 board (`.emn`) and library (`.emp`) files for MCAD: outline, cutouts,
/// routing and placement outlines (the board inside the copper-to-edge clearance), routing,
/// via and placement keep-outs, drilled holes and component placements with body outlines and
/// heights.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Idf {
    /// Output directory (default out/mcad, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Also list the vias as drilled holes (default: component and mounting holes only).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vias: bool,
    /// Include component placements (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<bool>,
}

/// Result of `export.idf`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct IdfExported {
    /// Files written (board, library).
    pub files: Vec<WrittenFile>,
    /// Components placed.
    pub components: usize,
    /// Drilled holes listed.
    pub holes: usize,
    /// Keep-out sections written (routing, via and placement keep-outs).
    #[serde(default)]
    pub keepouts: usize,
}

impl Command for Idf {
    const NAME: &'static str = "export.idf";
    const SUMMARY: &'static str = "Write IDF 3.0 board and library files (.emn, .emp) for MCAD";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["dir"];
    type Output = IdfExported;

    fn run(self, ctx: &mut Context<'_>) -> Result<IdfExported, CommandError> {
        check_placed(ctx)?;
        let p = ctx.project()?;
        let stem = fabout::file_stem(&p.manifest().name);
        let out = mcad::idf::export(p, &mcad_options(self.vias, self.components)).ok_or_else(no_outline)?;
        let files = vec![
            OutFile { name: format!("{stem}.emn"), function: "IdfBoard".into(), content: out.board },
            OutFile { name: format!("{stem}.emp"), function: "IdfLibrary".into(), content: out.library },
        ];
        let dir = resolve(ctx, self.dir.as_deref(), mcad_dir());
        let written = write(files.into_iter().map(|f| (dir.join(&f.name), f)).collect())?;
        report_no_body(ctx, out.no_body);
        report_model_errors(ctx, &out.model_errors);
        Ok(IdfExported { files: written.files, components: out.placed, holes: out.holes, keepouts: out.keepouts })
    }

    fn summarize(o: &IdfExported) -> String {
        let mut s = format!(
            "wrote {} file(s) ({} components, {} holes, {} keep-outs)",
            o.files.len(),
            o.components,
            o.holes,
            o.keepouts
        );
        for f in &o.files {
            s.push_str(&format!("\n  {} ({})", f.path, f.function));
        }
        s
    }
}

/// Write an IDX (ProSTEP iViP EDMD) baseline for ECAD-MCAD collaboration: board outline and
/// cutouts, drilled holes, keep-outs and component placements with body outlines and heights.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Idx {
    /// Output file (default `out/mcad/<project>.idx`, relative to the project; `.idx` is
    /// appended if missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Also write the vias (default: component and mounting holes only).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vias: bool,
    /// Include component placements (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<bool>,
}

/// Result of `export.idx`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct IdxExported {
    /// File written.
    pub path: String,
    /// Components placed.
    pub components: usize,
    /// Drilled holes written.
    pub holes: usize,
    /// Keep-out items written (one per forbidden kind, and per side for footprint keep-outs).
    pub keepouts: usize,
}

impl Command for Idx {
    const NAME: &'static str = "export.idx";
    const SUMMARY: &'static str = "Write an IDX (ProSTEP EDMD) baseline for ECAD-MCAD collaboration";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = IdxExported;

    fn run(self, ctx: &mut Context<'_>) -> Result<IdxExported, CommandError> {
        check_placed(ctx)?;
        let p = ctx.project()?;
        let stem = fabout::file_stem(&p.manifest().name);
        let out = mcad::idx::export(p, &mcad_options(self.vias, self.components)).ok_or_else(no_outline)?;
        let path = resolve(ctx, self.path.as_deref(), mcad_dir().join(format!("{stem}.idx")));
        let path = with_ext(path, "idx");
        let file = OutFile { name: String::new(), function: "IDX".into(), content: out.content };
        let written = write(vec![(path, file)])?;
        report_no_body(ctx, out.no_body);
        Ok(IdxExported {
            path: written.files[0].path.clone(),
            components: out.components,
            holes: out.holes,
            keepouts: out.keepouts,
        })
    }

    fn summarize(o: &IdxExported) -> String {
        format!("wrote {} ({} components, {} holes, {} keep-outs)", o.path, o.components, o.holes, o.keepouts)
    }
}

/// Reads an IDX file, mapping read and parse failures to errors with hints.
fn read_idx(path: &Path) -> Result<String, CommandError> {
    std::fs::read_to_string(path).map_err(|e| io(path, e))
}

fn idx_error(path: &Path, e: String) -> CommandError {
    CommandError::invalid_args("idx.invalid", format!("{}: {e}", path.display()))
        .with_hint("pass an IDX (ProSTEP EDMD) file: a baseline (`export.idx`), change or response message")
}

/// Write an IDX change message (`SendChanges`) with the component placements that changed
/// since earlier messages: moved, rotated or flipped components, components placed with
/// another package, added and removed ones, one transaction each so the MCAD side can accept
/// or reject them separately. Board, holes and keep-outs are not compared.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdxChanges {
    /// The messages exchanged so far, oldest first: the baseline (`export.idx`), then change
    /// and response messages (relative to the project). Rejected changes in a response are
    /// undone.
    pub since: Vec<PathBuf>,
    /// Output file (default `out/mcad/<project>-changes.idx`, relative to the project; `.idx`
    /// is appended if missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Name of the person sending the changes (the message's actor; default empty: projects
    /// store no people).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

/// One component change of `export.idx_changes`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct IdxComponentChange {
    /// Designator.
    pub refdes: String,
    /// `moved`, `changed` (another package), `added` or `removed`.
    pub change: String,
}

/// Result of `export.idx_changes`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct IdxChangesExported {
    /// File written.
    pub path: String,
    /// The changes, one transaction each.
    pub changes: Vec<IdxComponentChange>,
}

impl Command for IdxChanges {
    const NAME: &'static str = "export.idx_changes";
    const SUMMARY: &'static str = "Write an IDX change message with the placements changed since earlier IDX messages";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["since"];
    type Output = IdxChangesExported;

    fn run(self, ctx: &mut Context<'_>) -> Result<IdxChangesExported, CommandError> {
        if self.since.is_empty() {
            return Err(CommandError::invalid_args("idx.no_baseline", "no earlier IDX message given").with_hint(
                "pass the baseline written by `export.idx` (and later change/response messages) as `since`",
            ));
        }
        check_placed(ctx)?;
        let mut history = mcad::idx::History::default();
        for f in &self.since {
            let path = resolve(ctx, Some(f), PathBuf::new());
            history.read(&read_idx(&path)?).map_err(|e| idx_error(&path, e))?;
        }
        let p = ctx.project()?;
        let stem = fabout::file_stem(&p.manifest().name);
        let actor = self.actor.as_deref().unwrap_or("");
        let out = mcad::idx::changes(p, &mcad_options(false, None), &history, actor).ok_or_else(no_outline)?;
        let Some(content) = out.content else {
            return Err(CommandError::conflict(
                "idx.no_changes",
                "no component placement changed since those messages",
            )
            .with_hint("move, add or remove components first, or check the `since` files"));
        };
        let path = resolve(ctx, self.path.as_deref(), mcad_dir().join(format!("{stem}-changes.idx")));
        let path = with_ext(path, "idx");
        let written = write(vec![(path, OutFile { name: String::new(), function: "IDX".into(), content })])?;
        report_no_body(ctx, out.no_body);
        let changes = out
            .changes
            .iter()
            .map(|c| {
                let change = match c {
                    mcad::idx::ComponentChange::Moved(_) => "moved",
                    mcad::idx::ComponentChange::Changed(_) => "changed",
                    mcad::idx::ComponentChange::Added(_) => "added",
                    mcad::idx::ComponentChange::Removed(_) => "removed",
                };
                IdxComponentChange { refdes: c.refdes().to_string(), change: change.into() }
            })
            .collect();
        Ok(IdxChangesExported { path: written.files[0].path.clone(), changes })
    }

    fn summarize(o: &IdxChangesExported) -> String {
        let list: Vec<String> = o.changes.iter().map(|c| format!("{} {}", c.refdes, c.change)).collect();
        format!("wrote {} ({} change(s): {})", o.path, o.changes.len(), list.join(", "))
    }
}

/// Answer an IDX change message from MCAD: apply the component moves it proposes (position,
/// rotation, side of placed, unlocked components) and write the response message accepting
/// them. Transactions with changes cadlab cannot apply (other items, added or deleted
/// components, locked ones) or listed in `reject` are rejected.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceImportIdx {
    /// The change message (relative to the project).
    pub path: PathBuf,
    /// Reject the transactions changing these designators or item numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reject: Vec<String>,
    /// Comment written in every answered transaction (default: the reason of a rejection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Response file (default `out/mcad/<message name>-response.idx`, relative to the
    /// project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<PathBuf>,
}

/// One answered transaction of `place.import_idx`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct IdxAnswer {
    /// The transaction's description.
    pub description: String,
    /// Designators (or item numbers) it changes.
    pub items: Vec<String>,
    /// Accepted (and applied) or rejected.
    pub accepted: bool,
    /// Why it was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Result of `place.import_idx`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct IdxImported {
    /// Response file written.
    pub response: String,
    /// Transactions, in message order.
    pub transactions: Vec<IdxAnswer>,
    /// Designators moved.
    pub moved: Vec<String>,
}

impl Command for PlaceImportIdx {
    const NAME: &'static str = "place.import_idx";
    const SUMMARY: &'static str = "Apply the component moves of an IDX change message and write the response";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = IdxImported;

    fn run(self, ctx: &mut Context<'_>) -> Result<IdxImported, CommandError> {
        let path = resolve(ctx, Some(&self.path), PathBuf::new());
        let text = read_idx(&path)?;
        let transactions = mcad::idx::read_changes(&text).map_err(|e| idx_error(&path, e))?;
        let mut answers = Vec::new();
        let mut out = Vec::new();
        let mut moves = Vec::new();
        for t in &transactions {
            let p = ctx.project()?;
            let mut reason = None;
            let mut items = Vec::new();
            let mut planned = Vec::new();
            for c in &t.changes {
                let refdes = c.placement.as_ref().map(|pl| pl.refdes.clone());
                items.push(refdes.clone().unwrap_or_else(|| c.number.clone()));
                let why = match (&c.placement, c.deleted.is_empty()) {
                    (_, false) => Some(format!("{}: deleting components is not applied by cadlab", c.number)),
                    (None, _) => Some(format!("{}: only component placements are applied by cadlab", c.number)),
                    (Some(pl), _) => match p.board().footprints.get(&pl.refdes) {
                        None => Some(format!("{} is not placed on the board", pl.refdes)),
                        Some(f) if f.locked => Some(format!("{} is locked", pl.refdes)),
                        Some(_) => {
                            planned.push(pl.clone());
                            None
                        }
                    },
                };
                if reason.is_none() {
                    reason = why;
                }
            }
            if reason.is_none() && t.changes.is_empty() {
                reason = Some("the transaction has no change".into());
            }
            if reason.is_none()
                && let Some(r) = items.iter().find(|i| self.reject.iter().any(|x| x == *i))
            {
                reason = Some(format!("{r}: rejected"));
            }
            if reason.is_none()
                && let Some(c) = t.changes.iter().find(|c| self.reject.contains(&c.number))
            {
                reason = Some(format!("{}: rejected", c.number));
            }
            let accepted = reason.is_none();
            if accepted {
                moves.extend(planned);
            }
            answers
                .push(mcad::idx::Answer { accept: accepted, comment: self.comment.clone().or_else(|| reason.clone()) });
            out.push(IdxAnswer { description: t.description.clone(), items, accepted, reason });
        }
        let mut moved = Vec::new();
        let p = ctx.project_mut()?;
        for pl in moves {
            let (at, rotation, side) = pl.board_placement();
            if let Some(f) = p.board_mut().footprints.get_mut(&pl.refdes) {
                f.at = at;
                f.rotation = rotation;
                f.side = side;
                moved.push(pl.refdes);
            }
        }
        let response = mcad::idx::respond(&text, &answers).map_err(|e| idx_error(&path, e))?;
        let stem = path.file_stem().map_or("changes".into(), |s| s.to_string_lossy().to_string());
        let rpath = resolve(ctx, self.response.as_deref(), mcad_dir().join(format!("{stem}-response.idx")));
        let rpath = with_ext(rpath, "idx");
        let written = if ctx.is_dry_run() {
            rpath.display().to_string()
        } else {
            let file = OutFile { name: String::new(), function: "IDX".into(), content: response };
            write(vec![(rpath, file)])?.files[0].path.clone()
        };
        Ok(IdxImported { response: written, transactions: out, moved })
    }

    fn summarize(o: &IdxImported) -> String {
        let accepted = o.transactions.iter().filter(|t| t.accepted).count();
        let mut s = format!(
            "{accepted} of {} change(s) accepted, moved: {}; wrote {}",
            o.transactions.len(),
            if o.moved.is_empty() { "none".to_string() } else { o.moved.join(", ") },
            o.response
        );
        for t in o.transactions.iter().filter(|t| !t.accepted) {
            s.push_str(&format!("\n  rejected {}: {}", t.items.join(", "), t.reason.as_deref().unwrap_or("")));
        }
        s
    }
}

/// Write the board as a Specctra DSN design for an external autorouter (freerouting and
/// others); route it there, then apply the session with `route.import_ses`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportDsn {
    /// Output file (default `out/route/<project>.dsn`, relative to the project; `.dsn` is
    /// appended if missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Mark all existing tracks and vias as protected, so the router keeps them (default: only
    /// locked ones are protected).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub protect_existing: bool,
    /// Router resolution in steps per micrometer (default 10: 0.1 µm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<u32>,
}

/// Result of `export.dsn`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DsnExported {
    /// File written.
    pub path: String,
    /// Components (footprints and mounting holes) written.
    pub components: usize,
    /// Nets written.
    pub nets: usize,
    /// Net classes written.
    pub classes: usize,
    /// Padstacks written.
    pub padstacks: usize,
    /// Existing wires written.
    pub wires: usize,
    /// Existing vias written.
    pub vias: usize,
    /// Wires and vias marked protected.
    pub protected: usize,
}

impl Command for ExportDsn {
    const NAME: &'static str = "export.dsn";
    const SUMMARY: &'static str = "Write the board as a Specctra DSN design for an external autorouter";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = DsnExported;

    fn run(self, ctx: &mut Context<'_>) -> Result<DsnExported, CommandError> {
        if self.resolution == Some(0) {
            return Err(CommandError::invalid_args("export.dsn_resolution", "the resolution must be at least 1")
                .with_hint("use the default (10 steps per micrometer), or 1000 for 1 nm"));
        }
        let p = ctx.project()?;
        if p.board().outline.contours.is_empty() {
            return Err(CommandError::conflict("board.no_outline", "the board has no outline")
                .with_hint("set one with `board.outline` before exporting for routing"));
        }
        let name = p.manifest().name.clone();
        let opts = crate::specctra::export::Options {
            resolution: self.resolution.unwrap_or(10),
            protect_all: self.protect_existing,
        };
        let out = crate::specctra::export::export(p, &name, &opts);
        let mut path = resolve(ctx, self.path.as_deref(), Path::new("out").join("route").join(format!("{name}.dsn")));
        if path.extension().is_none_or(|e| e != "dsn") {
            let mut s = path.into_os_string();
            s.push(".dsn");
            path = PathBuf::from(s);
        }
        let d = &out.dsn;
        let result = DsnExported {
            path: path.display().to_string(),
            components: d.places.len(),
            nets: d.nets.len(),
            classes: d.classes.len(),
            padstacks: d.padstacks.len(),
            wires: d.wires.len(),
            vias: d.wire_vias.len(),
            protected: d.wires.iter().filter(|w| w.protect).count() + d.wire_vias.iter().filter(|v| v.protect).count(),
        };
        let file = OutFile { name: String::new(), function: "SpecctraDesign".into(), content: d.write() };
        write(vec![(path, file)])?;
        for w in out.warnings {
            ctx.report(
                Diagnostic::warning("export.dsn_incomplete", w)
                    .with_hint("the router does not see these items; check the imported routing with `drc.run`"),
            );
        }
        Ok(result)
    }

    fn summarize(o: &DsnExported) -> String {
        format!(
            "wrote {} ({} components, {} nets, {} classes, {} padstacks, {} wires, {} vias, {} protected)",
            o.path, o.components, o.nets, o.classes, o.padstacks, o.wires, o.vias, o.protected
        )
    }
}

/// Write a SPICE netlist (ngspice dialect) of the circuit: R, C, L, diodes and transistors from
/// part data, ICs as subcircuit calls (`spice_model`, `spice_lib`, `spice_pins` part parameters
/// or component properties), placeholders commented out with a warning when a part has no
/// model. Optional DC sources for supply nets and analysis/control lines for simulation.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Spice {
    /// Output file (default `out/spice/<project>.cir`; relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Ground net, written as node 0 (default `GND`, else the first ground-like net name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground: Option<String>,
    /// Add DC voltage sources: nets with a voltage (`net.set --voltage`), and driven nets whose
    /// name gives one (`3V3`, `+5V`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub supplies: bool,
    /// Analysis lines written as-is (".op", ".tran 1u 10m").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub analysis: Vec<String>,
    /// ngspice `.control` block lines ("op", "print v(out)").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub control: Vec<String>,
    /// Also write DNP components.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_dnp: bool,
    /// Only these components: designators, or block instances ("CH1") standing for theirs, to
    /// simulate a subcircuit on its own (default: the whole circuit).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}

/// Result of `export.spice`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SpiceExported {
    /// File written.
    pub path: String,
    /// Elements written (components and sources).
    pub elements: usize,
    /// Components written as commented-out placeholders (no model).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub placeholders: Vec<String>,
    /// Components not simulated (connectors, switches, test points, DNP).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<String>,
    /// Supply sources: net to voltage.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sources: std::collections::BTreeMap<String, crate::value::Quantity>,
    /// Net to SPICE node name (ground is `0`).
    pub nodes: std::collections::BTreeMap<String, String>,
}

impl Command for Spice {
    const NAME: &'static str = "export.spice";
    const SUMMARY: &'static str = "Write a SPICE netlist (ngspice): passives, diodes, models, supply sources";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = SpiceExported;

    fn run(self, ctx: &mut Context<'_>) -> Result<SpiceExported, CommandError> {
        let p = ctx.project()?;
        let only = if self.components.is_empty() {
            None
        } else {
            let mut set = std::collections::BTreeSet::new();
            for r in self.components.iter().flat_map(|r| r.split(',')).map(str::trim).filter(|r| !r.is_empty()) {
                if p.circuit().instances.contains_key(r) && !p.circuit().components.contains_key(r) {
                    set.extend(
                        p.circuit()
                            .components
                            .iter()
                            .filter(|(_, c)| c.block.as_deref() == Some(r))
                            .map(|(k, _)| k.clone()),
                    );
                } else {
                    set.insert(super::util::refdes_key(p, r)?);
                }
            }
            Some(set)
        };
        let opts = crate::spice::Options {
            ground: self.ground.clone(),
            supplies: self.supplies,
            analysis: self.analysis.clone(),
            control: self.control.clone(),
            include_dnp: self.include_dnp,
            only,
        };
        let n =
            crate::spice::export(p, &opts).map_err(|e| CommandError::not_found(e.code, e.message).with_hint(e.hint))?;
        let default =
            PathBuf::from("out/spice").join(format!("{}.cir", crate::model::part::slugify(&p.manifest().name)));
        let path = resolve(ctx, self.path.as_deref(), default);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        }
        std::fs::write(&path, n.text.as_bytes()).map_err(|e| io(&path, e))?;
        for d in n.diagnostics {
            ctx.report(d);
        }
        Ok(SpiceExported {
            path: path.display().to_string(),
            elements: n.elements,
            placeholders: n.placeholders,
            omitted: n.omitted,
            sources: n.sources.into_iter().collect(),
            nodes: n.nodes,
        })
    }

    fn summarize(o: &SpiceExported) -> String {
        let mut s = format!("wrote {} ({} elements)", o.path, o.elements);
        if !o.placeholders.is_empty() {
            s += &format!("\n  no model (commented out): {}", o.placeholders.join(", "));
        }
        if !o.sources.is_empty() {
            let v: Vec<String> = o.sources.iter().map(|(n, v)| format!("{n}={v}")).collect();
            s += &format!("\n  supplies: {}", v.join(", "));
        }
        s
    }
}

/// Export a region of the board to openEMS, a 3D full-wave (FDTD) solver: CSXCAD geometry
/// (stackup dielectrics, copper, vias and holes) with a graded mesh, and a Python driver that
/// adds the ports, the excitation and the boundaries, simulates each port and writes the
/// S-parameters as a Touchstone file (summarize it with `touchstone.summary`).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenEms {
    /// Output directory (default out/openems, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Base name of the files (default: the project's name): `<name>.xml`, `<name>.py`, and
    /// `<name>.sNp` written by the driver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Highest frequency of interest ("10GHz"): the excitation's bandwidth and the mesh
    /// resolution (λ/N in the dielectric).
    pub f_max: Quantity,
    /// Lowest frequency of interest (default 0: the pulse starts at DC, the Touchstone file at
    /// 1 % of `f_max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f_min: Option<Quantity>,
    /// Region: the copper of these nets (pads, tracks, vias; not pours) plus `margin`, e.g. a
    /// net's path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nets: Vec<String>,
    /// Region: the extent of these items plus `margin`: footprints (`J1`, a connector), pads
    /// (`J1.1`), vias (`via#3`, a via transition), tracks (`track#5`), nets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub around: Vec<String>,
    /// Region: two corners on the board, used as given (combined with `nets` / `around`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<[Point; 2]>,
    /// Margin around `nets` and `around` (default: three board stack heights, at least 1 mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin: Option<Nm>,
    /// Keep only the copper of `nets`, the ground nets and the ports' nets (default: all
    /// copper in the region).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub only_selected: bool,
    /// Ground nets kept by `only_selected` (default: nets named like ground, `GND`, `AGND`...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ground: Vec<String>,
    /// Ports, numbered from 1: `{"at": "J1.1"}` (lumped, at a pad) or
    /// `{"at": "track#4", "end": "end", "kind": "msl"}`; `z0` (default 50 Ω), `reference`
    /// layer (default: the nearest with copper under the port), `width`, `length`, `layer`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<openems::PortSpec>,
    /// Frequency points in the Touchstone file (default 201).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<u32>,
    /// Boundary condition on all sides: `pml` (default, 8 cells) or `mur`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<openems::Boundary>,
    /// PML thickness in cells (default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pml_cells: Option<u8>,
    /// Air above and below the board (default: a quarter wavelength at `f_max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air: Option<Nm>,
    /// Copper model: `solid` (default: extruded through its thickness) or `sheet`
    /// (zero-thickness; the stack is the dielectrics only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copper: Option<openems::CopperModel>,
    /// Mesh cells per wavelength in the material (default 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells_per_wavelength: Option<u32>,
    /// Largest mesh cell, instead of the wavelength rule ("0.2mm").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cell: Option<Nm>,
    /// Metal edges closer than this share a mesh line (default: a twentieth of the largest
    /// cell).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_cell: Option<Nm>,
    /// Largest size ratio between neighbouring cells (default 1.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grading: Option<Quantity>,
    /// End criterion: stop when the energy has decayed by this much ("40dB", the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_criteria: Option<Quantity>,
    /// Maximum number of timesteps per run (default 200000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timesteps: Option<u64>,
    /// Thirds rule: mesh lines a third of a fine cell inside and two thirds outside each
    /// axis-aligned copper edge instead of one line on it (more accurate line impedances; the
    /// default of `sim.sparams`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub thirds: bool,
    /// Solder mask thickness over the laminate ("20um"): a dielectric layer on both outer
    /// layers, open at pads with a mask opening (none by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Nm>,
    /// Solder mask thickness over the copper (default: `mask`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_copper: Option<Nm>,
    /// Solder mask relative permittivity (default 3.8, typical LPI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_er: Option<Quantity>,
    /// Solder mask loss tangent (default 0.025, typical LPI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_loss_tangent: Option<Quantity>,
    /// Copper conductivity for the skin-effect loss (default 58 MS/m, annealed copper: "58M"):
    /// openEMS conducting sheets on the copper surfaces, added by the driver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conductivity: Option<Quantity>,
    /// Lossless copper: perfect conductors only (the driver adds no conducting sheets).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub perfect_copper: bool,
    /// Copper RMS surface roughness (the Hammerstad–Jensen factor at the pulse's center, folded
    /// into the sheets' conductivity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<Nm>,
    /// Huray snowball radius (with `huray_ratio`: Huray's roughness model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huray_radius: Option<Nm>,
    /// Huray surface ratio: snowball surface per flat area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huray_ratio: Option<Quantity>,
}

/// Gaussian excitation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Excitation {
    /// Center frequency.
    pub f0: Quantity,
    /// 20 dB half-bandwidth (the pulse covers f0 − fc .. f0 + fc).
    pub fc: Quantity,
}

/// Result of `export.openems`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct OpenEmsExported {
    /// Files written.
    pub files: Vec<WrittenFile>,
    /// Touchstone file the driver writes.
    pub touchstone: String,
    /// How to run it.
    pub run: String,
    /// Board region simulated (two corners).
    pub window: [Point; 2],
    /// Domain bottom and top in z, with the air (z 0 = bottom of the bottom copper).
    pub z_range: [Nm; 2],
    /// Ports.
    pub ports: Vec<openems::PortInfo>,
    /// Mesh summary.
    pub mesh: openems::MeshInfo,
    /// Excitation.
    pub excitation: Excitation,
    /// Nets with copper in the region.
    pub nets: Vec<String>,
}

/// The solder mask options (`mask`, `mask_copper`, `mask_er`, `mask_loss_tangent`) shared by
/// `export.openems` and `sim.sparams`, with notes for assumed values.
pub(crate) fn mask_options(
    mask: Option<Nm>,
    mask_copper: Option<Nm>,
    mask_er: Option<Quantity>,
    mask_loss_tangent: Option<Quantity>,
) -> Result<(Option<openems::MaskOptions>, Vec<Diagnostic>), CommandError> {
    let mut notes = Vec::new();
    let Some(t) = mask else {
        if mask_copper.is_some() || mask_er.is_some() || mask_loss_tangent.is_some() {
            return Err(CommandError::invalid_args(
                "openems.bad_mask",
                "`mask_copper`, `mask_er` and `mask_loss_tangent` need `mask` (the mask thickness)",
            ));
        }
        return Ok((None, notes));
    };
    if t <= Nm::ZERO || mask_copper.is_some_and(|c| c <= Nm::ZERO) {
        return Err(CommandError::invalid_args("openems.bad_mask", "solder mask thicknesses must be positive"));
    }
    let er = match mask_er {
        Some(e) => {
            let e = crate::commands::electrical::expect_unit(e, Unit::None, "mask_er")?.to_f64();
            if e < 1.0 {
                return Err(CommandError::invalid_args("openems.bad_mask", "`mask_er` must be at least 1"));
            }
            e
        }
        None => {
            notes.push(
                Diagnostic::info(
                    "openems.mask_er_assumed",
                    format!("solder mask εr not given: assuming {}", crate::electrical::DEFAULT_MASK_ER),
                )
                .with_hint("give `mask_er` from the mask's datasheet (or your fab's stackup)"),
            );
            crate::electrical::DEFAULT_MASK_ER
        }
    };
    let loss_tangent = match mask_loss_tangent {
        Some(q) => {
            let v = crate::commands::electrical::expect_unit(q, Unit::None, "mask_loss_tangent")?.to_f64();
            if !(0.0..1.0).contains(&v) {
                return Err(CommandError::invalid_args(
                    "value.out_of_range",
                    "`mask_loss_tangent` must be at least 0 and below 1, e.g. 0.025",
                ));
            }
            v
        }
        None => {
            notes.push(
                Diagnostic::info(
                    "openems.mask_loss_tangent_assumed",
                    format!(
                        "solder mask loss tangent not given: assuming {}",
                        crate::electrical::DEFAULT_MASK_LOSS_TANGENT
                    ),
                )
                .with_hint("give `mask_loss_tangent` from the mask's datasheet"),
            );
            crate::electrical::DEFAULT_MASK_LOSS_TANGENT
        }
    };
    Ok((Some(openems::MaskOptions { thickness: t, over_copper: mask_copper, er, loss_tangent }), notes))
}

/// The copper options (`conductivity`, `perfect_copper`, `roughness`, `huray_radius`,
/// `huray_ratio`) shared by `export.openems` and `sim.sparams`: the conductivity (S/m; `None`
/// for perfect conductors, default 58 MS/m) and the roughness.
pub(crate) fn copper_options(
    conductivity: Option<Quantity>,
    perfect_copper: bool,
    roughness: Option<Nm>,
    huray_radius: Option<Nm>,
    huray_ratio: Option<Quantity>,
    code: &'static str,
) -> Result<(Option<f64>, crate::electrical::loss::Roughness), CommandError> {
    let (sigma, rough) =
        crate::commands::electrical::conductor_args(conductivity, roughness, huray_radius, huray_ratio)?;
    if perfect_copper && (conductivity.is_some() || roughness.is_some() || huray_radius.is_some()) {
        return Err(CommandError::invalid_args(
            code,
            "`perfect_copper` has no conductivity or roughness: drop one or the other",
        ));
    }
    Ok(((!perfect_copper).then(|| sigma.to_f64()), rough))
}

pub(crate) fn hertz(q: Quantity, field: &str) -> Result<u64, CommandError> {
    let q = crate::commands::electrical::expect_unit(q, Unit::Hertz, field)?;
    let v = q.to_f64();
    if !(0.0..1e15).contains(&v) {
        return Err(CommandError::invalid_args("value.out_of_range", format!("`{field}` must be between 0 and 1 PHz")));
    }
    Ok(v.round() as u64)
}

impl Command for OpenEms {
    const NAME: &'static str = "export.openems";
    const SUMMARY: &'static str =
        "Export a board region to openEMS (full-wave FDTD): geometry, mesh, ports and a driver writing Touchstone";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["dir"];
    type Output = OpenEmsExported;

    fn run(self, ctx: &mut Context<'_>) -> Result<OpenEmsExported, CommandError> {
        let (mask, mask_notes) = mask_options(self.mask, self.mask_copper, self.mask_er, self.mask_loss_tangent)?;
        let (conductivity, roughness) = copper_options(
            self.conductivity,
            self.perfect_copper,
            self.roughness,
            self.huray_radius,
            self.huray_ratio,
            "openems.perfect_copper",
        )?;
        let p = ctx.project()?;
        let name = self.name.clone().unwrap_or_else(|| crate::model::part::slugify(&p.manifest().name));
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err(CommandError::invalid_args("export.bad_name", "`name` must be a plain file name"));
        }
        let grading = match self.grading {
            Some(g) => {
                let g = crate::commands::electrical::expect_unit(g, Unit::None, "grading")?.to_f64();
                if !(g > 1.0 && g <= 3.0) {
                    return Err(CommandError::invalid_args(
                        "value.out_of_range",
                        "`grading` must be above 1 and at most 3 (1.5 is usual)",
                    ));
                }
                (g * 1000.0).round() as u32
            }
            None => 1500,
        };
        let end = match self.end_criteria {
            Some(e) if matches!(e.unit, Unit::Decibel | Unit::None) && e.to_f64() != 0.0 => {
                e.to_f64().abs().round() as u32
            }
            Some(_) => {
                return Err(CommandError::invalid_args(
                    "value.wrong_unit",
                    "`end_criteria` is an energy decay in dB, e.g. 40dB",
                ));
            }
            None => 40,
        };
        let opts = openems::Options {
            name: name.clone(),
            title: p.manifest().name.clone(),
            nets: self.nets.clone(),
            around: self.around.clone(),
            window: self.window.map(|[a, b]| (a, b)),
            margin: self.margin,
            only_selected: self.only_selected,
            ground: self.ground.clone(),
            ports: self.ports.clone(),
            f_min_hz: self.f_min.map(|f| hertz(f, "f_min")).transpose()?.unwrap_or(0),
            f_max_hz: hertz(self.f_max, "f_max")?,
            points: self.points.unwrap_or(201),
            boundary: self.boundary.unwrap_or_default(),
            pml_cells: self.pml_cells.unwrap_or(8).max(1),
            air: self.air,
            copper: self.copper.unwrap_or_default(),
            cells_per_wavelength: self.cells_per_wavelength.unwrap_or(20),
            max_cell: self.max_cell,
            min_cell: self.min_cell,
            grading_permille: grading,
            end_criteria_db: end,
            timesteps: self.timesteps.unwrap_or(200_000),
            thirds: self.thirds,
            port_planes: false,
            mask,
            conductivity,
            roughness,
        };
        let x = openems::export(p, &opts).map_err(|e| {
            let base = if e.code.starts_with("openems.unknown") {
                CommandError::not_found(e.code, e.message)
            } else {
                CommandError::invalid_args(e.code, e.message)
            };
            let mut c = base.with_hint(e.hint).with_suggestions(&e.suggestions);
            for s in e.subjects {
                c = c.with_subject(s);
            }
            c
        })?;
        let dir = resolve(ctx, self.dir.as_deref(), PathBuf::from("out/openems"));
        let file = |ext: &str, function: &str, content: String| {
            let name = format!("{name}.{ext}");
            (dir.join(&name), OutFile { name, function: function.into(), content })
        };
        let files = write(vec![
            file("xml", "openEMS geometry (CSXCAD XML) and mesh", x.xml),
            file("py", "openEMS driver (Python)", x.script),
        ])?;
        for d in mask_notes.into_iter().chain(x.diagnostics) {
            ctx.report(d);
        }
        let hz = |v: u64| Quantity::int(v as i64, Unit::Hertz);
        Ok(OpenEmsExported {
            run: format!("python3 {}", dir.join(format!("{name}.py")).display()),
            touchstone: dir.join(&x.touchstone).display().to_string(),
            files: files.files,
            window: [x.window.0, x.window.1],
            z_range: [x.z_range.0, x.z_range.1],
            ports: x.ports,
            mesh: x.mesh,
            excitation: Excitation { f0: hz(x.excitation.0), fc: hz(x.excitation.1) },
            nets: x.nets,
        })
    }

    fn summarize(o: &OpenEmsExported) -> String {
        let mut s = format!(
            "wrote {} file(s); mesh {}×{}×{} lines ({} cells, smallest {} / {} / {})",
            o.files.len(),
            o.mesh.lines[0],
            o.mesh.lines[1],
            o.mesh.lines[2],
            o.mesh.cells,
            o.mesh.min_cell[0],
            o.mesh.min_cell[1],
            o.mesh.min_cell[2]
        );
        for f in &o.files {
            s.push_str(&format!("\n  {} ({})", f.path, f.function));
        }
        for p in &o.ports {
            let kind = match p.kind {
                openems::PortKind::Lumped => "lumped",
                openems::PortKind::Msl => "microstrip",
            };
            s.push_str(&format!(
                "\n  port {}: {} ({kind}, {}) on {} over {}",
                p.number, p.at, p.z0, p.layer, p.reference
            ));
        }
        s.push_str(&format!("\nrun: {}  (writes {})", o.run, o.touchstone));
        s
    }
}
