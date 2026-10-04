//! Mouser Search API v1 (API key).
//!
//! Endpoints and fields from Mouser's published OpenAPI description
//! (<https://api.mouser.com/api/docs/V1>, UI at <https://api.mouser.com/api/docs/ui/index>):
//! `POST /api/v1/search/keyword?apiKey=KEY` with a `SearchByKeywordRequest`
//! (`keyword`, `records` ≤ 50, `startingRecord`, `searchOptions`), answering a
//! `SearchResponseRoot` (`Errors`, `SearchResults.Parts[]` of `MouserPart`). MPN lookups use the
//! keyword search with the MPN and keep exact matches: Mouser's `search/partnumber` takes Mouser
//! part numbers.
//!
//! Configuration: `[mouser] api_key` in `config.toml` (`cadlab config mouser`), overridden by
//! `MOUSER_API_KEY`. Prices come in the currency of the account the key belongs to. Mouser
//! allows 30 calls a minute and 1000 a day; responses are cached (`supplier::cache`), and the
//! key never goes into the cache key or error messages.

use std::sync::Arc;

use serde_json::{Value, json};

use super::cache::{Cache, offline};
use super::http::{Request, Transport, Ureq, redact};
use super::normalize;
use super::{Candidate, Lifecycle, PriceBreak, Provider, ProviderError, SearchQuery};
use crate::config::{MouserSettings, UserConfig};
use crate::model::part::ParamValue;

const ID: &str = "mouser";
/// Default API base.
pub const BASE: &str = "https://api.mouser.com";
/// Records per request (Mouser's maximum).
const PAGE: u32 = 50;

/// Mouser provider.
pub struct Mouser {
    api_key: String,
    base: String,
    transport: Arc<dyn Transport>,
    cache: Option<Cache>,
}

fn err(message: impl Into<String>) -> ProviderError {
    ProviderError::Unavailable { provider: ID.into(), message: message.into() }
}

fn bad_data(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidData { provider: ID.into(), message: message.into() }
}

impl Mouser {
    /// From the user settings (`[mouser]`), overridden by `MOUSER_API_KEY`. `None` without a key.
    pub fn from_settings(settings: Option<&MouserSettings>) -> Option<Self> {
        let key = std::env::var("MOUSER_API_KEY")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| settings.map(|s| s.api_key.clone()))
            .filter(|v| !v.is_empty())?;
        Some(Mouser::new(key, BASE, Arc::new(Ureq::default()), Cache::user_default()))
    }

    /// From the user settings file and the environment.
    pub fn from_env() -> Option<Self> {
        let cfg = UserConfig::load().ok().unwrap_or_default();
        Mouser::from_settings(cfg.mouser.as_ref())
    }

    /// Explicit configuration.
    pub fn new(api_key: String, base: &str, transport: Arc<dyn Transport>, cache: Option<Cache>) -> Self {
        Mouser { api_key, base: base.trim_end_matches('/').to_string(), transport, cache }
    }

    /// Checks the key with a one-record search (not cached).
    pub fn verify(&self) -> Result<(), ProviderError> {
        self.send("/api/v1/search/keyword", &keyword_body("resistor", 1, 0, false)).map(|_| ())
    }

    fn send(&self, path: &str, body: &Value) -> Result<Value, ProviderError> {
        let url = format!("{}{path}?apiKey={}", self.base, self.api_key);
        let resp = self
            .transport
            .post(&Request::json(url, body))
            .map_err(|e| err(format!("request failed: {}", redact(&e, &self.api_key))))?;
        let v: Value = serde_json::from_str(&resp.body).unwrap_or(Value::Null);
        if let Some(e) = errors(&v) {
            return Err(err(format!("{e} (HTTP {})", resp.status)));
        }
        match resp.status {
            200 if v.is_object() => Ok(v),
            200 => Err(bad_data("response is not a JSON object")),
            401 | 403 => Err(err(format!("API key rejected (HTTP {})", resp.status))),
            429 => Err(err("rate limited (HTTP 429): Mouser allows 30 calls a minute and 1000 a day")),
            s => Err(err(format!(
                "HTTP {s}: {}",
                redact(&resp.body.chars().take(200).collect::<String>(), &self.api_key)
            ))),
        }
    }

    /// POSTs through the cache.
    fn post(&self, path: &str, body: &Value) -> Result<Value, ProviderError> {
        let key = format!("{}{path} {body}", self.base);
        if let Some(c) = &self.cache
            && let Some(text) = c.get(ID, &key)
        {
            return serde_json::from_str(&text).map_err(|e| bad_data(e.to_string()));
        }
        if offline() {
            return Err(err("offline mode (CADLAB_OFFLINE=1) and no cached answer"));
        }
        let v = self.send(path, body)?;
        if let Some(c) = &self.cache {
            c.put(ID, &key, &v.to_string());
        }
        Ok(v)
    }

    fn keyword(&self, keyword: &str, in_stock: bool) -> Result<Vec<Candidate>, ProviderError> {
        let v = self.post("/api/v1/search/keyword", &keyword_body(keyword, PAGE, 0, in_stock))?;
        Ok(parse_response(&v))
    }
}

fn keyword_body(keyword: &str, records: u32, start: u32, in_stock: bool) -> Value {
    json!({"SearchByKeywordRequest": {
        "keyword": keyword,
        "records": records,
        "startingRecord": start,
        "searchOptions": if in_stock { "InStock" } else { "None" },
        "searchWithYourSignUpLanguage": "false",
    }})
}

/// The messages of a non-empty `Errors` array.
fn errors(v: &Value) -> Option<String> {
    let list = v["Errors"].as_array().filter(|a| !a.is_empty())?;
    let msgs: Vec<String> = list
        .iter()
        .map(|e| {
            let code = e["Code"].as_str().unwrap_or("");
            let msg = e["Message"].as_str().unwrap_or("error");
            if code.is_empty() { msg.to_string() } else { format!("{code}: {msg}") }
        })
        .collect();
    Some(msgs.join("; "))
}

fn text<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p[k].as_str().map(str::trim).filter(|s| !s.is_empty() && *s != "N/A")
}

/// Leading integer of a text such as `"12,345 In Stock"`.
fn leading_number(s: &str) -> Option<u64> {
    let digits: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Lifecycle from `LifecycleStatus` and `IsDiscontinued`. Mouser leaves the status empty for
/// ordinary production parts, which stay `unknown` here: cadlab does not guess.
fn lifecycle(p: &Value) -> Lifecycle {
    if text(p, "IsDiscontinued").is_some_and(|s| s.eq_ignore_ascii_case("true")) {
        return Lifecycle::Obsolete;
    }
    match text(p, "LifecycleStatus") {
        Some(s) if s.to_ascii_lowercase().starts_with("new") => Lifecycle::Active,
        Some(s) => normalize::lifecycle(s),
        None => Lifecycle::Unknown,
    }
}

/// Candidates from a search response (one per Mouser part number).
pub fn parse_response(v: &Value) -> Vec<Candidate> {
    let mut seen = std::collections::BTreeSet::new();
    v["SearchResults"]["Parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_part)
        .filter(|c| seen.insert(c.sku.clone()))
        .collect()
}

/// One `MouserPart`.
pub fn parse_part(p: &Value) -> Option<Candidate> {
    let sku = text(p, "MouserPartNumber")?.to_string();
    let mpn = text(p, "ManufacturerPartNumber")?.to_string();
    let attrs: Vec<(&str, &str)> = p["ProductAttributes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some((a["AttributeName"].as_str()?, a["AttributeValue"].as_str()?)))
        .collect();
    let mut params = normalize::params(attrs.iter().copied());
    let package = attrs
        .iter()
        .find(|(n, _)| {
            let n = n.to_ascii_lowercase().replace(' ', "");
            n == "package/case" || n == "case/package"
        })
        .and_then(|(_, v)| normalize::package(v));
    if let Some(pk) = &package {
        params.insert("package", ParamValue::Text(pk.clone()));
    }
    let prices = p["PriceBreaks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            let cur = b["Currency"].as_str().filter(|c| c.len() == 3).unwrap_or("USD");
            Some(PriceBreak { qty: b["Quantity"].as_u64()?, price: normalize::price_text(b["Price"].as_str()?, cur)? })
        })
        .collect();
    let stock = text(p, "AvailabilityInStock")
        .and_then(leading_number)
        .or_else(|| text(p, "Availability").and_then(leading_number))
        .unwrap_or(0);
    let category = text(p, "Category");
    Some(Candidate {
        provider: ID.into(),
        sku,
        manufacturer: text(p, "Manufacturer").map(String::from),
        mpn,
        description: text(p, "Description").unwrap_or("").to_string(),
        category: category.and_then(|c| normalize::category(&[c])),
        package,
        params,
        stock,
        moq: text(p, "Min").and_then(leading_number).unwrap_or(1).max(1),
        prices,
        lifecycle: lifecycle(p),
        datasheet: text(p, "DataSheetUrl").map(String::from),
        url: text(p, "ProductDetailUrl").map(String::from),
        drop_in: Vec::new(),
    })
}

impl Provider for Mouser {
    fn id(&self) -> &str {
        ID
    }

    fn search(&self, q: &SearchQuery) -> Result<Vec<Candidate>, ProviderError> {
        let words = normalize::keywords(q);
        if words.is_empty() && q.package.is_none() {
            return Ok(vec![]);
        }
        // With a package first (precise when Mouser names it the same way), then without it.
        let mut out: Vec<Candidate> = Vec::new();
        let add = |cands: Vec<Candidate>, out: &mut Vec<Candidate>| {
            for c in cands {
                if !out.iter().any(|o| o.sku == c.sku) {
                    out.push(c);
                }
            }
        };
        if let Some(p) = &q.package {
            let mut with = words.clone();
            with.push(p.clone());
            add(self.keyword(&with.join(" "), q.in_stock)?, &mut out);
        }
        if !words.is_empty() && out.iter().filter(|c| q.matches(c)).count() < q.limit {
            add(self.keyword(&words.join(" "), q.in_stock)?, &mut out);
        }
        Ok(out)
    }

    fn lookup(&self, mpn: &str) -> Result<Vec<Candidate>, ProviderError> {
        Ok(self.keyword(mpn, false)?.into_iter().filter(|c| c.mpn.eq_ignore_ascii_case(mpn)).collect())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::supplier::http::{Body, Response};

    /// Shaped like a `SearchResponseRoot` (field names from Mouser's OpenAPI description); values
    /// are made up.
    fn fixture() -> Value {
        json!({
            "Errors": [],
            "SearchResults": {
                "NumberOfResult": 2,
                "Parts": [{
                    "Availability": "12345 In Stock",
                    "AvailabilityInStock": "12345",
                    "DataSheetUrl": "https://www.mouser.com/datasheet/2/115/AP2112-x.pdf",
                    "Description": "LDO Voltage Regulators 600mA LDO 3.3V",
                    "Category": "LDO Voltage Regulators",
                    "LifecycleStatus": null,
                    "IsDiscontinued": "false",
                    "Manufacturer": "Diodes Incorporated",
                    "ManufacturerPartNumber": "AP2112K-3.3TRG1",
                    "Min": "1",
                    "Mult": "1",
                    "MouserPartNumber": "621-AP2112K-3.3TRG1",
                    "ProductAttributes": [
                        {"AttributeName": "Packaging", "AttributeValue": "Reel"},
                        {"AttributeName": "Packaging", "AttributeValue": "Cut Tape"},
                        {"AttributeName": "Package / Case", "AttributeValue": "SOT-23-5"},
                        {"AttributeName": "Output Voltage", "AttributeValue": "3.3 V"},
                        {"AttributeName": "Output Current", "AttributeValue": "600 mA"}
                    ],
                    "PriceBreaks": [
                        {"Quantity": 1, "Price": "$0.40", "Currency": "USD"},
                        {"Quantity": 10, "Price": "$0.276", "Currency": "USD"},
                        {"Quantity": 3000, "Price": "$0.099", "Currency": "USD"}
                    ],
                    "ProductDetailUrl": "https://www.mouser.com/ProductDetail/621-AP2112K-3.3TRG1",
                    "ROHSStatus": "RoHS Compliant"
                }, {
                    "Availability": "",
                    "DataSheetUrl": "",
                    "Description": "LDO Voltage Regulators Old",
                    "Category": "LDO Voltage Regulators",
                    "LifecycleStatus": "Obsolete",
                    "Manufacturer": "Diodes Incorporated",
                    "ManufacturerPartNumber": "AP2112K-3.3TRG1-OLD",
                    "Min": "3,000",
                    "MouserPartNumber": "621-OLD",
                    "ProductAttributes": [],
                    "PriceBreaks": [{"Quantity": 3000, "Price": "0,09 €", "Currency": "EUR"}]
                }, {
                    "Description": "no Mouser number",
                    "ManufacturerPartNumber": "X",
                    "MouserPartNumber": "N/A"
                }]
            }
        })
    }

    #[test]
    fn parses_parts() {
        let c = parse_response(&fixture());
        assert_eq!(c.len(), 2, "parts without a Mouser number skipped");
        let a = &c[0];
        assert_eq!(a.sku, "621-AP2112K-3.3TRG1");
        assert_eq!(a.mpn, "AP2112K-3.3TRG1");
        assert_eq!(a.manufacturer.as_deref(), Some("Diodes Incorporated"));
        assert_eq!(a.category, Some(crate::model::part::Category::Ldo));
        assert_eq!(a.package.as_deref(), Some("SOT-23-5"));
        assert_eq!(a.params.get("voltage_out").unwrap().to_string(), "3.3V");
        assert_eq!(a.params.get("current_out").unwrap().to_string(), "600mA");
        assert_eq!(a.stock, 12345);
        assert_eq!(a.prices[1].price.to_string(), "0.276 USD");
        assert_eq!(a.lifecycle, Lifecycle::Unknown);
        assert_eq!(a.datasheet.as_deref(), Some("https://www.mouser.com/datasheet/2/115/AP2112-x.pdf"));
        let b = &c[1];
        assert_eq!(b.moq, 3000);
        assert_eq!(b.stock, 0);
        assert_eq!(b.lifecycle, Lifecycle::Obsolete);
        assert_eq!(b.prices[0].price.to_string(), "0.09 EUR");
        assert_eq!(b.datasheet, None);
        let q = SearchQuery {
            text: "LDO".into(),
            package: Some("SOT-23-5".into()),
            filters: vec![super::super::ParamFilter::parse("current_out", ">=500mA").unwrap()],
            in_stock: true,
            ..Default::default()
        };
        assert!(q.matches(a), "normalized Mouser data passes cadlab filters");
    }

    #[test]
    fn requests_through_a_mock_transport() {
        let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
        let log = seen.clone();
        let transport = move |r: &Request| {
            log.lock().unwrap().push(r.clone());
            Ok(Response { status: 200, body: fixture().to_string() })
        };
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), std::time::Duration::from_secs(60));
        let m = Mouser::new("SECRET-KEY".into(), "https://mock", Arc::new(transport), Some(cache));
        let found = m.lookup("ap2112k-3.3trg1").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].sku, "621-AP2112K-3.3TRG1");
        // Same query again: answered by the cache.
        m.lookup("ap2112k-3.3trg1").unwrap();
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 1, "second lookup cached");
        assert_eq!(reqs[0].url, "https://mock/api/v1/search/keyword?apiKey=SECRET-KEY");
        let Body::Json(b) = &reqs[0].body else { panic!("JSON body") };
        let b: Value = serde_json::from_str(b).unwrap();
        assert_eq!(b["SearchByKeywordRequest"]["keyword"], "ap2112k-3.3trg1");
        assert_eq!(b["SearchByKeywordRequest"]["records"], 50);
        // The key is not in the cache.
        for e in std::fs::read_dir(dir.path().join(ID)).unwrap() {
            assert!(!std::fs::read_to_string(e.unwrap().path()).unwrap().contains("SECRET-KEY"));
        }
    }

    #[test]
    fn errors_are_reported_without_the_key() {
        let transport = |_: &Request| {
            Ok(Response {
                status: 200,
                body: json!({"Errors": [{"Id": 0, "Code": "Invalid", "Message": "Invalid unique identifier."}], "SearchResults": null}).to_string(),
            })
        };
        let m = Mouser::new("K".into(), "https://mock", Arc::new(transport), None);
        let e = m.verify().unwrap_err().to_string();
        assert!(e.contains("Invalid unique identifier"), "{e}");
        let failing = |r: &Request| Err(format!("connection refused: {}", r.url));
        let m = Mouser::new("TOPSECRET".into(), "https://mock", Arc::new(failing), None);
        let e = m.verify().unwrap_err().to_string();
        assert!(!e.contains("TOPSECRET") && e.contains("***"), "{e}");
    }
}
