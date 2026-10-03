//! `board.export_kicad`: KiCad board export (`.kicad_pcb` + `.kicad_pro` + `.kicad_dru`).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};

pub(crate) fn register(r: &mut Registry) {
    r.register::<ExportKicad>();
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
