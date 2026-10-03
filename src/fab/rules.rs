//! Design rules derived from a fab profile's capabilities (`board.rules` with `fab`).
//!
//! Only the resulting numbers go into the project (DECISIONS D12, D26): the profile is read once,
//! nothing refers back to it. Limits the profile does not give keep their current value.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Process;
use crate::model::board::Rules;
use crate::units::Nm;

/// How close to the fab's limits the derived rules are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Margin {
    /// Every minimum at the fab's published limit; default track and via at those minimums.
    Tightest,
    /// Minimums 25 % above the fab's limits (rounded up to 10 µm); default track width and via
    /// never below cadlab's class 2 defaults (0.25 mm, 0.3/0.6 mm).
    #[default]
    Comfortable,
}

/// One rule value taken from the profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DerivedRule {
    /// Rule field (`min_track_width`).
    pub field: String,
    /// Value set.
    pub value: Nm,
    /// Profile field it comes from (`min_track`).
    pub from: String,
    /// The fab's own limit.
    pub limit: Nm,
    /// The profile marks that value unverified.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unverified: bool,
}

/// Rule field ← process field, for the manufacturing minimums.
const MAP: &[(&str, &str)] = &[
    ("min_track_width", "min_track"),
    ("clearance", "min_space"),
    ("min_drill", "min_drill"),
    ("min_annular_ring", "min_via_ring"),
    ("hole_to_hole", "hole_to_hole"),
    ("copper_to_edge", "copper_to_edge"),
    ("silk_to_pad", "silk_to_pad"),
    ("min_silk_width", "min_silk_width"),
];

fn process_value(q: &Process, field: &str) -> Option<Nm> {
    match field {
        "min_track" => q.min_track,
        "min_space" => q.min_space,
        "min_drill" => q.min_drill,
        "min_via_ring" => q.min_via_ring,
        "hole_to_hole" => q.hole_to_hole,
        "copper_to_edge" => q.copper_to_edge,
        "silk_to_pad" => q.silk_to_pad,
        "min_silk_width" => q.min_silk_width,
        _ => None,
    }
}

/// `v` × 1.25, rounded up to 10 µm (`v` ≥ 0).
fn comfortable(v: Nm) -> Nm {
    let x = (v.0.max(0) * 5 + 3) / 4;
    Nm((x + 9_999) / 10_000 * 10_000)
}

/// Rules derived from a fab `process` on top of `base`, and the values taken. Fields the
/// profile leaves out keep `base`'s value (and are not listed). `ipc_class` is kept.
pub fn derive(process: &Process, margin: Margin, base: &Rules) -> (Rules, Vec<DerivedRule>) {
    let mut out = base.clone();
    let mut derived = Vec::new();
    let unverified = |f: &str| process.unverified.iter().any(|u| u == f);
    let mut set = |out: &mut Rules, field: &str, value: Nm, from: &str, limit: Nm| {
        *out.length_mut(field).expect("rule field") = value;
        derived.push(DerivedRule {
            field: field.into(),
            value,
            from: from.into(),
            limit,
            unverified: unverified(from),
        });
    };
    for (rule, field) in MAP {
        let Some(limit) = process_value(process, field) else { continue };
        let value = match margin {
            Margin::Tightest => limit,
            Margin::Comfortable => comfortable(limit),
        };
        set(&mut out, rule, value, field, limit);
    }
    // Defaults used when routing: from the minimums just derived.
    let d = Rules::default();
    let floor = |v: Nm, f: Nm| match margin {
        Margin::Tightest => v,
        Margin::Comfortable => v.max(f),
    };
    if let Some(l) = process.min_track {
        let (t, z) = (floor(out.min_track_width, d.track_width), floor(out.min_track_width, d.zone_min_width));
        set(&mut out, "track_width", t, "min_track", l);
        set(&mut out, "zone_min_width", z, "min_track", l);
    }
    if let Some(l) = process.min_drill {
        let v = floor(out.min_drill, d.via_drill);
        set(&mut out, "via_drill", v, "min_drill", l);
    }
    if let Some(l) = process.min_via_ring {
        let dia = floor(Nm(out.via_drill.0 + 2 * out.min_annular_ring.0), d.via_diameter);
        set(&mut out, "via_diameter", dia, "min_via_ring", l);
    } else if out.via_diameter <= out.via_drill {
        out.via_diameter = Nm(out.via_drill.0 + 2 * out.min_annular_ring.0);
    }
    derived.sort_by_key(|r| crate::model::board::RULE_FIELDS.iter().position(|f| *f == r.field));
    (out, derived)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fab::Profiles;

    #[test]
    fn rounding() {
        assert_eq!(comfortable(Nm::from_um(100)), Nm::from_um(130));
        assert_eq!(comfortable(Nm::from_um(150)), Nm::from_um(190));
        assert_eq!(comfortable(Nm::from_um(200)), Nm::from_um(250));
        assert_eq!(comfortable(Nm::from_um(50)), Nm::from_um(70));
    }

    #[test]
    fn jlcpcb_two_layer() {
        let ps = Profiles::builtin();
        let p = ps.get("jlcpcb").unwrap();
        let q = p.process("two-layer").unwrap();
        let (t, _) = derive(q, Margin::Tightest, &Rules::default());
        assert_eq!(
            (t.min_track_width, t.clearance, t.track_width),
            (Nm::from_um(100), Nm::from_um(100), Nm::from_um(100))
        );
        assert_eq!(
            (t.via_drill, t.via_diameter, t.min_annular_ring),
            (Nm::from_um(150), Nm::from_um(250), Nm::from_um(50))
        );
        let (c, d) = derive(q, Margin::Comfortable, &Rules::default());
        assert_eq!(
            (c.min_track_width, c.clearance, c.track_width),
            (Nm::from_um(130), Nm::from_um(130), Nm::from_um(250))
        );
        assert_eq!(
            (c.via_drill, c.via_diameter, c.min_annular_ring),
            (Nm::from_um(300), Nm::from_um(600), Nm::from_um(70))
        );
        assert_eq!(c.ipc_class, 2);
        let fields: Vec<&str> = d.iter().map(|r| r.field.as_str()).collect();
        assert_eq!(fields[..3], ["clearance", "track_width", "min_track_width"]);
    }
}
