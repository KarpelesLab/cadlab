//! Symbol generator: lays out a part's pins on a symbol, on a 2.54 mm (100 mil) grid.
//!
//! Two-terminal parts get a fixed drawing (resistor, capacitor, diode, ...). Everything else gets
//! a box: ground pins at the bottom, supply inputs at the top, inputs and bidirectional pins on
//! the left, outputs on the right, connector pins on the left in pin-number order. Explicit
//! `side`s on pins are kept. Groups (`PORTA`, `USB`, or the name prefix `PA`, `PB`) stay together,
//! and the left and right sides are balanced by moving whole groups.

use std::collections::BTreeMap;

use crate::geom::Point;
use crate::model::part::{Category, Pin, PinKind, Side, Symbol, SymbolStyle};
use crate::model::sections::natural_cmp;
use crate::units::Nm;

/// Pin spacing and grid.
pub const GRID: Nm = Nm::from_um(2540);
/// Pin length.
pub const PIN_LENGTH: Nm = Nm::from_um(2540);
/// Approximate width of one label character at the default text size.
const CHAR_WIDTH: Nm = Nm::from_um(800);

/// Drawing style for a category with the given number of pins.
pub fn style_for(category: Category, pins: usize) -> SymbolStyle {
    if pins != 2 {
        return SymbolStyle::Box;
    }
    match category {
        Category::Resistor => SymbolStyle::Resistor,
        Category::Capacitor => SymbolStyle::Capacitor,
        Category::Inductor => SymbolStyle::Inductor,
        Category::FerriteBead => SymbolStyle::FerriteBead,
        Category::Diode => SymbolStyle::Diode,
        Category::Led => SymbolStyle::Led,
        Category::Crystal => SymbolStyle::Crystal,
        Category::Fuse => SymbolStyle::Fuse,
        Category::Switch => SymbolStyle::Switch,
        _ => SymbolStyle::Box,
    }
}

/// Standard pins for a two-terminal part. Diodes and LEDs: pin 1 is the cathode (`K`), as on
/// SMD diode packages.
pub fn two_terminal_pins(category: Category) -> Vec<Pin> {
    match category {
        Category::Diode | Category::Led => {
            vec![Pin::new("1", "K", PinKind::Passive), Pin::new("2", "A", PinKind::Passive)]
        }
        _ => vec![Pin::new("1", "", PinKind::Passive), Pin::new("2", "", PinKind::Passive)],
    }
}

/// Lays out `pins` for a part of `category`.
pub fn generate(category: Category, mut pins: Vec<Pin>) -> Symbol {
    let style = style_for(category, pins.len());
    if style != SymbolStyle::Box {
        // Horizontal two-terminal symbol. Diodes: anode left, cathode right.
        let (left, right) = if matches!(style, SymbolStyle::Diode | SymbolStyle::Led) { (1, 0) } else { (0, 1) };
        let x = GRID + Nm(GRID.0 / 2);
        pins[left].side = Some(Side::Left);
        pins[left].at = Some(Point::new(-x, Nm::ZERO));
        pins[right].side = Some(Side::Right);
        pins[right].at = Some(Point::new(x, Nm::ZERO));
        return Symbol { style, body: None, pins };
    }

    // Assign sides.
    for p in &mut pins {
        if p.side.is_none() {
            p.side = Some(default_side(category, p));
        }
    }
    if category != Category::Connector {
        balance(&mut pins);
    }

    // Order within each side.
    let mut by_side: BTreeMap<Side, Vec<usize>> = BTreeMap::new();
    for (i, p) in pins.iter().enumerate() {
        by_side.entry(p.side.unwrap()).or_default().push(i);
    }
    for (side, idx) in by_side.iter_mut() {
        if category == Category::Connector {
            idx.sort_by(|&a, &b| natural_cmp(&pins[a].number, &pins[b].number));
        } else {
            idx.sort_by(|&a, &b| {
                let (pa, pb) = (&pins[a], &pins[b]);
                let key = |p: &Pin| p.group.clone().unwrap_or_else(|| name_group(p.label()));
                // Top/bottom: by name; left/right: by group, then name.
                if matches!(side, Side::Top | Side::Bottom) {
                    natural_cmp(pa.label(), pb.label())
                } else {
                    key(pa).cmp(&key(pb)).then_with(|| natural_cmp(pa.label(), pb.label()))
                }
            });
        }
    }

    let count = |s: Side| by_side.get(&s).map_or(0, Vec::len) as i64;
    let longest = |s: Side| {
        by_side.get(&s).map_or(0, |v| v.iter().map(|&i| pins[i].label().chars().count()).max().unwrap_or(0)) as i64
    };

    // Body size on the grid: room for labels on both sides, and for the top/bottom pins.
    let label_w = Nm(CHAR_WIDTH.0 * (longest(Side::Left) + longest(Side::Right)) + GRID.0 * 2);
    let tb_w = Nm(GRID.0 * (count(Side::Top).max(count(Side::Bottom)) + 1));
    let width = round_up_even(label_w.max(tb_w).max(GRID * 4));
    let rows = count(Side::Left).max(count(Side::Right)).max(1);
    // Rows are centered: top and bottom pin names each need room on both ends.
    let label_h = Nm(CHAR_WIDTH.0 * 2 * (longest(Side::Top).max(longest(Side::Bottom))));
    let height = round_up_even(
        Nm(GRID.0 * (rows + 1))
            + label_h
            + if count(Side::Top) > 0 || count(Side::Bottom) > 0 { GRID } else { Nm::ZERO },
    );
    let (hw, hh) = (Nm(width.0 / 2), Nm(height.0 / 2));

    for (side, idx) in &by_side {
        let n = idx.len() as i64;
        for (k, &i) in idx.iter().enumerate() {
            let k = k as i64;
            // Centered run of positions on the grid.
            let offset = Nm(GRID.0 * (n - 1)) / 2;
            let along = snap(offset - GRID * k);
            let at = match side {
                Side::Left => Point::new(-hw - PIN_LENGTH, along),
                Side::Right => Point::new(hw + PIN_LENGTH, along),
                Side::Top => Point::new(-along, hh + PIN_LENGTH),
                Side::Bottom => Point::new(-along, -hh - PIN_LENGTH),
            };
            pins[i].at = Some(at);
        }
    }
    // Keep pins in datasheet (pin number) order in the stored symbol.
    pins.sort_by(|a, b| natural_cmp(&a.number, &b.number));
    Symbol { style, body: Some((width, height)), pins }
}

/// Rounds up to an even number of grid steps, so the half-size (pin positions) stays on grid.
fn round_up_even(v: Nm) -> Nm {
    let step = GRID.0 * 2;
    Nm((v.0 + step - 1) / step * step)
}

/// Snaps to the grid (pins in an even-length run fall on half steps otherwise).
fn snap(v: Nm) -> Nm {
    Nm((v.0 as f64 / GRID.0 as f64).round() as i64 * GRID.0)
}

/// Whether a pin name denotes ground (`GND`, `AGND`, `VSS`, `EP`, ...).
pub fn is_ground(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("GND") || n.ends_with("GND") || n.starts_with("VSS") || n == "EP" || n == "PAD" || n == "V-"
}

/// Whether a pin name denotes a crystal/oscillator pin (`OSC_IN`, `XTAL1`, `XOUT`, ...). Such pins
/// share one side so the crystal can sit between them.
pub fn is_oscillator(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("OSC") || n.starts_with("XTAL") || matches!(n.as_str(), "XIN" | "XOUT" | "XI" | "XO")
}

fn default_side(category: Category, p: &Pin) -> Side {
    if category == Category::Connector {
        return Side::Left;
    }
    if is_oscillator(p.label()) && !matches!(p.kind, PinKind::PowerIn | PinKind::PowerOut) {
        return Side::Left;
    }
    match p.kind {
        PinKind::PowerIn if is_ground(p.label()) => Side::Bottom,
        PinKind::PowerIn => Side::Top,
        PinKind::Passive if is_ground(p.label()) => Side::Bottom,
        PinKind::Output | PinKind::TriState | PinKind::OpenCollector | PinKind::OpenEmitter | PinKind::PowerOut => {
            Side::Right
        }
        PinKind::NoConnect => Side::Right,
        PinKind::Input | PinKind::Bidirectional | PinKind::Passive | PinKind::Unspecified => Side::Left,
    }
}

/// Group key from a name: alphabetic prefix of port pins (`PA9` → `PA`), else the name itself.
fn name_group(name: &str) -> String {
    if is_oscillator(name) {
        return "OSC".into();
    }
    let prefix: String = name.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let rest = &name[prefix.len()..];
    if prefix.len() == 2 && prefix.starts_with('P') && rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        prefix
    } else {
        String::new()
    }
}

/// Moves whole groups of auto-placed pins from the fuller of left/right to the other side while
/// that improves balance.
fn balance(pins: &mut [Pin]) {
    loop {
        let count = |s: Side| pins.iter().filter(|p| p.side == Some(s)).count() as i64;
        let (l, r) = (count(Side::Left), count(Side::Right));
        let (from, to) = if l > r { (Side::Left, Side::Right) } else { (Side::Right, Side::Left) };
        let diff = (l - r).abs();
        // Candidate groups on the fuller side, largest first that still improves balance.
        let mut groups: BTreeMap<String, i64> = BTreeMap::new();
        for p in pins.iter().filter(|p| p.side == Some(from)) {
            let g = p.group.clone().unwrap_or_else(|| name_group(p.label()));
            if g.is_empty() {
                continue;
            }
            *groups.entry(g).or_default() += 1;
        }
        let best =
            groups.iter().filter(|(_, n)| 2 * **n <= diff).max_by_key(|(g, n)| (**n, std::cmp::Reverse((*g).clone())));
        let Some((g, _)) = best else { return };
        let g = g.clone();
        for p in pins.iter_mut().filter(|p| p.side == Some(from)) {
            if p.group.clone().unwrap_or_else(|| name_group(p.label())) == g {
                p.side = Some(to);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(n: &str, name: &str, kind: PinKind) -> Pin {
        Pin::new(n, name, kind)
    }

    #[test]
    fn resistor() {
        let s = generate(Category::Resistor, two_terminal_pins(Category::Resistor));
        assert_eq!(s.style, SymbolStyle::Resistor);
        assert_eq!(s.pins[0].at, Some(Point::new(-Nm::from_um(3810), Nm::ZERO)));
        assert_eq!(s.pins[1].at, Some(Point::new(Nm::from_um(3810), Nm::ZERO)));
    }

    #[test]
    fn diode_cathode_right() {
        let s = generate(Category::Led, two_terminal_pins(Category::Led));
        let k = s.pin("K").unwrap();
        assert_eq!(k.number, "1");
        assert_eq!(k.side, Some(Side::Right));
    }

    #[test]
    fn ldo_box() {
        let pins = vec![
            pin("1", "VIN", PinKind::PowerIn),
            pin("2", "GND", PinKind::PowerIn),
            pin("3", "EN", PinKind::Input),
            pin("4", "NC", PinKind::NoConnect),
            pin("5", "VOUT", PinKind::PowerOut),
        ];
        let s = generate(Category::Ldo, pins);
        assert_eq!(s.style, SymbolStyle::Box);
        let side = |n: &str| s.pin(n).unwrap().side.unwrap();
        assert_eq!(side("VIN"), Side::Top);
        assert_eq!(side("GND"), Side::Bottom);
        assert_eq!(side("EN"), Side::Left);
        assert_eq!(side("VOUT"), Side::Right);
        // Every pin end is on the grid.
        for p in &s.pins {
            let at = p.at.unwrap();
            assert_eq!(at.x.0 % GRID.0, 0, "{p:?}");
            assert_eq!(at.y.0 % GRID.0, 0, "{p:?}");
        }
        // Pins stay in number order.
        let numbers: Vec<&str> = s.pins.iter().map(|p| p.number.as_str()).collect();
        assert_eq!(numbers, ["1", "2", "3", "4", "5"]);
    }

    #[test]
    fn mcu_ports_balanced() {
        let mut pins = vec![pin("1", "VDD", PinKind::PowerIn), pin("2", "VSS", PinKind::PowerIn)];
        for i in 0..8 {
            pins.push(pin(&(3 + i).to_string(), &format!("PA{i}"), PinKind::Bidirectional));
            pins.push(pin(&(11 + i).to_string(), &format!("PB{i}"), PinKind::Bidirectional));
        }
        let s = generate(Category::Mcu, pins);
        let side = |n: &str| s.pin(n).unwrap().side.unwrap();
        assert_ne!(side("PA0"), side("PB0"), "ports should split across sides");
        assert_eq!(side("PA0"), side("PA7"), "a port stays together");
        // Order within a side: PA0 above PA1.
        let y = |n: &str| s.pin(n).unwrap().at.unwrap().y;
        assert!(y("PA0") > y("PA1"));
    }

    #[test]
    fn oscillator_pins_together() {
        let mut pins = vec![
            pin("1", "VDD", PinKind::PowerIn),
            pin("2", "OSC_IN", PinKind::Input),
            pin("3", "OSC_OUT", PinKind::Output),
            pin("4", "TX", PinKind::Output),
        ];
        pins.extend((0..4).map(|i| pin(&(5 + i).to_string(), &format!("PA{i}"), PinKind::Bidirectional)));
        let s = generate(Category::Mcu, pins);
        let side = |n: &str| s.pin(n).unwrap().side.unwrap();
        assert_eq!(side("OSC_IN"), side("OSC_OUT"));
        let y = |n: &str| s.pin(n).unwrap().at.unwrap().y;
        assert_eq!((y("OSC_IN") - y("OSC_OUT")).0.abs(), GRID.0, "adjacent");
        assert_eq!(generate(Category::Switch, two_terminal_pins(Category::Switch)).style, SymbolStyle::Switch);
    }

    #[test]
    fn connector_in_number_order() {
        let pins = (1..=4).map(|i| pin(&i.to_string(), &format!("P{i}"), PinKind::Passive)).collect();
        let s = generate(Category::Connector, pins);
        let y = |n: &str| s.pin(n).unwrap().at.unwrap().y;
        assert!(y("1") > y("2") && y("2") > y("3") && y("3") > y("4"));
        assert!(s.pins.iter().all(|p| p.side == Some(Side::Left)));
    }
}
