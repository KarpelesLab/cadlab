//! Untyped project content, the stage where migrations run.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::model::ModelError;

/// Project content as untyped JSON values: the manifest, one value per section (`bom`,
/// `circuit`, `schematic`, `board`; section `x` lives in `x.json`), and library items
/// (`library/parts/<id>.json`, `library/footprints/<name>.json`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawProject {
    /// `cadlab.toml`, as JSON.
    pub manifest: Value,
    /// Sections by name.
    pub sections: BTreeMap<String, Value>,
    /// Library parts by ID.
    pub parts: BTreeMap<String, Value>,
    /// Library footprints by name.
    pub footprints: BTreeMap<String, Value>,
}

impl RawProject {
    /// A raw project with only a manifest.
    pub fn from_manifest(manifest: Value) -> Self {
        RawProject {
            manifest,
            ..Default::default()
        }
    }

    /// The manifest's `schema_version`.
    pub fn schema_version(&self) -> Result<u32, ModelError> {
        self.manifest
            .get("schema_version")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| ModelError::invalid("cadlab.toml", "missing or invalid `schema_version`"))
    }

    pub(crate) fn set_schema_version(&mut self, v: u32) {
        self.manifest["schema_version"] = Value::from(v);
    }

    /// Single-document form: `{"manifest": ..., "<section>": ..., "library": {"parts": ...,
    /// "footprints": ...}}`.
    pub fn to_packed(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("manifest".into(), self.manifest.clone());
        for (k, v) in &self.sections {
            m.insert(k.clone(), v.clone());
        }
        if !self.parts.is_empty() || !self.footprints.is_empty() {
            let obj = |items: &BTreeMap<String, Value>| Value::Object(items.clone().into_iter().collect());
            m.insert(
                "library".into(),
                serde_json::json!({"parts": obj(&self.parts), "footprints": obj(&self.footprints)}),
            );
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
        let mut raw = RawProject::from_manifest(manifest);
        if let Some(lib) = m.remove("library") {
            let take = |key: &str| -> Result<BTreeMap<String, Value>, ModelError> {
                match lib.get(key) {
                    None => Ok(BTreeMap::new()),
                    Some(Value::Object(o)) => Ok(o.clone().into_iter().collect()),
                    Some(_) => Err(ModelError::invalid(
                        "<packed>",
                        format!("`library.{key}` must be an object"),
                    )),
                }
            };
            raw.parts = take("parts")?;
            raw.footprints = take("footprints")?;
        }
        raw.sections = m.into_iter().collect();
        Ok(raw)
    }
}
