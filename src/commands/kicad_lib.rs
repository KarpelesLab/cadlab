//! Import of the user's own KiCad library files (DECISIONS D41): `footprint.import_kicad`
//! (`.kicad_mod` / `.pretty` → project footprints), `part.import_kicad_sym` (`.kicad_sym` →
//! project parts) and `lib.import_kicad` (either, into a shared library, D19).
//!
//! Items equal to what the target holds are `unchanged`; different items with the same name are
//! a conflict (`import.conflict`) unless `replace` is given. Shared-library writes happen outside
//! the project's transaction, like `lib.publish`: skipped in dry runs, not undone by
//! `history.undo`.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::kicad_pcb::{read, resolve, to_error};
use super::lib::{Change, select};
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::kicad_import::library::{
    FootprintFile, FootprintSet, LibraryFootprint, SymbolOptions, SymbolSet, read_footprints, read_symbols,
};
use crate::model::Project;
use crate::model::footprint::Footprint;
use crate::model::part::Category;
use crate::userlib::{Existing, Item, ItemKind, UserLibrary};
use crate::{Diagnostic, ObjectRef};

pub(crate) fn register(r: &mut Registry) {
    r.register::<FootprintImportKicad>().register::<PartImportKicadSym>().register::<LibImportKicad>();
}

/// One item an import added, replaced or found unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportedItem {
    /// cadlab name (footprint name or part ID).
    pub name: String,
    /// Name in the KiCad library, when it differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kicad_name: Option<String>,
    /// What happened.
    pub change: Change,
    /// Footprint the part uses (parts only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
}

/// A symbol or file that was not imported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkippedItem {
    /// Name in the KiCad library.
    pub kicad_name: String,
    /// Code of the diagnostic explaining why (`import.symbol_power`, ...).
    pub reason: String,
}

/// Result of a KiCad library import.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KicadLibImport {
    /// Files read.
    pub source: String,
    /// Where the items went: `project`, or the shared library's name.
    pub target: String,
    /// Directory of the shared library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Footprints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footprints: Vec<ImportedItem>,
    /// Parts (one per symbol).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<ImportedItem>,
    /// Symbols not imported (power symbols, symbols without pins, broken `extends`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SkippedItem>,
    /// Footprint drawings, pads and settings not imported (each reported in the diagnostics).
    pub not_imported: usize,
    /// Nothing was written to the shared library (dry run).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
}

impl KicadLibImport {
    fn text(&self) -> String {
        let list = |items: &[ImportedItem]| {
            items
                .iter()
                .map(|i| {
                    let c = match i.change {
                        Change::Added => "added",
                        Change::Replaced => "replaced",
                        Change::Unchanged => "unchanged",
                        Change::Removed => "removed",
                    };
                    let fp = i.footprint.as_ref().map(|f| format!(", footprint {f}")).unwrap_or_default();
                    format!("{} ({c}{fp})", i.name)
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let into = match &self.path {
            Some(p) => format!("library `{}` ({})", self.target, p.display()),
            None => "the project".to_string(),
        };
        let mut s = format!("imported {} into {into}", self.source);
        if !self.footprints.is_empty() {
            s += &format!("\nfootprints: {}", list(&self.footprints));
        }
        if !self.parts.is_empty() {
            s += &format!("\nparts: {}", list(&self.parts));
        }
        if !self.skipped.is_empty() {
            let sk: Vec<String> = self.skipped.iter().map(|k| format!("{} ({})", k.kicad_name, k.reason)).collect();
            s += &format!("\nskipped: {}", sk.join(", "));
        }
        if self.not_imported > 0 {
            s += &format!("\n{} footprint items not imported (see diagnostics)", self.not_imported);
        }
        if self.dry_run {
            s += "\n[dry run, nothing written]";
        }
        s
    }
}

fn not_found(path: &Path, what: &str) -> CommandError {
    CommandError::not_found("import.file_not_found", format!("{} does not exist", path.display()))
        .with_hint(format!("give the path of the {what} (relative paths start at the project directory)"))
}

/// Reads a `.kicad_mod` file or the `.kicad_mod` files of a directory (a `.pretty` library),
/// keeping only `names` (KiCad names, case-insensitive) when given.
fn footprint_files(path: &Path, names: &[String]) -> Result<(Vec<FootprintFile>, String), CommandError> {
    let label = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let mut files = Vec::new();
    if path.is_dir() {
        let rd = std::fs::read_dir(path)
            .map_err(|e| CommandError::from(crate::model::ModelError::Io { path: path.to_path_buf(), source: e }))?;
        let mut paths: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "kicad_mod"))
            .collect();
        paths.sort();
        for p in paths {
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            files.push((name, p));
        }
        if files.is_empty() {
            return Err(CommandError::invalid_args(
                "import.no_footprints",
                format!("{} holds no `.kicad_mod` files", path.display()),
            )
            .with_hint("give a KiCad footprint library directory (`MyLib.pretty`) or a `.kicad_mod` file"));
        }
    } else if path.is_file() {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        files.push((name, path.to_path_buf()));
    } else {
        return Err(not_found(path, "`.kicad_mod` file or `.pretty` directory"));
    }
    if !names.is_empty() {
        let all: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
        for n in names {
            if !all.iter().any(|a| a.eq_ignore_ascii_case(n)) {
                let sug = crate::suggest::did_you_mean(n, all.iter().map(String::as_str), 3);
                return Err(CommandError::not_found(
                    "import.footprint_not_found",
                    format!("{label} has no footprint `{n}`"),
                )
                .with_suggestions(&sug)
                .with_hint_if_none(format!("footprints: {}", all.join(", "))));
            }
        }
        files.retain(|(f, _)| names.iter().any(|n| n.eq_ignore_ascii_case(f)));
    }
    let mut out = Vec::new();
    for (name, p) in files {
        out.push(FootprintFile { name, text: read(&p)? });
    }
    Ok((out, label))
}

fn report(ctx: &mut Context<'_>, diags: Vec<Diagnostic>) {
    for d in diags {
        ctx.report(d);
    }
}

fn kicad_name(cadlab: &str, kicad: &str) -> Option<String> {
    (cadlab != kicad).then(|| kicad.to_string())
}

fn conflict(list: &[String], target: &str) -> CommandError {
    CommandError::conflict(
        "import.conflict",
        format!("{target} already has different versions of: {}", list.join(", ")),
    )
    .with_hint("pass `replace: true` to overwrite them, or compare first with `--dry-run`")
}

/// Puts footprints into the project library.
fn merge_footprints(
    p: &mut Project,
    set: Vec<LibraryFootprint>,
    replace: bool,
) -> Result<Vec<ImportedItem>, CommandError> {
    let mut plan = Vec::new();
    let mut conflicts = Vec::new();
    for lf in set {
        let mut fp = lf.footprint;
        let change = match p.library().find_footprint_ci(&fp.name).map(String::from) {
            None => Change::Added,
            Some(k) => {
                let old = &p.library().footprints[&k];
                // A 3D model attached in cadlab survives a re-import.
                if fp.model.is_none() {
                    fp.model = old.model.clone();
                }
                if k == fp.name && *old == fp {
                    Change::Unchanged
                } else {
                    conflicts.push(format!("footprint {k}"));
                    Change::Replaced
                }
            }
        };
        let item = ImportedItem {
            name: fp.name.clone(),
            kicad_name: kicad_name(&fp.name, &lf.kicad_name),
            change,
            footprint: None,
        };
        plan.push((fp, item));
    }
    if !conflicts.is_empty() && !replace {
        return Err(conflict(&conflicts, "the project library"));
    }
    let lib = p.library_mut();
    let mut out = Vec::new();
    for (fp, item) in plan {
        if item.change == Change::Replaced
            && let Some(k) = lib.find_footprint_ci(&fp.name).map(String::from)
        {
            lib.footprints.remove(&k);
        }
        if item.change != Change::Unchanged {
            lib.footprints.insert(fp.name.clone(), fp);
        }
        out.push(item);
    }
    Ok(out)
}

/// Puts parts into the project library, warning when a replaced part in use loses pins.
fn merge_parts(ctx: &mut Context<'_>, set: &SymbolSet, replace: bool) -> Result<Vec<ImportedItem>, CommandError> {
    let p = ctx.project()?;
    let mut plan = Vec::new();
    let mut conflicts = Vec::new();
    let mut notes = Vec::new();
    for ls in &set.parts {
        let part = ls.part.clone();
        let change = match p.library().find_part_id_ci(&part.id).map(String::from) {
            None => Change::Added,
            Some(k) => {
                let old = &p.library().parts[&k];
                if k == part.id && *old == part {
                    Change::Unchanged
                } else {
                    conflicts.push(format!("part {k}"));
                    let lost: Vec<&str> = old
                        .symbol
                        .pins
                        .iter()
                        .map(|x| x.number.as_str())
                        .filter(|n| part.symbol.pin(n).is_none())
                        .collect();
                    let users: Vec<String> = p.circuit().using_part(&k).map(|(r, _)| r.clone()).collect();
                    if !lost.is_empty() && !users.is_empty() {
                        notes.push(
                            Diagnostic::warning(
                                "import.part_in_use",
                                format!(
                                    "part `{k}` is used by {} and loses pins {}",
                                    users.join(", "),
                                    lost.join(", ")
                                ),
                            )
                            .with_subject(ObjectRef::Part { scheme: "local".into(), id: k.clone() })
                            .with_hint("check the connections of those components (`circuit.erc`)"),
                        );
                    }
                    Change::Replaced
                }
            }
        };
        let item = ImportedItem {
            name: part.id.clone(),
            kicad_name: kicad_name(&part.id, &ls.kicad_name),
            change,
            footprint: part.footprint().map(|f| f.footprint.clone()),
        };
        plan.push((part, item));
    }
    if !conflicts.is_empty() && !replace {
        return Err(conflict(&conflicts, "the project library"));
    }
    if replace {
        report(ctx, notes);
    }
    let lib = ctx.project_mut()?.library_mut();
    let mut out = Vec::new();
    for (part, item) in plan {
        if item.change == Change::Replaced
            && let Some(k) = lib.find_part_id_ci(&part.id).map(String::from)
        {
            lib.parts.remove(&k);
        }
        if item.change != Change::Unchanged {
            lib.parts.insert(part.id.clone(), part);
        }
        out.push(item);
    }
    Ok(out)
}

fn skipped(set: &SymbolSet) -> Vec<SkippedItem> {
    set.skipped.iter().map(|s| SkippedItem { kicad_name: s.kicad_name.clone(), reason: s.reason.to_string() }).collect()
}

/// Imports footprints into the project: shared by `footprint.import_kicad` and
/// `part.import_kicad_sym --footprints`.
fn import_footprints_into_project(
    ctx: &mut Context<'_>,
    path: &Path,
    names: &[String],
    replace: bool,
    license: Option<&str>,
) -> Result<(Vec<ImportedItem>, usize, String), CommandError> {
    let path = resolve(ctx, path);
    let (files, label) = footprint_files(&path, names)?;
    let FootprintSet { footprints, diagnostics, not_imported } =
        read_footprints(&files, &label, license).map_err(to_error)?;
    report(ctx, diagnostics);
    let items = merge_footprints(ctx.project_mut()?, footprints, replace)?;
    Ok((items, not_imported, label))
}

/// Import footprints from a KiCad footprint file (`.kicad_mod`) or library directory
/// (`.pretty`) into the project library. Pads of every shape, drills, silkscreen, fab and
/// courtyard drawings come in; what cadlab footprints cannot hold is reported. Use
/// `lib.import_kicad` to import into a shared library instead.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FootprintImportKicad {
    /// `.kicad_mod` file or `.pretty` directory (relative paths start at the project directory).
    pub path: PathBuf,
    /// Only these footprints of a `.pretty` directory (KiCad names, the file names without
    /// `.kicad_mod`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footprint: Vec<String>,
    /// Overwrite project footprints of the same name that differ.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
    /// License of the library ("CC-BY-SA-4.0", "own work"), recorded in each footprint's
    /// provenance. You are responsible for having the right to use what you import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

impl Command for FootprintImportKicad {
    const NAME: &'static str = "footprint.import_kicad";
    const SUMMARY: &'static str = "Import footprints from a KiCad .kicad_mod file or .pretty library into the project";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = KicadLibImport;

    fn run(self, ctx: &mut Context<'_>) -> Result<KicadLibImport, CommandError> {
        ctx.project()?;
        let (footprints, not_imported, source) =
            import_footprints_into_project(ctx, &self.path, &self.footprint, self.replace, self.license.as_deref())?;
        Ok(KicadLibImport { source, target: "project".into(), footprints, not_imported, ..Default::default() })
    }

    fn summarize(o: &KicadLibImport) -> String {
        o.text()
    }
}

/// Import symbols from a KiCad symbol library (`.kicad_sym`) as parts of the project: pins
/// (number, name, type, side, unit, alternate functions), category from the reference prefix,
/// value, datasheet, description, MPN and manufacturer, other fields as parameters, and the
/// footprint the Footprint field names when the project has it (import it with `footprints`).
/// Symbols are drawn by cadlab's symbol generator; power symbols are skipped (they are nets).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartImportKicadSym {
    /// `.kicad_sym` file (relative paths start at the project directory).
    pub path: PathBuf,
    /// Only these symbols (KiCad names).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbol: Vec<String>,
    /// A `.pretty` directory or `.kicad_mod` file to import first, so the symbols' Footprint
    /// fields find their footprints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprints: Option<PathBuf>,
    /// Category of every imported part, instead of guessing it from the reference prefix
    /// (`R` resistor, `U` IC, `J` connector, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// Overwrite project parts (and footprints) of the same name that differ.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
    /// License of the library, recorded in each part's provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

impl Command for PartImportKicadSym {
    const NAME: &'static str = "part.import_kicad_sym";
    const SUMMARY: &'static str = "Import symbols from a KiCad .kicad_sym library as project parts";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = KicadLibImport;

    fn run(self, ctx: &mut Context<'_>) -> Result<KicadLibImport, CommandError> {
        ctx.project()?;
        let path = resolve(ctx, &self.path);
        if !path.is_file() {
            return Err(not_found(&path, "`.kicad_sym` file"));
        }
        let text = read(&path)?;
        let mut out = KicadLibImport { target: "project".into(), ..Default::default() };
        let mut source = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(fpath) = &self.footprints {
            let (items, n, label) =
                import_footprints_into_project(ctx, fpath, &[], self.replace, self.license.as_deref())?;
            out.footprints = items;
            out.not_imported = n;
            source = format!("{source} + {label}");
        }
        let opts = SymbolOptions {
            only: self.symbol.clone(),
            category: self.category,
            license: self.license.clone(),
            source: path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        let p = ctx.project()?;
        let lookup = |n: &str| {
            let k = p.library().find_footprint_ci(n)?;
            Some((k.to_string(), p.library().footprints[k].clone()))
        };
        let set = read_symbols(&text, &opts, &lookup).map_err(to_error)?;
        report(ctx, set.diagnostics.clone());
        out.parts = merge_parts(ctx, &set, self.replace)?;
        out.skipped = skipped(&set);
        out.source = source;
        Ok(out)
    }

    fn summarize(o: &KicadLibImport) -> String {
        o.text()
    }
}

/// Import a KiCad library into a shared library (D19) instead of the project: a `.kicad_sym`
/// (symbols become parts), a `.kicad_mod` or a `.pretty` directory (footprints). Works without
/// a project. Nothing is written in a dry run; undo does not revert library writes.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LibImportKicad {
    /// `.kicad_sym`, `.kicad_mod` or `.pretty` directory (relative paths start at the project
    /// directory, or the current directory without a project).
    pub path: PathBuf,
    /// Only these symbols or footprints (KiCad names).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub name: Vec<String>,
    /// With a `.kicad_sym`: a `.pretty` directory or `.kicad_mod` file to import too, so the
    /// symbols' Footprint fields find their footprints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprints: Option<PathBuf>,
    /// Category of every imported part, instead of guessing it from the reference prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// Target library (name or directory path). Default: the user library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
    /// Overwrite library items of the same name that differ.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
    /// License of the library, recorded in each item's provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

/// Plans writing `item` to `lib`: its change, and whether it conflicts.
fn plan_lib(lib: &UserLibrary, item: &mut Item) -> Result<(Change, bool), CommandError> {
    let mut e = lib.compare(item)?;
    // A 3D model attached in cadlab survives a re-import.
    if e == Existing::Different
        && let Item::Footprint(fp) = item
        && fp.model.is_none()
        && let Some(Item::Footprint(old)) = lib.read(ItemKind::Footprint, &fp.name)?
        && old.model.is_some()
    {
        fp.model = old.model;
        e = lib.compare(item)?;
    }
    Ok(match e {
        Existing::Absent => (Change::Added, false),
        Existing::Same => (Change::Unchanged, false),
        Existing::Different | Existing::CaseVariant(_) => (Change::Replaced, true),
    })
}

impl Command for LibImportKicad {
    const NAME: &'static str = "lib.import_kicad";
    const SUMMARY: &'static str =
        "Import a KiCad symbol or footprint library (.kicad_sym, .kicad_mod, .pretty) into a shared library";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = KicadLibImport;

    fn run(self, ctx: &mut Context<'_>) -> Result<KicadLibImport, CommandError> {
        let path = resolve(ctx, &self.path);
        let lib = select(&ctx.libraries()?, self.library.as_deref())?;
        lib.check_schema()?;
        let is_sym = path.is_file()
            && (path.extension().is_some_and(|x| x == "kicad_sym" || x == "lib")
                || read(&path)?.trim_start().starts_with("(kicad_symbol_lib"));
        let mut out = KicadLibImport {
            target: lib.name.clone(),
            path: Some(lib.path.clone()),
            dry_run: ctx.is_dry_run(),
            ..Default::default()
        };

        // Footprints: the path itself, or `footprints` next to a symbol library.
        let fp_path = if is_sym { self.footprints.as_ref().map(|f| resolve(ctx, f)) } else { Some(path.clone()) };
        if !is_sym && self.footprints.is_some() {
            return Err(CommandError::invalid_args(
                "import.footprints_with_footprints",
                "`footprints` goes with a `.kicad_sym`; this path is already a footprint library",
            ));
        }
        let mut fp_items: Vec<(Item, ImportedItem)> = Vec::new();
        let mut labels = Vec::new();
        if is_sym {
            labels.push(path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
        }
        let mut imported_fps: Vec<Footprint> = Vec::new();
        if let Some(fp_path) = fp_path {
            let names: &[String] = if is_sym { &[] } else { &self.name };
            let (files, label) = footprint_files(&fp_path, names)?;
            let set = read_footprints(&files, &label, self.license.as_deref()).map_err(to_error)?;
            labels.push(label);
            report(ctx, set.diagnostics);
            out.not_imported = set.not_imported;
            for lf in set.footprints {
                imported_fps.push(lf.footprint.clone());
                let ii = ImportedItem {
                    name: lf.footprint.name.clone(),
                    kicad_name: kicad_name(&lf.footprint.name, &lf.kicad_name),
                    change: Change::Added,
                    footprint: None,
                };
                fp_items.push((Item::Footprint(lf.footprint), ii));
            }
        }
        out.source = labels.join(" + ");

        // Parts.
        let mut part_items: Vec<(Item, ImportedItem)> = Vec::new();
        if is_sym {
            let text = read(&path)?;
            let opts = SymbolOptions {
                only: self.name.clone(),
                category: self.category,
                license: self.license.clone(),
                source: labels[0].clone(),
            };
            let lookup = |n: &str| -> Option<(String, Footprint)> {
                if let Some(f) = imported_fps.iter().find(|f| f.name.eq_ignore_ascii_case(n)) {
                    return Some((f.name.clone(), f.clone()));
                }
                let k = lib.resolve(ItemKind::Footprint, n).ok()??;
                match lib.read(ItemKind::Footprint, &k).ok()?? {
                    Item::Footprint(f) => Some((k, f)),
                    _ => None,
                }
            };
            let set = read_symbols(&text, &opts, &lookup).map_err(to_error)?;
            report(ctx, set.diagnostics.clone());
            out.skipped = skipped(&set);
            for ls in set.parts {
                let ii = ImportedItem {
                    name: ls.part.id.clone(),
                    kicad_name: kicad_name(&ls.part.id, &ls.kicad_name),
                    change: Change::Added,
                    footprint: ls.part.footprint().map(|f| f.footprint.clone()),
                };
                part_items.push((Item::Part(ls.part), ii));
            }
        }

        // Plan, check conflicts, write.
        let mut conflicts = Vec::new();
        for (item, ii) in fp_items.iter_mut().chain(part_items.iter_mut()) {
            let (change, conflicting) = plan_lib(&lib, item)?;
            ii.change = change;
            if conflicting {
                conflicts.push(format!("{} {}", item.kind().label(), item.name()));
            }
        }
        if !conflicts.is_empty() && !self.replace {
            return Err(conflict(&conflicts, &format!("library `{}`", lib.name)));
        }
        if !out.dry_run {
            for (item, ii) in fp_items.iter().chain(part_items.iter()) {
                if ii.change != Change::Unchanged {
                    lib.write(item)?;
                }
            }
        }
        out.footprints = fp_items.into_iter().map(|(_, ii)| ii).collect();
        out.parts = part_items.into_iter().map(|(_, ii)| ii).collect();
        Ok(out)
    }

    fn summarize(o: &KicadLibImport) -> String {
        o.text()
    }
}
