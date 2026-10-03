//! `fab.*`: fab profiles, manufacturability checks and fab-specific export (DECISIONS D11, D12).
//! See `docs/MANUFACTURING.md`.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::{Diagnostic, Severity};
use crate::fab::check::{self, Choices};
use crate::fab::export::{BundleInput, LOCK_FILE};
use crate::fab::{FabProfile, ProfileSource, Profiles};
use crate::fabout::Options;
use crate::refs::ObjectRef;

pub(crate) fn register(r: &mut Registry) {
    r.register::<List>().register::<Show>().register::<Check>().register::<Compare>().register::<Export>();
}

/// Loads the profiles, reporting unreadable user files.
fn profiles(ctx: &mut Context<'_>) -> Profiles {
    let ps = Profiles::load();
    for w in &ps.warnings {
        ctx.report(
            Diagnostic::warning("fab.profile_invalid", format!("fab profile skipped: {w}"))
                .with_hint("fix or remove the file in ~/.config/cadlab/fab-profiles/"),
        );
    }
    ps
}

fn profile<'a>(ps: &'a Profiles, id: &str) -> Result<(&'a FabProfile, ProfileSource), CommandError> {
    let key = id.to_ascii_lowercase();
    ps.profiles.get(&key).map(|(p, s)| (p, *s)).ok_or_else(|| {
        let ids = ps.ids();
        let near = crate::suggest::did_you_mean(id, ids.iter().map(String::as_str), 3);
        CommandError::not_found("fab.unknown", format!("no fab profile `{id}`"))
            .with_suggestions(&near)
            .with_hint_if_none(format!("profiles: {}", ids.join(", ")))
    })
}

fn default_boards() -> u64 {
    1
}

fn yes() -> bool {
    true
}

/// A profile in the list.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProfileInfo {
    /// ID.
    pub id: String,
    /// Name.
    pub name: String,
    /// Home page.
    pub website: String,
    /// Date the values were verified.
    pub verified_at: String,
    /// Built-in, user or merged.
    pub source: ProfileSource,
    /// Process IDs with their layer counts.
    pub processes: Vec<String>,
    /// Whether the fab assembles boards.
    pub assembly: bool,
    /// Number of values marked unverified.
    pub unverified: usize,
}

/// Profiles.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProfileList {
    /// Profiles by ID.
    pub profiles: Vec<ProfileInfo>,
}

/// List the fab profiles (built-in and user).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {}

impl Command for List {
    const NAME: &'static str = "fab.list";
    const SUMMARY: &'static str = "List fab profiles (built-in and ~/.config/cadlab/fab-profiles)";
    const KIND: CommandKind = CommandKind::Query;
    type Output = ProfileList;

    fn run(self, ctx: &mut Context<'_>) -> Result<ProfileList, CommandError> {
        let ps = profiles(ctx);
        let profiles = ps
            .profiles
            .values()
            .map(|(p, s)| ProfileInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                website: p.website.clone(),
                verified_at: p.verified_at.clone(),
                source: *s,
                processes: p
                    .processes
                    .iter()
                    .map(|q| {
                        let l: Vec<String> = q.layers.iter().map(u8::to_string).collect();
                        format!("{} ({}L)", q.id, l.join("/"))
                    })
                    .collect(),
                assembly: p.assembly.is_some(),
                unverified: p.unverified().len(),
            })
            .collect();
        Ok(ProfileList { profiles })
    }

    fn summarize(o: &ProfileList) -> String {
        o.profiles
            .iter()
            .map(|p| {
                format!(
                    "{:<10} {} (verified {}, {:?}): {}{}",
                    p.id,
                    p.name,
                    p.verified_at,
                    p.source,
                    p.processes.join(", "),
                    if p.assembly { ", assembly" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A profile in full.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProfileShown {
    /// Built-in, user or merged.
    pub source: ProfileSource,
    /// Values that could not be confirmed on the sources.
    pub unverified: Vec<String>,
    /// The profile.
    pub profile: FabProfile,
}

/// Show a fab profile: processes and limits, file conventions, assembly layouts, sources.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Show {
    /// Profile ID (`jlcpcb`, `pcbway`, `generic`).
    pub fab: String,
}

impl Command for Show {
    const NAME: &'static str = "fab.show";
    const SUMMARY: &'static str = "Show a fab profile (capabilities, file conventions, sources)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["fab"];
    type Output = ProfileShown;

    fn run(self, ctx: &mut Context<'_>) -> Result<ProfileShown, CommandError> {
        let ps = profiles(ctx);
        let (p, source) = profile(&ps, &self.fab)?;
        Ok(ProfileShown { source, unverified: p.unverified(), profile: p.clone() })
    }

    fn summarize(o: &ProfileShown) -> String {
        let p = &o.profile;
        let mut s = format!("{} ({}), verified {}, {:?}\n", p.name, p.id, p.verified_at, o.source);
        for q in &p.processes {
            let opt = |v: Option<crate::Nm>| v.map_or("-".to_string(), |x| x.to_string());
            s.push_str(&format!(
                "  {}: {}; track {} space {} drill {} edge {}\n",
                q.id,
                q.name,
                opt(q.min_track),
                opt(q.min_space),
                opt(q.min_drill),
                opt(q.copper_to_edge)
            ));
        }
        if !o.unverified.is_empty() {
            s.push_str(&format!("  unverified: {}\n", o.unverified.join(", ")));
        }
        s.push_str(&format!("  sources: {}", p.sources.join(" ")));
        s
    }
}

/// Result of `fab.check`; the findings are the command's diagnostics.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CheckReport {
    /// Profile ID.
    pub fab: String,
    /// Process used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Board options that would be ordered.
    pub choices: Choices,
    /// Errors (the fab cannot make or assemble the board as is).
    pub errors: usize,
    /// Warnings.
    pub warnings: usize,
    /// Profile values marked unverified.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
}

/// Runs the board check and, when asked, parts availability.
fn run_check(
    ctx: &Context<'_>,
    p: &FabProfile,
    process: Option<&str>,
    parts: bool,
    boards: u64,
) -> Result<(check::Report, Vec<check::LinePick>), CommandError> {
    let project = ctx.project()?;
    let mut report = check::check(project, p, process);
    if let Some(d) = report.diagnostics.iter().find(|d| d.code == "fab.unknown_process") {
        let mut e = CommandError::invalid_args("fab.unknown_process", d.message.clone());
        if let Some(h) = &d.hint {
            e = e.with_hint(h.clone());
        }
        return Err(e);
    }
    let mut picks = Vec::new();
    if parts && p.assembly.is_some() {
        let rows = crate::bom::rows(project);
        let (pk, diags) = check::parts(&rows, p, &ctx.session.suppliers, boards);
        picks = pk;
        report.diagnostics.extend(diags);
    }
    Ok((report, picks))
}

/// Check whether a fab can make (and assemble) the board: layer count, thickness, copper, finish
/// and color preferences, board size, the fab's minimum track/space, drills, annular rings,
/// hole-to-hole, copper-to-edge and silkscreen (cadlab's DRC with the fab's limits; the project's
/// rules are not changed), assembly sides and package sizes, and parts availability through the
/// configured suppliers.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Profile ID (`jlcpcb`, `pcbway`, `generic`).
    pub fab: String,
    /// Process ID (default: the first offering the board's layer count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Check parts availability through the configured suppliers.
    #[serde(default = "yes")]
    pub parts: bool,
    /// Number of boards to build (sets the stock needed).
    #[serde(default = "default_boards")]
    pub boards: u64,
}

impl Command for Check {
    const NAME: &'static str = "fab.check";
    const SUMMARY: &'static str = "Check the board against a fab profile (capabilities, limits, assembly, parts)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["fab"];
    type Output = CheckReport;

    fn run(self, ctx: &mut Context<'_>) -> Result<CheckReport, CommandError> {
        let ps = profiles(ctx);
        let (p, _) = profile(&ps, &self.fab)?;
        let (report, _) = run_check(ctx, p, self.process.as_deref(), self.parts, self.boards)?;
        let out = CheckReport {
            fab: p.id.clone(),
            process: report.process.clone(),
            choices: report.choices.clone(),
            errors: report.count(Severity::Error),
            warnings: report.count(Severity::Warning),
            unverified: p.unverified(),
        };
        for d in report.diagnostics {
            ctx.report(d);
        }
        Ok(out)
    }

    fn summarize(o: &CheckReport) -> String {
        let process = o.process.as_deref().unwrap_or("no process");
        if o.errors == 0 && o.warnings == 0 {
            format!("{} ({process}): OK", o.fab)
        } else {
            format!("{} ({process}): {} error(s), {} warning(s)", o.fab, o.errors, o.warnings)
        }
    }
}

/// One fab in a comparison.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Feasibility {
    /// Profile ID.
    pub fab: String,
    /// Process used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// No errors.
    pub feasible: bool,
    /// Errors.
    pub errors: usize,
    /// Warnings.
    pub warnings: usize,
    /// Codes of the failing constraints (errors), sorted.
    pub failing: Vec<String>,
    /// Codes of the warnings, sorted.
    pub warned: Vec<String>,
}

/// Comparison table.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Comparison {
    /// One row per fab, in the order asked (default: by ID).
    pub fabs: Vec<Feasibility>,
}

/// Compare fabs side by side: can each make the board, and which constraints fail.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Compare {
    /// Profile IDs (default: every profile).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fabs: Vec<String>,
    /// Also check parts availability through the configured suppliers.
    #[serde(default)]
    pub parts: bool,
    /// Number of boards to build (with `parts`).
    #[serde(default = "default_boards")]
    pub boards: u64,
}

impl Command for Compare {
    const NAME: &'static str = "fab.compare";
    const SUMMARY: &'static str = "Compare fabs: feasibility, error/warning counts, failing constraints";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["fabs"];
    type Output = Comparison;

    fn run(self, ctx: &mut Context<'_>) -> Result<Comparison, CommandError> {
        let ps = profiles(ctx);
        let ids = if self.fabs.is_empty() { ps.ids() } else { self.fabs.clone() };
        let mut rows = Vec::new();
        for id in ids {
            let (p, _) = profile(&ps, &id)?;
            let (report, _) = run_check(ctx, p, None, self.parts, self.boards.max(1))?;
            let codes = |s: Severity| {
                let mut v: Vec<String> =
                    report.diagnostics.iter().filter(|d| d.severity == s).map(|d| d.code.to_string()).collect();
                v.sort();
                v.dedup();
                v
            };
            let errors = report.count(Severity::Error);
            rows.push(Feasibility {
                fab: p.id.clone(),
                process: report.process.clone(),
                feasible: errors == 0,
                errors,
                warnings: report.count(Severity::Warning),
                failing: codes(Severity::Error),
                warned: codes(Severity::Warning),
            });
        }
        Ok(Comparison { fabs: rows })
    }

    fn summarize(o: &Comparison) -> String {
        let mut s = format!("{:<10} {:<12} {:>6} {:>8}  failing", "fab", "process", "errors", "warnings");
        for r in &o.fabs {
            s.push_str(&format!(
                "\n{:<10} {:<12} {:>6} {:>8}  {}",
                r.fab,
                r.process.as_deref().unwrap_or("-"),
                r.errors,
                r.warnings,
                if r.failing.is_empty() { "-".to_string() } else { r.failing.join(", ") }
            ));
        }
        s
    }
}

/// A written file.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WrittenFile {
    /// Path written.
    pub path: String,
    /// What it is.
    pub function: String,
    /// SHA-256 (as in the lock).
    pub sha256: String,
}

/// Result of `fab.export`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FabExported {
    /// Profile ID.
    pub fab: String,
    /// Process used.
    pub process: String,
    /// Files written, the lock last.
    pub files: Vec<WrittenFile>,
}

/// Write a fab's complete file set: Gerber and drill files named its way, its zip archive, BOM
/// and placement files in its column layouts (rotation offsets applied here, never stored in the
/// project), and `fab-lock.json` recording the profile, process options, file hashes and the part
/// chosen for each BOM line. Refuses when `fab.check` finds errors, unless `force`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Export {
    /// Profile ID (`jlcpcb`, `pcbway`, `generic`).
    pub fab: String,
    /// Output directory (default `out/fab/<fab>`, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Process ID (default: the first offering the board's layer count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Number of boards to build (sets the stock needed when choosing parts).
    #[serde(default = "default_boards")]
    pub boards: u64,
    /// Export even when the fab check finds errors.
    #[serde(default)]
    pub force: bool,
}

fn io(path: &Path, e: std::io::Error) -> CommandError {
    crate::model::ModelError::Io { path: path.to_path_buf(), source: e }.into()
}

impl Command for Export {
    const NAME: &'static str = "fab.export";
    const SUMMARY: &'static str = "Write a fab's file set (named Gerbers, drills, zip, BOM, CPL) and fab-lock.json";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["fab", "dir"];
    type Output = FabExported;

    fn run(self, ctx: &mut Context<'_>) -> Result<FabExported, CommandError> {
        let ps = profiles(ctx);
        let (p, source) = profile(&ps, &self.fab)?;
        let (report, picks) = run_check(ctx, p, self.process.as_deref(), true, self.boards)?;
        let errors = report.count(Severity::Error);
        let process_id = report.process.clone();
        let choices = report.choices.clone();
        for d in report.diagnostics {
            ctx.report(d);
        }
        let Some(process_id) = process_id else {
            return Err(CommandError::invalid_args("fab.layers", format!("{} has no process for this board", p.name))
                .with_hint("see fab.check"));
        };
        if errors > 0 && !self.force {
            return Err(CommandError::conflict(
                "fab.check_failed",
                format!("{} cannot make the board as is: fab.check found {errors} error(s)", p.name),
            )
            .with_hint("fix them (fab.check lists them), choose another fab (fab.compare), or pass force"));
        }
        let project = ctx.project()?;
        if crate::fabout::excellon::too_many_tools(project) {
            return Err(CommandError::invalid_args(
                "export.too_many_tools",
                format!("a drill file would need more than {} tools", crate::fabout::excellon::MAX_TOOLS),
            )
            .with_hint("use fewer distinct drill diameters (via and pad drills)"));
        }
        let dnp = &project.bom().dnp;
        let unplaced: Vec<String> = project
            .circuit()
            .components
            .keys()
            .filter(|r| !dnp.contains(*r) && !project.board().footprints.contains_key(*r))
            .cloned()
            .collect();
        let process = p.process(&process_id).expect("process chosen by the check");
        let input = BundleInput { profile: p, source, process, choices, picks, options: Options::default() };
        let (files, _lock) = crate::fab::export::bundle(project, &input).map_err(|e| io(Path::new(LOCK_FILE), e))?;
        for r in unplaced {
            ctx.report(
                Diagnostic::warning(
                    "export.unplaced",
                    format!("`{r}` is not placed and is missing from assembly outputs"),
                )
                .with_subject(ObjectRef::Name(r))
                .with_hint("place it with `place.set` or `place.auto`, or mark it DNP with `bom.dnp`"),
            );
        }
        let rel = self.dir.clone().unwrap_or_else(|| Path::new("out/fab").join(&p.id));
        let dir = match ctx.session.root() {
            Some(root) if rel.is_relative() => root.join(rel),
            _ => rel,
        };
        std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        let mut written = Vec::new();
        for f in files {
            let path = dir.join(&f.name);
            std::fs::write(&path, &f.content).map_err(|e| io(&path, e))?;
            written.push(WrittenFile {
                path: path.display().to_string(),
                function: f.function,
                sha256: crate::fab::sha256_hex(&f.content),
            });
        }
        Ok(FabExported { fab: p.id.clone(), process: process_id, files: written })
    }

    fn summarize(o: &FabExported) -> String {
        let mut s = format!("{} ({}): wrote {} file(s)", o.fab, o.process, o.files.len());
        for f in &o.files {
            s.push_str(&format!("\n  {} ({})", f.path, f.function));
        }
        s
    }
}
