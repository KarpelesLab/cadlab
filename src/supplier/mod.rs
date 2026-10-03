//! Part research: searching suppliers and catalogs for orderable parts, with stock and prices.
//!
//! Providers implement [`Provider`] (synchronous; network providers do their own blocking I/O,
//! DECISIONS D14). A [`Suppliers`] set queries several and merges results. Offers are cache data
//! (`~/.cache/cadlab`), never stored in projects: projects stay provider-agnostic (D12).

pub mod cache;
pub mod catalog;
mod money;
pub mod query;

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::part::{Category, Params};

pub use money::Money;
pub use query::{ParamFilter, SearchQuery};

/// Product lifecycle status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// In production.
    Active,
    /// Not recommended for new designs.
    Nrnd,
    /// Last-time buy announced.
    LastTimeBuy,
    /// Obsolete / end of life.
    Obsolete,
    /// Unknown.
    #[default]
    Unknown,
}

/// Unit price from a quantity upward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceBreak {
    /// Minimum quantity for this price.
    pub qty: u64,
    /// Unit price.
    pub price: Money,
}

/// An orderable part offered by a provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    /// Provider ID (`mystock`, `lcsc`, `digikey`, ...).
    #[serde(default)]
    pub provider: String,
    /// Provider's own part number (SKU), e.g. an LCSC `C` number.
    pub sku: String,
    /// Manufacturer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Manufacturer part number.
    pub mpn: String,
    /// Description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Category, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// Package name as given by the provider (`0402`, `SOT-23-5`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Parameters, normalized to cadlab keys and units where possible.
    #[serde(default)]
    pub params: Params,
    /// Units in stock.
    #[serde(default)]
    pub stock: u64,
    /// Minimum order quantity.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub moq: u64,
    /// Price breaks, ascending quantity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prices: Vec<PriceBreak>,
    /// Lifecycle.
    #[serde(default)]
    pub lifecycle: Lifecycle,
    /// Datasheet URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datasheet: Option<String>,
    /// Product page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

fn one() -> u64 {
    1
}

fn is_one(v: &u64) -> bool {
    *v == 1
}

impl Candidate {
    /// Unit price when buying `qty` (the break for the largest quantity ≤ `qty`; the first break
    /// if `qty` is below all of them, since the MOQ applies).
    pub fn unit_price(&self, qty: u64) -> Option<&Money> {
        self.prices
            .iter()
            .filter(|b| b.qty <= qty.max(self.moq))
            .max_by_key(|b| b.qty)
            .or(self.prices.first())
            .map(|b| &b.price)
    }

    /// Quantity actually bought for a need of `qty` (at least the MOQ).
    pub fn order_qty(&self, qty: u64) -> u64 {
        qty.max(self.moq)
    }
}

/// Error from a provider.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// Missing configuration (API key, catalog path).
    #[error("{provider}: not configured: {message}")]
    NotConfigured {
        /// Provider.
        provider: String,
        /// What is missing.
        message: String,
    },
    /// Network or I/O failure.
    #[error("{provider}: {message}")]
    Unavailable {
        /// Provider.
        provider: String,
        /// Details.
        message: String,
    },
    /// The provider's data could not be understood.
    #[error("{provider}: invalid data: {message}")]
    InvalidData {
        /// Provider.
        provider: String,
        /// Details.
        message: String,
    },
}

/// A source of orderable parts.
pub trait Provider: Send + Sync {
    /// Short stable ID.
    fn id(&self) -> &str;
    /// Searches by keywords and filters. Providers may return extra results; callers filter
    /// with [`SearchQuery::matches`].
    fn search(&self, q: &SearchQuery) -> Result<Vec<Candidate>, ProviderError>;
    /// Exact lookup by manufacturer part number.
    fn lookup(&self, mpn: &str) -> Result<Vec<Candidate>, ProviderError>;
}

/// Results of querying several providers.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct Results {
    /// Matching candidates, best first.
    pub candidates: Vec<Candidate>,
    /// Providers that failed, with the reason; the others still answered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// The configured providers.
#[derive(Clone, Default)]
pub struct Suppliers {
    providers: Vec<Arc<dyn Provider>>,
}

impl std::fmt::Debug for Suppliers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.providers.iter().map(|p| p.id())).finish()
    }
}

impl Suppliers {
    /// No providers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a provider.
    pub fn with(mut self, p: Arc<dyn Provider>) -> Self {
        self.providers.push(p);
        self
    }

    /// Providers from the environment: catalog files listed in `CADLAB_CATALOGS`
    /// (path-separated), plus `*.json` in the user catalog directory
    /// (`$XDG_CONFIG_HOME/cadlab/catalogs` or `~/.config/cadlab/catalogs`).
    pub fn from_env() -> Self {
        let mut s = Suppliers::new();
        let mut paths = Vec::new();
        if let Some(v) = std::env::var_os("CADLAB_CATALOGS") {
            paths.extend(std::env::split_paths(&v));
        }
        if let Some(dir) = config_dir().map(|d| d.join("catalogs"))
            && let Ok(rd) = std::fs::read_dir(dir)
        {
            let mut files: Vec<_> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect();
            files.sort();
            paths.extend(files);
        }
        for p in paths {
            s = s.with(Arc::new(catalog::Catalog::lazy(p)));
        }
        s
    }

    /// Provider IDs.
    pub fn ids(&self) -> Vec<String> {
        self.providers.iter().map(|p| p.id().to_string()).collect()
    }

    /// Whether no provider is configured.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    fn selected<'a>(&'a self, only: &'a [String]) -> impl Iterator<Item = &'a Arc<dyn Provider>> + 'a {
        self.providers
            .iter()
            .filter(move |p| only.is_empty() || only.iter().any(|o| o == p.id()))
    }

    /// Searches every (selected) provider, filters, ranks and truncates to `q.limit`.
    pub fn search(&self, q: &SearchQuery, only: &[String]) -> Results {
        let mut r = Results::default();
        for p in self.selected(only) {
            match p.search(q) {
                Ok(c) => r.candidates.extend(c.into_iter().map(|mut c| {
                    c.provider = p.id().to_string();
                    c
                })),
                Err(e) => r.errors.push(e.to_string()),
            }
        }
        r.candidates.retain(|c| q.matches(c));
        q.rank(&mut r.candidates);
        r.candidates.truncate(q.limit);
        r
    }

    /// Looks up an MPN at every (selected) provider.
    pub fn lookup(&self, mpn: &str, only: &[String]) -> Results {
        let mut r = Results::default();
        for p in self.selected(only) {
            match p.lookup(mpn) {
                Ok(c) => r
                    .candidates
                    .extend(c.into_iter().filter(|c| c.mpn.eq_ignore_ascii_case(mpn)).map(|mut c| {
                        c.provider = p.id().to_string();
                        c
                    })),
                Err(e) => r.errors.push(e.to_string()),
            }
        }
        r
    }
}

/// User configuration directory (`$XDG_CONFIG_HOME/cadlab` or `~/.config/cadlab`).
pub fn config_dir() -> Option<std::path::PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(std::path::PathBuf::from(x).join("cadlab"));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| std::path::PathBuf::from(h).join(".config/cadlab"))
}
