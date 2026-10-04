//! `board.export_kicad` / `board.import_kicad`: KiCad board export and import (`.kicad_pcb` +
//! `.kicad_pro` + `.kicad_dru`), and `board.import_kicad_rules` for the rules alone.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::kicad_import::{
    self, BoardImportOptions, BoardImportReport, ImportError, ImportErrorKind, OriginMode, rules,
};

pub(crate) fn register(r: &mut Registry) {
    r.register::<ExportKicad>().register::<ImportKicad>().register::<ImportKicadRules>();
}

fn to_error(e: ImportError) -> CommandError {
    let mut err = match e.kind {
        ImportErrorKind::Invalid => CommandError::invalid_args(e.code, e.message),
        ImportErrorKind::Conflict => CommandError::conflict(e.code, e.message),
    }
    .with_hint(e.hint);
    for s in e.subjects {
        err = err.with_subject(s);
    }
    err
}

fn resolve(ctx: &Context<'_>, path: &Path) -> PathBuf {
    match ctx.session.root() {
        Some(root) if path.is_relative() => root.join(path),
        _ => path.to_path_buf(),
    }
}

fn read(path: &Path) -> Result<String, CommandError> {
    std::fs::read_to_string(path)
        .map_err(|e| CommandError::from(crate::model::ModelError::Io { path: path.to_path_buf(), source: e }))
}

/// Reads the `.kicad_pro` and `.kicad_dru` next to `path` (when present).
fn sibling_rules(path: &Path) -> Result<Option<(rules::KicadRules, Vec<crate::Diagnostic>)>, CommandError> {
    let (pro, dru) = (path.with_extension("kicad_pro"), path.with_extension("kicad_dru"));
    let pro = if pro.is_file() { Some(read(&pro)?) } else { None };
    let dru = if dru.is_file() { Some(read(&dru)?) } else { None };
    if pro.is_none() && dru.is_none() {
        return Ok(None);
    }
    rules::parse(pro.as_deref(), dru.as_deref()).map(Some).map_err(to_error)
}

/// Import a KiCad board (.kicad_pcb) into the project: board setup, outline, footprints (added
/// to the project library), tracks, vias, zones, keep-outs, mounting holes and drawings, plus the
/// rules and net classes of the `.kicad_pro` / `.kicad_dru` next to it. With a circuit in the
/// project, footprints are matched by designator and nets by pads; without one, the circuit is
/// built from the board. What cannot be imported is reported, item by item.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportKicad {
    /// Board file (relative paths are relative to the project directory).
    pub path: PathBuf,
    /// Replace the current board (placements, copper, outline, drawings). Without it the board
    /// must be empty. The circuit is kept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
    /// Where cadlab's origin goes: `auto` (KiCad's auxiliary axis origin when set, else the
    /// outline's lower-left corner), `outline`, `aux` or `page`.
    #[serde(default)]
    pub origin: OriginMode,
    /// Skip the `.kicad_pro` / `.kicad_dru` rules next to the board.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_rules: bool,
}

impl Command for ImportKicad {
    const NAME: &'static str = "board.import_kicad";
    const SUMMARY: &'static str =
        "Import a KiCad board (.kicad_pcb, with its .kicad_pro/.kicad_dru rules) into the project";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = BoardImportReport;

    fn run(self, ctx: &mut Context<'_>) -> Result<BoardImportReport, CommandError> {
        ctx.project()?;
        let path = resolve(ctx, &self.path);
        let text = read(&path)?;
        let mut opts = BoardImportOptions {
            replace: self.replace,
            origin: self.origin,
            file_name: path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
            rules: None,
        };
        if !self.no_rules
            && let Some((k, diags)) = sibling_rules(&path)?
        {
            opts.rules = Some(k);
            for d in diags {
                ctx.report(d);
            }
        }
        let (report, diags) = kicad_import::import(ctx.project_mut()?, &text, &opts).map_err(to_error)?;
        for d in diags {
            ctx.report(d);
        }
        Ok(report)
    }

    fn summarize(o: &BoardImportReport) -> String {
        let mut s = format!(
            "imported {}: {} copper layers, {} footprints, {} holes, {} tracks, {} vias, {} zones, {} keep-outs, {} drawings",
            o.source, o.copper_layers, o.footprints, o.holes, o.tracks, o.vias, o.zones, o.keepouts, o.graphics
        );
        if let Some(n) = &o.netlist {
            s += &format!("\ncircuit built from the board: {} components, {} nets", n.components, n.nets);
        }
        if !o.library_footprints.is_empty() {
            s += &format!("\nfootprints added to the library: {}", o.library_footprints.join(", "));
        }
        if let Some(r) = &o.rules {
            s += &format!(
                "\nrules: {} board rules, {} net classes, {} nets assigned",
                r.rules.len(),
                r.netclasses.len(),
                r.assigned
            );
        }
        if o.not_imported > 0 {
            s += &format!("\n{} items not imported (see diagnostics)", o.not_imported);
        }
        s
    }
}

/// Import design rules and net classes from a KiCad project (.kicad_pro) and its custom rules
/// (.kicad_dru): board minimums, default track and via sizes, net classes and the nets they
/// apply to. Give either file (or the .kicad_pcb); both are read when present.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportKicadRules {
    /// `.kicad_pro`, `.kicad_dru` or `.kicad_pcb` file (relative to the project directory).
    pub path: PathBuf,
}

impl Command for ImportKicadRules {
    const NAME: &'static str = "board.import_kicad_rules";
    const SUMMARY: &'static str =
        "Import design rules and net classes from KiCad project files (.kicad_pro, .kicad_dru)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = rules::RulesReport;

    fn run(self, ctx: &mut Context<'_>) -> Result<rules::RulesReport, CommandError> {
        ctx.project()?;
        let path = resolve(ctx, &self.path);
        if !path.is_file() {
            return Err(CommandError::not_found("import.file_not_found", format!("{} does not exist", path.display()))
                .with_hint("give the path of the `.kicad_pro` (or `.kicad_dru`) file"));
        }
        let Some((k, diags)) = sibling_rules(&path)? else {
            return Err(CommandError::invalid_args(
                "import.no_rules_file",
                format!("{} is not a `.kicad_pro` or `.kicad_dru` file and has none next to it", path.display()),
            )
            .with_hint("give the KiCad project file (`.kicad_pro`)"));
        };
        for d in diags {
            ctx.report(d);
        }
        let p = ctx.project_mut()?;
        let (mut report, diags) = rules::apply(p, &k);
        let mut more = Vec::new();
        rules::assign(p, &k, &Default::default(), &mut report, &mut more);
        for d in diags.into_iter().chain(more) {
            ctx.report(d);
        }
        Ok(report)
    }

    fn summarize(o: &rules::RulesReport) -> String {
        let mut s =
            format!("{} board rules, {} net classes, {} nets assigned", o.rules.len(), o.netclasses.len(), o.assigned);
        for (k, v) in &o.rules {
            s += &format!("\n  {k} = {v}");
        }
        if !o.netclasses.is_empty() {
            s += &format!("\n  net classes: {}", o.netclasses.join(", "));
        }
        s
    }
}

/// Write the board as a KiCad project: `<name>.kicad_pcb`, plus `<name>.kicad_pro` (net classes,
/// design rules) and `<name>.kicad_dru` (net class width rules) next to it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportKicad {
    /// Output board file (`.kicad_pcb` is appended if missing; relative paths are relative to
    /// the project directory).
    pub path: PathBuf,
}

/// Result of `board.export_kicad`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct KicadExported {
    /// Board file written.
    pub pcb: String,
    /// Project file written.
    pub project: String,
    /// Custom rules file written.
    pub rules: String,
    /// Footprints written.
    pub footprints: usize,
    /// Tracks written.
    pub tracks: usize,
    /// Vias written.
    pub vias: usize,
    /// Zones written.
    pub zones: usize,
    /// Items that could not be exported faithfully.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Command for ExportKicad {
    const NAME: &'static str = "board.export_kicad";
    const SUMMARY: &'static str = "Write the board as a KiCad project (.kicad_pcb, .kicad_pro, .kicad_dru)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = KicadExported;

    fn run(self, ctx: &mut Context<'_>) -> Result<KicadExported, CommandError> {
        let p = ctx.project()?;
        let mut path = match ctx.session.root() {
            Some(root) if self.path.is_relative() => root.join(&self.path),
            _ => self.path.clone(),
        };
        if path.extension().is_none_or(|e| e != "kicad_pcb") {
            let mut s = path.into_os_string();
            s.push(".kicad_pcb");
            path = PathBuf::from(s);
        }
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "board".into());
        let out = crate::kicad_pcb::export(p, &stem);
        let pro = path.with_extension("kicad_pro");
        let dru = path.with_extension("kicad_dru");
        let write = |file: &PathBuf, text: &str| -> Result<(), CommandError> {
            let io = |e| CommandError::from(crate::model::ModelError::Io { path: file.clone(), source: e });
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).map_err(io)?;
            }
            std::fs::write(file, text).map_err(io)
        };
        write(&path, &out.pcb)?;
        write(&pro, &out.project)?;
        write(&dru, &out.rules)?;
        let b = p.board();
        Ok(KicadExported {
            pcb: path.display().to_string(),
            project: pro.display().to_string(),
            rules: dru.display().to_string(),
            footprints: out.footprints,
            tracks: b.tracks.len(),
            vias: b.vias.len(),
            zones: b.zones.len(),
            warnings: out.warnings,
        })
    }

    fn summarize(o: &KicadExported) -> String {
        let mut s = format!(
            "wrote {} ({} footprints, {} tracks, {} vias, {} zones)",
            o.pcb, o.footprints, o.tracks, o.vias, o.zones
        );
        for w in &o.warnings {
            s += &format!("\nwarning: {w}");
        }
        s
    }
}
