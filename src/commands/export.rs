//! `export.*`: generic manufacturing outputs (Gerber X2/X3, XNC drill, pick-and-place,
//! IPC-D-356A) and the Specctra DSN design for external autorouters. Fab-specific bundles come with fab profiles (`export fab`, DECISIONS D12).

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::fabout::{self, Options, OutFile};
use crate::model::Project;
use crate::refs::ObjectRef;
use crate::units::Nm;

pub(crate) fn register(r: &mut Registry) {
    r.register::<Gerber>()
        .register::<Drill>()
        .register::<Pnp>()
        .register::<Ipc356>()
        .register::<All>()
        .register::<ExportDsn>()
        .register::<Spice>();
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

fn resolve(ctx: &Context<'_>, path: Option<&Path>, default: PathBuf) -> PathBuf {
    let p = path.map(Path::to_path_buf).unwrap_or(default);
    match ctx.session.root() {
        Some(root) if p.is_relative() => root.join(p),
        _ => p,
    }
}

fn io(path: &Path, e: std::io::Error) -> CommandError {
    crate::model::ModelError::Io { path: path.to_path_buf(), source: e }.into()
}

fn write(files: Vec<(PathBuf, OutFile)>) -> Result<Exported, CommandError> {
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
    Options { mask_expansion: mask_expansion.unwrap_or(Nm::ZERO), ..Options::default() }
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
    /// Solder mask opening growth per side (default 0: openings equal pads).
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

/// Write every generic manufacturing output: Gerber X2/X3, XNC drill, pick-and-place, IPC-D-356A.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct All {
    /// Output directory (default out/fab, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Solder mask opening growth per side (default 0: openings equal pads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_expansion: Option<Nm>,
}

impl Command for All {
    const NAME: &'static str = "export.all";
    const SUMMARY: &'static str = "Write all generic fab outputs (Gerber X2/X3, drill, pick-and-place, IPC-D-356A)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["dir"];
    type Output = Exported;

    fn run(self, ctx: &mut Context<'_>) -> Result<Exported, CommandError> {
        check_board(ctx)?;
        check_placed(ctx)?;
        check_drills(ctx.project()?)?;
        let files = fabout::all(ctx.project()?, &options(self.mask_expansion));
        write_dir(ctx, self.dir.as_deref(), files)
    }

    fn summarize(o: &Exported) -> String {
        summary(o)
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
        let opts = crate::spice::Options {
            ground: self.ground.clone(),
            supplies: self.supplies,
            analysis: self.analysis.clone(),
            control: self.control.clone(),
            include_dnp: self.include_dnp,
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
