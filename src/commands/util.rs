//! Lookups shared by commands, with "did you mean" errors.

use crate::command::{CommandError, Context};
use crate::model::Project;
use crate::model::footprint::Footprint;
use crate::model::part::Part;
use crate::model::sections::Component;
use crate::refs::ObjectRef;
use crate::suggest::did_you_mean;
use crate::userlib::ItemKind;

/// Finds a part by ID (exact, then case-insensitive), or by MPN.
pub(crate) fn part<'a>(p: &'a Project, id: &str) -> Result<&'a Part, CommandError> {
    let lib = p.library();
    let id = id.strip_prefix("local:").or_else(|| id.strip_prefix("lib:")).unwrap_or(id);
    if let Some(part) = lib.parts.get(id) {
        return Ok(part);
    }
    if let Some(k) = lib.find_part_id_ci(id) {
        return Ok(&lib.parts[k]);
    }
    let mpn = id.strip_prefix("mpn:").unwrap_or(id);
    if let Some(part) = lib.parts.values().find(|p| p.mpn.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(mpn))) {
        return Ok(part);
    }
    let s = did_you_mean(id, lib.parts.keys().map(String::as_str), 3);
    Err(CommandError::not_found("part.not_found", format!("no part `{id}` in the project library"))
        .with_subject(ObjectRef::Part { scheme: "local".into(), id: id.into() })
        .with_suggestions(&s)
        .with_hint_if_none("list parts with `part.list`, or add one with `part.generic` / `part.create`"))
}

/// Like [`part`] on the open project; when the part is missing there but exists in a shared
/// library, the error's hint says how to import it. Projects never pull from libraries
/// implicitly (DECISIONS D19).
pub(crate) fn part_in<'a>(ctx: &'a Context<'_>, id: &str) -> Result<&'a Part, CommandError> {
    part(ctx.project()?, id).map_err(|e| with_library_hint(ctx, e, id))
}

/// Adds an import hint to a `part.not_found` error when a shared library has the part (by ID
/// or MPN). Library errors are ignored here: the hint is best effort.
pub(crate) fn with_library_hint(ctx: &Context<'_>, e: CommandError, id: &str) -> CommandError {
    if e.diagnostic.code != "part.not_found" {
        return e;
    }
    let Ok(libs) = ctx.libraries() else { return e };
    let id = id.strip_prefix("local:").or_else(|| id.strip_prefix("lib:")).unwrap_or(id);
    let found = match libs.find(&[ItemKind::Part], id) {
        Ok(v) if !v.is_empty() => Some((v[0].0.name.clone(), v[0].2.clone())),
        _ => match libs.find_part_by_mpn(id.strip_prefix("mpn:").unwrap_or(id)) {
            Ok(Some((l, p))) => Some((l.name, p.id)),
            _ => None,
        },
    };
    match found {
        Some((lib, pid)) => {
            let lib_arg = if lib == crate::userlib::USER_LIBRARY { String::new() } else { format!(" --library {lib}") };
            e.with_hint(format!(
                "part `{pid}` is in the shared library `{lib}`: import it with `lib.import {pid}{lib_arg}` (projects never use shared libraries implicitly)"
            ))
        }
        None => e,
    }
}

/// Finds a footprint by name (exact, then case-insensitive).
pub(crate) fn footprint<'a>(p: &'a Project, name: &str) -> Result<&'a Footprint, CommandError> {
    let lib = p.library();
    if let Some(f) = lib.footprints.get(name) {
        return Ok(f);
    }
    if let Some(k) = lib.find_footprint_ci(name) {
        return Ok(&lib.footprints[k]);
    }
    let s = did_you_mean(name, lib.footprints.keys().map(String::as_str), 3);
    Err(CommandError::not_found("footprint.not_found", format!("no footprint `{name}` in the project library"))
        .with_suggestions(&s)
        .with_hint_if_none("generate one with `footprint.generate`"))
}

/// Finds a component by reference designator.
pub(crate) fn component<'a>(p: &'a Project, refdes: &str) -> Result<&'a Component, CommandError> {
    let c = &p.circuit().components;
    if let Some(comp) = c.get(refdes) {
        return Ok(comp);
    }
    if let Some((_, comp)) = c.iter().find(|(k, _)| k.eq_ignore_ascii_case(refdes)) {
        return Ok(comp);
    }
    let s = did_you_mean(refdes, c.keys().map(String::as_str), 3);
    Err(CommandError::not_found("component.not_found", format!("no component `{refdes}`"))
        .with_subject(ObjectRef::Name(refdes.into()))
        .with_suggestions(&s)
        .with_hint_if_none("list components with `circuit.list`"))
}

/// The canonical key of a component (fixes case).
pub(crate) fn refdes_key(p: &Project, refdes: &str) -> Result<String, CommandError> {
    let id = component(p, refdes)?.id;
    Ok(p.circuit().components.iter().find(|(_, c)| c.id == id).map(|(k, _)| k.clone()).expect("found above"))
}
