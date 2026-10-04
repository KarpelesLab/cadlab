//! Design lint: circuit-level heuristics beyond ERC (`circuit.lint`, `circuit.erc --lint`).
//!
//! ERC checks that pins of given electrical types are wired consistently; the lint looks for
//! circuits that are wired consistently but are likely wrong. Every rule is a heuristic on
//! part categories, pin types and names, and net names, documented in `docs/ELECTRICAL.md`:
//!
//! | Code | Severity | Finding |
//! |---|---|---|
//! | `lint.missing_decoupling` | warning | an IC power input (or regulator output) rail with no capacitor to ground |
//! | `lint.i2c_pullup` | warning | an I²C line (net or pin named SDA/SCL) without a resistor to a supply |
//! | `lint.usb_esd` | warning | a USB data line reaching a connector with no ESD/TVS protection on it |
//! | `lint.clock_termination` | info | a clock output driving other parts directly, with no series resistor |
//! | `lint.floating_input` | warning | an input pin left unconnected, or on a net with only inputs and capacitors |

use std::collections::{BTreeMap, BTreeSet};

use crate::diag::Diagnostic;
use crate::model::Project;
use crate::model::circuit::{Net, PinRef};
use crate::model::part::{Category, Part, PinKind};
use crate::model::sections::natural_cmp;
use crate::refs::ObjectRef;
use crate::symbolgen::is_ground;

fn pin_ref(p: &PinRef) -> ObjectRef {
    ObjectRef::Pin { component: p.refdes.clone(), pin: p.pin.clone() }
}

/// Name tokens: split on separators, upper case.
fn tokens(name: &str) -> Vec<String> {
    name.split(['_', '/', ' ', '.', '(', ')', ',', ':'])
        .filter(|t| !t.is_empty())
        .map(str::to_ascii_uppercase)
        .collect()
}

/// `SDA`, `SCL`, optionally followed by digits (`SDA1`), as a token of a net or pin name.
fn i2c_line(name: &str) -> bool {
    tokens(name).iter().any(|t| {
        ["SDA", "SCL"].iter().any(|k| t.strip_prefix(k).is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit())))
    })
}

/// USB data line names: `D+`, `D-`, `DP`, `DM`, `DN`, `USB_DP`, `USBDM`, `USB_D+`, ...
fn usb_line(name: &str) -> bool {
    let t = tokens(name);
    let data =
        |s: &str| matches!(s, "D+" | "D-" | "DP" | "DM" | "DN" | "USBDP" | "USBDM" | "USBDN" | "USBD+" | "USBD-");
    t.iter().any(|s| data(s)) || (t.iter().any(|s| s.starts_with("USB")) && t.last().is_some_and(|s| data(s)))
}

/// Clock names: a token starting or ending with `CLK` (`CLK`, `MCLK`, `CLKOUT`, `SYSCLK`), or `CLOCK`.
fn clock_name(name: &str) -> bool {
    tokens(name).iter().any(|t| t.starts_with("CLK") || t.ends_with("CLK") || t.starts_with("CLOCK"))
}

struct Ctx<'a> {
    p: &'a Project,
    /// Pin → (part, label, kind).
    pins: BTreeMap<PinRef, (&'a Part, &'a str, PinKind)>,
    ground: BTreeSet<&'a str>,
    power: BTreeSet<&'a str>,
}

impl<'a> Ctx<'a> {
    fn new(p: &'a Project) -> Self {
        let c = p.circuit();
        let lib = p.library();
        let mut pins = BTreeMap::new();
        for (refdes, comp) in &c.components {
            let Some(part) = lib.parts.get(&comp.part) else { continue };
            for sp in &part.symbol.pins {
                pins.insert(PinRef::new(refdes, &sp.number), (part, sp.label(), sp.kind));
            }
        }
        let mut ground = BTreeSet::new();
        let mut power = BTreeSet::new();
        for (name, net) in &c.nets {
            let ground_pin = net
                .pins
                .iter()
                .any(|pr| pins.get(pr).is_some_and(|(_, label, kind)| *kind == PinKind::PowerIn && is_ground(label)));
            let named_ground = is_ground(name) && !matches!(name.to_ascii_uppercase().as_str(), "EP" | "PAD");
            if named_ground || ground_pin {
                ground.insert(name.as_str());
                continue;
            }
            let power_pin = net
                .pins
                .iter()
                .any(|pr| pins.get(pr).is_some_and(|(_, _, k)| matches!(k, PinKind::PowerIn | PinKind::PowerOut)));
            if power_pin || net.driven || net.voltage.is_some() || crate::spice::rail_voltage(name).is_some() {
                power.insert(name.as_str());
            }
        }
        Ctx { p, pins, ground, power }
    }

    fn net_of(&self, pin: &PinRef) -> Option<&'a str> {
        self.p.circuit().net_of(pin)
    }

    fn category(&self, pin: &PinRef) -> Option<Category> {
        self.pins.get(pin).map(|(part, _, _)| part.category)
    }

    /// Two-pin parts of `cat` with one pin on `net`: the net of their other pin (None: open).
    fn others(&self, net: &Net, cat: Category) -> Vec<(String, Option<&'a str>)> {
        let mut v = Vec::new();
        for pr in &net.pins {
            if self.category(pr) != Some(cat) {
                continue;
            }
            let (part, _, _) = self.pins[pr];
            if part.symbol.pins.len() != 2 {
                continue;
            }
            let other = part.symbol.pins.iter().find(|s| s.number != pr.pin).expect("two pins");
            v.push((pr.refdes.clone(), self.net_of(&PinRef::new(&pr.refdes, &other.number))));
        }
        v
    }
}

fn ic_like(c: Category) -> bool {
    matches!(c, Category::Mcu | Category::Ic | Category::Ldo | Category::Regulator | Category::Oscillator)
}

/// Runs every lint rule; diagnostics in a fixed order (rule, then net/component).
pub fn check(p: &Project) -> Vec<Diagnostic> {
    let ctx = Ctx::new(p);
    let mut out = Vec::new();
    decoupling(&ctx, &mut out);
    i2c(&ctx, &mut out);
    usb(&ctx, &mut out);
    clocks(&ctx, &mut out);
    floating(&ctx, &mut out);
    out
}

fn decoupling(ctx: &Ctx, out: &mut Vec<Diagnostic>) {
    let c = ctx.p.circuit();
    for refdes in c.refdes_sorted() {
        let comp = &c.components[refdes];
        let Some(part) = ctx.p.library().parts.get(&comp.part) else { continue };
        if !ic_like(part.category) {
            continue;
        }
        let regulator = matches!(part.category, Category::Ldo | Category::Regulator);
        // Rails this part draws from (or, for regulators, also drives).
        let mut rails: BTreeMap<&str, Vec<PinRef>> = BTreeMap::new();
        for sp in &part.symbol.pins {
            let wanted = match sp.kind {
                PinKind::PowerIn => !is_ground(sp.label()),
                PinKind::PowerOut => regulator,
                _ => false,
            };
            let pr = PinRef::new(refdes, &sp.number);
            if wanted
                && let Some(net) = ctx.net_of(&pr)
                && !ctx.ground.contains(net)
            {
                rails.entry(net).or_default().push(pr);
            }
        }
        for (net, pins) in rails {
            let caps = ctx.others(&c.nets[net], Category::Capacitor);
            if caps.iter().any(|(_, other)| other.is_some_and(|o| ctx.ground.contains(o))) {
                continue;
            }
            let labels: Vec<String> = pins.iter().map(|pr| format!("{pr} ({})", ctx.pins[pr].1)).collect();
            let what = if pins.iter().all(|pr| ctx.pins[pr].2 == PinKind::PowerOut) { "output" } else { "supply" };
            let mut d = Diagnostic::warning(
                "lint.missing_decoupling",
                format!(
                    "{refdes} ({}): {what} rail `{net}` ({}) has no capacitor to ground",
                    part.value(),
                    labels.join(", ")
                ),
            )
            .with_subject(ObjectRef::Net(net.to_string()))
            .with_hint(format!(
                "add a capacitor from `{net}` to ground next to {refdes} (typically 100 nF per supply pin, plus bulk \
                 per the datasheet): `circuit.add \"C 100nF 16V X7R 0402\"` then `net.connect`"
            ));
            for pr in &pins {
                d = d.with_subject(pin_ref(pr));
            }
            out.push(d);
        }
    }
}

fn i2c(ctx: &Ctx, out: &mut Vec<Diagnostic>) {
    for (name, net) in &ctx.p.circuit().nets {
        let pin_named = net.pins.iter().any(|pr| {
            ctx.pins.get(pr).is_some_and(|(part, label, _)| part.category != Category::Connector && i2c_line(label))
        });
        if !(i2c_line(name) || pin_named) || ctx.ground.contains(name.as_str()) {
            continue;
        }
        let pullups = ctx.others(net, Category::Resistor);
        if pullups.iter().any(|(_, other)| other.is_some_and(|o| ctx.power.contains(o))) {
            continue;
        }
        out.push(
            Diagnostic::warning("lint.i2c_pullup", format!("I²C line `{name}` has no pull-up resistor to a supply"))
                .with_subject(ObjectRef::Net(name.clone()))
                .with_hint(format!(
                    "I²C is open-drain: add a pull-up (2.2k to 10k, e.g. 4.7k) from `{name}` to the bus supply, \
                     unless the bus is pulled up elsewhere (then ignore this)"
                )),
        );
    }
}

fn protection(part: &Part) -> bool {
    if part.category == Category::Diode {
        return true;
    }
    let text = format!("{} {} {}", part.id, part.description, part.mpn.as_deref().unwrap_or("")).to_ascii_uppercase();
    text.contains("ESD") || text.contains("TVS")
}

fn usb(ctx: &Ctx, out: &mut Vec<Diagnostic>) {
    for (name, net) in &ctx.p.circuit().nets {
        let connector_pins: Vec<&PinRef> =
            net.pins.iter().filter(|pr| ctx.category(pr) == Some(Category::Connector)).collect();
        if connector_pins.is_empty() {
            continue;
        }
        let usb_pin = connector_pins.iter().any(|pr| usb_line(ctx.pins[*pr].1));
        if !(usb_line(name) || usb_pin) {
            continue;
        }
        if net.pins.iter().any(|pr| ctx.pins.get(pr).is_some_and(|(part, _, _)| protection(part))) {
            continue;
        }
        let mut d = Diagnostic::warning(
            "lint.usb_esd",
            format!("USB data line `{name}` reaches a connector without ESD protection"),
        )
        .with_subject(ObjectRef::Net(name.clone()))
        .with_hint(
            "add a low-capacitance ESD/TVS array for USB data lines close to the connector (a part whose \
             description or MPN says ESD or TVS, or a diode category part, on the net)",
        );
        for pr in connector_pins {
            d = d.with_subject(pin_ref(pr));
        }
        out.push(d);
    }
}

fn clocks(ctx: &Ctx, out: &mut Vec<Diagnostic>) {
    for (name, net) in &ctx.p.circuit().nets {
        let drivers: Vec<&PinRef> = net
            .pins
            .iter()
            .filter(|pr| {
                ctx.pins.get(pr).is_some_and(|(part, label, kind)| {
                    *kind == PinKind::Output
                        && (part.category == Category::Oscillator || clock_name(label) || clock_name(name))
                })
            })
            .collect();
        let [driver] = drivers.as_slice() else { continue };
        if net.pins.iter().any(|pr| ctx.category(pr) == Some(Category::Resistor)) {
            continue;
        }
        let receivers: Vec<&PinRef> = net
            .pins
            .iter()
            .filter(|pr| {
                pr.refdes != driver.refdes
                    && ctx.pins.get(pr).is_some_and(|(part, _, kind)| {
                        matches!(kind, PinKind::Input | PinKind::Bidirectional) && ic_like(part.category)
                    })
            })
            .collect();
        if receivers.is_empty() {
            continue;
        }
        let mut d = Diagnostic::info(
            "lint.clock_termination",
            format!("clock `{name}` from {driver} drives {} directly, with no series termination", receivers.len()),
        )
        .with_subject(ObjectRef::Net(name.clone()))
        .with_subject(pin_ref(driver))
        .with_hint(format!(
            "fast clock edges ring on unterminated lines: consider a series resistor (22 to 33 Ω) next to {driver}, \
             splitting the net, or ignore this for short, slow lines"
        ));
        for r in receivers {
            d = d.with_subject(pin_ref(r));
        }
        out.push(d);
    }
}

fn floating(ctx: &Ctx, out: &mut Vec<Diagnostic>) {
    let c = ctx.p.circuit();
    for (name, net) in &c.nets {
        if net.driven {
            continue;
        }
        let inputs: Vec<&PinRef> =
            net.pins.iter().filter(|pr| ctx.pins.get(pr).is_some_and(|(_, _, k)| *k == PinKind::Input)).collect();
        if inputs.is_empty() {
            continue;
        }
        let only_caps = net.pins.iter().all(|pr| {
            ctx.pins.get(pr).is_some_and(|(part, _, k)| *k == PinKind::Input || part.category == Category::Capacitor)
        });
        if only_caps && net.pins.len() > inputs.len() {
            let mut d = Diagnostic::warning(
                "lint.floating_input",
                format!("input(s) on net `{name}` see only capacitors: no DC level is defined"),
            )
            .with_subject(ObjectRef::Net(name.clone()))
            .with_hint("add a pull-up or pull-down resistor, or connect a driver");
            for pr in inputs {
                d = d.with_subject(pin_ref(pr));
            }
            out.push(d);
        }
    }
    let index = c.pin_index();
    let mut open: Vec<&PinRef> = ctx
        .pins
        .iter()
        .filter(|(pr, (part, _, k))| {
            *k == PinKind::Input && ic_like(part.category) && !index.contains_key(pr) && !c.no_connect.contains(pr)
        })
        .map(|(pr, _)| pr)
        .collect();
    open.sort_by(|a, b| natural_cmp(&a.refdes, &b.refdes).then_with(|| natural_cmp(&a.pin, &b.pin)));
    for pr in open {
        out.push(
            Diagnostic::warning(
                "lint.floating_input",
                format!("input {pr} ({}) is not connected: a floating CMOS input", ctx.pins[pr].1),
            )
            .with_subject(pin_ref(pr))
            .with_hint(
                "tie it to a defined level (supply or ground, directly or through a resistor, per the datasheet), \
                 or mark it `net.no_connect` if the part allows it to float",
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(i2c_line("SDA") && i2c_line("I2C1_SCL") && i2c_line("PB7/SDA") && i2c_line("SDA0"));
        assert!(!i2c_line("SCLK") && !i2c_line("SDAT") && !i2c_line("MOSI"));
        assert!(usb_line("USB_D+") && usb_line("D-") && usb_line("USB_DP") && usb_line("usb_dm"));
        assert!(!usb_line("DATA") && !usb_line("VBUS"));
        assert!(clock_name("MCLK") && clock_name("CLK_OUT") && clock_name("SYSCLK"));
        assert!(!clock_name("SCL") && !clock_name("LOCK"));
    }
}
