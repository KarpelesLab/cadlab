//! `lib.*`: shared libraries of parts, footprints and blocks, outside any project.
//!
//! Projects stay self-contained: nothing is read from a shared library implicitly. Items are
//! copied in with `lib.import` and out with `lib.publish` (DECISIONS D19). `lib.list`, `lib.show`
//! and `lib.remove` work without a project. Writes to a library happen outside the project's
//! transaction: they are skipped in dry runs and are not undone by `history.undo`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::util;
use crate::command::{Command, CommandError, CommandKind, Context, ErrorKind, Registry};
use crate::model::Project;
use crate::model::format::to_canonical_string;
use crate::suggest::did_you_mean;
use crate::userlib::{Existing, Item, ItemKind, LibBlock, LibError, Libraries, UserLibrary, models_used};
use crate::{Diagnostic, ObjectRef};

pub(crate) fn register(r: &mut Registry) {
    r.register::<List>().register::<Show>().register::<Publish>().register::<Import>().register::<Remove>();
}

impl From<LibError> for CommandError {
    fn from(e: LibError) -> Self {
        let msg = e.to_string();
        match e {
            LibError::Io(..) => CommandError::new(ErrorKind::Io, "lib.io", msg),
            LibError::Invalid(..) => CommandError::new(ErrorKind::Io, "lib.invalid_file", msg)
                .with_hint("fix or delete the file, or restore it from version control"),
            LibError::NewerSchema { .. } => CommandError::new(ErrorKind::Io, "lib.newer_schema", msg),
            LibError::NoDataDir => CommandError::new(ErrorKind::Io, "lib.no_data_dir", msg)
                .with_hint("set XDG_DATA_HOME or HOME, or pass `library` as a directory path"),
            LibError::Settings(_) => CommandError::new(ErrorKind::Io, "config.invalid", msg)
                .with_hint("fix the settings file (`cadlab config path` shows where it is)"),
            LibError::InvalidName(..) => CommandError::invalid_args("lib.invalid_name", msg),
        }
    }
}

/// What happened to one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// It was not there before.
    Added,
    /// A different version was overwritten.
    Replaced,
    /// An identical copy was already there.
    Unchanged,
    /// Deleted.
    Removed,
}

/// One item touched by a command.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ItemChange {
    /// Kind.
    pub kind: ItemKind,
    /// ID or name.
    pub name: String,
    /// What happened.
    pub change: Change,
}

/// Result of `lib.publish`, `lib.import` and `lib.remove`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LibChanges {
    /// Library written to (publish, remove) or read from (import).
    pub library: String,
    /// Its directory.
    pub path: PathBuf,
    /// Items, the requested one first, then what it brought along.
    pub items: Vec<ItemChange>,
    /// Nothing was written (dry run).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
}

impl LibChanges {
    fn text(&self, verb: &str, prep: &str) -> String {
        let items: Vec<String> = self
            .items
            .iter()
            .map(|i| {
                let c = match i.change {
                    Change::Added => "added",
                    Change::Replaced => "replaced",
                    Change::Unchanged => "unchanged",
                    Change::Removed => "removed",
                };
                format!("{} {} ({c})", i.kind.label(), i.name)
            })
            .collect();
        format!(
            "{verb} {prep} `{}` ({}): {}{}",
            self.library,
            self.path.display(),
            items.join(", "),
            if self.dry_run { " [dry run, nothing written]" } else { "" }
        )
    }
}

fn change_of(e: &Existing) -> Change {
    match e {
        Existing::Absent => Change::Added,
        Existing::Same => Change::Unchanged,
        Existing::Different | Existing::CaseVariant(_) => Change::Replaced,
    }
}

/// The library named `spec` (a name or a directory path), or the first one (the user library).
pub(super) fn select(libs: &Libraries, spec: Option<&str>) -> Result<UserLibrary, CommandError> {
    match spec {
        None => libs.list.first().cloned().ok_or_else(|| LibError::NoDataDir.into()),
        Some(s) => libs.select(s).ok_or_else(|| {
            let names: Vec<&str> = libs.list.iter().map(|l| l.name.as_str()).collect();
            let sug = did_you_mean(s, names.iter().copied(), 3);
            CommandError::not_found("lib.unknown_library", format!("no library named `{s}`"))
                .with_suggestions(&sug)
                .with_hint_if_none(format!(
                    "libraries: {}; or pass a directory path (`./lib`, `/path/to/lib`)",
                    names.join(", ")
                ))
        }),
    }
}

fn kinds(kind: Option<ItemKind>) -> Vec<ItemKind> {
    kind.map(|k| vec![k]).unwrap_or_else(|| ItemKind::ALL.to_vec())
}

/// Finds an item by name (or a part by MPN) in one library or all, in search order.
fn locate(
    libs: &Libraries,
    name: &str,
    kind: Option<ItemKind>,
    library: Option<&UserLibrary>,
) -> Result<(UserLibrary, Item), CommandError> {
    let scope = match library {
        Some(l) => Libraries::new(vec![l.clone()]),
        None => libs.clone(),
    };
    let ks = kinds(kind);
    let found = scope.find(&ks, name)?;
    if let Some((lib, _, _)) = found.first() {
        let here: Vec<&(UserLibrary, ItemKind, String)> = found.iter().filter(|(l, _, _)| l == lib).collect();
        if here.len() > 1 {
            let what: Vec<&str> = here.iter().map(|(_, k, _)| k.label()).collect();
            return Err(CommandError::invalid_args(
                "lib.ambiguous",
                format!("`{name}` is a {} in library `{}`", what.join(" and a "), lib.name),
            )
            .with_hint("pass `kind` (part, footprint or block)"));
        }
        let (lib, k, stored) = here[0];
        let item = lib.read(*k, stored)?.ok_or_else(|| LibError::Invalid(lib.path.clone(), "item vanished".into()))?;
        return Ok((lib.clone(), item));
    }
    if ks.contains(&ItemKind::Part)
        && let Some((lib, p)) = scope.find_part_by_mpn(name.strip_prefix("mpn:").unwrap_or(name))?
    {
        return Ok((lib, Item::Part(p)));
    }
    let mut names = Vec::new();
    for l in &scope.list {
        for &k in &ks {
            names.extend(l.names(k)?);
        }
    }
    let sug = did_you_mean(name, names.iter().map(String::as_str), 3);
    let what = kind.map(ItemKind::label).unwrap_or("item");
    let wher = match library {
        Some(l) => format!("library `{}`", l.name),
        None => "the shared libraries".into(),
    };
    Err(CommandError::not_found("lib.not_found", format!("no {what} `{name}` in {wher}"))
        .with_suggestions(&sug)
        .with_hint_if_none("list library items with `lib.list`"))
}

/// List items in the shared libraries.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {
    /// Only items whose name, description, manufacturer or MPN contain this (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Only this kind: part, footprint or block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ItemKind>,
    /// Only this library (name, or directory path). Default: every library, in search order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
}

/// A library.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LibInfo {
    /// Name (`user`, or the directory name).
    pub name: String,
    /// Directory.
    pub path: PathBuf,
    /// Whether the directory exists yet (it is created on first publish).
    pub exists: bool,
}

/// A library item.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LibItem {
    /// Kind.
    pub kind: ItemKind,
    /// ID or name.
    pub name: String,
    /// Library it is in.
    pub library: String,
    /// One-line description.
    pub description: String,
}

/// Result of `lib.list`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LibList {
    /// Libraries searched, in order.
    pub libraries: Vec<LibInfo>,
    /// Matching items, by library, then kind, then name. An item in an earlier library shadows
    /// one with the same name in a later one for `lib.import`.
    pub items: Vec<LibItem>,
}

impl Command for List {
    const NAME: &'static str = "lib.list";
    const SUMMARY: &'static str = "List parts, footprints and blocks in the shared user libraries";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["query"];
    type Output = LibList;

    fn run(self, ctx: &mut Context<'_>) -> Result<LibList, CommandError> {
        let libs = ctx.libraries()?;
        let scope = match &self.library {
            Some(s) => vec![select(&libs, Some(s))?],
            None => libs.list.clone(),
        };
        let mut items = Vec::new();
        for l in &scope {
            l.check_schema()?;
            for k in kinds(self.kind) {
                for it in l.items(k)? {
                    if self.query.as_deref().is_none_or(|q| it.matches(q)) {
                        items.push(LibItem {
                            kind: k,
                            name: it.name().to_string(),
                            library: l.name.clone(),
                            description: it.description(),
                        });
                    }
                }
            }
        }
        let libraries =
            scope.iter().map(|l| LibInfo { name: l.name.clone(), path: l.path.clone(), exists: l.exists() }).collect();
        Ok(LibList { libraries, items })
    }

    fn summarize(o: &LibList) -> String {
        let libs: Vec<String> = o.libraries.iter().map(|l| format!("{} ({})", l.name, l.path.display())).collect();
        if o.items.is_empty() {
            return format!("no items; libraries: {}", libs.join(", "));
        }
        let mut s: Vec<String> = o
            .items
            .iter()
            .map(|i| format!("{} {}  [{}]  {}", i.kind.label(), i.name, i.library, i.description))
            .collect();
        s.push(format!("{} item(s); libraries: {}", o.items.len(), libs.join(", ")));
        s.join("\n")
    }
}

/// Show one item from the shared libraries in full.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Show {
    /// Part ID (or MPN), footprint name or block name.
    pub name: String,
    /// Kind, when the name is ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ItemKind>,
    /// Library (name or directory path). Default: the first library that has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
}

/// Result of `lib.show`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LibShow {
    /// Kind.
    pub kind: ItemKind,
    /// ID or name.
    pub name: String,
    /// Library it is in.
    pub library: String,
    /// The item, as stored (a part, a footprint, or a block with its parts and footprints).
    pub item: Value,
}

impl Command for Show {
    const NAME: &'static str = "lib.show";
    const SUMMARY: &'static str = "Show a shared library item in full";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = LibShow;

    fn run(self, ctx: &mut Context<'_>) -> Result<LibShow, CommandError> {
        let libs = ctx.libraries()?;
        let scope = self.library.as_deref().map(|s| select(&libs, Some(s))).transpose()?;
        let (lib, item) = locate(&libs, &self.name, self.kind, scope.as_ref())?;
        Ok(LibShow { kind: item.kind(), name: item.name().to_string(), library: lib.name, item: item.to_value() })
    }

    fn summarize(o: &LibShow) -> String {
        format!("{} {} [{}]\n{}", o.kind.label(), o.name, o.library, to_canonical_string(&o.item).trim_end())
    }
}

/// Copy a part, footprint or block from the project into a shared library.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Publish {
    /// Part ID (or MPN). Its footprints are published with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// Footprint name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
    /// Block name. The block file carries copies of the parts and footprints it uses, so it
    /// imports into any project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
    /// Target library (name or directory path). Default: the user library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
    /// Overwrite library items that differ from the project's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

/// The project items `Publish` sends: the requested item first.
fn publish_items(ctx: &mut Context<'_>, cmd: &Publish) -> Result<Vec<Item>, CommandError> {
    let p = ctx.project()?;
    let mut warnings = Vec::new();
    let items = match (&cmd.part, &cmd.footprint, &cmd.block) {
        (Some(id), None, None) => {
            let part = util::part(p, id)?.clone();
            let mut items = vec![];
            for fr in &part.footprints {
                match p.library().footprints.get(&fr.footprint) {
                    Some(f) => items.push(Item::Footprint(f.clone())),
                    None => warnings.push(missing_footprint(&part.id, &fr.footprint)),
                }
            }
            items.insert(0, Item::Part(part));
            items
        }
        (None, Some(name), None) => vec![Item::Footprint(util::footprint(p, name)?.clone())],
        (None, None, Some(name)) => {
            let (lb, missing) = LibBlock::from_project(p, name).ok_or_else(|| block_not_found(p, name))?;
            if let Some(m) = missing.iter().find(|m| m.starts_with("part ")) {
                return Err(CommandError::not_found(
                    "lib.incomplete_block",
                    format!("block `{name}` uses {m}, which is not in the project library"),
                )
                .with_hint("add the part back, or recreate the block with `block.create`"));
            }
            for m in &missing {
                if let Some(file) = m.strip_prefix("model ") {
                    warnings.push(missing_model(file));
                    continue;
                }
                warnings.push(
                    Diagnostic::warning("lib.footprint_missing", format!("block `{name}`: {m} is not in the project"))
                        .with_hint("generate it with `footprint.generate` and publish again"),
                );
            }
            vec![Item::Block(lb)]
        }
        _ => {
            return Err(CommandError::invalid_args(
                "lib.publish_what",
                "give exactly one of `part`, `footprint` or `block`",
            ));
        }
    };
    // 3D model files of the parts and footprints sent (blocks embed theirs).
    let parts = items.iter().filter_map(|i| if let Item::Part(p) = i { Some(p) } else { None });
    let fps = items.iter().filter_map(|i| if let Item::Footprint(f) = i { Some(f) } else { None });
    let mut models = Vec::new();
    for m in models_used(parts, fps) {
        match p.library().models.get(&m) {
            Some(d) => models.push(Item::Model(m, d.clone())),
            None => warnings.push(missing_model(&m)),
        }
    }
    let mut items = items;
    items.extend(models);
    for w in warnings {
        ctx.report(w);
    }
    Ok(items)
}

fn missing_model(file: &str) -> Diagnostic {
    Diagnostic::warning("lib.model_missing", format!("3D model `{file}` is referenced but missing"))
        .with_hint("attach the model again with `footprint.model_set`")
}

/// Finds model `file` in `first`, then the other libraries in search order.
fn find_model(libs: &Libraries, first: &UserLibrary, file: &str) -> Result<Option<Item>, CommandError> {
    for l in std::iter::once(first).chain(libs.list.iter().filter(|l| *l != first)) {
        if let Some(n) = l.resolve(ItemKind::Model, file)?
            && n == file
        {
            return Ok(l.read(ItemKind::Model, &n)?);
        }
    }
    Ok(None)
}

fn missing_footprint(part: &str, fp: &str) -> Diagnostic {
    Diagnostic::warning("lib.footprint_missing", format!("part `{part}` references footprint `{fp}`, which is missing"))
        .with_subject(ObjectRef::Part { scheme: "local".into(), id: part.into() })
        .with_hint("generate it with `footprint.generate`, then publish or import it")
}

fn block_not_found(p: &Project, name: &str) -> CommandError {
    let s = did_you_mean(name, p.circuit().blocks.keys().map(String::as_str), 3);
    CommandError::not_found("block.not_found", format!("no block `{name}`"))
        .with_suggestions(&s)
        .with_hint_if_none("create one with `block.create` from existing components")
}

fn conflict_error(list: &[String], what: &str, hint: &str) -> CommandError {
    CommandError::conflict("lib.conflict", format!("{what}: {}", list.join(", "))).with_hint(hint)
}

impl Command for Publish {
    const NAME: &'static str = "lib.publish";
    const SUMMARY: &'static str =
        "Copy a part (with its footprints), a footprint or a block (with its parts) into a shared library";
    const KIND: CommandKind = CommandKind::Query;
    type Output = LibChanges;

    fn run(self, ctx: &mut Context<'_>) -> Result<LibChanges, CommandError> {
        let items = publish_items(ctx, &self)?;
        let lib = select(&ctx.libraries()?, self.library.as_deref())?;
        lib.check_schema()?;
        let mut plan = Vec::new();
        let mut conflicts = Vec::new();
        for it in items {
            let e = lib.compare(&it)?;
            if matches!(e, Existing::Different | Existing::CaseVariant(_)) {
                conflicts.push(format!("{} {}", it.kind().label(), it.name()));
            }
            plan.push((it, e));
        }
        if !conflicts.is_empty() && !self.replace {
            return Err(conflict_error(
                &conflicts,
                &format!("library `{}` already has different versions of", lib.name),
                "pass `replace: true` to overwrite them (other items using them in the library change too), or compare with `lib.show`",
            ));
        }
        let dry_run = ctx.is_dry_run();
        let mut out = Vec::new();
        for (it, e) in &plan {
            if *e != Existing::Same && !dry_run {
                lib.write(it)?;
            }
            out.push(ItemChange { kind: it.kind(), name: it.name().to_string(), change: change_of(e) });
        }
        Ok(LibChanges { library: lib.name, path: lib.path, items: out, dry_run })
    }

    fn summarize(o: &LibChanges) -> String {
        o.text("published", "to")
    }
}

/// Copy a part (with its footprints), footprint or block (with its parts and footprints) from a
/// shared library into the project.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Import {
    /// Part ID (or MPN), footprint name or block name.
    pub name: String,
    /// Kind, when the name is ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ItemKind>,
    /// Library (name or directory path). Default: the first library that has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
    /// Overwrite project items that differ from the library's (components using a replaced part
    /// get the library definition).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

/// How a library item compares to the project's.
fn compare_in_project(p: &Project, it: &Item) -> Existing {
    let lib = p.library();
    match it {
        Item::Part(part) => match lib.parts.get(&part.id) {
            Some(old) if old == part => Existing::Same,
            Some(_) => Existing::Different,
            None => lib.find_part_id_ci(&part.id).map_or(Existing::Absent, |k| Existing::CaseVariant(k.into())),
        },
        Item::Footprint(f) => match lib.footprints.get(&f.name) {
            Some(old) if old == f => Existing::Same,
            Some(_) => Existing::Different,
            None => lib.find_footprint_ci(&f.name).map_or(Existing::Absent, |k| Existing::CaseVariant(k.into())),
        },
        Item::Model(n, d) => match lib.models.get(n) {
            Some(old) if old == d => Existing::Same,
            Some(_) => Existing::Different,
            None => lib.find_model_ci(n).map_or(Existing::Absent, |k| Existing::CaseVariant(k.into())),
        },
        Item::Block(b) => match p.circuit().blocks.get(&b.name) {
            Some(old) if *old == b.block => Existing::Same,
            Some(_) => Existing::Different,
            None => p
                .circuit()
                .blocks
                .keys()
                .find(|k| k.eq_ignore_ascii_case(&b.name))
                .map_or(Existing::Absent, |k| Existing::CaseVariant(k.clone())),
        },
    }
}

impl Command for Import {
    const NAME: &'static str = "lib.import";
    const SUMMARY: &'static str =
        "Copy a part, footprint or block (with what it needs) from a shared library into the project";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = LibChanges;

    fn run(self, ctx: &mut Context<'_>) -> Result<LibChanges, CommandError> {
        ctx.project()?;
        let libs = ctx.libraries()?;
        let scope = self.library.as_deref().map(|s| select(&libs, Some(s))).transpose()?;
        let (lib, item) = locate(&libs, &self.name, self.kind, scope.as_ref())?;

        // The item first, then its dependencies.
        let mut items = Vec::new();
        let mut warnings = Vec::new();
        match &item {
            Item::Part(part) => {
                for fr in &part.footprints {
                    // Same library first, then the others in search order.
                    let mut found = None;
                    for l in std::iter::once(&lib).chain(libs.list.iter().filter(|l| **l != lib)) {
                        if let Some(n) = l.resolve(ItemKind::Footprint, &fr.footprint)?
                            && n == fr.footprint
                        {
                            found = l.read(ItemKind::Footprint, &n)?;
                            break;
                        }
                    }
                    match found {
                        Some(f) => items.push(f),
                        None if ctx.project()?.library().footprints.contains_key(&fr.footprint) => {}
                        None => warnings.push(missing_footprint(&part.id, &fr.footprint)),
                    }
                }
            }
            Item::Footprint(_) | Item::Model(..) => {}
            Item::Block(b) => {
                items.extend(b.parts.values().cloned().map(Item::Part));
                items.extend(b.footprints.values().cloned().map(Item::Footprint));
                items.extend(b.models.iter().map(|(n, d)| Item::Model(n.clone(), d.clone())));
            }
        }
        items.insert(0, item);
        // 3D model files of the parts and footprints (blocks bring theirs).
        if !matches!(items[0], Item::Block(_)) {
            let parts = items.iter().filter_map(|i| if let Item::Part(p) = i { Some(p) } else { None });
            let fps = items.iter().filter_map(|i| if let Item::Footprint(f) = i { Some(f) } else { None });
            let wanted = models_used(parts, fps);
            for m in wanted {
                match find_model(&libs, &lib, &m)? {
                    Some(it) => items.push(it),
                    None if ctx.project()?.library().models.contains_key(&m) => {}
                    None => warnings.push(missing_model(&m)),
                }
            }
        }

        let p = ctx.project()?;
        let mut plan = Vec::new();
        let (mut conflicts, mut case) = (Vec::new(), Vec::new());
        for it in items {
            let e = compare_in_project(p, &it);
            let label = format!("{} {}", it.kind().label(), it.name());
            match &e {
                Existing::Different => conflicts.push(label),
                Existing::CaseVariant(k) => case.push(format!("{label} (project has `{k}`)")),
                _ => {}
            }
            plan.push((it, e));
        }
        if !case.is_empty() {
            return Err(CommandError::conflict(
                "lib.case_conflict",
                format!("names differ only in case from project items: {}", case.join(", ")),
            )
            .with_hint(
                "rename or remove the project item first; IDs are file names and some filesystems ignore case",
            ));
        }
        if !conflicts.is_empty() && !self.replace {
            return Err(conflict_error(
                &conflicts,
                "the project already has different versions of",
                "pass `replace: true` to overwrite them with the library versions, or compare with `lib.show` and `part.show`",
            ));
        }

        let mut out = Vec::new();
        for (it, e) in plan {
            out.push(ItemChange { kind: it.kind(), name: it.name().to_string(), change: change_of(&e) });
            if e == Existing::Same {
                continue;
            }
            let p = ctx.project_mut()?;
            match it {
                Item::Part(part) => {
                    if e == Existing::Different {
                        let users: Vec<String> = p.circuit().using_part(&part.id).map(|(r, _)| r.clone()).collect();
                        if !users.is_empty() {
                            warnings.push(
                                Diagnostic::warning(
                                    "lib.replaced_in_use",
                                    format!("part `{}` was replaced; used by {}", part.id, users.join(", ")),
                                )
                                .with_hint("check their connections with `circuit.erc`"),
                            );
                        }
                    }
                    p.library_mut().parts.insert(part.id.clone(), part);
                }
                Item::Footprint(f) => {
                    p.library_mut().footprints.insert(f.name.clone(), f);
                }
                Item::Block(b) => {
                    p.circuit_mut().blocks.insert(b.name, b.block);
                }
                Item::Model(n, d) => {
                    p.library_mut().models.insert(n, d);
                }
            }
        }
        for w in warnings {
            ctx.report(w);
        }
        Ok(LibChanges { library: lib.name, path: lib.path, items: out, dry_run: false })
    }

    fn summarize(o: &LibChanges) -> String {
        o.text("imported", "from")
    }
}

/// Delete an item from a shared library.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Remove {
    /// Part ID, footprint name or block name.
    pub name: String,
    /// Kind, when the name is ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ItemKind>,
    /// Library (name or directory path). Default: the user library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
}

impl Command for Remove {
    const NAME: &'static str = "lib.remove";
    const SUMMARY: &'static str = "Delete a part, footprint or block from a shared library";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = LibChanges;

    fn run(self, ctx: &mut Context<'_>) -> Result<LibChanges, CommandError> {
        let libs = ctx.libraries()?;
        let target = select(&libs, self.library.as_deref())?;
        let (lib, item) = locate(&libs, &self.name, self.kind, Some(&target))?;
        if let Item::Footprint(f) = &item {
            let users: Vec<String> = lib
                .items(ItemKind::Part)?
                .into_iter()
                .filter_map(|it| match it {
                    Item::Part(p) if p.footprints.iter().any(|r| r.footprint == f.name) => Some(p.id),
                    _ => None,
                })
                .collect();
            if !users.is_empty() {
                return Err(CommandError::conflict(
                    "lib.in_use",
                    format!(
                        "footprint `{}` is used by part(s) {} in library `{}`",
                        f.name,
                        users.join(", "),
                        target.name
                    ),
                )
                .with_hint("remove those parts first"));
            }
        }
        if let Item::Model(name, _) = &item {
            let parts: Vec<_> = lib.items(ItemKind::Part)?;
            let fps: Vec<_> = lib.items(ItemKind::Footprint)?;
            let mut users: Vec<String> = fps
                .iter()
                .filter_map(|it| match it {
                    Item::Footprint(f) if f.model.as_ref().is_some_and(|m| &m.file == name) => Some(f.name.clone()),
                    _ => None,
                })
                .collect();
            users.extend(parts.iter().filter_map(|it| match it {
                Item::Part(p) if p.footprints.iter().any(|r| r.model.as_ref().is_some_and(|m| &m.file == name)) => {
                    Some(p.id.clone())
                }
                _ => None,
            }));
            if !users.is_empty() {
                return Err(CommandError::conflict(
                    "lib.in_use",
                    format!("model `{name}` is used by {} in library `{}`", users.join(", "), target.name),
                )
                .with_hint("remove those footprints and parts first"));
            }
        }
        let dry_run = ctx.is_dry_run();
        if !dry_run {
            lib.remove(item.kind(), item.name())?;
        }
        Ok(LibChanges {
            library: target.name,
            path: target.path,
            items: vec![ItemChange { kind: item.kind(), name: item.name().to_string(), change: Change::Removed }],
            dry_run,
        })
    }

    fn summarize(o: &LibChanges) -> String {
        o.text("removed", "from")
    }
}
