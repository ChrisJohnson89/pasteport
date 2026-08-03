use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths;

/// User-tunable behaviour. Written as TOML next to the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// How often the polling backends check for a new clipboard generation.
    pub poll_interval_ms: u64,
    /// Hard cap on stored unpinned clips. Oldest go first.
    pub max_items: usize,
    /// Unpinned clips older than this are pruned. Zero disables age pruning.
    pub retention_days: u64,
    /// Clips larger than this are dropped rather than stored.
    pub max_clip_bytes: usize,
    /// Skip clips whose source app matches any of these. Compared
    /// case-insensitively against both the app name and its bundle id, and
    /// matches on substring so `1password` covers every 1Password process.
    pub ignored_apps: Vec<String>,
    /// Store image clips at all. Off keeps the database small.
    pub capture_images: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            poll_interval_ms: 400,
            max_items: 10_000,
            retention_days: 90,
            max_clip_bytes: 8 * 1024 * 1024,
            ignored_apps: default_ignored_apps(),
            capture_images: true,
        }
    }
}

/// Password managers and secret stores, ignored out of the box. A clipboard
/// manager that quietly archives vault entries is a liability, so this list is
/// opt-out rather than opt-in.
fn default_ignored_apps() -> Vec<String> {
    [
        "1password",
        "bitwarden",
        "keepassxc",
        "lastpass",
        "dashlane",
        "enpass",
        "nordpass",
        "proton pass",
        "keychain access",
        "secretive",
        "gnome-keyring",
        "seahorse",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Config {
    /// Load config from the standard location, falling back to defaults when
    /// the file does not exist. A malformed file is an error rather than a
    /// silent reset, so a typo never wipes someone's ignore list.
    pub fn load() -> Result<Self> {
        let path = paths::config_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let mut cfg: Config = toml::from_str(&raw).map_err(|source| Error::Config {
                    path: path.to_path_buf(),
                    source,
                })?;
                cfg.normalize();
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let body = toml::to_string_pretty(self).expect("Config is always serializable");
        std::fs::write(path, body).map_err(|e| Error::io(path, e))?;
        paths::restrict_to_owner(path)
    }

    /// Whether a config file exists at the standard location. The daemon uses
    /// this to decide whether to write the defaults out on first run.
    pub fn exists() -> Result<bool> {
        Ok(paths::config_path()?.exists())
    }

    /// Clamp values that would make the daemon misbehave.
    fn normalize(&mut self) {
        self.poll_interval_ms = self.poll_interval_ms.clamp(50, 60_000);
        self.max_items = self.max_items.max(1);
        self.max_clip_bytes = self.max_clip_bytes.clamp(1024, 256 * 1024 * 1024);
    }

    /// True when a clip from this source should never be stored.
    pub fn is_ignored_source(&self, app: Option<&str>, bundle_id: Option<&str>) -> bool {
        let haystacks: Vec<String> = [app, bundle_id]
            .into_iter()
            .flatten()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        if haystacks.is_empty() {
            return false;
        }
        self.ignored_apps.iter().any(|needle| {
            let needle = needle.trim().to_ascii_lowercase();
            !needle.is_empty() && haystacks.iter().any(|h| h.contains(&needle))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_ignore_password_managers() {
        let cfg = Config::default();
        assert!(cfg.is_ignored_source(Some("1Password 8"), None));
        assert!(cfg.is_ignored_source(None, Some("com.bitwarden.desktop")));
        assert!(cfg.is_ignored_source(Some("KeePassXC"), None));
        assert!(!cfg.is_ignored_source(Some("Safari"), Some("com.apple.Safari")));
        assert!(!cfg.is_ignored_source(None, None));
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = Config {
            max_items: 42,
            ignored_apps: vec!["mysecrets".into()],
            ..Config::default()
        };
        cfg.save_to(&path).unwrap();

        let back = Config::load_from(&path).unwrap();
        assert_eq!(back.max_items, 42);
        assert_eq!(back.ignored_apps, vec!["mysecrets".to_string()]);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load_from(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(cfg.max_items, Config::default().max_items);
        assert_eq!(cfg.ignored_apps, Config::default().ignored_apps);
    }

    #[test]
    fn malformed_file_is_an_error_not_a_silent_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "max_items = \"not a number\"").unwrap();
        assert!(matches!(
            Config::load_from(&path),
            Err(Error::Config { .. })
        ));
    }

    #[test]
    fn absurd_poll_interval_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "poll_interval_ms = 0").unwrap();
        assert_eq!(Config::load_from(&path).unwrap().poll_interval_ms, 50);
    }
}
