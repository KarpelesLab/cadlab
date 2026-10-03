//! User settings: `config.toml` in the user configuration directory
//! (`$XDG_CONFIG_HOME/cadlab` or `~/.config/cadlab`).
//!
//! Holds per-user data that must never go into projects, such as supplier API credentials. The
//! file is written with owner-only permissions on Unix. Environment variables override it.
//!
//! ```toml
//! [digikey]
//! client_id = "..."
//! client_secret = "..."
//! site = "US"
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

/// The user settings file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// DigiKey credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digikey: Option<DigiKeySettings>,
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
            digikey: Some(DigiKeySettings {
                client_id: "id".into(),
                client_secret: "secret".into(),
                currency: Some("EUR".into()),
                ..Default::default()
            }),
        };
        c.save_to(&p).unwrap();
        assert_eq!(UserConfig::load_from(&p).unwrap(), c);
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
