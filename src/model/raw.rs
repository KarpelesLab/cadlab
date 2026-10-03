//! Untyped project content, the stage where migrations run.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::model::ModelError;

/// Project content as untyped JSON values: the manifest plus one value per section
/// (`bom`, `circuit`, `schematic`, `board`). Section `x` lives in `x.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawProject {
    /// `cadlab.toml`, as JSON.
    pub manifest: Value,
    /// Sections by name.
    pub sections: BTreeMap<String, Value>,
}

impl RawProject {
    /// A raw project with only a manifest.
    pub fn from_manifest(manifest: Value) -> Self {
        RawProject {
            manifest,
            sections: BTreeMap::new(),
        }
    }

    /// The manifest's `schema_version`.
    pub fn schema_version(&self) -> Result<u32, ModelError> {
        self.manifest
            .get("schema_version")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| {
                ModelError::invalid("cadlab.toml", "missing or invalid `schema_version`")
            })
    }

    pub(crate) fn set_schema_version(&mut self, v: u32) {
        self.manifest["schema_version"] = Value::from(v);
    }

    /// Single-document form (`project pack`): `{"manifest": ..., "<section>": ...}`.
    pub fn to_packed(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("manifest".into(), self.manifest.clone());
        for (k, v) in &self.sections {
            m.insert(k.clone(), v.clone());
        }
        Value::Object(m)
    }

    /// Inverse of [`RawProject::to_packed`].
    pub fn from_packed(v: Value) -> Result<Self, ModelError> {
        let Value::Object(mut m) = v else {
            return Err(ModelError::invalid("<packed>", "expected a JSON object"));
        };
        let manifest = m
            .remove("manifest")
            .ok_or_else(|| ModelError::invalid("<packed>", "missing `manifest`"))?;
        Ok(RawProject {
            manifest,
            sections: m.into_iter().collect(),
        })
    }
}
