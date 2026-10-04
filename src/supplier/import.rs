//! Importing parts lists (CSV) into offline catalogs (`catalog.rs` format).
//!
//! Meant for parts lists users download themselves, e.g. from JLCPCB's or LCSC's websites, whose
//! SKUs (LCSC `C` numbers) the JLCPCB fab profile orders by. Neither JLCPCB nor LCSC publishes a
//! stable export format, so columns are recognized by header name (case, spaces and punctuation
//! ignored; see [`FIELDS`]) and can be mapped explicitly. Parameters come from columns named after
//! distributor parameters (`Resistance`, `Capacitance`, ...) and, for passives, from the values in
//! the description (`10kΩ ±1% 62.5mW 0402 Thick Film Resistors`).

use std::collections::BTreeMap;

use serde::Serialize;

use super::{Candidate, Lifecycle, PriceBreak, normalize};
use crate::model::part::{Category, ParamValue, Params};

/// Catalog fields and the header names recognized for them (normalized: lowercase ASCII letters
/// and digits only). `JLCPCB Part #` / `LCSC Part #` are the SKU headers of JLCPCB's BOM format.
pub const FIELDS: &[(&str, &[&str])] = &[
    (
        "sku",
        &[
            "lcscpart",
            "lcscpartnumber",
            "lcscpartno",
            "lcsc",
            "jlcpcbpart",
            "jlcpcbpartnumber",
            "jlcpcbpartno",
            "supplierpart",
            "supplierpartnumber",
            "sku",
        ],
    ),
    ("mpn", &["mfrpart", "mfrpartnumber", "mfrpartno", "manufacturerpart", "manufacturerpartnumber", "mpn"]),
    ("manufacturer", &["manufacturer", "mfr", "brand"]),
    ("description", &["description", "desc"]),
    ("package", &["package", "packagecase", "footprint"]),
    ("category", &["category", "firstcategory", "secondcategory", "subcategory"]),
    ("stock", &["stock", "stockqty", "instock", "inventory", "quantityavailable"]),
    ("moq", &["moq", "minorderqty", "minimumorderquantity", "minimumorder"]),
    ("price", &["price", "unitprice", "prices"]),
    ("datasheet", &["datasheet", "datasheeturl"]),
    ("url", &["url", "producturl", "link"]),
    ("lifecycle", &["lifecycle", "lifecyclestatus", "status"]),
    ("class", &["librarytype", "parttype", "partclass", "class"]),
];

/// Import options.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// Provider ID written into the catalog (`lcsc` for JLCPCB/LCSC SKUs).
    pub provider: String,
    /// Currency of prices written without one.
    pub currency: String,
    /// Explicit columns: field (see [`FIELDS`], or `param:<key>`) → header as written in the file.
    pub columns: BTreeMap<String, String>,
}

/// A row that was not imported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Skipped {
    /// Line number in the file (1 = header).
    pub line: usize,
    /// Why.
    pub reason: String,
}

/// Import result.
#[derive(Clone, Debug)]
pub struct Imported {
    /// Parts, in file order (first row wins for a repeated SKU).
    pub parts: Vec<Candidate>,
    /// Columns used: field → header.
    pub columns: BTreeMap<String, String>,
    /// Headers that were not used.
    pub ignored: Vec<String>,
    /// Rows not imported.
    pub skipped: Vec<Skipped>,
}

/// Import error (the file as a whole).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    /// No header row.
    #[error("the file is empty")]
    Empty,
    /// A required column is missing.
    #[error("no `{field}` column (headers: {headers})")]
    MissingColumn {
        /// Field.
        field: String,
        /// Headers found.
        headers: String,
    },
    /// An explicit mapping names a header that is not there, or an unknown field.
    #[error("{0}")]
    BadMapping(String),
}

/// Lowercase ASCII letters and digits of a header.
pub fn header_key(h: &str) -> String {
    h.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// Parses CSV (RFC 4180: quoted fields, doubled quotes, CRLF or LF; a leading BOM is skipped).
/// Blank lines are kept as empty records so line numbers stay right.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                any = false;
            }
            _ => field.push(c),
        }
    }
    if any {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// Leading count of a text, with thousands separators: `12,345,678`, `200+`, `1-199` (1).
fn digits(s: &str) -> Option<u64> {
    let d: String =
        s.trim().chars().take_while(|c| c.is_ascii_digit() || *c == ',').filter(char::is_ascii_digit).collect();
    d.parse().ok()
}

/// Price breaks from `0.0123`, `$0.0123`, or a list `1-9:0.0057,10-99:0.0040,100+:0.0031`.
fn prices(s: &str, currency: &str, moq: u64) -> Vec<PriceBreak> {
    let s = s.trim();
    if s.is_empty() {
        return vec![];
    }
    let mut out: Vec<PriceBreak> = Vec::new();
    if s.contains(':') {
        for item in s.split([',', ';', '\n']) {
            let Some((q, p)) = item.split_once(':') else { continue };
            if let (Some(qty), Some(price)) = (digits(q), normalize::price_text(p, currency))
                && !out.iter().any(|b| b.qty == qty)
            {
                out.push(PriceBreak { qty, price });
            }
        }
        out.sort_by_key(|b| b.qty);
    } else if let Some(price) = normalize::price_text(s, currency) {
        out.push(PriceBreak { qty: moq.max(1), price });
    }
    out
}

/// Parameters of a passive read from its description: values with units, tolerance, dielectric.
pub fn description_params(category: Option<Category>, description: &str) -> Params {
    let mut p = Params::default();
    let Some(cat) = category else { return p };
    if !matches!(
        cat,
        Category::Resistor | Category::Capacitor | Category::Inductor | Category::FerriteBead | Category::Fuse
    ) {
        return p;
    }
    for raw in description.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let tok = raw.trim_matches(|c: char| c == '(' || c == ')');
        if tok.is_empty() {
            continue;
        }
        let value = tok.split('@').next().unwrap_or(tok).trim_start_matches('±');
        let up = value.to_ascii_uppercase();
        let key =
            if matches!(up.as_str(), "X7R" | "X5R" | "X6S" | "X7S" | "X8R" | "C0G" | "NP0" | "COG" | "Y5V" | "Z5U")
                && cat == Category::Capacitor
            {
                p.0.entry("dielectric".into())
                    .or_insert_with(|| ParamValue::Text(up.replace("NP0", "C0G").replace("COG", "C0G")));
                continue;
            } else if value.ends_with('%') {
                "tolerance"
            } else if up.ends_with('Ω') || up.ends_with("OHM") || up.ends_with("OHMS") {
                match cat {
                    Category::Resistor => "resistance",
                    Category::FerriteBead => "impedance",
                    _ => continue,
                }
            } else if up.ends_with('F') && cat == Category::Capacitor {
                "capacitance"
            } else if up.ends_with('H') && cat == Category::Inductor {
                "inductance"
            } else if up.ends_with('V') && cat == Category::Capacitor {
                "voltage_rating"
            } else if up.ends_with('W') && cat == Category::Resistor {
                "power_rating"
            } else if up.ends_with('A') && matches!(cat, Category::Inductor | Category::FerriteBead | Category::Fuse) {
                "current_rating"
            } else {
                continue;
            };
        if p.get(key).is_none()
            && let Ok(v @ ParamValue::Quantity(_)) = ParamValue::parse(key, &normalize::clean_value(value))
        {
            p.insert(key, v);
        }
    }
    p
}

/// Converts CSV text into catalog parts.
pub fn import_csv(text: &str, opts: &ImportOptions) -> Result<Imported, ImportError> {
    let rows = parse_csv(text);
    let header = rows.first().ok_or(ImportError::Empty)?;
    let headers: Vec<String> = header.iter().map(|h| h.trim().to_string()).collect();
    if headers.iter().all(String::is_empty) {
        return Err(ImportError::Empty);
    }
    // field → column indexes (category may use several).
    let mut map: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut used = vec![false; headers.len()];
    for (field, h) in &opts.columns {
        let known =
            FIELDS.iter().any(|(f, _)| f == field) || field.strip_prefix("param:").is_some_and(|k| !k.is_empty());
        if !known {
            let fields: Vec<&str> = FIELDS.iter().map(|(f, _)| *f).collect();
            return Err(ImportError::BadMapping(format!(
                "unknown field `{field}` (fields: {}, or `param:<key>`)",
                fields.join(", ")
            )));
        }
        let Some(i) = headers.iter().position(|x| x == h.trim() || header_key(x) == header_key(h)) else {
            return Err(ImportError::BadMapping(format!(
                "column `{h}` (for `{field}`) is not in the file (headers: {})",
                headers.join(", ")
            )));
        };
        map.entry(field.clone()).or_default().push(i);
        used[i] = true;
    }
    for (field, aliases) in FIELDS {
        if map.contains_key(*field) {
            continue;
        }
        for (i, h) in headers.iter().enumerate() {
            if !used[i] && aliases.contains(&header_key(h).as_str()) {
                map.entry(field.to_string()).or_default().push(i);
                used[i] = true;
                if *field != "category" {
                    break;
                }
            }
        }
    }
    // Remaining headers named like distributor parameters.
    for (i, h) in headers.iter().enumerate() {
        if !used[i]
            && let Some(key) = normalize::param_key(h)
        {
            map.entry(format!("param:{key}")).or_default().push(i);
            used[i] = true;
        }
    }
    for field in ["sku", "mpn"] {
        if !map.contains_key(field) {
            return Err(ImportError::MissingColumn { field: field.into(), headers: headers.join(", ") });
        }
    }

    let mut out = Imported {
        parts: Vec::new(),
        columns: map
            .iter()
            .map(|(f, idx)| (f.clone(), idx.iter().map(|&i| headers[i].clone()).collect::<Vec<_>>().join(" + ")))
            .collect(),
        ignored: headers
            .iter()
            .enumerate()
            .filter(|(i, h)| !used[*i] && !h.is_empty())
            .map(|(_, h)| h.clone())
            .collect(),
        skipped: Vec::new(),
    };
    let mut seen = std::collections::BTreeSet::new();
    for (n, row) in rows.iter().enumerate().skip(1) {
        let line = n + 1;
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let get = |f: &str| -> Option<String> {
            let vals: Vec<&str> =
                map.get(f)?.iter().filter_map(|&i| row.get(i)).map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            (!vals.is_empty()).then(|| vals.join(" / "))
        };
        let Some(sku) = get("sku") else {
            out.skipped.push(Skipped { line, reason: "no SKU".into() });
            continue;
        };
        let Some(mpn) = get("mpn") else {
            out.skipped.push(Skipped { line, reason: format!("{sku}: no MPN") });
            continue;
        };
        if !seen.insert(sku.clone()) {
            out.skipped.push(Skipped { line, reason: format!("{sku}: repeated SKU (the first row is kept)") });
            continue;
        }
        let description = get("description").unwrap_or_default();
        let category = match get("category") {
            Some(c) => normalize::category(&c.split(" / ").collect::<Vec<_>>()),
            None => normalize::category(&[description.as_str()]),
        };
        let package = get("package").and_then(|p| normalize::package(&p));
        let mut params = description_params(category, &description);
        for (f, _) in map.iter().filter(|(f, _)| f.starts_with("param:")) {
            let key = &f["param:".len()..];
            if let Some(v) = get(f) {
                // Distributor-named columns go through the same cleaning as API data.
                let parsed = normalize::params([(key, v.as_str())]);
                if parsed.0.is_empty() {
                    if let Ok(pv) = ParamValue::parse(key, &normalize::clean_value(&v)) {
                        params.insert(key, pv);
                    }
                } else {
                    params.0.extend(parsed.0);
                }
            }
        }
        if let Some(pk) = &package {
            params.insert("package", ParamValue::Text(pk.clone()));
        }
        if let Some(c) = get("class") {
            params.insert("part_class", ParamValue::Text(c.to_ascii_lowercase()));
        }
        let moq = get("moq").and_then(|m| digits(&m)).unwrap_or(1).max(1);
        out.parts.push(Candidate {
            provider: opts.provider.clone(),
            sku,
            manufacturer: get("manufacturer"),
            mpn,
            description,
            category,
            package,
            params,
            stock: get("stock").and_then(|s| digits(&s)).unwrap_or(0),
            moq,
            prices: get("price").map(|p| prices(&p, &opts.currency, moq)).unwrap_or_default(),
            lifecycle: get("lifecycle").map(|l| normalize::lifecycle(&l)).unwrap_or(Lifecycle::Unknown),
            datasheet: get("datasheet").filter(|d| d.starts_with("http")),
            url: get("url").filter(|d| d.starts_with("http")),
            drop_in: Vec::new(),
        });
    }
    Ok(out)
}

/// The catalog file content (`{"provider": ..., "parts": [...]}`), canonical JSON.
pub fn catalog_json(provider: &str, parts: &[Candidate]) -> String {
    #[derive(Serialize)]
    struct File<'a> {
        provider: &'a str,
        parts: Vec<Candidate>,
    }
    let v = serde_json::to_value(File { provider, parts: parts.to_vec() }).expect("catalog serializes to JSON");
    crate::model::format::to_canonical_string(&v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ImportOptions {
        ImportOptions { provider: "lcsc".into(), currency: "USD".into(), columns: BTreeMap::new() }
    }

    const CSV: &str = "\u{feff}LCSC Part #,First Category,Second Category,MFR.Part #,Package,Manufacturer,Library Type,Description,Datasheet,Price,Stock\r\n\
C25744,Resistors,Chip Resistor - Surface Mount,0402WGF1002TCE,0402,UNI-ROYAL(Uniroyal Elec),Basic,\"62.5mW Thick Film Resistors 50V ±1% ±100ppm/℃ 10kΩ 0402 Chip Resistor - Surface Mount ROHS\",https://example.com/c25744.pdf,\"1-199:0.0011,200-:0.0005\",\"12,345,678\"\r\n\
C307331,Capacitors,Multilayer Ceramic Capacitors MLCC - SMD/SMT,CL05B104KO5NNNC,0402,Samsung Electro-Mechanics,Basic,\"16V 100nF X7R ±10% 0402 Multilayer Ceramic Capacitors MLCC - SMD/SMT ROHS\",,0.0012,20000\r\n\
C5446,Power Management ICs,Linear Voltage Regulators (LDO),XC6206P332MR,SOT-23,Torex Semicon,Basic,\"Fixed 3.3V 200mA SOT-23 Linear Voltage Regulators (LDO) ROHS\",,$0.03,\r\n\
,Resistors,,NOSKU,0402,,,,,,\r\n\
C25744,Resistors,,DUP,0402,,,,,,\r\n\
\r\n";

    #[test]
    fn imports_a_jlcpcb_style_list() {
        let r = import_csv(CSV, &opts()).unwrap();
        assert_eq!(r.columns["sku"], "LCSC Part #");
        assert_eq!(r.columns["mpn"], "MFR.Part #");
        assert_eq!(r.columns["category"], "First Category + Second Category");
        assert_eq!(r.columns["class"], "Library Type");
        assert!(r.ignored.is_empty(), "{:?}", r.ignored);
        assert_eq!(r.parts.len(), 3);
        assert_eq!(r.skipped.iter().map(|s| s.line).collect::<Vec<_>>(), [5, 6]);

        let res = &r.parts[0];
        assert_eq!(res.sku, "C25744");
        assert_eq!(res.category, Some(Category::Resistor));
        assert_eq!(res.package.as_deref(), Some("0402"));
        assert_eq!(res.params.get("resistance").unwrap().to_string(), "10kΩ");
        assert_eq!(res.params.get("tolerance").unwrap().to_string(), "1%");
        assert_eq!(res.params.get("power_rating").unwrap().to_string(), "62.5mW");
        assert_eq!(res.params.get("part_class").unwrap().to_string(), "basic");
        assert_eq!(res.stock, 12_345_678);
        assert_eq!(res.prices.len(), 2);
        assert_eq!(res.unit_price(500).unwrap().to_string(), "0.0005 USD");

        let cap = &r.parts[1];
        assert_eq!(cap.category, Some(Category::Capacitor));
        assert_eq!(cap.params.get("capacitance").unwrap().to_string(), "100nF");
        assert_eq!(cap.params.get("voltage_rating").unwrap().to_string(), "16V");
        assert_eq!(cap.params.get("dielectric").unwrap().to_string(), "X7R");
        assert_eq!(cap.params.get("tolerance").unwrap().to_string(), "10%");
        assert_eq!(cap.prices[0].price.to_string(), "0.0012 USD");

        let ldo = &r.parts[2];
        assert_eq!(ldo.category, Some(Category::Ldo));
        assert!(ldo.params.get("voltage_out").is_none(), "no guessing for non-passives");
        assert_eq!(ldo.stock, 0);

        // The bom.resolve query for `C 100nF 16V X7R 0402` accepts the imported capacitor.
        let q = crate::supplier::SearchQuery {
            category: Some(Category::Capacitor),
            package: Some("0402".into()),
            filters: vec![
                crate::supplier::ParamFilter::parse("capacitance", "100nF").unwrap(),
                crate::supplier::ParamFilter::parse("voltage_rating", ">=16V").unwrap(),
                crate::supplier::ParamFilter::parse("dielectric", "X7R").unwrap(),
            ],
            in_stock: true,
            ..Default::default()
        };
        assert!(q.matches(cap));

        // Round trip through the catalog format.
        let json = catalog_json("lcsc", &r.parts);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lcsc.json");
        std::fs::write(&p, &json).unwrap();
        let cat = crate::supplier::catalog::Catalog::lazy(p);
        use crate::supplier::Provider;
        assert_eq!(cat.id(), "lcsc");
        let back = cat.lookup("CL05B104KO5NNNC").unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(&back[0], cap);
    }

    #[test]
    fn explicit_columns_and_errors() {
        let csv = "Code,Part,Maker,Qty,Unit price (EUR),Resistance\nX1,RC0402FR-0710KL,Yageo,100,\"0,002\",10 kOhms\n";
        let mut o = opts();
        assert!(matches!(import_csv(csv, &o), Err(ImportError::MissingColumn { .. })));
        o.columns = [("sku", "Code"), ("mpn", "part"), ("stock", "Qty"), ("price", "Unit price (EUR)")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        o.currency = "EUR".into();
        let r = import_csv(csv, &o).unwrap();
        assert_eq!(r.ignored, ["Maker"]);
        let p = &r.parts[0];
        assert_eq!((p.sku.as_str(), p.mpn.as_str(), p.stock), ("X1", "RC0402FR-0710KL", 100));
        assert_eq!(p.prices[0].price.to_string(), "0.002 EUR");
        assert_eq!(p.params.get("resistance").unwrap().to_string(), "10kΩ");
        o.columns.insert("colour".into(), "Maker".into());
        assert!(matches!(import_csv(csv, &o), Err(ImportError::BadMapping(_))));
        o.columns.remove("colour");
        o.columns.insert("manufacturer".into(), "Brand".into());
        assert!(import_csv(csv, &o).unwrap_err().to_string().contains("`Brand`"));
        assert_eq!(import_csv("", &o).unwrap_err(), ImportError::Empty);
    }

    #[test]
    fn csv_quoting() {
        let rows = parse_csv("a,\"b,\"\"c\"\"\"\r\n\"multi\nline\",d\n\nlast");
        assert_eq!(rows, vec![vec!["a", "b,\"c\""], vec!["multi\nline", "d"], vec![""], vec!["last"]]);
    }
}
