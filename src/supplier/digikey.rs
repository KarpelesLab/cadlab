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
use crate::config::{DigiKeySettings, UserConfig};
use crate::model::part::ParamValue;

const ID: &str = "digikey";
/// Results per request (DigiKey's maximum).
const PAGE: u32 = 50;
/// Pages fetched at most per keyword set.
const MAX_PAGES: u32 = 3;

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
    /// From the user settings (`[digikey]` in `config.toml`), overridden by the environment
    /// (`DIGIKEY_CLIENT_ID`, `DIGIKEY_CLIENT_SECRET`, `DIGIKEY_SITE`, `DIGIKEY_LANGUAGE`,
    /// `DIGIKEY_CURRENCY`, `DIGIKEY_SANDBOX=1`). `None` without both an ID and a secret.
    pub fn from_settings(settings: Option<&DigiKeySettings>) -> Option<Self> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let pick = |k: &str, file: Option<&String>, d: &str| env(k).or(file.cloned()).unwrap_or_else(|| d.to_string());
        let id =
            env("DIGIKEY_CLIENT_ID").or_else(|| settings.map(|s| s.client_id.clone())).filter(|v| !v.is_empty())?;
        let secret = env("DIGIKEY_CLIENT_SECRET")
            .or_else(|| settings.map(|s| s.client_secret.clone()))
            .filter(|v| !v.is_empty())?;
        let sandbox = match env("DIGIKEY_SANDBOX") {
            Some(v) => v == "1",
            None => settings.is_some_and(|s| s.sandbox),
        };
        Some(DigiKey::new(
            id,
            secret,
            if sandbox { "https://sandbox-api.digikey.com" } else { "https://api.digikey.com" },
            &pick("DIGIKEY_SITE", settings.and_then(|s| s.site.as_ref()), "US"),
            &pick("DIGIKEY_LANGUAGE", settings.and_then(|s| s.language.as_ref()), "en"),
            &pick("DIGIKEY_CURRENCY", settings.and_then(|s| s.currency.as_ref()), "USD"),
            Cache::user_default(),
        ))
    }

    /// From the user settings file and the environment.
    pub fn from_env() -> Option<Self> {
        let cfg = UserConfig::load().ok().unwrap_or_default();
        DigiKey::from_settings(cfg.digikey.as_ref())
    }

    /// Checks the credentials by requesting a fresh token.
    pub fn verify(&self) -> Result<(), ProviderError> {
        self.token(true).map(|_| ())
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
        if !force
            && let Some((tok, exp)) = &*t
            && Instant::now() + Duration::from_secs(30) < *exp
        {
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
            return Err(err(format!("credentials rejected (HTTP {status}): {}", error_text(&body))));
        }
        let v: Value = serde_json::from_str(&body).map_err(|e| bad_data(e.to_string()))?;
        let tok =
            v["access_token"].as_str().ok_or_else(|| bad_data("token response without access_token"))?.to_string();
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
        self.keyword_page(keywords, limit, 0).map(|(c, _)| c)
    }

    /// One page of results, and whether more pages exist.
    fn keyword_page(&self, keywords: &str, limit: u32, offset: u32) -> Result<(Vec<Candidate>, bool), ProviderError> {
        let v =
            self.post("/products/v4/search/keyword", &json!({"Keywords": keywords, "Limit": limit, "Offset": offset}))?;
        let total = v["ProductsCount"].as_u64().unwrap_or(0);
        let more = (offset as u64 + limit as u64) < total;
        Ok((parse_keyword_response(&v, &self.currency), more))
    }

    /// Pages through results (up to [`MAX_PAGES`]) until enough of them pass `q`'s filters:
    /// keyword relevance is fuzzy ("1UF" also matches "0.1UF"), so the first page may hold none.
    fn keyword_matching(&self, keywords: &str, q: &SearchQuery, out: &mut Vec<Candidate>) -> Result<(), ProviderError> {
        for page in 0..MAX_PAGES {
            let (cands, more) = self.keyword_page(keywords, PAGE, page * PAGE)?;
            for c in cands {
                if !out.iter().any(|o| o.sku == c.sku) {
                    out.push(c);
                }
            }
            if !more || out.iter().filter(|c| q.matches(c)).count() >= q.limit {
                break;
            }
        }
        Ok(())
    }
}

/// DigiKey description-style keywords for generic passives: `CAP CER 1UF 16V X7R`,
/// `RES 10K OHM 1%`, `FIXED IND 4.7UH`. `None` when the query is not a passive.
fn passive_words(q: &SearchQuery) -> Option<Vec<String>> {
    use crate::model::part::Category;
    let get = |k: &str| q.filters.iter().find(|f| f.key == k).map(|f| &f.value);
    let qty = |k: &str| match get(k) {
        Some(ParamValue::Quantity(v)) => Some(*v),
        _ => None,
    };
    let mut w: Vec<String> = Vec::new();
    match q.category? {
        Category::Capacitor => {
            let c = qty("capacitance")?;
            w.extend(["CAP".into(), "CER".into()]);
            // DigiKey writes picofarads below 1 nF, microfarads above: 100PF, 0.1UF, 1UF.
            w.push(if c.cmp_value(&crate::value::Quantity::new(1, -9, c.unit)).is_lt() {
                format!("{}PF", c.decimal_in(-12))
            } else {
                format!("{}UF", c.decimal_in(-6))
            });
            if let Some(v) = qty("voltage_rating") {
                w.push(format!("{}V", v.decimal_in(0)));
            }
            if let Some(ParamValue::Text(d)) = get("dielectric") {
                w.push(d.clone());
            }
        }
        Category::Resistor => {
            let r = qty("resistance")?;
            let (e, suffix) = match r.decimal_in(0).split('.').next().map(str::len).unwrap_or(0) {
                0..=3 => (0, ""),
                4..=6 => (3, "K"),
                _ => (6, "M"),
            };
            w.extend(["RES".into(), format!("{}{suffix}", r.decimal_in(e)), "OHM".into()]);
            if let Some(t) = qty("tolerance") {
                w.push(format!("{}%", t.decimal_in(0)));
            }
        }
        Category::Inductor => {
            let l = qty("inductance")?;
            w.extend(["FIXED".into(), "IND".into()]);
            w.push(if l.cmp_value(&crate::value::Quantity::new(1, -6, l.unit)).is_lt() {
                format!("{}NH", l.decimal_in(-9))
            } else {
                format!("{}UH", l.decimal_in(-6))
            });
        }
        _ => return None,
    }
    w.extend(q.text.split_whitespace().map(String::from));
    Some(w)
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
    let products =
        v["ExactMatches"].as_array().into_iter().flatten().chain(v["Products"].as_array().into_iter().flatten());
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
    let Some(mpn) = str_at(p, &["ManufacturerProductNumber"]) else {
        return vec![];
    };
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
        datasheet: str_at(p, &["DatasheetUrl"])
            .map(|u| if u.starts_with("//") { format!("https:{u}") } else { u.to_string() }),
        url: str_at(p, &["ProductUrl"]).map(String::from),
        drop_in: Vec::new(),
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
                .filter_map(|b| {
                    Some(PriceBreak { qty: b["BreakQuantity"].as_u64()?, price: money(&b["UnitPrice"], currency)? })
                })
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
        // DigiKey's keyword search matches its description text, so passives are phrased the way
        // DigiKey writes them ("CAP CER 1UF 16V X7R"); otherwise the values themselves ("LDO 3.3V").
        let mut words: Vec<String> = match passive_words(q) {
            Some(w) => w,
            None => {
                let mut w: Vec<String> = q.text.split_whitespace().map(String::from).collect();
                for f in q.filters.iter().filter(|f| f.op == Op::Eq) {
                    match &f.value {
                        ParamValue::Quantity(v) => w.push(v.to_string().replace('Ω', "")),
                        ParamValue::Text(t) if f.key != "package" => w.push(t.clone()),
                        _ => {}
                    }
                }
                w
            }
        };
        if words.is_empty()
            && let Some(c) = q.category
        {
            words.push(c.label().to_string());
        }
        if words.is_empty() && q.package.is_none() {
            return Ok(vec![]);
        }
        // With a package, search with it (precise when DigiKey uses the same name) and without it
        // (DigiKey may name it differently: SOT-25 for SOT-23-5); the caller filters on aliases.
        let mut out = Vec::new();
        if let Some(p) = &q.package {
            let mut with = words.clone();
            with.push(p.clone());
            self.keyword_matching(&with.join(" "), q, &mut out)?;
        }
        if !words.is_empty() && out.iter().filter(|c| q.matches(c)).count() < q.limit {
            self.keyword_matching(&words.join(" "), q, &mut out)?;
        }
        Ok(out)
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
    fn passive_keywords() {
        use crate::model::part::Category;
        use crate::supplier::ParamFilter;
        let q = |cat, filters: &[(&str, &str)]| SearchQuery {
            category: Some(cat),
            filters: filters.iter().map(|(k, v)| ParamFilter::parse(k, v).unwrap()).collect(),
            ..Default::default()
        };
        let w = |q: SearchQuery| passive_words(&q).unwrap().join(" ");
        assert_eq!(
            w(q(Category::Capacitor, &[("capacitance", "1uF"), ("voltage_rating", ">=16V"), ("dielectric", "X7R")])),
            "CAP CER 1UF 16V X7R"
        );
        assert_eq!(w(q(Category::Capacitor, &[("capacitance", "100nF")])), "CAP CER 0.1UF");
        assert_eq!(w(q(Category::Capacitor, &[("capacitance", "22pF")])), "CAP CER 22PF");
        assert_eq!(w(q(Category::Resistor, &[("resistance", "10k"), ("tolerance", "<=1%")])), "RES 10K OHM 1%");
        assert_eq!(w(q(Category::Resistor, &[("resistance", "4k7")])), "RES 4.7K OHM");
        assert_eq!(w(q(Category::Resistor, &[("resistance", "220")])), "RES 220 OHM");
        assert_eq!(w(q(Category::Resistor, &[("resistance", "1M")])), "RES 1M OHM");
        assert_eq!(w(q(Category::Inductor, &[("inductance", "4.7uH")])), "FIXED IND 4.7UH");
        assert!(passive_words(&q(Category::Ldo, &[])).is_none());
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
