use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::Diagnostic;
use crate::model::{Project, SaveReport};
use crate::supplier::Suppliers;
use crate::userlib::Libraries;

use super::history::History;
use super::{CommandError, Step};

/// Directory, inside a project, for caches and session state. Never committed.
pub const CACHE_DIR: &str = ".cadlab";

/// An editing session: at most one open project, where it lives on disk, and its history.
///
/// The CLI creates one per invocation (history is persisted, so undo still works); the MCP
/// server keeps sessions alive between calls.
#[derive(Debug, Default)]
pub struct Session {
    /// The open project.
    pub project: Option<Project>,
    root: Option<PathBuf>,
    /// Undo/redo history.
    pub history: History,
    dirty: bool,
    changes: u64,
    pending_log: Vec<Step>,
    /// Part research providers. Empty by default; frontends set them (e.g. from the environment).
    pub suppliers: Suppliers,
    /// Shared libraries for `lib.*` commands. `None` (the default) means the user library and
    /// the directories from the user settings; set it to use other directories (tests, embedding).
    pub libraries: Option<Libraries>,
}

impl Session {
    /// A session without a project.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens the project in `root`, with its persisted history. Problems with the history
    /// (not the project) are reported as warnings and the history starts empty.
    pub fn open(root: &Path) -> Result<(Session, Vec<Diagnostic>), CommandError> {
        let root = absolute(root);
        let project = Project::load(&root)?;
        let mut warnings = Vec::new();
        let history = match History::load(history_dir(&root)) {
            Ok(h) => h,
            Err(e) => {
                warnings.push(
                    Diagnostic::warning("history.unreadable", format!("undo history discarded: {e}"))
                        .with_hint("undo is unavailable for earlier steps; this does not affect the project"),
                );
                History::new(Some(history_dir(&root)))
            }
        };
        Ok((
            Session {
                project: Some(project),
                root: Some(root),
                history,
                dirty: false,
                changes: 0,
                pending_log: Vec::new(),
                suppliers: Suppliers::default(),
                libraries: None,
            },
            warnings,
        ))
    }

    /// Replaces the session content with a new, unsaved project located at `root`.
    pub fn create(&mut self, root: &Path, project: Project) {
        let root = absolute(root);
        self.history = History::new(Some(history_dir(&root)));
        self.project = Some(project);
        self.root = Some(root);
        self.pending_log.clear();
        self.mark_dirty();
    }

    /// Project directory, if any.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Whether there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Marks the session as having unsaved changes.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.changes += 1;
    }

    /// Counter incremented by every change; lets callers detect whether something happened.
    pub fn change_count(&self) -> u64 {
        self.changes
    }

    pub(crate) fn log(&mut self, steps: impl IntoIterator<Item = Step>) {
        self.pending_log.extend(steps);
    }

    /// Writes the project, its history and the operation log.
    pub fn save(&mut self) -> Result<SaveReport, CommandError> {
        let (Some(project), Some(root)) = (&self.project, &self.root) else {
            return Err(CommandError::new(super::ErrorKind::NoProject, "project.none", "no project is open"));
        };
        let report = project.save(root)?;
        let cache = root.join(CACHE_DIR);
        fs::create_dir_all(&cache).map_err(|e| io_err(&cache, e))?;
        let ignore = cache.join(".gitignore");
        if !ignore.exists() {
            fs::write(&ignore, "*\n").map_err(|e| io_err(&ignore, e))?;
        }
        self.history.save()?;
        self.flush_log()?;
        self.dirty = false;
        Ok(report)
    }

    /// Appends pending operation-log entries to `.cadlab/oplog.jsonl`.
    pub(crate) fn flush_log(&mut self) -> Result<(), CommandError> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        if self.pending_log.is_empty() {
            return Ok(());
        }
        let path = root.join(CACHE_DIR).join("oplog.jsonl");
        let mut f = OpenOptions::new().create(true).append(true).open(&path).map_err(|e| io_err(&path, e))?;
        for s in self.pending_log.drain(..) {
            let line = serde_json::to_string(&s).expect("step serializes");
            writeln!(f, "{line}").map_err(|e| io_err(&path, e))?;
        }
        Ok(())
    }
}

fn history_dir(root: &Path) -> PathBuf {
    root.join(CACHE_DIR).join("history")
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn io_err(path: &Path, e: std::io::Error) -> CommandError {
    crate::model::ModelError::Io { path: path.to_path_buf(), source: e }.into()
}
