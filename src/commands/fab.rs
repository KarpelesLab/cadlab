//! `fab.*`: fab profiles, manufacturability checks and fab-specific export (DECISIONS D11, D12).
//! See `docs/MANUFACTURING.md`.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::{Diagnostic, Severity};
use crate::fab::check::{self, Choices};
use crate::fab::export::{BundleInput, FabLock, LOCK_FILE, Substitution};
use crate::fab::{FabProfile, ProfileSource, Profiles};
use crate::fabout::Options;
use crate::refs::ObjectRef;
use crate::substitute::{Basis, LineSubstitutes};
use crate::supplier::SearchQuery;

pub(crate) fn register(r: &mut Registry) {
    r.register::<List>()
        .register::<Show>()
        .register::<Check>()
        .register::<Compare>()
        .register::<Export>()
        .register::<Substitute>();
}

/// Loads the profiles, reporting unreadable user files.
pub(crate) fn profiles(ctx: &mut Context<'_>) -> Profiles {
    let ps = Profiles::load();
    for w in &ps.warnings {
        ctx.report(
            Diagnostic::warning("fab.profile_invalid", format!("fab profile skipped: {w}"))
                .with_hint("fix or remove the file in ~/.config/cadlab/fab-profiles/"),
        );
    }
    ps
}

pub(crate) fn profile<'a>(ps: &'a Profiles, id: &str) -> Result<(&'a FabProfile, ProfileSource), CommandError> {
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
    /// Substitute candidates for lines the fab cannot source as designed (suggestions only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substitutes: Vec<LineSubstitutes>,
    /// Lines ordered as a substitute recorded in `fab-lock.json`: part → MPN.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substituted: Vec<(String, String)>,
}

fn substituted(picks: &[check::LinePick]) -> Vec<(String, String)> {
    picks.iter().filter(|k| k.replaces.is_some()).map(|k| (k.part.clone(), k.mpn.clone().unwrap_or_default())).collect()
}

fn substitutes_text(lines: &[LineSubstitutes]) -> String {
    let mut s = String::new();
    for l in lines {
        s += &format!("\n  {} ({:?}):", l.part, l.status);
        if l.candidates.is_empty() {
            s += &format!(" no substitute: {}", l.note.as_deref().unwrap_or("-"));
        }
        for (i, c) in l.candidates.iter().enumerate() {
            s += &format!("\n    {}. {} [{:?}]", i + 1, crate::substitute::describe(c), c.basis);
        }
    }
    s
}

/// Runs the board check and, when asked, parts availability (with the substitutions applied in
/// the lock at `out`, and substitute candidates for unavailable lines).
fn run_check(
    ctx: &mut Context<'_>,
    p: &FabProfile,
    process: Option<&str>,
    parts: bool,
    boards: u64,
    out: &Path,
    candidates: usize,
) -> Result<(check::Report, check::PartsReport), CommandError> {
    let project = ctx.project()?;
    let mut report = check::check(project, p, process);
    if let Some(d) = report.diagnostics.iter().find(|d| d.code == "fab.unknown_process") {
        let mut e = CommandError::invalid_args("fab.unknown_process", d.message.clone());
        if let Some(h) = &d.hint {
            e = e.with_hint(h.clone());
        }
        return Err(e);
    }
    let mut pr = check::PartsReport::default();
    if parts && p.assembly.is_some() {
        let applied = applied_substitutions(ctx, p, out);
        let project = ctx.project()?;
        let rows = crate::bom::rows(project);
        pr = check::parts(project, &rows, p, &ctx.session.suppliers, boards, &applied, candidates.clamp(1, 20));
        report.diagnostics.append(&mut pr.diagnostics);
    }
    Ok((report, pr))
}

/// Output directory of a fab: `dir` (relative to the project), default `out/fab/<fab>`.
pub(crate) fn out_dir(ctx: &Context<'_>, fab: &str, dir: Option<&Path>) -> PathBuf {
    let rel = dir.map(Path::to_path_buf).unwrap_or_else(|| Path::new("out/fab").join(fab));
    match ctx.session.root() {
        Some(root) if rel.is_relative() => root.join(rel),
        _ => rel,
    }
}

/// Substitutions recorded in `<out>/fab-lock.json` for this fab (none when there is no lock;
/// an unreadable lock or one of another fab is reported and ignored).
pub(crate) fn applied_substitutions(ctx: &mut Context<'_>, p: &FabProfile, out: &Path) -> Vec<Substitution> {
    let path = out.join(LOCK_FILE);
    match FabLock::read(&path) {
        Ok(None) => Vec::new(),
        Ok(Some(l)) if l.profile.id == p.id => l.substitutions,
        Ok(Some(l)) => {
            ctx.report(
                Diagnostic::warning(
                    "fab.lock_mismatch",
                    format!(
                        "{} is a {} lock; its substitutions are ignored for {}",
                        path.display(),
                        l.profile.id,
                        p.id
                    ),
                )
                .with_hint("use a separate output directory per fab (the default out/fab/<fab>)"),
            );
            Vec::new()
        }
        Err(e) => {
            ctx.report(
                Diagnostic::warning("fab.lock_invalid", format!("cannot read the lock, substitutions ignored: {e}"))
                    .with_hint("fix the file or delete it (fab.export writes a new one)"),
            );
            Vec::new()
        }
    }
}

fn three() -> usize {
    3
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
    /// Substitute candidates to list per unavailable line.
    #[serde(default = "three")]
    pub candidates: usize,
    /// Directory of the fab's `fab-lock.json`, whose substitutions apply (default
    /// `out/fab/<fab>`, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
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
        let out = out_dir(ctx, &p.id, self.dir.as_deref());
        let (report, parts) =
            run_check(ctx, p, self.process.as_deref(), self.parts, self.boards, &out, self.candidates)?;
        let out = CheckReport {
            fab: p.id.clone(),
            process: report.process.clone(),
            choices: report.choices.clone(),
            errors: report.count(Severity::Error),
            warnings: report.count(Severity::Warning),
            unverified: p.unverified(),
            substituted: substituted(&parts.picks),
            substitutes: parts.substitutes,
        };
        for d in report.diagnostics {
            ctx.report(d);
        }
        Ok(out)
    }

    fn summarize(o: &CheckReport) -> String {
        let process = o.process.as_deref().unwrap_or("no process");
        let mut s = if o.errors == 0 && o.warnings == 0 {
            format!("{} ({process}): OK", o.fab)
        } else {
            format!("{} ({process}): {} error(s), {} warning(s)", o.fab, o.errors, o.warnings)
        };
        for (part, mpn) in &o.substituted {
            s += &format!("\n  {part}: substituted by {mpn} (fab-lock.json)");
        }
        if !o.substitutes.is_empty() {
            s += "\nsubstitute candidates (apply with fab.substitute):";
            s += &substitutes_text(&o.substitutes);
        }
        s
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
            let out = out_dir(ctx, &p.id, None);
            let (report, _) = run_check(ctx, p, None, self.parts, self.boards.max(1), &out, 1)?;
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
    /// Substitute candidates for lines the fab cannot source as designed (suggestions only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substitutes: Vec<LineSubstitutes>,
    /// Lines ordered as a substitute recorded in `fab-lock.json`: part → MPN.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substituted: Vec<(String, String)>,
}

/// Write a fab's complete file set: Gerber and drill files named its way, its zip archive, BOM
/// and placement files in its column layouts (rotation offsets applied here, never stored in the
/// project), and `fab-lock.json` recording the profile, process options, file hashes and the part
/// chosen for each BOM line. Substitutions recorded in the existing lock (`fab.substitute`) are
/// used and kept; substitute candidates for unavailable lines are reported. Refuses when
/// `fab.check` finds errors, unless `force`.
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
        let dir = out_dir(ctx, &p.id, self.dir.as_deref());
        let (report, parts) = run_check(ctx, p, self.process.as_deref(), true, self.boards, &dir, 3)?;
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
        let substituted = substituted(&parts.picks);
        let input = BundleInput {
            profile: p,
            source,
            process,
            choices,
            picks: parts.picks,
            options: Options::default(),
            substitutions: parts.applied,
        };
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
        Ok(FabExported {
            fab: p.id.clone(),
            process: process_id,
            files: written,
            substitutes: parts.substitutes,
            substituted,
        })
    }

    fn summarize(o: &FabExported) -> String {
        let mut s = format!("{} ({}): wrote {} file(s)", o.fab, o.process, o.files.len());
        for f in &o.files {
            s.push_str(&format!("\n  {} ({})", f.path, f.function));
        }
        for (part, mpn) in &o.substituted {
            s += &format!("\n  {part}: substituted by {mpn} (fab-lock.json)");
        }
        if !o.substitutes.is_empty() {
            s += "\nsubstitute candidates (apply with fab.substitute, then export again):";
            s += &substitutes_text(&o.substitutes);
        }
        s
    }
}

/// Apply (or with `remove`, undo) a part substitution for one fab: the BOM line `part` is
/// ordered as `mpn` (default: the best candidate of `bom.substitutes`) at this fab. The choice
/// is recorded in the fab's `fab-lock.json` (created if needed), never in the design (D12):
/// `fab.check` and the next `fab.export` for this fab use it, other fabs do not. The MPN must be
/// one of the line's substitute candidates unless `force`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Substitute {
    /// Profile ID (`jlcpcb`, `pcbway`, `generic`).
    pub fab: String,
    /// Part ID of the BOM line (as in `bom.list`).
    pub part: String,
    /// Substitute MPN (default: the best candidate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    /// Remove the line's substitution instead.
    #[serde(default)]
    pub remove: bool,
    /// Accept an MPN that is not among the candidates (recorded with basis `manual`).
    #[serde(default)]
    pub force: bool,
    /// Number of boards to build (sets the stock needed).
    #[serde(default = "default_boards")]
    pub boards: u64,
    /// Directory of the fab's `fab-lock.json` (default `out/fab/<fab>`, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
}

/// Result of `fab.substitute`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Substituted {
    /// Profile ID.
    pub fab: String,
    /// Part ID of the line.
    pub part: String,
    /// The substitution recorded (none when removed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub substitution: Option<Substitution>,
    /// Lock file written.
    pub lock: String,
    /// Whether a substitution was removed.
    pub removed: bool,
    /// Nothing was written (dry run).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
}

impl Command for Substitute {
    const NAME: &'static str = "fab.substitute";
    const SUMMARY: &'static str = "Apply a substitute part for one fab (recorded in its fab-lock.json, not the design)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["fab", "part", "mpn"];
    type Output = Substituted;

    fn run(self, ctx: &mut Context<'_>) -> Result<Substituted, CommandError> {
        let ps = profiles(ctx);
        let (p, source) = profile(&ps, &self.fab)?;
        let dir = out_dir(ctx, &p.id, self.dir.as_deref());
        let path = dir.join(LOCK_FILE);
        let mut lock = match FabLock::read(&path) {
            Ok(l) => l,
            Err(e) => {
                return Err(CommandError::invalid_args("fab.lock_invalid", format!("cannot read the lock: {e}"))
                    .with_hint("fix the file or delete it (fab.export writes a new one)"));
            }
        };
        if let Some(l) = &lock
            && l.profile.id != p.id
        {
            return Err(CommandError::conflict(
                "fab.lock_mismatch",
                format!("{} is a {} lock, not {}", path.display(), l.profile.id, p.id),
            )
            .with_hint("use the fab's own directory (the default out/fab/<fab>)"));
        }
        let project = ctx.project()?;
        let rows = crate::bom::rows(project);
        let Some(row) = rows.iter().find(|r| r.part == self.part).cloned() else {
            let near = crate::suggest::did_you_mean(&self.part, rows.iter().map(|r| r.part.as_str()), 3);
            return Err(CommandError::not_found("bom.line_not_found", format!("no BOM line for part `{}`", self.part))
                .with_suggestions(&near)
                .with_hint_if_none("bom.list lists the lines by part ID"));
        };
        let mut substitution = None;
        let removed = self.remove;
        if self.remove {
            let had = lock.as_ref().is_some_and(|l| l.substitutions.iter().any(|s| s.part == self.part));
            if !had {
                return Err(CommandError::not_found(
                    "fab.not_substituted",
                    format!("`{}` has no substitution for {}", self.part, p.name),
                )
                .with_hint("fab.check lists the substitutions in effect"));
            }
        } else {
            super::part::require_suppliers(ctx)?;
            let suppliers = &ctx.session.suppliers;
            let only = check::catalog_providers(p, suppliers);
            let mut errors = Vec::new();
            let s = crate::sourcing::source_line(&row, self.boards.max(1), suppliers, &only, &mut errors);
            let part = project.library().parts.get(&row.part);
            let found = crate::substitute::for_line(&row, part, s.status, s.needed, suppliers, &only, 20, &mut errors);
            let pick = match &self.mpn {
                None => found.candidates.first().cloned(),
                Some(m) => found.candidates.iter().find(|c| c.offer.mpn.eq_ignore_ascii_case(m)).cloned(),
            };
            let chosen = match (pick, &self.mpn) {
                (Some(c), _) => Some((c.basis, c.offer)),
                (None, None) => {
                    return Err(CommandError::not_found(
                        "fab.no_substitute",
                        format!("no substitute candidate for `{}` at {}", self.part, p.name),
                    )
                    .with_hint(found.note.unwrap_or_else(|| "pass an MPN with force to choose one yourself".into())));
                }
                (None, Some(m)) if !self.force => {
                    let list: Vec<String> = found.candidates.iter().map(|c| c.offer.mpn.clone()).collect();
                    return Err(CommandError::invalid_args(
                        "fab.not_a_candidate",
                        format!("{m} is not a substitute candidate for `{}`", self.part),
                    )
                    .with_hint(if list.is_empty() {
                        "there are no candidates; pass force to record it anyway (check pinout and ratings yourself)"
                            .to_string()
                    } else {
                        format!("candidates: {}; or pass force to record it anyway", list.join(", "))
                    }));
                }
                (None, Some(m)) => {
                    let r = suppliers.lookup(m, &only);
                    errors.extend(r.errors);
                    let mut offers = r.candidates;
                    SearchQuery { quantity: s.needed.max(1), include_obsolete: true, ..Default::default() }
                        .rank(&mut offers);
                    match offers.into_iter().next() {
                        Some(o) => Some((Basis::Manual, o)),
                        None => {
                            substitution = Some(Substitution {
                                part: row.part.clone(),
                                replaces: row.order_mpn().map(|(_, m)| m.to_string()),
                                manufacturer: None,
                                mpn: m.clone(),
                                provider: None,
                                sku: None,
                                basis: Basis::Manual,
                            });
                            None
                        }
                    }
                }
            };
            if let Some((basis, o)) = chosen {
                substitution = Some(Substitution {
                    part: row.part.clone(),
                    replaces: row.order_mpn().map(|(_, m)| m.to_string()),
                    manufacturer: o.manufacturer,
                    mpn: o.mpn,
                    provider: Some(o.provider),
                    sku: Some(o.sku),
                    basis,
                });
            }
            errors.sort();
            errors.dedup();
            super::part::report_provider_errors(ctx, &errors);
        }
        let project = ctx.project()?;
        let mut l = lock.take().unwrap_or_else(|| FabLock {
            lock_version: crate::fab::export::LOCK_VERSION,
            generator: format!("cadlab {}", Options::default().version),
            project: project.manifest().name.clone(),
            profile: crate::fab::export::LockProfile {
                id: p.id.clone(),
                name: p.name.clone(),
                verified_at: p.verified_at.clone(),
                source,
            },
            process: None,
            files: Vec::new(),
            bom: Vec::new(),
            rotation_offsets: Vec::new(),
            substitutions: Vec::new(),
        });
        l.substitutions.retain(|s| s.part != self.part);
        l.substitutions.extend(substitution.clone());
        l.substitutions.sort_by(|a, b| a.part.cmp(&b.part));
        let dry_run = ctx.is_dry_run();
        if !dry_run {
            std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
            let text = l.to_text().map_err(|e| io(&path, e))?;
            std::fs::write(&path, text).map_err(|e| io(&path, e))?;
        }
        ctx.report(
            Diagnostic::info(
                "fab.export_needed",
                format!("{} records the choice; the exported BOM changes with the next fab.export", LOCK_FILE),
            )
            .with_hint(format!("run fab.export {}", p.id)),
        );
        Ok(Substituted {
            fab: p.id.clone(),
            part: self.part,
            substitution,
            lock: path.display().to_string(),
            removed,
            dry_run,
        })
    }

    fn summarize(o: &Substituted) -> String {
        match &o.substitution {
            Some(s) => format!(
                "{}: {} ordered as {}{}{} ({:?}){}",
                o.fab,
                o.part,
                s.manufacturer.as_deref().map(|m| format!("{m} ")).unwrap_or_default(),
                s.mpn,
                match (&s.provider, &s.sku) {
                    (Some(p), Some(k)) => format!(" [{p} {k}]"),
                    _ => String::new(),
                },
                s.basis,
                if o.dry_run { " [dry run]" } else { "" }
            ),
            None => {
                format!("{}: substitution of {} removed{}", o.fab, o.part, if o.dry_run { " [dry run]" } else { "" })
            }
        }
    }
}
