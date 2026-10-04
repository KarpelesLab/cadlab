//! Parts: what a component *is* (as opposed to a component instance, which is a use of a part
//! in the circuit). See `docs/PARTS.md`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::geom::Point;
use crate::units::Nm;
use crate::value::{Quantity, Unit, ValueError};

/// Part category. Drives the default reference designator prefix and symbol style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Resistor.
    Resistor,
    /// Capacitor.
    Capacitor,
    /// Inductor.
    Inductor,
    /// Ferrite bead.
    FerriteBead,
    /// Diode (rectifier, Schottky, Zener, TVS).
    Diode,
    /// Light-emitting diode.
    Led,
    /// Bipolar transistor.
    TransistorBjt,
    /// MOSFET.
    Mosfet,
    /// Linear regulator (LDO).
    Ldo,
    /// Switching regulator.
    Regulator,
    /// Microcontroller.
    Mcu,
    /// Other integrated circuit.
    Ic,
    /// Connector.
    Connector,
    /// Crystal or resonator.
    Crystal,
    /// Oscillator.
    Oscillator,
    /// Switch or button.
    Switch,
    /// Fuse or PTC.
    Fuse,
    /// Test point.
    TestPoint,
    /// Mounting hole or fiducial.
    Mechanical,
    /// Anything else.
    Other,
}

impl Category {
    /// Default reference designator prefix.
    pub const fn refdes_prefix(self) -> &'static str {
        match self {
            Category::Resistor => "R",
            Category::Capacitor => "C",
            Category::Inductor => "L",
            Category::FerriteBead => "FB",
            Category::Diode | Category::Led => "D",
            Category::TransistorBjt | Category::Mosfet => "Q",
            Category::Ldo | Category::Regulator | Category::Mcu | Category::Ic | Category::Oscillator => "U",
            Category::Connector => "J",
            Category::Crystal => "Y",
            Category::Switch => "SW",
            Category::Fuse => "F",
            Category::TestPoint => "TP",
            Category::Mechanical => "H",
            Category::Other => "X",
        }
    }

    /// Human name.
    pub fn label(self) -> &'static str {
        match self {
            Category::Resistor => "Resistor",
            Category::Capacitor => "Capacitor",
            Category::Inductor => "Inductor",
            Category::FerriteBead => "Ferrite bead",
            Category::Diode => "Diode",
            Category::Led => "LED",
            Category::TransistorBjt => "BJT",
            Category::Mosfet => "MOSFET",
            Category::Ldo => "LDO regulator",
            Category::Regulator => "Regulator",
            Category::Mcu => "Microcontroller",
            Category::Ic => "IC",
            Category::Connector => "Connector",
            Category::Crystal => "Crystal",
            Category::Oscillator => "Oscillator",
            Category::Switch => "Switch",
            Category::Fuse => "Fuse",
            Category::TestPoint => "Test point",
            Category::Mechanical => "Mechanical",
            Category::Other => "Part",
        }
    }
}

/// A parameter value: a quantity (`10k`, `16V`), a range (`-40..85°C`) or text (`X7R`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ParamValue {
    /// A single quantity.
    Quantity(Quantity),
    /// Inclusive range.
    Range {
        /// Minimum.
        min: Quantity,
        /// Maximum.
        max: Quantity,
    },
    /// Free text.
    Text(String),
}

impl ParamValue {
    /// Parses a parameter value for `key`. Known keys ([`known_param`]) fix the unit: a bare
    /// `10k` for `resistance` means 10 kΩ, and `10uF` is rejected for it.
    pub fn parse(key: &str, s: &str) -> Result<ParamValue, ValueError> {
        let s = s.trim();
        let expected = known_param(key).map(|k| k.unit);
        if expected == Some(None) {
            return Ok(ParamValue::Text(s.to_string()));
        }
        let parse_q = |t: &str| match expected.flatten() {
            Some(u) => Quantity::parse_as(t, u),
            None => Quantity::parse(t),
        };
        if let Some((a, b)) = s.split_once("..") {
            let (mut min, mut max) = (Quantity::parse(a), Quantity::parse(b));
            // `-40..85°C`: a unit on one side applies to both.
            if let (Ok(lo), Ok(hi)) = (&min, &max) {
                if lo.unit == Unit::None && hi.unit != Unit::None {
                    min = Ok(lo.with_unit(hi.unit));
                } else if hi.unit == Unit::None && lo.unit != Unit::None {
                    max = Ok(hi.with_unit(lo.unit));
                }
            }
            if let (Ok(lo), Ok(hi)) = (min, max) {
                let lo = parse_q(&lo.to_string())?;
                let hi = parse_q(&hi.to_string())?;
                return Ok(ParamValue::Range { min: lo, max: hi });
            }
        }
        match parse_q(s) {
            Ok(q) => Ok(ParamValue::Quantity(q)),
            Err(e) if expected.is_some() => Err(e),
            Err(_) => Ok(ParamValue::Text(s.to_string())),
        }
    }

    /// The quantity, if this is one.
    pub fn quantity(&self) -> Option<&Quantity> {
        match self {
            ParamValue::Quantity(q) => Some(q),
            _ => None,
        }
    }

    /// The text, if this is text.
    pub fn text(&self) -> Option<&str> {
        match self {
            ParamValue::Text(t) => Some(t),
            _ => None,
        }
    }
}

impl fmt::Display for ParamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParamValue::Quantity(q) => q.fmt(f),
            ParamValue::Range { min, max } => write!(f, "{min}..{max}"),
            ParamValue::Text(t) => f.write_str(t),
        }
    }
}

/// A known parameter: name, unit (`None` = text) and description.
pub struct KnownParam {
    /// Key.
    pub key: &'static str,
    /// Unit; `None` for text parameters.
    pub unit: Option<Unit>,
    /// What it means.
    pub description: &'static str,
}

/// Parameters with standard meaning. Others are allowed and parsed without a fixed unit.
pub const KNOWN_PARAMS: &[KnownParam] = &[
    KnownParam { key: "resistance", unit: Some(Unit::Ohm), description: "Resistance" },
    KnownParam { key: "capacitance", unit: Some(Unit::Farad), description: "Capacitance" },
    KnownParam { key: "inductance", unit: Some(Unit::Henry), description: "Inductance" },
    KnownParam { key: "impedance", unit: Some(Unit::Ohm), description: "Impedance (e.g. ferrite bead at 100 MHz)" },
    KnownParam { key: "tolerance", unit: Some(Unit::Percent), description: "Value tolerance" },
    KnownParam { key: "voltage_rating", unit: Some(Unit::Volt), description: "Maximum voltage" },
    KnownParam { key: "current_rating", unit: Some(Unit::Ampere), description: "Maximum continuous current" },
    KnownParam { key: "power_rating", unit: Some(Unit::Watt), description: "Maximum power dissipation" },
    KnownParam { key: "voltage_in", unit: Some(Unit::Volt), description: "Input voltage (or range)" },
    KnownParam { key: "voltage_out", unit: Some(Unit::Volt), description: "Output voltage" },
    KnownParam { key: "current_out", unit: Some(Unit::Ampere), description: "Maximum output current" },
    KnownParam { key: "dropout", unit: Some(Unit::Volt), description: "Dropout voltage" },
    KnownParam { key: "forward_voltage", unit: Some(Unit::Volt), description: "Forward voltage" },
    KnownParam { key: "frequency", unit: Some(Unit::Hertz), description: "Frequency" },
    KnownParam { key: "load_capacitance", unit: Some(Unit::Farad), description: "Crystal load capacitance" },
    KnownParam { key: "temperature", unit: Some(Unit::Celsius), description: "Operating temperature range" },
    KnownParam { key: "tempco", unit: Some(Unit::Ppm), description: "Temperature coefficient (ppm/°C)" },
    KnownParam { key: "dielectric", unit: None, description: "Capacitor dielectric (X7R, X5R, C0G, ...)" },
    KnownParam { key: "color", unit: None, description: "LED color" },
    KnownParam { key: "package", unit: None, description: "Package name (0402, SOT-23-5, QFN-32, ...)" },
    KnownParam { key: "pitch", unit: None, description: "Pin pitch, with unit (\"2.54mm\")" },
    KnownParam { key: "spice_model", unit: None, description: "SPICE model or subcircuit name (export.spice)" },
    KnownParam { key: "spice_lib", unit: None, description: "SPICE library file to .include (export.spice)" },
    KnownParam {
        key: "spice_pins",
        unit: None,
        description: "Subcircuit pin order, as pin numbers or names (export.spice)",
    },
];

/// Looks up a known parameter.
pub fn known_param(key: &str) -> Option<&'static KnownParam> {
    KNOWN_PARAMS.iter().find(|k| k.key == key)
}

/// Parameters by key. Serialized as a map of strings: `{"resistance": "10kΩ"}`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Params(pub BTreeMap<String, ParamValue>);

impl Params {
    /// Gets a parameter.
    pub fn get(&self, key: &str) -> Option<&ParamValue> {
        self.0.get(key)
    }

    /// Sets a parameter.
    pub fn insert(&mut self, key: impl Into<String>, v: ParamValue) {
        self.0.insert(key.into(), v);
    }

    /// Parses and sets a parameter.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), ValueError> {
        let v = ParamValue::parse(key, value)?;
        self.0.insert(key.to_string(), v);
        Ok(())
    }
}

impl Serialize for Params {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(self.0.iter().map(|(k, v)| (k, v.to_string())))
    }
}

impl<'de> Deserialize<'de> for Params {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, String>::deserialize(d)?;
        let mut out = BTreeMap::new();
        for (k, v) in raw {
            let pv =
                ParamValue::parse(&k, &v).map_err(|e| serde::de::Error::custom(format!("parameter `{k}`: {e}")))?;
            out.insert(k, pv);
        }
        Ok(Params(out))
    }
}

impl JsonSchema for Params {
    fn schema_name() -> Cow<'static, str> {
        "Params".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "additionalProperties": {"type": "string"},
            "description": "Parameters as strings: {\"resistance\": \"10k\", \"tolerance\": \"1%\", \"voltage_rating\": \"16V\", \"temperature\": \"-40..85°C\", \"dielectric\": \"X7R\"}."
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

/// Electrical type of a pin, used by ERC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PinKind {
    /// Input.
    Input,
    /// Output (push-pull).
    Output,
    /// Bidirectional.
    Bidirectional,
    /// Tri-state output.
    TriState,
    /// Passive (resistors, capacitors, connectors).
    #[default]
    Passive,
    /// Power input (VCC, VIN, GND of an IC).
    PowerIn,
    /// Power output (regulator output).
    PowerOut,
    /// Open collector / open drain.
    OpenCollector,
    /// Open emitter / open source.
    OpenEmitter,
    /// Not connected internally.
    NoConnect,
    /// Unknown.
    Unspecified,
}

/// Side of a symbol body a pin is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Left side, pin pointing left.
    Left,
    /// Right side.
    Right,
    /// Top side.
    Top,
    /// Bottom side.
    Bottom,
}

/// A pin of a part's symbol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    /// Pin number, as in the datasheet: `1`, `A3`, `EP`.
    pub number: String,
    /// Pin name: `VIN`, `PA9`, `~RESET`. Defaults to the number.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Electrical type.
    #[serde(default)]
    pub kind: PinKind,
    /// Functional group, for symbol layout (`power`, `PORTA`, `USB`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Where it is on the symbol; filled by the symbol generator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    /// Pin end position on the symbol (schematic units); filled by the symbol generator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Point>,
    /// Unit (gate) of a multi-unit part the pin belongs to, from 1 (`1` = unit A); absent for
    /// single-unit parts and for pins shared by every unit (supplies). Symbols are drawn as one
    /// body for now; the generator keeps each unit's pins together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<u32>,
    /// Alternate functions of the pin (`USART1_TX` on `PA9`), as KiCad symbols list them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternates: Vec<PinAlternate>,
}

/// An alternate function of a pin: another name and electrical type it can take.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinAlternate {
    /// Name of the function (`USART1_TX`).
    pub name: String,
    /// Electrical type in that function.
    #[serde(default)]
    pub kind: PinKind,
}

impl Pin {
    /// A pin with the given number, name and kind.
    pub fn new(number: impl Into<String>, name: impl Into<String>, kind: PinKind) -> Self {
        Pin {
            number: number.into(),
            name: name.into(),
            kind,
            group: None,
            side: None,
            at: None,
            unit: None,
            alternates: Vec::new(),
        }
    }

    /// The name, or the number when there is no name.
    pub fn label(&self) -> &str {
        if self.name.is_empty() { &self.number } else { &self.name }
    }
}

/// How a symbol is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SymbolStyle {
    /// Rectangle with pins around it (ICs, connectors).
    #[default]
    Box,
    /// Resistor.
    Resistor,
    /// Non-polarized capacitor.
    Capacitor,
    /// Polarized capacitor.
    CapacitorPolarized,
    /// Inductor.
    Inductor,
    /// Ferrite bead.
    FerriteBead,
    /// Diode.
    Diode,
    /// LED.
    Led,
    /// Crystal.
    Crystal,
    /// Fuse.
    Fuse,
    /// Test point.
    TestPoint,
    /// Push button or switch (normally open).
    Switch,
}

/// Schematic symbol: pins plus drawing style. Geometry is generated (`docs/PARTS.md`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Symbol {
    /// Drawing style.
    #[serde(default)]
    pub style: SymbolStyle,
    /// Body size for box symbols (schematic units); filled by the symbol generator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<(Nm, Nm)>,
    /// Pins.
    pub pins: Vec<Pin>,
}

impl Symbol {
    /// Finds a pin by number, then by name (case-sensitive).
    pub fn pin(&self, key: &str) -> Option<&Pin> {
        self.pins.iter().find(|p| p.number == key).or_else(|| self.pins.iter().find(|p| p.name == key))
    }
}

/// A footprint a part can use, with its pin → pad mapping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FootprintRef {
    /// Footprint name in the library.
    pub footprint: String,
    /// Pin number → pad numbers. Pins not listed map to the pad with the same number.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pin_map: BTreeMap<String, Vec<String>>,
    /// 3D model of this part on this footprint, overriding the footprint's own model
    /// (`footprint.model_set --part`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<crate::model::model3d::Model3d>,
}

impl FootprintRef {
    /// Reference with the identity pin map.
    pub fn new(footprint: impl Into<String>) -> Self {
        FootprintRef { footprint: footprint.into(), pin_map: BTreeMap::new(), model: None }
    }

    /// Pads a pin connects to.
    pub fn pads_for(&self, pin: &str) -> Vec<String> {
        self.pin_map.get(pin).cloned().unwrap_or_else(|| vec![pin.to_string()])
    }
}

/// Where a part definition came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Generated by cadlab from parameters.
    Generated,
    /// Written by a user or agent.
    #[default]
    Manual,
    /// Built from supplier data.
    Supplier,
    /// Imported from a file.
    Import,
}

/// Where a part came from, and under what license.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// Kind of origin.
    pub origin: Origin,
    /// Details: generator spec, supplier and SKU, source file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// License of the source data, when imported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

/// A part definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Part {
    /// Library-unique ID: letters, digits, `. _ + -`.
    pub id: String,
    /// Category.
    pub category: Category,
    /// One-line description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Manufacturer (concrete parts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Manufacturer part number. A part with an MPN is *concrete*; without, *generic*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    /// Typed parameters.
    #[serde(default, skip_serializing_if = "is_empty_params")]
    pub params: Params,
    /// Schematic symbol.
    pub symbol: Symbol,
    /// Footprints, preferred first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footprints: Vec<FootprintRef>,
    /// Datasheet URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datasheet: Option<String>,
    /// Origin and license.
    #[serde(default)]
    pub provenance: Provenance,
}

fn is_empty_params(p: &Params) -> bool {
    p.0.is_empty()
}

impl Part {
    /// Whether this is a concrete part (has an MPN).
    pub fn is_concrete(&self) -> bool {
        self.mpn.is_some()
    }

    /// Short value for BOMs and silkscreen: resistance, capacitance, ... for passives; the MPN
    /// (or ID) for everything else.
    pub fn value(&self) -> String {
        let passive = matches!(
            self.category,
            Category::Resistor
                | Category::Capacitor
                | Category::Inductor
                | Category::FerriteBead
                | Category::Crystal
                | Category::Led
                | Category::Fuse
        );
        if !passive {
            return self.mpn.clone().unwrap_or_else(|| self.id.clone());
        }
        for k in ["resistance", "capacitance", "inductance", "impedance", "frequency", "color"] {
            if let Some(v) = self.params.get(k) {
                return match v {
                    ParamValue::Quantity(q) if q.unit == Unit::Ohm => q.display_bare(),
                    v => v.to_string(),
                };
            }
        }
        self.mpn.clone().unwrap_or_else(|| self.id.clone())
    }

    /// Preferred footprint, if any.
    pub fn footprint(&self) -> Option<&FootprintRef> {
        self.footprints.first()
    }
}

/// Validates a library ID (parts and footprints).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && !id.starts_with('.')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// Turns arbitrary text into a valid ID: `R 10k 1% 0402` → `R_10k_1pct_0402`.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            c if c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-') => out.push(c),
            '%' => out.push_str("pct"),
            'µ' | 'μ' => out.push('u'),
            'Ω' => {}
            '°' => {}
            _ => {
                if !out.ends_with('_') && !out.is_empty() {
                    out.push('_');
                }
            }
        }
    }
    let out = out.trim_matches('_').trim_start_matches('.').to_string();
    if out.is_empty() { "part".into() } else { out.chars().take(100).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_parsing() {
        assert_eq!(ParamValue::parse("resistance", "10k").unwrap().to_string(), "10kΩ");
        assert!(ParamValue::parse("resistance", "10uF").is_err());
        assert_eq!(ParamValue::parse("dielectric", "X7R").unwrap(), ParamValue::Text("X7R".into()));
        assert_eq!(ParamValue::parse("temperature", "-40..85°C").unwrap().to_string(), "-40°C..85°C");
        assert_eq!(ParamValue::parse("temperature", "-40..85").unwrap().to_string(), "-40°C..85°C");
        assert_eq!(ParamValue::parse("voltage_in", "2.5V..6V").unwrap().to_string(), "2.5V..6V");
        assert_eq!(ParamValue::parse("custom", "hello").unwrap(), ParamValue::Text("hello".into()));
        assert_eq!(ParamValue::parse("custom", "12mA").unwrap().to_string(), "12mA");
        assert_eq!(ParamValue::parse("package", "0402").unwrap(), ParamValue::Text("0402".into()));
    }

    #[test]
    fn params_serde() {
        let mut p = Params::default();
        p.set("resistance", "4k7").unwrap();
        p.set("tolerance", "1%").unwrap();
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, r#"{"resistance":"4.7kΩ","tolerance":"1%"}"#);
        assert_eq!(serde_json::from_str::<Params>(&s).unwrap(), p);
        assert!(serde_json::from_str::<Params>(r#"{"resistance":"1uF"}"#).is_err());
    }

    #[test]
    fn slugs_and_ids() {
        assert_eq!(slugify("R 10k 1% 0402"), "R_10k_1pct_0402");
        assert_eq!(slugify("C 4.7µF 10V"), "C_4.7uF_10V");
        assert_eq!(slugify("  "), "part");
        assert!(valid_id("AP2112K-3.3TRG1"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id(".hidden"));
    }
}
