//! Human-facing object references, as written by users and agents.
//!
//! Syntax (see `docs/DATA_MODEL.md`):
//!
//! | Form | Meaning | Example |
//! |---|---|---|
//! | `net:<name>` | net | `net:VBUS`, `net:/usb/D+` |
//! | `mpn:<mpn>`, `local:<id>`, `lib:<id>` | part | `mpn:AP2112K-3.3TRG1` |
//! | `<refdes>.<pin>` | pin (number or name) | `U1.4`, `U1.PA9` |
//! | `<layer>` | layer | `F.Cu`, `In1.Cu`, `Edge.Cuts` |
//! | `<kind>#<n>` | board item by index/ID | `via#42`, `track#1203` |
//! | `<kind>:<name>` | other named object | `zone:GND_bottom`, `class:power` |
//! | anything else | bare name: refdes, net, ... resolved in context | `R12`, `VBUS` |
//!
//! Parsing is purely syntactic. Resolving a bare name against a project is done by the model,
//! which knows what exists.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A reference to a model object.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectRef {
    /// A net, `net:VBUS`.
    Net(String),
    /// A part, with its scheme: `mpn:...`, `local:...`, `lib:...`.
    Part {
        /// `mpn`, `local` or `lib`.
        scheme: String,
        /// The identifier after the colon.
        id: String,
    },
    /// A component pin, `U1.4`.
    Pin {
        /// Reference designator.
        component: String,
        /// Pin number or name.
        pin: String,
    },
    /// A layer, `F.Cu`.
    Layer(String),
    /// A board item by kind and number, `via#42`.
    Item {
        /// Item kind (`via`, `track`, ...).
        kind: String,
        /// Number.
        index: u64,
    },
    /// Another named object, `zone:GND_bottom`.
    Named {
        /// Object kind.
        kind: String,
        /// Name.
        name: String,
    },
    /// A bare name to resolve in context (refdes, net, ...).
    Name(String),
}

/// Part reference schemes.
pub const PART_SCHEMES: &[&str] = &["mpn", "local", "lib"];

impl ObjectRef {
    /// Parses a reference. Never fails on non-empty input: unknown forms become [`ObjectRef::Name`].
    pub fn parse(s: &str) -> Result<ObjectRef, RefError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(RefError::Empty);
        }
        if let Some((kind, rest)) = s.split_once('#')
            && is_ident(kind)
            && let Ok(index) = rest.parse::<u64>()
        {
            return Ok(ObjectRef::Item {
                kind: kind.to_string(),
                index,
            });
        }
        if let Some((scheme, rest)) = s.split_once(':')
            && is_ident(scheme)
            && !rest.is_empty()
        {
            let rest = rest.to_string();
            return Ok(match scheme {
                "net" => ObjectRef::Net(rest),
                "layer" => ObjectRef::Layer(rest),
                _ if PART_SCHEMES.contains(&scheme) => ObjectRef::Part {
                    scheme: scheme.to_string(),
                    id: rest,
                },
                _ => ObjectRef::Named {
                    kind: scheme.to_string(),
                    name: rest,
                },
            });
        }
        if is_layer_name(s) {
            return Ok(ObjectRef::Layer(s.to_string()));
        }
        if let Some((component, pin)) = s.split_once('.')
            && is_refdes(component)
            && !pin.is_empty()
        {
            return Ok(ObjectRef::Pin {
                component: component.to_string(),
                pin: pin.to_string(),
            });
        }
        Ok(ObjectRef::Name(s.to_string()))
    }
}

/// Error parsing an [`ObjectRef`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefError {
    /// Empty input.
    #[error("empty reference")]
    Empty,
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Letters followed by digits, optionally with a hierarchical prefix: `R12`, `U3`, `PWR_U1`.
fn is_refdes(s: &str) -> bool {
    let letters = s.trim_end_matches(|c: char| c.is_ascii_digit());
    letters.len() < s.len() && !letters.is_empty() && letters.chars().all(|c| c.is_ascii_alphabetic() || c == '_')
}

/// Standard layer names: `F.Cu`, `B.SilkS`, `In3.Cu`, `Edge.Cuts`, ...
pub fn is_layer_name(s: &str) -> bool {
    let Some((side, kind)) = s.split_once('.') else {
        return false;
    };
    let side_ok = matches!(side, "F" | "B" | "Edge" | "User")
        || side
            .strip_prefix("In")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    side_ok && !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectRef::Net(n) => write!(f, "net:{n}"),
            ObjectRef::Part { scheme, id } => write!(f, "{scheme}:{id}"),
            ObjectRef::Pin { component, pin } => write!(f, "{component}.{pin}"),
            ObjectRef::Layer(l) if is_layer_name(l) => f.write_str(l),
            ObjectRef::Layer(l) => write!(f, "layer:{l}"),
            ObjectRef::Item { kind, index } => write!(f, "{kind}#{index}"),
            ObjectRef::Named { kind, name } => write!(f, "{kind}:{name}"),
            ObjectRef::Name(n) => f.write_str(n),
        }
    }
}

impl FromStr for ObjectRef {
    type Err = RefError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ObjectRef::parse(s)
    }
}

impl Serialize for ObjectRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ObjectRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = Cow::<str>::deserialize(d)?;
        ObjectRef::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ObjectRef {
    fn schema_name() -> Cow<'static, str> {
        "ObjectRef".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Object reference: refdes (`U1`), pin (`U1.4`, `U1.PA9`), net (`VBUS` or `net:VBUS`), part (`mpn:...`), layer (`F.Cu`), item (`via#42`), named (`zone:GND`)."
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ObjectRef {
        ObjectRef::parse(s).unwrap()
    }

    #[test]
    fn parses_forms() {
        assert_eq!(p("net:VBUS"), ObjectRef::Net("VBUS".into()));
        assert_eq!(p("net:/usb/D+"), ObjectRef::Net("/usb/D+".into()));
        assert_eq!(
            p("mpn:AP2112K-3.3TRG1"),
            ObjectRef::Part {
                scheme: "mpn".into(),
                id: "AP2112K-3.3TRG1".into()
            }
        );
        assert_eq!(
            p("U1.4"),
            ObjectRef::Pin {
                component: "U1".into(),
                pin: "4".into()
            }
        );
        assert_eq!(
            p("U1.PA9"),
            ObjectRef::Pin {
                component: "U1".into(),
                pin: "PA9".into()
            }
        );
        assert_eq!(p("F.Cu"), ObjectRef::Layer("F.Cu".into()));
        assert_eq!(p("In1.Cu"), ObjectRef::Layer("In1.Cu".into()));
        assert_eq!(p("Edge.Cuts"), ObjectRef::Layer("Edge.Cuts".into()));
        assert_eq!(
            p("via#42"),
            ObjectRef::Item {
                kind: "via".into(),
                index: 42
            }
        );
        assert_eq!(
            p("zone:GND_bottom"),
            ObjectRef::Named {
                kind: "zone".into(),
                name: "GND_bottom".into()
            }
        );
        assert_eq!(p("R12"), ObjectRef::Name("R12".into()));
        assert_eq!(p("VBUS"), ObjectRef::Name("VBUS".into()));
        assert_eq!(p("3.3V"), ObjectRef::Name("3.3V".into()));
        assert!(ObjectRef::parse("  ").is_err());
    }

    #[test]
    fn display_roundtrips() {
        for s in [
            "net:VBUS",
            "mpn:X-1",
            "U1.4",
            "F.Cu",
            "via#42",
            "zone:GND",
            "R12",
            "layer:Mystery",
        ] {
            assert_eq!(p(s).to_string(), s);
            assert_eq!(p(&p(s).to_string()), p(s));
        }
    }
}
