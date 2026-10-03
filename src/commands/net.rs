//! `net.*` and `netclass.*`: connectivity and routing classes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, ErrorKind, Registry};
use crate::connect::{self, ConnectError, ConnectReport};
use crate::diag::Diagnostic;
use crate::model::Project;
use crate::model::circuit::{NetClass, PinRef};
use crate::model::part::PinKind;
use crate::refs::ObjectRef;
use crate::suggest::did_you_mean;
use crate::units::Nm;

pub(crate) fn register(r: &mut Registry) {
    r.register::<Connect>()
        .register::<Disconnect>()
        .register::<NoConnect>()
        .register::<Rename>()
        .register::<Remove>()
        .register::<Set>()
        .register::<List>()
        .register::<Show>()
        .register::<ClassSet>()
        .register::<ClassList>()
        .register::<ClassRemove>();
}

impl From<ConnectError> for CommandError {
    fn from(e: ConnectError) -> Self {
        let kind = match e.code {
            "component.not_found" | "part.not_found" | "pin.not_found" => ErrorKind::NotFound,
            "net.would_merge" => ErrorKind::Conflict,
            _ => ErrorKind::InvalidArgs,
        };
        let mut err = CommandError::new(kind, e.code, e.message);
        if let Some(h) = e.hint {
            err = err.with_hint(h);
        }
        err
    }
}

/// Finds a net by name (exact, then case-insensitive; `net:` prefix allowed).
pub(crate) fn net_name(p: &Project, name: &str) -> Result<String, CommandError> {
    let name = name.strip_prefix("net:").unwrap_or(name);
    let nets = &p.circuit().nets;
    if nets.contains_key(name) {
        return Ok(name.to_string());
    }
    if let Some(k) = nets.keys().find(|k| k.eq_ignore_ascii_case(name)) {
        return Ok(k.clone());
    }
    let s = did_you_mean(name, nets.keys().map(String::as_str), 3);
    Err(CommandError::not_found("net.not_found", format!("no net `{name}`"))
        .with_subject(ObjectRef::Net(name.into()))
        .with_suggestions(&s)
        .with_hint_if_none("list nets with `net.list`"))
}

fn resolve_all(p: &Project, exprs: &[String]) -> Result<Vec<Vec<PinRef>>, CommandError> {
    exprs.iter().map(|e| connect::resolve_pins(p, e).map_err(Into::into)).collect()
}

/// A net with its pins, as returned by commands.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NetInfo {
    /// Name.
    pub name: String,
    /// Pins.
    pub pins: Vec<PinRef>,
    /// Net class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Externally powered.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub driven: bool,
}

fn info(p: &Project, name: &str) -> NetInfo {
    let n = &p.circuit().nets[name];
    NetInfo { name: name.to_string(), pins: n.pins.iter().cloned().collect(), class: n.class.clone(), driven: n.driven }
}

fn line(n: &NetInfo) -> String {
    let pins: Vec<String> = n.pins.iter().map(ToString::to_string).collect();
    format!(
        "{}{}{}: {}",
        n.name,
        n.class.as_ref().map(|c| format!(" [{c}]")).unwrap_or_default(),
        if n.driven { " [driven]" } else { "" },
        pins.join(" ")
    )
}

/// Connect pins to a net (created if needed). With a bus name (`DATA[0..7]`), each pin range is
/// spread over the bus in order.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Connect {
    /// Net name ("VBUS", "/usb/D+"), or a bus ("DATA[0..7]").
    pub net: String,
    /// Pins: "U1.4", "U1.VIN" (by name; all pins with that name), ranges "U1.PA0..PA7", "J1.1..8".
    pub pins: Vec<String>,
    /// Allow merging when a pin is already on another net (that net is merged into this one).
    #[serde(default)]
    pub merge: bool,
}

/// Result of `net.connect`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Connected {
    /// The affected nets after the change.
    pub nets: Vec<NetInfo>,
    /// Nets created.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<String>,
    /// Nets merged into the target.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub merged: Vec<String>,
}

impl Command for Connect {
    const NAME: &'static str = "net.connect";
    const SUMMARY: &'static str = "Connect pins to a net (or a bus: DATA[0..7] with U1.PA0..PA7)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["net", "pins"];
    type Output = Connected;

    fn run(self, ctx: &mut Context<'_>) -> Result<Connected, CommandError> {
        if self.pins.is_empty() {
            return Err(CommandError::invalid_args("net.no_pins", "give at least one pin"));
        }
        let names = connect::expand_net(self.net.strip_prefix("net:").unwrap_or(&self.net))?;
        let groups = resolve_all(ctx.project()?, &self.pins)?;
        let width = names.len();
        let mut per_net: Vec<Vec<PinRef>> = vec![Vec::new(); width];
        if width == 1 {
            per_net[0] = groups.into_iter().flatten().collect();
        } else {
            for (expr, g) in self.pins.iter().zip(&groups) {
                if g.len() != width {
                    return Err(CommandError::invalid_args(
                        "net.bus_width",
                        format!("`{expr}` gives {} pin(s) but bus `{}` has {width} nets", g.len(), self.net),
                    )
                    .with_hint("each pin range must have as many pins as the bus, e.g. DATA[0..7] with U1.PA0..PA7"));
                }
                for (i, pin) in g.iter().enumerate() {
                    per_net[i].push(pin.clone());
                }
            }
        }
        // A pin may appear in only one net of the command.
        let mut seen = std::collections::BTreeSet::new();
        for pin in per_net.iter().flatten() {
            if !seen.insert(pin.clone()) && width > 1 {
                return Err(CommandError::invalid_args(
                    "net.pin_twice",
                    format!("{pin} appears twice in a bus connection"),
                ));
            }
        }
        let mut report = ConnectReport::default();
        for (name, pins) in names.iter().zip(&per_net) {
            connect::connect(ctx.project_mut()?, name, pins, self.merge, &mut report)?;
        }
        for pin in &report.unmarked_nc {
            ctx.report(
                Diagnostic::info("net.nc_cleared", format!("{pin} was marked no-connect; the mark was removed"))
                    .with_subject(ObjectRef::Pin { component: pin.refdes.clone(), pin: pin.pin.clone() }),
            );
        }
        let p = ctx.project()?;
        Ok(Connected {
            nets: names.iter().map(|n| info(p, n)).collect(),
            created: report.created,
            merged: report.merged,
        })
    }

    fn summarize(o: &Connected) -> String {
        let mut s: String = o.nets.iter().map(line).collect::<Vec<_>>().join("\n");
        if !o.merged.is_empty() {
            s += &format!("\nmerged: {}", o.merged.join(", "));
        }
        s
    }
}

/// Disconnect pins from their nets (empty nets are removed).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Disconnect {
    /// Pins, as for `net.connect`.
    pub pins: Vec<String>,
}

/// Nets touched.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Touched {
    /// Nets changed (or removed, if they no longer exist).
    pub nets: Vec<String>,
}

impl Command for Disconnect {
    const NAME: &'static str = "net.disconnect";
    const SUMMARY: &'static str = "Disconnect pins from their nets";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["pins"];
    type Output = Touched;

    fn run(self, ctx: &mut Context<'_>) -> Result<Touched, CommandError> {
        let pins: Vec<PinRef> = resolve_all(ctx.project()?, &self.pins)?.into_iter().flatten().collect();
        let nets = connect::disconnect(ctx.project_mut()?.circuit_mut(), &pins);
        Ok(Touched { nets })
    }

    fn summarize(o: &Touched) -> String {
        if o.nets.is_empty() {
            "no pin was connected".into()
        } else {
            format!("disconnected from {}", o.nets.join(", "))
        }
    }
}

/// Mark pins as intentionally unconnected (or clear the mark).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoConnect {
    /// Pins.
    pub pins: Vec<String>,
    /// Remove the marks instead.
    #[serde(default)]
    pub clear: bool,
}

/// Pins marked no-connect.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NoConnectList {
    /// All no-connect pins.
    pub no_connect: Vec<PinRef>,
}

impl Command for NoConnect {
    const NAME: &'static str = "net.no_connect";
    const SUMMARY: &'static str = "Mark pins as intentionally unconnected (clears ERC warnings)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["pins"];
    type Output = NoConnectList;

    fn run(self, ctx: &mut Context<'_>) -> Result<NoConnectList, CommandError> {
        let p = ctx.project()?;
        let pins: Vec<PinRef> = resolve_all(p, &self.pins)?.into_iter().flatten().collect();
        if !self.clear {
            for pin in &pins {
                if let Some(n) = p.circuit().net_of(pin) {
                    return Err(CommandError::conflict("net.pin_connected", format!("{pin} is on net `{n}`"))
                        .with_hint(format!("disconnect it first: `net.disconnect {pin}`")));
                }
            }
        }
        let c = ctx.project_mut()?.circuit_mut();
        for pin in pins {
            if self.clear {
                c.no_connect.remove(&pin);
            } else {
                c.no_connect.insert(pin);
            }
        }
        Ok(NoConnectList { no_connect: c.no_connect.iter().cloned().collect() })
    }

    fn summarize(o: &NoConnectList) -> String {
        let v: Vec<String> = o.no_connect.iter().map(ToString::to_string).collect();
        if v.is_empty() { "no pins marked no-connect".into() } else { format!("no-connect: {}", v.join(" ")) }
    }
}

/// Rename a net.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rename {
    /// Current name.
    pub from: String,
    /// New name.
    pub to: String,
    /// If `to` exists, merge into it.
    #[serde(default)]
    pub merge: bool,
}

impl Command for Rename {
    const NAME: &'static str = "net.rename";
    const SUMMARY: &'static str = "Rename a net (or merge it into another)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["from", "to"];
    type Output = NetInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<NetInfo, CommandError> {
        let from = net_name(ctx.project()?, &self.from)?;
        let to = connect::expand_net(&self.to)?;
        let [to] = to.as_slice() else {
            return Err(CommandError::invalid_args("net.invalid_name", "rename to a single net, not a bus"));
        };
        if from == *to {
            return Ok(info(ctx.project()?, &from));
        }
        let exists = ctx.project()?.circuit().nets.contains_key(to);
        if exists && !self.merge {
            return Err(CommandError::conflict("net.exists", format!("net `{to}` already exists"))
                .with_hint("pass `merge: true` to merge the two nets"));
        }
        let c = ctx.project_mut()?.circuit_mut();
        let net = c.nets.remove(&from).expect("found above");
        match c.nets.get_mut(to) {
            Some(t) => {
                t.pins.extend(net.pins);
                t.driven |= net.driven;
                if t.class.is_none() {
                    t.class = net.class;
                }
            }
            None => {
                c.nets.insert(to.clone(), net);
            }
        }
        Ok(info(ctx.project()?, to))
    }

    fn summarize(o: &NetInfo) -> String {
        line(o)
    }
}

/// Delete nets (their pins become unconnected).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Remove {
    /// Net names.
    pub nets: Vec<String>,
}

impl Command for Remove {
    const NAME: &'static str = "net.remove";
    const SUMMARY: &'static str = "Delete nets (their pins become unconnected)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["nets"];
    type Output = Touched;

    fn run(self, ctx: &mut Context<'_>) -> Result<Touched, CommandError> {
        let mut names = Vec::new();
        for n in &self.nets {
            names.push(net_name(ctx.project()?, n)?);
        }
        let c = ctx.project_mut()?.circuit_mut();
        for n in &names {
            c.nets.remove(n);
        }
        Ok(Touched { nets: names })
    }

    fn summarize(o: &Touched) -> String {
        format!("removed {}", o.nets.join(", "))
    }
}

/// Set a net's class or mark it as externally driven.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Set {
    /// Net names.
    pub nets: Vec<String>,
    /// Net class name (empty string clears).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Powered from outside the circuit (connector, external supply); satisfies ERC power checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driven: Option<bool>,
}

/// Nets after the change.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NetList {
    /// Nets.
    pub nets: Vec<NetInfo>,
}

impl Command for Set {
    const NAME: &'static str = "net.set";
    const SUMMARY: &'static str = "Set nets' class, or mark them driven (externally powered)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["nets"];
    type Output = NetList;

    fn run(self, ctx: &mut Context<'_>) -> Result<NetList, CommandError> {
        let p = ctx.project()?;
        let mut names = Vec::new();
        for n in &self.nets {
            names.push(net_name(p, n)?);
        }
        if let Some(cl) = self.class.as_deref().filter(|c| !c.is_empty())
            && !p.circuit().netclasses.contains_key(cl)
        {
            let s = did_you_mean(cl, p.circuit().netclasses.keys().map(String::as_str), 3);
            return Err(CommandError::not_found("netclass.not_found", format!("no net class `{cl}`"))
                .with_suggestions(&s)
                .with_hint_if_none("create it with `netclass.set`"));
        }
        let c = ctx.project_mut()?.circuit_mut();
        for n in &names {
            let net = c.nets.get_mut(n).expect("found above");
            if let Some(cl) = &self.class {
                net.class = (!cl.is_empty()).then(|| cl.clone());
            }
            if let Some(d) = self.driven {
                net.driven = d;
            }
        }
        let p = ctx.project()?;
        Ok(NetList { nets: names.iter().map(|n| info(p, n)).collect() })
    }

    fn summarize(o: &NetList) -> String {
        o.nets.iter().map(line).collect::<Vec<_>>().join("\n")
    }
}

/// List nets.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {
    /// Only nets whose name contains this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

impl Command for List {
    const NAME: &'static str = "net.list";
    const SUMMARY: &'static str = "List nets with their pins";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["query"];
    type Output = NetList;

    fn run(self, ctx: &mut Context<'_>) -> Result<NetList, CommandError> {
        let p = ctx.project()?;
        let q = self.query.as_deref().map(str::to_lowercase);
        let nets = p
            .circuit()
            .nets
            .keys()
            .filter(|n| q.as_ref().is_none_or(|q| n.to_lowercase().contains(q)))
            .map(|n| info(p, n))
            .collect();
        Ok(NetList { nets })
    }

    fn summarize(o: &NetList) -> String {
        if o.nets.is_empty() { "no nets".into() } else { o.nets.iter().map(line).collect::<Vec<_>>().join("\n") }
    }
}

/// Show a net: every pin with its name and electrical type.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Show {
    /// Net name.
    pub net: String,
}

/// A pin on a net, with details.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PinDetail {
    /// Pin.
    pub pin: PinRef,
    /// Pin name.
    pub name: String,
    /// Electrical type.
    pub kind: PinKind,
    /// Part of the component.
    pub part: String,
}

/// Net details.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NetDetail {
    /// Name.
    pub name: String,
    /// Class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Externally driven.
    pub driven: bool,
    /// Pins.
    pub pins: Vec<PinDetail>,
}

impl Command for Show {
    const NAME: &'static str = "net.show";
    const SUMMARY: &'static str = "Show a net: pins with names and electrical types";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["net"];
    type Output = NetDetail;

    fn run(self, ctx: &mut Context<'_>) -> Result<NetDetail, CommandError> {
        let p = ctx.project()?;
        let name = net_name(p, &self.net)?;
        let net = &p.circuit().nets[&name];
        let pins = net
            .pins
            .iter()
            .map(|pin| {
                let part_id = p.circuit().components.get(&pin.refdes).map(|c| c.part.clone()).unwrap_or_default();
                let sp =
                    p.library().parts.get(&part_id).and_then(|pt| pt.symbol.pins.iter().find(|s| s.number == pin.pin));
                PinDetail {
                    pin: pin.clone(),
                    name: sp.map(|s| s.name.clone()).unwrap_or_default(),
                    kind: sp.map_or(PinKind::Unspecified, |s| s.kind),
                    part: part_id,
                }
            })
            .collect();
        Ok(NetDetail { name, class: net.class.clone(), driven: net.driven, pins })
    }

    fn summarize(o: &NetDetail) -> String {
        let mut s = format!(
            "{}{}{}",
            o.name,
            o.class.as_ref().map(|c| format!(" [{c}]")).unwrap_or_default(),
            if o.driven { " [driven]" } else { "" }
        );
        for p in &o.pins {
            s += &format!("\n  {:<10} {:<10} {:?}  ({})", p.pin.to_string(), p.name, p.kind, p.part);
        }
        s
    }
}

/// Create or update a net class. Only given fields change.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassSet {
    /// Class name ("power", "usb", ...).
    pub name: String,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Track width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_width: Option<Nm>,
    /// Clearance to other nets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clearance: Option<Nm>,
    /// Via drill.
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

/// A net class and its nets.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClassInfo {
    /// Name.
    pub name: String,
    /// Rules.
    #[serde(flatten)]
    pub class: NetClass,
    /// Nets using it.
    pub nets: Vec<String>,
}

fn class_info(p: &Project, name: &str) -> ClassInfo {
    ClassInfo {
        name: name.to_string(),
        class: p.circuit().netclasses[name].clone(),
        nets: p
            .circuit()
            .nets
            .iter()
            .filter(|(_, n)| n.class.as_deref() == Some(name))
            .map(|(k, _)| k.clone())
            .collect(),
    }
}

fn class_line(c: &ClassInfo) -> String {
    let k = &c.class;
    let mut parts = Vec::new();
    for (label, v) in [
        ("track", k.track_width),
        ("clearance", k.clearance),
        ("via", k.via_diameter),
        ("drill", k.via_drill),
        ("dp width", k.diff_pair_width),
        ("dp gap", k.diff_pair_gap),
    ] {
        if let Some(v) = v {
            parts.push(format!("{label} {v}"));
        }
    }
    format!(
        "{}: {}{}",
        c.name,
        parts.join(", "),
        if c.nets.is_empty() { String::new() } else { format!("  nets: {}", c.nets.join(", ")) }
    )
}

impl Command for ClassSet {
    const NAME: &'static str = "netclass.set";
    const SUMMARY: &'static str = "Create or update a net class (track width, clearance, vias, diff pairs)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = ClassInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<ClassInfo, CommandError> {
        let name = self.name.trim().to_string();
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(CommandError::invalid_args("netclass.invalid_name", "class names have no spaces"));
        }
        for (label, v) in [
            ("track_width", self.track_width),
            ("clearance", self.clearance),
            ("via_drill", self.via_drill),
            ("via_diameter", self.via_diameter),
            ("diff_pair_width", self.diff_pair_width),
            ("diff_pair_gap", self.diff_pair_gap),
        ] {
            if v.is_some_and(|v| v <= Nm::ZERO) {
                return Err(CommandError::invalid_args(
                    "netclass.invalid_value",
                    format!("`{label}` must be positive"),
                ));
            }
        }
        if let (Some(d), Some(v)) = (self.via_drill, self.via_diameter)
            && d >= v
        {
            return Err(CommandError::invalid_args(
                "netclass.invalid_value",
                "via_diameter must be larger than via_drill",
            ));
        }
        let c = ctx.project_mut()?.circuit_mut().netclasses.entry(name.clone()).or_default();
        if self.description.is_some() {
            c.description = self.description.filter(|d| !d.is_empty());
        }
        for (field, v) in [
            (&mut c.track_width, self.track_width),
            (&mut c.clearance, self.clearance),
            (&mut c.via_drill, self.via_drill),
            (&mut c.via_diameter, self.via_diameter),
            (&mut c.diff_pair_width, self.diff_pair_width),
            (&mut c.diff_pair_gap, self.diff_pair_gap),
        ] {
            if v.is_some() {
                *field = v;
            }
        }
        Ok(class_info(ctx.project()?, &name))
    }

    fn summarize(o: &ClassInfo) -> String {
        class_line(o)
    }
}

/// List net classes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassList {}

/// Net classes.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClassListResult {
    /// Classes.
    pub classes: Vec<ClassInfo>,
}

impl Command for ClassList {
    const NAME: &'static str = "netclass.list";
    const SUMMARY: &'static str = "List net classes and their nets";
    const KIND: CommandKind = CommandKind::Query;
    type Output = ClassListResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<ClassListResult, CommandError> {
        let p = ctx.project()?;
        Ok(ClassListResult { classes: p.circuit().netclasses.keys().map(|k| class_info(p, k)).collect() })
    }

    fn summarize(o: &ClassListResult) -> String {
        if o.classes.is_empty() {
            "no net classes".into()
        } else {
            o.classes.iter().map(class_line).collect::<Vec<_>>().join("\n")
        }
    }
}

/// Remove a net class; nets using it fall back to the defaults.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassRemove {
    /// Class name.
    pub name: String,
}

impl Command for ClassRemove {
    const NAME: &'static str = "netclass.remove";
    const SUMMARY: &'static str = "Remove a net class (its nets fall back to default rules)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Touched;

    fn run(self, ctx: &mut Context<'_>) -> Result<Touched, CommandError> {
        let p = ctx.project()?;
        if !p.circuit().netclasses.contains_key(&self.name) {
            let s = did_you_mean(&self.name, p.circuit().netclasses.keys().map(String::as_str), 3);
            return Err(CommandError::not_found("netclass.not_found", format!("no net class `{}`", self.name))
                .with_suggestions(&s));
        }
        let c = ctx.project_mut()?.circuit_mut();
        c.netclasses.remove(&self.name);
        let mut nets = Vec::new();
        for (k, n) in c.nets.iter_mut() {
            if n.class.as_deref() == Some(self.name.as_str()) {
                n.class = None;
                nets.push(k.clone());
            }
        }
        Ok(Touched { nets })
    }

    fn summarize(o: &Touched) -> String {
        if o.nets.is_empty() {
            "removed".into()
        } else {
            format!("removed; nets now on default rules: {}", o.nets.join(", "))
        }
    }
}
