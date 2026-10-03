//! Normalizing distributor data to cadlab conventions: parameter names, value strings, packages,
//! categories, lifecycle. Shared by network providers.

use crate::model::part::{Category, ParamValue, Params};
use crate::value::Quantity;

use super::Lifecycle;

/// Cleans a distributor value string so [`ParamValue::parse`] understands it:
/// `"10 kOhms"` → `"10kΩ"`, `"±1%"` → `"1%"`, `"0.063W, 1/16W"` → `"0.063W"`,
/// `"-40°C ~ 85°C (TA)"` → `"-40°C..85°C"`, `"0.4V @ 600mA"` → `"0.4V"`, `"4.7 µH"` → `"4.7µH"`.
pub fn clean_value(s: &str) -> String {
    let mut v = s.trim().to_string();
    // First alternative only.
    if let Some(i) = v.find(',') {
        v.truncate(i);
    }
    // Conditions ("@ 600mA") and notes ("(TA)").
    if let Some(i) = v.find('@') {
        v.truncate(i);
    }
    if let Some(i) = v.find('(') {
        v.truncate(i);
    }
    let v = v.replace('±', "").replace("Ohms", "Ω").replace("Ohm", "Ω").replace(" ~ ", "..").replace('~', "..");
    // Drop spaces between number and unit, but keep range separators.
    v.split("..").map(|p| p.split_whitespace().collect::<String>()).collect::<Vec<_>>().join("..").trim().to_string()
}

/// Maps a distributor parameter name to a cadlab key, for the names that matter for selection.
pub fn param_key(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    Some(match n.as_str() {
        "resistance" => "resistance",
        "capacitance" => "capacitance",
        "inductance" => "inductance",
        "impedance @ frequency" => "impedance",
        "tolerance" => "tolerance",
        "voltage - rated" | "voltage rating" | "voltage - rated dc" => "voltage_rating",
        "power (watts)" | "power rating" => "power_rating",
        "current rating (amps)" | "current - rated" => "current_rating",
        "temperature coefficient" => "tempco_or_dielectric",
        "operating temperature" => "temperature",
        "voltage - output (min/fixed)" | "output voltage" => "voltage_out",
        "current - output" | "output current" => "current_out",
        "voltage - input (max)" => "voltage_in_max",
        "voltage dropout (max)" => "dropout",
        "frequency" => "frequency",
        "load capacitance" => "load_capacitance",
        "color" => "color",
        "voltage - forward (vf) (typ)" | "voltage - forward (vf) (max) @ if" => "forward_voltage",
        _ => return None,
    })
}

/// Converts distributor (name, value) pairs into cadlab parameters, skipping what does not parse.
pub fn params<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Params {
    let mut out = Params::default();
    for (name, value) in pairs {
        let Some(key) = param_key(name) else { continue };
        let cleaned = clean_value(value);
        if cleaned.is_empty() || cleaned == "-" {
            continue;
        }
        if key == "tempco_or_dielectric" {
            // Ceramic capacitors report the dielectric here (X7R, C0G); resistors a ppm figure.
            let up = cleaned.to_ascii_uppercase();
            if up.starts_with(['X', 'C', 'Y', 'Z', 'N']) && up.len() <= 4 {
                out.insert("dielectric", ParamValue::Text(up.replace("NP0", "C0G")));
            } else if let Ok(q) = Quantity::parse(&cleaned.replace("ppm/°C", "ppm")) {
                out.insert("tempco", ParamValue::Quantity(q));
            }
            continue;
        }
        if key == "color" {
            out.insert("color", ParamValue::Text(cleaned.to_ascii_lowercase()));
            continue;
        }
        if let Ok(v) = ParamValue::parse(key, &cleaned) {
            out.insert(key, v);
        }
    }
    out
}

/// Package name from distributor text: `"0402 (1005 Metric)"` → `0402`,
/// `"SOT-23-5 Thin, TSOT-23-5"` → `SOT-23-5`, `"SC-74A, SOT-753"` → `SC-74A`.
pub fn package(s: &str) -> Option<String> {
    let first = s.split(',').next()?.trim();
    let first = first.split('(').next()?.trim();
    let first = first.trim_end_matches(" Thin").trim_end_matches(" Exposed Pad").trim();
    (!first.is_empty() && first != "-").then(|| first.to_string())
}

/// Category from distributor category names (most specific last).
pub fn category(names: &[&str]) -> Option<Category> {
    let text = names.join(" / ").to_ascii_lowercase();
    let words: Vec<&str> = text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    // Short tokens must match whole words ("led" is in "isolated").
    let has = |s: &str| {
        if s.len() <= 4 && !s.contains(' ') {
            words.iter().any(|w| *w == s || *w == format!("{s}s"))
        } else {
            text.contains(s)
        }
    };
    Some(if has("ldo") || has("linear") && has("regulator") {
        Category::Ldo
    } else if has("switching regulator") || has("dc dc") || has("dc-dc") {
        Category::Regulator
    } else if has("microcontroller") {
        Category::Mcu
    } else if has("ferrite bead") {
        Category::FerriteBead
    } else if has("resistor") {
        Category::Resistor
    } else if has("capacitor") {
        Category::Capacitor
    } else if has("inductor") {
        Category::Inductor
    } else if has("led") {
        Category::Led
    } else if has("diode") || has("rectifier") || has("tvs") {
        Category::Diode
    } else if has("mosfet") || has("fet") {
        Category::Mosfet
    } else if has("bipolar") || has("bjt") {
        Category::TransistorBjt
    } else if has("crystal") {
        Category::Crystal
    } else if has("oscillator") {
        Category::Oscillator
    } else if has("connector") || has("header") {
        Category::Connector
    } else if has("switch") {
        Category::Switch
    } else if has("fuse") {
        Category::Fuse
    } else if has("integrated circuit") || has("ic") {
        Category::Ic
    } else {
        return None;
    })
}

/// Lifecycle from a status string.
pub fn lifecycle(s: &str) -> Lifecycle {
    let s = s.to_ascii_lowercase();
    if s == "active" {
        Lifecycle::Active
    } else if s.contains("not for new") || s.contains("nrnd") {
        Lifecycle::Nrnd
    } else if s.contains("last time") {
        Lifecycle::LastTimeBuy
    } else if s.contains("obsolete") || s.contains("discontinued") || s.contains("end of life") {
        Lifecycle::Obsolete
    } else {
        Lifecycle::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        assert_eq!(clean_value("10 kOhms"), "10kΩ");
        assert_eq!(clean_value("±1%"), "1%");
        assert_eq!(clean_value("0.063W, 1/16W"), "0.063W");
        assert_eq!(clean_value("-40°C ~ 85°C (TA)"), "-40°C..85°C");
        assert_eq!(clean_value("0.4V @ 600mA"), "0.4V");
        assert_eq!(clean_value("4.7 µH"), "4.7µH");
    }

    #[test]
    fn params_mapping() {
        let p = params([
            ("Resistance", "10 kOhms"),
            ("Tolerance", "±1%"),
            ("Power (Watts)", "0.063W, 1/16W"),
            ("Operating Temperature", "-55°C ~ 155°C"),
            ("Temperature Coefficient", "±100ppm/°C"),
            ("Packaging", "Cut Tape (CT)"),
        ]);
        assert_eq!(p.get("resistance").unwrap().to_string(), "10kΩ");
        assert_eq!(p.get("tolerance").unwrap().to_string(), "1%");
        assert_eq!(p.get("power_rating").unwrap().to_string(), "63mW");
        assert_eq!(p.get("temperature").unwrap().to_string(), "-55°C..155°C");
        assert_eq!(p.get("tempco").unwrap().to_string(), "100ppm");
        let p = params([("Capacitance", "0.1 µF"), ("Voltage - Rated", "16V"), ("Temperature Coefficient", "X7R")]);
        assert_eq!(p.get("capacitance").unwrap().to_string(), "100nF");
        assert_eq!(p.get("dielectric").unwrap().to_string(), "X7R");
        let p = params([("Voltage - Output (Min/Fixed)", "3.3V"), ("Current - Output", "600mA")]);
        assert_eq!(p.get("voltage_out").unwrap().to_string(), "3.3V");
        assert_eq!(p.get("current_out").unwrap().to_string(), "600mA");
    }

    #[test]
    fn packages_categories_lifecycle() {
        assert_eq!(package("0402 (1005 Metric)").as_deref(), Some("0402"));
        assert_eq!(package("SOT-23-5 Thin, TSOT-23-5").as_deref(), Some("SOT-23-5"));
        assert_eq!(
            category(&["Integrated Circuits (ICs)", "Voltage Regulators - Linear, Low Drop Out (LDO) Regulators"]),
            Some(Category::Ldo)
        );
        assert_eq!(category(&["Resistors", "Chip Resistor - Surface Mount"]), Some(Category::Resistor));
        assert_eq!(category(&["Capacitors", "Ceramic Capacitors"]), Some(Category::Capacitor));
        assert_eq!(category(&["Isolators", "Digital Isolators"]), None);
        assert_eq!(category(&["Optoelectronics", "LED Indication - Discrete"]), Some(Category::Led));
        assert_eq!(lifecycle("Not For New Designs"), Lifecycle::Nrnd);
        assert_eq!(lifecycle("Active"), Lifecycle::Active);
    }
}
