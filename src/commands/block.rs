//! `block.*`: reusable subcircuits.
//!
//! A block is a template captured from components already in the circuit: their parts, the nets
//! between them, and the nets that leave the block (ports). Instantiating copies the components
//! with fresh designators, names internal nets `<instance>/<net>`, and connects ports to nets of
//! the circuit. The circuit stays one flat netlist (DECISIONS D18).

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util;
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::connect::{self, ConnectReport};
use crate::model::Project;
use crate::model::circuit::{Block, BlockComponent, Component, PinRef};
use crate::model::part::valid_id;
use crate::model::sections::natural_cmp;
use crate::suggest::did_you_mean;

pub(crate) fn register(r: &mut Registry) {
    r.register::<Create>().register::<Instantiate>().register::<List>().register::<Show>().register::<Remove>();
}

fn block_name(p: &Project, name: &str) -> Result<String, CommandError> {
    let b = &p.circuit().blocks;
    if b.contains_key(name) {
        return Ok(name.to_string());
    }
    let s = did_you_mean(name, b.keys().map(String::as_str), 3);
    Err(CommandError::not_found("block.not_found", format!("no block `{name}`"))
        .with_suggestions(&s)
        .with_hint_if_none("create one with `block.create` from existing components"))
}

/// Short description of a block.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BlockSummary {
    /// Name.
    pub name: String,
    /// Description.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Local designators.
    pub components: Vec<String>,
    /// Ports.
    pub ports: Vec<String>,
    /// Internal nets.
    pub internal_nets: Vec<String>,
    /// Instances in the circuit.
    pub instances: Vec<String>,
}

fn summary(p: &Project, name: &str) -> BlockSummary {
    let b = &p.circuit().blocks[name];
    let mut components: Vec<String> = b.components.keys().cloned().collect();
    components.sort_by(|a, b| natural_cmp(a, b));
    BlockSummary {
        name: name.to_string(),
        description: b.description.clone(),
        components,
        ports: b.ports.clone(),
        internal_nets: b.nets.keys().filter(|n| !b.ports.contains(n)).cloned().collect(),
        instances: p.circuit().instances.iter().filter(|(_, bl)| *bl == name).map(|(i, _)| i.clone()).collect(),
    }
}

fn line(b: &BlockSummary) -> String {
    format!(
        "{}: {} component(s) [{}], ports: {}{}{}",
        b.name,
        b.components.len(),
        b.components.join(", "),
        if b.ports.is_empty() { "-".into() } else { b.ports.join(", ") },
        if b.instances.is_empty() { String::new() } else { format!("; instances: {}", b.instances.join(", ")) },
        if b.description.is_empty() { String::new() } else { format!(" — {}", b.description) }
    )
}

/// Capture existing components as a reusable block.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Create {
    /// Block name ("usb_c_power", "ldo_3v3").
    pub name: String,
    /// Components to capture (their designators become the block's local designators).
    pub components: Vec<String>,
    /// Nets exposed as ports. Default: every net that also connects outside the captured components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<Vec<String>>,
    /// What the block does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Replace an existing block of the same name.
    #[serde(default)]
    pub replace: bool,
}

impl Command for Create {
    const NAME: &'static str = "block.create";
    const SUMMARY: &'static str = "Capture existing components and their nets as a reusable block";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name", "components"];
    type Output = BlockSummary;

    fn run(self, ctx: &mut Context<'_>) -> Result<BlockSummary, CommandError> {
        if !valid_id(&self.name) {
            return Err(CommandError::invalid_args(
                "block.invalid_name",
                format!("invalid block name `{}`", self.name),
            ));
        }
        let p = ctx.project()?;
        if p.circuit().blocks.contains_key(&self.name) && !self.replace {
            return Err(CommandError::conflict("block.exists", format!("block `{}` already exists", self.name))
                .with_hint("pass `replace: true` to overwrite it"));
        }
        if self.components.is_empty() {
            return Err(CommandError::invalid_args("block.empty", "give the components to capture"));
        }
        let mut set = BTreeSet::new();
        for r in &self.components {
            set.insert(util::refdes_key(p, r)?);
        }
        let c = p.circuit();
        let mut nets: BTreeMap<String, BTreeSet<PinRef>> = BTreeMap::new();
        let mut leaving = BTreeSet::new();
        for (name, net) in &c.nets {
            let inside: BTreeSet<PinRef> = net.pins.iter().filter(|pin| set.contains(&pin.refdes)).cloned().collect();
            if inside.is_empty() {
                continue;
            }
            if inside.len() != net.pins.len() || net.driven {
                leaving.insert(name.clone());
            }
            nets.insert(name.clone(), inside);
        }
        let ports: Vec<String> = match self.ports {
            Some(list) => {
                for port in &list {
                    if !nets.contains_key(port) {
                        let s = did_you_mean(port, nets.keys().map(String::as_str), 3);
                        return Err(CommandError::invalid_args(
                            "block.invalid_port",
                            format!("`{port}` is not a net of the captured components"),
                        )
                        .with_suggestions(&s));
                    }
                }
                list
            }
            None => leaving.into_iter().collect(),
        };
        let components = set
            .iter()
            .map(|r| {
                let comp = &c.components[r];
                (r.clone(), BlockComponent { part: comp.part.clone(), properties: comp.properties.clone() })
            })
            .collect();
        let block = Block {
            description: self.description.unwrap_or_default(),
            components,
            nets,
            ports,
            no_connect: c.no_connect.iter().filter(|pin| set.contains(&pin.refdes)).cloned().collect(),
        };
        let name = self.name.clone();
        ctx.project_mut()?.circuit_mut().blocks.insert(name.clone(), block);
        Ok(summary(ctx.project()?, &name))
    }

    fn summarize(o: &BlockSummary) -> String {
        format!("block {}", line(o))
    }
}

/// Add a copy of a block to the circuit.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Instantiate {
    /// Block name.
    pub block: String,
    /// Instance name; internal nets become `<instance>/<net>`.
    pub instance: String,
    /// Port → circuit net. Ports not listed connect to a net with the port's own name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connect: BTreeMap<String, String>,
}

/// Result of `block.instantiate`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Instance {
    /// Instance name.
    pub instance: String,
    /// Block.
    pub block: String,
    /// Local designator → new designator.
    pub components: BTreeMap<String, String>,
    /// Block net → circuit net.
    pub nets: BTreeMap<String, String>,
}

impl Command for Instantiate {
    const NAME: &'static str = "block.instantiate";
    const SUMMARY: &'static str = "Add a copy of a block: new designators, internal nets prefixed, ports connected";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["block", "instance"];
    type Output = Instance;

    fn run(self, ctx: &mut Context<'_>) -> Result<Instance, CommandError> {
        let p = ctx.project()?;
        let name = block_name(p, &self.block)?;
        let block = p.circuit().blocks[&name].clone();
        let inst = self.instance.trim().to_string();
        if inst.is_empty() || !inst.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(CommandError::invalid_args(
                "block.invalid_instance",
                format!("invalid instance name `{inst}` (letters, digits, `_`, `-`)"),
            ));
        }
        if p.circuit().instances.contains_key(&inst) {
            return Err(CommandError::conflict("block.instance_exists", format!("instance `{inst}` already exists")));
        }
        for port in self.connect.keys() {
            if !block.ports.contains(port) {
                let s = did_you_mean(port, block.ports.iter().map(String::as_str), 3);
                return Err(CommandError::invalid_args(
                    "block.invalid_port",
                    format!("block `{name}` has no port `{port}`"),
                )
                .with_suggestions(&s)
                .with_hint_if_none(format!("ports: {}", block.ports.join(", "))));
            }
        }
        // Net names: ports map to circuit nets, internal nets get the instance prefix.
        let mut net_map = BTreeMap::new();
        for local in block.nets.keys() {
            let target = if block.ports.contains(local) {
                let t = self.connect.get(local).cloned().unwrap_or_else(|| local.clone());
                let expanded = connect::expand_net(&t)?;
                let [single] = expanded.as_slice() else {
                    return Err(CommandError::invalid_args("block.invalid_port", "connect ports to single nets"));
                };
                single.clone()
            } else {
                let t = format!("{inst}/{local}");
                if p.circuit().nets.contains_key(&t) {
                    return Err(CommandError::conflict("net.exists", format!("net `{t}` already exists")));
                }
                t
            };
            net_map.insert(local.clone(), target);
        }
        for bc in block.components.values() {
            if !p.library().parts.contains_key(&bc.part) {
                return Err(CommandError::not_found(
                    "part.not_found",
                    format!("block part `{}` is not in the library", bc.part),
                ));
            }
        }

        // Components, in natural order of their local designators.
        let mut locals: Vec<&String> = block.components.keys().collect();
        locals.sort_by(|a, b| natural_cmp(a, b));
        let mut comp_map = BTreeMap::new();
        for local in locals {
            let bc = &block.components[local];
            let p = ctx.project_mut()?;
            let prefix = p.library().parts[&bc.part].category.refdes_prefix();
            let refdes = p.circuit().next_refdes(prefix);
            let id = p.alloc_id();
            p.circuit_mut().components.insert(
                refdes.clone(),
                Component { id, part: bc.part.clone(), block: Some(inst.clone()), properties: bc.properties.clone() },
            );
            comp_map.insert(local.clone(), refdes);
        }
        let map_pin = |pin: &PinRef| PinRef::new(comp_map[&pin.refdes].clone(), pin.pin.clone());
        let mut report = ConnectReport::default();
        for (local, pins) in &block.nets {
            let pins: Vec<PinRef> = pins.iter().map(map_pin).collect();
            connect::connect(ctx.project_mut()?, &net_map[local], &pins, false, &mut report)?;
        }
        let c = ctx.project_mut()?.circuit_mut();
        for pin in &block.no_connect {
            c.no_connect.insert(map_pin(pin));
        }
        c.instances.insert(inst.clone(), name.clone());
        Ok(Instance { instance: inst, block: name, components: comp_map, nets: net_map })
    }

    fn summarize(o: &Instance) -> String {
        let comps: Vec<String> = o.components.iter().map(|(l, g)| format!("{l}→{g}")).collect();
        let nets: Vec<String> = o.nets.iter().map(|(l, g)| format!("{l}→{g}")).collect();
        format!("instance {} of {}: {}; nets {}", o.instance, o.block, comps.join(", "), nets.join(", "))
    }
}

/// List blocks.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {}

/// Blocks.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BlockList {
    /// Blocks.
    pub blocks: Vec<BlockSummary>,
}

impl Command for List {
    const NAME: &'static str = "block.list";
    const SUMMARY: &'static str = "List blocks, their ports and instances";
    const KIND: CommandKind = CommandKind::Query;
    type Output = BlockList;

    fn run(self, ctx: &mut Context<'_>) -> Result<BlockList, CommandError> {
        let p = ctx.project()?;
        Ok(BlockList { blocks: p.circuit().blocks.keys().map(|k| summary(p, k)).collect() })
    }

    fn summarize(o: &BlockList) -> String {
        if o.blocks.is_empty() { "no blocks".into() } else { o.blocks.iter().map(line).collect::<Vec<_>>().join("\n") }
    }
}

/// Show a block in full.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Show {
    /// Block name.
    pub name: String,
}

/// A block's definition.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BlockDetail {
    /// Name.
    pub name: String,
    /// Definition.
    pub block: Block,
    /// Instances.
    pub instances: Vec<String>,
}

impl Command for Show {
    const NAME: &'static str = "block.show";
    const SUMMARY: &'static str = "Show a block: components, nets, ports";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = BlockDetail;

    fn run(self, ctx: &mut Context<'_>) -> Result<BlockDetail, CommandError> {
        let p = ctx.project()?;
        let name = block_name(p, &self.name)?;
        Ok(BlockDetail { instances: summary(p, &name).instances, block: p.circuit().blocks[&name].clone(), name })
    }

    fn summarize(o: &BlockDetail) -> String {
        let mut s = format!(
            "{}{}",
            o.name,
            if o.block.description.is_empty() { String::new() } else { format!(": {}", o.block.description) }
        );
        for (r, c) in &o.block.components {
            s += &format!("\n  {r}: {}", c.part);
        }
        for (n, pins) in &o.block.nets {
            let pins: Vec<String> = pins.iter().map(ToString::to_string).collect();
            s += &format!("\n  {}{n}: {}", if o.block.ports.contains(n) { "port " } else { "" }, pins.join(" "));
        }
        s
    }
}

/// Delete a block definition (its instances must be removed first).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Remove {
    /// Block name.
    pub name: String,
}

impl Command for Remove {
    const NAME: &'static str = "block.remove";
    const SUMMARY: &'static str = "Delete a block definition with no instances";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = BlockList;

    fn run(self, ctx: &mut Context<'_>) -> Result<BlockList, CommandError> {
        let p = ctx.project()?;
        let name = block_name(p, &self.name)?;
        let inst = summary(p, &name).instances;
        if !inst.is_empty() {
            return Err(CommandError::conflict(
                "block.in_use",
                format!("block `{name}` has instances: {}", inst.join(", ")),
            )
            .with_hint("remove the instances' components first (`circuit.remove`)"));
        }
        ctx.project_mut()?.circuit_mut().blocks.remove(&name);
        let p = ctx.project()?;
        Ok(BlockList { blocks: p.circuit().blocks.keys().map(|k| summary(p, k)).collect() })
    }

    fn summarize(o: &BlockList) -> String {
        format!("{} block(s) left", o.blocks.len())
    }
}
