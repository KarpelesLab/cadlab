//! Built-in commands, by group.

pub mod block;
pub mod board;
pub mod bom;
pub mod catalog;
pub mod circuit;
pub mod diffpair;
pub mod drc;
pub mod electrical;
pub mod export;
pub mod fab;
pub mod footprint;
pub mod history;
pub mod kicad_pcb;
pub mod lib;
pub mod net;
pub mod part;
pub mod placement;
pub mod project;
pub mod render;
pub mod route;
mod util;
pub mod zone;

use crate::command::Registry;

/// Registers every built-in command.
pub fn register_all(r: &mut Registry) {
    project::register(r);
    part::register(r);
    footprint::register(r);
    circuit::register(r);
    net::register(r);
    diffpair::register(r);
    block::register(r);
    bom::register(r);
    board::register(r);
    electrical::register(r);
    placement::register(r);
    lib::register(r);
    catalog::register(r);
    drc::register(r);
    zone::register(r);
    route::register(r);
    render::register(r);
    kicad_pcb::register(r);
    export::register(r);
    fab::register(r);
    history::register(r);
}
