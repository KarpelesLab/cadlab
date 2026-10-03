//! Built-in commands, by group.

pub mod history;
pub mod project;

use crate::command::Registry;

/// Registers every built-in command.
pub fn register_all(r: &mut Registry) {
    project::register(r);
    history::register(r);
}
