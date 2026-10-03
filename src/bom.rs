//! Bill of materials: grouping components into lines, and CSV export.
//!
//! The BOM is computed from the circuit's components plus the sourcing overlay (`bom.json`).
//!
//! Fab-specific column layouts (JLCPCB, PCBWay) follow those fabs' published BOM templates as of
//! writing. They move into fab profiles in M4, where each layout records its source and
//! verification date (`docs/MANUFACTURING.md`); until then, check them against the fab's current
//! template before ordering.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::Project;
use crate::model::footprint::Mount;
use crate::model::part::{Category, ParamValue};
use crate::model::sections::{ApprovedPart, natural_cmp};

/// One BOM line: all components using the same part.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BomRow {
    /// Part ID.
    pub part: String,
    /// Category.
    pub category: Category,
    /// Populated components.
    pub refdes: Vec<String>,
    /// Components marked do-not-populate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dnp: Vec<String>,
    /// Quantity to assemble (populated components).
    pub quantity: usize,
    /// Value.
    pub value: String,
    /// Description.
    pub description: String,
    /// Footprint name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
    /// Package name (`package` parameter, or the footprint name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// SMD or THT.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount: Option<Mount>,
    /// Manufacturer (concrete parts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// MPN (concrete parts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    /// Approved alternates (concrete parts) or candidates (generic parts).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved: Vec<ApprovedPart>,
    /// Notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl BomRow {
    /// The MPN to order: the part's own, else the first approved candidate.
    pub fn order_mpn(&self) -> Option<(Option<&str>, &str)> {
        if let Some(m) = &self.mpn {
            return Some((self.manufacturer.as_deref(), m));
        }
        self.approved
            .first()
            .map(|a| (a.manufacturer.as_deref(), a.mpn.as_str()))
    }
}

/// Computes BOM lines, ordered by first reference designator.
pub fn rows(p: &Project) -> Vec<BomRow> {
    let lib = p.library();
    let bom = p.bom();
    let mut out: Vec<BomRow> = Vec::new();
    let mut ids: Vec<&str> = p.circuit().components.values().map(|c| c.part.as_str()).collect();
    ids.sort();
    ids.dedup();
    for id in ids {
        let mut refdes: Vec<String> = p.circuit().using_part(id).map(|(r, _)| r.clone()).collect();
        refdes.sort_by(|a, b| natural_cmp(a, b));
        let (dnp, refdes): (Vec<String>, Vec<String>) = refdes.into_iter().partition(|r| bom.dnp.contains(r));
        let part = lib.parts.get(id);
        let line = bom.lines.get(id);
        let footprint = part.and_then(|pt| pt.footprint()).map(|f| f.footprint.clone());
        let mount = footprint.as_ref().and_then(|f| lib.footprints.get(f)).map(|f| f.mount);
        let package = part
            .and_then(|pt| pt.params.get("package"))
            .and_then(ParamValue::text)
            .map(String::from)
            .or_else(|| footprint.clone());
        out.push(BomRow {
            part: id.to_string(),
            category: part.map_or(Category::Other, |pt| pt.category),
            quantity: refdes.len(),
            refdes,
            dnp,
            value: part.map(|pt| pt.value()).unwrap_or_else(|| id.to_string()),
            description: part.map(|pt| pt.description.clone()).unwrap_or_default(),
            footprint,
            package,
            mount,
            manufacturer: part.and_then(|pt| pt.manufacturer.clone()),
            mpn: part.and_then(|pt| pt.mpn.clone()),
            approved: line.map(|l| l.approved.clone()).unwrap_or_default(),
            notes: line.and_then(|l| l.notes.clone()),
        });
    }
    let first = |r: &BomRow| r.refdes.first().or(r.dnp.first()).cloned().unwrap_or_default();
    out.sort_by(|a, b| natural_cmp(&first(a), &first(b)));
    out
}

/// CSV layouts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CsvFormat {
    /// Every field, DNP lines included and flagged.
    #[default]
    Generic,
    /// JLCPCB assembly: Comment, Designator, Footprint, JLCPCB Part #.
    Jlcpcb,
    /// PCBWay assembly: Item #, Designator, Qty, Manufacturer, Mfg Part #, Description / Value,
    /// Package/Footprint, Type, Your Instructions / Notes.
    Pcbway,
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn csv_line(fields: &[String]) -> String {
    let mut l = fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(",");
    l.push_str("\r\n");
    l
}

/// Writes rows as CSV (RFC 4180, CRLF line endings). Fab layouts omit DNP components.
pub fn to_csv(rows: &[BomRow], format: CsvFormat) -> String {
    let mut out = String::new();
    let s = |v: &str| v.to_string();
    let o = |v: &Option<String>| v.clone().unwrap_or_default();
    match format {
        CsvFormat::Generic => {
            out += &csv_line(
                &[
                    "Line",
                    "Quantity",
                    "Designators",
                    "Value",
                    "Description",
                    "Package",
                    "Footprint",
                    "Manufacturer",
                    "MPN",
                    "Approved alternates",
                    "DNP",
                    "Notes",
                    "Part ID",
                ]
                .map(s),
            );
            for (i, r) in rows.iter().enumerate() {
                let approved: Vec<String> = r
                    .approved
                    .iter()
                    .map(|a| match &a.manufacturer {
                        Some(m) => format!("{m} {}", a.mpn),
                        None => a.mpn.clone(),
                    })
                    .collect();
                out += &csv_line(&[
                    (i + 1).to_string(),
                    r.quantity.to_string(),
                    r.refdes.join(","),
                    r.value.clone(),
                    r.description.clone(),
                    o(&r.package),
                    o(&r.footprint),
                    o(&r.manufacturer),
                    o(&r.mpn),
                    approved.join("; "),
                    r.dnp.join(","),
                    o(&r.notes),
                    r.part.clone(),
                ]);
            }
        }
        CsvFormat::Jlcpcb => {
            out += &csv_line(&["Comment", "Designator", "Footprint", "JLCPCB Part #"].map(s));
            for r in rows.iter().filter(|r| r.quantity > 0) {
                out += &csv_line(&[r.value.clone(), r.refdes.join(","), o(&r.package), String::new()]);
            }
        }
        CsvFormat::Pcbway => {
            out += &csv_line(
                &[
                    "Item #",
                    "Designator",
                    "Qty",
                    "Manufacturer",
                    "Mfg Part #",
                    "Description / Value",
                    "Package/Footprint",
                    "Type",
                    "Your Instructions / Notes",
                ]
                .map(s),
            );
            for (i, r) in rows.iter().filter(|r| r.quantity > 0).enumerate() {
                let (mfr, mpn) = r.order_mpn().map_or((String::new(), String::new()), |(m, p)| {
                    (m.unwrap_or("").into(), p.into())
                });
                let kind = match r.mount {
                    Some(Mount::Tht) => "THT",
                    Some(Mount::Smd) => "SMD",
                    None => "",
                };
                out += &csv_line(&[
                    (i + 1).to_string(),
                    r.refdes.join(","),
                    r.quantity.to_string(),
                    mfr,
                    mpn,
                    format!("{} {}", r.value, r.description).trim().to_string(),
                    o(&r.package),
                    kind.into(),
                    o(&r.notes),
                ]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quoting() {
        assert_eq!(csv_field("R1,R2"), "\"R1,R2\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_line(&["a".into(), "b,c".into()]), "a,\"b,c\"\r\n");
    }
}
