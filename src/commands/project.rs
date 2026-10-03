//! `project.*`: create, open, save, inspect and configure projects.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::LengthUnit;
use crate::model::{MANIFEST_FILE, ModelError, Project};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry, Session};

pub(crate) fn register(r: &mut Registry) {
    r.register::<New>().register::<Open>().register::<Save>().register::<Info>().register::<Set>();
}

/// Project settings (the editable part of the manifest).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectSettings {
    /// Project name.
    pub name: String,
    /// Description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Display unit for lengths.
    pub display_units: LengthUnit,
    /// Fab compatibility targets.
    pub targets: Vec<String>,
    /// Metadata.
    pub metadata: BTreeMap<String, String>,
}

impl ProjectSettings {
    fn of(p: &Project) -> Self {
        let m = p.manifest();
        ProjectSettings {
            name: m.name.clone(),
            description: m.description.clone(),
            display_units: m.display_units,
            targets: m.targets.clone(),
            metadata: m.metadata.clone(),
        }
    }

    fn text(&self) -> String {
        let mut s = format!("project `{}`", self.name);
        if let Some(d) = &self.description {
            s += &format!("\n  {d}");
        }
        s += &format!("\n  units: {}", self.display_units.suffix());
        if !self.targets.is_empty() {
            s += &format!("\n  targets: {}", self.targets.join(", "));
        }
        for (k, v) in &self.metadata {
            s += &format!("\n  {k}: {v}");
        }
        s
    }
}

/// Project overview: settings plus location and session state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectInfo {
    /// Settings.
    #[serde(flatten)]
    pub settings: ProjectSettings,
    /// Project directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// File format version.
    pub schema_version: u32,
    /// Whether there are unsaved changes.
    pub unsaved_changes: bool,
    /// Available undo steps.
    pub undo_depth: usize,
    /// Available redo steps.
    pub redo_depth: usize,
}

impl ProjectInfo {
    fn of(ctx: &Context<'_>) -> Result<Self, CommandError> {
        let p = ctx.project()?;
        let s = &*ctx.session;
        Ok(ProjectInfo {
            settings: ProjectSettings::of(p),
            path: s.root().map(|p| p.display().to_string()),
            schema_version: p.manifest().schema_version,
            unsaved_changes: s.is_dirty(),
            undo_depth: s.history.undo_len(),
            redo_depth: s.history.redo_len(),
        })
    }

    fn text(&self) -> String {
        let mut s = self.settings.text();
        if let Some(p) = &self.path {
            s += &format!("\n  path: {p}");
        }
        s += &format!("\n  undo: {} step(s), redo: {}", self.undo_depth, self.redo_depth);
        if self.unsaved_changes {
            s += "\n  (unsaved changes)";
        }
        s
    }
}

/// Validates and normalizes fab target IDs: lowercase, `[a-z0-9_-]`, deduplicated.
fn normalize_targets(targets: Vec<String>) -> Result<Vec<String>, CommandError> {
    let mut out: Vec<String> = Vec::new();
    for t in targets {
        let t = t.trim().to_ascii_lowercase();
        if t.is_empty() || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(CommandError::invalid_args("project.invalid_target", format!("invalid fab target `{t}`"))
                .with_hint("fab profile IDs look like `jlcpcb`, `pcbway`, `oshpark`"));
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

/// Create a new project directory.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct New {
    /// Directory to create the project in (created if missing).
    pub path: PathBuf,
    /// Project name. Defaults to the directory name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Fab profiles to stay compatible with (adds checks only), e.g. ["jlcpcb", "pcbway"].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
}

impl Command for New {
    const NAME: &'static str = "project.new";
    const SUMMARY: &'static str = "Create a new project directory (written immediately)";
    const KIND: CommandKind = CommandKind::Session;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = ProjectInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<ProjectInfo, CommandError> {
        if self.path.join(MANIFEST_FILE).exists() {
            return Err(ModelError::AlreadyExists(self.path).into());
        }
        let name = match self.name {
            Some(n) => n,
            None => default_name(&self.path)?,
        };
        if name.trim().is_empty() {
            return Err(CommandError::invalid_args("project.invalid_name", "project name cannot be empty"));
        }
        let mut p = Project::new(name.trim());
        p.manifest_mut().description = self.description;
        p.manifest_mut().targets = normalize_targets(self.targets)?;
        ctx.session.create(&self.path, p);
        // Creating a project creates its directory, whatever the autosave policy.
        ctx.session.save()?;
        ProjectInfo::of(ctx)
    }

    fn summarize(o: &ProjectInfo) -> String {
        format!("created {}", o.text())
    }
}

fn default_name(path: &Path) -> Result<String, CommandError> {
    std::path::absolute(path).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).ok_or_else(
        || {
            CommandError::invalid_args("project.invalid_name", "cannot derive a project name from the path")
                .with_hint("pass `name` explicitly")
        },
    )
}

/// Open an existing project.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Open {
    /// Project directory (containing cadlab.toml).
    pub path: PathBuf,
}

impl Command for Open {
    const NAME: &'static str = "project.open";
    const SUMMARY: &'static str = "Open an existing project";
    const KIND: CommandKind = CommandKind::Session;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = ProjectInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<ProjectInfo, CommandError> {
        let (mut session, warnings) = Session::open(&self.path)?;
        session.suppliers = std::mem::take(&mut ctx.session.suppliers);
        *ctx.session = session;
        for w in warnings {
            ctx.report(w);
        }
        ProjectInfo::of(ctx)
    }

    fn summarize(o: &ProjectInfo) -> String {
        format!("opened {}", o.text())
    }
}

/// Save the project to disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Save {}

/// Result of `project.save`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SaveResult {
    /// Project directory.
    pub path: String,
    /// Files written (unchanged files are skipped).
    pub written: Vec<String>,
    /// Files removed.
    pub removed: Vec<String>,
}

impl Command for Save {
    const NAME: &'static str = "project.save";
    const SUMMARY: &'static str = "Save the project to disk";
    const KIND: CommandKind = CommandKind::Session;
    type Output = SaveResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<SaveResult, CommandError> {
        ctx.project()?;
        let report = ctx.session.save()?;
        let root = ctx.session.root().map(Path::to_path_buf).unwrap_or_default();
        let rel = |v: Vec<PathBuf>| -> Vec<String> {
            v.iter().map(|p| p.strip_prefix(&root).unwrap_or(p).display().to_string()).collect()
        };
        Ok(SaveResult { path: root.display().to_string(), written: rel(report.written), removed: rel(report.removed) })
    }

    fn summarize(o: &SaveResult) -> String {
        if o.written.is_empty() && o.removed.is_empty() {
            format!("saved {} (no changes)", o.path)
        } else {
            format!("saved {} ({} file(s) written)", o.path, o.written.len() + o.removed.len())
        }
    }
}

/// Show project information.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Info {}

impl Command for Info {
    const NAME: &'static str = "project.info";
    const SUMMARY: &'static str = "Show project information";
    const KIND: CommandKind = CommandKind::Query;
    type Output = ProjectInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<ProjectInfo, CommandError> {
        ProjectInfo::of(ctx)
    }

    fn summarize(o: &ProjectInfo) -> String {
        o.text()
    }
}

/// Change project settings. Only the given fields change.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Set {
    /// New name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New description (empty string clears it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Display unit for lengths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_units: Option<LengthUnit>,
    /// Replace the fab compatibility targets (empty list clears them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<String>>,
    /// Metadata to set; a null value removes the key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Option<String>>,
}

impl Command for Set {
    const NAME: &'static str = "project.set";
    const SUMMARY: &'static str = "Change project name, description, units, targets or metadata";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = ProjectSettings;

    fn run(self, ctx: &mut Context<'_>) -> Result<ProjectSettings, CommandError> {
        let targets = self.targets.map(normalize_targets).transpose()?;
        if self.name.as_deref().is_some_and(|n| n.trim().is_empty()) {
            return Err(CommandError::invalid_args("project.invalid_name", "project name cannot be empty"));
        }
        let m = ctx.project_mut()?.manifest_mut();
        if let Some(n) = self.name {
            m.name = n.trim().to_string();
        }
        if let Some(d) = self.description {
            m.description = (!d.is_empty()).then_some(d);
        }
        if let Some(u) = self.display_units {
            m.display_units = u;
        }
        if let Some(t) = targets {
            m.targets = t;
        }
        for (k, v) in self.metadata {
            match v {
                Some(v) => m.metadata.insert(k, v),
                None => m.metadata.remove(&k),
            };
        }
        Ok(ProjectSettings::of(ctx.project()?))
    }

    fn summarize(o: &ProjectSettings) -> String {
        format!("updated {}", o.text())
    }
}
