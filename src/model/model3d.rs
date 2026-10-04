//! 3D models attached to footprints (and to a part's use of a footprint): the reference stored
//! in the design, and the model file bytes kept in the project library (`library/models/`).
//!
//! The stored reference is exact: offsets in [`Nm`], rotations in millidegree [`Angle`]s, scale
//! factors in ppm ([`Scale`]). Decoding the file and turning it into triangles is done by
//! [`crate::models3d`] through the `oxideav-mesh3d` scene model (DECISIONS D36).

use std::fmt;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::units::{Angle, Nm, Scale};

/// Largest model file accepted (bytes).
pub const MAX_MODEL_BYTES: usize = 64 << 20;

/// Length unit of a model file's coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ModelUnit {
    /// Millimeters.
    #[serde(rename = "mm")]
    Mm,
    /// Centimeters.
    #[serde(rename = "cm")]
    Cm,
    /// Meters.
    #[serde(rename = "m")]
    M,
    /// Inches.
    #[serde(rename = "in")]
    In,
    /// Thousandths of an inch.
    #[serde(rename = "mil")]
    Mil,
    /// Feet.
    #[serde(rename = "ft")]
    Ft,
}

impl ModelUnit {
    /// One model unit in nanometers (exact).
    pub fn nm(self) -> i64 {
        match self {
            ModelUnit::Mm => 1_000_000,
            ModelUnit::Cm => 10_000_000,
            ModelUnit::M => 1_000_000_000,
            ModelUnit::In => 25_400_000,
            ModelUnit::Mil => 25_400,
            ModelUnit::Ft => 304_800_000,
        }
    }

    /// Label as written in files.
    pub fn label(self) -> &'static str {
        match self {
            ModelUnit::Mm => "mm",
            ModelUnit::Cm => "cm",
            ModelUnit::M => "m",
            ModelUnit::In => "in",
            ModelUnit::Mil => "mil",
            ModelUnit::Ft => "ft",
        }
    }
}

/// Which model axis points up, away from the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ModelUp {
    /// +Y up (glTF, OBJ convention): the model's +Z (its front) faces the board's front edge (-Y).
    Y,
    /// +Z up (STL, STEP, CAD convention): model axes are board axes.
    Z,
}

fn is_zero3(v: &[Nm; 3]) -> bool {
    v.iter().all(|x| x.0 == 0)
}

fn is_zero_angles(v: &[Angle; 3]) -> bool {
    v.iter().all(|a| a.0 == 0)
}

fn is_one3(v: &[Scale; 3]) -> bool {
    v.iter().all(Scale::is_one)
}

fn one3() -> [Scale; 3] {
    [Scale::ONE; 3]
}

/// A 3D model placed on a footprint.
///
/// Model coordinates are converted to millimeters (`unit`), turned so that `up` is +Z, scaled
/// per axis, rotated about X, then Y, then Z (counter-clockwise looking down each axis), then
/// moved by `offset`. The result is in footprint coordinates: X, Y as the footprint (IPC zero
/// orientation, origin at the footprint origin), Z up from the board surface. Placement on the
/// board then works as for pads: bottom-side parts are mirrored under the board.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Model3d {
    /// Model file in the project library (`library/models/<file>`), with its format extension:
    /// `SOT-23-5.stl`, `USB-C.glb`.
    pub file: String,
    /// Offset of the model origin in footprint coordinates (X, Y, Z up from the board).
    #[serde(default, skip_serializing_if = "is_zero3")]
    pub offset: [Nm; 3],
    /// Rotation about X, Y and Z, applied in that order.
    #[serde(default, skip_serializing_if = "is_zero_angles")]
    pub rotation: [Angle; 3],
    /// Scale factor per axis (after the unit conversion).
    #[serde(default = "one3", skip_serializing_if = "is_one3")]
    pub scale: [Scale; 3],
    /// Unit of the model's coordinates. Default: what the file declares (STL: mm; glTF, OBJ: m;
    /// USDZ: its metersPerUnit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<ModelUnit>,
    /// Model axis pointing up. Default: what the file declares (STL: Z; glTF, OBJ: Y).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up: Option<ModelUp>,
}

impl Model3d {
    /// A reference to `file` with no transform.
    pub fn new(file: impl Into<String>) -> Self {
        Model3d {
            file: file.into(),
            offset: [Nm::ZERO; 3],
            rotation: [Angle::ZERO; 3],
            scale: one3(),
            unit: None,
            up: None,
        }
    }
}

/// The bytes of a model file. Cheap to clone. Serialized (packed projects, undo snapshots,
/// block files) as base64.
#[derive(Clone, PartialEq, Eq)]
pub struct ModelData(pub Arc<[u8]>);

impl ModelData {
    /// Wraps bytes.
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Self {
        ModelData(bytes.into())
    }

    /// The bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no bytes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ModelData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ModelData({} bytes)", self.0.len())
    }
}

impl Serialize for ModelData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64_encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for ModelData {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = std::borrow::Cow::<str>::deserialize(d)?;
        base64_decode(&s).map(ModelData::new).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ModelData {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ModelData".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "File content, base64 (RFC 4648, with padding)."
        })
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (RFC 4648 §4).
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Inverse of [`base64_encode`]; whitespace is ignored.
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    let mut pad = 0;
    for b in s.bytes().filter(|b| !b.is_ascii_whitespace()) {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            _ => return Err(format!("invalid base64 character `{}`", b as char)),
        };
        if pad > 0 {
            return Err("base64 data after padding".into());
        }
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if pad > 2 {
        return Err("invalid base64 padding".into());
    }
    Ok(out)
}

/// Whether `name` is a valid model file name: a library item name (letters, digits,
/// `. _ + -`) with an extension.
pub fn valid_model_name(name: &str) -> bool {
    crate::model::part::valid_id(name)
        && name.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        for (raw, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64_encode(raw.as_bytes()), enc);
            assert_eq!(base64_decode(enc).unwrap(), raw.as_bytes());
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&all)).unwrap(), all);
        assert!(base64_decode("Zm9v!").is_err());
    }

    #[test]
    fn reference_serde() {
        let m = Model3d::new("a.stl");
        assert_eq!(serde_json::to_value(&m).unwrap(), serde_json::json!({"file": "a.stl"}));
        let m: Model3d = serde_json::from_value(serde_json::json!({
            "file": "a.glb", "offset": ["0mm", "0mm", "1mm"], "rotation": [90, 0, "45.5"],
            "scale": ["2.54", 1, "1"], "unit": "in", "up": "z"
        }))
        .unwrap();
        assert_eq!(m.offset[2], Nm(1_000_000));
        assert_eq!(m.rotation[2], Angle(45_500));
        assert_eq!(m.scale[0], Scale(2_540_000));
        assert_eq!(m.unit, Some(ModelUnit::In));
        assert!(valid_model_name("SOT-23.stl") && !valid_model_name("noext") && !valid_model_name("a/b.stl"));
    }
}
