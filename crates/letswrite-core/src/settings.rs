//! User settings, persisted as TOML in the OS-standard config directory.
//!
//! Layout:
//! - Linux:   `$XDG_CONFIG_HOME/letswrite/settings.toml` (typically `~/.config/letswrite/`)
//! - macOS:   `~/Library/Application Support/letswrite/settings.toml`
//! - Windows: `%APPDATA%/letswrite/settings.toml`
//!
//! Atomic writes: settings are written to a sibling tempfile and renamed into
//! place so a crash mid-write never leaves a half-truncated file.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use unic_langid::LanguageIdentifier;

use crate::error::{Error, Result};

const QUALIFIER: &str = "dev";
const ORGANIZATION: &str = "letswrite";
const APPLICATION: &str = "letswrite";
const FILENAME: &str = "settings.toml";

/// All user-facing settings. Add fields with `#[serde(default)]` so old files
/// keep loading; removing or renaming requires a migration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// UI language as a BCP-47 tag (e.g. `en`, `de`, `pt-BR`).
    pub ui_language: LanguageIdentifier,

    /// Most recently opened project root, if any. Used to restore session.
    pub last_project: Option<PathBuf>,

    /// Window geometry from the previous session.
    pub window: WindowSettings,

    /// Color theme.
    pub theme: ThemePreference,

    /// AI assistant defaults.
    pub ai: AiSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ui_language: "en".parse().expect("\"en\" is a valid language tag"),
            last_project: None,
            window: WindowSettings::default(),
            theme: ThemePreference::default(),
            ai: AiSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowSettings {
    pub width: u32,
    pub height: u32,
    /// Fraction of total width occupied by the left sidebar (0.0..1.0).
    pub sidebar_ratio: f32,
    /// Fraction of the remaining (post-sidebar) width occupied by the editor (0.0..1.0).
    pub editor_ratio: f32,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            width: 1800,
            height: 1100,
            sidebar_ratio: 0.16,
            editor_ratio: 0.66,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ThemePreference {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiSettings {
    /// Name of the provider in the registry (e.g. `anthropic`). `None` =
    /// assistant disabled / not yet configured.
    pub provider: Option<String>,
    /// Provider-specific model identifier.
    pub model: Option<String>,
}

impl Settings {
    /// Resolve the on-disk path for settings, creating the parent directory
    /// if needed.
    pub fn path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
            .ok_or(Error::NoConfigDir)?;
        let dir = dirs.config_dir();
        if !dir.exists() {
            fs::create_dir_all(dir).map_err(|e| Error::io_at(dir, e))?;
        }
        Ok(dir.join(FILENAME))
    }

    /// Load settings from the OS-standard config path. Missing file → defaults.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        Self::load_from(&path)
    }

    /// Load from an explicit path (primarily for tests).
    pub fn load_from(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                Error::InvalidSettings(format!("{}: {e}", path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io_at(path, e)),
        }
    }

    /// Persist settings to the OS-standard config path atomically.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        self.save_to(&path)
    }

    /// Persist to an explicit path atomically (primarily for tests).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        let parent = path.parent().ok_or_else(|| {
            Error::InvalidSettings(format!("settings path has no parent: {}", path.display()))
        })?;
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| Error::io_at(parent, e))?;
        }

        // Write to a sibling tempfile, fsync, rename — survives a crash mid-write.
        let tmp = path.with_extension("toml.tmp");
        {
            let mut f = fs::File::create(&tmp).map_err(|e| Error::io_at(&tmp, e))?;
            f.write_all(text.as_bytes()).map_err(|e| Error::io_at(&tmp, e))?;
            f.sync_all().map_err(|e| Error::io_at(&tmp, e))?;
        }
        fs::rename(&tmp, path).map_err(|e| Error::io_at(path, e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");

        let original = Settings::default();
        original.save_to(&path).unwrap();
        let loaded = Settings::load_from(&path).unwrap();

        assert_eq!(loaded.ui_language, original.ui_language);
        assert_eq!(loaded.window.width, original.window.width);
        assert_eq!(loaded.window.height, original.window.height);
        assert_eq!(loaded.theme, original.theme);
        assert!(loaded.last_project.is_none());
        assert!(loaded.ai.provider.is_none());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.ui_language, Settings::default().ui_language);
    }

    #[test]
    fn invalid_toml_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        fs::write(&path, "not = valid = toml").unwrap();
        let err = Settings::load_from(&path).unwrap_err();
        assert!(matches!(err, Error::InvalidSettings(_)));
    }

    #[test]
    fn unknown_field_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        fs::write(&path, "ui_language = \"en\"\nmystery_field = 42").unwrap();
        let err = Settings::load_from(&path).unwrap_err();
        assert!(matches!(err, Error::InvalidSettings(_)));
    }

    #[test]
    fn preserves_non_english_language() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let s = Settings { ui_language: "pt-BR".parse().unwrap(), ..Settings::default() };
        s.save_to(&path).unwrap();
        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.ui_language.to_string(), "pt-BR");
    }

    #[test]
    fn atomic_write_no_tempfile_remains() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        Settings::default().save_to(&path).unwrap();
        let tmp = path.with_extension("toml.tmp");
        assert!(!tmp.exists(), "tempfile should have been renamed away");
    }
}
