//! Built-in commands, by group.

pub mod block;
pub mod bom;
pub mod circuit;
pub mod footprint;
pub mod history;
pub mod net;
pub mod part;
pub mod project;
mod util;

use crate::command::Registry;

/// Registers every built-in command.
pub fn register_all(r: &mut Registry) {
    project::register(r);
    part::register(r);
    footprint::register(r);
    circuit::register(r);
    net::register(r);
    block::register(r);
    bom::register(r);
    history::register(r);
}
