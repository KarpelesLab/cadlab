//! Nexar (Octopart) supply GraphQL API (OAuth 2 client credentials).
//!
//! From Nexar's documentation (<https://www.altium.com/documentation/altium-developer-center/octopart/api/authorization>,
//! <https://www.altium.com/documentation/altium-developer-center/octopart/api/search>,
//! <https://support.nexar.com/support/solutions/articles/101000494582>) and the published GraphQL
//! schema of `https://api.nexar.com/graphql`: a token from `https://identity.nexar.com/connect/token`
//! (`grant_type=client_credentials`, `scope=supply.domain`, valid 24 h), then `supSearch` (keywords)
//! and `supSearchMpn` (MPN) queries with `country`, `currency`, `limit`, `inStockOnly`.
//!
//! One candidate per seller offer: SKU `"<seller>:<seller SKU>"`, prices converted to the
//! configured currency (`convertedPrice`), stock from `inventoryLevel` (negative codes mean
//! unknown and count as 0). Brokers are always excluded; sellers not authorized by the
//! manufacturer only with `unauthorized = true`. Lifecycle and package come from the
//! `lifecyclestatus` and `case_package` specs. `similarParts` ("similar in specs and
//! functionality") is not a drop-in list and is not used (DECISIONS D26).
//!
//! Configuration: `[nexar]` in `config.toml` (`cadlab config nexar`), overridden by
//! `NEXAR_CLIENT_ID`, `NEXAR_CLIENT_SECRET`, `NEXAR_COUNTRY`, `NEXAR_CURRENCY`. Nexar plans count
//! matched parts, so searches ask for few parts; responses are cached (`supplier::cache`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::cache::{Cache, offline};
use super::http::{Request, Transport, Ureq};
use super::normalize;
use super::{Candidate, Lifecycle, PriceBreak, Provider, ProviderError, SearchQuery};
use crate::config::{NexarSettings, UserConfig};
use crate::model::part::ParamValue;

const ID: &str = "nexar";
/// Token endpoint.
pub const TOKEN_URL: &str = "https://identity.nexar.com/connect/token";
/// GraphQL endpoint.
pub const API_URL: &str = "https://api.nexar.com/graphql";
/// Parts asked for per keyword search.
const SEARCH_LIMIT: u32 = 10;
/// Parts asked for per MPN lookup.
const MPN_LIMIT: u32 = 5;

const PART_FIELDS: &str = "mpn manufacturer { name } shortDescription octopartUrl \
     category { name path } bestDatasheet { url } \
     specs { attribute { name shortname } displayValue } \
     sellers(authorizedOnly: $authorizedOnly, includeBrokers: false) { company { name } isAuthorized \
       offers { sku inventoryLevel moq packaging clickUrl prices { quantity convertedPrice convertedCurrency } } }";

/// Nexar provider.
pub struct Nexar {
    client_id: String,
    client_secret: String,
    token_url: String,
    api_url: String,
    country: String,
    currency: String,
    authorized_only: bool,
    transport: Arc<dyn Transport>,
    token: Mutex<Option<(String, Instant)>>,
    cache: Option<Cache>,
}

fn err(message: impl Into<String>) -> ProviderError {
    ProviderError::Unavailable { provider: ID.into(), message: message.into() }
}

fn bad_data(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidData { provider: ID.into(), message: message.into() }
}

impl Nexar {
    /// From the user settings (`[nexar]`), overridden by the environment. `None` without both
    /// a client ID and a secret.
    pub fn from_settings(settings: Option<&NexarSettings>) -> Option<Self> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let id = env("NEXAR_CLIENT_ID").or_else(|| settings.map(|s| s.client_id.clone())).filter(|v| !v.is_empty())?;
        let secret = env("NEXAR_CLIENT_SECRET")
            .or_else(|| settings.map(|s| s.client_secret.clone()))
            .filter(|v| !v.is_empty())?;
        let country = env("NEXAR_COUNTRY").or_else(|| settings.and_then(|s| s.country.clone()));
        let currency = env("NEXAR_CURRENCY").or_else(|| settings.and_then(|s| s.currency.clone()));
        let mut n = Nexar::new(
            id,
            secret,
            country.as_deref().unwrap_or("US"),
            currency.as_deref().unwrap_or("USD"),
            Arc::new(Ureq::default()),
            Cache::user_default(),
        );
        n.authorized_only = !settings.is_some_and(|s| s.unauthorized);
        Some(n)
    }

    /// From the user settings file and the environment.
    pub fn from_env() -> Option<Self> {
        let cfg = UserConfig::load().ok().unwrap_or_default();
        Nexar::from_settings(cfg.nexar.as_ref())
    }

    /// Explicit configuration (authorized sellers only, Nexar's production endpoints).
    pub fn new(
        client_id: String,
        client_secret: String,
        country: &str,
        currency: &str,
        transport: Arc<dyn Transport>,
        cache: Option<Cache>,
    ) -> Self {
        Nexar {
            client_id,
            client_secret,
            token_url: TOKEN_URL.into(),
            api_url: API_URL.into(),
            country: country.to_ascii_uppercase(),
            currency: currency.to_ascii_uppercase(),
            authorized_only: true,
            transport,
            token: Mutex::new(None),
            cache,
        }
    }

    /// Uses other endpoints (tests).
    pub fn with_endpoints(mut self, token_url: &str, api_url: &str) -> Self {
        self.token_url = token_url.into();
        self.api_url = api_url.into();
        self
    }

    /// Checks the credentials by requesting a fresh token.
    pub fn verify(&self) -> Result<(), ProviderError> {
        self.token(true).map(|_| ())
    }

    fn token(&self, force: bool) -> Result<String, ProviderError> {
        let mut t = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if !force
            && let Some((tok, exp)) = &*t
            && Instant::now() + Duration::from_secs(60) < *exp
        {
            return Ok(tok.clone());
        }
        let req = Request::form(
            &self.token_url,
            &[
                ("grant_type", "client_credentials"),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("scope", "supply.domain"),
            ],
        );
        let resp = self.transport.post(&req).map_err(|e| err(format!("token request failed: {e}")))?;
        let v: Value = serde_json::from_str(&resp.body).unwrap_or(Value::Null);
        if resp.status != 200 {
            let why = v["error_description"].as_str().or(v["error"].as_str()).unwrap_or("no details");
            return Err(err(format!("credentials rejected (HTTP {}): {why}", resp.status)));
        }
        let tok = v["access_token"].as_str().ok_or_else(|| bad_data("token response without access_token"))?;
        let ttl = v["expires_in"].as_u64().unwrap_or(3600);
        *t = Some((tok.to_string(), Instant::now() + Duration::from_secs(ttl)));
        Ok(tok.to_string())
    }

    /// Runs a GraphQL query through the cache; returns `data`.
    fn query(&self, query: &str, vars: &Value) -> Result<Value, ProviderError> {
        let body = json!({"query": query, "variables": vars});
        let key = format!("{} {body}", self.api_url);
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
            let req = Request::json(&self.api_url, &body).header("Authorization", &format!("Bearer {tok}"));
            let resp = self.transport.post(&req).map_err(|e| err(format!("request failed: {e}")))?;
            let v: Value = serde_json::from_str(&resp.body).unwrap_or(Value::Null);
            let auth_failed = resp.status == 401
                || v["errors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|e| e["extensions"]["code"].as_str().is_some_and(|c| c.starts_with("Auth")));
            if auth_failed && !retried {
                retried = true;
                continue;
            }
            if resp.status == 429 {
                return Err(err("rate limited (HTTP 429)"));
            }
            if let Some(errs) = v["errors"].as_array().filter(|e| !e.is_empty()) {
                let msgs: Vec<&str> = errs.iter().filter_map(|e| e["message"].as_str()).collect();
                return Err(err(format!("HTTP {}: {}", resp.status, msgs.join("; "))));
            }
            if resp.status != 200 || !v["data"].is_object() {
                return Err(err(format!("HTTP {}: {}", resp.status, resp.body.chars().take(200).collect::<String>())));
            }
            if let Some(c) = &self.cache {
                c.put(ID, &key, &v["data"].to_string());
            }
            return Ok(v["data"].clone());
        }
    }

    fn search_field(&self, field: &str, q: &str, limit: u32, in_stock: bool) -> Result<Vec<Candidate>, ProviderError> {
        let query = format!(
            "query Search($q: String, $limit: Int, $country: String!, $currency: String!, $inStockOnly: Boolean, \
             $authorizedOnly: Boolean!) {{ {field}(q: $q, limit: $limit, country: $country, currency: $currency, \
             inStockOnly: $inStockOnly) {{ hits results {{ part {{ {PART_FIELDS} }} }} }} }}"
        );
        let vars = json!({
            "q": q, "limit": limit, "country": self.country, "currency": self.currency,
            "inStockOnly": in_stock, "authorizedOnly": self.authorized_only,
        });
        let data = self.query(&query, &vars)?;
        Ok(parse_results(&data[field]))
    }
}

/// Lifecycle from Octopart's lifecycle status text (`Production`, `NRND`, `EOL`, `Obsolete`).
fn lifecycle(s: &str) -> Lifecycle {
    let l = s.to_ascii_lowercase();
    if l == "production" || l == "new" || l == "active" {
        Lifecycle::Active
    } else if l == "eol" {
        Lifecycle::Obsolete
    } else {
        normalize::lifecycle(s)
    }
}

/// Candidates from a `SupPartResultSet` (`supSearch` or `supSearchMpn`).
pub fn parse_results(set: &Value) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for r in set["results"].as_array().into_iter().flatten() {
        for c in parse_part(&r["part"]) {
            if !out.iter().any(|o| o.sku == c.sku) {
                out.push(c);
            }
        }
    }
    out
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v[k].as_str().map(str::trim).filter(|s| !s.is_empty())
}

/// Candidates from one `SupPart`: one per offer of each seller.
pub fn parse_part(p: &Value) -> Vec<Candidate> {
    let Some(mpn) = s(p, "mpn") else { return vec![] };
    let specs: Vec<(&str, &str, &str)> = p["specs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|sp| {
            Some((
                sp["attribute"]["name"].as_str()?,
                sp["attribute"]["shortname"].as_str().unwrap_or(""),
                sp["displayValue"].as_str()?,
            ))
        })
        .collect();
    let spec = |short: &str| specs.iter().find(|(_, sn, _)| *sn == short).map(|(_, _, v)| *v);
    let mut params = normalize::params(specs.iter().map(|(n, _, v)| (*n, *v)));
    let package = spec("case_package").and_then(normalize::package);
    if let Some(pk) = &package {
        params.insert("package", ParamValue::Text(pk.clone()));
    }
    let cat = &p["category"];
    let mut cat_names: Vec<String> = s(cat, "path")
        .map(|path| path.split('/').filter(|x| !x.is_empty()).map(|x| x.replace('-', " ")).collect())
        .unwrap_or_default();
    if let Some(n) = s(cat, "name") {
        cat_names.push(n.to_string());
    }
    let cat_refs: Vec<&str> = cat_names.iter().map(String::as_str).collect();
    let base = Candidate {
        provider: ID.into(),
        sku: String::new(),
        manufacturer: s(&p["manufacturer"], "name").map(String::from),
        mpn: mpn.to_string(),
        description: s(p, "shortDescription").unwrap_or("").to_string(),
        category: normalize::category(&cat_refs),
        package,
        params,
        stock: 0,
        moq: 1,
        prices: Vec::new(),
        lifecycle: spec("lifecyclestatus").map(lifecycle).unwrap_or_default(),
        datasheet: s(&p["bestDatasheet"], "url").map(String::from),
        url: s(p, "octopartUrl").map(String::from),
        drop_in: Vec::new(),
    };
    let mut out = Vec::new();
    for seller in p["sellers"].as_array().into_iter().flatten() {
        let Some(name) = s(&seller["company"], "name") else { continue };
        for o in seller["offers"].as_array().into_iter().flatten() {
            let Some(sku) = s(o, "sku") else { continue };
            let mut prices: Vec<PriceBreak> = Vec::new();
            for b in o["prices"].as_array().into_iter().flatten() {
                let (Some(qty), Some(cur)) = (b["quantity"].as_u64(), b["convertedCurrency"].as_str()) else {
                    continue;
                };
                if let Some(price) = normalize::price_number(&b["convertedPrice"], cur)
                    && !prices.iter().any(|x| x.qty == qty)
                {
                    prices.push(PriceBreak { qty, price });
                }
            }
            prices.sort_by_key(|b| b.qty);
            let mut description = base.description.clone();
            if let Some(pk) = s(o, "packaging") {
                description = format!("{description} [{name}, {pk}]").trim().to_string();
            } else {
                description = format!("{description} [{name}]").trim().to_string();
            }
            out.push(Candidate {
                sku: format!("{name}:{sku}"),
                description,
                stock: o["inventoryLevel"].as_i64().unwrap_or(0).max(0) as u64,
                moq: o["moq"].as_u64().unwrap_or(1).max(1),
                prices,
                url: s(o, "clickUrl").map(String::from).or(base.url.clone()),
                ..base.clone()
            });
        }
    }
    out
}

impl Provider for Nexar {
    fn id(&self) -> &str {
        ID
    }

    fn search(&self, q: &SearchQuery) -> Result<Vec<Candidate>, ProviderError> {
        let mut words = normalize::keywords(q);
        if let Some(p) = &q.package {
            words.push(p.clone());
        }
        if words.is_empty() {
            return Ok(vec![]);
        }
        self.search_field("supSearch", &words.join(" "), SEARCH_LIMIT, q.in_stock)
    }

    fn lookup(&self, mpn: &str) -> Result<Vec<Candidate>, ProviderError> {
        Ok(self
            .search_field("supSearchMpn", mpn, MPN_LIMIT, false)?
            .into_iter()
            .filter(|c| c.mpn.eq_ignore_ascii_case(mpn))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supplier::http::{Body, Response};

    /// Shaped like a `supSearchMpn` answer (field names from Nexar's published schema); values
    /// are made up.
    fn fixture() -> Value {
        json!({"supSearchMpn": {"hits": 1, "results": [{"part": {
            "mpn": "CL05B104KO5NNNC",
            "manufacturer": {"name": "Samsung Electro-Mechanics"},
            "shortDescription": "Cap Ceramic 0.1uF 16V X7R 10% SMD 0402",
            "octopartUrl": "https://octopart.com/cl05b104ko5nnnc-samsung-x",
            "category": {"name": "Ceramic Capacitors", "path": "/electronic-parts/passive-components/capacitors/ceramic-capacitors"},
            "bestDatasheet": {"url": "https://datasheet.octopart.com/x.pdf"},
            "specs": [
                {"attribute": {"name": "Capacitance", "shortname": "capacitance"}, "displayValue": "100 nF"},
                {"attribute": {"name": "Voltage Rating (DC)", "shortname": "voltagerating_dc_"}, "displayValue": "16 V"},
                {"attribute": {"name": "Dielectric", "shortname": "dielectric"}, "displayValue": "X7R"},
                {"attribute": {"name": "Tolerance", "shortname": "tolerance"}, "displayValue": "10 %"},
                {"attribute": {"name": "Case/Package", "shortname": "case_package"}, "displayValue": "0402"},
                {"attribute": {"name": "Lifecycle Status", "shortname": "lifecyclestatus"}, "displayValue": "Production"}
            ],
            "sellers": [{
                "company": {"name": "LCSC"}, "isAuthorized": true,
                "offers": [{"sku": "C307331", "inventoryLevel": 2000000, "moq": 10, "packaging": "Tape & Reel",
                    "clickUrl": "https://octopart.com/click/1",
                    "prices": [{"quantity": 10, "convertedPrice": 0.0012, "convertedCurrency": "USD"},
                               {"quantity": 100, "convertedPrice": 0.0009, "convertedCurrency": "USD"},
                               {"quantity": 100, "convertedPrice": 0.0011, "convertedCurrency": "USD"}]}]
            }, {
                "company": {"name": "Digi-Key"}, "isAuthorized": true,
                "offers": [{"sku": "1276-1001-1-ND", "inventoryLevel": -2, "moq": null, "packaging": null,
                    "clickUrl": "https://octopart.com/click/2", "prices": []}]
            }]
        }}]}})
    }

    #[test]
    fn parses_offers() {
        let c = parse_results(&fixture()["supSearchMpn"]);
        assert_eq!(c.len(), 2);
        let a = &c[0];
        assert_eq!(a.sku, "LCSC:C307331");
        assert_eq!(a.mpn, "CL05B104KO5NNNC");
        assert_eq!(a.category, Some(crate::model::part::Category::Capacitor));
        assert_eq!(a.package.as_deref(), Some("0402"));
        assert_eq!(a.params.get("capacitance").unwrap().to_string(), "100nF");
        assert_eq!(a.params.get("voltage_rating").unwrap().to_string(), "16V");
        assert_eq!(a.params.get("dielectric").unwrap().to_string(), "X7R");
        assert_eq!(a.params.get("tolerance").unwrap().to_string(), "10%");
        assert_eq!(a.lifecycle, Lifecycle::Active);
        assert_eq!(a.stock, 2_000_000);
        assert_eq!(a.moq, 10);
        assert_eq!(a.prices.len(), 2, "one price per quantity");
        assert_eq!(a.unit_price(100).unwrap().to_string(), "0.0009 USD");
        assert_eq!(a.datasheet.as_deref(), Some("https://datasheet.octopart.com/x.pdf"));
        assert_eq!(a.description, "Cap Ceramic 0.1uF 16V X7R 10% SMD 0402 [LCSC, Tape & Reel]");
        let b = &c[1];
        assert_eq!(b.sku, "Digi-Key:1276-1001-1-ND");
        assert_eq!(b.stock, 0, "negative inventory codes are unknown stock");
        assert_eq!(b.moq, 1);
    }

    #[test]
    fn token_query_and_retry_through_a_mock_transport() {
        let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
        let log = seen.clone();
        let tokens = Arc::new(Mutex::new(0));
        let transport = move |r: &Request| {
            log.lock().unwrap().push(r.clone());
            if r.url == "https://mock/token" {
                let mut n = tokens.lock().unwrap();
                *n += 1;
                return Ok(Response {
                    status: 200,
                    body: json!({"access_token": format!("tok{n}"), "expires_in": 86400, "token_type": "Bearer"})
                        .to_string(),
                });
            }
            // The first token is "expired".
            if r.headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer tok1") {
                return Ok(Response {
                    status: 200,
                    body: json!({"errors": [{"message": "Token validation failed.", "extensions": {"code": "AuthInvalidToken"}}], "data": null}).to_string(),
                });
            }
            Ok(Response { status: 200, body: json!({"data": fixture()}).to_string() })
        };
        let n = Nexar::new("id".into(), "secret".into(), "de", "eur", Arc::new(transport), None)
            .with_endpoints("https://mock/token", "https://mock/graphql");
        let found = n.lookup("cl05b104ko5nnnc").unwrap();
        assert_eq!(found.len(), 2);
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 4, "token, rejected query, new token, query");
        let Body::Form(f) = &reqs[0].body else { panic!("form") };
        assert!(f.contains(&("scope".into(), "supply.domain".into())));
        assert!(f.contains(&("grant_type".into(), "client_credentials".into())));
        let Body::Json(b) = &reqs[3].body else { panic!("JSON") };
        let b: Value = serde_json::from_str(b).unwrap();
        assert!(b["query"].as_str().unwrap().contains("supSearchMpn(q: $q"));
        assert_eq!(b["variables"]["country"], "DE");
        assert_eq!(b["variables"]["currency"], "EUR");
        assert_eq!(b["variables"]["authorizedOnly"], true);
    }

    #[test]
    fn rejected_credentials() {
        let transport =
            |_: &Request| Ok(Response { status: 400, body: json!({"error": "invalid_client"}).to_string() });
        let n = Nexar::new("id".into(), "bad".into(), "US", "USD", Arc::new(transport), None);
        let e = n.verify().unwrap_err().to_string();
        assert!(e.contains("invalid_client") && e.contains("HTTP 400"), "{e}");
    }
}
