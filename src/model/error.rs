use std::path::PathBuf;

/// Errors loading or saving a project.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// Filesystem error.
    #[error("{path}: {source}")]
    Io {
        /// File or directory involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// File is not valid JSON/TOML, or does not match the expected structure.
    #[error("{path}: {message}")]
    Invalid {
        /// File involved.
        path: PathBuf,
        /// What is wrong.
        message: String,
    },
    /// The directory has no `cadlab.toml`.
    #[error("{0}: not a cadlab project (no cadlab.toml)")]
    NotAProject(PathBuf),
    /// A project already exists there.
    #[error("{0}: a cadlab project already exists here")]
    AlreadyExists(PathBuf),
    /// Written by a newer cadlab.
    #[error(
        "project schema version {found} is newer than this cadlab supports ({supported}); upgrade cadlab"
    )]
    NewerSchema {
        /// Version in the files.
        found: u32,
        /// Highest version this build supports.
        supported: u32,
    },
}

impl ModelError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        ModelError::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn invalid(path: impl Into<PathBuf>, message: impl ToString) -> Self {
        ModelError::Invalid {
            path: path.into(),
            message: message.to_string(),
        }
    }
}
