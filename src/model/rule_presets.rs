//! Data behind the design rule presets (`board.rules --preset`): the IPC reference values with
//! their sources and verification status, and the rule values each preset sets. Read from
//! `rule-presets/ipc.toml`, built into the library. See `docs/BOARD.md`, "Rule presets".

use std::collections::BTreeMap;
use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::board::{RulePreset, Rules};
use crate::units::Nm;

const BUILTIN: &str = include_str!("../../rule-presets/ipc.toml");

/// Whether a reference value was confirmed on its sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// At least two independent sources read on the file's `verified_at` agree.
    Confirmed,
    /// Not confirmed: sources missing or in disagreement.
    Unverified,
}

/// A value taken from a standard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    /// Stable ID (`ipc6012.class3.external_ring`).
    pub id: String,
    /// Standard, section and table the value is attributed to.
    pub standard: String,
    /// What the value is.
    pub what: String,
    /// The value, with its unit (`0.05mm`).
    pub value: String,
    /// Verification status.
    pub status: Status,
    /// Pages read that give the value.
    pub sources: Vec<String>,
    /// Pages read that give another value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dissent: Vec<String>,
}

/// The rule values a preset sets over cadlab's default rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PresetData {
    /// IPC-6012 class.
    pub ipc_class: u8,
    /// Rule length fields that differ from the defaults, by name.
    #[serde(flatten)]
    pub lengths: BTreeMap<String, Nm>,
    /// References each value is built from, by rule field.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub derived: BTreeMap<String, Vec<String>>,
}

/// The rule preset data file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RulePresets {
    /// Date the references were last checked against their sources (`YYYY-MM-DD`).
    pub verified_at: String,
    /// Values taken from standards.
    #[serde(rename = "reference")]
    pub references: Vec<Reference>,
    /// Presets by name (`ipc2`, `ipc3`).
    #[serde(rename = "preset")]
    pub presets: BTreeMap<String, PresetData>,
}

impl RulePresets {
    /// Parses a preset file and checks it: known rule fields, known references.
    pub fn parse(text: &str) -> Result<RulePresets, String> {
        let p: RulePresets = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut probe = Rules::default();
        for (name, preset) in &p.presets {
            for field in preset.lengths.keys().chain(preset.derived.keys()) {
                if probe.length_mut(field).is_none() {
                    return Err(format!("preset {name}: unknown rule field `{field}`"));
                }
            }
            for id in preset.derived.values().flatten() {
                if p.reference(id).is_none() {
                    return Err(format!("preset {name}: unknown reference `{id}`"));
                }
            }
        }
        Ok(p)
    }

    /// The built-in presets.
    pub fn builtin() -> &'static RulePresets {
        static DATA: OnceLock<RulePresets> = OnceLock::new();
        DATA.get_or_init(|| RulePresets::parse(BUILTIN).expect("built-in rule presets are valid"))
    }

    /// A reference by ID.
    pub fn reference(&self, id: &str) -> Option<&Reference> {
        self.references.iter().find(|r| r.id == id)
    }

    fn data(&self, preset: RulePreset) -> Option<&PresetData> {
        self.presets.get(match preset {
            RulePreset::Ipc2 => "ipc2",
            RulePreset::Ipc3 => "ipc3",
        })
    }

    /// The rule set of a preset: the default rules with the preset's values.
    pub fn rules(&self, preset: RulePreset) -> Rules {
        let mut r = Rules::default();
        if let Some(d) = self.data(preset) {
            r.ipc_class = d.ipc_class;
            for (field, v) in &d.lengths {
                *r.length_mut(field).expect("checked when parsed") = *v;
            }
        }
        r
    }

    /// Rule fields of a preset whose value rests on an unverified reference, with those
    /// references, in field order.
    pub fn unverified(&self, preset: RulePreset) -> Vec<(&str, Vec<&Reference>)> {
        let Some(d) = self.data(preset) else { return Vec::new() };
        d.derived
            .iter()
            .filter_map(|(field, ids)| {
                let refs: Vec<&Reference> =
                    ids.iter().filter_map(|id| self.reference(id)).filter(|r| r.status == Status::Unverified).collect();
                (!refs.is_empty()).then_some((field.as_str(), refs))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nm(p: &RulePresets, id: &str) -> Nm {
        Nm::parse(&p.reference(id).unwrap().value).unwrap()
    }

    #[test]
    fn builtin_presets_follow_their_references() {
        let p = RulePresets::builtin();
        assert_eq!(p.verified_at, "2026-11-13");
        // Class 2 is the default rule set.
        assert_eq!(p.rules(RulePreset::Ipc2), Rules::default());
        // Class 3: ring = class 3 external minimum + half the level A allowance; the default
        // via gives exactly that ring.
        let r = p.rules(RulePreset::Ipc3);
        let ring = Nm(nm(p, "ipc6012.class3.external_ring").0 + nm(p, "ipc2221.allowance.level_a").0 / 2);
        assert_eq!(ring, Nm::from_um(250));
        assert_eq!(r.min_annular_ring, ring);
        assert_eq!(r.via_diameter, Nm(r.via_drill.0 + 2 * ring.0));
        assert_eq!(r.ipc_class, 3);
        // The default clearance covers IPC-2221B Table 6-1 up to 15 V in every column used.
        for id in ["b1", "b2", "b4", "a5", "a6"] {
            assert!(nm(p, &format!("ipc2221.spacing.15v.{id}")) <= r.clearance, "{id}");
        }
    }

    #[test]
    fn confirmed_references_have_two_sources() {
        let p = RulePresets::builtin();
        for r in &p.references {
            assert!(r.status != Status::Confirmed || r.sources.len() >= 2, "{}", r.id);
        }
        assert!(p.unverified(RulePreset::Ipc2).is_empty());
        assert!(p.unverified(RulePreset::Ipc3).is_empty());
    }

    #[test]
    fn unverified_references_are_reported_and_bad_files_rejected() {
        let text = BUILTIN.replace("status = \"confirmed\"", "status = \"unverified\"");
        let p = RulePresets::parse(&text).unwrap();
        let u = p.unverified(RulePreset::Ipc3);
        assert_eq!(u.iter().map(|(f, _)| *f).collect::<Vec<_>>(), ["min_annular_ring", "via_diameter"]);
        assert_eq!(u[0].1[0].id, "ipc6012.class3.external_ring");
        assert!(RulePresets::parse(&BUILTIN.replace("via_diameter = \"0.8mm\"", "via_size = \"0.8mm\"")).is_err());
        assert!(RulePresets::parse(&BUILTIN.replace("\"ipc2221.allowance.level_a\"]", "\"nope\"]")).is_err());
    }
}
