//! The project data model: what a project contains, how it is stored on disk, and how older
//! files are migrated. See `docs/DATA_MODEL.md`.

mod error;
pub mod footprint;
pub mod format;
mod manifest;
pub mod migrate;
pub mod part;
mod project;
mod raw;
pub mod sections;

pub use error::ModelError;
pub use manifest::Manifest;
pub use project::{MANIFEST_FILE, Project, SaveReport, find_project_root};
pub use raw::RawProject;
