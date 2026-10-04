//! User settings: `config.toml` in the user configuration directory
//! (`$XDG_CONFIG_HOME/cadlab` or `~/.config/cadlab`).
//!
//! Holds per-user data that must never go into projects, such as supplier API credentials
//! (DigiKey, Mouser, Nexar). The
//! file is written with owner-only permissions on Unix. Environment variables override it.
//!
//! ```toml
//! # Extra shared libraries, searched after the user library (DECISIONS D19).
//! libraries = ["~/hw/team-library", "/opt/cadlab/library"]
//!
//! [digikey]
//! client_id = "..."
//! client_secret = "..."
//! site = "US"
//! currency = "USD"
//!
//! [mouser]
//! api_key = "..."
//!
//! [nexar]
//! client_id = "..."
//! client_secret = "..."
//! country = "US"
//! currency = "USD"
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// DigiKey API settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DigiKeySettings {
    /// OAuth client ID of your DigiKey API app.
    pub client_id: String,
    /// OAuth client secret.
    pub client_secret: String,
    /// Locale site (`US`, `DE`, `UK`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// Language (`en`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Currency for prices (`USD`, `EUR`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Use the sandbox API.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sandbox: bool,
}

/// Mouser Search API settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MouserSettings {
    /// Search API key (from <https://www.mouser.com/api-search/>).
    pub api_key: String,
}

/// Nexar (Octopart) API settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NexarSettings {
    /// OAuth client ID of your Nexar application.
    pub client_id: String,
    /// OAuth client secret.
    pub client_secret: String,
    /// Country for offers (ISO 3166 alpha-2, default `US`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Currency prices are converted to (ISO 4217, default `USD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Also list offers from sellers not authorized by the manufacturer (brokers stay excluded).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unauthorized: bool,
}

/// The user settings file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// Additional shared library directories, searched after the user library. `~/` expands to
    /// the home directory. Kept before the tables so TOML serialization stays valid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub libraries: Vec<PathBuf>,
    /// DigiKey credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digikey: Option<DigiKeySettings>,
    /// Mouser API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouser: Option<MouserSettings>,
    /// Nexar credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nexar: Option<NexarSettings>,
}

/// Error reading or writing settings.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// No home or config directory could be determined.
    #[error("cannot determine the user configuration directory (set HOME or XDG_CONFIG_HOME)")]
    NoConfigDir,
    /// I/O error.
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    /// Invalid TOML.
    #[error("{0}: {1}")]
    Invalid(PathBuf, String),
}

/// Path of the settings file.
pub fn path() -> Option<PathBuf> {
    crate::supplier::config_dir().map(|d| d.join("config.toml"))
}

impl UserConfig {
    /// Loads the settings file; a missing file gives the defaults.
    pub fn load() -> Result<UserConfig, ConfigError> {
        match path() {
            Some(p) => Self::load_from(&p),
            None => Ok(UserConfig::default()),
        }
    }

    /// Loads from a specific file.
    pub fn load_from(p: &Path) -> Result<UserConfig, ConfigError> {
        match std::fs::read_to_string(p) {
            Ok(t) => toml::from_str(&t).map_err(|e| ConfigError::Invalid(p.to_path_buf(), e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserConfig::default()),
            Err(e) => Err(ConfigError::Io(p.to_path_buf(), e)),
        }
    }

    /// The configured library directories, with `~/` expanded.
    pub fn library_paths(&self) -> Vec<PathBuf> {
        self.libraries.iter().map(|p| expand_home(p)).collect()
    }

    /// Saves to the settings file, creating its directory. Returns the path written.
    pub fn save(&self) -> Result<PathBuf, ConfigError> {
        let p = path().ok_or(ConfigError::NoConfigDir)?;
        self.save_to(&p)?;
        Ok(p)
    }

    /// Saves to a specific file, readable by the owner only (Unix), atomically.
    pub fn save_to(&self, p: &Path) -> Result<(), ConfigError> {
        let io = |e| ConfigError::Io(p.to_path_buf(), e);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let text = toml::to_string(self).expect("settings serialize to TOML");
        let tmp = p.with_extension("toml.tmp");
        write_private(&tmp, &text).map_err(io)?;
        std::fs::rename(&tmp, p).map_err(io)
    }
}

/// Expands a leading `~/` (or a lone `~`) to the home directory.
pub fn expand_home(p: &Path) -> PathBuf {
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    if let Ok(rest) = p.strip_prefix("~")
        && let Some(h) = home()
    {
        return h.join(rest);
    }
    p.to_path_buf()
}

#[cfg(unix)]
fn write_private(p: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p)?;
    f.write_all(text.as_bytes())
}

#[cfg(not(unix))]
fn write_private(p: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(p, text)
}

/// Masks a secret for display: `abcd…wxyz` (or `****` when short).
pub fn mask(secret: &str) -> String {
    let n = secret.chars().count();
    if n <= 8 {
        "****".into()
    } else {
        let head: String = secret.chars().take(4).collect();
        let tail: String = secret.chars().skip(n - 4).collect();
        format!("{head}…{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/config.toml");
        assert_eq!(UserConfig::load_from(&p).unwrap(), UserConfig::default());
        let c = UserConfig {
            libraries: vec!["/a/lib".into(), "~/b".into()],
            digikey: Some(DigiKeySettings {
                client_id: "id".into(),
                client_secret: "secret".into(),
                currency: Some("EUR".into()),
                ..Default::default()
            }),
            mouser: Some(MouserSettings { api_key: "key".into() }),
            nexar: Some(NexarSettings {
                client_id: "nid".into(),
                client_secret: "nsecret".into(),
                country: Some("DE".into()),
                ..Default::default()
            }),
        };
        c.save_to(&p).unwrap();
        assert_eq!(UserConfig::load_from(&p).unwrap(), c);
        assert!(std::fs::read_to_string(&p).unwrap().starts_with("libraries = "));
        assert_eq!(c.library_paths()[0], PathBuf::from("/a/lib"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(&p, "[digikey]\nclient_id = 1\n").unwrap();
        assert!(UserConfig::load_from(&p).is_err());
    }

    #[test]
    fn masking() {
        assert_eq!(mask("short"), "****");
        assert_eq!(mask("abcdefghijklmnop"), "abcd…mnop");
    }
}
