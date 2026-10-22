//! Settings, saved connection profiles and on-disk locations.
//!
//! Everything is stored as TOML in the platform config directory
//! (`~/.config/pythia` on Linux). Passwords are never written here; they will
//! go to the OS keychain (CONCEPT.md §5.2).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Serialize, de::DeserializeOwned};

mod connections;
mod settings;
pub mod tns;

pub use connections::{ConnectionProfile, Connections, Privilege};
pub use settings::{EditorSettings, KeymapPreset, Settings, ThemeChoice};

/// Platform directories used by Pythia.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn detect() -> Result<Self> {
        let paths = Self::for_app("pythia")?;
        // Carry over settings, connections and the workspace from before the
        // app was renamed from orui to Pythia.
        if let Ok(legacy) = Self::for_app("orui") {
            move_if_absent(&legacy.config_dir, &paths.config_dir);
            move_if_absent(&legacy.data_dir, &paths.data_dir);
        }
        Ok(paths)
    }

    fn for_app(name: &str) -> Result<Self> {
        let dirs = directories::ProjectDirs::from("com", "Ortic", name)
            .context("could not determine a home directory")?;
        Ok(Self {
            config_dir: dirs.config_dir().to_owned(),
            data_dir: dirs.data_dir().to_owned(),
        })
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    pub fn connections_file(&self) -> PathBuf {
        self.config_dir.join("connections.toml")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Autosaved editor content for crash recovery.
    pub fn recovery_dir(&self) -> PathBuf {
        self.data_dir.join("recovery")
    }
}

/// Moves the directory `from` to `to` unless `to` already exists. Best effort:
/// on failure the app starts with fresh directories.
fn move_if_absent(from: &Path, to: &Path) {
    if to.exists() || !from.is_dir() {
        return;
    }
    if let Some(parent) = to.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Err(e) = fs::rename(from, to) {
        eprintln!("moving {} to {}: {e}", from.display(), to.display());
    }
}

/// Loads a TOML file, returning the default value if it doesn't exist.
pub fn load_toml<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Writes a TOML file atomically (write to a temp file, then rename).
pub fn save_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = toml::to_string_pretty(value)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("pythia-config-test-{}", std::process::id()));
        let path = dir.join("settings.toml");
        let mut settings = Settings::default();
        settings.editor.font_size = 15.0;
        save_toml(&path, &settings).unwrap();
        let loaded: Settings = load_toml(&path).unwrap();
        assert_eq!(loaded, settings);
        assert_eq!(
            load_toml::<Settings>(&dir.join("missing.toml")).unwrap(),
            Settings::default()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_dir_moves_only_when_new_is_absent() {
        let dir = std::env::temp_dir().join(format!("pythia-move-test-{}", std::process::id()));
        let old = dir.join("orui");
        let new = dir.join("nested").join("pythia");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("settings.toml"), "theme = \"light\"\n").unwrap();

        move_if_absent(&old, &new);
        assert!(!old.exists());
        assert!(new.join("settings.toml").exists());

        fs::create_dir_all(&old).unwrap();
        move_if_absent(&old, &new);
        assert!(old.exists(), "an existing target is left alone");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn partial_file_uses_defaults() {
        let settings: Settings =
            toml::from_str("theme = \"light\"\n[editor]\ntab_width = 2\n").unwrap();
        assert_eq!(settings.theme, ThemeChoice::Light);
        assert_eq!(settings.editor.tab_width, 2);
        assert_eq!(
            settings.editor.font_size,
            EditorSettings::default().font_size
        );
    }
}
