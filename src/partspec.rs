//! Generic part specs: `R 10k 1% 0402`, `C 100nF 16V X7R 0402`, `L 4.7uH 0805`, `LED red 0603`.
//!
//! A generic part describes a requirement (value, tolerance, rating, package) without picking a
//! manufacturer part. It is enough to design and lay out a board; a concrete part satisfying it is
//! chosen per fab at export time (`docs/PARTS.md`).

use crate::landpattern::{self, ChipKind, GenOptions};
use crate::model::footprint::Footprint;
use crate::model::part::{Category, FootprintRef, ParamValue, Params, Part, Provenance, slugify};
use crate::symbolgen;
use crate::value::{Quantity, Unit};

const DIELECTRICS: &[&str] = &["C0G", "NP0", "X5R", "X6S", "X7R", "X7S", "X7T", "X8R", "Y5V", "Z5U"];
const COLORS: &[&str] =
    &["red", "green", "blue", "yellow", "orange", "amber", "white", "warm-white", "pink", "purple", "uv", "ir"];

/// A parsed generic spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericSpec {
    /// Category.
    pub category: Category,
    /// Parameters, including `package`.
    pub params: Params,
}

fn category_of(token: &str) -> Option<Category> {
    Some(match token.to_ascii_uppercase().as_str() {
        "R" | "RES" | "RESISTOR" => Category::Resistor,
        "C" | "CAP" | "CAPACITOR" => Category::Capacitor,
        "L" | "IND" | "INDUCTOR" => Category::Inductor,
        "FB" | "BEAD" | "FERRITE" => Category::FerriteBead,
        "LED" => Category::Led,
        "D" | "DIODE" => Category::Diode,
        _ => return None,
    })
}

fn main_param(c: Category) -> (&'static str, Unit) {
    match c {
        Category::Resistor => ("resistance", Unit::Ohm),
        Category::Capacitor => ("capacitance", Unit::Farad),
        Category::Inductor => ("inductance", Unit::Henry),
        Category::FerriteBead => ("impedance", Unit::Ohm),
        _ => ("", Unit::None),
    }
}

/// Whether a package name is a chip size the generator knows.
fn chip_package(token: &str) -> Option<String> {
    let t = token.to_ascii_uppercase();
    if landpattern::chip_codes().any(|c| c == t) {
        return Some(t);
    }
    landpattern::packages::parse(token, ChipKind::Resistor)
        .ok()
        .filter(|s| matches!(s, landpattern::PackageSpec::Chip { .. }))
        .map(|_| token.to_string())
}

/// Parses a spec.
pub fn parse(spec: &str) -> Result<GenericSpec, String> {
    let mut tokens = spec.split_whitespace();
    let first = tokens.next().ok_or("empty part spec")?;
    let category = category_of(first).ok_or_else(|| {
        format!("`{first}` is not a generic part type; use R, C, L, FB, LED or D (e.g. \"R 10k 1% 0402\")")
    })?;
    let (main_key, main_unit) = main_param(category);
    let mut params = Params::default();
    for t in tokens {
        if let Some(p) = chip_package(t) {
            params.insert("package", ParamValue::Text(p));
            continue;
        }
        if let Some(d) = DIELECTRICS.iter().find(|d| d.eq_ignore_ascii_case(t)) {
            params.insert("dielectric", ParamValue::Text((*d).to_string()));
            continue;
        }
        if matches!(category, Category::Led) && COLORS.contains(&t.to_ascii_lowercase().as_str()) {
            params.insert("color", ParamValue::Text(t.to_ascii_lowercase()));
            continue;
        }
        let q = Quantity::parse(t).map_err(|_| format!("cannot understand `{t}` in \"{spec}\""))?;
        let key = match q.unit {
            Unit::None if !main_key.is_empty() => main_key,
            u if u == main_unit && !main_key.is_empty() => main_key,
            Unit::Percent => "tolerance",
            Unit::Volt => "voltage_rating",
            Unit::Watt => "power_rating",
            Unit::Ampere => "current_rating",
            _ => return Err(format!("`{t}` does not fit a {} spec", category.label().to_lowercase())),
        };
        let q = if q.unit == Unit::None { q.with_unit(main_unit) } else { q };
        if params.get(key).is_some() {
            return Err(format!("`{t}`: {key} given twice in \"{spec}\""));
        }
        params.insert(key, ParamValue::Quantity(q));
    }
    if !main_key.is_empty() && params.get(main_key).is_none() {
        return Err(format!("\"{spec}\" has no {main_key} value"));
    }
    if params.get("package").is_none() {
        return Err(format!("\"{spec}\" has no package; add one, e.g. 0402 or 0603"));
    }
    Ok(GenericSpec { category, params })
}

impl GenericSpec {
    /// Canonical spec string: `R 10k 1% 0402`.
    pub fn canonical(&self) -> String {
        let prefix = match self.category {
            Category::Resistor => "R",
            Category::Capacitor => "C",
            Category::Inductor => "L",
            Category::FerriteBead => "FB",
            Category::Led => "LED",
            Category::Diode => "D",
            _ => "X",
        };
        let mut parts = vec![prefix.to_string()];
        let (main_key, _) = main_param(self.category);
        for key in [
            main_key,
            "tolerance",
            "voltage_rating",
            "current_rating",
            "power_rating",
            "dielectric",
            "color",
            "package",
        ] {
            if key.is_empty() {
                continue;
            }
            if let Some(v) = self.params.get(key) {
                parts.push(match v {
                    ParamValue::Quantity(q) if q.unit == Unit::Ohm => q.display_bare(),
                    v => v.to_string(),
                });
            }
        }
        parts.join(" ")
    }

    /// Library ID: `R_10k_1pct_0402`.
    pub fn id(&self) -> String {
        slugify(&self.canonical())
    }

    fn chip_kind(&self) -> ChipKind {
        match self.category {
            Category::Capacitor => ChipKind::Capacitor,
            Category::Inductor | Category::FerriteBead => ChipKind::Inductor,
            Category::Led => ChipKind::Led,
            Category::Diode => ChipKind::Diode,
            _ => ChipKind::Resistor,
        }
    }

    /// Builds the part and its footprint.
    pub fn build(&self, opts: &GenOptions) -> Result<(Part, Footprint), String> {
        let package = self.params.get("package").and_then(ParamValue::text).ok_or("no package")?;
        let spec = landpattern::packages::parse(package, self.chip_kind())?;
        let footprint = landpattern::generate(&spec, opts).map_err(|e| e.to_string())?;
        let symbol = symbolgen::generate(self.category, symbolgen::two_terminal_pins(self.category));
        let mut desc = vec![self.category.label().to_string()];
        desc.extend(self.canonical().split(' ').skip(1).enumerate().map(|(i, t)| {
            // Show Ω on the main value in descriptions.
            if i == 0 && matches!(self.category, Category::Resistor | Category::FerriteBead) {
                format!("{t}Ω")
            } else {
                t.to_string()
            }
        }));
        let part = Part {
            id: self.id(),
            category: self.category,
            description: desc.join(" "),
            manufacturer: None,
            mpn: None,
            params: self.params.clone(),
            symbol,
            footprints: vec![FootprintRef::new(&footprint.name)],
            datasheet: None,
            provenance: Provenance {
                origin: crate::model::part::Origin::Generated,
                detail: Some(format!("generic: {}", self.canonical())),
                license: None,
            },
        };
        Ok((part, footprint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_names() {
        let s = parse("R 10k 1% 0402").unwrap();
        assert_eq!(s.canonical(), "R 10k 1% 0402");
        assert_eq!(s.id(), "R_10k_1pct_0402");
        let s = parse("resistor 0402 4k7").unwrap();
        assert_eq!(s.canonical(), "R 4.7k 0402");
        let s = parse("C 0.1uF 16V x7r 0402").unwrap();
        assert_eq!(s.canonical(), "C 100nF 16V X7R 0402");
        assert_eq!(s.id(), "C_100nF_16V_X7R_0402");
        let s = parse("LED Red 0603").unwrap();
        assert_eq!(s.canonical(), "LED red 0603");
        let s = parse("L 4.7uH 1A 0805").unwrap();
        assert_eq!(s.canonical(), "L 4.7uH 1A 0805");
    }

    #[test]
    fn errors() {
        assert!(parse("R 10k").unwrap_err().contains("package"));
        assert!(parse("R 0402").unwrap_err().contains("resistance"));
        assert!(parse("U LM358 SOIC-8").unwrap_err().contains("not a generic"));
        assert!(parse("R 10k 10uF 0402").unwrap_err().contains("does not fit"));
        assert!(parse("R 10k 4k7 0402").unwrap_err().contains("twice"));
        assert!(parse("C 1u bogus 0402").unwrap_err().contains("bogus"));
    }

    #[test]
    fn builds_part_and_footprint() {
        let (part, fp) = parse("C 100nF 16V X7R 0402").unwrap().build(&GenOptions::default()).unwrap();
        assert_eq!(part.id, "C_100nF_16V_X7R_0402");
        assert_eq!(part.description, "Capacitor 100nF 16V X7R 0402");
        assert_eq!(part.footprints[0].footprint, "CAPC1005X55N");
        assert_eq!(fp.name, "CAPC1005X55N");
        assert_eq!(part.value(), "100nF");
        let (r, _) = parse("R 10k 1% 0402").unwrap().build(&GenOptions::default()).unwrap();
        assert_eq!(r.description, "Resistor 10kΩ 1% 0402");
        assert_eq!(r.value(), "10k");
    }
}
