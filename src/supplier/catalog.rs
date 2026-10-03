//! Offline catalogs: JSON files listing orderable parts, e.g. a stock list, a parts drawer, or
//! data exported from a distributor. Also used to test everything above the provider layer.
//!
//! ```json
//! {
//!   "provider": "mystock",
//!   "parts": [
//!     {"sku": "C51118", "manufacturer": "Diodes", "mpn": "AP2112K-3.3TRG1",
//!      "description": "LDO 3.3V 600mA", "category": "ldo", "package": "SOT-23-5",
//!      "params": {"voltage_out": "3.3V", "current_out": "600mA"},
//!      "stock": 12000, "prices": [{"qty": 1, "price": "0.12 USD"}, {"qty": 100, "price": "0.08 USD"}],
//!      "lifecycle": "active", "datasheet": "https://..."}
//!   ]
//! }
//! ```

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;

use super::{Candidate, Provider, ProviderError, SearchQuery};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    provider: String,
    #[serde(default)]
    parts: Vec<Candidate>,
}

/// A catalog provider backed by a JSON file, loaded on first use.
pub struct Catalog {
    path: PathBuf,
    id: OnceLock<String>,
    data: OnceLock<Result<Vec<Candidate>, ProviderError>>,
}

impl Catalog {
    /// A catalog from `path`, read when first queried.
    pub fn lazy(path: PathBuf) -> Self {
        Catalog { path, id: OnceLock::new(), data: OnceLock::new() }
    }

    /// A catalog from in-memory parts.
    pub fn from_parts(id: &str, parts: Vec<Candidate>) -> Self {
        let c = Catalog::lazy(PathBuf::new());
        let _ = c.id.set(id.to_string());
        let _ = c.data.set(Ok(parts
            .into_iter()
            .map(|mut p| {
                p.provider = id.to_string();
                p
            })
            .collect()));
        c
    }

    fn load(&self) -> &Result<Vec<Candidate>, ProviderError> {
        self.data.get_or_init(|| {
            let name = self.path.display().to_string();
            let text = std::fs::read_to_string(&self.path)
                .map_err(|e| ProviderError::Unavailable { provider: name.clone(), message: e.to_string() })?;
            let f: File = serde_json::from_str(&text)
                .map_err(|e| ProviderError::InvalidData { provider: name.clone(), message: e.to_string() })?;
            let _ = self.id.set(f.provider.clone());
            Ok(f.parts
                .into_iter()
                .map(|mut p| {
                    p.provider = f.provider.clone();
                    p
                })
                .collect())
        })
    }
}

impl Provider for Catalog {
    fn id(&self) -> &str {
        let _ = self.load();
        self.id
            .get()
            .map(String::as_str)
            .unwrap_or_else(|| self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("catalog"))
    }

    fn search(&self, _q: &SearchQuery) -> Result<Vec<Candidate>, ProviderError> {
        // Small local data: return everything, the caller filters.
        self.load().clone()
    }

    fn lookup(&self, mpn: &str) -> Result<Vec<Candidate>, ProviderError> {
        Ok(self.load().clone()?.into_iter().filter(|c| c.mpn.eq_ignore_ascii_case(mpn)).collect())
    }
}
