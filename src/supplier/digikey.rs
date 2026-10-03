//! DigiKey Product Information API v4 (2-legged OAuth, client credentials).
//!
//! Configuration (environment): `DIGIKEY_CLIENT_ID`, `DIGIKEY_CLIENT_SECRET`; optional
//! `DIGIKEY_SITE` (default `US`), `DIGIKEY_LANGUAGE` (`en`), `DIGIKEY_CURRENCY` (`USD`),
//! `DIGIKEY_SANDBOX=1` for the sandbox API. Credentials never go into projects or the cache.
//!
//! Responses are cached (`supplier::cache`); tokens are kept in memory only.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::cache::{Cache, offline};
use super::normalize;
use super::query::Op;
use super::{Candidate, Money, PriceBreak, Provider, ProviderError, SearchQuery};
use crate::model::part::ParamValue;

const ID: &str = "digikey";

/// DigiKey provider.
pub struct DigiKey {
    client_id: String,
    client_secret: String,
    base: String,
    site: String,
    language: String,
    currency: String,
    agent: ureq::Agent,
    token: Mutex<Option<(String, Instant)>>,
    cache: Option<Cache>,
}

fn err(message: impl Into<String>) -> ProviderError {
    ProviderError::Unavailable { provider: ID.into(), message: message.into() }
}

fn bad_data(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidData { provider: ID.into(), message: message.into() }
}

impl DigiKey {
    /// From the environment; `None` when `DIGIKEY_CLIENT_ID` / `DIGIKEY_CLIENT_SECRET` are unset.
    pub fn from_env() -> Option<Self> {
        let id = std::env::var("DIGIKEY_CLIENT_ID").ok().filter(|v| !v.is_empty())?;
        let secret = std::env::var("DIGIKEY_CLIENT_SECRET").ok().filter(|v| !v.is_empty())?;
        let var = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
        let sandbox = std::env::var("DIGIKEY_SANDBOX").is_ok_and(|v| v == "1");
        Some(DigiKey::new(
            id,
            secret,
            if sandbox { "https://sandbox-api.digikey.com" } else { "https://api.digikey.com" },
            &var("DIGIKEY_SITE", "US"),
            &var("DIGIKEY_LANGUAGE", "en"),
            &var("DIGIKEY_CURRENCY", "USD"),
            Cache::user_default(),
        ))
    }

    /// Explicit configuration.
    pub fn new(
        client_id: String,
        client_secret: String,
        base: &str,
        site: &str,
        language: &str,
        currency: &str,
        cache: Option<Cache>,
    ) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        DigiKey {
            client_id,
            client_secret,
            base: base.trim_end_matches('/').to_string(),
            site: site.into(),
            language: language.into(),
            currency: currency.to_ascii_uppercase(),
            agent,
            token: Mutex::new(None),
            cache,
        }
    }

    fn token(&self, force: bool) -> Result<String, ProviderError> {
        let mut t = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if !force && let Some((tok, exp)) = &*t && Instant::now() + Duration::from_secs(30) < *exp {
            return Ok(tok.clone());
        }
        let mut resp = self
            .agent
            .post(format!("{}/v1/oauth2/token", self.base))
            .send_form([
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("grant_type", "client_credentials"),
            ])
            .map_err(|e| err(format!("token request failed: {e}")))?;
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_string().map_err(|e| err(e.to_string()))?;
        if status != 200 {
            return Err(ProviderError::NotConfigured {
                provider: ID.into(),
                message: format!("authentication failed (HTTP {status}): {}", error_text(&body)),
            });
        }
        let v: Value = serde_json::from_str(&body).map_err(|e| bad_data(e.to_string()))?;
        let tok = v["access_token"].as_str().ok_or_else(|| bad_data("token response without access_token"))?.to_string();
        let ttl = v["expires_in"].as_u64().unwrap_or(600);
        *t = Some((tok.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(tok)
    }

    /// POSTs JSON to an API path, through the cache.
    fn post(&self, path: &str, body: &Value) -> Result<Value, ProviderError> {
        let key = format!("{} {}|{}|{}|{} {}", self.base, path, self.site, self.language, self.currency, body);
        if let Some(c) = &self.cache
            && let Some(text) = c.get(ID, &key)
        {
            return serde_json::from_str(&text).map_err(|e| bad_data(e.to_string()));
        }
        if offline() {
            return Err(err("offline mode (CADLAB_OFFLINE=1) and no cached answer"));
        }
        let mut retried = false;
        loop {
            let tok = self.token(retried)?;
            let mut resp = self
                .agent
                .post(format!("{}{path}", self.base))
                .header("Authorization", &format!("Bearer {tok}"))
                .header("X-DIGIKEY-Client-Id", &self.client_id)
                .header("X-DIGIKEY-Locale-Site", &self.site)
                .header("X-DIGIKEY-Locale-Language", &self.language)
                .header("X-DIGIKEY-Locale-Currency", &self.currency)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
                .send(body.to_string())
                .map_err(|e| err(format!("request failed: {e}")))?;
            let status = resp.status().as_u16();
            let text = resp.body_mut().read_to_string().map_err(|e| err(e.to_string()))?;
            match status {
                200 => {
                    if let Some(c) = &self.cache {
                        c.put(ID, &key, &text);
                    }
                    return serde_json::from_str(&text).map_err(|e| bad_data(e.to_string()));
                }
                401 if !retried => retried = true,
                429 => return Err(err(format!("rate limited (HTTP 429): {}", error_text(&text)))),
                _ => return Err(err(format!("HTTP {status}: {}", error_text(&text)))),
            }
        }
    }

    fn keyword(&self, keywords: &str, limit: u32) -> Result<Vec<Candidate>, ProviderError> {
        let v = self.post("/products/v4/search/keyword", &json!({"Keywords": keywords, "Limit": limit, "Offset": 0}))?;
        Ok(parse_keyword_response(&v, &self.currency))
    }
}

/// Error message from a DigiKey problem-details body.
fn error_text(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    for k in ["detail", "title", "ErrorMessage", "error_description", "message"] {
        if let Some(s) = v[k].as_str() {
            return s.to_string();
        }
    }
    body.chars().take(200).collect()
}

/// Money from a JSON number without going through float formatting surprises.
fn money(v: &Value, currency: &str) -> Option<Money> {
    let f = v.as_f64()?;
    Money::parse(&format!("{f:.6} {currency}")).ok()
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str().filter(|s| !s.is_empty())
}

/// Converts a keyword search response into candidates: one per packaging variation (Digi-Reel
/// skipped: it carries a reeling fee).
pub fn parse_keyword_response(v: &Value, currency: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let products = v["ExactMatches"].as_array().into_iter().flatten().chain(v["Products"].as_array().into_iter().flatten());
    for p in products {
        for c in parse_product(p, currency) {
            if seen.insert(c.sku.clone()) {
                out.push(c);
            }
        }
    }
    out
}

/// Candidates from one product.
pub fn parse_product(p: &Value, currency: &str) -> Vec<Candidate> {
    let Some(mpn) = str_at(p, &["ManufacturerProductNumber"]) else { return vec![] };
    let params_list: Vec<(&str, &str)> = p["Parameters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| Some((x["ParameterText"].as_str()?, x["ValueText"].as_str()?)))
        .collect();
    let mut params = normalize::params(params_list.iter().copied());
    let find = |name: &str| params_list.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| *v);
    // Chips: "0402 (1005 Metric)" from Package / Case; ICs: the supplier device package (SOT-23-5).
    let case = find("Package / Case").and_then(normalize::package);
    let device = find("Supplier Device Package").and_then(normalize::package);
    let package = match (&case, &device) {
        (Some(c), _) if c.starts_with(|ch: char| ch.is_ascii_digit()) => case.clone(),
        (_, Some(_)) => device.clone(),
        _ => case.clone(),
    };
    if let Some(pk) = &package {
        params.insert("package", ParamValue::Text(pk.clone()));
    }
    let mut cats = Vec::new();
    let mut cat = &p["Category"];
    while let Some(name) = cat["Name"].as_str() {
        cats.push(name);
        cat = match cat["ChildCategories"].as_array().and_then(|c| c.first()) {
            Some(c) => c,
            None => break,
        };
    }
    let mut lifecycle = str_at(p, &["ProductStatus", "Status"]).map(normalize::lifecycle).unwrap_or_default();
    if p["Discontinued"].as_bool() == Some(true) || p["EndOfLife"].as_bool() == Some(true) {
        lifecycle = super::Lifecycle::Obsolete;
    }
    let base = Candidate {
        provider: ID.into(),
        sku: String::new(),
        manufacturer: str_at(p, &["Manufacturer", "Name"]).map(String::from),
        mpn: mpn.to_string(),
        description: str_at(p, &["Description", "ProductDescription"])
            .or_else(|| str_at(p, &["Description", "DetailedDescription"]))
            .unwrap_or("")
            .to_string(),
        category: normalize::category(&cats),
        package,
        params,
        stock: p["QuantityAvailable"].as_u64().unwrap_or(0),
        moq: 1,
        prices: Vec::new(),
        lifecycle,
        datasheet: str_at(p, &["DatasheetUrl"]).map(|u| if u.starts_with("//") { format!("https:{u}") } else { u.to_string() }),
        url: str_at(p, &["ProductUrl"]).map(String::from),
    };
    let variations = p["ProductVariations"].as_array().cloned().unwrap_or_default();
    if variations.is_empty() {
        return vec![Candidate { sku: mpn.to_string(), ..base }];
    }
    variations
        .iter()
        .filter(|var| !str_at(var, &["PackageType", "Name"]).unwrap_or("").contains("Digi-Reel"))
        .filter_map(|var| {
            let sku = str_at(var, &["DigiKeyProductNumber"])?.to_string();
            let prices = var["StandardPricing"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| Some(PriceBreak { qty: b["BreakQuantity"].as_u64()?, price: money(&b["UnitPrice"], currency)? }))
                .collect();
            Some(Candidate {
                sku,
                stock: var["QuantityAvailableforPackageType"].as_u64().unwrap_or(base.stock),
                moq: var["MinimumOrderQuantity"].as_u64().unwrap_or(1).max(1),
                prices,
                ..base.clone()
            })
        })
        .collect()
}

impl Provider for DigiKey {
    fn id(&self) -> &str {
        ID
    }

    fn search(&self, q: &SearchQuery) -> Result<Vec<Candidate>, ProviderError> {
        // DigiKey's keyword search works best with the values themselves: "LDO SOT-23-5 3.3V".
        let mut words: Vec<String> = q.text.split_whitespace().map(String::from).collect();
        if let Some(p) = &q.package {
            words.push(p.clone());
        }
        for f in q.filters.iter().filter(|f| f.op == Op::Eq) {
            match &f.value {
                ParamValue::Quantity(v) => words.push(v.to_string().replace('Ω', "")),
                ParamValue::Text(t) => words.push(t.clone()),
                ParamValue::Range { .. } => {}
            }
        }
        if words.is_empty()
            && let Some(c) = q.category
        {
            words.push(c.label().to_string());
        }
        if words.is_empty() {
            return Ok(vec![]);
        }
        self.keyword(&words.join(" "), 50)
    }

    fn lookup(&self, mpn: &str) -> Result<Vec<Candidate>, ProviderError> {
        Ok(self.keyword(mpn, 20)?.into_iter().filter(|c| c.mpn.eq_ignore_ascii_case(mpn)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like a v4 keyword search response (field names from DigiKey's v4 documentation and
    /// changelog); values are made up.
    fn fixture() -> Value {
        json!({
            "Products": [{
                "Description": {"ProductDescription": "IC REG LINEAR 3.3V 600MA SOT25", "DetailedDescription": "Linear Voltage Regulator IC Positive Fixed 1 Output 600mA SOT-25"},
                "Manufacturer": {"Id": 31, "Name": "Diodes Incorporated"},
                "ManufacturerProductNumber": "AP2112K-3.3TRG1",
                "ProductUrl": "https://www.digikey.com/en/products/detail/x",
                "DatasheetUrl": "//www.diodes.com/assets/Datasheets/AP2112.pdf",
                "QuantityAvailable": 120000,
                "ProductStatus": {"Id": 0, "Status": "Active"},
                "Discontinued": false,
                "EndOfLife": false,
                "Category": {"CategoryId": 32, "Name": "Integrated Circuits (ICs)", "ChildCategories": [
                    {"CategoryId": 699, "Name": "Voltage Regulators - Linear, Low Drop Out (LDO) Regulators", "ChildCategories": []}
                ]},
                "Parameters": [
                    {"ParameterId": 1, "ParameterText": "Voltage - Output (Min/Fixed)", "ValueText": "3.3V"},
                    {"ParameterId": 2, "ParameterText": "Current - Output", "ValueText": "600mA"},
                    {"ParameterId": 3, "ParameterText": "Voltage Dropout (Max)", "ValueText": "0.4V @ 600mA"},
                    {"ParameterId": 4, "ParameterText": "Operating Temperature", "ValueText": "-40°C ~ 85°C (TA)"},
                    {"ParameterId": 5, "ParameterText": "Package / Case", "ValueText": "SC-74A, SOT-753"},
                    {"ParameterId": 6, "ParameterText": "Supplier Device Package", "ValueText": "SOT-23-5"}
                ],
                "ProductVariations": [
                    {"DigiKeyProductNumber": "AP2112K-3.3TRG1DICT-ND", "PackageType": {"Id": 2, "Name": "Cut Tape (CT)"},
                     "StandardPricing": [{"BreakQuantity": 1, "UnitPrice": 0.4, "TotalPrice": 0.4}, {"BreakQuantity": 10, "UnitPrice": 0.276, "TotalPrice": 2.76}],
                     "QuantityAvailableforPackageType": 20000, "MinimumOrderQuantity": 1},
                    {"DigiKeyProductNumber": "AP2112K-3.3TRG1DITR-ND", "PackageType": {"Id": 1, "Name": "Tape & Reel (TR)"},
                     "StandardPricing": [{"BreakQuantity": 3000, "UnitPrice": 0.09879, "TotalPrice": 296.37}],
                     "QuantityAvailableforPackageType": 99000, "MinimumOrderQuantity": 3000},
                    {"DigiKeyProductNumber": "AP2112K-3.3TRG1DIDKR-ND", "PackageType": {"Id": 243, "Name": "Digi-Reel®"},
                     "StandardPricing": [{"BreakQuantity": 1, "UnitPrice": 0.4, "TotalPrice": 0.4}],
                     "QuantityAvailableforPackageType": 20000, "MinimumOrderQuantity": 1}
                ]
            }],
            "ProductsCount": 1,
            "ExactMatches": []
        })
    }

    #[test]
    fn parses_products() {
        let c = parse_keyword_response(&fixture(), "USD");
        assert_eq!(c.len(), 2, "Digi-Reel skipped");
        let ct = &c[0];
        assert_eq!(ct.sku, "AP2112K-3.3TRG1DICT-ND");
        assert_eq!(ct.mpn, "AP2112K-3.3TRG1");
        assert_eq!(ct.manufacturer.as_deref(), Some("Diodes Incorporated"));
        assert_eq!(ct.category, Some(crate::model::part::Category::Ldo));
        assert_eq!(ct.package.as_deref(), Some("SOT-23-5"));
        assert_eq!(ct.params.get("voltage_out").unwrap().to_string(), "3.3V");
        assert_eq!(ct.params.get("current_out").unwrap().to_string(), "600mA");
        assert_eq!(ct.params.get("dropout").unwrap().to_string(), "400mV");
        assert_eq!(ct.params.get("temperature").unwrap().to_string(), "-40°C..85°C");
        assert_eq!(ct.stock, 20000);
        assert_eq!(ct.prices[1].price.to_string(), "0.276 USD");
        assert_eq!(ct.datasheet.as_deref(), Some("https://www.diodes.com/assets/Datasheets/AP2112.pdf"));
        assert_eq!(ct.lifecycle, super::super::Lifecycle::Active);
        let tr = &c[1];
        assert_eq!(tr.moq, 3000);
        assert_eq!(tr.unit_price(10).unwrap().to_string(), "0.09879 USD");
    }

    #[test]
    fn matches_cadlab_query() {
        let c = parse_keyword_response(&fixture(), "USD");
        let q = SearchQuery {
            text: "LDO".into(),
            package: Some("SOT-23-5".into()),
            filters: vec![super::super::ParamFilter::parse("current_out", ">=500mA").unwrap()],
            in_stock: true,
            ..Default::default()
        };
        assert!(c.iter().all(|c| q.matches(c)), "normalized DigiKey data passes cadlab filters");
    }
}
