//! Built-in commands, by group.

pub mod block;
pub mod board;
pub mod bom;
pub mod circuit;
pub mod export;
pub mod footprint;
pub mod history;
pub mod net;
pub mod part;
pub mod project;
pub mod render;
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
    board::register(r);
    render::register(r);
    export::register(r);
    history::register(r);
}
