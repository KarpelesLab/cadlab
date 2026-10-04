//! `catalog.*`: offline supplier catalogs (`supplier::catalog`), outside any project.
//!
//! `catalog.import` turns a parts list the user downloaded (CSV, e.g. from JLCPCB or LCSC) into a
//! catalog file in the user catalog directory, so `part.search`, `bom.resolve`, `fab.check` and
//! the substitutes use its SKUs (DECISIONS D33). Catalog writes happen outside the project: they
//! are skipped in dry runs and are not undone by `history.undo`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, ErrorKind, Registry};
use crate::supplier::catalog_dir;
use crate::supplier::import::{ImportError, ImportOptions, Skipped, import_csv};
use crate::{Diagnostic, ObjectRef};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Import>().register::<List>();
}

/// Import a parts list (CSV) into an offline catalog in your catalog directory.
///
/// Columns are recognized by header (`LCSC Part #` / `JLCPCB Part #` / `SKU`, `MFR.Part #` /
/// `MPN`, `Manufacturer`, `Description`, `Package`, `Category`, `Stock`, `MOQ`, `Price`,
/// `Datasheet`, `Library Type`, and parameter names such as `Resistance`); map others with
/// `columns`. Parameters of passives are also read from the description.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Import {
    /// CSV file (relative paths are relative to the current directory).
    pub path: PathBuf,
    /// Provider ID of the SKUs. `lcsc` for JLCPCB/LCSC `C` numbers: the JLCPCB fab profile
    /// orders by it.
    #[serde(default = "lcsc")]
    pub provider: String,
    /// Currency of prices written without one.
    #[serde(default = "usd")]
    pub currency: String,
    /// Explicit columns, field to header: {"sku": "Code", "mpn": "Part", "param:voltage_rating":
    /// "Rated voltage"}. Fields: sku, mpn, manufacturer, description, package, category, stock,
    /// moq, price, datasheet, url, lifecycle, class, or `param:<key>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub columns: BTreeMap<String, String>,
    /// Catalog file to write. Default: `<config dir>/catalogs/<provider>.json`, which is loaded
    /// automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<PathBuf>,
    /// Overwrite an existing catalog file.
    #[serde(default)]
    pub replace: bool,
}

fn lcsc() -> String {
    "lcsc".into()
}

fn usd() -> String {
    "USD".into()
}

/// Result of `catalog.import`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Imported {
    /// Provider ID.
    pub provider: String,
    /// Catalog file.
    pub path: PathBuf,
    /// Parts written.
    pub parts: usize,
    /// Columns used: field to header.
    pub columns: BTreeMap<String, String>,
    /// Headers not used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored: Vec<String>,
    /// Rows not imported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SkippedRow>,
    /// Whether an existing file was replaced.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replaced: bool,
    /// Nothing was written (dry run).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
}

/// A row not imported.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SkippedRow {
    /// Line in the file (1 = header).
    pub line: usize,
    /// Why.
    pub reason: String,
}

impl From<Skipped> for SkippedRow {
    fn from(s: Skipped) -> Self {
        SkippedRow { line: s.line, reason: s.reason }
    }
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Command for Import {
    const NAME: &'static str = "catalog.import";
    const SUMMARY: &'static str = "Import a parts list (CSV, e.g. a JLCPCB/LCSC download) into an offline catalog";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = Imported;

    fn run(self, ctx: &mut Context<'_>) -> Result<Imported, CommandError> {
        if !valid_id(&self.provider) {
            return Err(CommandError::invalid_args(
                "catalog.invalid_provider",
                format!("provider ID `{}` must be letters, digits, `-` or `_`", self.provider),
            )
            .with_hint("use a short ID such as `lcsc` (JLCPCB/LCSC SKUs) or `mystock`"));
        }
        let currency = self.currency.trim().to_ascii_uppercase();
        if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(CommandError::invalid_args(
                "catalog.invalid_currency",
                format!("`{}` is not an ISO 4217 currency code", self.currency),
            )
            .with_hint("e.g. USD, EUR, CNY"));
        }
        let file = ObjectRef::Named { kind: "file".into(), name: self.path.display().to_string() };
        let text = std::fs::read(&self.path).map_err(|e| {
            CommandError::new(ErrorKind::Io, "catalog.read", format!("{}: {e}", self.path.display()))
                .with_subject(file.clone())
                .with_hint("pass the path of a CSV file you downloaded (Excel files: save as CSV first)")
        })?;
        if text.starts_with(b"PK\x03\x04") || text.starts_with(&[0xd0, 0xcf, 0x11, 0xe0]) {
            return Err(CommandError::invalid_args(
                "catalog.not_csv",
                format!("{} is an Excel workbook, not CSV", self.path.display()),
            )
            .with_subject(file)
            .with_hint("open it in a spreadsheet program and save it as CSV (UTF-8), then import the CSV"));
        }
        let text = String::from_utf8_lossy(&text);
        let opts = ImportOptions { provider: self.provider.clone(), currency, columns: self.columns.clone() };
        let r = import_csv(&text, &opts).map_err(|e| {
            let code = match e {
                ImportError::Empty => "catalog.empty",
                ImportError::MissingColumn { .. } => "catalog.missing_column",
                ImportError::BadMapping(_) => "catalog.bad_mapping",
            };
            CommandError::invalid_args(code, format!("{}: {e}", self.path.display()))
                .with_subject(file.clone())
                .with_hint("map the columns explicitly with `columns`, e.g. {\"sku\": \"Code\", \"mpn\": \"Part\"}")
        })?;
        if r.parts.is_empty() {
            return Err(CommandError::invalid_args(
                "catalog.no_parts",
                format!("{}: no row has both a SKU and an MPN", self.path.display()),
            )
            .with_subject(file)
            .with_hint("check the `columns` mapping; `skipped` rows say what is missing"));
        }
        let path = match &self.output {
            Some(p) => p.clone(),
            None => catalog_dir()
                .ok_or_else(|| {
                    CommandError::new(
                        ErrorKind::Io,
                        "catalog.no_config_dir",
                        "cannot determine the user config directory",
                    )
                    .with_hint("set HOME or XDG_CONFIG_HOME, or pass `output`")
                })?
                .join(format!("{}.json", self.provider)),
        };
        let exists = path.exists();
        if exists && !self.replace {
            return Err(CommandError::conflict(
                "catalog.exists",
                format!("catalog file {} already exists", path.display()),
            )
            .with_subject(ObjectRef::Named { kind: "file".into(), name: path.display().to_string() })
            .with_hint("pass `replace: true` to overwrite it, or `output` to write elsewhere"));
        }
        for s in r.skipped.iter().take(20) {
            ctx.report(
                Diagnostic::warning("catalog.row_skipped", format!("line {}: {}", s.line, s.reason))
                    .with_subject(ObjectRef::Named { kind: "line".into(), name: s.line.to_string() })
                    .with_hint("fill in the missing cell or map the column with `columns`"),
            );
        }
        if r.skipped.len() > 20 {
            ctx.report(Diagnostic::warning(
                "catalog.row_skipped",
                format!("{} more rows skipped (see `skipped`)", r.skipped.len() - 20),
            ));
        }
        let dry_run = ctx.is_dry_run();
        if !dry_run {
            write(&path, &crate::supplier::import::catalog_json(&self.provider, &r.parts)).map_err(|e| {
                CommandError::new(ErrorKind::Io, "catalog.write", format!("{}: {e}", path.display()))
                    .with_hint("check that the directory is writable, or pass another `output`")
            })?;
            let in_default_dir = self.output.is_none();
            if in_default_dir && !ctx.session.suppliers.ids().iter().any(|i| i == &self.provider) {
                let s = std::mem::take(&mut ctx.session.suppliers);
                ctx.session.suppliers = s.with(Arc::new(crate::supplier::catalog::Catalog::lazy(path.clone())));
            } else if in_default_dir {
                ctx.report(
                    Diagnostic::info(
                        "catalog.reload",
                        format!("this session keeps the `{}` data it already loaded", self.provider),
                    )
                    .with_hint("the new file is used from the next CLI run or MCP server start"),
                );
            } else {
                ctx.report(
                    Diagnostic::info("catalog.not_loaded", format!("{} is not loaded automatically", path.display()))
                        .with_hint("list it in CADLAB_CATALOGS, or import without `output`"),
                );
            }
        }
        Ok(Imported {
            provider: self.provider,
            path,
            parts: r.parts.len(),
            columns: r.columns,
            ignored: r.ignored,
            skipped: r.skipped.into_iter().map(SkippedRow::from).collect(),
            replaced: exists,
            dry_run,
        })
    }

    fn summarize(o: &Imported) -> String {
        format!(
            "imported {} `{}` parts into {}{}{}",
            o.parts,
            o.provider,
            o.path.display(),
            if o.skipped.is_empty() { String::new() } else { format!(" ({} rows skipped)", o.skipped.len()) },
            if o.dry_run { " [dry run, nothing written]" } else { "" }
        )
    }
}

fn write(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent()
        && !d.as_os_str().is_empty()
    {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

/// List the configured part providers: catalog files and network providers.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {}

/// Result of `catalog.list`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Providers {
    /// Provider IDs in query order.
    pub providers: Vec<String>,
    /// Directory where `catalog.import` writes and catalog files are loaded from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_dir: Option<PathBuf>,
}

impl Command for List {
    const NAME: &'static str = "catalog.list";
    const SUMMARY: &'static str = "List configured part providers (catalog files, DigiKey, Mouser, Nexar)";
    const KIND: CommandKind = CommandKind::Query;
    type Output = Providers;

    fn run(self, ctx: &mut Context<'_>) -> Result<Providers, CommandError> {
        Ok(Providers { providers: ctx.session.suppliers.ids(), catalog_dir: catalog_dir() })
    }

    fn summarize(o: &Providers) -> String {
        if o.providers.is_empty() {
            "no part providers configured".into()
        } else {
            format!("providers: {}", o.providers.join(", "))
        }
    }
}
