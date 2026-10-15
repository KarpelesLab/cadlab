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
pub mod kicad_lib;
pub mod kicad_pcb;
pub mod lib;
pub mod net;
pub mod part;
pub mod placement;
pub mod project;
pub mod render;
pub mod route;
pub mod touchstone;
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
    kicad_lib::register(r);
    export::register(r);
    touchstone::register(r);
    fab::register(r);
    history::register(r);
}

/// One line per command group, for CLI group help and MCP tool descriptions (a bare
/// "part commands" tells an agent nothing about where to look).
pub const GROUP_SUMMARIES: &[(&str, &str)] = &[
    ("project", "Create, open, save and describe projects"),
    ("part", "Project parts: generic passives, parts from pin lists, supplier search, KiCad symbol import"),
    ("footprint", "Land patterns: generate (IPC-7351B), import from KiCad, pad settings, 3D models"),
    ("circuit", "Components, ERC, design lint, summary, netlist import/export"),
    ("net", "Connect pins to nets, no-connects, net properties"),
    ("netclass", "Net classes: track width, clearance, vias per class of nets"),
    ("diffpair", "Differential pair definitions"),
    ("lengthgroup", "Length-matching groups"),
    ("block", "Reusable circuit blocks"),
    ("bom", "Bill of materials: list, export, approve MPNs, sourcing, cost"),
    ("board", "Board setup, outline, design rules, holes, cutouts, stackup, KiCad board exchange"),
    ("place", "Footprint placement: set, move, rotate, auto-place, align"),
    ("track", "Manual tracks"),
    ("via", "Manual vias"),
    ("impedance", "Track impedance from the stackup"),
    ("current", "Track width for a current (IPC-2152)"),
    ("lib", "Shared user libraries (parts, footprints, blocks outside projects)"),
    ("catalog", "Offline part catalogs and configured providers"),
    ("drc", "Design rule check (`cadlab drc run`, or `cadlab drc`)"),
    ("zone", "Copper pours"),
    ("keepout", "Keep-out areas"),
    ("route", "Autorouter, interactive routing, fanout, length tuning, Specctra session import"),
    ("render", "Render schematic, board (2D/3D), symbols and footprints to PNG/SVG"),
    ("schematic", "Schematic placement overrides and KiCad schematic export"),
    ("export", "Generic fab and MCAD outputs: Gerber, drill, pick-and-place, IPC, STEP; simulation: SPICE, openEMS"),
    ("touchstone", "Read Touchstone (.sNp) S-parameter files: summaries of simulation or measurement results"),
    ("fab", "Fab profiles: check, compare, export a fab's file bundle, substitutes"),
    ("history", "Undo/redo history (alone: list steps)"),
];

/// The one-line summary of a command group (`part` → "Project parts: ..."), if known.
pub fn group_summary(group: &str) -> Option<&'static str> {
    GROUP_SUMMARIES.iter().find(|(g, _)| *g == group).map(|(_, s)| *s)
}
