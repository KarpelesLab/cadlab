//! 3D model files for accurate component bodies (roadmap M9, DECISIONS D36).
//!
//! cadlab has no mesh types or 3D file parsers of its own: files are decoded by the
//! `oxideav-mesh3d` format crates into an [`oxideav_mesh3d::Scene3D`], found through a
//! [`oxideav_mesh3d::Mesh3DRegistry`] by file extension. This module wires the registry
//! ([`registry`]), decodes model files ([`decode`]) and places a scene's triangles in footprint
//! coordinates following a [`Model3d`] reference ([`facets`], [`load`]), for the 3D renderer
//! (`render::board3d`) and the MCAD exporters (`mcad`).
//!
//! Formats available now: STL (`oxideav-stl`), Wavefront OBJ (`oxideav-obj`), glTF 2.0 /
//! GLB (`oxideav-gltf`) and USDZ (`oxideav-usdz`). STEP and VRML decoders are being added to
//! the oxideav-mesh3d family; once published they plug in through the same registry: add the
//! crate to the `models3d` feature in `Cargo.toml` and one `register` call in
//! `build_registry` below. Nothing else in cadlab changes — commands, the renderer and the
//! exporters only see the registry and the decoded `Scene3D`.
//!
//! Without the `models3d` cargo feature, model references and files are still stored, listed
//! and copied, but nothing is decoded: renders and exports fall back to generated bodies.

use crate::model::Project;
use crate::model::model3d::Model3d;

/// Formats expected from upstream oxideav crates that are not available yet: (format,
/// extensions).
pub const PENDING_FORMATS: &[(&str, &[&str])] = &[("step", &["step", "stp"]), ("vrml", &["wrl", "vrml"])];

/// A decodable model format.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FormatInfo {
    /// Format id (`stl`, `gltf`, ...).
    pub format: String,
    /// File extensions, lower case.
    pub extensions: Vec<String>,
}

/// Why a model could not be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ModelError3d {
    /// No decoder for the file's extension.
    #[error("`{file}`: no 3D model decoder for `.{ext}` files")]
    Unsupported {
        /// File name.
        file: String,
        /// Extension (lower case, may be empty).
        ext: String,
    },
    /// cadlab was built without the `models3d` feature.
    #[error("`{file}`: this build cannot read 3D models (cargo feature `models3d` is off)")]
    Disabled {
        /// File name.
        file: String,
    },
    /// The decoder rejected the file.
    #[error("`{file}`: {message}")]
    Invalid {
        /// File name.
        file: String,
        /// Decoder message.
        message: String,
    },
    /// The file holds no triangles.
    #[error("`{file}` contains no triangles")]
    Empty {
        /// File name.
        file: String,
    },
    /// The project library has no such model file.
    #[error("no model file `{file}` in the project library")]
    Missing {
        /// File name.
        file: String,
    },
}

impl ModelError3d {
    /// Stable diagnostic code.
    pub fn code(&self) -> &'static str {
        match self {
            ModelError3d::Unsupported { .. } => "model.unsupported_format",
            ModelError3d::Disabled { .. } => "model.disabled",
            ModelError3d::Invalid { .. } => "model.invalid",
            ModelError3d::Empty { .. } => "model.empty",
            ModelError3d::Missing { .. } => "model.missing",
        }
    }

    /// How to fix it.
    pub fn hint(&self) -> String {
        match self {
            ModelError3d::Unsupported { ext, .. } => {
                let pending = PENDING_FORMATS.iter().any(|(_, exts)| exts.contains(&ext.as_str()));
                let mut h = format!("formats available now: {}", available_text());
                if pending {
                    h.push_str(
                        "; STEP and VRML arrive with the oxideav STEP/VRML decoder crates (not published yet): convert the model to glTF/GLB or STL meanwhile",
                    );
                } else {
                    h.push_str("; STEP (.step/.stp) and VRML (.wrl) will follow with the oxideav STEP/VRML crates");
                }
                h
            }
            ModelError3d::Disabled { .. } => "build cadlab with the `models3d` feature (on by default)".into(),
            ModelError3d::Invalid { .. } => {
                "check the file in a 3D viewer, or re-export it (binary STL or GLB are the most robust)".into()
            }
            ModelError3d::Empty { .. } => "the model must contain a triangle mesh (not only lines or points)".into(),
            ModelError3d::Missing { .. } => {
                "attach the model again with `footprint.model_set` (it copies the file into library/models)".into()
            }
        }
    }

    /// As a warning diagnostic about `refdes` (renders and exports fall back to a generated
    /// body).
    pub fn warning(&self, refdes: &str) -> crate::Diagnostic {
        crate::Diagnostic::warning(self.code(), format!("{refdes}: {self}; using a generated body instead"))
            .with_subject(crate::ObjectRef::Name(refdes.into()))
            .with_hint(self.hint())
    }
}

/// Lower-case extension of a file name (`""` when none).
pub fn extension(file: &str) -> String {
    file.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// The formats this build decodes, by format id (empty without the `models3d` feature).
pub fn formats() -> Vec<FormatInfo> {
    #[cfg(feature = "models3d")]
    {
        let r = registry();
        let mut ids: Vec<&str> = r.decoder_formats().collect();
        ids.sort_unstable();
        ids.into_iter()
            .map(|f| FormatInfo {
                format: f.to_string(),
                extensions: r.decoder_extensions(f).map(<[String]>::to_vec).unwrap_or_default(),
            })
            // Material libraries (`.mtl`) are not models.
            .filter(|f| f.format != "mtl")
            .collect()
    }
    #[cfg(not(feature = "models3d"))]
    {
        Vec::new()
    }
}

/// `.stl, .obj, .gltf, .glb, .usdz` (or "none" without the feature).
fn available_text() -> String {
    let exts: Vec<String> = formats().into_iter().flat_map(|f| f.extensions).map(|e| format!(".{e}")).collect();
    if exts.is_empty() { "none (cargo feature `models3d` is off)".into() } else { exts.join(", ") }
}

/// The format id a file name maps to, if this build can decode it.
pub fn format_of(file: &str) -> Option<String> {
    let ext = extension(file);
    formats().into_iter().find(|f| f.extensions.contains(&ext)).map(|f| f.format)
}

/// Checks that `file` has a decodable extension. Without the feature, any extension of a
/// known or pending format is accepted (the model is stored; nothing is decoded).
pub fn check_format(file: &str) -> Result<(), ModelError3d> {
    let ext = extension(file);
    if cfg!(feature = "models3d") {
        if format_of(file).is_some() {
            return Ok(());
        }
    } else if ["stl", "obj", "gltf", "glb", "usdz"].contains(&ext.as_str()) {
        return Ok(());
    }
    Err(ModelError3d::Unsupported { file: file.to_string(), ext })
}

/// The decoder registry: every oxideav-mesh3d format crate cadlab is built with.
#[cfg(feature = "models3d")]
pub fn registry() -> &'static oxideav_mesh3d::Mesh3DRegistry {
    static R: std::sync::OnceLock<oxideav_mesh3d::Mesh3DRegistry> = std::sync::OnceLock::new();
    R.get_or_init(build_registry)
}

/// The plug-in point for format crates. A new oxideav-mesh3d format crate (STEP, VRML, ...)
/// is added here with its `register` function and to the `models3d` feature in `Cargo.toml`.
#[cfg(feature = "models3d")]
fn build_registry() -> oxideav_mesh3d::Mesh3DRegistry {
    let mut r = oxideav_mesh3d::Mesh3DRegistry::new();
    oxideav_stl::register(&mut r);
    oxideav_obj::register(&mut r);
    oxideav_gltf::register(&mut r);
    oxideav_usdz::register(&mut r);
    r
}

/// Decodes a model file into an oxideav-mesh3d scene, choosing the decoder by extension.
#[cfg(feature = "models3d")]
pub fn decode(file: &str, bytes: &[u8]) -> Result<oxideav_mesh3d::Scene3D, ModelError3d> {
    let ext = extension(file);
    if ext == "mtl" {
        return Err(ModelError3d::Unsupported { file: file.into(), ext });
    }
    let mut dec = registry()
        .decoder_for_extension(&ext)
        .ok_or_else(|| ModelError3d::Unsupported { file: file.into(), ext: ext.clone() })?;
    let scene = dec.decode(bytes).map_err(|e| ModelError3d::Invalid { file: file.into(), message: e.to_string() })?;
    if scene.triangle_count() == 0 {
        return Err(ModelError3d::Empty { file: file.into() });
    }
    Ok(scene)
}

/// A triangle placed in footprint coordinates (millimeters; X, Y as the footprint, Z up from
/// the board surface) with its display color (sRGB, 0..1). Transient output for renderers and
/// exporters; the model itself is the decoded `Scene3D`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Facet {
    /// Vertices, in the file's winding order.
    pub v: [[f64; 3]; 3],
    /// Color.
    pub color: [f32; 3],
}

/// A model's placed triangles with their bounding box.
#[derive(Clone, Debug, PartialEq)]
pub struct Facets {
    /// Triangles.
    pub tris: Vec<Facet>,
    /// Minimum corner (mm).
    pub min: [f64; 3],
    /// Maximum corner (mm).
    pub max: [f64; 3],
}

impl Facets {
    /// Triangles with their bounding box.
    pub fn new(tris: Vec<Facet>) -> Facets {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for v in tris.iter().flat_map(|t| t.v.iter()) {
            for i in 0..3 {
                min[i] = min[i].min(v[i]);
                max[i] = max[i].max(v[i]);
            }
        }
        Facets { tris, min, max }
    }
}

/// Color of triangles without a material.
pub const DEFAULT_COLOR: [f32; 3] = [0.62, 0.62, 0.64];

/// Linear (glTF base color) to sRGB.
#[cfg(feature = "models3d")]
fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// Matrix taking model coordinates (file units, file up axis) to footprint millimeters, from
/// the reference's unit, up axis, scale, rotations and offset. Row-major, `p' = M · p`.
pub fn placement_matrix(m: &Model3d, file_unit_nm: i64, file_up_z: bool) -> [[f64; 4]; 4] {
    let unit_mm = m.unit.map_or(file_unit_nm, |u| u.nm()) as f64 / 1e6;
    let up_z = m.up.map_or(file_up_z, |u| u == crate::model::model3d::ModelUp::Z);
    let mut a = [[0.0; 4]; 4];
    for (i, row) in a.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    // Unit and up axis: Y-up → Z-up turns (x, y, z) into (x, -z, y).
    let s = |i: usize| m.scale[i].to_f64() * unit_mm;
    let base: [[f64; 3]; 3] = if up_z {
        [[s(0), 0.0, 0.0], [0.0, s(1), 0.0], [0.0, 0.0, s(2)]]
    } else {
        [[s(0), 0.0, 0.0], [0.0, 0.0, -s(1)], [0.0, s(2), 0.0]]
    };
    let rot = |axis: usize, deg: f64| -> [[f64; 3]; 3] {
        let (sn, cs) = deg.to_radians().sin_cos();
        let (sn, cs) = if deg % 90.0 == 0.0 { (sn.round(), cs.round()) } else { (sn, cs) };
        match axis {
            0 => [[1.0, 0.0, 0.0], [0.0, cs, -sn], [0.0, sn, cs]],
            1 => [[cs, 0.0, sn], [0.0, 1.0, 0.0], [-sn, 0.0, cs]],
            _ => [[cs, -sn, 0.0], [sn, cs, 0.0], [0.0, 0.0, 1.0]],
        }
    };
    let mul = |x: [[f64; 3]; 3], y: [[f64; 3]; 3]| -> [[f64; 3]; 3] {
        let mut o = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                o[i][j] = (0..3).map(|k| x[i][k] * y[k][j]).sum();
            }
        }
        o
    };
    // Scale (with base), then rotate X, Y, Z: R = Rz · Ry · Rx · S.
    let r = mul(
        rot(2, m.rotation[2].to_deg_f64()),
        mul(rot(1, m.rotation[1].to_deg_f64()), mul(rot(0, m.rotation[0].to_deg_f64()), base)),
    );
    for i in 0..3 {
        a[i][..3].copy_from_slice(&r[i]);
        a[i][3] = m.offset[i].0 as f64 / 1e6;
    }
    a
}

/// The scene's triangles placed in footprint coordinates following `m`. Nodes are visited in
/// order with their world transforms; meshes no node uses (STL files have no nodes) are
/// placed as they are.
#[cfg(feature = "models3d")]
pub fn facets(scene: &oxideav_mesh3d::Scene3D, m: &Model3d) -> Facets {
    use oxideav_mesh3d::{Axis, Mesh, NodeId, Unit};
    let unit_nm = match scene.unit {
        Unit::Metres => 1_000_000_000,
        Unit::Centimetres => 10_000_000,
        Unit::Millimetres => 1_000_000,
        Unit::Inches => 25_400_000,
        Unit::Feet => 304_800_000,
        Unit::Yards => 914_400_000,
    };
    let up_z = matches!(scene.up_axis, Axis::PosZ);
    let a = placement_matrix(m, unit_nm, up_z);
    let place = |p: [f32; 3]| -> [f64; 3] {
        let (x, y, z) = (p[0] as f64, p[1] as f64, p[2] as f64);
        [
            a[0][0] * x + a[0][1] * y + a[0][2] * z + a[0][3],
            a[1][0] * x + a[1][1] * y + a[1][2] * z + a[1][3],
            a[2][0] * x + a[2][1] * y + a[2][2] * z + a[2][3],
        ]
    };
    let material_color = |id: Option<oxideav_mesh3d::MaterialId>| -> [f32; 4] {
        id.and_then(|id| scene.materials.get(id.0 as usize))
            .map(|mat| {
                let c = mat.base_color;
                [to_srgb(c[0]), to_srgb(c[1]), to_srgb(c[2]), 1.0]
            })
            .unwrap_or([DEFAULT_COLOR[0], DEFAULT_COLOR[1], DEFAULT_COLOR[2], 1.0])
    };
    let mut out = Vec::new();
    let mut emit = |mesh: &Mesh| {
        for prim in &mesh.primitives {
            let base = material_color(prim.material);
            let vcol = prim.colors.first().filter(|c| c.len() == prim.positions.len());
            for t in prim.triangle_indices() {
                let Some(vs) =
                    t.iter().map(|&i| prim.positions.get(i as usize).copied()).collect::<Option<Vec<[f32; 3]>>>()
                else {
                    continue;
                };
                if vs.iter().flatten().any(|c| !c.is_finite()) {
                    continue;
                }
                let mut color = [base[0], base[1], base[2]];
                if let Some(vc) = vcol {
                    for (k, ck) in color.iter_mut().enumerate() {
                        let avg: f32 = t.iter().map(|&i| vc[i as usize][k]).sum::<f32>() / 3.0;
                        *ck *= to_srgb(avg);
                    }
                }
                out.push(Facet { v: [place(vs[0]), place(vs[1]), place(vs[2])], color });
            }
        }
    };
    let worlds = scene.world_node_transforms();
    let mut used = vec![false; scene.meshes.len()];
    for (i, node) in scene.nodes.iter().enumerate() {
        let Some(mid) = node.mesh else { continue };
        if let Some(u) = used.get_mut(mid.0 as usize) {
            *u = true;
        }
        if let Some(mesh) = scene.world_mesh_with(NodeId(i as _), &worlds) {
            emit(&mesh);
        }
    }
    for (i, mesh) in scene.meshes.iter().enumerate() {
        if !used[i] {
            emit(mesh);
        }
    }
    Facets::new(out)
}

/// Decodes the model file `m` refers to (from the project library) and places it.
pub fn load(p: &Project, m: &Model3d) -> Result<Facets, ModelError3d> {
    let data = p.library().models.get(&m.file).ok_or_else(|| ModelError3d::Missing { file: m.file.clone() })?;
    #[cfg(feature = "models3d")]
    {
        let scene = decode(&m.file, data.bytes())?;
        let f = facets(&scene, m);
        if f.tris.is_empty() {
            return Err(ModelError3d::Empty { file: m.file.clone() });
        }
        Ok(f)
    }
    #[cfg(not(feature = "models3d"))]
    {
        let _ = data;
        Err(ModelError3d::Disabled { file: m.file.clone() })
    }
}

/// The model of a placed component: its part's model for the footprint in use, else the
/// footprint's own model.
pub fn model_for<'a>(p: &'a Project, refdes: &str) -> Option<&'a Model3d> {
    let fp = crate::board::footprint_for(p, refdes)?;
    let part = p.circuit().components.get(refdes).and_then(|c| p.library().parts.get(&c.part));
    part.and_then(|part| part.footprints.iter().find(|r| r.footprint == fp.name).and_then(|r| r.model.as_ref()))
        .or(fp.model.as_ref())
}

/// Loads placed models for many components, decoding each distinct reference once.
#[derive(Debug, Default)]
pub struct Cache {
    loaded: std::collections::BTreeMap<Model3d, Result<std::sync::Arc<Facets>, ModelError3d>>,
}

impl Cache {
    /// The facets of `m` (decoded on first use).
    pub fn get(&mut self, p: &Project, m: &Model3d) -> Result<std::sync::Arc<Facets>, ModelError3d> {
        self.loaded.entry(m.clone()).or_insert_with(|| load(p, m).map(std::sync::Arc::new)).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::{Angle, Nm, Scale};

    fn apply(a: &[[f64; 4]; 4], p: [f64; 3]) -> [f64; 3] {
        let mut o = [0.0; 3];
        for (i, oi) in o.iter_mut().enumerate() {
            *oi = a[i][0] * p[0] + a[i][1] * p[1] + a[i][2] * p[2] + a[i][3];
        }
        o
    }

    #[test]
    fn placement() {
        // Identity for a Z-up millimeter file.
        let m = Model3d::new("a.stl");
        assert_eq!(apply(&placement_matrix(&m, 1_000_000, true), [1.0, 2.0, 3.0]), [1.0, 2.0, 3.0]);
        // Y-up meters: +Y becomes +Z, +Z (front) becomes -Y, scaled by 1000.
        assert_eq!(apply(&placement_matrix(&m, 1_000_000_000, false), [0.0, 1.0, 0.0]), [0.0, 0.0, 1000.0]);
        assert_eq!(apply(&placement_matrix(&m, 1_000_000_000, false), [0.0, 0.0, 1.0]), [0.0, -1000.0, 0.0]);
        // Overrides, rotation about Z by 90°, scale and offset.
        let mut m = Model3d::new("a.wrl");
        m.unit = Some(crate::model::model3d::ModelUnit::In);
        m.scale = [Scale(100_000); 3];
        m.rotation = [Angle::ZERO, Angle::ZERO, Angle::DEG_90];
        m.offset = [Nm(1_000_000), Nm::ZERO, Nm(500_000)];
        let q = apply(&placement_matrix(&m, 1, false), [1.0, 0.0, 0.0]);
        let want = [1.0, 2.54, 0.5];
        for i in 0..3 {
            assert!((q[i] - want[i]).abs() < 1e-12, "{q:?}");
        }
    }

    #[test]
    fn format_checks() {
        assert_eq!(extension("A.B.GLB"), "glb");
        let e = check_format("part.step").unwrap_err();
        assert_eq!(e.code(), "model.unsupported_format");
        assert!(e.hint().contains("STEP and VRML arrive with the oxideav"), "{}", e.hint());
        assert!(check_format("part.wrl").is_err());
        assert!(check_format("part.stl").is_ok());
        #[cfg(feature = "models3d")]
        {
            assert!(e.hint().contains(".stl") && e.hint().contains(".glb"), "{}", e.hint());
            let f: Vec<String> = formats().into_iter().map(|f| f.format).collect();
            assert!(f.contains(&"stl".to_string()) && f.contains(&"gltf".to_string()), "{f:?}");
            assert!(!f.contains(&"mtl".to_string()));
            assert_eq!(format_of("x.GLB").as_deref(), Some("gltf"));
        }
    }
}
