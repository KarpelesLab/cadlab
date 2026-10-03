use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::Diagnostic;
use crate::model::Project;
use crate::userlib::{LibError, Libraries};

use super::{CommandError, ErrorKind, Session};

/// Receives progress from long-running commands.
pub trait Progress: Sync {
    /// Reports `done` out of `total` (if known), with a short status message.
    fn report(&self, done: u64, total: Option<u64>, message: &str);
}

/// Ignores progress.
pub struct NoProgress;

impl Progress for NoProgress {
    fn report(&self, _: u64, _: Option<u64>, _: &str) {}
}

/// Cooperative cancellation flag, shared between the caller and a running command.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// What a running command can access.
pub struct Context<'a> {
    /// The session: open project, its location and history.
    pub session: &'a mut Session,
    pub(crate) diagnostics: Vec<Diagnostic>,
    progress: &'a dyn Progress,
    cancel: &'a CancelToken,
    dry_run: bool,
}

impl<'a> Context<'a> {
    pub(crate) fn new(
        session: &'a mut Session,
        progress: &'a dyn Progress,
        cancel: &'a CancelToken,
        dry_run: bool,
    ) -> Self {
        Context { session, diagnostics: Vec::new(), progress, cancel, dry_run }
    }

    /// Whether this is a dry run. Project changes are rolled back automatically; commands with
    /// effects outside the project (e.g. writing to a shared library) must skip them.
    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// The shared libraries: the session's override, else the user library and the directories
    /// from the user settings.
    pub fn libraries(&self) -> Result<Libraries, LibError> {
        match &self.session.libraries {
            Some(l) => Ok(l.clone()),
            None => Libraries::from_settings(),
        }
    }

    /// The open project, or a `project.none` error.
    pub fn project(&self) -> Result<&Project, CommandError> {
        self.session.project.as_ref().ok_or_else(no_project)
    }

    /// The open project, mutably.
    pub fn project_mut(&mut self) -> Result<&mut Project, CommandError> {
        self.session.project.as_mut().ok_or_else(no_project)
    }

    /// Reports a diagnostic alongside the result.
    pub fn report(&mut self, d: Diagnostic) {
        self.diagnostics.push(d);
    }

    /// Reports progress.
    pub fn progress(&self, done: u64, total: Option<u64>, message: &str) {
        self.progress.report(done, total, message);
    }

    /// Returns a `cancelled` error if cancellation was requested. Long-running commands call this
    /// regularly.
    pub fn check_cancelled(&self) -> Result<(), CommandError> {
        if self.cancel.is_cancelled() {
            Err(CommandError::new(ErrorKind::Cancelled, "cancelled", "operation cancelled"))
        } else {
            Ok(())
        }
    }
}

fn no_project() -> CommandError {
    CommandError::new(ErrorKind::NoProject, "project.none", "no project is open")
        .with_hint("open one with `project.open`, create one with `project.new`, or run inside a project directory")
}
