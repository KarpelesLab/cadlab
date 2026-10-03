use std::fmt;

use crate::Diagnostic;
use crate::model::ModelError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Broad error category, for exit codes and protocol mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Arguments are malformed or out of range.
    InvalidArgs,
    /// A referenced object or command does not exist.
    NotFound,
    /// The operation conflicts with the current state.
    Conflict,
    /// No project is open.
    NoProject,
    /// Filesystem or project file error.
    Io,
    /// Cancelled by the caller.
    Cancelled,
    /// Bug in cadlab.
    Internal,
}

/// A failed command: a category plus a full diagnostic (code, message, subjects, hint).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommandError {
    /// Category.
    pub kind: ErrorKind,
    /// Details (boxed to keep `Result<_, CommandError>` small).
    #[serde(flatten)]
    pub diagnostic: Box<Diagnostic>,
}

impl CommandError {
    /// New error.
    pub fn new(kind: ErrorKind, code: &'static str, message: impl Into<String>) -> Self {
        CommandError {
            kind,
            diagnostic: Box::new(Diagnostic::error(code, message)),
        }
    }

    /// Invalid arguments.
    pub fn invalid_args(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgs, code, message)
    }

    /// Something was not found.
    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, code, message)
    }

    /// State conflict.
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, code, message)
    }

    /// Adds a hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.diagnostic = Box::new(self.diagnostic.with_hint(hint));
        self
    }

    /// Adds "did you mean" suggestions as the hint.
    pub fn with_suggestions(mut self, s: &[String]) -> Self {
        self.diagnostic = Box::new(self.diagnostic.with_suggestions(s));
        self
    }

    /// Adds a subject.
    pub fn with_subject(mut self, r: crate::ObjectRef) -> Self {
        self.diagnostic = Box::new(self.diagnostic.with_subject(r));
        self
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostic.fmt(f)
    }
}

impl std::error::Error for CommandError {}

impl From<ModelError> for CommandError {
    fn from(e: ModelError) -> Self {
        match &e {
            ModelError::NotAProject(_) => {
                CommandError::not_found("project.not_found", e.to_string())
                    .with_hint("create one with `project.new`, or pass the project directory")
            }
            ModelError::AlreadyExists(_) => CommandError::conflict("project.exists", e.to_string())
                .with_hint("open it with `project.open` instead"),
            ModelError::NewerSchema { .. } => {
                CommandError::new(ErrorKind::Io, "project.newer_schema", e.to_string())
            }
            ModelError::Invalid { .. } => {
                CommandError::new(ErrorKind::Io, "project.invalid_file", e.to_string())
                    .with_hint("fix the file by hand, or restore it from version control")
            }
            ModelError::Io { .. } => CommandError::new(ErrorKind::Io, "io", e.to_string()),
        }
    }
}
