//! cadlab: headless electronics CAD, designed to be driven by programs and AI agents.
//!
//! Every operation is a [`command::Command`] in the [`registry()`]. Run commands with
//! [`command::run`] (typed) or [`command::Registry::execute`] (by name, JSON arguments) against
//! a [`command::Session`]. The `cadlab` binary exposes the same registry as a CLI and an MCP
//! server.
//!
//! Layout:
//! - [`units`], [`geom`], [`id`], [`refs`], [`diag`], [`suggest`]: core types. Lengths are integer
//!   nanometers ([`Nm`]), angles integer millidegrees ([`Angle`]); floats never appear in stored data.
//! - [`model`]: the project data model, on-disk format and migrations.
//! - [`command`]: the command system (trait, registry, sessions, transactions, undo).
//! - [`commands`]: the built-in commands, by group.
//!
//! ```
//! use cadlab::prelude::*;
//! use cadlab::commands::project;
//!
//! # let dir = std::env::temp_dir().join(format!("cadlab-doc-{}", std::process::id()));
//! let mut session = Session::new();
//! run(&mut session, project::New { path: dir.clone(), name: Some("demo".into()), description: None, targets: vec![] },
//!     RunOptions::default())?;
//! run(&mut session, project::Set { description: Some("LED blinker".into()), ..Default::default() },
//!     RunOptions::default())?;
//! session.save()?;
//! # std::fs::remove_dir_all(&dir).ok();
//! # Ok::<(), cadlab::command::CommandError>(())
//! ```

#![warn(missing_docs)]

use std::sync::OnceLock;

pub mod board;
pub mod bom;
pub mod command;
pub mod commands;
pub mod config;
pub mod connect;
pub mod diag;
pub mod drc;
pub mod erc;
pub mod fabout;
pub mod geom;
pub mod id;
pub mod kicad_pcb;
pub mod landpattern;
pub mod model;
pub mod netlist;
pub mod partspec;
pub mod refs;
pub mod render;
pub mod schematic;
pub mod sourcing;
pub mod suggest;
pub mod supplier;
pub mod symbolgen;
pub mod units;
pub mod userlib;
pub mod value;

pub use diag::{Diagnostic, Severity};
pub use geom::{BBox, Point, Transform};
pub use id::{IdAllocator, ObjectId};
pub use refs::ObjectRef;
pub use units::{Angle, LengthUnit, Nm, UnitError, mil, mm};
pub use value::{Quantity, Unit};

/// The registry of all built-in commands.
pub fn registry() -> &'static command::Registry {
    static R: OnceLock<command::Registry> = OnceLock::new();
    R.get_or_init(command::Registry::with_builtins)
}

/// Common imports.
pub mod prelude {
    pub use crate::command::{Command, CommandError, Registry, RunOptions, Session, run};
    pub use crate::model::Project;
    pub use crate::registry;
    pub use crate::{Angle, BBox, Diagnostic, LengthUnit, Nm, ObjectRef, Point, Severity, Transform, mil, mm};
}
