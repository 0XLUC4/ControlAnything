//! Interface preferences (per user, outside the daemon config): `ui.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Product name (preferences folder, autostart entry).
const APP_NAME: &str = "ControlAnything";
/// Former name: its preferences and autostart entry are migrated.
const LEGACY_APP_NAME: &str = "PC Hub";

/// Temperature display unit; values stay in °C internally.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TempUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

impl TempUnit {
    /// Absolute temperature (°C) in the displayed unit.
    pub fn convert(self, celsius: f32) -> f32 {
        match self {
            Self::Celsius => celsius,
            Self::Fahrenheit => celsius * 1.8 + 32.0,
        }
    }

    /// Temperature difference (hysteresis...): no 32 °F offset.
    pub fn delta(self, celsius: f32) -> f32 {
        match self {
            Self::Celsius => celsius,
            Self::Fahrenheit => celsius * 1.8,
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Celsius => "°C",
            Self::Fahrenheit => "°F",
        }
    }

    /// "51.2 °C" / "124.2 °F".
    pub fn format(self, celsius: f32, decimals: usize) -> String {
        format!("{:.decimals$} {}", self.convert(celsius), self.symbol())
    }

    /// Formatted difference ("2.0 °C" → "3.6 °F").
    pub fn format_delta(self, celsius: f32, decimals: usize) -> String {
        format!("{:.decimals$} {}", self.delta(celsius), self.symbol())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub launch_at_startup: bool,
    /// Launched with `--minimized` (session startup): stay in the system tray.
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub temperature_unit: TempUnit,
    pub hotkeys: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            launch_at_startup: false,
            start_minimized: true,
            close_to_tray: true,
            temperature_unit: TempUnit::Celsius,
            hotkeys: true,
        }
    }
}

impl Prefs {
    /// `.../ControlAnything/config/ui.toml` (Windows: `%APPDATA%\ControlAnything\config\ui.toml`).
    fn path() -> Option<PathBuf> {
        prefs_path(APP_NAME)
    }

    /// Loads `ui.toml`; missing or invalid file → default values.
    /// On the first launch under the new name, migrates the "PC Hub" `ui.toml`.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            tracing::warn!(
                "user config folder not found: default preferences"
            );
            return Self::default();
        };
        if let Some(legacy) = prefs_path(LEGACY_APP_NAME) {
            match migrate_file(&legacy, &path) {
                Ok(true) => tracing::info!("preferences migrated from {}", legacy.display()),
                Ok(false) => {}
                Err(err) => tracing::warn!("cannot migrate {}: {err}", legacy.display()),
            }
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!(
                    "{} invalid ({err}): default preferences",
                    path.display()
                );
                Self::default()
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!(
                    "cannot read {} ({err}): default preferences",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("User config folder not found.")?;
        let text = toml::to_string(self).map_err(|err| err.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        std::fs::write(&path, text)
            .map_err(|err| format!("Cannot write {}: {err}", path.display()))
    }
}

fn prefs_path(app_name: &str) -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", app_name).map(|d| d.config_dir().join("ui.toml"))
}

/// Copies `legacy` to `target` if `target` does not exist yet. `true` if copied.
fn migrate_file(legacy: &Path, target: &Path) -> std::io::Result<bool> {
    if target.exists() || !legacy.is_file() {
        return Ok(false);
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::copy(legacy, target)?;
    Ok(true)
}

/// Removes the "PC Hub" autostart entry; if it was active, recreates it under the new name.
/// Returns `true` if autostart was re-enabled.
pub fn migrate_legacy_autostart() -> bool {
    // A dev build must not redirect the autostart entry to its `target` folder.
    if cfg!(debug_assertions) {
        return false;
    }
    let Ok(legacy) = auto_launch(LEGACY_APP_NAME) else {
        return false;
    };
    if !legacy.is_enabled().unwrap_or(false) {
        return false;
    }
    if let Err(err) = legacy.disable() {
        tracing::warn!("former autostart entry \"{LEGACY_APP_NAME}\": {err}");
        return false;
    }
    set_autostart(true)
        .inspect_err(|err| tracing::warn!("{err}"))
        .is_ok()
}

/// Autostart entry: the current executable, launched in the system tray.
fn auto_launch(app_name: &str) -> Result<auto_launch::AutoLaunch, String> {
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name(app_name)
        .set_app_path(&exe.to_string_lossy())
        .set_args(&["--minimized"])
        // HKCU key: no administrator rights needed.
        .set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser)
        .build()
        .map_err(|err| err.to_string())
}

/// Enables or removes launch at session login.
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let launcher = auto_launch(APP_NAME)?;
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|err| format!("Autostart: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn celsius_fahrenheit_conversion() {
        assert_eq!(TempUnit::Celsius.convert(42.0), 42.0);
        assert_eq!(TempUnit::Fahrenheit.convert(0.0), 32.0);
        assert_eq!(TempUnit::Fahrenheit.convert(100.0), 212.0);
        assert_eq!(TempUnit::Fahrenheit.delta(10.0), 18.0);
        assert_eq!(TempUnit::Celsius.format(51.24, 1), "51.2 °C");
        assert_eq!(TempUnit::Fahrenheit.format(20.0, 0), "68 °F");
        assert_eq!(TempUnit::Fahrenheit.format_delta(2.0, 1), "3.6 °F");
    }

    #[test]
    fn prefs_defaults_and_roundtrip() {
        let defaults = Prefs::default();
        assert!(!defaults.launch_at_startup);
        assert!(defaults.start_minimized && defaults.close_to_tray && defaults.hotkeys);
        assert_eq!(defaults.temperature_unit, TempUnit::Celsius);

        let custom = Prefs {
            temperature_unit: TempUnit::Fahrenheit,
            hotkeys: false,
            ..Prefs::default()
        };
        let text = toml::to_string(&custom).unwrap_or_default();
        assert_eq!(toml::from_str::<Prefs>(&text).ok(), Some(custom));
        // Missing keys: default values.
        let partial: Prefs = toml::from_str("close_to_tray = false").unwrap_or_default();
        assert!(!partial.close_to_tray && partial.start_minimized);
    }

    #[test]
    fn autostart_entry_uses_minimized_flag() {
        // Building the entry does not touch the registry.
        let launcher = auto_launch(APP_NAME).expect("autostart entry");
        assert_eq!(launcher.get_app_name(), "ControlAnything");
        assert_eq!(launcher.get_args(), ["--minimized"]);
    }

    #[test]
    fn legacy_prefs_are_copied_once() {
        let dir = std::env::temp_dir().join(format!("controlanything-prefs-{}", std::process::id()));
        let legacy = dir.join("legacy").join("ui.toml");
        let target = dir.join("new").join("config").join("ui.toml");
        let _ = std::fs::remove_dir_all(&dir);
        // No former file: nothing to do.
        assert!(!migrate_file(&legacy, &target).expect("migration"));
        std::fs::create_dir_all(legacy.parent().expect("parent")).expect("folder");
        std::fs::write(&legacy, "hotkeys = false").expect("write");
        assert!(migrate_file(&legacy, &target).expect("migration"));
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "hotkeys = false");
        // The new file exists: never overwritten.
        std::fs::write(&legacy, "hotkeys = true").expect("write");
        assert!(!migrate_file(&legacy, &target).expect("migration"));
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "hotkeys = false");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
