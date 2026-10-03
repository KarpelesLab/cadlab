//! Connectivity, the source of truth (`circuit.json`): components, nets, net classes, blocks.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::id::ObjectId;
use crate::model::sections::natural_cmp;
use crate::units::Nm;

/// Whether `s` is a reference designator: uppercase letters (or `_`), then a number (`R1`, `SW3`).
pub fn valid_refdes(s: &str) -> bool {
    let letters = s.trim_end_matches(|c: char| c.is_ascii_digit());
    !letters.is_empty() && letters.len() < s.len() && letters.chars().all(|c| c.is_ascii_uppercase() || c == '_')
}

/// A use of a part in the circuit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// Stable internal ID.
    pub id: ObjectId,
    /// Part ID in the library.
    pub part: String,
    /// Block instance this component belongs to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
    /// Free-form properties (e.g. `"function": "status LED"`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
}

/// A component pin: reference designator and pin *number*. Written `U1.4`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PinRef {
    /// Reference designator.
    pub refdes: String,
    /// Pin number.
    pub pin: String,
}

impl PinRef {
    /// New pin reference.
    pub fn new(refdes: impl Into<String>, pin: impl Into<String>) -> Self {
        PinRef { refdes: refdes.into(), pin: pin.into() }
    }

    /// Parses `U1.4`.
    pub fn parse(s: &str) -> Option<PinRef> {
        let (r, p) = s.trim().split_once('.')?;
        (!r.is_empty() && !p.is_empty()).then(|| PinRef::new(r, p))
    }
}

impl PartialOrd for PinRef {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for PinRef {
    /// Natural order: `R2.1` < `R10.1`, `U1.2` < `U1.10`.
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        natural_cmp(&self.refdes, &o.refdes).then_with(|| natural_cmp(&self.pin, &o.pin))
    }
}

impl fmt::Display for PinRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.refdes, self.pin)
    }
}

impl Serialize for PinRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PinRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = Cow::<str>::deserialize(d)?;
        PinRef::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("`{s}` is not a pin reference (REFDES.PIN)")))
    }
}

impl JsonSchema for PinRef {
    fn schema_name() -> Cow<'static, str> {
        "PinRef".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "description": "Pin reference REFDES.PIN, e.g. \"U1.4\"."})
    }

    fn inline_schema() -> bool {
        true
    }
}

/// A net: a set of connected pins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Net {
    /// Stable internal ID.
    pub id: ObjectId,
    /// Connected pins.
    #[serde(default)]
    pub pins: BTreeSet<PinRef>,
    /// Net class (routing rules), by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Powered from outside the circuit (e.g. through a connector): satisfies ERC for power
    /// inputs on this net, like KiCad's PWR_FLAG.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub driven: bool,
}

/// Routing rules for a group of nets. Unset values fall back to the board's design rules (M4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetClass {
    /// What it is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Track width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_width: Option<Nm>,
    /// Copper clearance to other nets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clearance: Option<Nm>,
    /// Via finished hole diameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_drill: Option<Nm>,
    /// Via pad diameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_diameter: Option<Nm>,
    /// Differential pair track width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_pair_width: Option<Nm>,
    /// Differential pair gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_pair_gap: Option<Nm>,
}

/// A reusable subcircuit: components with local designators, nets with local names, and the
/// nets exposed as ports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Block {
    /// What it does.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Components by local designator.
    pub components: BTreeMap<String, BlockComponent>,
    /// Nets by local name: pins as LOCALREF.PIN.
    pub nets: BTreeMap<String, BTreeSet<PinRef>>,
    /// Nets connected outside the block when instantiated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    /// Pins intentionally left unconnected (local designators).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub no_connect: BTreeSet<PinRef>,
}

/// A component inside a block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlockComponent {
    /// Part ID.
    pub part: String,
    /// Properties.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
}

/// Connectivity (`circuit.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Circuit {
    /// Components by reference designator.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, Component>,
    /// Nets by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub nets: BTreeMap<String, Net>,
    /// Pins intentionally left unconnected.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub no_connect: BTreeSet<PinRef>,
    /// Net classes by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub netclasses: BTreeMap<String, NetClass>,
    /// Reusable blocks by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub blocks: BTreeMap<String, Block>,
    /// Block instances: instance name → block name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub instances: BTreeMap<String, String>,
}

impl Circuit {
    /// Reference designators in natural order (`R2` before `R10`).
    pub fn refdes_sorted(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.components.keys().map(String::as_str).collect();
        v.sort_by(|a, b| natural_cmp(a, b));
        v
    }

    /// Next free designator with `prefix`: `R1`, `R2`, ...
    pub fn next_refdes(&self, prefix: &str) -> String {
        let max = self
            .components
            .keys()
            .filter_map(|k| k.strip_prefix(prefix))
            .filter_map(|n| n.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        format!("{prefix}{}", max + 1)
    }

    /// Components using `part`.
    pub fn using_part<'a>(&'a self, part: &'a str) -> impl Iterator<Item = (&'a String, &'a Component)> + 'a {
        self.components.iter().filter(move |(_, c)| c.part == part)
    }

    /// The net a pin is on.
    pub fn net_of(&self, pin: &PinRef) -> Option<&str> {
        self.nets.iter().find(|(_, n)| n.pins.contains(pin)).map(|(k, _)| k.as_str())
    }

    /// Pin → net index.
    pub fn pin_index(&self) -> BTreeMap<&PinRef, &str> {
        self.nets.iter().flat_map(|(name, n)| n.pins.iter().map(move |p| (p, name.as_str()))).collect()
    }

    /// Removes every pin of `refdes` from nets and no-connect marks; drops nets left empty.
    pub fn detach_component(&mut self, refdes: &str) {
        for n in self.nets.values_mut() {
            n.pins.retain(|p| p.refdes != refdes);
        }
        self.nets.retain(|_, n| !n.pins.is_empty());
        self.no_connect.retain(|p| p.refdes != refdes);
    }

    /// Renames a component everywhere it is referenced.
    pub fn rename_component(&mut self, from: &str, to: &str) {
        if let Some(c) = self.components.remove(from) {
            self.components.insert(to.to_string(), c);
        }
        let fix = |set: &mut BTreeSet<PinRef>| {
            let moved: Vec<PinRef> = set.iter().filter(|p| p.refdes == from).cloned().collect();
            for p in moved {
                set.remove(&p);
                set.insert(PinRef::new(to, p.pin));
            }
        };
        for n in self.nets.values_mut() {
            fix(&mut n.pins);
        }
        fix(&mut self.no_connect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinref_order_and_serde() {
        let mut v = [PinRef::new("U1", "10"), PinRef::new("R10", "1"), PinRef::new("U1", "2"), PinRef::new("R2", "1")];
        v.sort();
        let s: Vec<String> = v.iter().map(ToString::to_string).collect();
        assert_eq!(s, ["R2.1", "R10.1", "U1.2", "U1.10"]);
        assert_eq!(serde_json::to_string(&PinRef::new("U1", "PA9")).unwrap(), "\"U1.PA9\"");
        assert!(serde_json::from_str::<PinRef>("\"U1\"").is_err());
    }

    #[test]
    fn rename_and_detach() {
        let mut c = Circuit::default();
        c.components.insert(
            "R1".into(),
            Component { id: ObjectId(1), part: "r".into(), block: None, properties: Default::default() },
        );
        c.nets.insert(
            "A".into(),
            Net {
                id: ObjectId(2),
                pins: [PinRef::new("R1", "1"), PinRef::new("R2", "1")].into(),
                class: None,
                driven: false,
            },
        );
        c.no_connect.insert(PinRef::new("R1", "2"));
        c.rename_component("R1", "R5");
        assert!(c.components.contains_key("R5"));
        assert_eq!(c.net_of(&PinRef::new("R5", "1")), Some("A"));
        assert!(c.no_connect.contains(&PinRef::new("R5", "2")));
        c.detach_component("R5");
        c.detach_component("R2");
        assert!(c.nets.is_empty() && c.no_connect.is_empty());
    }
}
