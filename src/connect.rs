//! Connectivity editing: resolving pin expressions, bus names, connecting and merging nets.
//!
//! Pin expressions:
//! - `U1.4`: pin number 4
//! - `U1.VIN`: pin(s) named VIN (all of them: `U1.GND` connects every GND pin)
//! - `U1.PA0..PA7`, `U1.1..8`, `J1.8..1`: a range, in the order written
//!
//! Net names: `VBUS`, `/usb/D+`; a bus `DATA[0..7]` expands to `DATA0` … `DATA7`.

use crate::model::Project;
use crate::model::circuit::{Circuit, Net, PinRef};
use crate::model::part::Part;
use crate::suggest::did_you_mean;

/// Why a connectivity edit failed. Codes are stable diagnostic codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectError {
    /// Diagnostic code.
    pub code: &'static str,
    /// Message.
    pub message: String,
    /// How to fix it.
    pub hint: Option<String>,
}

fn e(code: &'static str, message: impl Into<String>) -> ConnectError {
    ConnectError {
        code,
        message: message.into(),
        hint: None,
    }
}

impl ConnectError {
    fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }
}

/// Splits `PA12` into (`PA`, 12); `7` into (``, 7).
fn split_num(s: &str) -> Option<(&str, u32)> {
    let digits = s.len() - s.trim_start_matches(|c: char| !c.is_ascii_digit()).len();
    let (prefix, num) = s.split_at(digits);
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((prefix, num.parse().ok()?))
}

/// Expands `A..B` with a common prefix and numeric suffixes: `PA0..PA3`, `1..4`, `8..5`.
fn expand_range(a: &str, b: &str) -> Option<Vec<String>> {
    let (pa, na) = split_num(a)?;
    let (pb, nb) = split_num(b)?;
    // `PA0..3` is accepted as `PA0..PA3`.
    if pa != pb && !pb.is_empty() {
        return None;
    }
    let n = na.abs_diff(nb) + 1;
    if n > 1024 {
        return None;
    }
    Some(
        (0..n)
            .map(|i| {
                let k = if nb >= na { na + i } else { na - i };
                format!("{pa}{k}")
            })
            .collect(),
    )
}

/// Expands a net name: `DATA[0..7]` → `DATA0`…`DATA7`; anything else is itself.
pub fn expand_net(name: &str) -> Result<Vec<String>, ConnectError> {
    let name = name.trim();
    if name.is_empty() || name.contains(char::is_whitespace) || name.contains(',') {
        return Err(
            e("net.invalid_name", format!("invalid net name `{name}`")).hint("net names have no spaces or commas")
        );
    }
    if let Some(open) = name.find('[')
        && let Some(inner) = name[open + 1..].strip_suffix(']')
    {
        let (a, b) = inner
            .split_once("..")
            .ok_or_else(|| e("net.invalid_bus", format!("`{name}`: write buses as NAME[0..7]")))?;
        let (pa, na) = (a.trim().parse::<u32>(), b.trim().parse::<u32>());
        let (Ok(na), Ok(nb)) = (pa, na) else {
            return Err(e("net.invalid_bus", format!("`{name}`: bus indices must be numbers")));
        };
        let base = &name[..open];
        let n = na.abs_diff(nb) + 1;
        if n > 1024 {
            return Err(e("net.invalid_bus", format!("`{name}`: bus too wide")));
        }
        return Ok((0..n)
            .map(|i| format!("{base}{}", if nb >= na { na + i } else { na - i }))
            .collect());
    }
    if name.contains(['[', ']']) || name.contains("..") {
        return Err(e("net.invalid_name", format!("invalid net name `{name}`")).hint("buses are written NAME[0..7]"));
    }
    Ok(vec![name.to_string()])
}

fn part_of<'a>(p: &'a Project, refdes: &str) -> Result<(&'a str, &'a Part), ConnectError> {
    let c = &p.circuit().components;
    let (key, comp) = match c.get_key_value(refdes) {
        Some(kv) => kv,
        None => c.iter().find(|(k, _)| k.eq_ignore_ascii_case(refdes)).ok_or_else(|| {
            let s = did_you_mean(refdes, c.keys().map(String::as_str), 3);
            let mut err = e("component.not_found", format!("no component `{refdes}`"));
            if let Some(first) = s.first() {
                err = err.hint(format!("did you mean `{first}`?"));
            }
            err
        })?,
    };
    let part = p.library().parts.get(&comp.part).ok_or_else(|| {
        e(
            "part.not_found",
            format!("{key} uses part `{}`, which is missing from the library", comp.part),
        )
    })?;
    Ok((key.as_str(), part))
}

/// Pins of `part` matching `key`: by number, else by name (case-sensitive, then insensitive).
fn match_pins(part: &Part, key: &str) -> Vec<String> {
    if let Some(p) = part.symbol.pins.iter().find(|p| p.number == key) {
        return vec![p.number.clone()];
    }
    let by_name: Vec<String> = part
        .symbol
        .pins
        .iter()
        .filter(|p| p.name == key)
        .map(|p| p.number.clone())
        .collect();
    if !by_name.is_empty() {
        return by_name;
    }
    part.symbol
        .pins
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case(key))
        .map(|p| p.number.clone())
        .collect()
}

/// Resolves a pin expression to pins, in order.
pub fn resolve_pins(p: &Project, expr: &str) -> Result<Vec<PinRef>, ConnectError> {
    let expr = expr.trim();
    let (refdes, pin) = expr
        .split_once('.')
        .filter(|(r, q)| !r.is_empty() && !q.is_empty())
        .ok_or_else(|| {
            e(
                "pin.invalid",
                format!("`{expr}` is not a pin (write REFDES.PIN, e.g. U1.4 or U1.VIN)"),
            )
        })?;
    let (key, part) = part_of(p, refdes)?;
    let keys: Vec<String> = match pin.split_once("..") {
        Some((a, b)) => expand_range(a, b).ok_or_else(|| {
            e(
                "pin.invalid_range",
                format!("`{expr}`: ranges look like U1.PA0..PA7 or U1.1..8"),
            )
        })?,
        None => vec![pin.to_string()],
    };
    let mut out = Vec::new();
    for k in keys {
        let found = match_pins(part, &k);
        if found.is_empty() {
            let names = part
                .symbol
                .pins
                .iter()
                .flat_map(|p| [p.number.as_str(), p.name.as_str()])
                .filter(|s| !s.is_empty());
            let s = did_you_mean(&k, names, 3);
            let mut err = e("pin.not_found", format!("{key} ({}) has no pin `{k}`", part.id));
            err = match s.first() {
                Some(f) => err.hint(format!(
                    "did you mean `{key}.{f}`? `part.show {}` lists the pins",
                    part.id
                )),
                None => err.hint(format!("`part.show {}` lists the pins", part.id)),
            };
            return Err(err);
        }
        out.extend(found.into_iter().map(|n| PinRef::new(key, n)));
    }
    Ok(out)
}

/// What a connect did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConnectReport {
    /// Nets created.
    pub created: Vec<String>,
    /// Nets merged away (into the target).
    pub merged: Vec<String>,
    /// Pins newly added to nets.
    pub added: usize,
    /// Pins whose no-connect mark was removed.
    pub unmarked_nc: Vec<PinRef>,
}

/// Connects `pins` to `net`. A pin already on another net makes the two nets one: allowed only
/// with `merge` (the other net is merged into `net`).
pub fn connect(
    p: &mut Project,
    net: &str,
    pins: &[PinRef],
    merge: bool,
    report: &mut ConnectReport,
) -> Result<(), ConnectError> {
    // Check first, then mutate (the command layer also rolls back on error).
    let index: Vec<(PinRef, String)> = pins
        .iter()
        .filter_map(|pin| p.circuit().net_of(pin).map(|n| (pin.clone(), n.to_string())))
        .collect();
    let others: Vec<&(PinRef, String)> = index.iter().filter(|(_, n)| n != net).collect();
    if !merge && let Some((pin, other)) = others.first() {
        return Err(e(
            "net.would_merge",
            format!("{pin} is already on net `{other}`; connecting it to `{net}` would merge the nets"),
        )
        .hint(format!(
            "pass `merge: true` to merge `{other}` into `{net}`, or `net.disconnect {pin}` first"
        )));
    }
    if !p.circuit().nets.contains_key(net) {
        let id = p.alloc_id();
        p.circuit_mut().nets.insert(
            net.to_string(),
            Net {
                id,
                pins: Default::default(),
                class: None,
                driven: false,
            },
        );
        report.created.push(net.to_string());
    }
    let c: &mut Circuit = p.circuit_mut();
    for (_, other) in others {
        if let Some(o) = c.nets.remove(other) {
            let target = c.nets.get_mut(net).expect("created above");
            target.pins.extend(o.pins);
            target.driven |= o.driven;
            if target.class.is_none() {
                target.class = o.class;
            }
            if !report.merged.contains(other) {
                report.merged.push(other.clone());
            }
        }
    }
    let target = c.nets.get_mut(net).expect("created above");
    for pin in pins {
        if target.pins.insert(pin.clone()) {
            report.added += 1;
        }
    }
    for pin in pins {
        if c.no_connect.remove(pin) {
            report.unmarked_nc.push(pin.clone());
        }
    }
    Ok(())
}

/// Removes pins from their nets; nets left empty are deleted. Returns the nets touched.
pub fn disconnect(c: &mut Circuit, pins: &[PinRef]) -> Vec<String> {
    let mut touched = Vec::new();
    for (name, n) in c.nets.iter_mut() {
        let before = n.pins.len();
        n.pins.retain(|p| !pins.contains(p));
        if n.pins.len() != before {
            touched.push(name.clone());
        }
    }
    c.nets.retain(|_, n| !n.pins.is_empty());
    touched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(expand_range("PA0", "PA3").unwrap(), ["PA0", "PA1", "PA2", "PA3"]);
        assert_eq!(expand_range("1", "3").unwrap(), ["1", "2", "3"]);
        assert_eq!(expand_range("8", "6").unwrap(), ["8", "7", "6"]);
        assert_eq!(expand_range("PA0", "2").unwrap(), ["PA0", "PA1", "PA2"]);
        assert!(expand_range("PA0", "PB2").is_none());
        assert!(expand_range("X", "Y").is_none());
    }

    #[test]
    fn buses() {
        assert_eq!(expand_net("DATA[0..3]").unwrap(), ["DATA0", "DATA1", "DATA2", "DATA3"]);
        assert_eq!(expand_net("A[2..0]").unwrap(), ["A2", "A1", "A0"]);
        assert_eq!(expand_net("/usb/D+").unwrap(), ["/usb/D+"]);
        assert!(expand_net("bad name").is_err());
        assert!(expand_net("X[a..b]").is_err());
        assert!(expand_net("X..Y").is_err());
    }
}
