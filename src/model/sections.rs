//! Project sections. Filled in by later milestones (parts and BOM in M1, circuit in M2,
//! schematic in M3, board in M4); M0 defines the containers and their files.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parts available to the project (`library/`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Library {}

/// Sourcing overlay: requirements, approved MPNs, DNP (`bom.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bom {}

/// Connectivity, the source of truth: components, nets, blocks (`circuit.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Circuit {}

/// Optional schematic presentation hints (`schematic.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Schematic {}

/// Physical board: stackup, outline, placement, copper (`board.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Board {}
