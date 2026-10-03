//! Electrical rule check.
//!
//! Per net, pins are classified by their electrical type ([`PinKind`]):
//!
//! | Rule | Severity |
//! |---|---|
//! | two or more push-pull/power outputs (`output`, `power_out`) on one net | error |
//! | push-pull output with open-collector or tri-state pins | warning |
//! | power inputs with no power output, unless the net is marked `driven` or they are all ground pins | error |
//! | inputs with nothing that can drive them, unless `driven` | warning |
//! | a pin of type `no_connect` connected to a net | warning |
//! | net with a single pin | warning |
//! | unconnected `power_in` pin | error; other unconnected pins: warning (unless marked no-connect) |
//! | references to missing components, parts or pins | error |

use std::collections::BTreeMap;

use crate::diag::Diagnostic;
use crate::model::Project;
use crate::model::circuit::PinRef;
use crate::model::part::{Category, PinKind};
use crate::refs::ObjectRef;

fn pin_ref(p: &PinRef) -> ObjectRef {
    ObjectRef::Pin {
        component: p.refdes.clone(),
        pin: p.pin.clone(),
    }
}

fn net_ref(n: &str) -> ObjectRef {
    ObjectRef::Net(n.to_string())
}

fn list(pins: &[&PinRef]) -> String {
    pins.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

/// Runs every rule; diagnostics are ordered by net, then by component.
pub fn check(p: &Project) -> Vec<Diagnostic> {
    let c = p.circuit();
    let lib = p.library();
    let mut out = Vec::new();

    // Kind of every pin in nets; stale references reported.
    let kind_of = |pin: &PinRef| -> Result<(PinKind, Category), Box<Diagnostic>> {
        let comp = c.components.get(&pin.refdes).ok_or_else(|| {
            Box::new(
                Diagnostic::error(
                    "erc.missing_component",
                    format!("{pin} refers to a component that does not exist"),
                )
                .with_subject(pin_ref(pin))
                .with_hint("remove it with `net.disconnect`"),
            )
        })?;
        let part = lib.parts.get(&comp.part).ok_or_else(|| {
            Box::new(
                Diagnostic::error(
                    "erc.missing_part",
                    format!("{} uses part `{}`, missing from the library", pin.refdes, comp.part),
                )
                .with_subject(ObjectRef::Name(pin.refdes.clone())),
            )
        })?;
        let sp = part.symbol.pins.iter().find(|s| s.number == pin.pin).ok_or_else(|| {
            Box::new(
                Diagnostic::error(
                    "erc.missing_pin",
                    format!("{pin}: part `{}` has no pin {}", part.id, pin.pin),
                )
                .with_subject(pin_ref(pin))
                .with_hint("the part changed (e.g. `bom.replace`); reconnect the net to an existing pin"),
            )
        })?;
        Ok((sp.kind, part.category))
    };

    for (name, net) in &c.nets {
        let mut kinds: BTreeMap<PinKind, Vec<&PinRef>> = BTreeMap::new();
        let mut has_connector = false;
        for pin in &net.pins {
            match kind_of(pin) {
                Ok((k, cat)) => {
                    kinds.entry(k).or_default().push(pin);
                    has_connector |= cat == Category::Connector;
                }
                Err(d) => out.push(d.with_subject(net_ref(name))),
            }
        }
        let get = |k: PinKind| kinds.get(&k).map(Vec::as_slice).unwrap_or(&[]);
        let outputs: Vec<&PinRef> = get(PinKind::Output)
            .iter()
            .chain(get(PinKind::PowerOut))
            .copied()
            .collect();
        if outputs.len() >= 2 {
            let mut d = Diagnostic::error(
                "erc.output_conflict",
                format!("net `{name}` has several outputs driving it: {}", list(&outputs)),
            )
            .with_subject(net_ref(name))
            .with_hint("only one push-pull or power output may drive a net; check for a short or a wrong pin");
            for pin in &outputs {
                d = d.with_subject(pin_ref(pin));
            }
            out.push(d);
        }
        let weak: Vec<&PinRef> = get(PinKind::OpenCollector)
            .iter()
            .chain(get(PinKind::OpenEmitter))
            .chain(get(PinKind::TriState))
            .copied()
            .collect();
        if !get(PinKind::Output).is_empty() && !weak.is_empty() {
            out.push(
                Diagnostic::warning(
                    "erc.output_contention",
                    format!(
                        "net `{name}`: push-pull output {} shares the net with {}",
                        list(get(PinKind::Output)),
                        list(&weak)
                    ),
                )
                .with_subject(net_ref(name)),
            );
        }
        let power_in = get(PinKind::PowerIn);
        // Ground is the reference: power inputs that are all ground pins need no driver.
        let all_ground = !power_in.is_empty()
            && power_in.iter().all(|pin| {
                let name = c
                    .components
                    .get(&pin.refdes)
                    .and_then(|comp| lib.parts.get(&comp.part))
                    .and_then(|pt| pt.symbol.pins.iter().find(|s| s.number == pin.pin))
                    .map(|s| s.label().to_string())
                    .unwrap_or_default();
                crate::symbolgen::is_ground(&name)
            });
        if !power_in.is_empty() && !all_ground && get(PinKind::PowerOut).is_empty() && !net.driven {
            let hint = if has_connector {
                format!("if `{name}` comes from a connector or external supply, mark it: `net.set {name} --driven`")
            } else {
                format!(
                    "connect a power output (regulator, supply) to `{name}`, or mark it `--driven` if powered externally"
                )
            };
            let mut d = Diagnostic::error(
                "erc.power_not_driven",
                format!(
                    "power input(s) {} on net `{name}` are not driven by any power output",
                    list(power_in)
                ),
            )
            .with_subject(net_ref(name))
            .with_hint(hint);
            for pin in power_in {
                d = d.with_subject(pin_ref(pin));
            }
            out.push(d);
        }
        let inputs = get(PinKind::Input);
        let drivers = [
            PinKind::Output,
            PinKind::Bidirectional,
            PinKind::TriState,
            PinKind::PowerOut,
            PinKind::OpenCollector,
            PinKind::OpenEmitter,
            PinKind::Passive,
            PinKind::Unspecified,
            PinKind::PowerIn,
        ];
        if !inputs.is_empty() && !net.driven && drivers.iter().all(|k| get(*k).is_empty()) {
            out.push(
                Diagnostic::warning(
                    "erc.input_not_driven",
                    format!("input(s) {} on net `{name}` have no driver", list(inputs)),
                )
                .with_subject(net_ref(name))
                .with_hint("connect an output, a pull-up/down resistor, or mark the net `--driven`"),
            );
        }
        for pin in get(PinKind::NoConnect) {
            out.push(
                Diagnostic::warning(
                    "erc.nc_connected",
                    format!("{pin} is a no-connect pin but is on net `{name}`"),
                )
                .with_subject(pin_ref(pin))
                .with_subject(net_ref(name))
                .with_hint("check the datasheet; NC pins are usually left open"),
            );
        }
        if net.pins.len() == 1 && !net.driven {
            let pin = net.pins.iter().next().expect("one pin");
            out.push(
                Diagnostic::warning("erc.single_pin_net", format!("net `{name}` connects only {pin}"))
                    .with_subject(net_ref(name))
                    .with_hint("connect it to something, or remove the net and mark the pin no-connect"),
            );
        }
    }

    // Unconnected pins, by component in natural order.
    let index = c.pin_index();
    for refdes in c.refdes_sorted() {
        let comp = &c.components[refdes];
        let Some(part) = lib.parts.get(&comp.part) else {
            out.push(
                Diagnostic::error(
                    "erc.missing_part",
                    format!("{refdes} uses part `{}`, missing from the library", comp.part),
                )
                .with_subject(ObjectRef::Name(refdes.to_string())),
            );
            continue;
        };
        let mut floating = Vec::new();
        for sp in &part.symbol.pins {
            let pin = PinRef::new(refdes, &sp.number);
            if index.contains_key(&pin) || c.no_connect.contains(&pin) || sp.kind == PinKind::NoConnect {
                continue;
            }
            if sp.kind == PinKind::PowerIn {
                out.push(
                    Diagnostic::error(
                        "erc.power_unconnected",
                        format!("power pin {pin} ({}) is not connected", sp.label()),
                    )
                    .with_subject(pin_ref(&pin))
                    .with_hint("connect it to its supply or ground net"),
                );
            } else {
                floating.push((pin, sp.label().to_string(), sp.kind));
            }
        }
        if !floating.is_empty() {
            let names: Vec<String> = floating
                .iter()
                .map(|(p, l, _)| {
                    if l == &p.pin {
                        p.to_string()
                    } else {
                        format!("{p} ({l})")
                    }
                })
                .collect();
            let mut d = Diagnostic::warning(
                "erc.unconnected",
                format!("{refdes}: unconnected pin(s) {}", names.join(", ")),
            )
            .with_hint("connect them, or mark intentionally open pins with `net.no_connect`");
            for (p, _, _) in &floating {
                d = d.with_subject(pin_ref(p));
            }
            out.push(d);
        }
    }
    for pin in &c.no_connect {
        if !c.components.contains_key(&pin.refdes) {
            out.push(
                Diagnostic::warning(
                    "erc.stale_no_connect",
                    format!("no-connect mark on {pin}, which no longer exists"),
                )
                .with_subject(pin_ref(pin)),
            );
        }
    }
    out
}
