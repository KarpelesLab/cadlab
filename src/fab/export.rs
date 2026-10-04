//! Fab-specific outputs: the generic files of [`crate::fabout`] renamed and selected by the
//! profile, BOM and placement CSVs in the fab's column layouts (rotation offsets applied here,
//! never stored in the project: DECISIONS D12), a deterministic zip archive and `fab-lock.json`.

use std::collections::BTreeMap;
use std::io::Write as _;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::check::{Choices, LinePick};
use super::{Assembly, BomField, BomLayout, CplField, FabProfile, Origin, Process, ProfileSource, sha256_hex};
use crate::bom::BomRow;
use crate::fabout::gerber::mm;
use crate::fabout::{self, FileKind, Options, OutFile, deg, outline_origin, populated};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::BoardSide;
use crate::model::footprint::Mount;
use crate::model::sections::natural_cmp;
use crate::units::{Angle, Nm};

/// Name of the lock file.
pub const LOCK_FILE: &str = "fab-lock.json";

/// Version of the lock file format.
pub const LOCK_VERSION: u32 = 1;

/// A file of the bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFile {
    /// File name.
    pub name: String,
    /// What it is (Gerber `.FileFunction`, `Drill`, `BOM`, `PickPlace`, `Archive`, ...).
    pub function: String,
    /// Content.
    pub content: Vec<u8>,
}

/// A rotation offset applied to a component in the placement file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppliedOffset {
    /// Designator.
    pub designator: String,
    /// Package or footprint the offset matched.
    pub package: String,
    /// Pattern of the profile entry.
    pub pattern: String,
    /// Offset added.
    pub offset: Angle,
}

/// `fab-lock.json`: exactly what an export produced, plus the part substitutions chosen for
/// this fab (`fab.substitute`). A lock written by `fab.substitute` before any export has no
/// process, files or BOM yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FabLock {
    /// Lock format version.
    pub lock_version: u32,
    /// `cadlab <version>`.
    pub generator: String,
    /// Project name.
    pub project: String,
    /// Profile used.
    pub profile: LockProfile,
    /// Process options (none until the first export).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<LockProcess>,
    /// Files written (archive included), in output order.
    #[serde(default)]
    pub files: Vec<LockFile>,
    /// Chosen part per populated BOM line.
    #[serde(default)]
    pub bom: Vec<LockLine>,
    /// Rotation offsets applied in the placement file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rotation_offsets: Vec<AppliedOffset>,
    /// Substitutions applied for this fab, by part ID; kept across exports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substitutions: Vec<Substitution>,
}

/// A part substitution for one fab: the BOM line `part` is ordered as `mpn` at this fab. Stored
/// in `fab-lock.json` only, never in the design (D12, D26).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Substitution {
    /// Part ID of the BOM line.
    pub part: String,
    /// MPN it replaces (none for a generic line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    /// Manufacturer of the substitute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Substitute MPN.
    pub mpn: String,
    /// Provider of the offer chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// SKU at that provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sku: Option<String>,
    /// How it was found.
    pub basis: crate::substitute::Basis,
}

impl FabLock {
    /// Reads a lock file; `Ok(None)` when it does not exist.
    pub fn read(path: &std::path::Path) -> Result<Option<FabLock>, String> {
        match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).map(Some).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The lock as written: pretty JSON with a final newline.
    pub fn to_text(&self) -> std::io::Result<String> {
        let mut text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        text.push('\n');
        Ok(text)
    }
}

/// Profile identity in the lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LockProfile {
    /// Profile ID.
    pub id: String,
    /// Name.
    pub name: String,
    /// Verification date of its values.
    pub verified_at: String,
    /// Built-in, user or merged.
    pub source: ProfileSource,
}

/// Process options in the lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LockProcess {
    /// Process ID.
    pub id: String,
    /// Copper layers.
    pub layers: u8,
    /// Board thickness.
    pub thickness: Nm,
    /// Outer copper.
    pub outer_copper: Nm,
    /// Inner copper (multilayer boards).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_copper: Option<Nm>,
    /// Finish and colors.
    #[serde(flatten)]
    pub choices: Choices,
}

/// A file in the lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LockFile {
    /// File name.
    pub name: String,
    /// SHA-256, lowercase hex.
    pub sha256: String,
    /// Size in bytes.
    pub bytes: u64,
}

/// A BOM line in the lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LockLine {
    /// Designators.
    pub designators: Vec<String>,
    /// Quantity per board.
    pub quantity: usize,
    /// Choice.
    #[serde(flatten)]
    pub pick: LinePick,
}

/// Profile key of a generic file kind, plus `{n}`/`{layer}`/`{from}`/`{to}` values.
fn kind_key(kind: &FileKind, layers: usize) -> Option<(&'static str, [usize; 4])> {
    let side = |s: &BoardSide, top: &'static str, bottom: &'static str| match s {
        BoardSide::Top => top,
        BoardSide::Bottom => bottom,
    };
    Some(match kind {
        FileKind::Copper(name) => {
            if name == "F.Cu" {
                ("copper_top", [0, 1, 0, 0])
            } else if name == "B.Cu" {
                ("copper_bottom", [0, layers, 0, 0])
            } else {
                let n: usize = name.strip_prefix("In")?.strip_suffix(".Cu")?.parse().ok()?;
                ("copper_inner", [n, n + 1, 0, 0])
            }
        }
        FileKind::Mask(s) => (side(s, "mask_top", "mask_bottom"), [0; 4]),
        FileKind::Paste(s) => (side(s, "paste_top", "paste_bottom"), [0; 4]),
        FileKind::Legend(s) => (side(s, "silk_top", "silk_bottom"), [0; 4]),
        FileKind::Component(s) => (side(s, "component_top", "component_bottom"), [0; 4]),
        FileKind::Profile => ("profile", [0; 4]),
        FileKind::Drill { plated, from, to, through } => {
            let k = match (plated, through) {
                (true, true) => "drill_pth",
                (false, true) => "drill_npth",
                _ => "drill_span",
            };
            (k, [0, 0, *from, *to])
        }
        FileKind::Ipc356 => ("ipc356", [0; 4]),
        FileKind::DrillGerber { .. } | FileKind::PickPlace | FileKind::Ipc2581 => return None,
    })
}

fn group_of(key: &str) -> &str {
    key.split('_').next().unwrap_or(key)
}

/// Fills a name template.
fn expand(t: &str, project: &str, fab: &str, v: [usize; 4]) -> String {
    t.replace("{project}", project)
        .replace("{fab}", fab)
        .replace("{n}", &v[0].to_string())
        .replace("{layer}", &v[1].to_string())
        .replace("{from}", &v[2].to_string())
        .replace("{to}", &v[3].to_string())
}

/// Every generic file kind the project could produce, to map generic names back to kinds.
fn kinds(p: &Project) -> Vec<FileKind> {
    let names = p.board().stackup.copper_names();
    let n = names.len();
    let mut v: Vec<FileKind> = names.into_iter().map(FileKind::Copper).collect();
    for s in [BoardSide::Top, BoardSide::Bottom] {
        v.extend([FileKind::Mask(s), FileKind::Paste(s), FileKind::Legend(s), FileKind::Component(s)]);
    }
    v.push(FileKind::Profile);
    for plated in [true, false] {
        for from in 1..=n {
            for to in from..=n {
                v.push(FileKind::Drill { plated, from, to, through: from == 1 && to == n });
            }
        }
    }
    v.push(FileKind::Ipc356);
    v
}

/// Project name as used in file names.
fn base_name(p: &Project) -> String {
    let s: String = p
        .manifest()
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' })
        .collect();
    if s.is_empty() { "board".into() } else { s }
}

/// The fabrication files (Gerber, drill, optionally IPC-D-356A) the profile asks for, named by
/// its table.
pub fn fab_files(p: &Project, profile: &FabProfile, o: &Options) -> Vec<OutFile> {
    let project = base_name(p);
    let layers = p.board().stackup.copper_names().len();
    let by_name: BTreeMap<String, FileKind> =
        kinds(p).into_iter().map(|k| (fabout::file_name(&p.manifest().name, &k), k)).collect();
    let mut files = fabout::gerbers(p, o);
    files.extend(fabout::excellon::drills(p, o));
    files.push(fabout::ipc356::netlist(p, o));
    let inc = &profile.output.include;
    files
        .into_iter()
        .filter_map(|mut f| {
            let (key, vals) = kind_key(by_name.get(&f.name)?, layers)?;
            if !inc.iter().any(|i| i == key || i == group_of(key)) {
                return None;
            }
            if let Some(t) = profile.output.names.get(key) {
                f.name = expand(t, &project, &profile.id, vals);
            }
            Some(f)
        })
        .collect()
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

fn csv_line(out: &mut String, fields: impl IntoIterator<Item = String>) {
    let v: Vec<String> = fields.into_iter().map(|f| csv_field(&f)).collect();
    out.push_str(&v.join(","));
    out.push_str("\r\n");
}

/// The BOM in the layout (RFC 4180, CRLF): populated lines only. `picks` (by part ID) supply the
/// chosen MPN and SKU; lines without a pick use the BOM's own MPN.
pub fn bom_csv(layout: &BomLayout, rows: &[BomRow], picks: &BTreeMap<String, LinePick>) -> String {
    let mut s = String::new();
    csv_line(&mut s, layout.columns.iter().map(|c| c.header.clone()));
    for (i, r) in rows.iter().filter(|r| r.quantity > 0).enumerate() {
        let pick = picks.get(&r.part);
        let own = r.order_mpn();
        let o = |v: &Option<String>| v.clone().unwrap_or_default();
        csv_line(
            &mut s,
            layout.columns.iter().map(|c| match c.field {
                BomField::Line => (i + 1).to_string(),
                BomField::Quantity => r.quantity.to_string(),
                BomField::Designators => r.refdes.join(","),
                BomField::Value => r.value.clone(),
                BomField::Description => r.description.clone(),
                BomField::ValueDescription => format!("{} {}", r.value, r.description).trim().to_string(),
                BomField::Package => o(&r.package),
                BomField::Footprint => o(&r.footprint),
                BomField::Manufacturer => match pick {
                    Some(p) => o(&p.manufacturer),
                    None => own.and_then(|(m, _)| m).unwrap_or_default().to_string(),
                },
                BomField::Mpn => match pick {
                    Some(p) => o(&p.mpn),
                    None => own.map(|(_, m)| m).unwrap_or_default().to_string(),
                },
                BomField::Sku => pick.and_then(|p| p.sku.clone()).unwrap_or_default(),
                BomField::Mount => match r.mount {
                    Some(Mount::Smd) => layout.mount_names[0].clone(),
                    Some(Mount::Tht) => layout.mount_names[1].clone(),
                    None => String::new(),
                },
                BomField::Notes => o(&r.notes),
                BomField::Empty => String::new(),
            }),
        );
    }
    s
}

/// The placement file in the profile's layout, with rotation offsets applied, and the offsets
/// used. Placed, populated components only, naturally sorted by designator.
pub fn cpl_csv(p: &Project, a: &Assembly) -> (String, Vec<AppliedOffset>) {
    let l = &a.cpl;
    let origin = match l.origin {
        Origin::Board => Point::ORIGIN,
        Origin::OutlineLowerLeft => outline_origin(p).unwrap_or(Point::ORIGIN),
    };
    let info = populated(p);
    let board = p.board();
    let mut refs: Vec<&String> = board.footprints.keys().filter(|r| info.contains_key(*r)).collect();
    refs.sort_by(|a, b| natural_cmp(a, b));
    let mut s = String::new();
    let mut applied = Vec::new();
    csv_line(&mut s, l.columns.iter().map(|c| c.header.clone()));
    for r in refs {
        let pf = &board.footprints[r];
        let ci = &info[r];
        let mut rot = pf.rotation;
        if let Some((off, name)) = a.rotation_offsets.iter().find_map(|o| {
            [&ci.package, &ci.footprint].into_iter().find(|n| !n.is_empty() && o.matches(n)).map(|n| (o, n))
        }) {
            rot = rot + off.offset;
            applied.push(AppliedOffset {
                designator: r.clone(),
                package: name.clone(),
                pattern: off.package.clone(),
                offset: off.offset,
            });
        }
        let coord = |v: i64| format!("{}{}", mm(v), l.coordinate_suffix);
        csv_line(
            &mut s,
            l.columns.iter().map(|c| match c.field {
                CplField::Designator => r.clone(),
                CplField::Value => ci.value.clone(),
                CplField::Package => ci.package.clone(),
                CplField::Footprint => ci.footprint.clone(),
                CplField::X => coord(pf.at.x.0 - origin.x.0),
                CplField::Y => coord(pf.at.y.0 - origin.y.0),
                CplField::Side => match pf.side {
                    BoardSide::Top => l.side_names[0].clone(),
                    BoardSide::Bottom => l.side_names[1].clone(),
                },
                CplField::Rotation => deg(rot.normalized()),
                CplField::Empty => String::new(),
            }),
        );
    }
    (s, applied)
}

/// A zip archive of `files` in order, deflated, with fixed timestamps (1980-01-01) and
/// permissions, so the same files always give the same bytes.
pub fn zip(files: &[BundleFile]) -> std::io::Result<Vec<u8>> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    for f in files {
        w.start_file(f.name.as_str(), opts).map_err(std::io::Error::other)?;
        w.write_all(&f.content)?;
    }
    Ok(w.finish().map_err(std::io::Error::other)?.into_inner())
}

/// What [`bundle`] needs besides the project.
pub struct BundleInput<'a> {
    /// Profile.
    pub profile: &'a FabProfile,
    /// Where it came from.
    pub source: ProfileSource,
    /// Process.
    pub process: &'a Process,
    /// Board options chosen by the check.
    pub choices: Choices,
    /// Part picks (from [`super::check::parts`]).
    pub picks: Vec<LinePick>,
    /// Output options.
    pub options: Options,
    /// Substitutions to keep in the lock (from the previous lock).
    pub substitutions: Vec<Substitution>,
}

/// The complete bundle: fabrication files, BOM and placement files (when the profile has
/// assembly), the zip archive of the fabrication files, and the lock (whose own file is the
/// last entry of the returned list).
pub fn bundle(p: &Project, input: &BundleInput<'_>) -> std::io::Result<(Vec<BundleFile>, FabLock)> {
    let profile = input.profile;
    let project = base_name(p);
    let mut fab: Vec<BundleFile> = fab_files(p, profile, &input.options)
        .into_iter()
        .map(|f| BundleFile { name: f.name, function: f.function, content: f.content.into_bytes() })
        .collect();
    let archive_name = expand(&profile.output.archive, &project, &profile.id, [0; 4]);
    let archive = BundleFile { name: archive_name, function: "Archive".into(), content: zip(&fab)? };
    let rows = crate::bom::rows(p);
    let mut rest = Vec::new();
    let mut offsets = Vec::new();
    let picks: BTreeMap<String, LinePick> = input.picks.iter().map(|k| (k.part.clone(), k.clone())).collect();
    if let Some(a) = &profile.assembly {
        let bom = bom_csv(&a.bom, &rows, &picks);
        rest.push(BundleFile {
            name: expand(&a.bom.file, &project, &profile.id, [0; 4]),
            function: "BOM".into(),
            content: bom.into_bytes(),
        });
        let (cpl, applied) = cpl_csv(p, a);
        offsets = applied;
        rest.push(BundleFile {
            name: expand(&a.cpl.file, &project, &profile.id, [0; 4]),
            function: "PickPlace".into(),
            content: cpl.into_bytes(),
        });
    }
    fab.extend(rest);
    fab.push(archive);
    let s = &p.board().stackup;
    let lock = FabLock {
        lock_version: LOCK_VERSION,
        generator: format!("cadlab {}", input.options.version),
        project: p.manifest().name.clone(),
        profile: LockProfile {
            id: profile.id.clone(),
            name: profile.name.clone(),
            verified_at: profile.verified_at.clone(),
            source: input.source,
        },
        process: Some(LockProcess {
            id: input.process.id.clone(),
            layers: s.copper_layers,
            thickness: s.thickness,
            outer_copper: s.outer_copper,
            inner_copper: (s.copper_layers > 2).then_some(s.inner_copper),
            choices: input.choices.clone(),
        }),
        files: fab
            .iter()
            .map(|f| LockFile { name: f.name.clone(), sha256: sha256_hex(&f.content), bytes: f.content.len() as u64 })
            .collect(),
        bom: rows
            .iter()
            .filter(|r| r.quantity > 0)
            .map(|r| LockLine {
                designators: r.refdes.clone(),
                quantity: r.quantity,
                pick: picks.get(&r.part).cloned().unwrap_or_else(|| {
                    let own = r.order_mpn();
                    LinePick {
                        part: r.part.clone(),
                        manufacturer: own.and_then(|(m, _)| m.map(String::from)),
                        mpn: own.map(|(_, m)| m.to_string()),
                        sku: None,
                        provider: None,
                        status: None,
                        replaces: None,
                    }
                }),
            })
            .collect(),
        rotation_offsets: offsets,
        substitutions: input.substitutions.clone(),
    };
    let text = lock.to_text()?;
    fab.push(BundleFile { name: LOCK_FILE.into(), function: "FabLock".into(), content: text.into_bytes() });
    Ok((fab, lock))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        assert_eq!(expand("{project}.G{layer}L", "demo", "jlcpcb", [1, 2, 0, 0]), "demo.G2L");
        assert_eq!(expand("{project}-{fab}-L{from}-L{to}", "d", "x", [0, 0, 1, 2]), "d-x-L1-L2");
        assert_eq!(kind_key(&FileKind::Copper("In2.Cu".into()), 4), Some(("copper_inner", [2, 3, 0, 0])));
        assert_eq!(kind_key(&FileKind::Copper("B.Cu".into()), 4), Some(("copper_bottom", [0, 4, 0, 0])));
        assert_eq!(group_of("drill_npth"), "drill");
    }
}
