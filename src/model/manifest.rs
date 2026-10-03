use std::collections::BTreeMap;

use crate::{IdAllocator, LengthUnit};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::migrate::CURRENT_SCHEMA_VERSION;

/// Project manifest, stored as `cadlab.toml`. Hand-editable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// File format version; managed by cadlab.
    pub schema_version: u32,
    /// Project name.
    pub name: String,
    /// Free-form description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Unit used when displaying lengths. Files always store millimeters.
    #[serde(default)]
    pub display_units: LengthUnit,
    /// Fab profiles to stay compatible with. Only adds checks; the project stays
    /// provider-agnostic (DECISIONS D12).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// Next object ID; managed by cadlab.
    #[serde(default)]
    pub next_id: IdAllocator,
    /// Free-form key/value metadata (author, revision, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

impl Manifest {
    /// A manifest for a new project.
    pub fn new(name: impl Into<String>) -> Self {
        Manifest {
            schema_version: CURRENT_SCHEMA_VERSION,
            name: name.into(),
            description: None,
            display_units: LengthUnit::default(),
            targets: Vec::new(),
            next_id: IdAllocator::default(),
            metadata: BTreeMap::new(),
        }
    }
}
